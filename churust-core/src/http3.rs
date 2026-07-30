//! HTTP/3 over QUIC (feature `http3`).
//!
//! HTTP/3 is a different transport, not a different framing of the same one:
//! it runs over QUIC on UDP, so it needs its own socket and its own listener
//! rather than an extra branch inside the TCP engine. That is why this is
//! started separately from [`App::start`](crate::App::start), and why an
//! application that wants both runs both.
//!
//! ```no_run
//! use churust_core::{Call, Churust};
//!
//! # async fn run() -> std::io::Result<()> {
//! let app = Churust::server()
//!     // Tell HTTP/1.1 and HTTP/2 clients that h3 is available on the same
//!     // port, so they can upgrade themselves on the next request.
//!     .advertise_http3(8443)
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "served over h3" });
//!     })
//!     .build();
//!
//! churust_core::http3::serve(
//!     app,
//!     "0.0.0.0:8443".parse().unwrap(),
//!     "cert.pem",
//!     "key.pem",
//! )
//! .await
//! # }
//! ```
//!
//! # How clients find it
//!
//! They do not, on their own. A browser reaches a new origin over TCP, and only
//! moves to h3 if the response says h3 exists. That is the `Alt-Svc` header, and
//! [`AppBuilder::advertise_http3`](crate::AppBuilder::advertise_http3) is what
//! sets it. Serving h3 without advertising it means almost nothing will ever
//! use it.
//!
//! # What is supported
//!
//! Request routing, headers, request bodies and response bodies, including
//! streamed ones, through the same pipeline every other transport uses: a
//! handler cannot tell which transport it is answering.
//!
//! WebSockets are not carried over h3. The upgrade mechanism there is Extended
//! CONNECT from RFC 9220, which is a different handshake from the HTTP/1.1 one
//! the `ws` feature implements, and pretending otherwise would produce a
//! connection that fails at the first frame.

#![cfg(feature = "http3")]

use crate::app::App;
use crate::body::Body;
use crate::pem::{load_certs, load_key};
use bytes::{Buf, Bytes};
use http::{HeaderMap, Method, Request, Uri};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

/// How long a QUIC connection may sit idle before quinn closes it, when no
/// value is supplied.
///
/// Matches the default `keep_alive_ms`, so an idle connection costs the same
/// whichever transport it arrived on. [`serve`] passes the app's own
/// `keep_alive_ms` instead; this is what [`server_config_from_pem`] uses, since
/// it is a free function with no access to the app config.
const DEFAULT_IDLE_MS: u64 = 75_000;

/// Build a QUIC server configuration from a PEM certificate chain and key.
///
/// The ALPN protocol is `h3` and nothing else: a QUIC connection that
/// negotiated anything else would not be HTTP/3, and there is nothing here to
/// hand it to.
///
/// The transport bounds are the defaults, because there is no app here to ask.
/// [`serve`] builds its config through the same code with the app's own
/// settings, so the knobs are honoured on the ordinary path; a caller reaching
/// for this function directly and wanting other values adjusts the returned
/// config's transport before handing it to [`serve_with_config`].
///
/// # Errors
///
/// If either file is missing or unreadable, if no private key is found, or if
/// rustls rejects the pair.
pub fn server_config_from_pem(cert_path: &str, key_path: &str) -> io::Result<quinn::ServerConfig> {
    server_config_from_pem_with_limits(cert_path, key_path, TransportLimits::defaults())
}

/// The QUIC idle bound for a configured `keep_alive_ms`.
///
/// `0` means "answer and close" on TCP. QUIC has no equivalent: a connection
/// multiplexes streams, and `max_idle_timeout(None)` is the opposite — it never
/// expires, which would unbound the `max_connections` slot the timeout exists to
/// release. Fall back to the default bound rather than invent a QUIC meaning for
/// a TCP-shaped setting.
fn idle_ms_for(keep_alive_ms: u64) -> u64 {
    match keep_alive_ms {
        0 => DEFAULT_IDLE_MS,
        ms => ms,
    }
}

/// Convert milliseconds into a QUIC idle timeout.
///
/// An error rather than an `expect`, because the value is no longer a constant:
/// a QUIC idle timeout is carried as a varint and rejects anything past its
/// range, and `keep_alive_ms` is a `u64` an operator fills in. Refusing at
/// startup names the bad setting; panicking would take the process down for it.
fn idle_timeout(idle_ms: u64) -> io::Result<quinn::IdleTimeout> {
    std::time::Duration::from_millis(idle_ms)
        .try_into()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("keep_alive_ms = {idle_ms} is too large for a QUIC idle timeout: {e}"),
            )
        })
}

