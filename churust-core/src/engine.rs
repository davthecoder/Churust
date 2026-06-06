//! The hyper-based HTTP/1.1 serving engine that drives an [`App`] over a real
//! socket.
//!
//! Most applications never call into this module directly — [`App::start`] and
//! [`App::start_with_shutdown`] bind a listener and delegate to [`serve`]. It is
//! public so advanced callers can drive serving with a custom address and
//! shutdown signal.

use crate::app::App;
#[cfg(feature = "tls")]
use crate::tls::acceptor_from_pem;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use tokio::net::TcpListener;

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
    let fut = watcher.watch(conn);
    tokio::spawn(async move {
        let _ = fut.await;
    });
}

async fn handle(
    app: App,
    req: HyperRequest<Incoming>,
    max_body: usize,
    timeout_ms: u64,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();

    // Enforce max body size before buffering.
    let collected = Limited::new(body, max_body).collect().await;
    let body_bytes = match collected {
        Ok(buf) => buf.to_bytes(),
        Err(_) => {
            let mut resp = HyperResponse::new(Full::new(Bytes::from("Payload Too Large")));
            *resp.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
            return Ok(resp);
        }
    };

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
        .body(Full::new(res.body))
        .expect("response build is infallible for buffered body"))
}
