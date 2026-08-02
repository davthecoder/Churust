//! The hyper-based HTTP/1.1 serving engine that drives an [`App`] over a real
//! socket.
//!
//! Most applications never call into this module directly — [`App::start`] and
//! [`App::start_with_shutdown`] bind a listener and delegate to [`serve`]. It is
//! public so advanced callers can drive serving with a custom address and
//! shutdown signal.

use crate::app::App;
use crate::body::Body;
#[cfg(feature = "tls")]
use crate::tls::acceptor_from_pem;
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{
    combinators::UnsyncBoxBody, BodyDataStream, BodyExt, Full, Limited, StreamBody,
};
use hyper::body::Frame;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;

/// Refuse to serve a TLS-configured application that was built without the
/// `tls` feature.
///
/// Before this check, that combination started successfully and served
/// plaintext on the port the operator had configured a certificate for. Nothing
/// said so: the build succeeded, the server came up, and requests were answered
/// — in clear text, on a port every client and every runbook treated as HTTPS.
///
/// An error at startup is the only safe answer. A warning would be missed in
/// exactly the deployments that most need it, and serving unencrypted traffic
/// the configuration says is encrypted is worse than not serving at all.
fn refuse_tls_without_the_feature(app: &App) -> std::io::Result<()> {
    if cfg!(not(feature = "tls")) && app.config().tls.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "TLS is configured but this binary was built without the `tls` feature, \
             so the listener would serve plaintext on a port configured for HTTPS. \
             Enable the feature (`churust = { version = \"0.3\", features = [\"tls\"] }`) \
             or remove the TLS configuration.",
        ));
    }
    Ok(())
}

/// Serve `app` on `addr` until `shutdown` resolves (graceful drain).
///
/// Uses HTTP/1.1 (`hyper::server::conn::http1::Builder`).  The plan
/// referenced `auto::Builder` but that type's `serve_connection` returns a
/// `Connection<'_, …>` that borrows the builder, which cannot be moved into a
/// `tokio::spawn` closure.  Using `http1::Builder` directly returns an owned
/// `Connection<I, S>` that is `'static`, compiles correctly, and still
/// satisfies the HTTP/1.1-only requirement of Plan 1.
///
/// Prefer [`App::start`] / [`App::start_with_shutdown`] unless you need to
/// control the bind address and shutdown future yourself.
///
/// [`App::start`]: crate::App::start
/// [`App::start_with_shutdown`]: crate::App::start_with_shutdown
///
/// ```no_run
/// use churust_core::{Churust, Call, engine};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
///     .build();
/// let addr = "127.0.0.1:8080".parse().unwrap();
/// // Serve until Ctrl-C.
/// engine::serve(app, addr, async { let _ = tokio::signal::ctrl_c().await; }).await?;
/// # Ok::<(), std::io::Error>(())
/// # });
/// ```
pub async fn serve<F>(app: App, addr: SocketAddr, shutdown: F) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    refuse_tls_without_the_feature(&app)?;
    let listener = bind_tcp(addr, app.config().backlog)?;
    let limits = std::sync::Arc::new(AcceptLimits::from(app.config()));
    serve_listener(app, listener, limits, shutdown).await
}

/// Serve `app` on an already-bound listener until `shutdown` resolves.
///
/// Use this when the port must be known before the server starts — a test that
/// needs the address, a socket passed in by a supervisor, or anything binding
/// under its own policy.
///
/// It also closes a race that [`serve`] cannot: discovering a free port by
/// binding, dropping, and letting `serve` bind again leaves a window in which
/// something else can take it. Handing over the listener removes the window.
///
/// ```no_run
/// use churust_core::{Churust, Call, engine};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
///     .build();
/// let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
/// let addr = listener.local_addr()?;
/// println!("listening on {addr}");
/// engine::serve_on(app, listener, async { let _ = tokio::signal::ctrl_c().await; }).await?;
/// # Ok::<(), std::io::Error>(())
/// # });
/// ```
pub async fn serve_on<F>(
    app: App,
    listener: tokio::net::TcpListener,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    refuse_tls_without_the_feature(&app)?;
    let limits = std::sync::Arc::new(AcceptLimits::from(app.config()));
    serve_listener(app, listener, limits, shutdown).await
}

/// Bind one address, with the backlog and socket options Churust wants.
///
/// Separate from serving so several addresses can be bound *before* any accept
/// loop starts — see [`serve_many`].
fn bind_tcp(addr: SocketAddr, backlog: u32) -> std::io::Result<tokio::net::TcpListener> {
    // Bind through a socket so the backlog is ours to choose. tokio's
    // TcpListener::bind hard-codes 1024, which is fine until it is not.
    let socket = match addr {
        std::net::SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4()?,
        std::net::SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6()?,
    };
    // Without this a restart fails while the previous socket sits in
    // TIME_WAIT — the single most common cause of "address already in use".
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(backlog)
}

/// Bind one address with the chosen backlog, without needing a runtime.
///
/// [`bind_tcp`] goes through `tokio::net::TcpSocket`, whose `listen` panics
/// unless a reactor is already in scope. `serve_sharded` binds *before* it
/// builds the runtime that will accept — that is the whole point of binding
/// first, so a bad address is reported before any worker starts — so it needs a
/// listener it can create with no runtime at all and register later.
fn bind_tcp_std(addr: SocketAddr, backlog: u32) -> std::io::Result<std::net::TcpListener> {
    let domain = match addr {
        SocketAddr::V4(_) => socket2::Domain::IPV4,
        SocketAddr::V6(_) => socket2::Domain::IPV6,
    };
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, None)?;
    // Same reason as `bind_tcp`: without it a restart fails while the previous
    // socket sits in TIME_WAIT.
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    // Saturating rather than `as`, so a large `backlog` cannot wrap into a
    // negative `c_int` and ask the kernel for nonsense.
    socket.listen(backlog.min(i32::MAX as u32) as i32)?;
    // tokio requires this of any listener handed to `from_std`.
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

/// Bind a Unix socket, with the backlog Churust was told to use.
///
/// Bound through a socket for the same reason `bind_tcp` is: neither std's nor
/// tokio's `UnixListener::bind` lets the backlog be chosen, so this listener
/// silently used the platform default — 128 on Linux — however `backlog` was
/// set. tokio has a `TcpSocket` but no `UnixSocket`, hence `socket2` here and
/// not there.
///
/// The caller is responsible for having established that any node already at
/// `path` is stale; this only binds.
#[cfg(unix)]
fn bind_unix(path: &std::path::Path, backlog: u32) -> std::io::Result<tokio::net::UnixListener> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.bind(&socket2::SockAddr::unix(path)?)?;
    // Saturating rather than `as`: `backlog` is a `u32` an operator fills in, and
    // wrapping a large one into a negative `c_int` would ask the kernel for a
    // backlog of nonsense.
    socket.listen(backlog.min(i32::MAX as u32) as i32)?;
    // tokio requires a non-blocking listener; `from_std` documents this as the
    // caller's job.
    socket.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(std::os::unix::net::UnixListener::from(socket))
}

/// Serve on an already-bound listener until `shutdown` resolves.
async fn serve_listener<F>(
    app: App,
    listener: tokio::net::TcpListener,
    limits: std::sync::Arc<AcceptLimits>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let conn_cfg = ConnSettings::from(app.config());
    let nodelay = app.config().tcp_nodelay;
    let shutdown_timeout_ms = app.config().shutdown_timeout_ms;
    let drain = Drain::new();
    let mut backoff = AcceptBackoff::default();
    tokio::pin!(shutdown);

    #[cfg(feature = "tls")]
    let tls_acceptor = match &app.config().tls {
        Some(t) => Some(acceptor_from_pem(&t.cert, &t.key)?),
        None => None,
    };

    loop {
        // Waiting for a slot and accepting are one cancellable step, raced
        // against shutdown.
        //
        // Both halves can park indefinitely: `accept` when no client comes, and
        // `connection_slot` when every permit is held. Awaiting either *outside*
        // the `select!` means a saturated server never reaches the shutdown arm
        // — SIGTERM would hang until the orchestrator escalates to SIGKILL,
        // which is a worse failure than the unbounded drain this budget was
        // added alongside. Both are cancel-safe: a cancelled slot returns its
        // permit, and a cancelled `accept` has not taken a connection.
        let acquire = async {
            // Take the slot before accepting, so excess load waits in the
            // kernel backlog — the client's problem — rather than as memory and
            // descriptors in this process.
            let slot = limits.connection_slot().await;
            (slot, listener.accept().await)
        };

        tokio::select! {
            // Shutdown wins a tie: with a signal pending there is no reason to
            // begin serving one more connection.
            biased;
            _ = &mut shutdown => break,
            (slot, accepted) = acquire => {
                let (stream, peer) = match accepted {
                    Ok((stream, peer)) => {
                        backoff.reset();
                        // Nagle's algorithm holds a small write back waiting for
                        // more to send, and the peer's delayed ACK holds the
                        // acknowledgement back waiting for a response. An HTTP
                        // response written as a head and a body — which is what
                        // hyper writes — can land in exactly that standoff, and
                        // it resolves on the delayed-ACK timer rather than on
                        // anything either side does. A server that answers in
                        // microseconds cannot afford to sometimes answer in
                        // tens of milliseconds instead. Every general-purpose
                        // HTTP server disables it; Churust simply had not.
                        //
                        // A failure here is not fatal: the connection works,
                        // it is just latency-prone, and refusing to serve it
                        // would be the larger harm.
                        if nodelay {
                            if let Err(e) = stream.set_nodelay(true) {
                            tracing::debug!(%peer, error = %e, "could not disable Nagle");
                            }
                        }
                        (stream, peer)
                    }
                    Err(e) => {
                        // A persistent error — EMFILE when the descriptor table
                        // is full, which is exactly the state a flood drives
                        // toward — would otherwise spin this loop at full tilt
                        // and starve the very tasks that would free a slot.
                        let pause = backoff.next_pause();
                        tracing::warn!(error = %e, pause_ms = pause.as_millis() as u64, "accept failed");
                        tokio::time::sleep(pause).await;
                        continue;
                    }
                };
                #[cfg(feature = "tls")]
                {
                    if let Some(acceptor) = tls_acceptor.clone() {
                        let app = app.clone();
                        let handshakes = limits.handshakes.clone();
                        let handshake_timeout = limits.tls_handshake_timeout;
                        // The token is taken *before* spawning, so a connection
                        // that is still shaking hands when shutdown fires is
                        // already counted — otherwise the drain could finish
                        // while a handshake was in flight. The handshake itself
                        // runs on the spawned task so a slow one does not block
                        // the accept loop.
                        let token = drain.token();
                        let conn_builder_fut = async move {
                            // Queueing for the handshake budget and performing
                            // the handshake are one timed step.
                            //
                            // Handshakes get their own, much smaller budget: the
                            // work is asymmetric, cheap to ask for and expensive
                            // to answer, so the connection cap alone is too
                            // loose a bound. But the deadline has to cover the
                            // wait for that budget as well as the work it
                            // guards, because the thing being bounded is how
                            // long a peer that has not yet proved it can speak
                            // TLS may hold a *connection* permit — and the
                            // connection permit is taken before this task even
                            // starts.
                            //
                            // Timing only the handshake turned the budget into
                            // a rate limiter on failure: with the default 256
                            // permits and a 10s deadline, stalled ClientHellos
                            // expire 256 per 10s while the rest wait on
                            // `acquire_owned` with no deadline at all. Filling
                            // the connection budget that way costs an attacker
                            // one TCP connect each and blocks the accept loop
                            // for as long as it takes to drain — minutes, at
                            // the default caps, rather than the advertised ten
                            // seconds.
                            //
                            // The cost of covering the queue is that under
                            // genuine handshake overload a legitimate client
                            // can be dropped while still queued. That is load
                            // shedding, and it is the behaviour the knob
                            // promises: no peer holds a connection permit for
                            // longer than this.
                            let queue_and_shake = async {
                                let permit = match &handshakes {
                                    Some(sem) => sem.clone().acquire_owned().await.ok(),
                                    None => None,
                                };
                                // Held alongside the result so it outlives the
                                // handshake and is released deliberately below,
                                // rather than at the end of this block.
                                (acceptor.accept(stream).await, permit)
                            };
                            let (accepted, handshake_permit) = match handshake_timeout {
                                Some(limit) => {
                                    match tokio::time::timeout(limit, queue_and_shake).await {
                                        Ok(pair) => pair,
                                        Err(_) => {
                                            // Dropping the future here cancels
                                            // the acquire as well as the
                                            // handshake, so a peer that timed
                                            // out while queued returns nothing
                                            // and holds nothing.
                                            //
                                            // Before the handshake completes
                                            // there is no HTTP layer, so
                                            // `header_read_timeout` cannot
                                            // cover this case.
                                            tracing::debug!(%peer, "TLS handshake timed out");
                                            return;
                                        }
                                    }
                                }
                                None => queue_and_shake.await,
                            };
                            match accepted {
                                Ok(tls_stream) => {
                                    // The handshake budget is for handshakes; the
                                    // connection budget takes over from here.
                                    drop(handshake_permit);
                                    serve_stream(app, tls_stream, conn_cfg, peer, token, slot).await;
                                }
                                // Certificate, protocol-version and SNI failures
                                // all land here. Logged at debug because an
                                // internet-facing port sees scanner noise
                                // constantly, and a warning per probe is its own
                                // denial of service on the log pipeline.
                                Err(e) => tracing::debug!(%peer, error = %e, "TLS handshake failed"),
                            }
                        };
                        tokio::spawn(conn_builder_fut);
                        continue;
                    }
                }
                serve_stream(app.clone(), stream, conn_cfg, peer, drain.token(), slot).await;
            }
        }
    }

    drain.wait(shutdown_timeout_ms).await;
    Ok(())
}