/// The per-connection stream cap for a configured `h2_max_concurrent_streams`.
///
/// `0` removes the limit on HTTP/2. QUIC has no unlimited: the value is carried
/// in a transport parameter as a varint, so the nearest thing is its maximum.
///
/// A cap matters more here than the name suggests. Every in-flight request
/// buffers its body up to `max_body_bytes`, so what a single connection can make
/// this process hold is that cap multiplied by the streams it may open at once —
/// and quinn's own default of 100 applied however `h2_max_concurrent_streams`
/// was set, which is the same knob-ignored-on-one-transport shape as the idle
/// timeout above.
fn max_streams_for(h2_max_concurrent_streams: u32) -> quinn::VarInt {
    match h2_max_concurrent_streams {
        0 => quinn::VarInt::MAX,
        n => quinn::VarInt::from_u32(n),
    }
}

/// The QUIC transport bounds that come from the app's own configuration.
///
/// Grouped so there is one list of them: a *transport parameter* that belongs
/// here and is not in this struct is one HTTP/3 silently ignores, which is the
/// failure this type exists to make visible.
///
/// Only quinn transport parameters, because that is all this reaches — it is
/// handed to [`server_config_from_pem_with_limits`] and nothing else. App-level
/// deadlines are applied where the work they bound happens, in
/// [`serve_connection`]: `h2_max_header_list_size` as h3's
/// `max_field_section_size`, and `header_read_timeout_ms` around the wait for a
/// HEADERS frame. Both consequently apply on the [`serve_with_config`] path too,
/// where these bounds do not.
struct TransportLimits {
    /// From `keep_alive_ms`, via [`idle_ms_for`].
    idle_ms: u64,
    /// From `h2_max_concurrent_streams`, via [`max_streams_for`].
    max_streams: quinn::VarInt,
}

impl TransportLimits {
    /// The bounds this app configures.
    fn from(cfg: &crate::app::ServerConfig) -> Self {
        Self {
            idle_ms: idle_ms_for(cfg.keep_alive_ms),
            max_streams: max_streams_for(cfg.h2_max_concurrent_streams),
        }
    }

    /// The defaults, for a caller with no app to ask.
    fn defaults() -> Self {
        Self::from(&crate::app::ServerConfig::default())
    }
}

/// [`server_config_from_pem`], with the transport bounds named rather than
/// assumed.
///
/// The one place QUIC transport bounds are applied, so a value cannot drift away
/// from the setting it came from the way a second copy would.
fn server_config_from_pem_with_limits(
    cert_path: &str,
    key_path: &str,
    limits: TransportLimits,
) -> io::Result<quinn::ServerConfig> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(quic));

    // Bound the connection's life, not just the request's. A permit is held for
    // as long as the QUIC connection lives, and quinn's default idle timeout is
    // negotiated with the peer — so a client that opens a connection and then
    // says nothing at all, or answers one request and lingers, pinned a
    // `max_connections` slot indefinitely. Neither case involves a request, so
    // no request-level deadline can reach them.
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(idle_timeout(limits.idle_ms)?));
    // Bidirectional only: an HTTP/3 request arrives on a bidi stream, and the
    // unidirectional ones carry control and QPACK data rather than requests, so
    // a request-concurrency setting is not the right bound for them.
    transport.max_concurrent_bidi_streams(limits.max_streams);
    config.transport_config(Arc::new(transport));
    Ok(config)
}

/// Serve `app` over HTTP/3 on `addr` until the process ends.
///
/// The transport bounds come from the app: `keep_alive_ms` becomes the QUIC idle
/// timeout and `h2_max_concurrent_streams` the per-connection request limit, so
/// lowering either bounds this transport too and not only TCP.
///
/// `header_read_timeout_ms` and `h2_max_header_list_size` are honoured as well,
/// applied per stream in [`serve_connection`] rather than as QUIC transport
/// parameters, so they hold on [`serve_with_config`] too.
///
/// # Errors
///
/// If the certificate or key cannot be loaded, if no private key is found, if
/// rustls rejects the pair, if `keep_alive_ms` is too large for a QUIC idle
/// timeout, or if the UDP socket cannot be bound.
pub async fn serve(app: App, addr: SocketAddr, cert_path: &str, key_path: &str) -> io::Result<()> {
    let limits = TransportLimits::from(app.config());
    let config = server_config_from_pem_with_limits(cert_path, key_path, limits)?;
    serve_with_config(app, addr, config).await
}

