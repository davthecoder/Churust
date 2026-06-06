# Churust Core Kernel — Implementation Plan (Plan 1 of 3)

**Goal:** Build the Churust core kernel — a working, Ktor-inspired Rust HTTP server with a routing DSL, a phased onion middleware pipeline, an `install(plugin)` hook, panic isolation, and an in-process test harness.

**Architecture:** Built on tokio + hyper (HTTP/1.1). A request becomes an owned `Call` context that flows through an owned `Next` chain (middleware onion) into a trie `Router` that dispatches to a handler returning `impl IntoResponse`. The exact same `App::process()` path is used by both the hyper engine and the in-process `TestClient`, so everything is testable without binding a socket.

**Tech Stack:** Rust 2021 (MSRV 1.96), tokio, hyper 1.x, hyper-util, http-body-util, http, bytes, futures-util, async-trait.

**Key design decisions (refinement of the spec, same concepts):**
- Handlers receive **owned `Call`** and return `Result<R: IntoResponse>`. Owned (not `&mut`) avoids the "async fn borrowing its arg through a trait object" lifetime trap → clean, robust, compiles. The single-`call`-context Ktor feel is preserved.
- `Next` is **owned** (clones cheap `Arc`s, no lifetime params) so `async-trait` middleware composes without lifetime gymnastics.
- `respond_*` / `Response` builders are **sync** (body is in memory); only body-reading (`receive_*`) is `async`.
- Plan 1 is plaintext HTTP/1.1. TLS + `churust.toml` config land in Plan 2.

---

## File Structure

```
Cargo.toml                         # [workspace]
churust-core/
  Cargo.toml
  src/lib.rs                       # re-exports, module wiring
  src/error.rs                     # Error, Result
  src/response.rs                  # Response, IntoResponse
  src/call.rs                      # Call context (request + response staging)
  src/router.rs                    # trie Router, PathPattern, Params, RouteBuilder DSL
  src/handler.rs                   # Handler trait + closure blanket impl
  src/pipeline.rs                  # Middleware trait, Next (owned onion)
  src/app.rs                       # App, AppBuilder, Plugin trait, process()
  src/engine.rs                    # hyper/tokio serve loop, graceful shutdown
  src/test.rs                      # TestClient in-process harness
churust/
  Cargo.toml                       # umbrella; re-exports core + prelude
  src/lib.rs
examples/
  hello/Cargo.toml
  hello/src/main.rs
```

Responsibilities: one file = one concern. `app.rs` owns assembly + the single request entry point `process()`; `engine.rs` only adapts hyper↔`process()`; `test.rs` only adapts a fake request↔`process()`.

---

## Task 1: Workspace skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `churust-core/Cargo.toml`
- Create: `churust-core/src/lib.rs`

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["churust-core", "churust", "examples/hello"]

[workspace.package]
edition = "2021"
rust-version = "1.96"
license = "MIT"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
hyper = { version = "1", features = ["http1", "server"] }
hyper-util = { version = "0.1", features = ["tokio", "server", "server-graceful"] }
http-body-util = "0.1"
http = "1"
bytes = "1"
futures-util = "0.3"
async-trait = "0.1"
```

- [ ] **Step 2: Create the core crate manifest**

`churust-core/Cargo.toml`:
```toml
[package]
name = "churust-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio.workspace = true
hyper.workspace = true
hyper-util.workspace = true
http-body-util.workspace = true
http.workspace = true
bytes.workspace = true
futures-util.workspace = true
async-trait.workspace = true
```

- [ ] **Step 3: Create a placeholder lib so the workspace builds**

`churust-core/src/lib.rs`:
```rust
//! Churust core kernel.

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

> Note: `churust/` and `examples/hello/` members are created in Tasks 11–12. Until then, temporarily set `members = ["churust-core"]` in `Cargo.toml`, then restore the full list in Task 11.

- [ ] **Step 4: Pin exact dependency versions**

Run: `cd churust-core && cargo add tokio hyper hyper-util http-body-util http bytes futures-util async-trait 2>/dev/null; cd ..`
(If the workspace deps already resolve, skip. Confirm latest 1.x / 0.1 lines resolve.)

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core`
Expected: PASS (`workspace_builds`).

---

## Task 2: Error type

**Files:**
- Create: `churust-core/src/error.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `churust-core/src/error.rs`:
```rust
use http::StatusCode;
use std::fmt;

/// Crate-wide result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A handler/framework error carrying an HTTP status.
#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Error {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), source: None }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
    pub fn with_source(
        mut self,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self
    }
    pub fn status(&self) -> StatusCode { self.status }
    pub fn message(&self) -> &str { &self.message }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|s| s.as_ref() as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_status_and_message() {
        let e = Error::not_found("nope");
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        assert_eq!(e.message(), "nope");
    }

    #[test]
    fn display_includes_status() {
        let e = Error::bad_request("bad");
        assert!(format!("{e}").contains("400"));
    }
}
```

- [ ] **Step 2: Wire the module + run to verify it fails first**

In `churust-core/src/lib.rs` add at top:
```rust
pub mod error;
pub use error::{Error, Result};
```
Run: `cargo test -p churust-core error::` — Expected at first authoring: compile error / FAIL until the file is saved, then PASS.

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core`
Expected: PASS.

---

## Task 3: Response + IntoResponse

**Files:**
- Create: `churust-core/src/response.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the type, builders, trait, and tests**

