# Churust WebSockets — Design Spec (v2.0)

**Date:** 2026-06-06
**Status:** Approved (design), pending implementation plan
**Builds on:** Churust v1 (Plans 1–3, all green). WebSockets is the first v2 feature.

## 1. Summary

Add WebSocket support to Churust with a Ktor/axum-style ergonomic API: a
`WebSocketUpgrade` extractor that a handler uses to switch a normal HTTP request
into a bidirectional WebSocket connection. This requires real engine work
because Churust's request path is buffered (request → `Call` → buffered
`Response`); WebSockets need an HTTP **connection upgrade** (101 Switching
Protocols) and a raw bidirectional stream afterward.

## 2. Goals & non-goals

### Goals
- `WebSocketUpgrade` extractor that fits the existing `FromCallParts` model.
- `ws.on_upgrade(|socket| async { ... })` returning a 101 `Response`; the socket
  loop runs after the handshake completes.
- A clean `WebSocket` type (`recv`/`send`/`close` + `send_text`/`send_binary`)
  and a Churust `Message` enum that does not leak `tungstenite` into user code.
- Works transparently over TLS (the upgrade happens on whatever stream the
  engine accepted, plaintext or rustls).
- Real round-trip integration test using a `tokio-tungstenite` client.

### Non-goals (YAGNI for v2.0)
- per-message-deflate compression
- Full Autobahn conformance suite
- Subprotocol negotiation beyond echoing a single requested subprotocol
- Built-in pub/sub / rooms (the `examples/chat` shows broadcast in user code)

## 3. Decisions (locked)

| Decision | Choice |
|----------|--------|
| Placement | `ws` feature in **`churust-core`** (engine-coupled; avoids cross-crate plumbing of a non-`Clone` upgrade handle) |
| Protocol lib | `tokio-tungstenite` (brings `tungstenite`; provides `derive_accept_key`) |
| Handler API | `WebSocketUpgrade` extractor + `on_upgrade(callback) -> Response` (axum-style; no router/handler-trait changes) |
| Message type | Thin Churust `Message` enum with `From`/`Into<tungstenite::Message>` |
| Upgrade plumbing | Engine captures `hyper::upgrade::on(&mut req)` for upgrade requests into the per-`Call` extensions (Plan-3 map), wrapped in a `Clone` holder |

## 4. Architecture

### 4.1 Engine changes (`churust-core/src/engine.rs`)
- Connections are served with `.with_upgrades()` so hyper performs the upgrade
  after a 101 response. (Gated on `feature = "ws"`; the `GracefulShutdown`
  interaction is verified during implementation — fall back to watching the
  upgradeable connection or, if `GracefulConnection` is not implemented for it,
  serving WS connections without the graceful wrapper while preserving plaintext
  behavior.)
- In the per-request handler, **before** `req.into_parts()` and body buffering:
  if the request carries `Upgrade: websocket`, call
  `let on_upgrade = hyper::upgrade::on(&mut req);` and insert a
  `OnUpgradeHandle(Arc<Mutex<Option<hyper::upgrade::OnUpgrade>>>)` into the
  `Call` extensions. (WS requests have no body, so buffering yields empty.)
- `OnUpgradeHandle` is a small `Clone` newtype defined in core so it satisfies
  `Call::insert<T: Clone + Send + Sync + 'static>` and can be taken back out by
  the extractor (`Option::take`).

### 4.2 `churust-core/src/ws.rs` (feature `ws`)
- **`WebSocketUpgrade`** — `FromCallParts`:
  - Validates handshake: `Upgrade: websocket` (case-insensitive), `Connection`
    contains `upgrade`, `Sec-WebSocket-Version: 13`, and a `Sec-WebSocket-Key`
    is present. On failure → `Error` with **426 Upgrade Required** (or 400 for a
    malformed key).
  - Reads any `Sec-WebSocket-Protocol` (first value) to optionally echo back.
  - Takes the `OnUpgradeHandle` from `Call` extensions; computes
    `Sec-WebSocket-Accept` via `tungstenite::handshake::derive_accept_key`.
  - Holds: the `OnUpgrade` future, the accept key, optional chosen subprotocol,
    and a `tungstenite` `WebSocketConfig` (with a default max message/frame size
    for safety).
- **`WebSocketUpgrade::on_upgrade<F, Fut>(self, callback: F) -> Response`**
  where `F: FnOnce(WebSocket) -> Fut + Send + 'static`, `Fut: Future<Output=()> + Send`:
  - `tokio::spawn`s a task that `await`s the `OnUpgrade` future, wraps the
    upgraded IO as `tokio_tungstenite::WebSocketStream::from_raw_socket(
    TokioIo::new(upgraded), Role::Server, Some(config)).await`, builds a
    `WebSocket`, and runs `callback`.
  - Returns a `Response` with status **101**, headers `Upgrade: websocket`,
    `Connection: Upgrade`, `Sec-WebSocket-Accept: <key>`, and (if negotiated)
    `Sec-WebSocket-Protocol`.
  - The task resolves `OnUpgrade` only after the engine writes the 101 and hyper
    finishes the upgrade — so spawning before returning the `Response` is correct.
