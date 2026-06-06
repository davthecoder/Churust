# Churust WebSockets — Implementation Plan (v2.0)

> **Builds on Churust v1** (Plans 1–3, all green). Extend existing files; do not recreate them.

**Goal:** Add WebSocket support to Churust behind a `ws` feature: a `WebSocketUpgrade` extractor + `on_upgrade(callback)` that switches an HTTP request into a bidirectional WebSocket, with a clean `WebSocket`/`Message` API.

**Architecture:** The engine captures hyper's `OnUpgrade` handle for upgrade requests (before buffering the body) and seeds it into the `Call`'s per-call extensions; connections are served `.with_upgrades()`. A `WebSocketUpgrade` extractor (`FromCallParts`) validates the handshake, takes the handle, and computes the accept key; `on_upgrade` spawns a task that wraps the upgraded IO in a `tokio-tungstenite` `WebSocketStream` and returns a `101` response. All `ws` code is feature-gated so default builds are byte-for-byte unchanged.

**Tech Stack additions:** `tokio-tungstenite` (optional, brings `tungstenite` + `derive_accept_key`).

**Critical rules:**
- Everything WebSocket-specific is behind `#[cfg(feature = "ws")]`. Default builds (and all existing tests/clippy) must be unaffected.
- Do not change `App::process` / `Call::new` signatures. Add `App::process_with_extensions` (process delegates) and `Call::seed_extensions`.
- `OnUpgradeHandle` must be `Clone + Send + Sync + 'static` (it's `Arc<Mutex<Option<OnUpgrade>>>`) so it fits `Call::insert`.
- No git, ever.

---

## File Structure

```
churust-core/Cargo.toml        ADD optional tokio-tungstenite + [features] ws; dev-dep tokio-tungstenite
churust-core/src/ws.rs         NEW (feature ws) — OnUpgradeHandle, is_upgrade_request, Message, WebSocket, WebSocketUpgrade
churust-core/src/call.rs       ADD seed_extensions(http::Extensions)
churust-core/src/app.rs        ADD process_with_extensions(...); process() delegates
churust-core/src/engine.rs     ADD .with_upgrades() (ws) + capture OnUpgrade -> extensions (ws)
churust-core/src/lib.rs        ADD #[cfg(feature="ws")] pub mod ws; + re-exports
churust-core/tests/websocket.rs NEW (feature ws) — real-client echo integration test
churust/Cargo.toml             ADD feature ws = ["churust-core/ws"]
churust/src/lib.rs             cfg re-exports + prelude additions
examples/chat/                 NEW example — echo + broadcast room
Cargo.toml                     members += examples/chat
```

---

## Task 1: Core plumbing — `ws` feature, extension seeding, upgrade holder

**Files:**
- Modify: `churust-core/Cargo.toml`
- Modify: `churust-core/src/call.rs`
- Modify: `churust-core/src/app.rs`
- Create: `churust-core/src/ws.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Add the feature + dependency**

In `churust-core/Cargo.toml` `[dependencies]` add:
```toml
tokio-tungstenite = { version = "0.26", optional = true }
```
In `[features]` add (keep the existing `tls` line):
```toml
ws = ["dep:tokio-tungstenite"]
```
Add a dev-dependency (for the integration-test client in Task 5):
```toml
[dev-dependencies]
tokio-tungstenite = "0.26"
```
Run: `cd churust-core && cargo add tokio-tungstenite --optional && cargo add tokio-tungstenite --dev; cd ..`
(If `cargo add` resolves a newer 0.2x, accept it and adapt API per later notes.)

- [ ] **Step 2: Write the failing test for extension seeding**

Add to `churust-core/src/app.rs` `mod tests`:
```rust
    #[tokio::test]
    async fn process_with_extensions_seeds_call() {
        #[derive(Clone)]
        struct Marker(u32);
        let app = Churust::server()
            .routing(|r| {
                r.get("/", |c: Call| async move {
                    format!("{}", c.get::<Marker>().map(|m| m.0).unwrap_or(0))
                });
            })
            .build();
        let mut ext = http::Extensions::new();
        ext.insert(Marker(7));
        let res = app
            .process_with_extensions(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new(), ext)
            .await;
        assert_eq!(res.body, Bytes::from("7"));
    }
```

- [ ] **Step 3: Add `Call::seed_extensions`**

In `churust-core/src/call.rs`, inside `impl Call` (near `set_state`), add:
```rust
    /// Merge externally-built extensions into this call (used by the engine to
    /// inject per-connection data such as a pending WebSocket upgrade). Existing
    /// entries of the same type are overwritten.
    pub(crate) fn seed_extensions(&mut self, ext: http::Extensions) {
        self.extensions.extend(ext);
    }
```

- [ ] **Step 4: Add `App::process_with_extensions`; delegate from `process`**

In `churust-core/src/app.rs`, replace the existing `process` method with the following two methods (the body of the old `process` moves into `process_with_extensions`):
```rust
    /// THE single request entry point used by the engine and `TestClient`.
    /// Panic-isolated: a panicking handler yields 500.
    pub async fn process(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        self.process_with_extensions(method, uri, headers, body, http::Extensions::new())
            .await
    }

    /// Like [`App::process`], but seeds the `Call` with pre-built extensions
    /// (e.g. a captured WebSocket upgrade handle). Advanced/engine use.
    pub async fn process_with_extensions(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
        extensions: http::Extensions,
    ) -> Response {
        let app = self.clone();
        let fut = async move {
            let mut call = Call::new(method, uri, headers, body);
            call.seed_extensions(extensions);
            call.set_state(app.inner.state.clone());
            app.run_pipeline(call).await
        };
        match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
            Ok(res) => res,
            Err(_) => Response::text("Internal Server Error")
                .with_status(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
```

- [ ] **Step 5: Create the `ws` module stub (holder + detection)**

`churust-core/src/ws.rs`:
```rust
//! WebSocket support (feature `ws`).
//!
//! A handler upgrades a request by taking the [`WebSocketUpgrade`] extractor and
//! calling [`WebSocketUpgrade::on_upgrade`]:
//!
//! ```no_run
//! use churust_core::{Call, Churust};
//! use churust_core::ws::{Message, WebSocketUpgrade};
//!
//! # fn build() {
//! Churust::server().routing(|r| {
//!     r.get("/echo", |ws: WebSocketUpgrade| async move {
//!         ws.on_upgrade(|mut sock| async move {
//!             while let Some(Ok(msg)) = sock.recv().await {
//!                 if sock.send(msg).await.is_err() { break; }
//!             }
//!         })
//!     });
//! });
//! # }
//! ```

use http::header::{CONNECTION, UPGRADE};
use http::HeaderMap;
use hyper::upgrade::OnUpgrade;
use std::sync::{Arc, Mutex};

/// A cloneable, takeable holder for hyper's pending connection upgrade. The
/// engine inserts one into a [`Call`](crate::Call)'s extensions for WebSocket
/// handshake requests; [`WebSocketUpgrade`] takes it back out.
#[derive(Clone)]
pub struct OnUpgradeHandle(Arc<Mutex<Option<OnUpgrade>>>);

impl OnUpgradeHandle {
    /// Wrap a pending upgrade.
    pub fn new(on_upgrade: OnUpgrade) -> Self {
        Self(Arc::new(Mutex::new(Some(on_upgrade))))
    }

    /// Take the upgrade future (can only succeed once).
    pub(crate) fn take(&self) -> Option<OnUpgrade> {
        self.0.lock().ok().and_then(|mut guard| guard.take())
    }
}

impl std::fmt::Debug for OnUpgradeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnUpgradeHandle")
    }
}

/// True if the request headers request a WebSocket upgrade (`Connection:
/// upgrade` + `Upgrade: websocket`, case-insensitive).
pub(crate) fn is_upgrade_request(headers: &HeaderMap) -> bool {
    let connection_upgrade = headers
        .get(CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().split(',').any(|p| p.trim() == "upgrade"))
        .unwrap_or(false);
    let upgrade_websocket = headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    connection_upgrade && upgrade_websocket
}
```

- [ ] **Step 6: Wire the module + run the seeding test**

In `churust-core/src/lib.rs` add:
```rust
#[cfg(feature = "ws")]
pub mod ws;
```
Run: `cargo test -p churust-core app::process_with_extensions_seeds_call`
Expected: PASS.
Also confirm the ws module compiles: `cargo build -p churust-core --features ws`
Expected: builds (the `OnUpgradeHandle`/`is_upgrade_request` items compile; `is_upgrade_request` is `pub(crate)` so the unused-in-this-task warning is acceptable until Task 4 — if building under `-D warnings`, it is used by tests in later tasks; for now just `cargo build`, not clippy).

- [ ] **Step 7: Checkpoint**

Run: `cargo test -p churust-core` (default) and `cargo build -p churust-core --features ws`.
Expected: PASS / builds.

---

## Task 2: `Message` enum + conversions

**Files:**
- Modify: `churust-core/src/ws.rs`

- [ ] **Step 1: Write the failing tests**

Add to `churust-core/src/ws.rs` (append a `tests` module at the end):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    #[test]
    fn message_round_trips_through_tungstenite() {
        let cases = [
            Message::Text("hi".into()),
            Message::Binary(vec![1, 2, 3]),
            Message::Ping(vec![9]),
            Message::Pong(vec![8]),
            Message::Close,
        ];
        for m in cases {
            let t: TMessage = m.clone().into();
            let back: Message = t.into();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn accept_key_matches_rfc6455_example() {
        // RFC 6455 §1.3 worked example.
        let accept =
            tokio_tungstenite::tungstenite::handshake::derive_accept_key(b"dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
```

- [ ] **Step 2: Implement `Message` + conversions**

Add to `churust-core/src/ws.rs` (after the `is_upgrade_request` function):
```rust
use tokio_tungstenite::tungstenite::Message as TMessage;

/// A WebSocket message. A deliberately small enum so user code never has to name
/// `tungstenite` types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A UTF-8 text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
    /// A ping control frame (payload echoed back by the peer as a pong).
    Ping(Vec<u8>),
    /// A pong control frame.
    Pong(Vec<u8>),
    /// A close frame (connection is closing).
    Close,
}

impl From<Message> for TMessage {
    fn from(m: Message) -> Self {
        match m {
            Message::Text(s) => TMessage::Text(s.into()),
            Message::Binary(b) => TMessage::Binary(b.into()),
            Message::Ping(b) => TMessage::Ping(b.into()),
            Message::Pong(b) => TMessage::Pong(b.into()),
            Message::Close => TMessage::Close(None),
        }
    }
}

impl From<TMessage> for Message {
    fn from(m: TMessage) -> Self {
        match m {
            TMessage::Text(s) => Message::Text(s.to_string()),
            TMessage::Binary(b) => Message::Binary(b.to_vec()),
            TMessage::Ping(b) => Message::Ping(b.to_vec()),
            TMessage::Pong(b) => Message::Pong(b.to_vec()),
            TMessage::Close(_) => Message::Close,
            // Raw frames are not surfaced to user code.
            _ => Message::Close,
        }
    }
}
```
> Version note: the conversions use `.into()` / `.to_vec()` / `.to_string()`, which work whether the resolved `tungstenite` uses `String`/`Vec<u8>` (≤0.21) or `Utf8Bytes`/`Bytes` (≥0.23) for the frame payloads. The trailing `_ =>` arm covers `Message::Frame` in versions that have it.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p churust-core --features ws ws::`
Expected: PASS (2 tests).

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-core --features ws`
Expected: PASS.

---

## Task 3: `WebSocket` + `WebSocketUpgrade` + `on_upgrade`

**Files:**
- Modify: `churust-core/src/ws.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the failing test (426 rejection via TestClient)**

Add to `churust-core/src/ws.rs` `mod tests`:
```rust
    use crate::{Call, Churust, TestClient};

    #[tokio::test]
    async fn plain_get_to_ws_route_is_426() {
        let app = Churust::server()
            .routing(|r| {
                r.get("/ws", |ws: WebSocketUpgrade| async move {
                    ws.on_upgrade(|_sock| async {})
                });
            })
            .build();
        // A normal GET (no upgrade headers, no captured handle) must be rejected.
        let res = TestClient::new(app).get("/ws").send().await;
        assert_eq!(res.status(), http::StatusCode::UPGRADE_REQUIRED);
    }
```

- [ ] **Step 2: Implement `WebSocket`, `WebSocketUpgrade`, `on_upgrade`**

Add to `churust-core/src/ws.rs` (after the `Message` impls). Add these imports at the top of the file (merge with existing `use` lines):
```rust
use crate::call::Call;
use crate::error::{Error, Result};
use crate::extract::FromCallParts;
use crate::response::Response;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use http::header::{
    SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL, SEC_WEBSOCKET_VERSION,
};
use http::{HeaderValue, StatusCode};
use hyper_util::rt::TokioIo;
use std::future::Future;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::WebSocketStream;
```
Then the types:
```rust
/// An established WebSocket connection. Obtained inside the
/// [`WebSocketUpgrade::on_upgrade`] callback.
pub struct WebSocket {
    inner: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
}

impl WebSocket {
    /// Receive the next message. `None` when the connection has closed.
    pub async fn recv(&mut self) -> Option<Result<Message>> {
        match self.inner.next().await {
            Some(Ok(msg)) => Some(Ok(msg.into())),
            Some(Err(e)) => Some(Err(Error::internal(format!("websocket recv: {e}")))),
            None => None,
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Message) -> Result<()> {
        self.inner
            .send(msg.into())
            .await
            .map_err(|e| Error::internal(format!("websocket send: {e}")))
    }

    /// Convenience: send a text message.
    pub async fn send_text(&mut self, text: impl Into<String>) -> Result<()> {
        self.send(Message::Text(text.into())).await
    }

    /// Convenience: send a binary message.
    pub async fn send_binary(&mut self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.send(Message::Binary(bytes.into())).await
    }

    /// Close the connection.
    pub async fn close(&mut self) -> Result<()> {
        self.inner
            .close(None)
            .await
            .map_err(|e| Error::internal(format!("websocket close: {e}")))
    }
}

/// Extractor that represents a pending WebSocket upgrade. A handler takes it as
/// an argument, then calls [`on_upgrade`](WebSocketUpgrade::on_upgrade).
///
/// Extraction fails with **426 Upgrade Required** if the request is not a valid
/// WebSocket handshake.
pub struct WebSocketUpgrade {
    on_upgrade: OnUpgrade,
    accept_key: HeaderValue,
    protocol: Option<HeaderValue>,
}

#[async_trait]
impl FromCallParts for WebSocketUpgrade {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let version_ok = call
            .header(SEC_WEBSOCKET_VERSION.as_str())
            .map(|v| v == "13")
            .unwrap_or(false);
        if !is_upgrade_request(call.headers()) || !version_ok {
            return Err(Error::new(
                StatusCode::UPGRADE_REQUIRED,
                "expected a WebSocket upgrade request",
            )
            .with_response_header(UPGRADE, HeaderValue::from_static("websocket")));
        }

        let key = call
            .header(SEC_WEBSOCKET_KEY.as_str())
            .ok_or_else(|| Error::bad_request("missing Sec-WebSocket-Key"))?;
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        let accept_key =
            HeaderValue::from_str(&accept).map_err(|_| Error::internal("invalid accept key"))?;

        let protocol = call
            .header(SEC_WEBSOCKET_PROTOCOL.as_str())
            .and_then(|p| p.split(',').next())
            .and_then(|p| HeaderValue::from_str(p.trim()).ok());

        let handle = call.get::<OnUpgradeHandle>().ok_or_else(|| {
            Error::new(
                StatusCode::UPGRADE_REQUIRED,
                "WebSocket upgrade unavailable (no pending connection upgrade)",
            )
        })?;
        let on_upgrade = handle
            .take()
            .ok_or_else(|| Error::internal("WebSocket upgrade already consumed"))?;

        Ok(WebSocketUpgrade { on_upgrade, accept_key, protocol })
    }
}

impl WebSocketUpgrade {
    /// Finish the handshake: spawn a task that runs `callback` with the
    /// established [`WebSocket`] once the upgrade completes, and return the
    /// `101 Switching Protocols` response the engine will send.
    pub fn on_upgrade<F, Fut>(self, callback: F) -> Response
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let WebSocketUpgrade { on_upgrade, accept_key, protocol } = self;

        tokio::spawn(async move {
            if let Ok(upgraded) = on_upgrade.await {
                let stream = WebSocketStream::from_raw_socket(
                    TokioIo::new(upgraded),
                    Role::Server,
                    None,
                )
                .await;
                callback(WebSocket { inner: stream }).await;
            }
        });

        let mut res = Response::new(StatusCode::SWITCHING_PROTOCOLS);
        res.headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        res.headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        res.headers.insert(SEC_WEBSOCKET_ACCEPT, accept_key);
        if let Some(p) = protocol {
            res.headers.insert(SEC_WEBSOCKET_PROTOCOL, p);
        }
        res
    }
}
```
> Version note: `WebSocketStream::from_raw_socket(io, Role::Server, None)` uses `tungstenite`'s default config (which already caps message/frame size). If the resolved `tokio-tungstenite` changes this signature, adapt minimally while keeping server-role + default config.

- [ ] **Step 3: Re-export from core**

In `churust-core/src/lib.rs`, below the `pub mod ws;` line:
```rust
#[cfg(feature = "ws")]
pub use ws::{WebSocket, WebSocketUpgrade};
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p churust-core --features ws ws::plain_get_to_ws_route_is_426`
Expected: PASS.

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core --features ws`
Then: `cargo clippy -p churust-core --all-targets --features ws -- -D warnings`
Expected: PASS / clean.

