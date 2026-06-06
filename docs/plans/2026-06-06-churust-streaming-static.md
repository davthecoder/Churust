# Churust Streaming Responses + Static Files — Implementation Plan (v2.1)

> **Builds on Churust v1 + v2.0** (all green). Extend existing files; do not recreate them.

**Goal:** Replace the buffered-only `Response` body with a `Body` type (buffered `Bytes` *or* a lazy stream), wire it through the engine, and add a `StaticFiles` directory handler behind an `fs` feature.

**Architecture:** A new `Body` enum lives on `Response.body`; `From<Bytes>/String/&str/Vec<u8>` keeps every existing constructor and `IntoResponse` impl unchanged, and `impl PartialEq<Bytes> for Body` keeps existing `assert_eq!(res.body, Bytes::from(..))` tests compiling. The engine emits a `BoxBody<Bytes, io::Error>` (Full for buffered, StreamBody for streamed). `StaticFiles` streams files with a hand-rolled async read loop (no new deps) and a built-in MIME map.

**Tech Stack:** No new dependencies — `http-body-util`, `futures-util`, `bytes`, `tokio` are already in `churust-core`.

**Critical rules:**
- `Body` is always-on (no feature). Only `StaticFiles` is behind the `fs` feature.
- Keep all existing `Response` constructor signatures. `Response` loses its `Clone` derive (streams aren't `Clone`; no code clones a `Response` — verified).
- `Body::Stream` item type is `crate::Result<Bytes>` (i.e. `Result<Bytes, crate::Error>`).
- No git, ever.

---

## File Structure

```
churust-core/src/body.rs        NEW — Body enum (Bytes | Stream), conversions, collect
churust-core/src/response.rs    Response.body: Bytes -> Body; drop Clone; add stream(); fix doctests
churust-core/src/handler.rs     fix 2 doctests using &res.body[..]
churust-core/src/call.rs        fix 1 doctest using &res.body[..]
churust-core/src/engine.rs      emit BoxBody; convert Body -> Full/StreamBody
churust-core/src/test.rs        TestResponse collects the (possibly streamed) body to Bytes
churust-core/src/ws.rs          101 response uses Body::empty()
churust-core/src/fs.rs          NEW (feature fs) — StaticFiles + MIME map + traversal guard
churust-core/src/lib.rs         export Body; #[cfg(feature="fs")] export fs/StaticFiles
churust-core/Cargo.toml         add [features] fs
churust-core/tests/streaming.rs NEW — streamed response over a real socket
churust-json/src/lib.rs         JSON error middleware: rewrite only buffered bodies (as_bytes)
churust/Cargo.toml              feature fs = ["churust-core/fs"]
churust/src/lib.rs              re-export Body always; StaticFiles under fs; prelude
examples/static/                NEW example — serve ./public + a streamed route
Cargo.toml                      members += examples/static
```

---

## Task 1: The `Body` type

**Files:**
- Create: `churust-core/src/body.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

`churust-core/src/body.rs`:
```rust
//! The response [`Body`]: either a fully-buffered `Bytes` payload or a lazy
//! stream of byte chunks (for files, large/dynamic responses, SSE, etc.).

use crate::error::{Error, Result};
use bytes::{Bytes, BytesMut};
use futures_util::stream::{Stream, StreamExt};
use std::pin::Pin;

/// A response body.
///
/// Construct buffered bodies with `From` (`Body::from(bytes)`,
/// `"text".into()`), an empty body with [`Body::empty`], or a streaming body
/// with [`Body::from_stream`]. The engine sends buffered bodies in one shot and
/// streamed bodies frame-by-frame.
pub enum Body {
    /// A fully-buffered, in-memory body.
    Bytes(Bytes),
    /// A lazily-produced stream of byte chunks.
    Stream(Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + 'static>>),
}

impl Body {
    /// An empty buffered body.
    pub fn empty() -> Self {
        Body::Bytes(Bytes::new())
    }

    /// Build a streaming body from any `Stream` of byte chunks. Chunk errors are
    /// converted into [`Error`] and surface when the body is read/collected.
    pub fn from_stream<S, E>(stream: S) -> Self
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
        E: std::fmt::Display,
    {
        let mapped = stream.map(|chunk| chunk.map_err(|e| Error::internal(format!("body stream: {e}"))));
        Body::Stream(Box::pin(mapped))
    }

    /// Borrow the buffered bytes, or `None` if this body is a stream. Used by
    /// middleware that only post-processes already-materialized bodies.
    pub fn as_bytes(&self) -> Option<&Bytes> {
        match self {
            Body::Bytes(b) => Some(b),
            Body::Stream(_) => None,
        }
    }

    /// Borrow the buffered bytes as a slice, or `None` for a stream.
    pub fn as_slice(&self) -> Option<&[u8]> {
        self.as_bytes().map(|b| b.as_ref())
    }

    /// True if this is a buffered, empty body. A stream is never reported empty
    /// (its length is unknown until read).
    pub fn is_empty(&self) -> bool {
        matches!(self, Body::Bytes(b) if b.is_empty())
    }

    /// Collect the whole body into `Bytes` (returns the buffer directly when
    /// already buffered, or drains the stream).
    pub async fn into_bytes(self) -> Result<Bytes> {
        match self {
            Body::Bytes(b) => Ok(b),
            Body::Stream(mut s) => {
                let mut buf = BytesMut::new();
                while let Some(chunk) = s.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(buf.freeze())
            }
        }
    }
}

impl Default for Body {
    fn default() -> Self {
        Body::empty()
    }
}

impl From<Bytes> for Body {
    fn from(b: Bytes) -> Self {
        Body::Bytes(b)
    }
}
impl From<Vec<u8>> for Body {
    fn from(b: Vec<u8>) -> Self {
        Body::Bytes(Bytes::from(b))
    }
}
impl From<String> for Body {
    fn from(s: String) -> Self {
        Body::Bytes(Bytes::from(s))
    }
}
impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Body::Bytes(Bytes::from_static(s.as_bytes()))
    }
}