/// Connection tracking for graceful shutdown: signal every live connection to
/// wind down, then wait until the last one has finished.
///
/// hyper-util's `GracefulShutdown` cannot do this for us. `Watcher::watch`
/// requires the sealed `GracefulConnection` trait, which hyper-util 0.1.x
/// implements for `auto::Connection` but *not* for the upgradeable connection
/// that WebSockets need — and the trait being sealed means we cannot supply
/// the impl ourselves. So this reproduces the same two-part contract over the
/// connection type we actually serve: `UpgradeableConnection` exposes
/// `graceful_shutdown`, which is the only piece that was ever missing.
struct Drain {
    /// Fires once. Every connection task watches it and stops accepting new
    /// requests on its connection when it flips.
    signal: tokio::sync::watch::Sender<bool>,
    /// Handed out by [`Drain::token`] and held for the lifetime of each
    /// connection task. `wait` finishes when the last clone is dropped, which
    /// is exactly when the last connection has finished.
    guard: Option<tokio::sync::mpsc::Sender<()>>,
    finished: tokio::sync::mpsc::Receiver<()>,
}

/// A connection's share of the drain: watch one end, hold the other.
struct DrainToken {
    signal: tokio::sync::watch::Receiver<bool>,
    /// Never read. Dropping it is the signal that this connection is done.
    _guard: tokio::sync::mpsc::Sender<()>,
}

impl Drain {
    fn new() -> Self {
        let (signal, _) = tokio::sync::watch::channel(false);
        let (guard, finished) = tokio::sync::mpsc::channel(1);
        Self {
            signal,
            guard: Some(guard),
            finished,
        }
    }

    fn token(&self) -> DrainToken {
        DrainToken {
            signal: self.signal.subscribe(),
            // `guard` is Some for as long as the accept loop runs; `wait`
            // takes it, so this cannot be called afterwards.
            _guard: self.guard.clone().expect("token after wait"),
        }
    }

    /// Tell every connection to wind down, then wait — for at most
    /// `timeout_ms`, or indefinitely if that is zero.
    ///
    /// Bounding this is the point: one slow request must not delay exit
    /// forever, because under an orchestrator that means being killed rather
    /// than shutting down cleanly.
    async fn wait(mut self, timeout_ms: u64) {
        let _ = self.signal.send(true);
        // Drop our own guard, or the count never reaches zero.
        self.guard.take();

        // `recv` resolves with `None` once every clone has been dropped.
        let drained = async move { while self.finished.recv().await.is_some() {} };

        if timeout_ms == 0 {
            drained.await;
        } else {
            let grace = std::time::Duration::from_millis(timeout_ms);
            if tokio::time::timeout(grace, drained).await.is_err() {
                tracing_drain_timeout(timeout_ms);
            }
        }
    }
}

/// How long an idle connection is given to put its GOAWAY (or HTTP/1 close) on
/// the wire during shutdown before it is dropped.
///
/// Long enough for a frame to be written and flushed; short enough that a
/// rolling restart is not held up by peers that have nothing to say. Not
/// configurable: the tunable that matters is the grace period, and this is
/// deliberately far below any sane value of it.
const GOAWAY_LINGER: std::time::Duration = std::time::Duration::from_millis(250);

/// The `Content-Type` of the refusals this module composes itself.
///
/// Spelled the same way [`Response::text`](crate::Response::text) spells it, so
/// a `413` the engine wrote and a `413` a handler returned are indistinguishable
/// to a client parsing the header.
const TEXT_PLAIN: &str = "text/plain; charset=utf-8";

/// A connection's share of the connection budget, held for as long as the
/// connection is served. `None` when the budget is unlimited.
type ConnSlot = Option<tokio::sync::OwnedSemaphorePermit>;

/// A connection's share of *both* budgets, in a form that can outlive the
/// connection future.
///
/// hyper resolves an upgraded connection the moment it dispatches the `101`,
/// while the socket itself lives on in a detached task. Without handing that
/// task a share, a live WebSocket held no permit and no drain token at all, so
/// `max_connections` bounded nothing for WebSocket traffic and the drain could
/// not see them. Cloning is what lets both the connection loop and the upgraded
/// task hold it; the budget is returned when the last clone drops.
#[derive(Clone)]
pub(crate) struct ConnGuard(#[allow(dead_code)] std::sync::Arc<ConnGuardInner>);

pub(crate) struct ConnGuardInner {
    _slot: ConnSlot,
    /// Underscored because only the `ws` build reads it: for an HTTP connection
    /// the token is held, not watched — `serve_stream` watches the signal
    /// directly — and merely holding it is what makes the drain wait.
    _token: DrainToken,
}

impl ConnGuard {
    fn new(slot: ConnSlot, token: DrainToken) -> Self {
        Self(std::sync::Arc::new(ConnGuardInner {
            _slot: slot,
            _token: token,
        }))
    }

    /// Resolves when the server has been asked to shut down.
    ///
    /// Holding a drain token makes the drain *wait* for this connection; it does
    /// not tell the connection to wind down. For an HTTP connection the loop in
    /// `serve_stream` watches the signal and does that. An upgraded WebSocket
    /// leaves that loop the moment the `101` is dispatched — hyper resolves the
    /// connection there and the socket lives on in a detached task — so the task
    /// has to watch the signal itself, and until it could, every shutdown waited
    /// out the full grace period for any live WebSocket and never returned at all
    /// at `shutdown_timeout_ms = 0`.
    #[cfg(feature = "ws")]
    pub(crate) async fn draining(&self) {
        let mut signal = self.0._token.signal.clone();
        // `wait_for` rather than `changed`, so a signal that fired before this
        // was called is still seen — the same reason `serve_stream` uses it.
        let _ = signal.wait_for(|fired| *fired).await;
    }
}

/// How many connections, and how many TLS handshakes, may be in progress.
///
/// The listen backlog bounds what the kernel queues before the accept loop
/// reaches it. These bound what the process takes on. Without them the only
/// limits are what the OS will hand out, and the failure mode is the process
/// dying rather than new connections waiting their turn.
struct AcceptLimits {
    connections: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    #[cfg(feature = "tls")]
    handshakes: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    #[cfg(feature = "tls")]
    tls_handshake_timeout: Option<std::time::Duration>,
}

impl AcceptLimits {
    /// Like [`AcceptLimits::from`], but with the connection budget replaced.
    ///
    /// The sharded engine's `SO_REUSEPORT` path needs a *per-worker* budget:
    /// every worker acquires a permit before it accepts, so several workers
    /// drawing on one shared budget deadlock the moment the budget is smaller
    /// than the number of workers — one worker holds the only permit while the
    /// kernel hands the connection to another that is still waiting for one.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn with_connections(cfg: &crate::app::ServerConfig, connections: usize) -> Self {
        let mut limits = Self::from(cfg);
        limits.connections = (connections > 0)
            .then(|| std::sync::Arc::new(tokio::sync::Semaphore::new(connections)));
        limits
    }

    fn from(cfg: &crate::app::ServerConfig) -> Self {
        // `0` means unlimited for every one of these, so a caller who does not
        // want a bound is not forced to invent a large number.
        let sem = |n: usize| (n > 0).then(|| std::sync::Arc::new(tokio::sync::Semaphore::new(n)));
        Self {
            connections: sem(cfg.max_connections),
            #[cfg(feature = "tls")]
            handshakes: sem(cfg.max_tls_handshakes),
            #[cfg(feature = "tls")]
            tls_handshake_timeout: (cfg.tls_handshake_timeout_ms > 0)
                .then(|| std::time::Duration::from_millis(cfg.tls_handshake_timeout_ms)),
        }
    }

    /// Wait until a connection may be served. Acquired before `accept`, so
    /// excess load waits in the kernel backlog rather than in this process.
    async fn connection_slot(&self) -> ConnSlot {
        match &self.connections {
            // `acquire_owned` only fails on a closed semaphore, and nothing
            // closes these.
            Some(sem) => sem.clone().acquire_owned().await.ok(),
            None => None,
        }
    }
}