/// Serve `app` over HTTP/3 with an already-built QUIC configuration.
///
/// The configuration is used as given: `keep_alive_ms` is not applied here,
/// because a caller who built the transport themselves has already said what
/// they want. [`serve`] is the path that reads the app's knob.
///
/// # Errors
///
/// If the UDP socket cannot be bound.
pub async fn serve_with_config(
    app: App,
    addr: SocketAddr,
    config: quinn::ServerConfig,
) -> io::Result<()> {
    Http3Server::bind(addr, config)?.serve(app).await;
    Ok(())
}

/// A bound QUIC listener, not yet serving.
///
/// Binding and serving are separate steps so the port can be read back before
/// anything is accepted. That matters for the ordinary case of binding port 0
/// and telling something else where to connect, and it is what lets a test know
/// the address without racing the server.
pub struct Http3Server {
    endpoint: quinn::Endpoint,
}

impl std::fmt::Debug for Http3Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http3Server")
            .field("local_addr", &self.endpoint.local_addr().ok())
            .finish_non_exhaustive()
    }
}

impl Http3Server {
    /// Bind a UDP socket for QUIC without serving yet.
    ///
    /// # Errors
    ///
    /// If the socket cannot be bound.
    pub fn bind(addr: SocketAddr, config: quinn::ServerConfig) -> io::Result<Self> {
        Ok(Self {
            endpoint: quinn::Endpoint::server(config, addr)?,
        })
    }

    /// The address actually bound, which is what resolves a port of 0.
    ///
    /// # Errors
    ///
    /// If the socket cannot report its address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Accept connections and serve them through `app` until the endpoint
    /// closes.
    pub async fn serve(self, app: App) {
        accept_loop(app, self.endpoint).await;
    }
}

/// Accept QUIC connections until the endpoint closes.
async fn accept_loop(app: App, endpoint: quinn::Endpoint) {
    // The same connection budget the TCP engine applies. Without it, enabling
    // http3 opted a deployment out of `max_connections` entirely — and
    // `advertise_http3` actively steers clients here.
    let slots = (app.config().max_connections > 0)
        .then(|| std::sync::Arc::new(tokio::sync::Semaphore::new(app.config().max_connections)));

    // `max_tls_handshakes` and `tls_handshake_timeout_ms`, applied to QUIC for
    // the reason they exist on TCP: a QUIC handshake *is* a TLS 1.3 handshake,
    // asymmetric in exactly the same way — cheap to ask for, expensive to
    // answer — so `max_connections` alone is too loose a bound on it. Enabling
    // http3 previously opted a deployment out of both knobs, which is the same
    // hole `slots` above was added to close.
    //
    // The existing knobs rather than QUIC-specific ones, on the precedent
    // `serve_connection` sets just below for `h2_max_header_list_size`: the
    // setting asks the same question of the same kind of work, so one value
    // should not need saying twice.
    let handshakes = (app.config().max_tls_handshakes > 0).then(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(app.config().max_tls_handshakes))
    });
    let handshake_timeout = (app.config().tls_handshake_timeout_ms > 0)
        .then(|| std::time::Duration::from_millis(app.config().tls_handshake_timeout_ms));

    while let Some(incoming) = endpoint.accept().await {
        let app = app.clone();
        // Taken before accepting, so excess load waits in QUIC's own queue
        // rather than as memory in this process. Held for the connection's
        // life by the task below.
        let permit = match &slots {
            Some(sem) => sem.clone().acquire_owned().await.ok(),
            None => None,
        };
        let handshakes = handshakes.clone();
        // One task per connection, like the TCP engine: a slow peer must not
        // hold up the accept loop.
        tokio::spawn(async move {
            let _permit = permit;

            // Queueing for the handshake budget and performing the handshake
            // are one timed step, as on TCP. Timing only the handshake would
            // turn the budget into a rate limiter: permits would expire at
            // `max_tls_handshakes` per deadline while everything else waited on
            // `acquire_owned` with no deadline at all, and each of those waiters
            // is already holding a connection permit taken above. What the knob
            // promises is that no peer holds a connection permit for longer
            // than this without having proved it can speak TLS, so the wait for
            // the budget has to be inside the deadline that guards it.
            //
            // The handshake permit is scoped to this block and released when it
            // ends — before `serve_connection` runs. The handshake budget is
            // for handshakes; the connection budget takes over from there.
            let queue_and_shake = async {
                let _handshake_permit = match &handshakes {
                    Some(sem) => sem.clone().acquire_owned().await.ok(),
                    None => None,
                };
                incoming.await
            };

            let accepted = match handshake_timeout {
                Some(limit) => match tokio::time::timeout(limit, queue_and_shake).await {
                    Ok(res) => res,
                    // Dropping the future cancels the acquire as well as the
                    // handshake, so a peer that timed out while queued returns
                    // nothing and holds nothing. Until the handshake completes
                    // there is no HTTP layer, so no request-level deadline can
                    // reach this.
                    Err(_) => {
                        tracing::debug!("http3 handshake timed out");
                        return;
                    }
                },
                None => queue_and_shake.await,
            };

            match accepted {
                Ok(connection) => {
                    if let Err(e) = serve_connection(app, connection).await {
                        tracing::debug!(error = %e, "http3 connection ended");
                    }
                }
                // A failed handshake on an internet-facing port is constant
                // background noise, so debug rather than warn, matching how the
                // TCP engine treats a failed TLS handshake.
                Err(e) => tracing::debug!(error = %e, "http3 handshake failed"),
            }
        });
    }
}