/// Compare a body to `Bytes` (true only when buffered and equal). Lets tests and
/// callers assert on buffered bodies ergonomically.
impl PartialEq<Bytes> for Body {
    fn eq(&self, other: &Bytes) -> bool {
        matches!(self, Body::Bytes(b) if b == other)
    }
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Body::Bytes(b) => f.debug_tuple("Body::Bytes").field(b).finish(),
            Body::Stream(_) => f.write_str("Body::Stream(..)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn buffered_round_trips() {
        let body = Body::from(Bytes::from("hello"));
        assert_eq!(body.as_slice(), Some(&b"hello"[..]));
        assert!(!body.is_empty());
        assert_eq!(body.into_bytes().await.unwrap(), Bytes::from("hello"));
    }

    #[tokio::test]
    async fn empty_is_empty() {
        assert!(Body::empty().is_empty());
        assert_eq!(Body::empty().into_bytes().await.unwrap(), Bytes::new());
    }

    #[tokio::test]
    async fn stream_collects_and_has_no_bytes_view() {
        let chunks = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from("ab")),
            Ok(Bytes::from("cd")),
        ]);
        let body = Body::from_stream(chunks);
        assert!(body.as_bytes().is_none()); // streams expose no borrowed view
        assert!(!body.is_empty());
        assert_eq!(body.into_bytes().await.unwrap(), Bytes::from("abcd"));
    }

    #[test]
    fn partial_eq_bytes() {
        assert!(Body::from("x".to_string()) == Bytes::from("x"));
        let s = Body::from_stream(futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::new())]));
        assert!(!(s == Bytes::new())); // a stream never equals bytes
    }
}
```

- [ ] **Step 2: Wire module + run tests**

In `churust-core/src/lib.rs` add (near the other `pub mod`/`pub use` lines):
```rust
pub mod body;
pub use body::Body;
```
Run: `cargo test -p churust-core body::`
Expected: PASS (4 tests).

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core body::`
Expected: PASS.

