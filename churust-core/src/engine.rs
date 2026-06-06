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
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, Limited, StreamBody};
use hyper::body::Frame;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// Convert a [`Body`] into the boxed hyper body the engine writes to the wire:
/// `Full` for a buffered payload, `StreamBody` for a lazy stream.
///
/// The boxed body is the `!Sync` [`UnsyncBoxBody`] rather than the plan's
/// `BoxBody`: `Body::Stream` wraps a `Pin<Box<dyn Stream + Send>>` (intentionally
/// `Send` but not `Sync`), and the resolved `http-body-util` 0.1.3 `BodyExt::boxed`
/// requires `Self: Send + Sync`. `boxed_unsync` only requires `Send`, and hyper
/// accepts any `http_body::Body` response body, so behavior is unchanged.
fn into_boxed_body(body: Body) -> UnsyncBoxBody<Bytes, std::io::Error> {
    match body {
        Body::Bytes(bytes) => Full::new(bytes)
            .map_err(|never| match never {})
            .boxed_unsync(),
        Body::Stream(stream) => {
            let frames = stream.map(|chunk| {
                chunk
                    .map(Frame::data)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            });
            StreamBody::new(frames).boxed_unsync()
        }
    }
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
    let listener = TcpListener::bind(addr).await?;
    let max_body = app.config().max_body_bytes;
    let timeout_ms = app.config().request_timeout_ms;
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    tokio::pin!(shutdown);

    #[cfg(feature = "tls")]
    let tls_acceptor = match &app.config().tls {
        Some(t) => Some(acceptor_from_pem(&t.cert, &t.key)?),
        None => None,
    };

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                #[cfg(feature = "tls")]
                {
                    if let Some(acceptor) = tls_acceptor.clone() {
                        let app = app.clone();
                        // `Watcher` is the owned counterpart of `GracefulShutdown`
                        // (`watcher()` exists precisely so it can be moved onto
                        // another task before `watch()`). We hand it to the
                        // spawned handshake task so the TLS connection is still
                        // drained on graceful shutdown — without blocking the
                        // accept loop on the (potentially slow) TLS handshake.
                        let watcher = graceful.watcher();
                        let conn_builder_fut = async move {
                            // A failed TLS handshake silently drops the connection.
                            if let Ok(tls_stream) = acceptor.accept(stream).await {
                                serve_stream(app, TokioIo::new(tls_stream), max_body, timeout_ms, watcher).await;
                            }
                        };
                        tokio::spawn(conn_builder_fut);
                        continue;
                    }
                }
                serve_stream(app.clone(), TokioIo::new(stream), max_body, timeout_ms, graceful.watcher()).await;
            }
            _ = &mut shutdown => {
                break;
            }
        }
    }

    graceful.shutdown().await;
    Ok(())
}

async fn serve_stream<I>(
    app: App,
    io: I,
    max_body: usize,
    timeout_ms: u64,
    watcher: hyper_util::server::graceful::Watcher,
) where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let svc = service_fn(move |req: HyperRequest<Incoming>| {
        let app = app.clone();
        async move { handle(app, req, max_body, timeout_ms).await }
    });
    let conn = hyper::server::conn::http1::Builder::new().serve_connection(io, svc);
    // Under the `ws` feature the connection is made upgradeable. The
    // resolved `hyper-util` (0.1.x) does not implement `GracefulConnection`
    // for `http1::UpgradeableConnection`, so `watcher.watch(conn)` rejects it.
    // Per the plan's implementer note we fall back to awaiting the connection
    // directly for the ws build (dropping graceful drain only for WS
    // connections) while keeping the non-ws path exactly as-is.
    #[cfg(feature = "ws")]
    {
        let _ = &watcher;
        let conn = conn.with_upgrades();
        tokio::spawn(async move {
            let _ = conn.await;
        });
    }
    #[cfg(not(feature = "ws"))]
    {
        let fut = watcher.watch(conn);
        tokio::spawn(async move {
            let _ = fut.await;
        });
    }
}

async fn handle(
    app: App,
    req: HyperRequest<Incoming>,
    max_body: usize,
    timeout_ms: u64,
) -> Result<HyperResponse<UnsyncBoxBody<Bytes, std::io::Error>>, Infallible> {
    #[cfg(feature = "ws")]
    let mut req = req;
    #[cfg(feature = "ws")]
    let on_upgrade = if crate::ws::is_upgrade_request(req.headers()) {
        Some(hyper::upgrade::on(&mut req))
    } else {
        None
    };

    let (parts, body) = req.into_parts();

    // Enforce max body size before buffering.
    let collected = Limited::new(body, max_body).collect().await;
    let body_bytes = match collected {
        Ok(buf) => buf.to_bytes(),
        Err(_) => {
            let mut resp = HyperResponse::new(
                Full::new(Bytes::from("Payload Too Large"))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            );
            *resp.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
            return Ok(resp);
        }
    };

    #[cfg(feature = "ws")]
    let process = {
        let mut extensions = http::Extensions::new();
        if let Some(on_upgrade) = on_upgrade {
            extensions.insert(crate::ws::OnUpgradeHandle::new(on_upgrade));
        }
        app.process_with_extensions(
            parts.method,
            parts.uri,
            parts.headers,
            body_bytes,
            extensions,
        )
    };
    #[cfg(not(feature = "ws"))]
    let process = app.process(parts.method, parts.uri, parts.headers, body_bytes);

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
        .body(into_boxed_body(res.body))
        .expect("response build is infallible"))
}