/// Capped exponential backoff for a failing `accept`.
///
/// Capped because an uncapped one turns a transient blip into minutes of
/// unavailability; reset on success because the next failure is a new problem,
/// not a continuation of the last one.
#[derive(Default)]
struct AcceptBackoff {
    /// Zero means "no failure yet"; the next pause starts from `FIRST_MS`.
    current_ms: u64,
}

impl AcceptBackoff {
    const FIRST_MS: u64 = 5;
    const MAX_MS: u64 = 1_000;

    fn reset(&mut self) {
        self.current_ms = 0;
    }

    fn next_pause(&mut self) -> std::time::Duration {
        self.current_ms = match self.current_ms {
            0 => Self::FIRST_MS,
            n => (n * 2).min(Self::MAX_MS),
        };
        std::time::Duration::from_millis(self.current_ms)
    }
}

/// When a connection last did anything, and whether it is doing something now.
///
/// hyper's HTTP/1 `keep_alive` knob is a `bool` — reuse the connection or do
/// not — with no idle bound. Without one, an accepted connection that goes
/// quiet is held until the client goes away, which costs a file descriptor and
/// a task each and is the cheapest way to exhaust a server. This is the state
/// the idle watchdog reads.
struct ConnActivity {
    /// Requests currently being served. Idle means zero: a handler slower than
    /// the keep-alive period is busy, and closing its connection would be a
    /// self-inflicted truncation.
    in_flight: std::sync::atomic::AtomicUsize,
    /// Milliseconds since `origin` at the end of the last request.
    last_ms: std::sync::atomic::AtomicU64,
    /// Whether any request has finished on this connection.
    ///
    /// Separate from `last_ms` rather than derived from it, because `last_ms` is
    /// a millisecond count from `origin` and a request served in under a
    /// millisecond stores `0` — indistinguishable from "nothing has run yet".
    /// The `keep_alive_ms == 0` close needs exactly that distinction, and it is
    /// the fast request that would get it wrong.
    served: std::sync::atomic::AtomicBool,
    /// Fixed reference point, so activity is a cheap integer rather than a
    /// mutex around an `Instant`.
    origin: tokio::time::Instant,
    /// Whether anything ever waits on `request_finished`.
    ///
    /// Only the `keep_alive_ms == 0` branch of the connection loop does, and
    /// that is off by default. Without this flag every completed request paid
    /// an atomic read-modify-write to store a permit no one would ever consume.
    notify_on_finish: bool,
    /// Signalled when a request finishes, for the `keep_alive_ms == 0` close.
    ///
    /// `notify_one` rather than `notify_waiters`, because the two are not
    /// equivalent under a `select!`: the loop re-creates its `notified()` future
    /// on every iteration, so a wake delivered while it was polling another
    /// branch would be lost. `notify_one` stores a permit instead, and the next
    /// `notified()` consumes it — the completion cannot be missed whatever the
    /// loop happened to be doing.
    request_finished: tokio::sync::Notify,
}

/// Holds a connection's "a request is in flight" count for as long as it lives.
///
/// Dropping it records the activity timestamp and decrements. Being a guard
/// rather than a pair of calls is the point: the future it lives in can be
/// cancelled, and a leaked increment silently exempts the connection from every
/// idle and shutdown bound there is.
struct InFlight(std::sync::Arc<ConnActivity>);

impl InFlight {
    fn new(activity: std::sync::Arc<ConnActivity>) -> Self {
        activity
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(activity)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.last_ms.store(
            self.0.origin.elapsed().as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.0
            .served
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.0
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        // After the decrement, so a loop woken by this observes the count it is
        // about to act on rather than the one that was still standing. Skipped
        // entirely when no branch of the connection loop is listening — see
        // `notify_on_finish`.
        if self.0.notify_on_finish {
            self.0.request_finished.notify_one();
        }
    }
}

/// The body the engine hands to hyper, and the [`InFlight`] guard that has to
/// outlive it.
///
/// One type doing both jobs, because doing them in two cost two heap
/// allocations per response and the common case needs neither. A buffered
/// response used to be boxed into an `UnsyncBoxBody` by `into_boxed_body` and
/// then boxed *again* by `attach_guard` to carry the guard — two allocations
/// and two layers of dynamic dispatch to wrap a `Bytes` that was already in
/// hand. Here the buffered case is a plain `Full<Bytes>` in an enum arm and the
/// guard is a field, so the whole thing is a move.
///
/// `size_hint` and `is_end_stream` delegate rather than being left to their
/// defaults: hyper frames a response with `Content-Length` only when the body
/// can state its exact size, so a wrong answer here silently turns every
/// response chunked.
pub(crate) struct EngineBody {
    inner: EngineBodyInner,
    /// Dropped with the body — after the last frame, or when the client goes
    /// away mid-transfer. `None` on the responses written before a connection
    /// guard exists.
    _guard: Option<InFlight>,
}

enum EngineBodyInner {
    /// A payload already in memory. No boxing: this is the overwhelmingly
    /// common shape and it is `Unpin`, `Sized`, and known-length.
    Full(Full<Bytes>),
    /// A lazily-produced payload. Still boxed — a stream is a `dyn Stream`
    /// either way — but now boxed once instead of twice.
    Stream(UnsyncBoxBody<Bytes, std::io::Error>),
}

impl EngineBody {
    /// Wrap a Churust [`Body`] for the wire.
    fn new(body: Body) -> Self {
        let inner = match body {
            Body::Bytes(bytes) => EngineBodyInner::Full(Full::new(bytes)),
            Body::Stream(stream) => {
                let frames = stream.map(|chunk| {
                    chunk
                        .map(Frame::data)
                        .map_err(|e| std::io::Error::other(e.to_string()))
                });
                EngineBodyInner::Stream(StreamBody::new(frames).boxed_unsync())
            }
        };
        Self {
            inner,
            _guard: None,
        }
    }

    /// A constant body, for the refusals the engine writes itself.
    fn from_static(bytes: &'static [u8]) -> Self {
        Self::new(Body::Bytes(Bytes::from_static(bytes)))
    }

    /// Keep `guard` alive until this body has been fully written.
    ///
    /// A field assignment, where the previous shape allocated a box to hold the
    /// same guard.
    fn with_guard(mut self, guard: InFlight) -> Self {
        self._guard = Some(guard);
        self
    }
}

impl hyper::body::Body for EngineBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<Frame<Bytes>, std::io::Error>>> {
        // Both arms are `Unpin`, so this needs no projection and no `unsafe` —
        // which the workspace forbids outright.
        match &mut self.get_mut().inner {
            EngineBodyInner::Full(full) => std::pin::Pin::new(full)
                .poll_frame(cx)
                .map(|opt| opt.map(|res| res.map_err(|never| match never {}))),
            EngineBodyInner::Stream(boxed) => std::pin::Pin::new(boxed).poll_frame(cx),
        }
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        match &self.inner {
            EngineBodyInner::Full(full) => full.size_hint(),
            EngineBodyInner::Stream(boxed) => boxed.size_hint(),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.inner {
            EngineBodyInner::Full(full) => full.is_end_stream(),
            EngineBodyInner::Stream(boxed) => boxed.is_end_stream(),
        }
    }
}

impl ConnActivity {
    fn new(notify_on_finish: bool) -> Self {
        Self {
            notify_on_finish,
            in_flight: std::sync::atomic::AtomicUsize::new(0),
            last_ms: std::sync::atomic::AtomicU64::new(0),
            served: std::sync::atomic::AtomicBool::new(false),
            origin: tokio::time::Instant::now(),
            request_finished: tokio::sync::Notify::new(),
        }
    }

    /// Whether a request is being served right now.
    fn busy(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed) > 0
    }

    /// Whether any request on this connection has finished.
    ///
    /// Set when an [`InFlight`] guard drops, which is after the response body
    /// has been written.
    ///
    /// Used by the `keep_alive_ms == 0` branch of the connection loop, which must
    /// close *after* a response and not before the first one. Deliberately not
    /// `last_ms > 0`: that is a millisecond count from the connection's own
    /// start, so a request served in under a millisecond records `0` and would
    /// read as "nothing has run yet" — leaving the fast connections, which is to
    /// say most of them, never closed.
    fn has_served(&self) -> bool {
        self.served.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Resolves when a request has finished since the last time it was awaited.
    async fn request_finished(&self) {
        self.request_finished.notified().await;
    }

    /// `None` if the connection has been idle for at least `keep_alive_ms`,
    /// otherwise how much longer to wait before asking again.
    fn idle_for(&self, keep_alive_ms: u64) -> Option<std::time::Duration> {
        if self.in_flight.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            // Busy. Check again a full period from now rather than computing a
            // deadline against a request that has not finished.
            return Some(std::time::Duration::from_millis(keep_alive_ms));
        }
        let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
        let idle_ms = (self.origin.elapsed().as_millis() as u64).saturating_sub(last);
        match keep_alive_ms.checked_sub(idle_ms) {
            Some(0) | None => None,
            Some(remaining) => Some(std::time::Duration::from_millis(remaining)),
        }
    }
}

/// The 24 bytes an HTTP/2 client sends before anything else (RFC 9113 §3.4).
const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Whether `auto::Builder` has finished deciding which protocol this is.
///
/// The two header deadlines the engine configures — `http1().header_read_timeout`
/// and the h2 keep-alive ping — belong to a `Connection` that hyper-util only
/// builds *after* it has sniffed the preface, and the sniffing future itself
/// (`ReadVersion`) holds no timer: it is a bare read of up to 24 bytes. A peer
/// that connects and sends nothing, or that sends 23 of the 24 preface bytes,
/// parks there holding a connection permit and a drain token with no deadline
/// on it at all.
///
/// Reading this flag is what lets the connection loop bound that phase and stop
/// bounding it the moment a protocol exists — which matters, because a live
/// HTTP/2 client is allowed to sit idle far longer than the header deadline
/// once it has handshaken.
#[derive(Default)]
struct Negotiated(std::sync::atomic::AtomicBool);

impl Negotiated {
    fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Wraps the socket to watch the bytes `ReadVersion` reads, and nothing else.
///
/// The condition below mirrors hyper-util's: it stops sniffing on the first
/// read that diverges from the preface, on end of file, and once all 24 bytes
/// have matched. Mirroring is safe to do here because the preface is a protocol
/// constant rather than a hyper internal — hyper-util cannot decide differently
/// without HTTP/2 itself changing.
///
/// Once `flag` is set the wrapper is a pure delegate; the comparison costs at
/// most 24 byte tests per connection.
struct Sniffing<S> {
    inner: S,
    flag: std::sync::Arc<Negotiated>,
    /// Preface bytes matched so far, across however many reads they arrived in.
    matched: usize,
}

impl<S> Sniffing<S> {
    fn new(inner: S, flag: std::sync::Arc<Negotiated>) -> Self {
        Self {
            inner,
            flag,
            matched: 0,
        }
    }

