//! An oversized body must be refused whether or not a handler reads it.
//!
//! Moving to streaming bodies replaced a pre-dispatch collect-and-check with a
//! lazy `Limited` stream. The cap then only tripped if something actually read
//! the body: a handler that ignores it, or middleware that short-circuits
//! before an extractor runs, answered `200` for a request the server had
//! declared it would refuse. `config.rs` promises "larger bodies are rejected
//! with 413 Payload Too Large" without qualification.

use churust_core::{Call, Churust};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn serve_app(app: churust_core::App) -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    // The sender must outlive this function: dropping it resolves `rx` and
    // shuts the server down before the test has connected.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    std::mem::forget(tx);
    tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    addr
}

/// Send a POST with a declared Content-Length and read the status line.
async fn post(addr: std::net::SocketAddr, path: &str, len: usize) -> String {
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {len}\r\n\r\n");
    sock.write_all(head.as_bytes()).await.unwrap();
    sock.write_all(&vec![b'x'; len]).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .expect("no response")
        .expect("read failed");
    String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn app() -> churust_core::App {
    Churust::server()
        .max_body_bytes(1024)
        .routing(|r| {
            // Never touches the body.
            r.post("/ignore", |_c: Call| async { "ignored" });
            // Reads it, so the stream limit would have caught this one anyway.
            r.post(
                "/read",
                |body: String| async move { format!("{}", body.len()) },
            );
        })
        .build()
}

#[tokio::test]
async fn an_oversized_body_is_refused_even_when_the_handler_ignores_it() {
    let addr = serve_app(app()).await;
    let status = post(addr, "/ignore", 4096).await;
    assert!(
        status.contains("413"),
        "a handler that ignores the body answered: {status}"
    );
}

#[tokio::test]
async fn an_oversized_body_is_refused_when_the_handler_reads_it() {
    let addr = serve_app(app()).await;
    let status = post(addr, "/read", 4096).await;
    assert!(status.contains("413"), "{status}");
}

#[tokio::test]
async fn a_body_within_the_cap_is_served() {
    let addr = serve_app(app()).await;
    assert!(post(addr, "/ignore", 512).await.contains("200"));
    assert!(post(addr, "/read", 512).await.contains("200"));
}

#[tokio::test]
async fn a_body_exactly_at_the_cap_is_served() {
    let addr = serve_app(app()).await;
    assert!(post(addr, "/read", 1024).await.contains("200"));
}

#[tokio::test]
async fn an_empty_body_is_unaffected() {
    let addr = serve_app(app()).await;
    assert!(post(addr, "/ignore", 0).await.contains("200"));
}
