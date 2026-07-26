//! Wire-level framing of error replies once `ContentNegotiation` is installed.
//!
//! `TestClient` drives `App::process` directly and never touches hyper, so it
//! cannot show what is actually written to the socket. A middleware that
//! rewrites a body has to keep the framing headers honest or hyper will be
//! asked to encode a length it was never given, which in a debug build panics
//! the encoder and drops the connection. This drives a real listener, following
//! the pattern in `churust-core/tests/head_options.rs`.

use churust_core::{Call, Churust, Error};
use churust_json::ContentNegotiation;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Send one request over its own connection and return the raw bytes hyper
/// wrote back, headers and all.
async fn exchange(addr: std::net::SocketAddr, request_line: &str) -> String {
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("{request_line} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    String::from_utf8_lossy(&raw).into_owned()
}

fn content_length(response: &str) -> Option<usize> {
    response
        .split("\r\n\r\n")
        .next()
        .unwrap_or("")
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
}

#[tokio::test]
async fn a_head_error_reply_is_framed_like_the_get_it_stands_in_for() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .install(ContentNegotiation::new())
        .routing(|r| {
            r.get("/boom", |_c: Call| async {
                Err::<&str, _>(Error::bad_request("nope"))
            });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_with_shutdown(async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let head = exchange(addr, "HEAD /boom").await;
    let get = exchange(addr, "GET /boom").await;

    println!("--- HEAD ---\n{head}\n--- GET ---\n{get}\n--- end ---");

    assert!(
        head.starts_with("HTTP/1.1 400"),
        "the HEAD reply never made it off the socket intact: {head:?}"
    );
    assert_eq!(
        head.split("\r\n\r\n").nth(1).unwrap_or(""),
        "",
        "a HEAD response must not carry a body: {head:?}"
    );

    let body = get.split("\r\n\r\n").nth(1).unwrap_or("");
    assert_eq!(body, r#"{"error":"nope","status":400}"#);
    assert_eq!(
        content_length(&get),
        Some(body.len()),
        "the rewritten JSON body must be framed with its own length: {get:?}"
    );
    // RFC 9110 §9.3.2 allows a HEAD to omit a field it can only learn by
    // generating the content, which is the case here: the plain-text message
    // was already dropped before this middleware could re-encode it. What it
    // may never do is quote the length of a representation GET will not send.
    if let Some(len) = content_length(&head) {
        assert_eq!(
            len,
            body.len(),
            "HEAD promised a size the GET does not deliver: {head:?}"
        );
    }

    let _ = tx.send(());
    let _ = server.await;
}