    /// Feed the bytes a single read produced.
    fn observe(&mut self, fresh: &[u8]) {
        // End of file. hyper-util stops sniffing here too, and the connection
        // is about to end on its own.
        if fresh.is_empty() {
            self.flag.set();
            return;
        }
        for &byte in fresh {
            if byte != H2_PREFACE[self.matched] {
                // Not HTTP/2: hyper-util builds an HTTP/1 connection, whose own
                // header deadline takes over from here.
                self.flag.set();
                return;
            }
            self.matched += 1;
            if self.matched == H2_PREFACE.len() {
                self.flag.set();
                return;
            }
        }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Sniffing<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(polled, std::task::Poll::Ready(Ok(()))) && !this.flag.get() {
            // Copied out because `observe` takes `&mut self` while `buf` is
            // still borrowed from the read above.
            let fresh: Vec<u8> = buf.filled()[before..].to_vec();
            this.observe(&fresh);
        }
        polled
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Sniffing<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    // Delegated rather than left to the default: hyper writes a response head
    // and body as separate slices, and losing vectored writes here would turn
    // every response into an extra syscall.
    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// Per-connection settings, resolved once from the app config.
///
/// Grouped rather than passed individually: nine positional parameters is a
/// call site nobody can read.
#[derive(Clone, Copy)]
struct ConnSettings {
    max_body: usize,
    timeout_ms: u64,
    max_headers: usize,
    header_read_timeout_ms: u64,
    keep_alive_ms: u64,
    h2_max_header_list_size: u32,
    h2_max_concurrent_streams: u32,
    pipeline_flush: bool,
}

impl ConnSettings {
    fn from(cfg: &crate::app::ServerConfig) -> Self {
        Self {
            max_body: cfg.max_body_bytes,
            timeout_ms: cfg.request_timeout_ms,
            max_headers: cfg.max_headers,
            header_read_timeout_ms: cfg.header_read_timeout_ms,
            keep_alive_ms: cfg.keep_alive_ms,
            h2_max_header_list_size: cfg.h2_max_header_list_size,
            h2_max_concurrent_streams: cfg.h2_max_concurrent_streams,
            pipeline_flush: cfg.pipeline_flush,
        }
    }
}

/// Serve `app` on `addrs` across `workers` independent single-threaded
/// runtimes, until `shutdown` resolves. Blocks the calling thread.
///
/// # Why a second way to serve
///
/// The ordinary path ([`serve`], [`serve_many`]) runs every connection on one
/// shared work-stealing runtime. That is the right default: it balances
/// perfectly under uneven load, and one slow handler cannot monopolise a core.
/// It also means a connection's read wakeup, its handler and its write can each
/// land on a different thread, and each of those hops is an atomic operation, a
/// cross-core cache miss and — often — an `unpark` syscall.
///
/// Measured on this project's comparison harness, that overhead was the
/// difference between 8.7µs and 21.7µs of server CPU per request: the same work
/// costing two and a half times as much because of where it ran rather than
/// what it did. Pinning a connection to one runtime for its whole life removes
/// the hops. It is the shape actix-web uses, and it is most of why actix-web is
/// faster than every hyper-based server it is compared with.
///
/// The trade is real and is the reason this is not the default: with
/// per-connection affinity a runtime whose connections happen to be busy cannot
/// borrow an idle one's core. Choose it for many short, uniform requests;
/// keep the default for uneven or long-running work.
///
/// # Why one acceptor rather than `SO_REUSEPORT`
///
/// The obvious implementation gives each worker its own listener on the same
/// address. On Linux that distributes connections across them; on macOS and the
/// BSDs `SO_REUSEPORT` means only "several sockets may bind here" and the last
/// bind takes essentially everything. Measured on macOS, a twelve-worker
/// `SO_REUSEPORT` build served exactly what a one-worker build served, because
/// eleven of the twelve never received a connection. One accept loop handing
/// sockets out in round-robin behaves the same everywhere, and accepting is one
/// syscall against a connection's whole lifetime.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if any address cannot be bound, or if a worker
/// thread cannot be started. Binding happens before any worker starts, so a
/// failure leaves nothing half-serving.
pub fn serve_sharded<F>(
    app: App,
    addrs: Vec<SocketAddr>,
    workers: usize,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    if addrs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no addresses to bind",
        ));
    }
    refuse_tls_without_the_feature(&app)?;
    let workers = workers.max(1);

    let backlog = app.config().backlog;
    // One budget for the process however the connections are distributed.
    let limits = std::sync::Arc::new(AcceptLimits::from(app.config()));

    // On Linux the kernel can do the distribution, so the workers each own a
    // listener and run the ordinary accept loop — the same `serve_listener`
    // the shared engine uses, with its TLS branch, its backoff and its drain,
    // rather than a second implementation of all three.

    // Linux distributes accepted connections across SO_REUSEPORT listeners in
    // the kernel; nothing else here does. See `reuseport_listeners`.
    //
    // Except when the connection cap is too small to divide. Each worker
    // acquires a permit before accepting, so a budget shared across listeners
    // starves: one worker holds the permit, the kernel delivers the connection
    // to another, and nobody serves it. Splitting the budget per worker fixes
    // that and keeps the total bounded — but only while there is at least one
    // permit each to split. Below that the central acceptor is the only shape
    // that honours the number the operator configured, and honouring a
    // denial-of-service bound outranks the throughput of a configuration
    // nobody runs in production.
    #[cfg(target_os = "linux")]
    {
        let cap = app.config().max_connections;
        if cap == 0 || cap >= workers {
            let per_worker = if cap == 0 { 0 } else { cap / workers };
            let limits =
                std::sync::Arc::new(AcceptLimits::with_connections(app.config(), per_worker));
            serve_sharded_reuseport(app, addrs, workers, backlog, limits, shutdown)
        } else {
            serve_sharded_central(app, addrs, workers, backlog, limits, shutdown)
        }
    }
    #[cfg(not(target_os = "linux"))]
    #[cfg(not(target_os = "linux"))]
    {
        serve_sharded_central(app, addrs, workers, backlog, limits, shutdown)
    }
}

/// The Linux implementation: one `SO_REUSEPORT` listener per worker, each
/// running the ordinary accept loop.
#[cfg(target_os = "linux")]
fn serve_sharded_reuseport<F>(
    app: App,
    addrs: Vec<SocketAddr>,
    workers: usize,
    backlog: u32,
    limits: std::sync::Arc<AcceptLimits>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (stop, _) = tokio::sync::broadcast::channel::<()>(1);
    let mut handles = Vec::with_capacity(workers * addrs.len());

    for addr in &addrs {
        for listener in reuseport_listeners(*addr, backlog, workers)? {
            let app = app.replica();
            let limits = limits.clone();
            let mut rx = stop.subscribe();
            handles.push(
                std::thread::Builder::new()
                    .name(format!("churust-worker-{}", handles.len()))
                    .spawn(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                tracing::error!(error = %e, "worker runtime could not be built");
                                return;
                            }
                        };
                        rt.block_on(async move {
                            let listener = match tokio::net::TcpListener::from_std(listener) {
                                Ok(l) => l,
                                Err(e) => {
                                    tracing::error!(error = %e, "worker listener not adopted");
                                    return;
                                }
                            };
                            let _ = serve_listener(app, listener, limits, async move {
                                let _ = rx.recv().await;
                            })
                            .await;
                        });
                    })?,
            );
        }
    }

    // A runtime just to await the caller's shutdown future and fan it out.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(shutdown);
    let _ = stop.send(());
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// Accept centrally and hand each socket to a worker.
///
/// The only shape off Linux, where `SO_REUSEPORT` does not distribute, and the
/// shape on Linux too when the connection cap is smaller than the worker count
/// — see `serve_sharded`.
fn serve_sharded_central<F>(
    app: App,
    addrs: Vec<SocketAddr>,
    workers: usize,
    backlog: u32,
    limits: std::sync::Arc<AcceptLimits>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    // Bound before a single worker starts, for the reason `serve_many`
    // documents: a server that came up on half its addresses and said nothing
    // is worse than one that refused to start.
    let listeners = addrs
        .iter()
        .map(|addr| bind_tcp_std(*addr, backlog))
        .collect::<std::io::Result<Vec<_>>>()?;
    let conn_cfg = ConnSettings::from(app.config());
    let nodelay = app.config().tcp_nodelay;
    let shutdown_timeout_ms = app.config().shutdown_timeout_ms;

    // A certificate that cannot be loaded must stop the server rather than be
    // discovered per connection. Cloning a `TlsAcceptor` is an `Arc` clone, so
    // each worker holding one costs nothing.
    #[cfg(feature = "tls")]
    let tls_acceptor = match &app.config().tls {
        Some(t) => Some(acceptor_from_pem(&t.cert, &t.key)?),
        None => None,
    };

    // One handoff queue per worker. Unbounded because the thing that bounds
    // admission is the connection semaphore below, which is taken *before* the
    // accept — a second bound here would only decide which of two already-
    // admitted connections waits, and would let a momentarily busy worker
    // stall the accept loop for every other worker too.
    let mut senders = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers);

    for i in 0..workers {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Handoff>();
        senders.push(tx);
        // `replica`, not `clone`: each worker gets its own `AppInner`
        // allocation so the per-request refcount it touches is its own. See
        // `App::replica` for the measurement that made this worth doing.
        let app = app.replica();
        #[cfg(feature = "tls")]
        let acceptor = tls_acceptor.clone();
        let limits = limits.clone();
        handles.push(
            std::thread::Builder::new()
                .name(format!("churust-worker-{i}"))
                .spawn(move || {
                    worker_loop(
                        app,
                        rx,
                        conn_cfg,
                        shutdown_timeout_ms,
                        limits,
                        #[cfg(feature = "tls")]
                        acceptor,
                    )
                })?,
        );
    }

    // The acceptor gets a runtime of its own rather than borrowing a worker's:
    // a worker blocked on a slow handler must not also be the thing that stops
    // accepting.
    let accept_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    accept_rt.block_on(async move {
        let (stop, _) = tokio::sync::broadcast::channel::<()>(1);
        let mut loops = Vec::with_capacity(listeners.len());
        let senders = std::sync::Arc::new(senders);

        for listener in listeners {
            // Registered with this runtime's reactor now that there is one.
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(error = %e, "bound listener could not be adopted");
                    continue;
                }
            };
            let limits = limits.clone();
            let senders = senders.clone();
            let mut rx = stop.subscribe();
            loops.push(tokio::spawn(async move {
                shard_accept_loop(listener, limits, senders, nodelay, async move {
                    let _ = rx.recv().await;
                })
                .await
            }));
        }

        shutdown.await;
        let _ = stop.send(());
        for l in loops {
            let _ = l.await;
        }
    });

    // The senders were moved into the block above and are gone with it, which
    // is what tells each worker there will be no more connections; each then
    // drains its own live ones and returns.
    for h in handles {
        // A worker that panicked has already reported it. Joining anyway is
        // what makes this function's return mean "everything has stopped".
        let _ = h.join();
    }
    Ok(())
}