---

## Task 4: Engine upgrade support

**Files:**
- Modify: `churust-core/src/engine.rs`

- [ ] **Step 1: Serve connections with upgrades enabled (ws feature)**

In `churust-core/src/engine.rs`, in `serve_stream`, change the connection build so upgrades are enabled when the `ws` feature is on:
```rust
    let conn = hyper::server::conn::http1::Builder::new().serve_connection(io, svc);
    #[cfg(feature = "ws")]
    let conn = conn.with_upgrades();
    let fut = watcher.watch(conn);
    tokio::spawn(async move {
        let _ = fut.await;
    });
```
> Implementer note: `hyper_util`'s `GracefulConnection` is implemented for `http1::UpgradeableConnection`, so `watcher.watch(conn)` accepts the upgraded connection. If a version mismatch makes `watch` reject it, fall back for the ws build to `tokio::spawn(async move { let _ = conn.await; });` (dropping graceful drain only for WS connections) — keep the non-ws path exactly as-is.

- [ ] **Step 2: Capture the upgrade handle and seed it into the call (ws feature)**

In `churust-core/src/engine.rs`, change `handle` so it captures `OnUpgrade` before consuming the request and passes it via extensions. Replace the start of `handle` (the `let (parts, body) = req.into_parts();` line and the `app.process(...)` call) as follows. Note the parameter stays `req` (not `mut`) to avoid an unused-mut warning on default builds; it is shadowed as mutable only under `ws`:
```rust
async fn handle(
    app: App,
    req: HyperRequest<Incoming>,
    max_body: usize,
    timeout_ms: u64,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    #[cfg(feature = "ws")]
    let mut req = req;
    #[cfg(feature = "ws")]
    let on_upgrade = if crate::ws::is_upgrade_request(req.headers()) {
        Some(hyper::upgrade::on(&mut req))
    } else {
        None
    };

    let (parts, body) = req.into_parts();

    // Enforce max body size before buffering.
    let collected = Limited::new(body, max_body).collect().await;
    let body_bytes = match collected {
        Ok(buf) => buf.to_bytes(),
        Err(_) => {
            let mut resp = HyperResponse::new(Full::new(Bytes::from("Payload Too Large")));
            *resp.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
            return Ok(resp);
        }
    };

    #[cfg(feature = "ws")]
    let process = {
        let mut extensions = http::Extensions::new();
        if let Some(on_upgrade) = on_upgrade {
            extensions.insert(crate::ws::OnUpgradeHandle::new(on_upgrade));
        }
        app.process_with_extensions(parts.method, parts.uri, parts.headers, body_bytes, extensions)
    };
    #[cfg(not(feature = "ws"))]
    let process = app.process(parts.method, parts.uri, parts.headers, body_bytes);

    let res = if timeout_ms == 0 {
        process.await
    } else {
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), process).await {
            Ok(res) => res,
            Err(_) => crate::response::Response::text("Request Timeout")
                .with_status(StatusCode::REQUEST_TIMEOUT),
        }
    };

    let mut builder = HyperResponse::builder().status(res.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = res.headers;
    }
    Ok(builder
        .body(Full::new(res.body))
        .expect("response build is infallible for buffered body"))
}
```
Add the `http` import if not present at the top of `engine.rs`: it is already a transitive dep; reference it as `http::Extensions` (add `use http;` is unnecessary — use the full path `http::Extensions` as written).