`churust-core/src/response.rs`:
```rust
use crate::error::Error;
use bytes::Bytes;
use http::header::{HeaderName, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, StatusCode};

/// A fully-buffered HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Response {
    pub fn new(status: StatusCode) -> Self {
        Self { status, headers: HeaderMap::new(), body: Bytes::new() }
    }

    pub fn text(body: impl Into<String>) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
        r.body = Bytes::from(body.into());
        r
    }

    pub fn bytes(content_type: &'static str, body: impl Into<Bytes>) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        r.body = body.into();
        r
    }

    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }
}

/// Convert a handler return value into a `Response`.
pub trait IntoResponse {
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response { self }
}

impl IntoResponse for () {
    fn into_response(self) -> Response { Response::new(StatusCode::OK) }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response { Response::text(self) }
}

impl IntoResponse for String {
    fn into_response(self) -> Response { Response::text(self) }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response { Response::new(self) }
}

impl<T: IntoResponse> IntoResponse for (StatusCode, T) {
    fn into_response(self) -> Response {
        let (status, inner) = self;
        inner.into_response().with_status(status)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        Response::text(self.message().to_string()).with_status(self.status())
    }
}

/// A `Result` whose `Ok`/`Err` both render to a response.
impl<T: IntoResponse> IntoResponse for crate::error::Result<T> {
    fn into_response(self) -> Response {
        match self {
            Ok(v) => v.into_response(),
            Err(e) => e.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sets_content_type_and_body() {
        let r = Response::text("hi");
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.body, Bytes::from("hi"));
        assert_eq!(r.headers.get(CONTENT_TYPE).unwrap(), "text/plain; charset=utf-8");
    }

    #[test]
    fn status_tuple_overrides_status() {
        let r = (StatusCode::CREATED, "made").into_response();
        assert_eq!(r.status, StatusCode::CREATED);
        assert_eq!(r.body, Bytes::from("made"));
    }

    #[test]
    fn error_renders_with_its_status() {
        let r = Error::bad_request("x").into_response();
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod response;
pub use response::{IntoResponse, Response};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core response::`
Expected: PASS (3 tests).

---

## Task 4: Call context

**Files:**
- Create: `churust-core/src/call.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the Call type + tests**

`churust-core/src/call.rs`:
```rust
use crate::error::{Error, Result};
use crate::response::Response;
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use std::collections::HashMap;

/// Per-request context: the single object a handler receives (Ktor-style).
#[derive(Debug)]
pub struct Call {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    params: HashMap<String, String>,
    body: Bytes,
}

impl Call {
    /// Construct a Call from already-parsed request parts (used by the engine
    /// and the test harness alike).
    pub fn new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Self { method, uri, headers, params: HashMap::new(), body }
    }

    pub fn method(&self) -> &Method { &self.method }
    pub fn uri(&self) -> &Uri { &self.uri }
    pub fn path(&self) -> &str { self.uri.path() }
    pub fn headers(&self) -> &HeaderMap { &self.headers }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Raw query string (without `?`).
    pub fn query_string(&self) -> &str {
        self.uri.query().unwrap_or("")
    }

    /// First value for a query key.
    pub fn query(&self, key: &str) -> Option<String> {
        form_urlencoded_first(self.query_string(), key)
    }

    /// Set by the router after a successful match.
    pub(crate) fn set_params(&mut self, params: HashMap<String, String>) {
        self.params = params;
    }

    /// Raw path parameter value.
    pub fn param_raw(&self, name: &str) -> Option<&str> {
        self.params.get(name).map(|s| s.as_str())
    }

    /// Parsed path parameter; 400 on missing or unparseable.
    pub fn param<T>(&self, name: &str) -> Result<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        let raw = self
            .param_raw(name)
            .ok_or_else(|| Error::bad_request(format!("missing path param `{name}`")))?;
        raw.parse::<T>()
            .map_err(|e| Error::bad_request(format!("bad path param `{name}`: {e}")))
    }

    /// Take the request body as bytes (async to allow future streaming).
    pub async fn receive_bytes(&mut self) -> Bytes {
        std::mem::take(&mut self.body)
    }

    /// Take the request body as a UTF-8 string; 400 if not valid UTF-8.
    pub async fn receive_text(&mut self) -> Result<String> {
        let bytes = self.receive_bytes().await;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::bad_request(format!("body is not valid UTF-8: {e}")))
    }

    // ---- response convenience (sync; body is in memory) ----

    pub fn respond_text(&self, body: impl Into<String>) -> Response {
        Response::text(body)
    }

    pub fn respond_status(&self, status: StatusCode) -> Response {
        Response::new(status)
    }
}