/// One listener per worker, all bound to the same address with `SO_REUSEPORT`.
///
/// Linux distributes accepted connections across such listeners in the kernel,
/// hashing by connection so each socket gets a share. That is strictly better
/// than accepting centrally and handing sockets to workers: no channel, no
/// re-registering the socket with a second runtime, and no acceptor thread
/// competing with the workers for the cores they were pinned to.
///
/// Deliberately Linux-only. On macOS and the BSDs `SO_REUSEPORT` means only
/// "several sockets may bind here" — the last bind takes essentially
/// everything, and a twelve-worker build measured exactly what a one-worker
/// build measured because eleven never received a connection. Those platforms
/// keep the central acceptor, which behaves the same everywhere at the cost of
/// the handoff.
#[cfg(target_os = "linux")]
fn reuseport_listeners(
    addr: SocketAddr,
    backlog: u32,
    n: usize,
) -> std::io::Result<Vec<std::net::TcpListener>> {
    (0..n)
        .map(|_| {
            let domain = match addr {
                SocketAddr::V4(_) => socket2::Domain::IPV4,
                SocketAddr::V6(_) => socket2::Domain::IPV6,
            };
            let socket = socket2::Socket::new(domain, socket2::Type::STREAM, None)?;
            socket.set_reuse_address(true)?;
            socket.set_reuse_port(true)?;
            socket.bind(&addr.into())?;
            socket.listen(backlog.min(i32::MAX as u32) as i32)?;
            socket.set_nonblocking(true)?;
            Ok(socket.into())
        })
        .collect()
}

/// One accepted connection on its way to a worker.
type Handoff = (std::net::TcpStream, SocketAddr, ConnSlot);

/// Accept on one listener and hand each connection to a worker in turn.
async fn shard_accept_loop<F>(
    listener: tokio::net::TcpListener,
    limits: std::sync::Arc<AcceptLimits>,
    senders: std::sync::Arc<Vec<tokio::sync::mpsc::UnboundedSender<Handoff>>>,
    nodelay: bool,
    shutdown: F,
) where
    F: Future<Output = ()>,
{
    let mut backoff = AcceptBackoff::default();
    let mut next = 0usize;
    tokio::pin!(shutdown);

    loop {
        let acquire = async {
            let slot = limits.connection_slot().await;
            (slot, listener.accept().await)
        };

        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            (slot, accepted) = acquire => {
                let (stream, peer) = match accepted {
                    Ok((stream, peer)) => {
                        backoff.reset();
                        if nodelay {
                            if let Err(e) = stream.set_nodelay(true) {
                            tracing::debug!(%peer, error = %e, "could not disable Nagle");
                            }
                        }
                        (stream, peer)
                    }
                    Err(e) => {
                        let pause = backoff.next_pause();
                        tracing::warn!(error = %e, pause_ms = pause.as_millis() as u64, "accept failed");
                        tokio::time::sleep(pause).await;
                        continue;
                    }
                };

                // Back to a plain socket for the trip across. A tokio
                // `TcpStream` is registered with the reactor of the runtime
                // that created it, and handing that registration to another
                // runtime is how a connection ends up never being polled
                // again. `into_std` deregisters it; the worker registers it
                // with its own.
                let std_stream = match stream.into_std() {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(%peer, error = %e, "could not detach accepted socket");
                        continue;
                    }
                };

                // Round-robin. Not least-loaded: knowing which worker is least
                // loaded costs a shared counter written on every connection by
                // every worker, and with connections that outlive a single
                // request the difference washes out anyway.
                //
                // A send only fails if that worker's runtime is gone, which is
                // a worker that panicked. Falling through to the next one keeps
                // the server serving on the survivors rather than dropping
                // every connection that hashes to a dead thread; the failed
                // send hands the payload back so it can be offered onward.
                let n = senders.len();
                let mut payload = (std_stream, peer, slot);
                let mut delivered = false;
                for attempt in 0..n {
                    match senders[(next + attempt) % n].send(payload) {
                        Ok(()) => {
                            delivered = true;
                            break;
                        }
                        Err(returned) => payload = returned.0,
                    }
                }
                if !delivered {
                    tracing::error!(%peer, "every worker is gone; dropping the connection");
                }
                next = next.wrapping_add(1);
            }
        }
    }
}

/// One worker: a single-threaded runtime that serves whatever the acceptor
/// hands it, for as long as the acceptor is alive.
fn worker_loop(
    app: App,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Handoff>,
    cfg: ConnSettings,
    shutdown_timeout_ms: u64,
    // Only the TLS build reads this: it carries the handshake budget, and
    // without `tls` there are no handshakes to bound.
    #[cfg_attr(not(feature = "tls"), allow(unused_variables))] limits: std::sync::Arc<AcceptLimits>,
    #[cfg(feature = "tls")] tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "worker runtime could not be built");
            return;
        }
    };

    rt.block_on(async move {
        let drain = Drain::new();
        while let Some((std_stream, peer, slot)) = rx.recv().await {
            if let Err(e) = std_stream.set_nonblocking(true) {
                tracing::debug!(%peer, error = %e, "could not set the socket non-blocking");
                continue;
            }
            match tokio::net::TcpStream::from_std(std_stream) {
                Ok(stream) => {
                    // TLS terminates here, on the worker, rather than on the
                    // acceptor: a handshake is expensive to answer and cheap to
                    // ask for, so doing it on the single accept loop would let
                    // one peer's slow ClientHello stall every other connection's
                    // admission. The budget and the deadline are the shared
                    // engine's, applied the same way — the deadline covers the
                    // wait for the budget as well as the handshake, because what
                    // is being bounded is how long an unproven peer may hold a
                    // connection permit.
                    #[cfg(feature = "tls")]
                    if let Some(acceptor) = tls_acceptor.clone() {
                        let app = app.clone();
                        let token = drain.token();
                        let handshakes = limits.handshakes.clone();
                        let deadline = limits.tls_handshake_timeout;
                        tokio::spawn(async move {
                            let queue_and_shake = async {
                                let permit = match &handshakes {
                                    Some(sem) => sem.clone().acquire_owned().await.ok(),
                                    None => None,
                                };
                                (acceptor.accept(stream).await, permit)
                            };
                            let (accepted, permit) = match deadline {
                                Some(limit) => {
                                    match tokio::time::timeout(limit, queue_and_shake).await {
                                        Ok(pair) => pair,
                                        Err(_) => {
                                            tracing::debug!(%peer, "TLS handshake timed out");
                                            return;
                                        }
                                    }
                                }
                                None => queue_and_shake.await,
                            };
                            match accepted {
                                Ok(tls) => {
                                    drop(permit);
                                    serve_stream(app, tls, cfg, peer, token, slot).await;
                                }
                                Err(e) => {
                                    tracing::debug!(%peer, error = %e, "TLS handshake failed")
                                }
                            }
                        });
                        continue;
                    }
                    serve_stream(app.clone(), stream, cfg, peer, drain.token(), slot).await;
                }
                Err(e) => tracing::debug!(%peer, error = %e, "could not adopt handed-over socket"),
            }
        }
        // The acceptor is gone. Finish what is still in flight.
        drain.wait(shutdown_timeout_ms).await;
    });
}

