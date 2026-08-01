//! Application assembly: the [`Churust`] entry point, the [`AppBuilder`] DSL,
//! the immutable [`App`], the [`Plugin`] trait, and the resolved
//! [`ServerConfig`].

use crate::call::Call;
use crate::pipeline::{Middleware, Next, Phase};
use crate::response::Response;
use crate::router::{BorrowedMatch, Match, RouteBuilder, Router};
use crate::state::StateMap;
use bytes::Bytes;
use futures_util::FutureExt;
use http::header::ALLOW;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use std::sync::Arc;

/// A reusable bundle of behavior that installs itself into an [`AppBuilder`]
/// at build time — Churust's analogue of Ktor's `install(Plugin)`.
///
/// A plugin typically registers one or more [`Middleware`] (and may add state)
/// inside [`install`](Plugin::install). Pass a plugin to
/// [`AppBuilder::install`]; the builder boxes it and calls `install`.
///
/// ```
/// use churust_core::{App, AppBuilder, Churust, Call, Middleware, Next, Plugin, Response, TestClient};
/// use async_trait::async_trait;
/// use std::sync::Arc;
/// use http::{header::HeaderName, HeaderValue};
///
/// struct Mark;
/// #[async_trait]
/// impl Middleware for Mark {
///     async fn handle(&self, call: Call, next: Next) -> Response {
///         let mut res = next.run(call).await;
///         res.headers.insert(HeaderName::from_static("x-plugin"), HeaderValue::from_static("on"));
///         res
///     }
/// }
///
/// struct MarkPlugin;
/// impl Plugin for MarkPlugin {
///     fn install(self: Box<Self>, app: &mut AppBuilder) {
///         app.add_middleware(Arc::new(Mark));
///     }
/// }
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(MarkPlugin)
///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
///     .build();
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.header("x-plugin"), Some("on"));
/// # });
/// ```
pub trait Plugin {
    /// Install this plugin into the builder (register middleware, state, etc.).
    /// Consumes the boxed plugin.
    fn install(self: Box<Self>, app: &mut AppBuilder);
}

/// The server configuration resolved at build time and carried by an [`App`].
///
/// Populated from defaults, an optional [`Config`](crate::Config), and the
/// builder's DSL setters. Read it back from a built app with
/// [`App::config`].
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Bind address host.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// Maximum accepted request body size in bytes; larger bodies get `413`.
    pub max_body_bytes: usize,
    /// Per-request timeout in milliseconds; `0` disables the timeout.
    pub request_timeout_ms: u64,
    /// Deadline for a client to finish sending its header block; `0` disables.
    pub header_read_timeout_ms: u64,
    /// Maximum number of request headers accepted.
    pub max_headers: usize,
    /// Maximum path segments before a request is rejected with `414`.
    pub max_path_segments: usize,
    /// Maximum WebSocket frame size in bytes (`ws` feature).
    pub ws_max_frame_bytes: usize,
    /// Maximum reassembled WebSocket message size in bytes (`ws` feature).
    pub ws_max_message_bytes: usize,
    /// Idle keep-alive in milliseconds; `0` disables connection reuse.
    /// Idle means no request in flight, so a slow handler is never cut off.
    pub keep_alive_ms: u64,
    /// Listen backlog.
    pub backlog: u32,
    /// Graceful-shutdown grace period in milliseconds; `0` waits forever.
    pub shutdown_timeout_ms: u64,
    /// What to do with a non-canonical path spelling.
    pub path_policy: crate::path::PathPolicy,
    /// Maximum size of a received HTTP/2 header block, in bytes. The h2
    /// counterpart of `max_headers`, which configures HTTP/1 only.
    pub h2_max_header_list_size: u32,
    /// Maximum concurrent HTTP/2 streams per connection; `0` removes the limit.
    pub h2_max_concurrent_streams: u32,
    /// WebSocket idle bound in milliseconds; `0` disables it (`ws` feature).
    pub ws_idle_timeout_ms: u64,
    /// Maximum simultaneously served connections; `0` means unlimited.
    pub max_connections: usize,
    /// Maximum TLS handshakes in progress at once; `0` means unlimited.
    pub max_tls_handshakes: usize,
    /// TLS handshake deadline in milliseconds; `0` disables the bound.
    pub tls_handshake_timeout_ms: u64,
    /// TLS settings, or `None` for plaintext HTTP.
    pub tls: Option<crate::config::TlsSection>,
    /// Disable Nagle's algorithm on accepted TCP connections.
    ///
    /// On by default, and it should stay on for anything answering real
    /// clients: with Nagle enabled a small write waits for more data to send
    /// while the peer's delayed ACK waits for a response, and the standoff is
    /// broken by a timer rather than by either side — tens of milliseconds
    /// added to a response the server produced in microseconds.
    ///
    /// The one workload it costs is HTTP/1.1 *pipelining*, where coalescing the
    /// responses to a batch of requests into one segment is exactly what you
    /// want. Turn it off there, and see `pipeline_flush`, which achieves the
    /// same coalescing without giving up the latency guarantee.
    pub tcp_nodelay: bool,
    /// Aggregate the writes for pipelined HTTP/1.1 responses into one flush.
    ///
    /// A client that pipelines sends several requests without waiting for the
    /// replies. Answered one flush at a time, each reply is its own write
    /// syscall and — with `tcp_nodelay` on — its own packet. Answered as one
    /// flush, a batch of 64 replies costs one of each.
    ///
    /// Off by default because it is the wrong trade for the ordinary case: a
    /// non-pipelining client's response then waits for a flush that only
    /// happens once the connection has nothing left to read. Measured on
    /// loopback with one request in flight at a time, that is a median 90µs
    /// instead of 56µs — 61% more latency on every response, to help a client
    /// shape that is rare on the open internet.
    pub pipeline_flush: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            max_body_bytes: 1 << 20,
            request_timeout_ms: 30_000,
            header_read_timeout_ms: 10_000,
            max_headers: 100,
            max_path_segments: 64,
            ws_max_frame_bytes: 1 << 20,
            ws_max_message_bytes: 4 << 20,
            keep_alive_ms: 75_000,
            backlog: 1024,
            shutdown_timeout_ms: 30_000,
            path_policy: crate::path::PathPolicy::Strict,
            h2_max_header_list_size: 16 << 10,
            h2_max_concurrent_streams: 200,
            ws_idle_timeout_ms: 300_000,
            max_connections: 25_000,
            max_tls_handshakes: 256,
            tls_handshake_timeout_ms: 10_000,
            tls: None,
            tcp_nodelay: true,
            pipeline_flush: false,
        }
    }
}

/// The fluent builder for an application, returned by [`Churust::server`].
///
/// Chain DSL methods to configure the server ([`host`](AppBuilder::host),
/// [`port`](AppBuilder::port), [`tls`](AppBuilder::tls), ...), register shared
/// [`state`](AppBuilder::state), [`install`](AppBuilder::install) plugins, and
/// define routes with [`routing`](AppBuilder::routing). Finish with
/// [`build`](AppBuilder::build) to get an [`App`], or
/// [`start`](AppBuilder::start) to build and serve in one step. DSL setters take
/// precedence over a [`with_config`](AppBuilder::with_config) applied earlier.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .port(3000)
///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
///     .build();
/// assert_eq!(app.config().port, 3000);
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.text(), "hi");
/// # });
/// ```
pub struct AppBuilder {
    router: Router,
    middleware: Vec<(Phase, Arc<dyn Middleware>)>,
    config: ServerConfig,
    state: StateMap,
    /// `None` means the application opted out. Defaults to the conservative
    /// set, so an app that never mentions security headers still sends them.
    security: Option<crate::security::SecurityHeaders>,
    /// Extra `host:port` strings from [`AppBuilder::bind`].
    extra_binds: Vec<String>,
    /// Optional renderer for error statuses. See [`AppBuilder::on_error`].
    on_error: Option<ErrorRenderer>,
}