fn form_urlencoded_first(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        if k == key {
            let v = it.next().unwrap_or("");
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    // Minimal `+` and %XX decoding sufficient for v1 query parsing.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => { out.push(b' '); i += 1; }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => { out.push((h * 16 + l) as u8); i += 3; }
                    _ => { out.push(bytes[i]); i += 1; }
                }
            }
            b => { out.push(b); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(path: &str, body: &str) -> Call {
        Call::new(
            Method::GET,
            path.parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::from(body.to_string()),
        )
    }

    #[test]
    fn reads_method_and_path() {
        let c = call("/a/b?x=1", "");
        assert_eq!(c.method(), &Method::GET);
        assert_eq!(c.path(), "/a/b");
    }

    #[test]
    fn parses_query() {
        let c = call("/s?q=hello+world&n=5", "");
        assert_eq!(c.query("q").as_deref(), Some("hello world"));
        assert_eq!(c.query("n").as_deref(), Some("5"));
        assert_eq!(c.query("missing"), None);
    }

    #[test]
    fn parses_path_param() {
        let mut c = call("/users/42", "");
        let mut p = HashMap::new();
        p.insert("id".to_string(), "42".to_string());
        c.set_params(p);
        let id: u64 = c.param("id").unwrap();
        assert_eq!(id, 42);
        assert!(c.param::<u64>("missing").is_err());
    }

    #[tokio::test]
    async fn receives_text_body() {
        let mut c = call("/", "payload");
        assert_eq!(c.receive_text().await.unwrap(), "payload");
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod call;
pub use call::Call;
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core call::`
Expected: PASS (4 tests).

---

## Task 5: Handler trait

**Files:**
- Create: `churust-core/src/handler.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the trait, blanket impl, and test**

`churust-core/src/handler.rs`:
```rust
use crate::call::Call;
use crate::response::{IntoResponse, Response};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A boxed future returned by a handler. `'static` because handlers own their
/// `Call` (it is moved in), so nothing is borrowed across the await point.
pub type HandlerFuture = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

/// Anything that can handle a `Call` and produce a `Response`.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, call: Call) -> HandlerFuture;
}

/// Blanket impl: any async closure `Fn(Call) -> impl Future<Output = impl IntoResponse>`.
///
/// Because `Call` is taken by value, the returned future is a single concrete
/// `'static` type — no lifetime parameter — so this impl compiles cleanly.
impl<F, Fut, R> Handler for F
where
    F: Fn(Call) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + 'static,
{
    fn handle(&self, call: Call) -> HandlerFuture {
        let fut = self(call);
        Box::pin(async move { fut.await.into_response() })
    }
}

/// Type-erased shared handler stored in the router.
pub type BoxHandler = Arc<dyn Handler>;

pub fn boxed<H: Handler>(h: H) -> BoxHandler {
    Arc::new(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, Method, StatusCode, Uri};

    fn sample_call() -> Call {
        Call::new(Method::GET, "/".parse::<Uri>().unwrap(), HeaderMap::new(), Bytes::new())
    }

    #[tokio::test]
    async fn closure_returning_str_is_a_handler() {
        let h = boxed(|_call: Call| async move { "hello" });
        let res = h.handle(sample_call()).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, Bytes::from("hello"));
    }

    #[tokio::test]
    async fn closure_returning_result_is_a_handler() {
        let h = boxed(|call: Call| async move {
            let _ = call.path();
            Ok::<_, crate::Error>((StatusCode::CREATED, "made"))
        });
        let res = h.handle(sample_call()).await;
        assert_eq!(res.status, StatusCode::CREATED);
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod handler;
pub use handler::{boxed, BoxHandler, Handler};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core handler::`
Expected: PASS (2 tests).

---

## Task 6: Router (trie) + Routing DSL

**Files:**
- Create: `churust-core/src/router.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the trie router, DSL, and tests**

`churust-core/src/router.rs`:
```rust
use crate::call::Call;
use crate::handler::{boxed, BoxHandler, Handler};
use crate::response::Response;
use http::{Method, StatusCode};
use std::collections::HashMap;

/// Outcome of routing a request.
pub enum Match {
    Found { handler: BoxHandler, params: HashMap<String, String> },
    MethodNotAllowed { allow: Vec<Method> },
    NotFound,
}

#[derive(Default)]
struct Node {
    statics: HashMap<String, Node>,
    param: Option<(String, Box<Node>)>,      // {name}
    wildcard: Option<(String, BoxHandlers)>, // {name...} terminal
    handlers: BoxHandlers,
}

#[derive(Default)]
struct BoxHandlers(HashMap<Method, BoxHandler>);

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node").finish_non_exhaustive()
    }
}

/// Compiled router.
#[derive(Debug, Default)]
pub struct Router {
    root: Node,
}

impl Router {
    pub fn new() -> Self { Self::default() }

    /// Insert a handler. `pattern` is the FULL path, e.g. `/users/{id}`.
    pub fn add(&mut self, method: Method, pattern: &str, handler: BoxHandler) {
        let mut node = &mut self.root;
        let segments: Vec<&str> = split_segments(pattern);
        for (i, seg) in segments.iter().enumerate() {
            if let Some(name) = seg.strip_suffix("...").and_then(|s| s.strip_prefix('{')) {
                // wildcard must be terminal
                assert!(i == segments.len() - 1, "wildcard `{{{name}...}}` must be last segment");
                let entry = node.wildcard.get_or_insert_with(|| (name.to_string(), BoxHandlers::default()));
                entry.1 .0.insert(method, handler);
                return;
            } else if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                let entry = node.param.get_or_insert_with(|| (name.to_string(), Box::new(Node::default())));
                node = entry.1.as_mut();
            } else {
                node = node.statics.entry((*seg).to_string()).or_default();
            }
        }
        node.handlers.0.insert(method, handler);
    }

    pub fn route(&self, method: &Method, path: &str) -> Match {
        let segments = split_segments(path);
        let mut params = HashMap::new();
        match Self::walk(&self.root, &segments, 0, &mut params) {
            Some(node) => match node.handlers.0.get(method) {
                Some(h) => Match::Found { handler: h.clone(), params },
                None if node.handlers.0.is_empty() => Match::NotFound,
                None => Match::MethodNotAllowed { allow: node.handlers.0.keys().cloned().collect() },
            },
            None => {
                // try wildcard at the deepest matchable ancestor
                if let Some(m) = Self::walk_wildcard(&self.root, &segments, 0, method, &mut params) {
                    m
                } else {
                    Match::NotFound
                }
            }
        }
    }

    fn walk<'a>(
        node: &'a Node,
        segs: &[&str],
        i: usize,
        params: &mut HashMap<String, String>,
    ) -> Option<&'a Node> {
        if i == segs.len() {
            return Some(node);
        }
        let seg = segs[i];
        if let Some(child) = node.statics.get(seg) {
            if let Some(n) = Self::walk(child, segs, i + 1, params) {
                return Some(n);
            }
        }
        if let Some((name, child)) = &node.param {
            params.insert(name.clone(), seg.to_string());
            if let Some(n) = Self::walk(child, segs, i + 1, params) {
                return Some(n);
            }
            params.remove(name);
        }
        None
    }

    fn walk_wildcard(
        node: &Node,
        segs: &[&str],
        i: usize,
        method: &Method,
        params: &mut HashMap<String, String>,
    ) -> Option<Match> {
        if let Some((name, handlers)) = &node.wildcard {
            let rest = segs[i..].join("/");
            params.insert(name.clone(), rest);
            return Some(match handlers.0.get(method) {
                Some(h) => Match::Found { handler: h.clone(), params: std::mem::take(params) },
                None => Match::MethodNotAllowed { allow: handlers.0.keys().cloned().collect() },
            });
        }
        if i < segs.len() {
            if let Some(child) = node.statics.get(segs[i]) {
                if let Some(m) = Self::walk_wildcard(child, segs, i + 1, method, params) {
                    return Some(m);
                }
            }
            if let Some((pname, child)) = &node.param {
                params.insert(pname.clone(), segs[i].to_string());
                if let Some(m) = Self::walk_wildcard(child, segs, i + 1, method, params) {
                    return Some(m);
                }
                params.remove(pname);
            }
        }
        None
    }
}

fn split_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Builder used inside `routing(|r| { ... })`. Tracks a path prefix so nested
/// `route("/x", |r| ...)` scopes compose.
pub struct RouteBuilder<'r> {
    router: &'r mut Router,
    prefix: String,
}

impl<'r> RouteBuilder<'r> {
    pub(crate) fn new(router: &'r mut Router) -> Self {
        Self { router, prefix: String::new() }
    }

    fn full(&self, path: &str) -> String {
        let mut p = self.prefix.clone();
        if !path.starts_with('/') { p.push('/'); }
        p.push_str(path);
        p
    }

    pub fn method<H: Handler>(&mut self, method: Method, path: &str, handler: H) -> &mut Self {
        let full = self.full(path);
        self.router.add(method, &full, boxed(handler));
        self
    }

    pub fn get<H: Handler>(&mut self, path: &str, handler: H) -> &mut Self {
        self.method(Method::GET, path, handler)
    }
    pub fn post<H: Handler>(&mut self, path: &str, handler: H) -> &mut Self {
        self.method(Method::POST, path, handler)
    }
    pub fn put<H: Handler>(&mut self, path: &str, handler: H) -> &mut Self {
        self.method(Method::PUT, path, handler)
    }
    pub fn delete<H: Handler>(&mut self, path: &str, handler: H) -> &mut Self {
        self.method(Method::DELETE, path, handler)
    }

    /// Nested scope: all routes inside get `path` prepended.
    pub fn route(&mut self, path: &str, f: impl FnOnce(&mut RouteBuilder)) -> &mut Self {
        let mut child = RouteBuilder { router: self.router, prefix: self.full(path) };
        f(&mut child);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, Uri};

    fn build() -> Router {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/", |_c: Call| async { "root" });
            b.route("/users", |b| {
                b.get("/{id}", |c: Call| async move {
                    format!("user {}", c.param_raw("id").unwrap())
                });
                b.post("/", |_c: Call| async { (StatusCode::CREATED, "created") });
            });
            b.get("/files/{path...}", |c: Call| async move {
                format!("file {}", c.param_raw("path").unwrap())
            });
        }
        r
    }

    fn run(r: &Router, m: Method, path: &str) -> Match {
        r.route(&m, path)
    }

    #[tokio::test]
    async fn matches_static_and_param() {
        let r = build();
        match run(&r, Method::GET, "/users/7") {
            Match::Found { handler, params } => {
                assert_eq!(params.get("id").unwrap(), "7");
                let mut c = Call::new(Method::GET, "/users/7".parse::<Uri>().unwrap(), HeaderMap::new(), Bytes::new());
                c.set_params(params);
                let res = handler.handle(c).await;
                assert_eq!(res.body, Bytes::from("user 7"));
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn unknown_path_is_not_found() {
        let r = build();
        assert!(matches!(run(&r, Method::GET, "/nope"), Match::NotFound));
    }

    #[test]
    fn known_path_wrong_method_is_405() {
        let r = build();
        match run(&r, Method::DELETE, "/users/7") {
            Match::MethodNotAllowed { allow } => assert!(allow.contains(&Method::GET)),
            _ => panic!("expected 405"),
        }
    }

    #[test]
    fn wildcard_captures_rest() {
        let r = build();
        match run(&r, Method::GET, "/files/a/b/c.txt") {
            Match::Found { params, .. } => assert_eq!(params.get("path").unwrap(), "a/b/c.txt"),
            _ => panic!("expected wildcard Found"),
        }
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod router;
pub use router::{Match, RouteBuilder, Router};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core router::`
Expected: PASS (4 tests).

---

## Task 7: Pipeline (Middleware + owned Next onion)

**Files:**
- Create: `churust-core/src/pipeline.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the Middleware trait, Next, and tests**

`churust-core/src/pipeline.rs`:
```rust
use crate::call::Call;
use crate::response::Response;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The terminal of the pipeline: routes the call and runs the matched handler.
/// Returns a `'static` boxed future (Call is owned/moved in).
pub type Endpoint =
    Arc<dyn Fn(Call) -> Pin<Box<dyn Future<Output = Response> + Send + 'static>> + Send + Sync>;

/// A pipeline interceptor. Owns the call, may inspect/modify it, calls
/// `next.run(call)` to proceed inward, and may post-process the `Response`.
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    async fn handle(&self, call: Call, next: Next) -> Response;
}

/// The remaining chain. Owned (Arc clones) — no lifetime params, so it threads
/// cleanly through `async_trait` futures.
pub struct Next {
    chain: VecDeque<Arc<dyn Middleware>>,
    endpoint: Endpoint,
}

impl Next {
    pub(crate) fn new(chain: VecDeque<Arc<dyn Middleware>>, endpoint: Endpoint) -> Self {
        Self { chain, endpoint }
    }

    /// Proceed to the next layer, or the endpoint if the chain is exhausted.
    pub async fn run(mut self, call: Call) -> Response {
        match self.chain.pop_front() {
            Some(mw) => mw.handle(call, self).await,
            None => (self.endpoint)(call).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::IntoResponse;
    use bytes::Bytes;
    use http::header::HeaderName;
    use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};

    fn sample_call() -> Call {
        Call::new(Method::GET, "/".parse::<Uri>().unwrap(), HeaderMap::new(), Bytes::new())
    }

    fn endpoint() -> Endpoint {
        Arc::new(|_call: Call| Box::pin(async { "inner".into_response() }) as _)
    }

    struct AddHeader;
    #[async_trait]
    impl Middleware for AddHeader {
        async fn handle(&self, call: Call, next: Next) -> Response {
            let mut res = next.run(call).await;
            res.headers.insert(
                HeaderName::from_static("x-mw"),
                HeaderValue::from_static("1"),
            );
            res
        }
    }

    struct ShortCircuit;
    #[async_trait]
    impl Middleware for ShortCircuit {
        async fn handle(&self, _call: Call, _next: Next) -> Response {
            Response::new(StatusCode::FORBIDDEN)
        }
    }

    #[tokio::test]
    async fn runs_endpoint_when_chain_empty() {
        let next = Next::new(VecDeque::new(), endpoint());
        let res = next.run(sample_call()).await;
        assert_eq!(res.body, Bytes::from("inner"));
    }

    #[tokio::test]
    async fn middleware_post_processes_response() {
        let mut chain: VecDeque<Arc<dyn Middleware>> = VecDeque::new();
        chain.push_back(Arc::new(AddHeader));
        let res = Next::new(chain, endpoint()).run(sample_call()).await;
        assert_eq!(res.headers.get("x-mw").unwrap(), "1");
        assert_eq!(res.body, Bytes::from("inner"));
    }

    #[tokio::test]
    async fn middleware_can_short_circuit() {
        let mut chain: VecDeque<Arc<dyn Middleware>> = VecDeque::new();
        chain.push_back(Arc::new(ShortCircuit));
        chain.push_back(Arc::new(AddHeader)); // should never run
        let res = Next::new(chain, endpoint()).run(sample_call()).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN);
        assert!(res.headers.get("x-mw").is_none());
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod pipeline;
pub use pipeline::{Endpoint, Middleware, Next};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core pipeline::`
Expected: PASS (3 tests).

---

## Task 8: App + AppBuilder + Plugin + process()

**Files:**
- Create: `churust-core/src/app.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write App/AppBuilder/Plugin + the single request entry point + tests**

`churust-core/src/app.rs`:
```rust
use crate::call::Call;
use crate::pipeline::{Endpoint, Middleware, Next};
use crate::response::{IntoResponse, Response};
use crate::router::{Match, RouteBuilder, Router};
use bytes::Bytes;
use futures_util::FutureExt;
use http::header::ALLOW;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use std::collections::VecDeque;
use std::sync::Arc;

/// A plugin installs middleware (and, in later plans, extractors/state) into
/// the application during build (Ktor's `install(Plugin)`).
pub trait Plugin {
    fn install(self: Box<Self>, app: &mut AppBuilder);
}

/// Server configuration captured at build time (extended in Plan 2).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub max_body_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { host: "127.0.0.1".into(), port: 8080, max_body_bytes: 1 << 20 }
    }
}

/// Builder for an application (the `Churust::server()` entry point).
pub struct AppBuilder {
    router: Router,
    middleware: Vec<Arc<dyn Middleware>>,
    config: ServerConfig,
}

impl AppBuilder {
    fn new() -> Self {
        Self { router: Router::new(), middleware: Vec::new(), config: ServerConfig::default() }
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.config.host = host.into();
        self
    }
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.config.max_body_bytes = n;
        self
    }

    /// Install a plugin (consumes its config object).
    pub fn install<P: Plugin + 'static>(mut self, plugin: P) -> Self {
        Box::new(plugin).install(&mut self);
        self
    }

    /// Register a middleware directly (plugins use this internally).
    pub fn add_middleware(&mut self, mw: Arc<dyn Middleware>) {
        self.middleware.push(mw);
    }

    /// Define routes.
    pub fn routing(mut self, f: impl FnOnce(&mut RouteBuilder)) -> Self {
        let mut b = RouteBuilder::new(&mut self.router);
        f(&mut b);
        self
    }

    /// Finish building into an immutable, shareable `App`.
    pub fn build(self) -> App {
        App {
            inner: Arc::new(AppInner {
                router: self.router,
                middleware: self.middleware,
                config: self.config,
            }),
        }
    }
}

struct AppInner {
    router: Router,
    middleware: Vec<Arc<dyn Middleware>>,
    config: ServerConfig,
}

/// The assembled, cheaply-cloneable application.
#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

impl App {
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// THE single request entry point. Both the hyper engine and the
    /// `TestClient` call this. Panic-isolated: a panicking handler yields 500.
    pub async fn process(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let app = self.clone();
        let fut = async move {
            let call = Call::new(method, uri, headers, body);
            app.run_pipeline(call).await
        };
        match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
            Ok(res) => res,
            Err(_) => Response::text("Internal Server Error")
                .with_status(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }

    async fn run_pipeline(&self, call: Call) -> Response {
        let inner = self.inner.clone();
        let endpoint: Endpoint = Arc::new(move |mut call: Call| {
            let inner = inner.clone();
            Box::pin(async move {
                match inner.router.route(call.method(), call.path()) {
                    Match::Found { handler, params } => {
                        call.set_params(params);
                        handler.handle(call).await
                    }
                    Match::MethodNotAllowed { allow } => {
                        let value = allow
                            .iter()
                            .map(|m| m.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        Response::new(StatusCode::METHOD_NOT_ALLOWED).with_header(
                            ALLOW,
                            HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("")),
                        )
                    }
                    Match::NotFound => {
                        Response::text("Not Found").with_status(StatusCode::NOT_FOUND)
                    }
                }
            }) as _
        });

        let chain: VecDeque<Arc<dyn Middleware>> =
            self.inner.middleware.iter().cloned().collect();
        Next::new(chain, endpoint).run(call).await
    }
}

/// Entry point namespace: `Churust::server()`.
pub struct Churust;

impl Churust {
    pub fn server() -> AppBuilder {
        AppBuilder::new()
    }
}

// Allow `(impl IntoResponse)` builders to be used ergonomically in docs/tests.
#[allow(unused_imports)]
use crate::response::IntoResponse as _IntoResponseUsed;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Next;
    use async_trait::async_trait;

    fn app() -> App {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "home" });
                r.get("/boom", |_c: Call| async {
                    panic!("handler exploded");
                    #[allow(unreachable_code)]
                    ""
                });
            })
            .build()
    }

    async fn get(app: &App, path: &str) -> Response {
        app.process(
            Method::GET,
            path.parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
    }

    #[tokio::test]
    async fn routes_to_handler() {
        let res = get(&app(), "/").await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, Bytes::from("home"));
    }

    #[tokio::test]
    async fn unknown_path_is_404() {
        let res = get(&app(), "/missing").await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn panicking_handler_yields_500_not_crash() {
        let res = get(&app(), "/boom").await;
        assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn middleware_installed_via_plugin_runs() {
        struct MarkPlugin;
        struct Mark;
        #[async_trait]
        impl Middleware for Mark {
            async fn handle(&self, call: Call, next: Next) -> Response {
                let mut res = next.run(call).await;
                res.headers.insert(
                    http::header::HeaderName::from_static("x-plugin"),
                    HeaderValue::from_static("on"),
                );
                res
            }
        }
        impl Plugin for MarkPlugin {
            fn install(self: Box<Self>, app: &mut AppBuilder) {
                app.add_middleware(Arc::new(Mark));
            }
        }

        let app = Churust::server()
            .install(MarkPlugin)
            .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
            .build();
        let res = get(&app, "/").await;
        assert_eq!(res.headers.get("x-plugin").unwrap(), "on");
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod app;
pub use app::{App, AppBuilder, Churust, Plugin, ServerConfig};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core app::`
Expected: PASS (4 tests, incl. panic→500).

---

## Task 9: hyper/tokio engine

**Files:**
- Create: `churust-core/src/engine.rs`
- Modify: `churust-core/src/app.rs` (add `start()` / `start_with_shutdown()`)
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the engine (serve loop + body-limit + adapter)**

`churust-core/src/engine.rs`:
```rust
use crate::app::App;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// Serve `app` on `addr` until `shutdown` resolves (graceful drain).
pub async fn serve<F>(app: App, addr: SocketAddr, shutdown: F) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind(addr).await?;
    let max_body = app.config().max_body_bytes;
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let io = TokioIo::new(stream);
                let app = app.clone();
                let svc = service_fn(move |req: HyperRequest<Incoming>| {
                    let app = app.clone();
                    async move { handle(app, req, max_body).await }
                });
                let conn = ConnBuilder::new(TokioExecutor::new())
                    .serve_connection(io, svc);
                let fut = graceful.watch(conn);
                tokio::spawn(async move {
                    let _ = fut.await;
                });
            }
            _ = &mut shutdown => {
                break;
            }
        }
    }

    graceful.shutdown().await;
    Ok(())
}

async fn handle(
    app: App,
    req: HyperRequest<Incoming>,
    max_body: usize,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
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

    let res = app
        .process(parts.method, parts.uri, parts.headers, body_bytes)
        .await;

    let mut builder = HyperResponse::builder().status(res.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = res.headers;
    }
    Ok(builder
        .body(Full::new(res.body))
        .expect("response build is infallible for buffered body"))
}
```

- [ ] **Step 2: Add `start` helpers to `App`**

Append to `churust-core/src/app.rs` (inside `impl App`):
```rust
    /// Bind and serve until Ctrl-C (SIGINT). Blocks the async task.
    pub async fn start(self) -> std::io::Result<()> {
        let addr = format!("{}:{}", self.inner.config.host, self.inner.config.port)
            .parse::<std::net::SocketAddr>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        crate::engine::serve(self, addr, shutdown).await
    }

    /// Serve until the provided future resolves (for tests / custom signals).
    pub async fn start_with_shutdown<F>(self, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let addr = format!("{}:{}", self.inner.config.host, self.inner.config.port)
            .parse::<std::net::SocketAddr>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        crate::engine::serve(self, addr, shutdown).await
    }
```
Also make `AppBuilder` chainable to start directly. Append to `impl AppBuilder`:
```rust
    /// Convenience: build and start (Ctrl-C shutdown).
    pub async fn start(self) -> std::io::Result<()> {
        self.build().start().await
    }
```

- [ ] **Step 3: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod engine;
```

- [ ] **Step 4: Write an integration test that binds an ephemeral port**

`churust-core/tests/engine_serve.rs`:
```rust
use churust_core::{Call, Churust};
use std::time::Duration;

#[tokio::test]
async fn serves_real_http_and_shuts_down() {
    let app = Churust::server()
        .host("127.0.0.1")
        .port(0) // ephemeral
        .routing(|r| { r.get("/ping", |_c: Call| async { "pong" }); })
        .build();

    // Bind ourselves to learn the port, then hand the listener semantics to start_with_shutdown.
    // Simpler: bind a known-free port by trying 0 via std, then reuse.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = churust_core::Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| { r.get("/ping", |_c: Call| async { "pong" }); })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_with_shutdown(async move { let _ = rx.await; }).await.unwrap();
    });

    // Give it a moment to bind.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Minimal raw HTTP/1.1 GET using tokio TcpStream.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET /ping HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    assert!(text.contains("pong"), "got: {text}");

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}
```

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core --test engine_serve`
Expected: PASS. Then `cargo test -p churust-core` — all green.

---

## Task 10: In-process TestClient

**Files:**
- Create: `churust-core/src/test.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write TestClient + TestResponse + self-tests**

`churust-core/src/test.rs`:
```rust
//! In-process test harness. Drives `App::process` directly — no socket bind.

use crate::app::App;
use crate::response::Response;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};

