#![cfg(feature = "ws")]

use churust_core::ws::{Message, WebSocketUpgrade};
use churust_core::Churust;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;

#[tokio::test]
async fn websocket_echo_round_trip() {
    // Pick a free port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/echo", |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut sock| async move {
                    while let Some(Ok(msg)) = sock.recv().await {
                        match msg {
                            Message::Close => break,
                            other => {
                                if sock.send(other).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                })
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
    tokio::time::sleep(Duration::from_millis(150)).await;

    use tokio_tungstenite::tungstenite::Message as TM;
    let url = format!("ws://{addr}/echo");
    let (mut client, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connect");

    // Text echo.
    client.send(TM::Text("hello".into())).await.unwrap();
    let got = client.next().await.unwrap().unwrap();
    assert_eq!(got.to_text().unwrap(), "hello");

    // Binary echo.
    client.send(TM::Binary(vec![1, 2, 3].into())).await.unwrap();
    let got = client.next().await.unwrap().unwrap();
    assert_eq!(got.into_data().to_vec(), vec![1, 2, 3]);

    // Clean close.
    client.close(None).await.unwrap();

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}
