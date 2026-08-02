//! The floor: hyper with no framework on top of it.
//!
//! Every other app here is a framework, so a gap between two of them is easy to
//! attribute to the wrong thing. This one answers the question the others
//! cannot — how much of a request's cost is the HTTP implementation itself,
//! before anyone adds routing, extraction or a pipeline. Churust's own overhead
//! is then the difference between it and this, and that difference is the only
//! part of the gap Churust can do anything about.
//!
//! Deliberately built the way Churust builds it: the same `auto::Builder`, the
//! same one-runtime-per-core with SO_REUSEPORT, the same responses byte for
//! byte. It is not a serious server — it has no timeouts, no connection cap, no
//! graceful shutdown, no TLS — and that is the point. Whatever it costs is a
//! lower bound nothing built on hyper can beat.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{body::Incoming, header, Request, Response, StatusCode};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::convert::Infallible;
use std::net::SocketAddr;

async fn route(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path();
    let (ctype, body): (&'static str, Bytes) = if path == "/plaintext" {
        ("text/plain; charset=utf-8", Bytes::from_static(b"Hello, World!"))
    } else if path == "/json" {
        (
            "application/json",
            Bytes::from_static(br#"{"message":"Hello, World!"}"#),
        )
    } else if let Some(id) = path.strip_prefix("/user/") {
        match id.parse::<u64>() {
            Ok(id) => (
                "text/plain; charset=utf-8",
                Bytes::from(format!("user {id}")),
            ),
            Err(_) => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(Full::new(Bytes::from_static(b"Bad Request")))
                    .expect("infallible"));
            }
        }
    } else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from_static(b"Not Found")))
            .expect("infallible"));
    };

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, header::HeaderValue::from_static(ctype))
        .body(Full::new(body))
        .expect("infallible"))
}

fn listener(addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
    // Matches Churust's sharded engine: one SO_REUSEPORT listener per worker,
    // so the kernel distributes and no thread does.
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
    sock.set_reuse_address(true)?;
    sock.set_reuse_port(true)?;
    sock.bind(&addr.into())?;
    sock.listen(1024)?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

fn main() {
    let port: u16 = std::env::var("PORT").expect("PORT").parse().expect("a port");
    let workers: usize = std::env::var("WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("an address");
    // Read once, here, rather than per connection. Reading it inside the accept
    // loop is a `getenv` and a `String` allocation on every connection, which
    // is real work this app exists specifically not to do — a floor that pays a
    // cost the framework above it does not is not a floor.
    //
    // Off by default because that is `ServerConfig`'s default, and the floor
    // should differ from Churust in what it omits, never in how it is tuned.
    let pipeline_flush = std::env::var("PIPELINE_FLUSH").as_deref() == Ok("1");

    let mut handles = Vec::new();
    for _ in 0..workers {
        let std_listener = listener(addr).expect("bind");
        handles.push(std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            rt.block_on(async move {
                let l = tokio::net::TcpListener::from_std(std_listener).expect("adopt");
                loop {
                    let Ok((stream, _)) = l.accept().await else {
                        continue;
                    };
                    let _ = stream.set_nodelay(true);
                    tokio::spawn(async move {
                        let mut b =
                            hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                        b.http1().pipeline_flush(pipeline_flush);
                        let _ = b
                            .serve_connection(TokioIo::new(stream), service_fn(route))
                            .await;
                    });
                }
            });
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}