/// A test client bound to an assembled `App`.
pub struct TestClient {
    app: App,
}

/// Builder for a single in-process request.
pub struct TestRequest<'c> {
    client: &'c TestClient,
    method: Method,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

/// The response returned by the in-process pipeline.
pub struct TestResponse {
    inner: Response,
}

impl TestClient {
    pub fn new(app: App) -> Self {
        Self { app }
    }

    pub fn request(&self, method: Method, uri: impl Into<String>) -> TestRequest<'_> {
        TestRequest {
            client: self,
            method,
            uri: uri.into(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    pub fn get(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::GET, uri)
    }
    pub fn post(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::POST, uri)
    }
    pub fn put(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::PUT, uri)
    }
    pub fn delete(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::DELETE, uri)
    }
}

impl<'c> TestRequest<'c> {
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("valid header value"),
        );
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    pub async fn send(self) -> TestResponse {
        let uri = self.uri.parse::<Uri>().expect("valid URI");
        let res = self
            .client
            .app
            .process(self.method, uri, self.headers, self.body)
            .await;
        TestResponse { inner: res }
    }
}

impl TestResponse {
    pub fn status(&self) -> StatusCode {
        self.inner.status
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.headers.get(name).and_then(|v| v.to_str().ok())
    }
    pub fn body_bytes(&self) -> &Bytes {
        &self.inner.body
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.inner.body).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::Call;
    use crate::Churust;