---

## Task 2: Swap `Response.body` to `Body`

**Files:**
- Modify: `churust-core/src/response.rs`
- Modify: `churust-core/src/handler.rs` (2 doctests)
- Modify: `churust-core/src/call.rs` (1 doctest)

- [ ] **Step 1: Change the struct + constructors + add `stream`**

In `churust-core/src/response.rs`:
- Add import: `use crate::body::Body;`
- Change the derive on `Response` from `#[derive(Debug, Clone)]` to `#[derive(Debug)]`.
- Change the `body` field type and doc:
```rust
    /// The response body — buffered bytes or a lazy stream.
    pub body: Body,
```
- In `Response::new`, set `body: Body::empty(),`.
- In `Response::text`, change `r.body = Bytes::from(body.into());` to `r.body = Body::from(body.into());`.
- In `Response::bytes`, change `r.body = body.into();` to `r.body = Body::from(body.into());` (the `body: impl Into<Bytes>` first goes to `Bytes`, then to `Body`):
```rust
    pub fn bytes(content_type: &'static str, body: impl Into<Bytes>) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        r.body = Body::from(body.into());
        r
    }
```
- Add a streaming constructor after `bytes`:
```rust
    /// Create a `200 OK` response whose body is produced lazily from `stream`,
    /// with an explicit `Content-Type`. Use for large or dynamic payloads.
    ///
    /// ```
    /// use churust_core::{Body, Response};
    /// use bytes::Bytes;
    ///
    /// let chunks = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from("hi"))]);
    /// let res = Response::stream("text/plain", Body::from_stream(chunks));
    /// assert!(res.body.as_bytes().is_none());
    /// ```
    pub fn stream(content_type: &'static str, body: Body) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        r.body = body;
        r
    }
```
> `futures_util` is a dependency of `churust-core`, so the doctest can name it.

- [ ] **Step 2: Fix `response.rs` doctests that index the body**

In `churust-core/src/response.rs`, update these doctest assertions:
- Line in `Response` struct doc: `assert_eq!(&res.body[..], b"created");` → `assert_eq!(res.body.as_slice(), Some(&b"created"[..]));`
- In `Response::new` doc: `assert!(res.body.is_empty());` stays (Body has `is_empty`).
- In `Response::bytes` doc: `assert_eq!(&res.body[..], &[1, 2, 3]);` → `assert_eq!(res.body.as_slice(), Some(&[1u8, 2, 3][..]));`
- In the `IntoResponse` trait doc: `assert_eq!(&res.body[..], b"made");` → `assert_eq!(res.body.as_slice(), Some(&b"made"[..]));`

- [ ] **Step 3: Fix the unit tests in `response.rs`**

The `mod tests` uses `assert_eq!(r.body, Bytes::from("hi"))` and `assert_eq!(r.body, Bytes::from("made"))` — these now rely on `impl PartialEq<Bytes> for Body` (Task 1) and compile unchanged. Leave them as-is.

- [ ] **Step 4: Fix doctests in `handler.rs` and `call.rs`**

In `churust-core/src/handler.rs`, both doctests containing `assert_eq!(&res.body[..], b"hi");` → `assert_eq!(res.body.as_slice(), Some(&b"hi"[..]));`
In `churust-core/src/call.rs`, the doctest containing `assert_eq!(&res.body[..], b"ok");` → `assert_eq!(res.body.as_slice(), Some(&b"ok"[..]));`

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core` (default features).
Expected: PASS. The crate compiles with `Response.body: Body`; existing `assert_eq!(res.body, Bytes::from(..))` unit tests pass via `PartialEq<Bytes>`; doctests pass via `as_slice`/`is_empty`.

