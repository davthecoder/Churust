#![cfg(feature = "ws")]
//! An open WebSocket must consume a connection permit.
//!
//! hyper resolves an upgraded connection the moment it dispatches the `101`,
//! while the socket lives on in a detached task. Without carrying the
//! connection's budget share into that task, a live WebSocket held no permit —
//! so `max_connections` bounded nothing for WebSocket traffic and an unbounded
//! number of sockets could be held open.

use churust_core::ws::{Message, WebSocketUpgrade};
use churust_core::{Call, Churust};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn an_open_websocket_holds_a_connection_permit() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .max_connections(1)
        .keep_alive_ms(60_000)
        .routing(|r| {
            r.get("/ws", |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut sock| async move {
                    while let Some(Ok(msg)) = sock.recv().await {
                        if let Message::Close = msg {
                            break;
                        }
                        if sock.send(msg).await.is_err() {
                            break;
                        }
                    }
                })
            });
            r.get("/plain", |_c: Call| async { "plain" });
        })
        .build();

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Open a WebSocket and keep it open. It must hold the only permit.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("handshake");
    ws.send(tokio_tungstenite::tungstenite::Message::Text("hi".into()))
        .await
        .unwrap();
    let echoed = ws.next().await.unwrap().unwrap();
    assert_eq!(echoed.to_text().unwrap(), "hi");

    // A second, ordinary connection must now wait rather than be served.
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET /plain HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 512];
    let served = tokio::time::timeout(Duration::from_millis(700), sock.read(&mut buf)).await;
    assert!(
        served.is_err(),
        "a second connection was served while an open WebSocket should hold the only permit"
    );

    // Closing the socket must return the permit, or the cap is a leak.
    drop(ws);
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .expect("the permit was never returned after the WebSocket closed")
        .unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).contains("200"));
}