/// Serve on several addresses at once, until `shutdown` resolves.
///
/// Useful for binding IPv4 and IPv6 separately, or for exposing an admin port
/// alongside the public one. Each address gets its own accept loop; the first
/// bind failure aborts, since a server that silently came up on half its
/// addresses is worse than one that refused to start.
pub async fn serve_many<F>(app: App, addrs: Vec<SocketAddr>, shutdown: F) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    if addrs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no addresses to bind",
        ));
    }
    refuse_tls_without_the_feature(&app)?;

    // Bind everything first, so a failure is reported *before* any address
    // starts serving. Binding inside the spawned tasks meant a failure was not
    // observed until after `shutdown.await` — the process ran happily on the
    // addresses that did work, said nothing, and surfaced the error only on the
    // way out. That is exactly the half-up server the doc comment above rules
    // out.
    let mut listeners = Vec::with_capacity(addrs.len());
    for addr in addrs {
        match bind_tcp(addr, app.config().backlog) {
            Ok(l) => listeners.push(l),
            Err(e) => {
                tracing::error!(%addr, error = %e, "bind failed; not starting");
                return Err(e);
            }
        }
    }

    // One shutdown signal fans out to every loop.
    let (tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let mut tasks = Vec::with_capacity(listeners.len());

    // One budget for the process, not one per listener. Constructing it inside
    // each accept loop multiplied `max_connections` and `max_tls_handshakes` by
    // the number of bound addresses — binding v4 and v6 quietly doubled a cap
    // documented as bounding "what the process serves at once".
    let limits = std::sync::Arc::new(AcceptLimits::from(app.config()));

    for listener in listeners {
        let app = app.clone();
        let limits = limits.clone();
        let mut rx = tx.subscribe();
        tasks.push(tokio::spawn(async move {
            serve_listener(app, listener, limits, async move {
                let _ = rx.recv().await;
            })
            .await
        }));
    }

    shutdown.await;
    let _ = tx.send(());

    let mut first_err: Option<std::io::Error> = None;
    for t in tasks {
        let outcome = match t.await {
            Ok(r) => r,
            Err(e) => Err(std::io::Error::other(format!("listener task failed: {e}"))),
        };
        if let Err(e) = outcome {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Serve on a Unix domain socket instead of TCP.
///
/// Useful behind a local reverse proxy: no TCP stack and no port to firewall.
///
/// The socket path is unlinked first if a *stale* file is present — a crashed
/// process leaves one behind, and binding would otherwise fail forever. Stale
/// is established rather than assumed: a socket node that still has a listener
/// is left alone and the bind fails with [`AddrInUse`], so a second instance
/// started on a path already in use refuses instead of taking it over.
///
/// # Permissions
///
/// The socket node is created under the process umask and this function does
/// not chmod it. Do not rely on the node's own mode for access control: whether
/// the permission bits on a socket are consulted at `connect(2)` is
/// platform-dependent — Linux enforces them, some BSDs historically did not.
/// What is enforced everywhere is path resolution, so put the socket in a
/// directory whose permissions you control, and set the umask before calling if
/// the node's own mode matters to you. Adjusting it afterwards is racy: the
/// socket accepts connections from the moment it is bound.
///
/// Unix only; there is no Windows equivalent.
///
/// [`AddrInUse`]: std::io::ErrorKind::AddrInUse
#[cfg(unix)]
pub async fn serve_unix<F>(
    app: App,
    path: impl AsRef<std::path::Path>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let path = path.as_ref();
    // A leftover socket node from a crash blocks bind with EADDRINUSE, so one
    // has to go — but only once it is known to be dead. The unlink used to be
    // unconditional, which meant a second `serve_unix` on a path an instance
    // was already serving deleted that instance's node and bound its own: the
    // first process kept running on an inode nothing could reach any more,
    // still reporting itself healthy, while every new connection went to the
    // second. Probing with a connect distinguishes the two cases the way any
    // other server does it — a refused connection means nobody is listening,
    // and an accepted one means the path is genuinely in use.
    // A Unix socket carries no TLS, and this listener never consulted the
    // setting — so an application configured with `tls` served *plaintext* here
    // while `apply_security_headers` kept asserting HSTS on every response,
    // because that gate reads `config.tls.is_some()` rather than the transport.
    // The result was a cleartext service telling its clients it was
    // HTTPS-only.
    //
    // Refused rather than downgraded. The two settings contradict each other —
    // "terminate TLS with this certificate" and "listen on a Unix socket" cannot
    // both be honoured — and silently picking one is how the mismatch went
    // unnoticed. An application fronted by a TLS-terminating proxy should not be
    // configuring `tls` at all; one that is has said something it does not mean.
    if app.config().tls.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "this app is configured for TLS, which a Unix socket cannot carry: \
             serving it here would be cleartext while still advertising HSTS. \
             Drop the `tls` configuration to serve over a Unix socket, or use \
             `serve`/`start` to terminate TLS on a TCP listener.",
        ));
    }

    unlink_if_stale(path).await?;

    let listener = bind_unix(path, app.config().backlog)?;
    // Remember which inode this bind produced. At shutdown the node at `path`
    // may no longer be ours, and removing whatever happens to be there then
    // would leave a live successor serving a path with no socket on it. See
    // the unlink at the end of this function.
    let bound = socket_identity(path);
    let conn_cfg = ConnSettings::from(app.config());
    let shutdown_timeout_ms = app.config().shutdown_timeout_ms;

    let drain = Drain::new();
    let limits = std::sync::Arc::new(AcceptLimits::from(app.config()));
    let mut backoff = AcceptBackoff::default();
    let mut shutdown = std::pin::pin!(shutdown);

    // A Unix peer has no IP; report the loopback address so `peer_addr` has a
    // consistent shape rather than being absent only on this transport.
    let peer = std::net::SocketAddr::from(([127, 0, 0, 1], 0));

    loop {
        // Same shape as the TCP loop: a saturated budget must not outrank the
        // shutdown signal. See the comment there.
        let acquire = async {
            let slot = limits.connection_slot().await;
            (slot, listener.accept().await)
        };

        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            (slot, accepted) = acquire => {
                let (stream, _) = match accepted {
                    Ok(s) => {
                        backoff.reset();
                        s
                    }
                    Err(e) => {
                        let pause = backoff.next_pause();
                        tracing::warn!(error = %e, pause_ms = pause.as_millis() as u64, "accept failed");
                        tokio::time::sleep(pause).await;
                        continue;
                    }
                };
                serve_stream(app.clone(), stream, conn_cfg, peer, drain.token(), slot).await;
            }
        }
    }

    drain.wait(shutdown_timeout_ms).await;
    // Leave no socket file behind for the next start to trip over — but only
    // our own. If something else has taken the path over in the meantime, the
    // node sitting there belongs to a listener that is still serving, and
    // deleting it would take the successor off the air without either process
    // noticing: it goes on accepting on an inode with no name, and the next
    // client to resolve the path finds nothing. Comparing device and inode is
    // exact where comparing paths is not.
    if bound.is_some() && bound == socket_identity(path) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Identify the node currently at `path` by device and inode.
///
/// `None` means there is nothing there, or that it could not be stated — either
/// way it is not something this process should claim, so the callers treat it
/// as "not ours". `symlink_metadata` rather than `metadata`: a symlink pointing
/// at the socket is a different node from the socket, and following it would
/// let a link swapped in under us stand in for the real thing.
#[cfg(unix)]
fn socket_identity(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::symlink_metadata(path).ok()?;
    Some((md.dev(), md.ino()))
}

/// Clear `path` for a fresh bind, refusing to disturb a socket still in use.
///
/// Returns `AddrInUse` when the path already has a listener behind it. Anything
/// else there is removed, as before: a socket node that refuses connections is
/// the leftover this exists to clean up, and a non-socket file at the path is
/// removed too, because that is the behaviour `serve_unix` has always had and
/// bind would fail on it regardless.
#[cfg(unix)]
async fn unlink_if_stale(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let Ok(md) = std::fs::symlink_metadata(path) else {
        return Ok(()); // nothing in the way
    };

    // Only a socket can have a listener, so only a socket is worth probing.
    // The probe is a real `connect(2)`, which is the only way to tell a live
    // socket from an abandoned node: both look identical on disk, since neither
    // a crash nor a plain `drop` of the listener unlinks the name.
    if md.file_type().is_socket() && tokio::net::UnixStream::connect(path).await.is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "{} is already being served by another listener",
                path.display()
            ),
        ));
    }

    // The removal is reported now rather than swallowed, because the bind that
    // follows would fail with a bare EADDRINUSE that says nothing about why the
    // path could not be cleared. A node that vanished between the stat and the
    // removal is not a failure: the path is clear, which is all this wanted.
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

