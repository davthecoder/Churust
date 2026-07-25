//! `HEAD` and `OPTIONS` synthesis.
//!
//! RFC 9110 §9.3.2 requires `HEAD` to be available wherever `GET` is. Churust
//! previously answered `405`.

use churust_core::{App, Call, Churust, TestClient};
use http::{Method, StatusCode};

fn app() -> App {
    Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hello" });
            r.post("/submit", |_c: Call| async { "posted" });
            // An explicitly registered HEAD route must win over the fallback.
            r.method(Method::HEAD, "/explicit", |_c: Call| async {
                StatusCode::NO_CONTENT
            });
            r.get("/explicit", |_c: Call| async { "get body" });
        })
        .build()
}

#[tokio::test]
async fn head_falls_back_to_get() {
    let res = TestClient::new(app())
        .request(Method::HEAD, "/")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "", "a HEAD response must not carry a body");
}

#[tokio::test]
async fn explicit_head_route_wins_over_the_fallback() {
    let res = TestClient::new(app())
        .request(Method::HEAD, "/explicit")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn head_on_a_post_only_route_is_still_405() {
    let res = TestClient::new(app())
        .request(Method::HEAD, "/submit")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// `TestClient` drives `App::process` directly and never touches hyper, so it
/// cannot show what is actually written to the socket. This drives a real
/// listener, following the pattern in `engine_serve.rs`.
#[tokio::test]
async fn head_over_a_real_socket_sends_no_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/", |_c: Call| async { "hello" });
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

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("HEAD / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    println!("--- wire response ---\n{text}\n--- end ---");

    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");

    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    assert_eq!(body, "", "HEAD response carried a body: {body:?}");

    // RFC 9110 §9.3.2: the same header fields GET would have sent. "hello" is
    // five bytes, so a client sizing the resource with HEAD gets the truth.
    assert!(
        text.to_ascii_lowercase().contains("content-length: 5"),
        "HEAD should report the length GET would send, got: {text}"
    );

    let _ = tx.send(());
    let _ = server.await;
}

#[tokio::test]
async fn get_is_unaffected() {
    let res = TestClient::new(app()).get("/").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "hello");
}