- [ ] **Step 3: Checkpoint**

Run: `cargo build -p churust-core --features ws`
Then: `cargo test -p churust-core` (default — engine unchanged for non-ws) and `cargo clippy -p churust-core --all-targets --features ws -- -D warnings` and `cargo clippy -p churust-core --all-targets -- -D warnings` (default — confirm no unused-mut/dead-code warnings).
Expected: builds / PASS / clean.

---

## Task 5: WebSocket echo integration test (real client)

**Files:**
- Create: `churust-core/tests/websocket.rs`

- [ ] **Step 1: Write the integration test**

`churust-core/tests/websocket.rs`:
```rust
#![cfg(feature = "ws")]

use churust_core::ws::{Message, WebSocketUpgrade};
use churust_core::{Call, Churust};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;

#[tokio::test]
async fn websocket_echo_round_trip() {
    // Pick a free port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

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
        app.start_with_shutdown(async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    use tokio_tungstenite::tungstenite::Message as TM;
    let url = format!("ws://{addr}/echo");
    let (mut client, _resp) = tokio_tungstenite::connect_async(url).await.expect("connect");

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
```
> Version note: `to_text()` returns `&str`; `into_data()` returns the payload (`Bytes` in ≥0.23, `Vec<u8>` in ≤0.21) — `.to_vec()` normalizes both. `TM::Text("hello".into())` and `TM::Binary(vec.into())` compile against both payload representations.

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p churust-core --features ws --test websocket`
Expected: PASS (echo + close).

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core --features ws`
Expected: PASS (unit + integration).