    fn app() -> App {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "home" });
                r.post("/echo", |mut c: Call| async move {
                    c.receive_text().await.unwrap_or_default()
                });
            })
            .build()
    }

    #[tokio::test]
    async fn get_returns_body_and_status() {
        let client = TestClient::new(app());
        let res = client.get("/").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "home");
    }

    #[tokio::test]
    async fn post_echoes_body() {
        let client = TestClient::new(app());
        let res = client.post("/echo").body("ping").send().await;
        assert_eq!(res.text(), "ping");
    }

    #[tokio::test]
    async fn missing_route_is_404() {
        let client = TestClient::new(app());
        let res = client.get("/nope").send().await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
```

- [ ] **Step 2: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod test;
pub use test::{TestClient, TestRequest, TestResponse};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core test::`
Expected: PASS (3 tests).

---

## Task 11: Umbrella crate + prelude

**Files:**
- Create: `churust/Cargo.toml`
- Create: `churust/src/lib.rs`
- Modify: `Cargo.toml` (restore full members list)

- [ ] **Step 1: Restore workspace members**

`Cargo.toml` members:
```toml
members = ["churust-core", "churust", "examples/hello"]
```

- [ ] **Step 2: Umbrella manifest**

`churust/Cargo.toml`:
```toml
[package]
name = "churust"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "Churust — a Ktor-inspired Rust web framework (Churro + Rust)."

[dependencies]
churust-core = { path = "../churust-core", version = "0.1.0" }
```

- [ ] **Step 3: Re-exports + prelude**

`churust/src/lib.rs`:
```rust
//! Churust — a Ktor-inspired Rust web framework.
//!
//! ```no_run
//! use churust::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     Churust::server()
//!         .routing(|r| {
//!             r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
//!         })
//!         .start()
//!         .await
//! }
//! ```

pub use churust_core::*;

pub mod prelude {
    pub use churust_core::{
        App, AppBuilder, Call, Churust, Error, IntoResponse, Middleware, Next, Plugin,
        Response, Result, Router,
    };
    pub use http::{Method, StatusCode};
}
```

- [ ] **Step 4: Doc-test checkpoint**

Run: `cargo test -p churust --doc`
Expected: PASS (the prelude doc example compiles). Then `cargo build -p churust`.

---

## Task 12: Hello example

**Files:**
- Create: `examples/hello/Cargo.toml`
- Create: `examples/hello/src/main.rs`

- [ ] **Step 1: Example manifest**

`examples/hello/Cargo.toml`:
```toml
[package]
name = "hello"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
churust = { path = "../../churust" }
tokio = { workspace = true }
```

- [ ] **Step 2: Example main**

`examples/hello/src/main.rs`:
```rust
use churust::prelude::*;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .routing(|r| {
            r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
            r.get("/users/{id}", |call: Call| async move {
                let id: u64 = call.param("id")?;
                Ok::<_, Error>(format!("user #{id}"))
            });
        })
        .start()
        .await
}
```

- [ ] **Step 3: Build + manual smoke (optional, no commit)**

Run: `cargo build -p hello`
Expected: builds clean.
Optional manual check: `cargo run -p hello &` then `curl -s localhost:8080/users/7` → `user #7`; then stop the process.