> Note: `churust-core` alone will NOT fully build yet if `engine.rs` reads `res.body` as `Bytes` — it does (`Full::new(res.body)`). That is fixed in Task 3. Run `cargo test -p churust-core` AFTER Task 3 if Task 2's checkpoint shows an engine type error; the engine fix is required for the crate to compile. Implement Task 2 and Task 3 back-to-back. (Task 2 self-check: `cargo build -p churust-core 2>&1 | grep -c "src/engine.rs"` should show only engine.rs errors remain.)

---

## Task 3: Engine emits a boxed body

**Files:**
- Modify: `churust-core/src/engine.rs`

- [ ] **Step 1: Update imports + body conversion**

In `churust-core/src/engine.rs`:
- Change the body-util import line to:
```rust
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited, StreamBody};
```
- Add:
```rust
use crate::body::Body;
use futures_util::StreamExt;
use hyper::body::Frame;
```
- Add a helper to convert `Body` into the boxed hyper body:
```rust
fn into_boxed_body(body: Body) -> BoxBody<Bytes, std::io::Error> {
    match body {
        Body::Bytes(bytes) => Full::new(bytes)
            .map_err(|never| match never {})
            .boxed(),
        Body::Stream(stream) => {
            let frames = stream.map(|chunk| {
                chunk
                    .map(Frame::data)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            });
            StreamBody::new(frames).boxed()
        }
    }
}
```

- [ ] **Step 2: Update `handle`'s return type and body building**

In `churust-core/src/engine.rs`, change `handle` to return `BoxBody`:
- Signature return type: `-> Result<HyperResponse<BoxBody<Bytes, std::io::Error>>, Infallible>`.
- The `PAYLOAD_TOO_LARGE` early return must box its body:
```rust
        Err(_) => {
            let mut resp = HyperResponse::new(
                Full::new(Bytes::from("Payload Too Large"))
                    .map_err(|never| match never {})
                    .boxed(),
            );
            *resp.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
            return Ok(resp);
        }
```
- The final response build changes from `Full::new(res.body)` to the boxed body:
```rust
    let mut builder = HyperResponse::builder().status(res.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = res.headers;
    }
    Ok(builder
        .body(into_boxed_body(res.body))
        .expect("response build is infallible"))
```
- `serve_connection`/`with_upgrades` and the rest of `serve_stream`/`serve` are unchanged; `service_fn` now yields a `BoxBody` response, which hyper accepts.

- [ ] **Step 3: Checkpoint**

Run: `cargo build -p churust-core` then `cargo test -p churust-core`.
Then: `cargo build -p churust-core --features ws` and `cargo test -p churust-core --features ws` (the ws 101 response still builds — Task 4 switches it to `Body::empty()`, but `Response::new` already yields an empty `Body`, so ws compiles now too).
Expected: PASS.

---

## Task 4: Fix the body readers (TestClient, JSON middleware, ws)

**Files:**
- Modify: `churust-core/src/test.rs`
- Modify: `churust-json/src/lib.rs`
- Modify: `churust-core/src/ws.rs`

- [ ] **Step 1: TestClient collects the body**

In `churust-core/src/test.rs`:
- Change `TestResponse` to hold collected fields instead of the raw `Response`:
```rust
/// The response captured by the in-process [`TestClient`]. The body is fully
/// collected (streamed bodies are drained), so the accessors are synchronous.
pub struct TestResponse {
    status: http::StatusCode,
    headers: HeaderMap,
    body: Bytes,
}
```
(Add `use http::HeaderMap;` and ensure `use bytes::Bytes;` are imported at the top of `test.rs`.)
- Change `send` to collect the body:
```rust
    pub async fn send(self) -> TestResponse {
        let uri = self.uri.parse::<Uri>().expect("valid URI");
        let res = self
            .client
            .app
            .process(self.method, uri, self.headers, self.body)
            .await;
        let status = res.status;
        let headers = res.headers;
        let body = res.body.into_bytes().await.unwrap_or_default();
        TestResponse { status, headers, body }
    }
```
- Update the accessors to read the collected fields:
```rust
    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
    pub fn body_bytes(&self) -> &Bytes {
        &self.body
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
```
(Keep `StatusCode` imported as before. Remove the now-unused `Response` import from `test.rs` if it triggers an unused-import warning.)

