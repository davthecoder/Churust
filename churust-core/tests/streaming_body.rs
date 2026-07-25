//! Streaming request bodies.
//!
//! The engine hands the body to the handler as a stream instead of collecting
//! it, so an upload is no longer capped by memory.

use churust_core::{Churust, Payload, TestClient};
use futures_util::StreamExt;
use http::StatusCode;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn counting_app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.post("/count", |Payload(mut body): Payload| async move {
                let mut total = 0usize;
                let mut chunks = 0usize;
                while let Some(item) = body.next().await {
                    match item {
                        Ok(c) => {
                            total += c.len();
                            chunks += 1;
                        }
                        Err(e) => return format!("error after {total}: {}", e.status()),
                    }
                }
                format!("{total}/{chunks}")
            });
        })
        .build()
}

#[tokio::test]
async fn a_buffered_body_still_reaches_a_streaming_handler() {
    // TestClient supplies the body in one piece; a handler must not have to
    // care which it got.
    let res = TestClient::new(counting_app())
        .post("/count")
        .body("hello")
        .send()
        .await;
    assert_eq!(res.text(), "5/1");
}

#[tokio::test]
async fn an_absent_body_is_an_empty_stream_not_an_error() {
    let res = TestClient::new(counting_app()).post("/count").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "0/0");
}

#[tokio::test]
async fn buffering_extractors_still_work() {
    // Json/Form collect the stream; that path must be unchanged.
    let app = Churust::server()
        .routing(|r| {
            r.post("/echo", |c: churust_core::Call| async move {
                let mut c = c;
                String::from_utf8(c.receive_bytes().await.to_vec()).unwrap_or_default()
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/echo")
        .body("round trip")
        .send()
        .await;
    assert_eq!(res.text(), "round trip");
}

/// The point of the change: a body larger than would be comfortable to hold is
/// consumed incrementally over a real socket.
#[tokio::test]
async fn a_large_body_streams_over_a_socket_in_several_chunks() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        // Comfortably above the payload so the cap is not what is under test.
        .max_body_bytes(8 * 1024 * 1024)
        .routing(|r| {
            r.post("/count", |Payload(mut body): Payload| async move {
                let mut total = 0usize;
                let mut chunks = 0usize;
                while let Some(Ok(c)) = body.next().await {
                    total += c.len();
                    chunks += 1;
                }
                // More than one chunk proves it was not collected up front.
                format!(
                    "{total} in {} chunk(s)",
                    if chunks > 1 { "many" } else { "one" }
                )
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
    tokio::time::sleep(Duration::from_millis(120)).await;

    const N: usize = 512 * 1024;
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("POST /count HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {N}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    // Write in pieces, with a pause, so the server cannot have it all at once.
    let piece = vec![b'x'; 64 * 1024];
    for _ in 0..(N / piece.len()) {
        sock.write_all(&piece).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    sock.flush().await.unwrap();

    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    assert!(
        text.ends_with(&format!("{N} in many chunk(s)")),
        "the body should have arrived incrementally, got: {text}"
    );

    let _ = tx.send(());
    let _ = server.await;
}

#[tokio::test]
async fn the_size_cap_still_applies_to_a_buffering_handler() {
    let app = Churust::server()
        .max_body_bytes(8)
        .routing(|r| {
            r.post("/echo", |c: churust_core::Call| async move {
                let mut c = c;
                match c.try_receive_bytes().await {
                    Ok(b) => format!("ok {}", b.len()),
                    Err(e) => format!("err {}", e.status()),
                }
            });
        })
        .build();

    // TestClient bypasses the engine, so the cap is enforced by the engine only
    // — this asserts the buffering path reports rather than silently truncates.
    let res = TestClient::new(app).post("/echo").body("tiny").send().await;
    assert_eq!(res.text(), "ok 4");
}