---

## Task 6: Umbrella feature, prelude, and `examples/chat`

**Files:**
- Modify: `churust/Cargo.toml`
- Modify: `churust/src/lib.rs`
- Create: `examples/chat/Cargo.toml`
- Create: `examples/chat/src/main.rs`
- Modify: `Cargo.toml` (members += `examples/chat`)

- [ ] **Step 1: Umbrella feature**

In `churust/Cargo.toml` `[features]` add:
```toml
ws = ["churust-core/ws"]
```
(Leave `full` as the four plugins; `ws` is opt-in separately, mirroring `tls`.)

- [ ] **Step 2: cfg re-exports + prelude**

In `churust/src/lib.rs`, after the existing `pub use churust_core::*;`, add:
```rust
/// WebSocket types (`WebSocket`, `WebSocketUpgrade`, `ws::Message`). Enabled by
/// the `ws` feature.
#[cfg(feature = "ws")]
pub use churust_core::ws;
```
Extend the `prelude` module (append inside `pub mod prelude { ... }`):
```rust
    #[cfg(feature = "ws")]
    pub use churust_core::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
```

- [ ] **Step 3: `examples/chat` — echo + broadcast room**

In root `Cargo.toml` add `"examples/chat"` to `members`.

`examples/chat/Cargo.toml`:
```toml
[package]
name = "chat"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
churust = { path = "../../churust", features = ["ws"] }
tokio = { workspace = true }
```

