//! Graceful shutdown must drain in-flight requests, and must not drain forever.
//!
//! The distinction these tests draw is between `serve()` *returning* and
//! `serve()` *waiting*. Returning is easy and was already covered; waiting is
//! the actual guarantee, and asserting only the former let a no-op drain ship.

use churust_core::{Call, Churust};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bind an ephemeral port and release it, so the engine can claim the address.
fn free_addr() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

/// A server whose `/slow` handler takes `handler_ms` to answer.
fn slow_app(handler_ms: u64, shutdown_timeout_ms: u64) -> churust_core::App {
    Churust::server()
        .shutdown_timeout_ms(shutdown_timeout_ms)
        .routing(move |r| {
            r.get("/slow", move |_c: Call| async move {
                tokio::time::sleep(Duration::from_millis(handler_ms)).await;
                "done"
            });
        })
        .build()
}

#[tokio::test]
async fn an_in_flight_request_completes_before_serve_returns() {
    let addr = free_addr();
    // Generous grace: the point is that the drain waits, not that it gives up.
    let app = slow_app(800, 5_000);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();

    // Shut down while the handler is still sleeping.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let t0 = Instant::now();
    let _ = tx.send(());
    server.await.unwrap().unwrap();
    let waited = t0.elapsed();

    // The response must survive the shutdown...
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), sock.read_to_end(&mut buf))
        .await
        .expect("client read timed out")
        .expect("client read failed");
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("done"), "in-flight response was lost: {text}");

    // ...and `serve()` must have waited for it. Without this assertion the
    // test passes against a drain that does nothing: the spawned connection
    // task outlives `serve()` inside the test process, so the client still
    // reads a response. A real binary exits and the response is lost.
    assert!(
        waited >= Duration::from_millis(500),
        "serve() returned after {waited:?} — it did not wait for the in-flight request"
    );
}

#[tokio::test]
async fn an_idle_connection_does_not_hold_shutdown_for_the_grace_period() {
    // A connection with nothing in flight has nothing to drain. It will not
    // necessarily close itself, though: an idle HTTP/2 connection is wound down
    // by sending GOAWAY and waiting for the *peer* to close, which an idle peer
    // never does. Without a bound on that wait, every shutdown costs the full
    // grace period and a rolling restart pays it on every instance.
    let addr = free_addr();
    let app = slow_app(10, 30_000);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Establish an HTTP/2 connection by prior knowledge and then say nothing.
    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    const SETTINGS: &[u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(PREFACE).await.unwrap();
    sock.write_all(SETTINGS).await.unwrap();
    let mut head = [0u8; 9];
    tokio::time::timeout(Duration::from_secs(2), sock.read_exact(&mut head))
        .await
        .expect("no SETTINGS reply — not an h2 connection")
        .expect("connection closed");

    let t0 = Instant::now();
    let _ = tx.send(());
    server.await.unwrap().unwrap();
    let waited = t0.elapsed();

    assert!(
        waited < Duration::from_secs(5),
        "an idle h2 connection held shutdown for {waited:?} of a 30s grace period"
    );
    // Keep the socket alive to the end, so the server cannot have been waiting
    // on the client to close.
    drop(sock);
}

#[tokio::test]
async fn the_drain_gives_up_after_the_configured_grace_period() {
    let addr = free_addr();
    // A handler far slower than the grace period: the drain must abandon it.
    let app = slow_app(5_000, 200);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let t0 = Instant::now();
    let _ = tx.send(());
    server.await.unwrap().unwrap();
    let waited = t0.elapsed();

    // Bounded below: it did wait for the grace period rather than exiting flat.
    assert!(
        waited >= Duration::from_millis(150),
        "serve() returned after {waited:?} — shutdown_timeout_ms was ignored"
    );
    // Bounded above: exiting on time is the whole point. Under an orchestrator
    // an unbounded drain means being killed rather than shutting down.
    assert!(
        waited < Duration::from_millis(1_500),
        "serve() waited {waited:?} for a 200ms grace period"
    );
}