/// Run one QUIC connection's request streams through the pipeline.
async fn serve_connection(
    app: App,
    connection: quinn::Connection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // The peer address, which the TCP engine seeds and this path did not — so
    // `Call::peer_addr` was `None` and anything keying on it (rate limiting,
    // audit logs, IP allowlists) treated the entire h3 fleet as one client.
    let peer = connection.remote_address();

    // h3's default `max_field_section_size` is `VarInt::MAX`, i.e. no bound on
    // a header block at all, where the TCP engine caps HTTP/1 headers by count
    // and HTTP/2 by encoded size. Reuse the HTTP/2 knob: it asks the same
    // question of the same kind of header block.
    let mut h3 = h3::server::builder()
        .max_field_section_size(app.config().h2_max_header_list_size as u64)
        .build(h3_quinn::Connection::new(connection))
        .await?;

    // `header_read_timeout_ms`, the slow-loris defence, which this transport did
    // not apply to any phase of a request.
    //
    // `accept` returns as soon as a bidi stream exists, before a frame has been
    // read, so a peer could open `h2_max_concurrent_streams` streams, send no
    // HEADERS frame on any of them, and hold a task and an h3 resolver per stream
    // for the life of the process. Nothing reclaimed them: `request_timeout_ms`
    // is armed further down, after the headers have arrived; the QUIC idle
    // timeout is a packet-activity timer that any keepalive resets, where the TCP
    // watchdog measures *completed requests* and so does close a connection whose
    // streams never produce one; and the handshake deadline has ended long
    // before a stream exists.
    //
    // Read here rather than through `TransportLimits`, which is only for quinn
    // transport parameters — this is an app-level deadline, and the precedent is
    // `max_field_section_size` just above. One consequence worth naming: like
    // that knob, it therefore also applies on the `serve_with_config` path.
    let header_deadline = (app.config().header_read_timeout_ms > 0)
        .then(|| std::time::Duration::from_millis(app.config().header_read_timeout_ms));

    loop {
        match h3.accept().await {
            Ok(Some(resolver)) => {
                let app = app.clone();
                // One task per request: h3 multiplexes streams on one
                // connection, so serving them in sequence would make a slow
                // handler block every other request on that connection.
                //
                // Resolving belongs inside the task for the same reason, and
                // for a second one. `accept` returns as soon as a bidi stream
                // exists, before any frame has been read, so awaiting
                // `resolve_request` out here waited for one peer's HEADERS
                // frame before the next stream could even be accepted — a
                // stream whose headers never arrive head-of-line blocked every
                // request behind it. And its error is a *stream* error:
                // propagating it with `?` returned from this function, dropped
                // the `h3::server::Connection`, and closed the whole QUIC
                // connection, killing every other request multiplexed on it.
                tokio::spawn(async move {
                    // Bounded per stream, never per connection. A sibling that
                    // never sends its headers must not take the connection down
                    // with it — dropping the resolver drops that stream alone,
                    // and `h3::server::Connection` is untouched.
                    let resolved = match header_deadline {
                        Some(limit) => match tokio::time::timeout(limit, resolver.resolve_request())
                            .await
                        {
                            Ok(res) => res,
                            // Dropping the timed-out future drops the resolver,
                            // which drops the quinn stream. `resolve_request`
                            // consumes the resolver, so there is no way to send a
                            // deliberate H3_REQUEST_INCOMPLETE from here without
                            // reaching a `doc(hidden)` field; the implicit reset
                            // is the report, and no request was dispatched.
                            Err(_) => {
                                tracing::debug!("http3 request headers timed out");
                                return;
                            }
                        },
                        None => resolver.resolve_request().await,
                    };
                    let (request, stream) = match resolved {
                        Ok(pair) => pair,
                        // Scoped to this stream by construction. h3 has already
                        // done whatever the stream needed — a `431` for an
                        // oversized header block, a reset for one that ended
                        // before its headers — and the connection is untouched.
                        //
                        // The genuinely connection-fatal case is not lost by
                        // handling it here: h3 records it on the shared
                        // connection state, so the next `accept` above returns
                        // it and the `Err` arm ends the connection then. That
                        // path is also *better* than the `?` was, because it
                        // lets h3 emit the real code — H3_FRAME_UNEXPECTED,
                        // say — where dropping the connection sent
                        // H3_NO_ERROR for what was actually a protocol
                        // violation.
                        Err(e) => {
                            tracing::debug!(error = %e, "http3 request headers failed");
                            return;
                        }
                    };
                    if let Err(e) = serve_request(app, request, stream, peer).await {
                        tracing::debug!(error = %e, "http3 request failed");
                    }
                });
            }
            // The peer closed the connection cleanly.
            Ok(None) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}

/// Answer one HTTP/3 request.
async fn serve_request<S>(
    app: App,
    request: Request<()>,
    mut stream: h3::server::RequestStream<S, Bytes>,
    peer: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let (parts, _) = request.into_parts();
    let max_body = app.config().max_body_bytes;

    // Refuse a body the client has already declared too large, before a byte of
    // it is read — the same check the TCP path makes for the same reason.
    // `read_body` below refuses at the chunk that crosses the cap, which bounds
    // what is held but still accepts and buffers everything up to it.
    // `Content-Length` is the client's own statement of size, so refusing on it
    // costs one header lookup and no buffering at all. A body that declares
    // nothing is still bounded by the cap when it is read.
    if let Some(declared) = parts
        .headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        if declared > max_body as u64 {
            return send_status(&app, &mut stream, http::StatusCode::PAYLOAD_TOO_LARGE).await;
        }
    }

    // The deadline has to start here, not after the body has arrived. Reading
    // the body is the attacker-controlled phase — a client can dribble one byte
    // and stop — and wrapping only the handler left exactly that phase
    // unbounded, which the TCP path does not do.
    let timeout_ms = app.config().request_timeout_ms;
    let deadline = (timeout_ms > 0)
        .then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms));

    let read = async { read_body(&mut stream, max_body).await };
    let read = match deadline {
        Some(at) => match tokio::time::timeout_at(at, read).await {
            Ok(r) => r,
            Err(_) => {
                return send_status(&app, &mut stream, http::StatusCode::REQUEST_TIMEOUT).await;
            }
        },
        None => read.await,
    };

    let body = match read {
        Ok(body) => body,
        Err(BodyRefused::TooLarge) => {
            return send_status(&app, &mut stream, http::StatusCode::PAYLOAD_TOO_LARGE).await;
        }
        // Reset rather than a 400, and the reason is what can actually reach
        // the peer. The stream failed while we were reading it, so in the
        // common case the peer has already reset its side — and a client that
        // resets a request stream is cancelling it, which under RFC 9114 §4.1
        // means it also stops reading the response. A status written into that
        // is discarded, and when the failure was a `ConnectionError` there is
        // no connection left to write it to at all. A 400 would therefore be a
        // status this server believes it sent and the client never saw.
        //
        // Returning without resetting is not an option either, for the reason
        // spelled out on the streamed-response arm below: the `RequestStream`
        // is dropped on the way out and quinn finishes a stream when it drops,
        // so the peer would get a clean FIN. Resetting first is what wins that
        // race, and H3_REQUEST_INCOMPLETE is the code RFC 9114 defines for
        // exactly this — "the client's stream terminated without containing a
        // fully-formed request".
        //
        // The part that matters most is above this line rather than in it: we
        // return before `process_with_extensions`, so the handler never runs
        // and never commits half a payload. Refusing the stream is the report;
        // not dispatching is the fix.
        Err(BodyRefused::Incomplete(e)) => {
            tracing::debug!(error = %e, "http3 request body ended before it was complete");
            stream.stop_stream(h3::error::Code::H3_REQUEST_INCOMPLETE);
            return Ok(());
        }
    };

    let mut extensions = http::Extensions::new();
    extensions.insert(crate::call::PeerAddr(peer));

    // Carry the authority across in the `Host` field, because `normalise_uri`
    // below is about to drop it.
    //
    // `Call::host` reads the URI authority first and the `Host` field only as a
    // fallback (`call.rs`), and HTTP/3 replaced the field with the `:authority`
    // pseudo-header, which arrives in the URI. Normalising to origin form
    // therefore removed the only signal there was: `Call::host` answered `None`
    // over h3, so `guard::host` matched nothing and a host-scoped route was
    // either a silent 404 or — with an unguarded sibling on the same method and
    // path, which is the ordinary virtual-host shape — served by the wrong
    // handler. `guard.rs` documents this same failure as a fixed HTTP/2
    // regression; on this transport it had come back.
    //
    // Seeding the field rather than keeping the authority in the URI is what
    // leaves `normalise_uri`'s own decision intact: a handler still sees the
    // same origin-form target on every transport.
    //
    // `insert`, not `append`: `Call::host` resolves a request carrying both an
    // authority and a `Host` field to the authority, so a stray field must not
    // survive beside the value it is supposed to lose to.
    let mut headers = parts.headers.clone();
    if let Some(authority) = parts.uri.authority() {
        if let Ok(value) = http::HeaderValue::from_str(authority.as_str()) {
            headers.insert(http::header::HOST, value);
        }
    }

    let process = app.process_with_extensions(
        parts.method.clone(),
        normalise_uri(&parts.uri, &parts.method),
        headers,
        body,
        extensions,
    );

    // The remainder of the same budget, so the whole exchange is bounded once
    // rather than the body and the handler each getting a full allowance.
    let response = match deadline {
        None => process.await,
        Some(at) => match tokio::time::timeout_at(at, process).await {
            Ok(res) => res,
            Err(_) => crate::response::Response::text("Request Timeout")
                .with_status(http::StatusCode::REQUEST_TIMEOUT),
        },
    };

    send_response(&app, &mut stream, response, parts.method == Method::HEAD).await
}