`examples/chat/src/main.rs`:
```rust
use churust::prelude::*;
use churust::ws::{Message, WebSocketUpgrade};
use std::sync::Arc;
use tokio::sync::broadcast;

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
            r.get("/room", |room: State<Room>, ws: WebSocketUpgrade| async move {
                let tx = room.tx.clone();
                let mut rx = tx.subscribe();
                ws.on_upgrade(move |mut sock| async move {
                    loop {
                        tokio::select! {
                            incoming = sock.recv() => match incoming {
                                Some(Ok(Message::Text(t))) => { let _ = tx.send(t); }
                                Some(Ok(Message::Close)) | None => break,
                                Some(Err(_)) => break,
                                _ => {}
                            },
                            outgoing = rx.recv() => match outgoing {
                                Ok(text) => { if sock.send_text(text).await.is_err() { break; } }
                                Err(_) => {}
                            },
                        }
                    }
                })
            });
        })
        .start()
        .await
}
```
> Note the `/room` handler has TWO arguments: `State<Room>` (FromCallParts) and `WebSocketUpgrade` (FromCallParts, used last but still a parts extractor) — covered by the arity-2 handler impl from Plan 2.

- [ ] **Step 4: Checkpoint**

Run: `cargo build -p chat`
Then: `cargo build -p churust --features ws` and `cargo test -p churust --doc --features ws`.
Expected: builds / PASS.