async fn serve_stream<S>(
    app: App,
    stream: S,
    cfg: ConnSettings,
    peer: std::net::SocketAddr,
    token: DrainToken,
    slot: ConnSlot,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // Taken as the raw stream rather than as an already-wrapped `TokioIo` so
    // the preface sniffing can be watched underneath it — see `Sniffing`.
    let negotiated = std::sync::Arc::new(Negotiated::default());
    let io = TokioIo::new(Sniffing::new(stream, negotiated.clone()));

    // The idle watchdog needs to know when this connection last did anything.
    // hyper exposes no per-request hook, but the service *is* the per-request
    // hook: every request on this connection passes through the closure below.
    let activity = std::sync::Arc::new(ConnActivity::new(cfg.keep_alive_ms == 0));
    // Subscribe before the token is moved into the guard: the connection loop
    // watches the shutdown signal, the guard owns the drain's release side.
    let mut signal = token.signal.clone();
    // One share of the budget for this connection, cloned into any upgraded
    // socket so the permit outlives the HTTP connection future.
    let guard = ConnGuard::new(slot, token);

    // Taken once for the connection. See `App::security_snapshot`.
    let security = app.security_snapshot();

    let svc = {
        let activity = activity.clone();
        let guard = guard.clone();
        service_fn(move |req: HyperRequest<Incoming>| {
            // The one `Arc` clone the request path makes. It is moved from here
            // all the way into the pipeline's terminal rather than cloned again
            // at each hop, which is what the rest of this path was reworked to
            // allow.
            let app = app.clone();
            let security = security.clone();
            let activity = activity.clone();
            let conn_guard = guard.clone();
            async move {
                // Busy, not idle: a handler slower than the keep-alive period
                // must not have its own connection closed underneath it.
                //
                // An RAII guard rather than paired begin/end calls, because a
                // request future can be *dropped* mid-await — an HTTP/2 client
                // sending RST_STREAM makes hyper do exactly that — and a missed
                // decrement pinned the connection as busy forever, exempt from
                // the idle watchdog and from the shutdown linger.
                let in_flight = InFlight::new(activity);
                let res = handle(
                    app,
                    security,
                    req,
                    cfg.max_body,
                    cfg.timeout_ms,
                    peer,
                    conn_guard,
                )
                .await;
                // The guard rides on the response body: the connection is still
                // working while bytes are being written, and releasing it when
                // the handler returned meant a large download or an SSE stream
                // counted as idle and was dropped during shutdown.
                res.map(|r| r.map(|body| body.with_guard(in_flight)))
            }
        })
    };
    // One builder that negotiates HTTP/1.1 or HTTP/2 per connection: h2 over
    // TLS via ALPN, and h2c prior-knowledge in plaintext. The v1 engine used
    // `http1::Builder` directly with a note that `auto::Builder`'s
    // `serve_connection` returned a future the spawn closure could not own —
    // `serve_connection_with_upgrades` returns an owned one, which is what
    // makes this possible while keeping the WebSocket upgrade path working.
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder.http1().max_headers(cfg.max_headers);
    // `max_headers` is HTTP/1 only — it counts headers, and h2 has no count,
    // only an encoded size. Both protocols need bounding, and one knob cannot
    // do it.
    builder
        .http2()
        .max_header_list_size(cfg.h2_max_header_list_size);
    // An h2 connection multiplexes many requests, so without this one
    // connection is an unbounded amount of concurrent work.
    builder.http2().max_concurrent_streams(
        (cfg.h2_max_concurrent_streams > 0).then_some(cfg.h2_max_concurrent_streams),
    );
    // `0` means no connection reuse: answer and close. Any other value means
    // reuse *bounded by an idle timeout*, which hyper has no knob for — see
    // the watchdog branch in the connection loop below.
    builder.http1().keep_alive(cfg.keep_alive_ms > 0);
    // One flush for a whole batch of pipelined replies rather than one per
    // reply. Off unless asked for — see `ServerConfig::pipeline_flush` for why
    // it is the wrong default for a client that does not pipeline.
    builder.http1().pipeline_flush(cfg.pipeline_flush);
    // Without this a client can hold a connection open indefinitely by
    // dribbling header bytes: the per-request timeout cannot help, because
    // there is no request until the header block is complete.
    if cfg.header_read_timeout_ms > 0 {
        // hyper panics if a timeout is configured without a timer to drive it.
        let deadline = std::time::Duration::from_millis(cfg.header_read_timeout_ms);
        builder.http1().timer(hyper_util::rt::TokioTimer::new());
        builder.http1().header_read_timeout(deadline);
        builder.http2().timer(hyper_util::rt::TokioTimer::new());
        // HTTP/2 has no header-read deadline to set: a header block arrives in
        // frames on an already-open connection, so hyper offers no equivalent
        // knob. Its equivalent question — is this peer actually still there? —
        // is answered by a keep-alive PING, which hyper leaves disabled by
        // default. Without it the documented slow-loris defence covered only
        // one of the two protocols served on the port: a peer that completed
        // the preface and then dribbled a partial HEADERS frame was bounded by
        // nothing but the idle watchdog, at `keep_alive_ms` rather than at the
        // 10s this knob advertises, while holding a connection permit.
        //
        // Ping at the deadline, drop at the deadline again, so a stalled peer
        // is gone within roughly twice the configured value and a live one that
        // simply has nothing to say answers and stays.
        builder.http2().keep_alive_interval(deadline);
        builder.http2().keep_alive_timeout(deadline);
    }

    // The connection borrows the builder, which is the ownership problem the v1
    // engine hit and worked around by using `http1::Builder` instead. Moving
    // the builder into the task resolves it: the task owns both.
    tokio::spawn(async move {
        // `with_upgrades` is required for WebSockets and harmless otherwise: an
        // HTTP/2 connection never carries an HTTP/1 upgrade.
        let conn = builder.serve_connection_with_upgrades(io, svc);
        let mut conn = std::pin::pin!(conn);
        let mut winding_down = false;

        // The idle deadline. Re-armed on every wake that finds the connection
        // busy or recently active, so the common case costs one timer per
        // connection rather than one per request.
        //
        // `keep_alive_ms == 0` is handled by its own branch below rather than by
        // this timer, because it is not a deadline: it means "answer and close",
        // so the moment to act is a request finishing, not a period elapsing.
        let idle_enabled = cfg.keep_alive_ms > 0;
        let zero_keep_alive = cfg.keep_alive_ms == 0;
        let idle = tokio::time::sleep(std::time::Duration::from_millis(if idle_enabled {
            cfg.keep_alive_ms
        } else {
            // Parked: the branch is disabled, but `select!` still needs a
            // future to name.
            u64::MAX / 2
        }));
        let mut idle = std::pin::pin!(idle);

        // Built once, not once per iteration. `select!` re-evaluates its branch
        // expressions every pass, and `wait_for` registers a waiter on the
        // watch's notify list when polled and removes it when dropped — two
        // mutex round-trips, paid on every connection wake, which at these
        // request rates means paid per request. The signal fires at most once
        // and the branch disables itself afterwards, so one future covers the
        // connection's whole life.
        let shutdown_signal = async move {
            let _ = signal.wait_for(|fired| *fired).await;
        };
        let mut shutdown_signal = std::pin::pin!(shutdown_signal);

        // Parked until a shutdown signal arms it; see the signal branch below.
        let linger = tokio::time::sleep(std::time::Duration::from_secs(3_600));
        let mut linger = std::pin::pin!(linger);
        let mut lingering = false;

        // The deadline for choosing a protocol at all. Both header deadlines
        // configured above belong to a connection hyper-util has not built yet,
        // and the sniffing it does first carries no timer, so this is the only
        // thing standing between a silent socket and a permit held for the life
        // of the process — the idle watchdog is a backstop at `keep_alive_ms`
        // rather than at the advertised deadline, and at `keep_alive_ms` of 0 it
        // deliberately waits for a first response before closing anything, so it
        // does not cover this phase either.
        //
        // It stops applying at negotiation, not at the first request: an HTTP/2
        // client that has handshaken is entitled to idle for far longer than
        // the header deadline, and the h2 keep-alive ping is what asks whether
        // it is still there.
        let mut negotiation_armed = cfg.header_read_timeout_ms > 0;
        let negotiation =
            tokio::time::sleep(std::time::Duration::from_millis(if negotiation_armed {
                cfg.header_read_timeout_ms
            } else {
                // Parked, like the linger above: the branch is disabled, but
                // `select!` still needs a future to name.
                u64::MAX / 2
            }));
        let mut negotiation = std::pin::pin!(negotiation);

        loop {
            // The negotiation deadline bounds how long a peer may take to
            // *choose* a protocol. Once one is chosen the question is settled,
            // but the branch below only disarmed itself when the timer fired —
            // so for the remainder of `header_read_timeout_ms`, every wake on
            // an already-negotiated connection polled a timer that could no
            // longer do anything. At these request rates that is a poll per
            // request. Reading the flag here settles it on the first wake after
            // negotiation instead.
            if negotiation_armed && negotiated.get() {
                negotiation_armed = false;
            }

            tokio::select! {
                // Bias the connection: with both branches ready, finishing the
                // request in hand beats re-checking a signal we have already
                // acted on.
                biased;
                _ = conn.as_mut() => break,
                // `wait_for` rather than `changed` so a signal that fired
                // before this task got scheduled is still seen. Never polled
                // again once it has fired: `winding_down` disables the branch.
                _ = shutdown_signal.as_mut(), if !winding_down => {
                    winding_down = true;
                    // Finish the in-flight request, refuse further ones on this
                    // connection, then let the loop poll it to completion.
                    conn.as_mut().graceful_shutdown();

                    // A connection with nothing in flight has nothing to drain,
                    // but it will not necessarily close on its own: an idle
                    // HTTP/2 connection is shut down by sending GOAWAY and then
                    // waiting for the *peer* to close, which an idle peer never
                    // does. Left alone, every shutdown costs the full grace
                    // period — a rolling restart would pay it every time.
                    //
                    // So give the GOAWAY (or the HTTP/1 close) a brief window to
                    // reach the wire and then stop. Only for connections with no
                    // request in flight; a busy one still gets the full grace.
                    if !activity.busy() {
                        linger.as_mut().reset(
                            tokio::time::Instant::now() + GOAWAY_LINGER,
                        );
                        lingering = true;
                    }
                }
                // The linger elapsed on an idle connection: drop it.
                _ = linger.as_mut(), if lingering => {
                    if !activity.busy() {
                        break;
                    }
                    // A request arrived between arming this and now — rare.
                    // Re-check later rather than clearing `winding_down`, which
                    // would let the shutdown branch fire again and turn this
                    // into a 250ms cycle of repeated `graceful_shutdown` calls.
                    // The grace period still bounds the wait overall.
                    linger
                        .as_mut()
                        .reset(tokio::time::Instant::now() + GOAWAY_LINGER);
                }
                // The peer never got as far as a protocol. Wound down the same
                // way the idle watchdog does rather than dropped outright, so
                // the socket is closed by hyper and anything already buffered
                // reaches the wire.
                _ = negotiation.as_mut(), if negotiation_armed && !winding_down => {
                    // `select!` evaluates a branch's precondition when it is
                    // *entered*, not when the branch fires. This one is entered
                    // before the peer has sent anything, so `negotiated` cannot
                    // be tested there: a connection that handshakes a
                    // millisecond later still arrives here with the branch
                    // armed, and closing it would drop every live HTTP/2 client
                    // that idles past the header deadline — which is exactly
                    // what `a_responsive_http2_client_is_not_dropped` exists to
                    // catch. Read the flag here, where it is current.
                    if negotiated.get() {
                        // Disarms the branch on the next pass, so a deadline
                        // that has already elapsed does not keep waking us.
                        negotiation_armed = false;
                    } else {
                        tracing::debug!(
                            %peer,
                            deadline_ms = cfg.header_read_timeout_ms,
                            "no protocol chosen within the header deadline; closing"
                        );
                        winding_down = true;
                        conn.as_mut().graceful_shutdown();
                        // Nothing has been served, so there is nothing to drain
                        // — but an idle connection does not necessarily close
                        // on its own, which is what this linger is for
                        // elsewhere too.
                        linger
                            .as_mut()
                            .reset(tokio::time::Instant::now() + GOAWAY_LINGER);
                        lingering = true;
                    }
                }
                // `keep_alive_ms == 0` means "answer and close". hyper does that
                // for HTTP/1 via `keep_alive(false)` above and has no h2
                // counterpart, and the idle watchdog used to be switched off
                // entirely at 0 — so an HTTP/2 connection got neither, and was
                // held for the life of the process. That is the "never expires"
                // reading `http3::idle_ms_for` refused for QUIC, and it made the
                // strictest setting available *weaker* than the 75s default.
                //
                // Woken by a request finishing rather than by a timer: the moment
                // to close is when nothing is in flight, and `InFlight`'s `Drop`
                // is what knows when that is. A timer would have to guess an
                // interval, and the obvious small one costs a wake every interval
                // for as long as a handler runs — measured at ~40/second against a
                // one-second handler, and `request_timeout_ms` allows thirty. This
                // is one wake per completed request instead.
                //
                // Gated on `has_served` so the pre-protocol phase stays the
                // negotiation deadline's job. A connection that negotiates and
                // never sends a request is left to the h2 keep-alive ping, as at
                // any other value of this knob.
                //
                // `!busy()` is belt-and-braces rather than what prevents a
                // truncation, and worth saying so: the wake arrives *after* the
                // finishing request's own count is gone, so with one request in
                // flight it is true either way, and an HTTP/2 connection that has
                // several would still drain them — `graceful_shutdown` finishes
                // in-flight streams, and the linger below re-checks `busy()`
                // before it breaks. Keeping the condition here means this branch
                // states its own precondition instead of inheriting it from
                // hyper's drain semantics and a check two branches away, either of
                // which could change without this one noticing.
                _ = activity.request_finished(), if zero_keep_alive && !winding_down => {
                    if activity.has_served() && !activity.busy() {
                        winding_down = true;
                        conn.as_mut().graceful_shutdown();
                        linger
                            .as_mut()
                            .reset(tokio::time::Instant::now() + GOAWAY_LINGER);
                        lingering = true;
                    }
                }
                _ = idle.as_mut(), if idle_enabled && !winding_down => {
                    match activity.idle_for(cfg.keep_alive_ms) {
                        // Nothing in flight and nothing recent: close it.
                        None => {
                            winding_down = true;
                            conn.as_mut().graceful_shutdown();
                            // Arm the linger here too. `graceful_shutdown` on an
                            // HTTP/2 connection sends GOAWAY and then waits for
                            // the peer, which an idle peer never answers — and
                            // with `winding_down` set every other branch is
                            // gated off, so the loop had nothing left to poll
                            // but a future that never resolves. The connection
                            // and its permit were then held for the life of the
                            // process; at the default cap that is a server that
                            // stops accepting anything at all.
                            linger
                                .as_mut()
                                .reset(tokio::time::Instant::now() + GOAWAY_LINGER);
                            lingering = true;
                        }
                        // Busy or recently active — wait out the remainder.
                        Some(remaining) => idle
                            .as_mut()
                            .reset(tokio::time::Instant::now() + remaining),
                    }
                }
            }
        }
        // The guard drops here — but only *this* clone. An upgraded WebSocket
        // holds its own, so the permit and the drain token are returned when
        // the socket task ends rather than when the 101 was dispatched.
        drop(guard);
    });
}

/// Reported when the grace period expires with requests still in flight.
///
/// Not an error: exiting on time is the point, and the alternative is hanging.
fn tracing_drain_timeout(ms: u64) {
    tracing::warn!(
        grace_ms = ms,
        "graceful shutdown timed out; exiting with requests in flight"
    );
}