- [ ] **Step 4: Full-suite checkpoint**

Run: `cargo test` (whole workspace) and `cargo clippy --all-targets -- -D warnings`
Expected: all tests PASS, no clippy errors.

---

## Self-Review

**Spec coverage (vs `2026-06-06-churust-framework-design.md`):**
- §4 Workspace → Tasks 1, 11, 12 (json/logging/cors/auth crates are Plan 3; macros crate is Plan 2 — noted, not gaps).
- §5 Core types: `Call` (T4), `IntoResponse`/`Response` (T3), `Error` (T2), `Handler` (T5). Extractors (`FromCall`, `Path/Query/Json/State`) are **Plan 2** — intentionally deferred.
- §6 Routing (trie, `{param}`, `{rest...}`, nested DSL, 404/405) → T6.
- §7 Pipeline (Middleware, Next, onion, short-circuit) → T7. Named *phases* (Setup/Monitoring/Plugins/Call/Fallback) are simplified to install-order middleware in Plan 1; full phase ordering arrives with the plugin system in **Plan 3** — noted deviation.
- §8 Plugin system (`Plugin::install`, builder `install()`) → T8. Concrete plugins → Plan 3.
- §9 Engine (tokio listener, hyper service, graceful shutdown) → T9. rustls TLS → **Plan 2** (noted).
- §10 App state / DI → **Plan 2**.
- §11 Config (toml + env + DSL) → **Plan 2** (Plan 1 ships code-DSL `host/port/max_body_bytes` only).
- §12 Security: body-size limit (T9), panic isolation (T8), no version banner (we never set a `Server` header) — covered. Timeouts → Plan 2 with config.
- §13 Macros (`#[churust::main]`) → **Plan 2**.
- §14 Test harness → T10. ✓
- §15 Hello example → T12. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to" — every code step is complete. The one `// minimal ... sufficient for v1` comment in `percent_decode` is a scope note, not a placeholder; the function is fully implemented.