/// Answer with a bare status and nothing else, hardened like any other reply.
///
/// The refusals above are composed here rather than in the pipeline, which is
/// why they need their own call to `apply_security_headers`: the middleware
/// that would otherwise add them never runs for a request that was never
/// dispatched. Funnelled through one function so the next refusal added to
/// `serve_request` cannot forget.
async fn send_status<S>(
    app: &App,
    stream: &mut h3::server::RequestStream<S, Bytes>,
    status: http::StatusCode,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut response = http::Response::builder().status(status).body(())?;
    app.apply_security_headers(response.headers_mut(), true);
    stream.send_response(response).await?;
    stream.finish().await?;
    Ok(())
}

/// Why a request body was not handed to the pipeline.
enum BodyRefused {
    /// The request body exceeded the server's cap.
    TooLarge,
    /// The stream failed before the body was whole, so what was read is a
    /// fragment of a request rather than a request.
    Incomplete(h3::error::StreamError),
}

/// Read a request body, refusing one past `max_body` and one cut short.
///
/// Counted as it arrives rather than collected and then measured, so an
/// oversized body is refused at the chunk that crosses the line instead of
/// after all of it has been held in memory.
///
/// # Why this buffers where the TCP engine streams
///
/// Deliberately, and not because streaming was overlooked. Collecting the body
/// before dispatch is what lets a truncated one be refused *without the handler
/// ever running*, which is the guarantee the `Incomplete` arm below exists for:
/// a peer that announces 5000 bytes, sends 1200 and resets has sent a fragment,
/// and a fragment is undetectable after the fact because a truncated body is a
/// well-formed shorter body.
///
/// The TCP path hands the handler a stream instead, so there the truncation
/// surfaces mid-handler — after any side effect it has on the bytes it already
/// read. `Call::body_stream` says as much: exceeding the cap "surfaces as an
/// error item in the stream rather than a `413`, because the response has
/// usually begun by then". This transport is the stricter of the two, and the
/// cost is memory: one `max_body_bytes` per in-flight request.
///
/// That cost is bounded rather than removed. `TransportLimits::max_streams`
/// caps the requests a connection may have in flight, so the ceiling is
/// `max_body_bytes × h2_max_concurrent_streams × max_connections` and every
/// term is configurable; a declared oversize is refused in `serve_request`
/// before a byte is buffered.
///
/// Converting this to a stream would level the two transports down to the
/// weaker guarantee. If the asymmetry is worth closing, close it the other way.
async fn read_body<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    max_body: usize,
) -> Result<Bytes, BodyRefused>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut buf = bytes::BytesMut::new();
    // `recv_data` has three outcomes and they have to stay three. This was a
    // `while let Ok(Some(..))`, which is only two: a chunk, or the loop ends.
    // The clean end of body and a stream that failed both fell into "the loop
    // ends", so a body cut short — a peer that announced 5000 bytes, sent
    // 1200, then RESET_STREAM, which surfaces here as
    // `StreamError::RemoteTerminate` — was returned as `Ok` and dispatched as
    // if the whole request had arrived. The handler ran, committed whatever
    // side effect it has on a fragment of the payload, and answered 200. That
    // is the failure mode the caller cannot detect afterwards, because a
    // truncated body is a well-formed shorter body.
    loop {
        match stream.recv_data().await {
            Ok(Some(mut chunk)) => {
                let piece = chunk.copy_to_bytes(chunk.remaining());
                if buf.len() + piece.len() > max_body {
                    return Err(BodyRefused::TooLarge);
                }
                buf.extend_from_slice(&piece);
            }
            // The peer finished its side of the stream: the body is whole.
            Ok(None) => return Ok(buf.freeze()),
            Err(e) => return Err(BodyRefused::Incomplete(e)),
        }
    }
}