- **`WebSocket`** wraps `WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>`:
  - `async fn recv(&mut self) -> Option<Result<Message>>`
  - `async fn send(&mut self, msg: Message) -> Result<()>`
  - `async fn send_text(&mut self, s: impl Into<String>) -> Result<()>`
  - `async fn send_binary(&mut self, b: impl Into<Vec<u8>>) -> Result<()>`
  - `async fn close(&mut self) -> Result<()>`
- **`Message`** — `enum Message { Text(String), Binary(Vec<u8>), Ping(Vec<u8>), Pong(Vec<u8>), Close }`
  with `From<Message> for tungstenite::Message` and `TryFrom`/`From` back.
  WebSocket errors map to `churust_core::Error` (or a `WsError` re-export) for
  the `Result` alias used by `recv`/`send`.

### 4.3 Umbrella + examples
- `churust` umbrella: feature `ws = ["churust-core/ws"]`; re-export
  `WebSocketUpgrade`, `WebSocket`, `Message` at the crate root and in `prelude`
  under `#[cfg(feature = "ws")]`.
- `examples/chat`: an echo endpoint plus a simple broadcast room (using a
  `tokio::sync::broadcast` channel held in app state) to demonstrate real use.

## 5. Data flow

```
client GET /echo (Upgrade: websocket, Sec-WebSocket-Key: ...)
  └─ engine: detects upgrade → on_upgrade = hyper::upgrade::on(&mut req)
             → insert OnUpgradeHandle into Call.extensions → build Call (empty body)
  └─ pipeline → handler:  |ws: WebSocketUpgrade| ...
        └─ WebSocketUpgrade::from_call_parts: validate + take handle + accept key
        └─ ws.on_upgrade(cb): spawn task(await OnUpgrade → WebSocket → cb), return 101 Response
  └─ engine writes 101 (with_upgrades) → hyper completes upgrade → OnUpgrade resolves
  └─ spawned task: WebSocketStream over upgraded IO → cb(socket) runs recv/send loop
```

## 6. Error handling
- Non-upgrade request hitting a WS handler → `WebSocketUpgrade` extraction fails
  → **426 Upgrade Required** (rendered as the normal error response; if the
  `json` plugin is installed it becomes JSON, consistent with the rest of the
  framework).
- Post-upgrade errors (broken pipe, protocol error) surface through
  `recv`/`send` `Result`s; the user's loop decides whether to break.
- A panic inside the spawned WS task is isolated to that task (does not affect
  the server); document that handlers should avoid panicking and handle errors.

## 7. Security
- Pre-upgrade body limit + request timeout still apply to the handshake request.
- Post-upgrade, the raw stream is **not** subject to the request timeout (it's a
  long-lived connection by design). Incoming message size is capped via
  `WebSocketConfig` (default e.g. 16 MiB max message, 1 MiB max frame);
  documented so users can tune it. WS handlers own their own idle/read timeouts.
- Works over TLS automatically (engine upgrades whatever stream it accepted).

## 8. Testing
- **Integration** (`churust-core/tests/websocket.rs`, `#[cfg(feature="ws")]`):
  bind ephemeral port, start the app with an echo WS route, connect with a
  `tokio-tungstenite` client, send text + binary, assert echoes, then close
  cleanly. Mirrors `engine_serve.rs`.
- **Unit:** `Message` ↔ `tungstenite::Message` conversions; accept-key
  derivation matches the RFC 6455 example (key `dGhlIHNhbXBsZSBub25jZQ==` →
  `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`); `WebSocketUpgrade` rejects a plain GET with
  426 (via `TestClient`).

## 9. Out-of-scope confirmation
No compression, no Autobahn, no rooms in core, no subprotocol negotiation beyond
single-echo. These can be later v2.x increments.

## 10. Build order (detailed in the plan)
1. Add `ws` feature + `tokio-tungstenite` optional dep to `churust-core`;
   `OnUpgradeHandle` newtype.
2. Engine: `.with_upgrades()` + capture `OnUpgrade` for upgrade requests.
3. `Message` enum + conversions (unit-tested).
4. `WebSocket` wrapper (recv/send/close).
5. `WebSocketUpgrade` extractor + `on_upgrade` (accept key, 101 response, spawn).
6. Integration echo test (real client) + extractor-rejection test.
7. Umbrella feature + prelude re-exports; `examples/chat`.
8. Full-suite + clippy (default and `ws`/`full,ws`) checkpoint.
