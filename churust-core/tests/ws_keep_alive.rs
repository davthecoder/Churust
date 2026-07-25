#![cfg(feature = "ws")]
//! An upgraded WebSocket must outlive the HTTP idle keep-alive timeout.
//!
//! The idle watchdog measures activity at the *service* boundary, and an
//! upgraded connection never re-enters the service — so its activity clock
//! stops at the upgrade. This asserts the watchdog cannot truncate a quiet but
//! live socket, which is the failure mode that design invites.
use churust_core::ws::{Message, WebSocketUpgrade};
use churust_core::Churust;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;

#[tokio::test]
async fn a_quiet_websocket_outlives_the_keep_alive_period() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);

    let app = Churust::server()
        .keep_alive_ms(300) // deliberately tiny
        .routing(|r| {
            r.get("/echo", |ws: WebSocketUpgrade| async move {
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
        })
        .build();

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/echo"))
        .await
        .unwrap();
    // Stay quiet for 4x the keep-alive period, then try to use the socket.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        "still here".into(),
    ))
    .await
    .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
    eprintln!("after 1200ms quiet on a 300ms keep-alive: {got:?}");
    let msg = got
        .expect("timed out")
        .expect("stream ended")
        .expect("ws error");
    assert!(
        msg.to_text().unwrap().contains("still here"),
        "socket was closed by the idle watchdog"
    );
}
