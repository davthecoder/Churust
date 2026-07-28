# WebSockets

<span class="feature-tag">feature: ws</span>

## Enable

```toml
churust = { version = "0.3", features = ["ws"] }
```

## Upgrade

A handler takes `WebSocketUpgrade` and calls `on_upgrade`. A plain GET to the
route is rejected with **`426 Upgrade Required`**.

```rust
use churust::prelude::*;
use churust::ws::{Message, WebSocketUpgrade};

r.get("/echo", |ws: WebSocketUpgrade| async move {
    ws.on_upgrade(|mut sock| async move {
        while let Some(Ok(msg)) = sock.recv().await {
            if matches!(msg, Message::Close) || sock.send(msg).await.is_err() {
                break;
            }
        }
    })
});
```

## Broadcast room

Hold a `broadcast` channel in app state and `select!` between socket reads and
channel receives. See the full example:

- Source: [`examples/chat`](https://github.com/davthecoder/Churust/tree/main/examples/chat)
- Recipe: [Chat room](../recipes/chat-room.md)

```bash
cargo run -p chat
websocat ws://localhost:8080/echo
websocat ws://localhost:8080/room
```

Use `churust::tokio` for `broadcast` and `select!` — no separate tokio
dependency required.

## Limits

WebSocket frame and message caps are part of the secure defaults. WebSockets
over HTTP/3 (Extended CONNECT) are **not** supported yet — see
[What is not supported](../reference/non-goals.md).

## API reference

- [docs.rs/churust](https://docs.rs/churust) (`ws` module when feature is enabled)