---

## Task 7: Full-suite + clippy checkpoint

**Files:** none (verification only)

- [ ] **Step 1: Run the matrix**

Run each and confirm green:
```
cargo test --workspace
cargo test -p churust-core --features ws
cargo test -p churust --features ws
cargo build -p chat
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p churust-core --all-targets --features ws -- -D warnings
cargo clippy -p churust --all-targets --features ws -- -D warnings
```
Expected: all PASS / clean. (Default-feature builds must be unchanged from before this plan.)

- [ ] **Step 2: Optional manual smoke (no commit)**

```
cargo run -p chat &
# Use any ws client (e.g. websocat): echo
#   websocat ws://127.0.0.1:8080/echo   -> type a line, see it echoed
# stop the process
```

- [ ] **Step 3: Update README WebSocket section (optional but recommended)**

Add a short "WebSockets (feature `ws`)" subsection to `README.md` showing the `/echo` handler and noting `features = ["ws"]`. Keep it to ~10 lines.

---

## Self-Review

**Spec coverage (vs `2026-06-06-churust-websockets-design.md`):**
- §4.1 engine: `.with_upgrades()` + capture `OnUpgrade` → Task 4. ✓
- §4.1 `OnUpgradeHandle` (Clone holder) + seeding via extensions → Task 1 (`OnUpgradeHandle`, `seed_extensions`, `process_with_extensions`). ✓
- §4.2 `WebSocketUpgrade` (validate, 426, accept key, protocol echo, take handle) + `on_upgrade` (spawn, 101) → Task 3. ✓
- §4.2 `WebSocket` (recv/send/send_text/send_binary/close) → Task 3. ✓
- §4.2 `Message` enum + tungstenite conversions → Task 2. ✓
- §4.3 umbrella feature + prelude + `examples/chat` (echo + broadcast) → Task 6. ✓
- §8 testing: real-client echo integration (Task 5), Message conversions + accept-key (Task 2), 426 rejection via TestClient (Task 3). ✓
- §7 security: pre-upgrade limits unchanged; default tungstenite config caps message size (Task 3 uses default config); documented. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to". Version notes are guidance beside complete code, not placeholders.