- [ ] **Step 2: JSON error middleware rewrites only buffered bodies**

In `churust-json/src/lib.rs`, the `JsonErrors` middleware currently reads `&res.body`. Replace the body-rewrite block so it only rewrites buffered bodies (streamed bodies pass through). Change:
```rust
        if is_error && is_text {
            let msg = String::from_utf8_lossy(&res.body).into_owned();
            ...
            res.body = Bytes::from(bytes);
            ...
        }
```
to:
```rust
        if is_error && is_text {
            if let Some(buffered) = res.body.as_bytes() {
                let msg = String::from_utf8_lossy(buffered).into_owned();
                let body = serde_json::json!({ "error": msg, "status": res.status.as_u16() });
                let bytes = if self.pretty {
                    serde_json::to_vec_pretty(&body)
                } else {
                    serde_json::to_vec(&body)
                }
                .unwrap_or_default();
                res.body = churust_core::Body::from(bytes);
                res.headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            }
        }
```
(`churust_core::Body` is re-exported from core — see Task 7 Step 2; it is already accessible as `churust_core::Body` once Task 1 exports it. The `Bytes` import in `churust-json` may now be unused — remove it if clippy flags it, since `Body::from(Vec<u8>)` is used.)

- [ ] **Step 3: ws 101 response uses `Body::empty()`**

In `churust-core/src/ws.rs`, the `on_upgrade` 101 response is built with `Response::new(StatusCode::SWITCHING_PROTOCOLS)`, which already yields an empty `Body` — no change needed. (If any code in `ws.rs` set `res.body = Bytes::...` directly, change it to `churust_core::Body`/`crate::Body::empty()`; it does not.)

- [ ] **Step 4: Checkpoint**

Run:
```
cargo test -p churust-core
cargo test -p churust-core --features ws
cargo test -p churust-json
```
Expected: PASS.

---

## Task 5: Streaming integration test + TestClient streamed-collect test

**Files:**
- Create: `churust-core/tests/streaming.rs`
- Modify: `churust-core/src/test.rs` (add a streamed-collect unit test)

- [ ] **Step 1: TestClient collects a streamed response**

Add to `churust-core/src/test.rs` `mod tests`:
```rust
    #[tokio::test]
    async fn collects_streamed_body() {
        use crate::body::Body;
        use crate::{Call, Churust};
        use bytes::Bytes;

        let app = Churust::server()
            .routing(|r| {
                r.get("/stream", |_c: Call| async {
                    let chunks = futures_util::stream::iter(vec![
                        Ok::<_, std::io::Error>(Bytes::from("foo")),
                        Ok(Bytes::from("bar")),
                    ]);
                    crate::Response::stream("text/plain", Body::from_stream(chunks))
                });
            })
            .build();
        let res = TestClient::new(app).get("/stream").send().await;
        assert_eq!(res.status(), http::StatusCode::OK);
        assert_eq!(res.text(), "foobar");
    }
```

- [ ] **Step 2: Streamed response over a real socket**

`churust-core/tests/streaming.rs`:
```rust
use churust_core::{Body, Call, Churust, Response};
use std::time::Duration;

#[tokio::test]
async fn streamed_response_reaches_client_in_full() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/big", |_c: Call| async {
                let chunks = futures_util::stream::iter(
                    (0..5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("chunk{i};")))),
                );
                Response::stream("text/plain", Body::from_stream(chunks))
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

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET /big HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    assert!(
        text.contains("chunk0;chunk1;chunk2;chunk3;chunk4;"),
        "streamed body incomplete: {text}"
    );

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core --test streaming` then `cargo test -p churust-core test::collects_streamed_body`.
Expected: PASS.

---

## Task 6: `fs` feature — `StaticFiles`