/// Serve one request, and harden whatever comes back on the way out.
///
/// The hardening is here rather than only inside `respond` because `respond`
/// has several exits and two of them never reach the pipeline: a refusal is
/// composed and returned before `process_call` is ever called, so the
/// `SecurityHeaders` middleware — which is a pipeline middleware — could not
/// see it. A `413` for an oversized upload therefore went out with nothing but
/// `Connection: close`, while every other status the same server produced,
/// including a plain `404`, carried the full set.
///
/// Wrapping the whole function rather than patching each refusal is what keeps
/// that from happening again: any exit added later passes through here. It is
/// safe to run over a pipeline response too, because a header already present
/// is left alone, so the second application is a no-op.
async fn handle(
    app: App,
    security: Option<(crate::security::SecurityHeaders, bool)>,
    req: HyperRequest<Incoming>,
    max_body: usize,
    timeout_ms: u64,
    peer: std::net::SocketAddr,
    conn_guard: ConnGuard,
) -> Result<HyperResponse<EngineBody>, Infallible> {
    // `app` is moved, not cloned: the security set was taken from it once when
    // this connection was accepted, so nothing here needs it back afterwards.
    let mut res = respond(app, req, max_body, timeout_ms, peer, conn_guard).await?;
    // The `bool` is the builder's own certificate. This transport cannot tell
    // TLS from plaintext by itself — the stream arrives already decrypted, or
    // never was — so that is the only evidence available. HTTP/3 is the case
    // that can say `true` on its own.
    if let Some((set, over_tls)) = &security {
        set.apply_to(res.headers_mut(), *over_tls);
    }
    Ok(res)
}

/// Turn one hyper request into one hyper response.
///
/// Everything below is about *whether* to dispatch: the two refusals return
/// without ever reaching the pipeline. `handle` above is what makes the answer
/// look the same either way.
async fn respond(
    app: App,
    req: HyperRequest<Incoming>,
    max_body: usize,
    timeout_ms: u64,
    peer: std::net::SocketAddr,
    conn_guard: ConnGuard,
) -> Result<HyperResponse<EngineBody>, Infallible> {
    // Refuse a body the client has already declared too large, before the
    // request is dispatched.
    //
    // Streaming made the cap lazy: `Limited` only trips when something reads
    // the body, so a handler that ignores it — or middleware that
    // short-circuits before an extractor runs — answered `200` for a request
    // the server had declared it would refuse. `Content-Length` is the client's
    // own statement of size, so this costs one header lookup and needs no
    // buffering. A chunked body still declares nothing and remains bounded by
    // the stream limit at the point it is read.
    if let Some(declared) = req
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        if declared > max_body as u64 {
            let res = HyperResponse::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                // Nothing will read the body, so the connection cannot be
                // reused: whatever the client is still sending would be read as
                // the next request.
                .header(http::header::CONNECTION, "close")
                // Every response this server composes says what its body is,
                // and this one did not: it went out as seventeen unlabelled
                // bytes, leaving the client's own sniffing to decide. Naming
                // the type is what the accompanying `nosniff` is there to
                // enforce, and the pair only means something together.
                .header(http::header::CONTENT_TYPE, TEXT_PLAIN)
                .body(EngineBody::from_static(b"Payload Too Large"))
                .expect("response build is infallible");
            return Ok(res);
        }
    }

    // Only the WebSocket branch below consumes this. Discarded explicitly
    // rather than silencing the whole function, which would also hide a
    // genuinely unused variable.
    #[cfg(not(feature = "ws"))]
    let _ = &conn_guard;

    // RFC 9112 §6.3 rule 3: a request carrying both `Transfer-Encoding` and
    // `Content-Length` is framed by the transfer coding, and "ought to be
    // handled as an error" — no legitimate client sends both.
    //
    // The danger is not what Churust does with the message; it is what an
    // intermediary in front of Churust did with it. A proxy that believes the
    // `Content-Length` forwards a different number of body bytes than this
    // server consumes, and the leftovers become the start of the *next* request
    // on a reused connection — request smuggling. Since we cannot know what is
    // upstream, refuse the ambiguity and close, which removes the desync
    // surface whatever the proxy decided.
    //
    // This only fires below hyper 1.11. From 1.11 hyper removes the
    // `Content-Length` from the parsed header map itself, so nothing here can
    // observe the ambiguity — and there is no supported way to ask for the
    // headers as they arrived (`HeaderCaseMap` is `pub(crate)`). Recovering the
    // `400` would mean parsing HTTP/1 message framing off the socket ahead of
    // hyper, which is a second framing implementation guarding the seam between
    // two framing implementations. Not worth it, because 1.11 also sets
    // `keep_alive = false` for this exact shape: the message is served, framed
    // by the transfer coding, and the connection closes — which is the desync
    // removal this branch existed to buy. `hyper = "1"` still resolves to
    // pre-1.11 versions, where it is the only thing standing there, so it stays.
    if req.headers().contains_key(http::header::TRANSFER_ENCODING)
        && req.headers().contains_key(http::header::CONTENT_LENGTH)
    {
        tracing::warn!(
            path = %req.uri().path(),
            "rejected a request with both Transfer-Encoding and Content-Length"
        );
        let res = HyperResponse::builder()
            .status(StatusCode::BAD_REQUEST)
            // Closing is the point: leaving the connection open would let
            // whatever the peer framed differently be read as a new request.
            .header(http::header::CONNECTION, "close")
            // As above: a body with no declared type is one the recipient has
            // to guess at.
            .header(http::header::CONTENT_TYPE, TEXT_PLAIN)
            .body(EngineBody::from_static(b"Bad Request"))
            .expect("response build is infallible");
        return Ok(res);
    }

    // RFC 9112 §3.2: an HTTP/1.1 request must carry exactly one `Host`, and a
    // server must answer `400` to one that carries none or several.
    //
    // The reason is the same as the framing check above, and so is the risk. A
    // request with two `Host` fields is one where this server and an
    // intermediary can disagree about which site it was for — the intermediary
    // routes or authorizes on one, the origin serves the other — and a request
    // with none leaves `Call::host` and every `guard::host` route deciding on
    // nothing at all.
    //
    // Gated on the version, because the other transports do not carry it there:
    // HTTP/2 and HTTP/3 replaced the field with `:authority`, which hyper and h3
    // put in the URI, and an ungated check would refuse every request on them.
    // HTTP/1.0 predates the requirement. An absolute-form target is accepted
    // without the field for the same reason `Call::host` prefers the URI: the
    // authority is already there and it is the one that wins.
    if req.version() == http::Version::HTTP_11 {
        let hosts = req.headers().get_all(http::header::HOST).iter().count();
        let bad = hosts > 1 || (hosts == 0 && req.uri().authority().is_none());
        if bad {
            tracing::warn!(
                path = %req.uri().path(),
                hosts,
                "rejected an HTTP/1.1 request without exactly one Host"
            );
            let res = HyperResponse::builder()
                .status(StatusCode::BAD_REQUEST)
                // Closing for the same reason as the framing refusal: the
                // disagreement is about what the message meant, so nothing after
                // it on this connection can be trusted to mean what it says.
                .header(http::header::CONNECTION, "close")
                .header(http::header::CONTENT_TYPE, TEXT_PLAIN)
                .body(EngineBody::from_static(b"Bad Request"))
                .expect("response build is infallible");
            return Ok(res);
        }
    }

    #[cfg(feature = "ws")]
    let ws_max_frame_bytes = app.config().ws_max_frame_bytes;
    #[cfg(feature = "ws")]
    let ws_max_message_bytes = app.config().ws_max_message_bytes;
    #[cfg(feature = "ws")]
    let ws_idle_timeout_ms = app.config().ws_idle_timeout_ms;
    #[cfg(feature = "ws")]
    let mut req = req;
    #[cfg(feature = "ws")]
    let on_upgrade = if crate::ws::is_upgrade_request(req.headers()) {
        Some(hyper::upgrade::on(&mut req))
    } else {
        None
    };

    let (parts, body) = req.into_parts();

    // A request that has already ended carries nothing to stream, and wrapping
    // "nothing" in a `Limited`, a `BodyDataStream`, a `map` and a `Box::pin`
    // costs a heap allocation to arrive at an empty buffer. `is_end_stream` is
    // hyper's own answer to "is there a body here" — true for a `GET` with no
    // `Content-Length` and for an explicit `Content-Length: 0` — and those are
    // the overwhelming majority of requests a server answers. A body that
    // exists, or that is chunked and so cannot say yet, takes the stream path
    // below unchanged.
    let empty_body = http_body::Body::is_end_stream(&body);

    // The body is handed to the handler as a stream rather than collected here.
    // Collecting first made `max_body_bytes` a hard ceiling on upload size and
    // made memory scale with concurrent uploads. `Limited` still enforces the
    // cap; exceeding it now surfaces as an error item in the stream, which
    // `Call::try_receive_bytes` turns back into `413` for handlers that buffer.
    let body_stream: Option<crate::call::BodyStream> = if empty_body {
        None
    } else {
        let limited = Limited::new(body, max_body);
        Some(Box::pin(BodyDataStream::new(limited).map(|r| {
            r.map_err(|e| {
                // Match the error's *type*, not its message. The message belongs
                // to http-body-util and can change in a patch release, which would
                // silently turn every oversized body into a `400`.
                if e.downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some()
                {
                    crate::Error::new(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
                } else {
                    crate::Error::bad_request(format!("error reading request body: {e}"))
                }
            })
        })))
    };

    // A buffered empty body where there was nothing to stream. Identical to
    // what draining the stream would have produced, without the allocation.
    let build_call = move || {
        let mut call =
            crate::call::Call::new(parts.method, parts.uri, parts.headers, bytes::Bytes::new());
        call.set_peer(peer);
        match body_stream {
            Some(stream) => call.with_body_stream(stream),
            None => call,
        }
    };

    #[cfg(feature = "ws")]
    let process = {
        let mut extensions = http::Extensions::new();
        if let Some(on_upgrade) = on_upgrade {
            extensions.insert(crate::ws::OnUpgradeHandle::new(on_upgrade));
            // So the upgraded socket keeps this connection's budget share.
            extensions.insert(conn_guard.clone());
            extensions.insert(crate::ws::WsIdleTimeout(ws_idle_timeout_ms));
            extensions.insert(crate::ws::WsLimits {
                max_frame_bytes: ws_max_frame_bytes,
                max_message_bytes: ws_max_message_bytes,
            });
        }
        app.process_call(build_call(), extensions)
    };
    #[cfg(not(feature = "ws"))]
    let process = {
        // Empty on an ordinary request, and an empty `Extensions` allocates
        // nothing. The peer address rides on the `Call` itself; see
        // `Call::set_peer`.
        let extensions = http::Extensions::new();
        app.process_call(build_call(), extensions)
    };

    let res = if timeout_ms == 0 {
        process.await
    } else {
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), process).await {
            Ok(res) => res,
            Err(_) => crate::response::Response::text("Request Timeout")
                .with_status(StatusCode::REQUEST_TIMEOUT),
        }
    };

    let mut builder = HyperResponse::builder().status(res.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = res.headers;
    }
    Ok(builder
        .body(EngineBody::new(res.body))
        .expect("response build is infallible"))
}