**Type consistency:**
- `OnUpgradeHandle::new(OnUpgrade)` (T1) ↔ engine `OnUpgradeHandle::new(on_upgrade)` (T4) ↔ extractor `call.get::<OnUpgradeHandle>()` + `.take()` (T3). ✓
- `App::process_with_extensions(Method, Uri, HeaderMap, Bytes, http::Extensions)` (T1) ↔ engine call site (T4). ✓
- `Call::seed_extensions(http::Extensions)` (T1) used by `process_with_extensions` (T1). ✓
- `is_upgrade_request(&HeaderMap)` (T1) used by extractor (T3) and engine (T4). ✓
- `Message` variants (T2) used by `WebSocket`/conversions (T3), example (T6), integration test (T5). ✓
- `WebSocketUpgrade::on_upgrade<F: FnOnce(WebSocket)->Fut>` (T3) used by example + tests. ✓
- `WebSocket::{recv,send,send_text,close}` (T3) used by example (T6) + test (T5). ✓

**Risk notes:**
- `tokio-tungstenite` payload types changed across versions (`String`/`Vec<u8>` → `Utf8Bytes`/`Bytes`); all conversions use `.into()`/`.to_vec()`/`.to_string()` to stay version-agnostic. If `cargo add` resolves a version with a different `from_raw_socket`/`WebSocketConfig` API, adapt minimally.
- `GracefulConnection for UpgradeableConnection`: assumed present in hyper-util (it is for current 0.1.x). Fallback documented in Task 4 Step 1.
- All `ws` code is feature-gated; default-feature builds + the entire existing test/clippy matrix must remain unchanged — Task 4 Step 3 and Task 7 explicitly re-verify the default build.

---

## Execution Handoff

Execute sequentially, task by task: implement, then spec review, then quality review. This is the first v2 feature; after it's green, Churust supports WebSockets behind the `ws` feature.