**Files:**
- Modify: `churust-core/Cargo.toml`
- Create: `churust-core/src/fs.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Add the feature**

In `churust-core/Cargo.toml` `[features]` add (alongside `tls`, `ws`):
```toml
fs = []
```
(No new dependencies — uses `tokio::fs`, `futures-util`, `bytes`, all already present.)

- [ ] **Step 2: Write `StaticFiles` + MIME map + tests**

`churust-core/src/fs.rs`:
```rust
//! Static file serving (feature `fs`).
//!
//! Register [`StaticFiles`] on a wildcard route to stream files from a
//! directory:
//!
//! ```no_run
//! use churust_core::{Churust, fs::StaticFiles};
//!
//! # fn build() {
//! Churust::server().routing(|r| {
//!     r.get("/assets/{path...}", StaticFiles::dir("./public").index("index.html").handler());
//! });
//! # }
//! ```

use crate::body::Body;
use crate::call::Call;
use crate::error::{Error, Result};
use crate::handler::Handler;
use crate::response::Response;
use bytes::Bytes;
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

/// Serves files from a directory. Build with [`StaticFiles::dir`], then mount
/// its [`handler`](StaticFiles::handler) on a `{path...}` wildcard route.
#[derive(Debug, Clone)]
pub struct StaticFiles {
    root: PathBuf,
    index: Option<String>,
}

impl StaticFiles {
    /// Serve files rooted at `root`.
    pub fn dir(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), index: None }
    }

    /// Serve `<dir>/<index>` when a request resolves to a directory.
    pub fn index(mut self, file: impl Into<String>) -> Self {
        self.index = Some(file.into());
        self
    }

    /// Turn this into a [`Handler`] for a wildcard route. The handler reads the
    /// route's single captured path parameter as the relative path.
    pub fn handler(self) -> impl Handler {
        let cfg = self;
        move |call: Call| {
            let cfg = cfg.clone();
            async move { cfg.serve(&call).await }
        }
    }

    async fn serve(&self, call: &Call) -> Result<Response> {
        // The relative path is the route's (single) captured param value.
        let rel = call
            .params_iter()
            .map(|(_, v)| v.to_string())
            .next()
            .unwrap_or_default();

        let safe = sanitize(&rel).ok_or_else(|| Error::not_found("not found"))?;
        let mut path = self.root.join(safe);

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        if meta.is_dir() {
            match &self.index {
                Some(index) => path = path.join(index),
                None => return Err(Error::not_found("not found")),
            }
        }

        // Symlink-escape guard: the canonical path must stay within root.
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        let canonical_root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|_| Error::internal("static root does not exist"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(Error::not_found("not found"));
        }

        let file = tokio::fs::File::open(&canonical)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        let content_type = content_type_for(&canonical);
        Ok(Response::stream(content_type, Body::from_stream(file_stream(file))))
    }
}

/// Reject path traversal: no `..`, no root/prefix components. Returns a relative
/// `PathBuf` of plain normal components, or `None` if unsafe.
fn sanitize(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            // `..`, `/`, `C:\`, etc. are all rejected.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Stream a file in 64 KiB chunks (no `tokio-util` dependency).
fn file_stream(
    file: tokio::fs::File,
) -> impl futures_util::stream::Stream<Item = std::result::Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold((file, false), |(mut file, done)| async move {
        if done {
            return None;
        }
        let mut buf = vec![0u8; 64 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), (file, false)))
            }
            Err(e) => Some((Err(e), (file, true))),
        }
    })
}

