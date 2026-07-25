//! # Chat — WebSockets, echo and broadcast
//!
//! Shows the `ws` feature: the `WebSocketUpgrade` extractor, `on_upgrade`, and
//! a broadcast room built on a channel held in application state.
//!
//! ## Run it
//!
//! ```text
//! cargo run -p chat
//! ```
//!
//! ## Try it
//!
//! Needs a WebSocket client; `websocat` is the easiest.
//!
//! ```text
//! websocat ws://localhost:8080/echo
//! # type a line, it comes straight back
//!
//! # In two terminals, to see the fan-out:
//! websocat ws://localhost:8080/room
//! websocat ws://localhost:8080/room
//! ```
//!
//! A plain `GET` to either route is rejected with `426 Upgrade Required`.
//!
//! ## In your own project
//!
//! ```toml
//! [dependencies]
//! churust = { version = "0.2", features = ["ws"] }
//! ```
//!
//! Note there is no `tokio` entry, even though this example uses
//! `broadcast` and `select!`. Churust re-exports the runtime, so
//! `churust::tokio` gives you the whole of tokio without a second dependency.

use churust::prelude::*;
use churust::tokio::sync::broadcast;
use churust::ws::{Message, WebSocketUpgrade};
use std::sync::Arc;

#[derive(Clone)]
struct Room {
    tx: Arc<broadcast::Sender<String>>,
}

#[churust::main]
async fn main() -> std::io::Result<()> {
    let (tx, _rx) = broadcast::channel::<String>(100);
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .state(Room { tx: Arc::new(tx) })
        .routing(|r| {
            // Plain echo.
            r.get("/echo", |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut sock| async move {
                    while let Some(Ok(msg)) = sock.recv().await {
                        if matches!(msg, Message::Close) {
                            break;
                        }
                        if sock.send(msg).await.is_err() {
                            break;
                        }
                    }
                })
            });

            // Broadcast room: every message is fanned out to all connected clients.
            r.get(
                "/room",
                |room: State<Room>, ws: WebSocketUpgrade| async move {
                    let tx = room.tx.clone();
                    let mut rx = tx.subscribe();
                    ws.on_upgrade(move |mut sock| async move {
                        loop {
                            churust::tokio::select! {
                                incoming = sock.recv() => match incoming {
                                    Some(Ok(Message::Text(t))) => { let _ = tx.send(t); }
                                    Some(Ok(Message::Close)) | None => break,
                                    Some(Err(_)) => break,
                                    _ => {}
                                },
                                outgoing = rx.recv() => {
                                    if let Ok(text) = outgoing {
                                        if sock.send_text(text).await.is_err() { break; }
                                    }
                                }
                            }
                        }
                    })
                },
            );
        })
        .start()
        .await
}