/// Renders an error status into a response. Returning `None` keeps the default.
type ErrorRenderer = Arc<dyn Fn(StatusCode, &Call) -> Option<Response> + Send + Sync>;

impl AppBuilder {
    fn new() -> Self {
        Self {
            router: Router::new(),
            middleware: Vec::new(),
            config: ServerConfig::default(),
            state: StateMap::default(),
            security: Some(crate::security::SecurityHeaders::default()),
            on_error: None,
            extra_binds: Vec::new(),
        }
    }

    /// Set the bind host (default `"127.0.0.1"`). Returns `self` for chaining.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.config.host = host.into();
        self
    }
    /// Set the bind port (default `8080`). Returns `self` for chaining.
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }
    /// Set the maximum accepted request body size in bytes (default `1 MiB`).
    /// Larger bodies are rejected with `413 Payload Too Large`. Returns `self`
    /// for chaining.
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.config.max_body_bytes = n;
        self
    }

    /// Apply a fully-resolved [`Config`](crate::Config), overwriting the current
    /// server settings.
    ///
    /// This is lowest precedence: DSL setters called *after* it (e.g. another
    /// [`port`](AppBuilder::port)) win. Use it to seed the builder from a config
    /// file/env, then override specific fields in code. Returns `self` for
    /// chaining.
    pub fn with_config(mut self, cfg: crate::config::Config) -> Self {
        self.config.host = cfg.server.host;
        self.config.port = cfg.server.port;
        self.config.max_body_bytes = cfg.server.max_body_bytes;
        self.config.request_timeout_ms = cfg.server.request_timeout_ms;
        self.config.header_read_timeout_ms = cfg.server.header_read_timeout_ms;
        self.config.max_headers = cfg.server.max_headers;
        self.config.max_path_segments = cfg.server.max_path_segments;
        self.config.ws_max_frame_bytes = cfg.server.ws_max_frame_bytes;
        self.config.ws_max_message_bytes = cfg.server.ws_max_message_bytes;
        self.config.keep_alive_ms = cfg.server.keep_alive_ms;
        self.config.backlog = cfg.server.backlog;
        self.config.shutdown_timeout_ms = cfg.server.shutdown_timeout_ms;
        self.config.path_policy = cfg.server.path_policy;
        self.config.h2_max_header_list_size = cfg.server.h2_max_header_list_size;
        self.config.h2_max_concurrent_streams = cfg.server.h2_max_concurrent_streams;
        self.config.ws_idle_timeout_ms = cfg.server.ws_idle_timeout_ms;
        self.config.max_connections = cfg.server.max_connections;
        self.config.max_tls_handshakes = cfg.server.max_tls_handshakes;
        self.config.tls_handshake_timeout_ms = cfg.server.tls_handshake_timeout_ms;
        self.config.tls = cfg.tls;
        self
    }

    /// Set the per-request timeout in milliseconds (default `30000`). A value of
    /// `0` disables the timeout. Requests exceeding it get `408 Request Timeout`.
    /// Returns `self` for chaining.
    pub fn request_timeout_ms(mut self, ms: u64) -> Self {
        self.config.request_timeout_ms = ms;
        self
    }

    /// Set how long a client may take to send its complete header block
    /// (default `10000` ms; `0` disables).
    ///
    /// This is the slow-loris defence. The per-request timeout cannot cover it,
    /// because there is no request until the headers arrive.
    pub fn header_read_timeout_ms(mut self, ms: u64) -> Self {
        self.config.header_read_timeout_ms = ms;
        self
    }

    /// Set the maximum number of request headers accepted (default `100`).
    pub fn max_headers(mut self, n: usize) -> Self {
        self.config.max_headers = n;
        self
    }

    /// Set how long an idle connection is kept for reuse (default `75000` ms;
    /// `0` disables keep-alive and closes after each response).
    ///
    /// A connection with a request in flight is not idle, however long the
    /// handler takes. Lower this when connection count matters more than
    /// round-trip latency; raise it for chatty clients on slow links.
    ///
    /// Over HTTP/3 this becomes the QUIC idle timeout, so a lowered bound
    /// applies there too. The one exception is `0`: a QUIC connection
    /// multiplexes streams and cannot be closed after a single response, so
    /// there is nothing for "answer and close" to mean and the HTTP/3 listener
    /// keeps the default `75000` ms bound rather than never expiring.
    ///
    /// `0` reaches HTTP/2 as well, where hyper has no reuse switch to turn off:
    /// the connection is closed once a response has been written and nothing
    /// else is in flight. A connection that has not answered anything yet is
    /// left to `header_read_timeout_ms`, which is what bounds a peer that has
    /// not made a request.
    pub fn keep_alive_ms(mut self, ms: u64) -> Self {
        self.config.keep_alive_ms = ms;
        self
    }

    /// Set the listen backlog (default `1024`).
    pub fn backlog(mut self, n: u32) -> Self {
        self.config.backlog = n;
        self
    }

    /// Disable Nagle's algorithm on accepted connections (default `true`).
    ///
    /// Leave it on unless you know the workload pipelines. See
    /// [`ServerConfig::tcp_nodelay`] for what turning it off costs.
    ///
    /// ```
    /// use churust_core::{Call, Churust};
    /// let app = Churust::server()
    ///     .tcp_nodelay(false) // only for a client that pipelines
    ///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
    ///     .build();
    /// assert!(!app.config().tcp_nodelay);
    /// ```
    pub fn tcp_nodelay(mut self, on: bool) -> Self {
        self.config.tcp_nodelay = on;
        self
    }

    /// Answer a batch of pipelined HTTP/1.1 requests with one flush instead of
    /// one per response (default `false`).
    ///
    /// Turn this on only for a workload that actually pipelines. See
    /// [`ServerConfig::pipeline_flush`] for why it is not the default.
    ///
    /// ```
    /// use churust_core::{Call, Churust};
    /// let app = Churust::server()
    ///     .pipeline_flush(true)
    ///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
    ///     .build();
    /// assert!(app.config().pipeline_flush);
    /// ```
    pub fn pipeline_flush(mut self, on: bool) -> Self {
        self.config.pipeline_flush = on;
        self
    }

    /// Set how long graceful shutdown waits for in-flight requests (default
    /// `30000` ms; `0` waits indefinitely).
    ///
    /// Unbounded waiting means one slow request can delay shutdown forever,
    /// which in a container means being killed rather than exiting cleanly.
    pub fn shutdown_timeout_ms(mut self, ms: u64) -> Self {
        self.config.shutdown_timeout_ms = ms;
        self
    }

    /// Set what happens to a non-canonical path spelling (default
    /// [`PathPolicy::Strict`](crate::PathPolicy::Strict)).
    ///
    /// `//a`, `/a//b` and `/a/` are aliases of `/a`. Serving them silently
    /// gives one resource several URLs, which makes prefix-based checks
    /// bypassable and cache identity ambiguous.
    pub fn path_policy(mut self, policy: crate::path::PathPolicy) -> Self {
        self.config.path_policy = policy;
        self
    }

    /// Set the maximum size of a received HTTP/2 header block in bytes
    /// (default `16384`).
    ///
    /// `max_headers` configures HTTP/1 only — it counts headers, and HTTP/2 has
    /// no equivalent count, only an encoded size. Set both if you serve both.
    pub fn h2_max_header_list_size(mut self, n: u32) -> Self {
        self.config.h2_max_header_list_size = n;
        self
    }

    /// Set the maximum concurrent HTTP/2 streams per connection (default
    /// `200`; `0` removes the limit).
    ///
    /// An h2 connection multiplexes many requests, so this is what stops one
    /// connection from becoming an unbounded amount of concurrent work.
    pub fn h2_max_concurrent_streams(mut self, n: u32) -> Self {
        self.config.h2_max_concurrent_streams = n;
        self
    }

    /// Set how long an upgraded WebSocket may sit idle before it is closed
    /// (default `300000` ms; `0` disables the bound).
    ///
    /// An upgraded socket holds a connection permit for its whole life, and no
    /// HTTP-level timeout survives the upgrade — so without this a peer that
    /// completes the handshake and then goes silent holds a permit until the
    /// process restarts. Idle means no frame in either direction.
    pub fn ws_idle_timeout_ms(mut self, ms: u64) -> Self {
        self.config.ws_idle_timeout_ms = ms;
        self
    }

    /// Set the maximum number of simultaneously served connections (default
    /// `25000`; `0` means unlimited).
    ///
    /// The backlog bounds what the kernel queues before the accept loop reaches
    /// it; this bounds what the process serves at once. Excess connections wait
    /// for a slot rather than being accepted, so the pressure shows up as
    /// latency instead of as an out-of-memory or out-of-descriptors death.
    pub fn max_connections(mut self, n: usize) -> Self {
        self.config.max_connections = n;
        self
    }

    /// Set the maximum number of TLS handshakes in progress at once (default
    /// `256`; `0` means unlimited).
    ///
    /// Deliberately far below `max_connections`: a handshake is asymmetric
    /// work, cheap for the client to request and expensive for the server to
    /// perform, so it gets its own tighter bound.
    ///
    /// Applies to HTTP/3 as well, whose QUIC handshake is a TLS 1.3 handshake
    /// and asymmetric in the same way.
    pub fn max_tls_handshakes(mut self, n: usize) -> Self {
        self.config.max_tls_handshakes = n;
        self
    }

    /// Set how long a TLS handshake may take before the connection is dropped
    /// (default `10000` ms; `0` disables the bound).
    ///
    /// `header_read_timeout_ms` cannot cover this: until the handshake
    /// finishes there is no HTTP layer to time out. Without it, a client that
    /// completes the TCP handshake and then dribbles bytes holds a connection
    /// open indefinitely.
    ///
    /// Applies to HTTP/3 too, and bounds the wait for the
    /// [`max_tls_handshakes`](Self::max_tls_handshakes) budget as well as the
    /// handshake itself — a peer queued for that budget is already holding a
    /// connection permit, so timing only the handshake would leave the wait
    /// unbounded.
    pub fn tls_handshake_timeout_ms(mut self, ms: u64) -> Self {
        self.config.tls_handshake_timeout_ms = ms;
        self
    }

    /// Set the maximum number of path segments accepted (default `64`).
    /// Longer paths are rejected with `414 URI Too Long`.
    pub fn max_path_segments(mut self, n: usize) -> Self {
        self.config.max_path_segments = n;
        self
    }

    /// Enable TLS, reading the certificate chain from `cert_path` and the
    /// private key from `key_path` (PEM). The files are loaded when the server
    /// starts; this only records the paths. Requires the `tls` feature to have
    /// any effect at serve time. Returns `self` for chaining.
    pub fn tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.config.tls = Some(crate::config::TlsSection {
            cert: cert_path.into(),
            key: key_path.into(),
        });
        self
    }

    /// Register a shared application-state value of type `T`, retrieved later
    /// via the [`State`](crate::State) extractor or
    /// [`Call::state`](crate::Call::state). One value is held per type;
    /// registering another `T` replaces it. Returns `self` for chaining.
    ///
    /// ```
    /// use churust_core::{Churust, State, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #[derive(Clone)]
    /// struct Db { name: &'static str }
    /// let app = Churust::server()
    ///     .state(Db { name: "postgres" })
    ///     .routing(|r| { r.get("/", |db: State<Db>| async move { db.name }); })
    ///     .build();
    /// assert_eq!(TestClient::new(app).get("/").send().await.text(), "postgres");
    /// # });
    /// ```
    pub fn state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.insert_state(value);
        self
    }

    /// Advertise HTTP/3 on `port` to clients arriving over TCP.
    ///
    /// Adds `Alt-Svc: h3=":<port>"; ma=86400` to every response, which is how a
    /// browser learns that h3 exists at all: it reaches a new origin over TCP,
    /// reads this header, and uses QUIC for subsequent requests. Serving h3
    /// without advertising it means almost nothing will ever use it.
    ///
    /// Available with or without the `http3` feature, because the process that
    /// terminates h3 is often a proxy in front rather than this server. Setting
    /// it when nothing is listening on that UDP port costs a client one failed
    /// QUIC attempt before it falls back, so only set it when something is.
    ///
    /// ```
    /// use churust_core::{Call, Churust, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .advertise_http3(8443)
    ///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
    ///     .build();
    /// let res = TestClient::new(app).get("/").send().await;
    /// assert_eq!(res.header("alt-svc"), Some("h3=\":8443\"; ma=86400"));
    /// # });
    /// ```
    pub fn advertise_http3(mut self, port: u16) -> Self {
        self.add_middleware_in(Phase::Setup, Arc::new(AltSvc(port)));
        self
    }

    /// Register application state through a mutable borrow.
    ///
    /// The counterpart to [`state`](Self::state) for callers that hold
    /// `&mut AppBuilder` rather than owning it, which is every [`Plugin`]: a
    /// plugin that wants to publish something for its own extractor to find has
    /// no other way to reach the state map. Same pairing as
    /// [`add_middleware`](Self::add_middleware) and
    /// [`install_middleware`](Self::install_middleware).
    pub fn insert_state<T: Send + Sync + 'static>(&mut self, value: T) {
        self.state.insert(value);
    }

    /// Install a [`Plugin`], letting it register middleware/state. Consumes the
    /// plugin value. Returns `self` for chaining.
    pub fn install<P: Plugin + 'static>(mut self, plugin: P) -> Self {
        Box::new(plugin).install(&mut self);
        self
    }

    /// Register a [`Middleware`] in a specific [`Phase`]. Plugins use this to
    /// place their middleware precisely; the builder sorts all middleware by
    /// phase (stably) at [`build`](AppBuilder::build) time.
    pub fn add_middleware_in(&mut self, phase: Phase, mw: Arc<dyn Middleware>) {
        self.middleware.push((phase, mw));
    }

    /// Register a [`Middleware`] in the default [`Phase::Plugins`] phase — the
    /// common case for application middleware.
    pub fn add_middleware(&mut self, mw: Arc<dyn Middleware>) {
        self.add_middleware_in(Phase::Plugins, mw);
    }

    /// Define routes via the [`RouteBuilder`] DSL. The closure receives a
    /// mutable builder on which to register handlers and nested scopes. Returns
    /// `self` for chaining.
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "home" });
    ///         r.post("/echo", |mut c: Call| async move { c.receive_text().await.unwrap_or_default() });
    ///     })
    ///     .build();
    /// assert_eq!(TestClient::new(app).get("/").send().await.text(), "home");
    /// # });
    /// ```
    pub fn routing(mut self, f: impl FnOnce(&mut RouteBuilder)) -> Self {
        let mut b = RouteBuilder::new(&mut self.router);
        f(&mut b);
        self
    }

    /// Every route registered so far, as `(method, pattern)`.
    ///
    /// Called between two [`routing`](Self::routing) blocks, this is the
    /// inventory an API description is generated from: describe what is already
    /// registered, then register the route that serves the description.
    ///
    /// ```
    /// use churust_core::{Call, Churust};
    /// use http::Method;
    ///
    /// let builder = Churust::server().routing(|r| {
    ///     r.get("/users/{id}", |_c: Call| async { "" });
    /// });
    /// assert_eq!(builder.routes(), &[(Method::GET, "/users/{id}".to_string())]);
    /// ```
    pub fn routes(&self) -> &[(Method, String)] {
        self.router.routes()
    }

    /// Build the app and serve it until Ctrl-C — a shorthand for
    /// `self.build().start().await`. Binds a socket, so it does not return until
    /// shutdown.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn run() -> std::io::Result<()> {
    /// Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .start()
    ///     .await
    /// # }
    /// ```
    pub async fn start(self) -> std::io::Result<()> {
        // Genuinely nothing but `build().start()`. The extra-address handling
        // that used to live here is now in `App`, where every entry point can
        // reach it; keeping a second copy here is how the two got to disagree.
        self.build().start().await
    }

    /// Render error responses yourself — v1 design §5.3's "StatusPages-lite".
    ///
    /// The closure runs for any response the pipeline produces with a `4xx` or
    /// `5xx` status, whether it came from a handler returning `Err`, a `404`
    /// for an unmatched path, or a `405`. Return `Some(response)` to replace
    /// it, or `None` to keep the default rendering — so a hook can take over
    /// just the statuses it cares about.
    ///
    /// It does **not** run for a request the server refused to admit: an
    /// oversized `Content-Length`, a message framed both by `Transfer-Encoding`
    /// and `Content-Length`, or a deadline that expired before the pipeline
    /// returned anything. Those are answered by the transport as `413`, `400`
    /// and `408` in plain text, and deliberately so — running the pipeline
    /// would mean inventing a `Call` for a request that was never accepted, and
    /// every middleware with a side effect (a rate-limit counter, an audit
    /// entry, a session touch) would then record a request the server declined
    /// to dispatch. Security headers *are* applied to them, because those are
    /// added by the transport on the way out rather than by the pipeline.
    ///
    /// It receives the status rather than the `Error` because by the time a
    /// response exists the error has already been rendered; this is the same
    /// shape as Ktor's `StatusPages`, which Churust's pipeline is modelled on.
    ///
    /// ```
    /// use churust_core::{Call, Churust, Response, TestClient};
    /// use http::StatusCode;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .on_error(|status, call| {
    ///         (status == StatusCode::NOT_FOUND)
    ///             .then(|| Response::text(format!("no {} here", call.path())).with_status(status))
    ///     })
    ///     .routing(|r| { r.get("/", |_c: Call| async { "home" }); })
    ///     .build();
    ///
    /// let res = TestClient::new(app).get("/missing").send().await;
    /// assert_eq!(res.text(), "no /missing here");
    /// # });
    /// ```
    pub fn on_error<F>(mut self, f: F) -> Self
    where
        F: Fn(StatusCode, &Call) -> Option<Response> + Send + Sync + 'static,
    {
        self.on_error = Some(Arc::new(f));
        self
    }

    /// Install a single [`Middleware`] app-wide, in the `Plugins` phase.
    ///
    /// The chainable counterpart to [`add_middleware`](AppBuilder::add_middleware),
    /// which exists for plugins holding a `&mut AppBuilder`. For middleware that
    /// should apply to only part of the route tree, use
    /// [`RouteBuilder::intercept`](crate::RouteBuilder::intercept) instead.
    pub fn install_middleware<M: Middleware>(mut self, mw: M) -> Self {
        self.middleware.push((Phase::Plugins, Arc::new(mw)));
        self
    }

    /// Replace the default [`SecurityHeaders`](crate::SecurityHeaders) set.
    ///
    /// ```
    /// use churust_core::{Churust, SecurityHeaders};
    /// # fn build() {
    /// Churust::server()
    ///     .security_headers(SecurityHeaders::new().frame_options(Some("SAMEORIGIN")));
    /// # }
    /// ```
    pub fn security_headers(mut self, headers: crate::security::SecurityHeaders) -> Self {
        self.security = Some(headers);
        self
    }

    /// Send no security headers at all.
    ///
    /// Reach for this only when something in front of the server already adds
    /// them; the defaults exist because most applications never get around to
    /// setting them by hand.
    pub fn without_security_headers(mut self) -> Self {
        self.security = None;
        self
    }

    /// Bind an additional address.
    ///
    /// Call it more than once to listen on several — IPv4 and IPv6, or a public
    /// port alongside an admin one. The configured host and port are always
    /// bound; this adds to them.
    ///
    /// The addresses survive [`build`](Self::build), so they are honoured by
    /// [`App::start`] and [`App::start_with_shutdown`] just as much as by
    /// [`start`](Self::start). The two exceptions are the entry points that are
    /// handed a socket instead of choosing one — [`App::start_on`] takes an
    /// already-bound listener and [`start_unix`](Self::start_unix) serves a
    /// filesystem path — and neither binds the configured `host:port` either,
    /// so there is nothing for this to add to.
    ///
    /// ```no_run
    /// use churust_core::{Call, Churust};
    /// # async fn run() -> std::io::Result<()> {
    /// Churust::server()
    ///     .host("127.0.0.1")
    ///     .port(8080)
    ///     .bind("[::1]:8080")
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .start()
    ///     .await
    /// # }
    /// ```
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.extra_binds.push(addr.into());
        self
    }

    /// Serve on a Unix domain socket until Ctrl-C.
    ///
    /// See [`engine::serve_unix`](crate::engine::serve_unix). Unix only.
    #[cfg(unix)]
    pub async fn start_unix(self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        self.build().start_unix(path).await
    }

    /// Finish building into an immutable, cheaply-cloneable [`App`].
    ///
    /// Installed middleware is sorted by [`Phase`] (stably, preserving install
    /// order within a phase) and the configuration is frozen.
    pub fn build(self) -> App {
        let mut mw = self.middleware;

        // Setup phase, so it wraps everything and post-processes every response
        // — including 404s and errors, which are just as reachable as handler
        // output. Pushed before the sort, which is stable, so an application's
        // own Setup middleware installed earlier still runs outside this.
        if let Some(render) = self.on_error {
            // Monitoring rather than Setup: security headers live in Setup and
            // must wrap this, so a replaced error page is protected too.
            mw.push((
                Phase::Monitoring,
                Arc::new(ErrorPages { render }) as Arc<dyn Middleware>,
            ));
        }

        // Kept as well as installed. The middleware covers everything the
        // pipeline produces, but a transport also answers on its own — a body
        // refused on its declared length, a deadline that expired before the
        // pipeline returned anything — and those responses have to be given the
        // same set by the transport itself. It cannot ask the middleware,
        // because by then it is an opaque `Arc<dyn Middleware>` in a list.
        let security = self.security.clone();
        if let Some(headers) = self.security {
            let tls_enabled = self.config.tls.is_some();
            mw.push((
                Phase::Setup,
                Arc::new(headers.into_middleware(tls_enabled)) as Arc<dyn Middleware>,
            ));
        }

        mw.sort_by_key(|(phase, _)| *phase); // stable: install order preserved within a phase
        let middleware: Arc<[Arc<dyn Middleware>]> = mw.into_iter().map(|(_, m)| m).collect();
        App {
            inner: Arc::new(AppInner {
                router: self.router,
                middleware,
                config: self.config,
                state: Arc::new(self.state),
                extra_binds: self.extra_binds,
                security,
            }),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppInner {
    router: Router,
    /// Built once, shared by every request. See [`Next`] for why this is a
    /// slice behind an `Arc` rather than something the pipeline rebuilds.
    middleware: Arc<[Arc<dyn Middleware>]>,
    config: ServerConfig,
    state: Arc<StateMap>,
    /// Carried through from [`AppBuilder::bind`]. `build` used to drop these,
    /// which made `bind` a no-op for everybody who built an [`App`] and then
    /// served it — the ordinary shape once a custom shutdown signal is in play.
    /// The extra addresses are serve-time state rather than request-time state,
    /// but so are `host` and `port`, which already ride along in `config`.
    extra_binds: Vec<String>,
    /// The same set the pipeline middleware was built from, for the responses
    /// a transport writes without going through the pipeline. `None` when the
    /// application called `without_security_headers`, so the opt-out reaches
    /// those responses too.
    security: Option<crate::security::SecurityHeaders>,
}

/// An assembled, immutable, cheaply-cloneable application.
///
/// Produced by [`AppBuilder::build`]. Internally reference-counted, so cloning
/// is cheap and clones share the same router, middleware, state, and config.
/// Serve it with [`start`](App::start) (or
/// [`start_with_shutdown`](App::start_with_shutdown)), drive a single request
/// in-process with [`process`](App::process), or hand it to a
/// [`TestClient`](crate::TestClient) for testing.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
///     .build();
/// let clone = app.clone(); // cheap; shares the same inner app
/// let res = TestClient::new(clone).get("/").send().await;
/// assert_eq!(res.status().as_u16(), 200);
/// # });
/// ```
#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

impl App {
    /// The resolved [`ServerConfig`] this app was built with.
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// Decorate a response with the configured
    /// [`SecurityHeaders`](crate::SecurityHeaders).
    ///
    /// For the transports to call on the way out, on every response — not only
    /// on the ones they synthesised. Sorting those two apart would mean a new
    /// refusal added later is bare until someone notices, which is exactly how
    /// the pre-dispatch `413` and the h3 `413` came to be bare. Calling it on a
    /// response the pipeline already decorated costs a handful of header
    /// lookups and changes nothing, since a header already present is kept.
    ///
    /// `over_tls` is the transport asserting that this response is encrypted
    /// regardless of what the builder was told — true for HTTP/3, which has no
    /// plaintext mode. Over TCP the builder's own certificate is the only
    /// evidence available, so the flag is false there and
    /// `config.tls.is_some()` decides.
    /// HTTP/3 only. The TCP transports take a
    /// [`security_snapshot`](App::security_snapshot) once per connection
    /// instead, so that the response path can consume its `App` rather than
    /// keep a second one alive to ask this question afterwards. h3 has no
    /// equivalent per-connection hook to hang it from.
    #[cfg(feature = "http3")]
    pub(crate) fn apply_security_headers(&self, headers: &mut HeaderMap, over_tls: bool) {
        if let Some(security) = &self.inner.security {
            security.apply_to(headers, over_tls || self.inner.config.tls.is_some());
        }
    }

    /// A copy of this application that shares everything expensive and nothing
    /// contended.
    ///
    /// The routes, handlers, middleware and state inside are the same objects —
    /// they are behind their own `Arc`s and are never cloned per request. What
    /// is *not* shared is the `AppInner` allocation itself, and therefore its
    /// reference count. That matters because the request path clones the `App`
    /// once per request, and when every core is cloning the same `Arc` the
    /// cache line holding its count becomes the thing they queue for: measured
    /// on the comparison harness, twelve workers sharing one `AppInner` reached
    /// 1.29M requests a second where twelve processes not sharing it reached
    /// 2.07M — the same code, the same cores, differing only in whether one
    /// integer was contended.
    ///
    /// Used by [`engine::serve_sharded`](crate::engine::serve_sharded), which
    /// hands each worker its own. Not a semantic clone: an application is
    /// immutable once built, so two replicas cannot disagree.
    pub(crate) fn replica(&self) -> Self {
        Self {
            inner: Arc::new((*self.inner).clone()),
        }
    }

    /// The security-header set and whether the transport is already TLS, in a
    /// form a connection can hold for its whole life.
    ///
    /// Taken once per connection rather than read off the `App` once per
    /// response, so the response path can *consume* its `App` on the way into
    /// the pipeline instead of keeping a second one alive to ask this question
    /// afterwards. That second one was an `Arc` clone and drop per request on
    /// the cache line every core serving requests already contends for.
    ///
    /// `None` when the application opted out, which is also the cheapest case:
    /// nothing to copy and nothing to apply.
    pub(crate) fn security_snapshot(&self) -> Option<(crate::security::SecurityHeaders, bool)> {
        self.inner
            .security
            .as_ref()
            .map(|s| (s.clone(), self.inner.config.tls.is_some()))
    }

    /// The single request entry point: run one request through the full
    /// pipeline (middleware then routed handler) and return the [`Response`].
    ///
    /// Both the hyper engine and the [`TestClient`](crate::TestClient) call
    /// this. It is panic-isolated — a panicking handler is caught and turned
    /// into `500 Internal Server Error` rather than crashing the task.
    ///
    /// ```
    /// use churust_core::{Churust, Call};
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
    ///     .build();
    /// let res = app.process(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new()).await;
    /// assert_eq!(res.status.as_u16(), 200);
    /// # });
    /// ```
    /// THE single request entry point used by the engine and `TestClient`.
    /// Panic-isolated: a panicking handler yields 500.
    pub async fn process(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        self.process_with_extensions(method, uri, headers, body, http::Extensions::new())
            .await
    }

    /// Like [`App::process`], but seeds the `Call` with pre-built extensions
    /// (e.g. a captured WebSocket upgrade handle). Advanced/engine use.
    pub async fn process_with_extensions(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
        extensions: http::Extensions,
    ) -> Response {
        self.clone()
            .process_call(Call::new(method, uri, headers, body), extensions)
            .await
    }

    /// Run the pipeline over an already-built [`Call`]. Engine use: this is how
    /// a streaming request body reaches a handler without being buffered first.
    pub(crate) async fn process_call(self, call: Call, extensions: http::Extensions) -> Response {
        let fut = async move {
            let mut call = call;
            call.seed_extensions(extensions);
            // Only when there is something to share. An application that
            // registered no state used to pay an `Arc` clone and drop per
            // request for a map with nothing in it — and that clone is an
            // atomic on a line every core touches, which is the expensive part
            // rather than the pointer copy.
            if !self.inner.state.is_empty() {
                call.set_state(Some(self.inner.state.clone()));
            }
            self.run_pipeline(call).await
        };
        match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
            Ok(res) => res,
            Err(_) => Response::text("Internal Server Error")
                .with_status(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }

    /// Every TCP address this app should listen on: the configured `host:port`
    /// first, then whatever [`AppBuilder::bind`] added.
    ///
    /// One list built in one place, so the address-based entry points cannot
    /// drift apart on which addresses count — that drift is exactly how `bind`
    /// came to be honoured by `AppBuilder::start` and silently ignored by every
    /// other way of serving the same application.
    fn bind_addrs(&self) -> std::io::Result<Vec<std::net::SocketAddr>> {
        std::iter::once(format!(
            "{}:{}",
            self.inner.config.host, self.inner.config.port
        ))
        .chain(self.inner.extra_binds.iter().cloned())
        .map(|a| {
            a.parse::<std::net::SocketAddr>()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
        })
        .collect()
    }

    /// Bind the configured address — plus anything [`AppBuilder::bind`] added —
    /// and serve until Ctrl-C (SIGINT), then drain in-flight connections
    /// gracefully. Does not return until shutdown.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if any configured address is invalid or a
    /// socket cannot be bound (e.g. the port is in use).
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn run() -> std::io::Result<()> {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// app.start().await
    /// # }
    /// ```
    pub async fn start(self) -> std::io::Result<()> {
        self.start_with_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Bind and serve until the provided `shutdown` future resolves, then drain
    /// gracefully. Use this to wire a custom shutdown signal (e.g. in tests, or
    /// to combine SIGTERM with SIGINT).
    ///
    /// Serves every address [`AppBuilder::bind`] registered as well as the
    /// configured `host:port`; a bind failure on any one of them aborts the
    /// whole start rather than leaving a half-up server.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if any configured address is invalid or a
    /// socket cannot be bound.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn run() -> std::io::Result<()> {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    /// // ... later: tx.send(()) to trigger shutdown ...
    /// app.start_with_shutdown(async { let _ = rx.await; }).await
    /// # }
    /// ```
    pub async fn start_with_shutdown<F>(self, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut addrs = self.bind_addrs()?;
        // The single-address case keeps going through `serve`, which serves the
        // accept loop on this task. `serve_many` has to spawn one task per
        // listener and fan a broadcast out to them, so routing the common case
        // through it would change how a panic or a join failure surfaces for
        // every existing caller, to buy nothing.
        if addrs.len() == 1 {
            return crate::engine::serve(self, addrs.remove(0), shutdown).await;
        }
        crate::engine::serve_many(self, addrs, shutdown).await
    }

    /// Serve on an already-bound listener until `shutdown` resolves.
    ///
    /// The counterpart to [`start_with_shutdown`](App::start_with_shutdown) for
    /// callers that must know the port before the server runs — a supervisor
    /// passing a socket in, or a test that needs the address.
    ///
    /// It also closes a race the address-based entry points cannot: finding a
    /// free port by binding, dropping, and letting the server bind again leaves
    /// a window for something else to claim it. Handing over the listener
    /// removes the window.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn f() -> std::io::Result<()> {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    /// println!("listening on {}", listener.local_addr()?);
    /// app.start_on(listener, async { let _ = tokio::signal::ctrl_c().await; }).await
    /// # }
    /// ```
    pub async fn start_on<F>(
        self,
        listener: tokio::net::TcpListener,
        shutdown: F,
    ) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        crate::engine::serve_on(self, listener, shutdown).await
    }

    /// Serve on a Unix domain socket until Ctrl-C, then drain.
    #[cfg(unix)]
    pub async fn start_unix(self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        crate::engine::serve_unix(self, path, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Serve with one single-threaded runtime per worker, until Ctrl-C. Blocks
    /// the calling thread and builds its own runtimes, so call it from a plain
    /// `fn main` — **not** from inside `#[tokio::main]`.
    ///
    /// Pins each connection to one runtime for its whole life. Where the
    /// default [`start`](App::start) lets a connection's wakeups, handler and
    /// writes land on whichever worker thread is free — paying an atomic, a
    /// cache miss and often a syscall at each hop — this pays none of them.
    ///
    /// # The trade, measured
    ///
    /// On this project's comparison harness, the same application on the same
    /// pinned cores:
    ///
    /// | | requests/second | p99 latency |
    /// |---|---:|---:|
    /// | [`start`](App::start) (shared runtime) | 390,772 | **444 µs** |
    /// | `run_sharded` | **699,786** | 603 µs |
    ///
    /// **1.79× the throughput, for some tail latency.** With per-connection
    /// affinity, a request that lands on a busy worker waits for that worker
    /// instead of being picked up by an idle one, and the slowest percentile is
    /// where that shows up.
    ///
    /// Reach for it when throughput is the constraint and requests are many,
    /// short and uniform. Keep [`start`](App::start) when the 99th percentile is
    /// a number anyone looks at. See
    /// [`engine::serve_sharded`](crate::engine::serve_sharded) for why the
    /// acceptor is centralised rather than using `SO_REUSEPORT`.
    ///
    /// `workers` of `0` means one per available core.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if any configured address is invalid or
    /// cannot be bound, or if a worker thread cannot be started.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # fn main() -> std::io::Result<()> {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// app.run_sharded(0)
    /// # }
    /// ```
    pub fn run_sharded(self, workers: usize) -> std::io::Result<()> {
        let workers = match workers {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        let addrs = self.bind_addrs()?;
        crate::engine::serve_sharded(self, addrs, workers, async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }

    /// Consumes the handle rather than borrowing it, so the `Arc<AppInner>` the
    /// caller already owns is *moved* into the pipeline instead of cloned into
    /// it. Every clone of that pointer is an atomic read-modify-write on one
    /// cache line shared by every core serving requests, and the request path
    /// used to do five of them; this is the last one, and it does not happen.
    async fn run_pipeline(self, call: Call) -> Response {
        Next::new(self.inner).run(call).await
    }
}

/// Whether a lookup actually found a handler.
fn is_match(m: &crate::router::BorrowedMatch<'_>) -> bool {
    matches!(
        m,
        crate::router::BorrowedMatch::Found { .. }
            | crate::router::BorrowedMatch::OwnedFound { .. }
    )
}

impl AppInner {
    /// The middleware chain, in the order it runs.
    pub(crate) fn middleware(&self) -> &[Arc<dyn Middleware>] {
        &self.middleware
    }

    /// Route one call and run whatever the router found: the centre of the
    /// onion, reached once the middleware chain is spent.
    ///
    /// A method rather than the boxed closure this used to be. The closure was
    /// rebuilt on every request — `Arc::new` for the closure, `Box::pin` for
    /// the future it returned — purely so the pipeline could name its terminal
    /// uniformly. `Terminal::App` names it without allocating.
    pub(crate) async fn dispatch(self: Arc<Self>, mut call: Call) -> Response {
        let inner = self;
        {
            {
                // Borrowed from the call's URI, not copied out of it. This was
                // `call.path().to_string()` — one heap allocation and copy per
                // request, for a string every read below could have borrowed.
                // The borrow ends at the last read of `path`, which is before
                // `set_params` needs the call mutably.
                let path = call.uri().path();
                let method = call.method().clone();

                // RFC 9110 §9.3.7: `OPTIONS *` asks about the server as a
                // whole, not about a resource. Routing it as a path meant a
                // `404`, which says the server does not exist — the one answer
                // that is definitely wrong to a capability probe.
                if method == Method::OPTIONS && path == "*" {
                    let value = crate::router::allow_header_value(inner.router.all_methods())
                        .unwrap_or_else(|| Method::OPTIONS.as_str().to_string());
                    return Response::new(StatusCode::NO_CONTENT).with_header(
                        ALLOW,
                        HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("")),
                    );
                }

                // Resolve the path spelling once, here rather than in the
                // router, so there is a single decision point for it.
                //
                // "Here" is the endpoint, which is the *innermost* layer: the
                // middleware chain has already run on the way in, so middleware
                // observes the raw spelling and only the router and the
                // handlers are downstream of the decision. That is deliberate,
                // not an oversight. A refusal or a redirect is an ordinary
                // response that has to travel back out through the chain to be
                // finished — security headers live in `Phase::Setup` and
                // `on_error` in `Phase::Monitoring`, and a `404` is exactly as
                // reachable as handler output, so a decision taken outside the
                // chain would ship an alias refusal with neither.
                //
                // Nothing is bypassable by it under `Strict` or `Redirect`:
                // whatever a prefix-keyed middleware concludes from the raw
                // spelling, the endpoint replaces the response regardless, so
                // no handler runs. `Collapse` does keep the hazard, because it
                // serves the alias and does not rewrite the URI — which is what
                // `PathPolicy::Collapse` documents itself as, and why it is a
                // migration step rather than a supported posture.
                if let Some(canonical) = crate::path::canonical_path(path) {
                    match inner.config.path_policy {
                        crate::path::PathPolicy::Strict => {
                            return Response::text("Not Found").with_status(StatusCode::NOT_FOUND);
                        }
                        crate::path::PathPolicy::Redirect => {
                            // Carry the query across: dropping it would change
                            // the request while claiming to be the same one.
                            let target = match call.uri().query() {
                                Some(q) if !q.is_empty() => format!("{canonical}?{q}"),
                                _ => canonical,
                            };
                            // 308 rather than 301: a 301 lets a client retry a
                            // POST as a GET, which silently discards the body.
                            return Response::new(StatusCode::PERMANENT_REDIRECT).with_header(
                                http::header::LOCATION,
                                HeaderValue::from_str(&target)
                                    .unwrap_or(HeaderValue::from_static("/")),
                            );
                        }
                        // Fall through and let the router collapse them.
                        crate::path::PathPolicy::Collapse => {}
                    }
                }

                // Refuse an over-deep path before the router walks it. `walk`
                // recurses once per segment with backtracking, so depth is a
                // stack-depth question rather than merely a cost one.
                if inner.config.max_path_segments > 0
                    && path.split('/').filter(|s| !s.is_empty()).count()
                        > inner.config.max_path_segments
                {
                    return Response::text("URI Too Long").with_status(StatusCode::URI_TOO_LONG);
                }

                // `route_borrowed`, not `route`: the handler comes back
                // borrowed from the router, so the hot path does not do an
                // atomic refcount bump on a cache line every core serving this
                // route shares. See `Router::route_borrowed`.
                let mut lookup = inner.router.route_borrowed(&method, path, &call);

                // RFC 9110 §9.3.2: HEAD must be available wherever GET is.
                // Only synthesized when no HEAD route was registered, so an
                // explicit HEAD handler always wins.
                let mut synthesized_head = false;
                if method == Method::HEAD && !is_match(&lookup) {
                    let as_get = inner.router.route_borrowed(&Method::GET, path, &call);
                    if is_match(&as_get) {
                        lookup = as_get;
                        synthesized_head = true;
                    }
                }

                // Automatic OPTIONS, only where no handler claimed the request.
                // CORS runs in the Plugins phase and short-circuits preflight
                // before this endpoint is reached, so an installed Cors keeps
                // priority over this.
                if method == Method::OPTIONS && !is_match(&lookup) {
                    if let Some(value) =
                        crate::router::allow_header_value(inner.router.methods_for(path))
                    {
                        return Response::new(StatusCode::NO_CONTENT).with_header(
                            ALLOW,
                            HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("")),
                        );
                    }
                }

                let (handler, owned_handler, params, rest) = match lookup {
                    BorrowedMatch::Found { handler, params } => {
                        (Some(handler), None, Some(params), None)
                    }
                    BorrowedMatch::OwnedFound { handler, params } => {
                        (None, Some(handler), Some(params), None)
                    }
                    BorrowedMatch::Other(other) => (None, None, None, Some(other)),
                };

                if let Some(params) = params {
                    call.set_params(params);
                    // The borrow of the router ends when `handle` returns: it
                    // hands back an owned `'static` future, so nothing is held
                    // across the await.
                    let fut = match (handler, &owned_handler) {
                        (Some(h), _) => h.handle(call),
                        (None, Some(h)) => h.handle(call),
                        (None, None) => unreachable!("params implies a handler"),
                    };
                    let res = fut.await;
                    return if synthesized_head {
                        strip_body(res)
                    } else {
                        res
                    };
                }

                match rest.expect("no handler implies a non-match") {
                    // Unreachable: a `Found` was turned into a handler above.
                    Match::Found { handler, params } => {
                        call.set_params(params);
                        handler.handle(call).await
                    }
                    // Same generator as the `OPTIONS` arm above: one resource,
                    // one answer. `None` means nothing is registered here at
                    // all, which is a `404` — a `405` with an empty `Allow`
                    // would tell the client to retry with nothing.
                    Match::MethodNotAllowed { allow } => {
                        match crate::router::allow_header_value(allow) {
                            Some(value) => Response::new(StatusCode::METHOD_NOT_ALLOWED)
                                .with_header(
                                    ALLOW,
                                    HeaderValue::from_str(&value)
                                        .unwrap_or(HeaderValue::from_static("")),
                                ),
                            None => Response::text("Not Found").with_status(StatusCode::NOT_FOUND),
                        }
                    }
                    Match::NotFound => {
                        Response::text("Not Found").with_status(StatusCode::NOT_FOUND)
                    }
                    // A malformed escape or non-UTF-8 bytes in the path. The
                    // request is unintelligible rather than merely unmatched,
                    // so it is a 400 and not a 404.
                    Match::BadPath => {
                        Response::text("Bad Request").with_status(StatusCode::BAD_REQUEST)
                    }
                }
            }
        }
    }
}

/// Adds the `Alt-Svc` header that points TCP clients at HTTP/3.
///
/// In [`Phase::Setup`], the outermost phase, so it is present on error
/// responses too. A client that only ever gets a `404` from an origin should
/// still learn that the origin speaks h3.
struct AltSvc(u16);

#[async_trait::async_trait]
impl Middleware for AltSvc {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let mut res = next.run(call).await;
        // `ma` is how long the client may remember this, in seconds. A day is
        // the common choice: long enough to matter, short enough that turning
        // h3 off does not strand clients for a week.
        if let Ok(value) = http::HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", self.0)) {
            res.headers
                .insert(http::header::HeaderName::from_static("alt-svc"), value);
        }
        res
    }
}

/// Runs the application's [`on_error`](AppBuilder::on_error) renderer over any
/// error response the pipeline produces. A connection-level refusal is answered
/// before there is a pipeline to run — see the note on `on_error`.
struct ErrorPages {
    render: ErrorRenderer,
}

#[async_trait::async_trait]
impl Middleware for ErrorPages {
    async fn handle(&self, call: Call, next: Next) -> Response {
        // The renderer needs the request, and `next.run` consumes the call, so
        // keep the parts it can ask about.
        let snapshot = call.snapshot_for_error();
        let res = next.run(call).await;
        if res.status.is_client_error() || res.status.is_server_error() {
            if let Some(mut replacement) = (self.render)(res.status, &snapshot) {
                // Carry over headers the renderer did not set. Some of them are
                // not decoration: RFC 9110 §15.5.6 requires `Allow` on a `405`,
                // §15.5.2 requires `WWW-Authenticate` on a `401`, and a `416`
                // carries `Content-Range`. Replacing the whole response dropped
                // them, so installing `on_error` silently made those responses
                // non-conforming. The renderer still wins wherever it sets a
                // header itself.
                for name in res.headers.keys() {
                    if replacement.headers.contains_key(name) {
                        continue;
                    }
                    // Never carry a header that describes the body that was
                    // just replaced. `Content-Length` in particular is framed
                    // on by hyper, so grafting the original's length onto a
                    // different body truncates or hangs the response; with the
                    // header absent hyper measures the real body.
                    if name == http::header::CONTENT_LENGTH
                        || name == http::header::CONTENT_TYPE
                        || name == http::header::CONTENT_ENCODING
                    {
                        continue;
                    }
                    // Every value, not just the first: `HeaderMap::iter` yields
                    // one item per value, so a `contains_key` guard inside that
                    // loop kept only the first `Set-Cookie` of several.
                    for value in res.headers.get_all(name) {
                        replacement.headers.append(name.clone(), value.clone());
                    }
                }
                return replacement;
            }
        }
        res
    }
}

/// Drop a response body for a synthesized `HEAD` reply, keeping status and
/// headers.
///
/// RFC 9110 §9.3.2 says a `HEAD` response should carry the same header fields a
/// `GET` would, so a buffered body's length is preserved as `Content-Length`
/// before the bytes are discarded — clients do use `HEAD` to size a resource.
///
/// A streamed body has no known length and is dropped rather than drained:
/// draining it would do exactly the work the client declined to ask for. The
/// same section permits omitting fields that are only determined while
/// generating the content, which is precisely this case.
fn strip_body(mut res: Response) -> Response {
    if let Some(bytes) = res.body.as_bytes() {
        if let Ok(value) = HeaderValue::from_str(&bytes.len().to_string()) {
            res.headers.insert(http::header::CONTENT_LENGTH, value);
        }
    }
    res.body = crate::body::Body::empty();
    res
}

/// The framework entry point — a zero-sized namespace for starting an
/// [`AppBuilder`].
///
/// Begin with [`Churust::server`] for an empty builder, or
/// [`Churust::from_config`] to seed it from `churust.toml` plus `CHURUST_*`
/// environment variables.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
///     .build();
/// assert_eq!(TestClient::new(app).get("/").send().await.text(), "hi");
/// # });
/// ```
pub struct Churust;

impl Churust {
    /// Start a fresh [`AppBuilder`] with default configuration.
    pub fn server() -> AppBuilder {
        AppBuilder::new()
    }

    /// Start an [`AppBuilder`] pre-loaded from `churust.toml` and `CHURUST_*`
    /// environment variables (via [`Config::load_default`](crate::Config::load_default)).
    /// Chain DSL setters afterward to override individual fields.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// // Loads churust.toml + env, then overrides the port in code.
    /// let app = Churust::from_config()
    ///     .port(3000)
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// let _ = app;
    /// ```
    pub fn from_config() -> AppBuilder {
        AppBuilder::new().with_config(crate::config::Config::load_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Next;
    use async_trait::async_trait;

    #[tokio::test]
    async fn middleware_runs_in_phase_order() {
        use crate::pipeline::{Next, Phase};
        use async_trait::async_trait;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Recorder {
            log: Arc<Mutex<Vec<&'static str>>>,
            tag: &'static str,
        }
        #[async_trait]
        impl Middleware for Recorder {
            async fn handle(&self, call: Call, next: Next) -> Response {
                self.log.lock().unwrap().push(self.tag);
                next.run(call).await
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let mut builder = Churust::server();
        // Install OUT of phase order; expect execution IN phase order.
        builder.add_middleware_in(
            Phase::Fallback,
            Arc::new(Recorder {
                log: log.clone(),
                tag: "fallback",
            }),
        );
        builder.add_middleware_in(
            Phase::Setup,
            Arc::new(Recorder {
                log: log.clone(),
                tag: "setup",
            }),
        );
        builder.add_middleware_in(
            Phase::Monitoring,
            Arc::new(Recorder {
                log: log.clone(),
                tag: "monitoring",
            }),
        );
        let app = builder
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build();
        let _ = get(&app, "/").await;
        assert_eq!(
            *log.lock().unwrap(),
            vec!["setup", "monitoring", "fallback"]
        );
    }

    fn app() -> App {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "home" });
                r.get("/boom", |_c: Call| async {
                    panic!("handler exploded");
                    #[allow(unreachable_code)]
                    ""
                });
            })
            .build()
    }

    async fn get(app: &App, path: &str) -> Response {
        app.process(
            Method::GET,
            path.parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
    }

    #[tokio::test]
    async fn routes_to_handler() {
        let res = get(&app(), "/").await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, Bytes::from("home"));
    }

    #[tokio::test]
    async fn unknown_path_is_404() {
        let res = get(&app(), "/missing").await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn panicking_handler_yields_500_not_crash() {
        let res = get(&app(), "/boom").await;
        assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn process_with_extensions_seeds_call() {
        #[derive(Clone)]
        struct Marker(u32);
        let app = Churust::server()
            .routing(|r| {
                r.get("/", |c: Call| async move {
                    format!("{}", c.get::<Marker>().map(|m| m.0).unwrap_or(0))
                });
            })
            .build();
        let mut ext = http::Extensions::new();
        ext.insert(Marker(7));
        let res = app
            .process_with_extensions(
                Method::GET,
                "/".parse().unwrap(),
                HeaderMap::new(),
                Bytes::new(),
                ext,
            )
            .await;
        assert_eq!(res.body, Bytes::from("7"));
    }

    #[tokio::test]
    async fn state_extractor_end_to_end() {
        use crate::extract::State;
        #[derive(Clone)]
        struct Counter(u32);

        let app = Churust::server()
            .state(Counter(5))
            .routing(|r| {
                r.get(
                    "/n",
                    |s: State<Counter>| async move { format!("n={}", s.0 .0) },
                );
            })
            .build();
        let res = get(&app, "/n").await;
        assert_eq!(res.body, Bytes::from("n=5"));
    }

    #[tokio::test]
    async fn middleware_installed_via_plugin_runs() {
        struct MarkPlugin;
        struct Mark;
        #[async_trait]
        impl Middleware for Mark {
            async fn handle(&self, call: Call, next: Next) -> Response {
                let mut res = next.run(call).await;
                res.headers.insert(
                    http::header::HeaderName::from_static("x-plugin"),
                    HeaderValue::from_static("on"),
                );
                res
            }
        }
        impl Plugin for MarkPlugin {
            fn install(self: Box<Self>, app: &mut AppBuilder) {
                app.add_middleware(Arc::new(Mark));
            }
        }

        let app = Churust::server()
            .install(MarkPlugin)
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build();
        let res = get(&app, "/").await;
        assert_eq!(res.headers.get("x-plugin").unwrap(), "on");
    }
}