/// Built-in extension → `Content-Type` map (not exhaustive). Falls back to
/// `application/octet-stream`.
fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Churust, TestClient};
    use http::StatusCode;

    fn temp_dir_with_files() -> PathBuf {
        // Unique-enough dir under the OS temp dir (no extra deps).
        let base = std::env::temp_dir().join(format!("churust-fs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(base.join("sub"));
        std::fs::write(base.join("hello.txt"), b"hello world").unwrap();
        std::fs::write(base.join("page.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(base.join("index.html"), b"INDEX").unwrap();
        base
    }

    fn app(root: PathBuf) -> crate::App {
        Churust::server()
            .routing(move |r| {
                r.get(
                    "/files/{path...}",
                    StaticFiles::dir(root.clone()).index("index.html").handler(),
                );
            })
            .build()
    }

    #[tokio::test]
    async fn serves_file_with_content_type() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root)).get("/files/hello.txt").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.header("content-type"), Some("text/plain; charset=utf-8"));
        assert_eq!(res.text(), "hello world");
    }

    #[tokio::test]
    async fn html_content_type() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root)).get("/files/page.html").send().await;
        assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    }

    #[tokio::test]
    async fn missing_file_is_404() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root)).get("/files/nope.txt").send().await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn directory_serves_index() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root)).get("/files/sub").send().await;
        // sub/ has no index.html, so 404; root resolves index on "" path:
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize("../etc/passwd").is_none());
        assert!(sanitize("/etc/passwd").is_none());
        assert!(sanitize("a/../../b").is_none());
        assert_eq!(sanitize("css/app.css"), Some(PathBuf::from("css/app.css")));
        assert_eq!(sanitize("./x.txt"), Some(PathBuf::from("x.txt")));
    }
}
```

- [ ] **Step 3: Wire the module**

In `churust-core/src/lib.rs` add:
```rust
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "fs")]
pub use fs::StaticFiles;
```

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-core --features fs`
Then: `cargo clippy -p churust-core --all-targets --features fs -- -D warnings`
Expected: PASS / clean.

---

## Task 7: Umbrella feature, prelude, example, and full matrix

**Files:**
- Modify: `churust/Cargo.toml`
- Modify: `churust/src/lib.rs`
- Create: `examples/static/Cargo.toml`, `examples/static/src/main.rs`
- Modify: `Cargo.toml` (members += `examples/static`)

- [ ] **Step 1: Umbrella feature**

In `churust/Cargo.toml` `[features]` add:
```toml
fs = ["churust-core/fs"]
```

- [ ] **Step 2: Re-exports + prelude**

In `churust/src/lib.rs`:
- `Body` is always available via `pub use churust_core::*;` — confirm it is re-exported (it is, through the glob). Add an explicit doc-bearing re-export is unnecessary.
- Add the `fs` re-export after the `ws` one:
```rust
/// Static file serving (`StaticFiles`). Enabled by the `fs` feature.
#[cfg(feature = "fs")]
pub use churust_core::fs;
```
- In `prelude`, add:
```rust
    #[cfg(feature = "fs")]
    pub use churust_core::fs::StaticFiles;
```

- [ ] **Step 3: `examples/static`**

In root `Cargo.toml`, add `"examples/static"` to `members`.

`examples/static/Cargo.toml`:
```toml
[package]
name = "static-example"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
churust = { path = "../../churust", features = ["fs"] }
tokio = { workspace = true }
bytes = { workspace = true }
futures-util = { workspace = true }
```

`examples/static/src/main.rs`:
```rust
use churust::prelude::*;
use churust::Body;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .routing(|r| {
            // Serve files from ./public (create it with an index.html to try).
            r.get(
                "/{path...}",
                StaticFiles::dir("./public").index("index.html").handler(),
            );
            // A streamed dynamic response.
            r.get("/numbers", |_c: Call| async {
                let chunks = futures_util::stream::iter(
                    (1..=5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("{i}\n")))),
                );
                Response::stream("text/plain", Body::from_stream(chunks))
            });
        })
        .start()
        .await
}
```

- [ ] **Step 4: Full-suite + clippy matrix**

Run each and confirm green:
```
cargo test --workspace
cargo test -p churust-core --features fs
cargo test -p churust-core --features ws
cargo test -p churust --features full
cargo build -p static-example
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p churust-core --all-targets --features fs -- -D warnings
cargo clippy -p churust-core --all-targets --features ws -- -D warnings
cargo clippy -p churust --all-targets --features full -- -D warnings
```
Expected: all PASS / clean.

