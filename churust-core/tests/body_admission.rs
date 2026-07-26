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
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    // The sender must outlive this function: dropping it resolves `rx` and
    // shuts the server down before the test has connected.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    std::mem::forget(tx);
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    addr
}

/// Send a POST with a declared Content-Length and read whatever comes back.
///
/// Lower-cased, because a test that asserts on a header name is asserting on
/// something HTTP says is case-insensitive.
async fn raw_post(addr: std::net::SocketAddr, path: &str, len: usize) -> String {
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {len}\r\n\r\n");
    sock.write_all(head.as_bytes()).await.unwrap();
    // The server may already have answered and closed, in which case the peer
    // is gone and writing the body fails. That is the refusal working, not a
    // test failure, so it is not unwrapped.
    let _ = sock.write_all(&vec![b'x'; len]).await;
    let mut buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .expect("no response")
        .expect("read failed");
    String::from_utf8_lossy(&buf[..n]).to_lowercase()
}

/// Send a POST with a declared Content-Length and read the status line.
async fn post(addr: std::net::SocketAddr, path: &str, len: usize) -> String {
    raw_post(addr, path, len)
        .await
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

#[tokio::test]
async fn a_refusal_before_dispatch_still_carries_the_security_headers() {
    // Refusing before dispatch is deliberate — see `engine::respond` — but it
    // also took the refusal out of reach of the pipeline, and the security
    // headers are a pipeline middleware. So the one response an operator is
    // most likely to see from an unfamiliar client was the one response that
    // arrived bare, while `security.rs` promised them on every response and
    // `error_responses_are_also_protected` pinned the property for a `404`.
    let addr = serve_app(app()).await;
    let res = raw_post(addr, "/ignore", 4096).await;

    assert!(res.contains("413"), "{res}");
    assert!(
        res.contains("x-content-type-options: nosniff"),
        "the pre-dispatch refusal shipped without the default headers: {res}"
    );
    assert!(res.contains("x-frame-options: deny"), "{res}");
    assert!(res.contains("referrer-policy: no-referrer"), "{res}");
}

#[tokio::test]
async fn a_refusal_before_dispatch_says_what_its_body_is() {
    // The body is the literal `Payload Too Large` and the response carried no
    // `Content-Type` at all, so what a client did with those bytes was down to
    // its own sniffing. Every other response this server writes declares its
    // type; this one now does too.
    let addr = serve_app(app()).await;
    let res = raw_post(addr, "/ignore", 4096).await;

    assert!(
        res.contains("content-type: text/plain; charset=utf-8"),
        "the refusal did not declare its body's type: {res}"
    );
}

#[tokio::test]
async fn a_refusal_before_dispatch_honours_a_server_that_wants_no_headers() {
    // `without_security_headers` is an opt-out for a deployment that already
    // has something in front adding them. Hardcoding the headers at the
    // refusal site would have quietly ignored it.
    let app = Churust::server()
        .max_body_bytes(1024)
        .without_security_headers()
        .routing(|r| {
            r.post("/ignore", |_c: Call| async { "ignored" });
        })
        .build();
    let addr = serve_app(app).await;
    let res = raw_post(addr, "/ignore", 4096).await;

    assert!(res.contains("413"), "{res}");
    assert!(
        !res.contains("x-content-type-options"),
        "the opt-out was ignored on the refusal path: {res}"
    );
}