**Type consistency check:**
- `Response { status, headers, body }` — used identically in T3, T7, T8, T9, T10. ✓
- `Handler::handle(&self, Call) -> HandlerFuture` (T5) matches calls in T6/T8. ✓
- `Middleware::handle(&self, Call, Next) -> Response` (T7) matches plugin test (T8). ✓
- `Next::new(VecDeque<Arc<dyn Middleware>>, Endpoint)` (T7) matches construction in T8. ✓
- `App::process(Method, Uri, HeaderMap, Bytes) -> Response` (T8) matches engine (T9) and TestClient (T10). ✓
- `Endpoint = Arc<dyn Fn(Call) -> Pin<Box<dyn Future<Output=Response> + Send + 'static>> + Send + Sync>` (T7) matches endpoint built in T8. ✓
- `RouteBuilder` methods `get/post/put/delete/route` (T6) match usage in T8/T10/T12. ✓

**Risk notes for the implementer:**
- hyper 1.x / hyper-util 0.1 API surface (`auto::Builder`, `GracefulShutdown`, `TokioIo`, `service_fn`) is current as of the 1.x line; if `cargo add` resolves a different minor with a moved item, adjust imports — the structure (collect-with-limit → `process()` → build `Full<Bytes>`) is stable.
- `catch_unwind` requires `futures_util::FutureExt`; panics must be `UnwindSafe` — `AssertUnwindSafe` wraps that. Handlers that hold a lock across a panic could poison it; acceptable for v1.

---

## Execution Handoff (after this plan is implemented)

Plan 2 (extractors, app state, config, `#[churust::main]`, TLS, timeouts) and Plan 3 (json/logging/cors/auth plugins, named pipeline phases) will be written next, each as its own `docs/plans/` file following this same TDD structure.
