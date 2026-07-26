//! HTTP/2 support.
//!
//! `TestClient` never constructs a connection, so nothing here can be proven
//! through it — every test drives a real socket.

use churust_core::{Call, Churust};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Start a server on an ephemeral port and return its address plus a shutdown
/// handle.
async fn serve() -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/", |_c: Call| async { "hello" });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        app.start_on(listener, async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    (addr, tx, handle)
}

#[tokio::test]
async fn http1_still_works() {
    let (addr, tx, handle) = serve().await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    assert!(text.ends_with("hello"), "got: {text}");

    let _ = tx.send(());
    let _ = handle.await;
}

/// h2c with prior knowledge: the client opens with the HTTP/2 connection
/// preface instead of negotiating. A server that only speaks HTTP/1 answers
/// this with a 400 or closes; one that speaks h2 replies with SETTINGS.
#[tokio::test]
async fn h2c_prior_knowledge_is_accepted() {
    let (addr, tx, handle) = serve().await;

    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    // An empty SETTINGS frame: length 0, type 0x4, flags 0, stream 0.
    const SETTINGS: &[u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(PREFACE).await.unwrap();
    sock.write_all(SETTINGS).await.unwrap();
    sock.flush().await.unwrap();

    // The server must answer with its own SETTINGS frame.
    let mut head = [0u8; 9];
    let read = tokio::time::timeout(Duration::from_secs(2), sock.read_exact(&mut head)).await;

    assert!(
        read.is_ok(),
        "timed out waiting for a frame — the server did not speak HTTP/2"
    );
    read.unwrap()
        .expect("connection closed instead of replying");

    let frame_type = head[3];
    assert_eq!(
        frame_type, 0x4,
        "expected a SETTINGS frame (0x4) as the first reply, got type {frame_type:#x}"
    );

    let _ = tx.send(());
    let _ = handle.await;
}

/// An HTTP/1 request that is not an h2 preface must not be misread as one.
#[tokio::test]
async fn a_plain_http1_post_is_not_mistaken_for_h2() {
    let (addr, tx, handle) = serve().await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("POST /nope HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    assert!(text.starts_with("HTTP/1.1 404"), "got: {text}");

    let _ = tx.send(());
    let _ = handle.await;
}