/// HTTP/3 always carries an absolute-form target, TCP requests usually do not.
///
/// The router matches on the path, and `Call::path` reads it off the URI, so
/// both forms already work. What differs is what a handler sees when it prints
/// the URI, and an absolute form there would be a gratuitous difference between
/// transports. `CONNECT` and `OPTIONS *` are left alone: their target is not a
/// path and rewriting it would destroy the request.
fn normalise_uri(uri: &Uri, method: &Method) -> Uri {
    if method == Method::CONNECT || uri.path() == "*" {
        return uri.clone();
    }
    let Some(path_and_query) = uri.path_and_query() else {
        return uri.clone();
    };
    path_and_query
        .as_str()
        .parse()
        .unwrap_or_else(|_| uri.clone())
}

/// Write a [`Response`](crate::Response) out over an h3 stream.
async fn send_response<S>(
    app: &App,
    stream: &mut h3::server::RequestStream<S, Bytes>,
    response: crate::Response,
    head_only: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut builder = http::Response::builder().status(response.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = sanitise(response.headers);
        // `true`, which only this module is in a position to assert.
        // QUIC has no plaintext mode — `server_config_from_pem` pins TLS 1.3,
        // and a connection that negotiated anything but `h3` never gets here —
        // so a response written to this stream is encrypted whatever
        // `AppBuilder::tls` was told. The pipeline cannot know that: it runs
        // the same code on every transport and reads a builder field that
        // `http3::serve` has no way to set, since it is handed the certificate
        // as an argument and the `App` is already built. The result was that
        // the one transport where TLS is mandatory was the one that never sent
        // HSTS.
        //
        // Widening the gate, not bypassing what is behind it: `hsts(None)` and
        // `without_security_headers` are still obeyed, and a handler that set
        // the header keeps its own value.
        //
        // Most of what this adds is already there — the pipeline's own
        // middleware ran for anything routed — but a response the pipeline
        // never produced (the `408` from an expired deadline just above)
        // arrives with nothing, and that is the second reason this is here
        // rather than in `serve_request`'s happy path.
        app.apply_security_headers(headers, true);
    }
    stream.send_response(builder.body(())?).await?;

    if !head_only {
        match response.body {
            Body::Bytes(bytes) => {
                if !bytes.is_empty() {
                    stream.send_data(bytes).await?;
                }
            }
            Body::Stream(mut chunks) => {
                use futures_util::StreamExt;
                // Forwarded chunk by chunk, so a streamed response stays
                // streamed over h3 instead of being collected to send it.
                while let Some(chunk) = chunks.next().await {
                    match chunk {
                        Ok(bytes) => stream.send_data(bytes).await?,
                        // The body failed partway. There is no status left to
                        // change, so the honest signal is an incomplete
                        // stream: reset it rather than finishing cleanly and
                        // claiming the truncated body was the whole thing.
                        //
                        // Returning without resetting did the opposite of what
                        // this comment promised. It skips the `finish` below,
                        // but the `RequestStream` is owned by this task and is
                        // dropped on the way out — and quinn's `SendStream`
                        // finishes the stream when it drops. The peer got a
                        // clean FIN and a truncated body it could not tell
                        // apart from the whole one: three records of a hundred,
                        // stored as the complete answer. Over HTTP/1.1 and
                        // HTTP/2 the same handler surfaces the error to hyper,
                        // which aborts.
                        //
                        // Resetting first is what wins the race: after
                        // `stop_stream` the drop-time finish fails with
                        // `ClosedStream`, which quinn ignores, so RESET_STREAM
                        // is what reaches the peer.
                        Err(e) => {
                            tracing::debug!(error = %e, "http3 response body failed mid-stream");
                            stream.stop_stream(h3::error::Code::H3_INTERNAL_ERROR);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    stream.finish().await?;
    Ok(())
}

/// Drop headers HTTP/3 forbids.
///
/// RFC 9114 §4.2 bans connection-specific fields, and a `Connection:
/// keep-alive` copied from a handler written for HTTP/1.1 is enough for a
/// conforming client to treat the whole response as malformed.
fn sanitise(mut headers: HeaderMap) -> HeaderMap {
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_target_becomes_origin_form() {
        let uri: Uri = "https://example.com/a/b?x=1".parse().unwrap();
        assert_eq!(
            normalise_uri(&uri, &Method::GET).to_string(),
            "/a/b?x=1",
            "a handler should see the same target on every transport"
        );
    }

    #[test]
    fn an_origin_form_target_is_left_alone() {
        let uri: Uri = "/a/b".parse().unwrap();
        assert_eq!(normalise_uri(&uri, &Method::GET).to_string(), "/a/b");
    }

    #[test]
    fn a_connect_target_is_left_alone() {
        let uri: Uri = "example.com:443".parse().unwrap();
        assert_eq!(
            normalise_uri(&uri, &Method::CONNECT).to_string(),
            "example.com:443"
        );
    }

    #[test]
    fn connection_specific_headers_are_dropped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("content-type", "text/plain".parse().unwrap());

        let clean = sanitise(headers);
        assert!(clean.get("connection").is_none());
        assert!(clean.get("transfer-encoding").is_none());
        assert_eq!(clean.get("content-type").unwrap(), "text/plain");
    }

    #[test]
    fn a_missing_certificate_is_reported_rather_than_panicking() {
        match server_config_from_pem("/nonexistent/cert.pem", "/nonexistent/key.pem") {
            Ok(_) => panic!("expected an error for missing files"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
        }
    }

    #[test]
    fn a_configured_keep_alive_becomes_the_quic_idle_bound() {
        assert_eq!(idle_ms_for(5_000), 5_000);
    }

    #[test]
    fn a_zero_keep_alive_keeps_the_default_bound() {
        // `0` asks TCP to answer and close. QUIC cannot do that, and the
        // alternative reading — never expire — would let one idle connection
        // hold a `max_connections` slot for good, which is the opposite of what
        // the setting asks for.
        assert_eq!(idle_ms_for(0), DEFAULT_IDLE_MS);
        assert_ne!(idle_ms_for(0), 0);
    }

    #[test]
    fn the_default_bound_agrees_with_the_default_keep_alive() {
        // The two constants live in different files; this is what would notice
        // if one moved without the other.
        assert_eq!(
            DEFAULT_IDLE_MS,
            crate::app::ServerConfig::default().keep_alive_ms
        );
    }

    #[test]
    fn a_configured_stream_limit_becomes_the_quic_bidi_cap() {
        assert_eq!(max_streams_for(100), quinn::VarInt::from_u32(100));
    }

    #[test]
    fn a_zero_stream_limit_means_unlimited_not_zero() {
        // `0` removes the limit on HTTP/2. Passing it straight through would
        // forbid every request stream instead, which is the opposite.
        assert_eq!(max_streams_for(0), quinn::VarInt::MAX);
        assert_ne!(max_streams_for(0), quinn::VarInt::from_u32(0));
    }

    #[test]
    fn the_transport_defaults_come_from_the_app_defaults() {
        // What would notice a knob added to `ServerConfig` and wired into the
        // TCP engine but never into `TransportLimits`.
        let cfg = crate::app::ServerConfig::default();
        let limits = TransportLimits::defaults();
        assert_eq!(limits.idle_ms, idle_ms_for(cfg.keep_alive_ms));
        assert_eq!(
            limits.max_streams,
            max_streams_for(cfg.h2_max_concurrent_streams)
        );
    }

    #[test]
    fn an_ordinary_keep_alive_converts_to_an_idle_timeout() {
        assert!(idle_timeout(75_000).is_ok());
    }

    #[test]
    fn a_keep_alive_past_the_varint_range_is_refused_rather_than_panicking() {
        // Reachable now that the value is an operator's `u64` rather than a
        // constant, so it has to be an error and not an `expect`.
        match idle_timeout(u64::MAX) {
            Ok(_) => panic!("expected an error for an out-of-range idle timeout"),
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
                assert!(
                    e.to_string().contains("keep_alive_ms"),
                    "the error should name the setting at fault, got: {e}"
                );
            }
        }
    }
}