- [ ] **Step 5: README (optional but recommended)**

Add a short "Static files & streaming (feature `fs`)" subsection to `README.md` (~10 lines) showing `StaticFiles::dir(...).handler()` and a `Response::stream(...)` example, and update the Status section to mention v2.1.

---

## Self-Review

**Spec coverage (vs `2026-06-06-churust-streaming-static-design.md`):**
- §4.1 `Body` enum + constructors + `as_bytes`/`into_bytes`/`is_empty`/`from_stream` → Task 1. ✓
- §4.2 `Response.body: Body`, constructors unchanged, `Response::stream` → Task 2. ✓ (`Response::body(Body)` helper from the spec is subsumed by `stream`/`From`; not separately needed — YAGNI.)
- §4.3 engine emits `BoxBody` (Full/StreamBody) → Task 3. ✓
- §4.4 ripple: JSON middleware `as_bytes`, TestClient collect, ws empty → Task 4. ✓
- §4.5 `StaticFiles` (`fs`): traversal guard, content-type map, index, 404, file stream, no `tokio-util` → Task 6. ✓
- §4.6 umbrella feature + prelude + example → Task 7. ✓
- §8 testing: Body units (T1), engine streaming integration + TestClient collect (T5), static-files incl. traversal (T6), default build untouched (T2/T7). ✓
- §7 security: traversal rejected pre-FS-access + symlink canonical-prefix check → Task 6. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to". The `directory_serves_index` test asserts 404 (sub/ lacks an index) with an explanatory comment — it is a real assertion, not a placeholder. Version/ordering notes are guidance beside complete code.

**Type consistency:**
- `Body` variants + `from_stream`/`as_bytes`/`as_slice`/`is_empty`/`into_bytes` (T1) used by `Response::stream` (T2), engine `into_boxed_body` (T3), TestClient (T4), JSON middleware (T4), `StaticFiles` (T6), tests (T5/T6). ✓
- `impl PartialEq<Bytes> for Body` (T1) keeps `assert_eq!(res.body, Bytes::from(..))` unit tests compiling (T2 leaves them untouched). ✓
- `Response.body: Body` (T2) read via `into_bytes` (T4 TestClient), `as_bytes` (T4 JSON), `into_boxed_body` (T3). ✓
- `into_boxed_body(Body) -> BoxBody<Bytes, io::Error>` (T3) matches `handle`'s return type (T3). ✓
- `StaticFiles::dir(...).index(...).handler() -> impl Handler` (T6) matches route registration in tests + example (T6/T7). ✓
- Dropping `Response: Clone` (T2): no `Response` is cloned anywhere (verified by grep — only `app.clone()` exists). ✓

**Risk notes:**
- `Full::new(b).map_err(|never| match never {})`: `Full`'s body error is `Infallible`; the empty match converts it. If the resolved `http-body-util` uses a different never-type spelling, adapt (`.map_err(|e| match e {})` or `Infallible`).
- `StreamBody::new` expects a stream of `Result<Frame<Bytes>, E>`; the engine maps `Result<Bytes>` → `Result<Frame<Bytes>, io::Error>`. Confirm `Frame::data` is the constructor in the resolved version.
- Static-file tests write to `std::env::temp_dir()` under a pid-named subdir; they read-only assert and don't clean up (acceptable for CI). If a stricter isolation is wanted, the implementer may switch to a `tempfile` dev-dep — but the plan avoids the dep.
- All `fs` code is feature-gated; the `Body` change is the only always-on change. Task 2/Task 7 re-verify the default build + full existing matrix.

---

## Execution Handoff

Execute sequentially, task by task: implement, then spec review, then quality review. **Tasks 2 and 3 must land together** (Task 2 leaves the engine temporarily referencing the old body type until Task 3 fixes it) — the spec reviewer for Task 2 should treat "engine.rs type errors only" as expected and gate on Task 3's checkpoint.
