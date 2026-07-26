//! The connection's peer address.

use churust_core::{Call, Churust, TestClient};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn peer_address_is_available_over_a_real_socket() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/who", |c: Call| async move {
                match c.peer_addr() {
                    Some(p) => format!("peer {}", p.ip()),
                    None => "none".to_string(),
                }
            });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_on(listener, async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(120)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("GET /who HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    assert!(
        text.ends_with("peer 127.0.0.1"),
        "the handler should see the client address, got: {text}"
    );

    let _ = tx.send(());
    let _ = server.await;
}

#[tokio::test]
async fn there_is_no_peer_address_without_a_socket() {
    // TestClient drives the pipeline directly; reporting a fabricated address
    // would be worse than reporting none.
    let app = Churust::server()
        .routing(|r| {
            r.get(
                "/who",
                |c: Call| async move { format!("{:?}", c.peer_addr()) },
            );
        })
        .build();

    assert_eq!(TestClient::new(app).get("/who").send().await.text(), "None");
}
