# Churust Plugins + Named Phases — Implementation Plan (Plan 3 of 3)

> **Builds on Plans 1 & 2** (both fully implemented and green). Extend existing files; do not recreate them.

**Goal:** Ship Churust's v1 plugin ecosystem — ContentNegotiation/JSON, CallLogging, CORS, Authentication — on top of a **named, ordered pipeline** so plugin execution order is deterministic.

**Architecture:** A `Phase` enum (`Setup < Monitoring < Plugins < Call < Fallback`) orders middleware; `AppBuilder` sorts installed middleware by phase (stable within phase) before building the onion. Plugins are separate feature-gated crates depending on `churust-core`, re-exported through the umbrella `churust` crate behind Cargo features. Two small core additions support auth: a per-`Call` typed **extensions** map (so middleware can pass an authenticated principal to handlers) and **response headers on `Error`** (so a 401 can carry `WWW-Authenticate`).

**Tech Stack additions:** serde_json (json), tracing (logging), jsonwebtoken + base64 (auth).

**Critical rules for the implementer:**
- `Phase` derives `Ord` from declaration order — keep the variant order `Setup, Monitoring, Plugins, Call, Fallback`.
- Keep `AppBuilder::add_middleware(mw)` working (defaults to `Phase::Plugins`); add `add_middleware_in(Phase, mw)`. Plan-1/2 callers must not break.
- The JSON plugin type is named **`ContentNegotiation`** (not `Json`) because `Json<T>` is the generic extractor/responder type.
- Plugin crates use `churust_core::TestClient` for tests (re-exported from core).
- No git, ever.

---

## File Structure

```
churust-core/src/pipeline.rs   ADD Phase enum
churust-core/src/app.rs        phase-aware middleware storage + ordered build
churust-core/src/call.rs       ADD extensions (http::Extensions) + insert/get/extensions
churust-core/src/error.rs      ADD response headers to Error
churust-core/src/response.rs   apply Error headers in IntoResponse
churust-core/src/lib.rs        export Phase
churust-json/                  NEW crate — Json<T> + ContentNegotiation plugin
churust-logging/               NEW crate — CallLogging plugin
churust-cors/                  NEW crate — Cors plugin
churust-auth/                  NEW crate — Auth (bearer/basic/jwt) + Principal<P>
churust/Cargo.toml             optional plugin deps + features json/logging/cors/auth/full
churust/src/lib.rs             cfg-gated re-exports + prelude additions
examples/api/                  NEW example — JSON CRUD using all four plugins
Cargo.toml                     workspace members += 4 plugin crates + examples/api
```

---

## Task 1: Named pipeline phases

**Files:**
- Modify: `churust-core/src/pipeline.rs`
- Modify: `churust-core/src/app.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Add the `Phase` enum + test**

In `churust-core/src/pipeline.rs`, add near the top (after imports):
```rust
/// Ordered insertion points for middleware (Ktor-style). Lower variants run
/// further "outside" in the onion (first on the way in, last on the way out).
/// `Ord` follows declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    Setup,
    Monitoring,
    Plugins,
    Call,
    Fallback,
}
```
Add a test in `pipeline.rs` `mod tests`:
```rust
    #[test]
    fn phases_are_ordered() {
        assert!(Phase::Setup < Phase::Monitoring);
        assert!(Phase::Monitoring < Phase::Plugins);
        assert!(Phase::Plugins < Phase::Call);
        assert!(Phase::Call < Phase::Fallback);
    }
```

- [ ] **Step 2: Make `AppBuilder` phase-aware**

In `churust-core/src/app.rs`:
- Add import: `use crate::pipeline::Phase;`
- Change the `AppBuilder` field `middleware: Vec<Arc<dyn Middleware>>` to `middleware: Vec<(Phase, Arc<dyn Middleware>)>`.
- In `AppBuilder::new`, initialize `middleware: Vec::new()` (unchanged).
- Replace `add_middleware` and add `add_middleware_in`:
```rust
    /// Register a middleware in a specific phase (plugins use this).
    pub fn add_middleware_in(&mut self, phase: Phase, mw: Arc<dyn Middleware>) {
        self.middleware.push((phase, mw));
    }

    /// Register a middleware in the default `Plugins` phase.
    pub fn add_middleware(&mut self, mw: Arc<dyn Middleware>) {
        self.add_middleware_in(Phase::Plugins, mw);
    }
```
- In `build()`, sort by phase (stable) and strip the phase into the stored `Vec<Arc<dyn Middleware>>`:
```rust
    pub fn build(self) -> App {
        let mut mw = self.middleware;
        mw.sort_by_key(|(phase, _)| *phase); // stable: install order preserved within a phase
        let middleware: Vec<Arc<dyn Middleware>> = mw.into_iter().map(|(_, m)| m).collect();
        App {
            inner: Arc::new(AppInner {
                router: self.router,
                middleware,
                config: self.config,
                state: Arc::new(self.state),
            }),
        }
    }
```
(`AppInner.middleware` stays `Vec<Arc<dyn Middleware>>`; `run_pipeline` is unchanged — it already builds the `VecDeque` from `inner.middleware` in order.)

- [ ] **Step 3: Export `Phase`**

In `churust-core/src/lib.rs`:
```rust
pub use pipeline::{Endpoint, Middleware, Next, Phase};
```

- [ ] **Step 4: Add an ordering integration test**

Append to `churust-core/src/app.rs` `mod tests`:
```rust
    #[tokio::test]
    async fn middleware_runs_in_phase_order() {
        use crate::pipeline::{Next, Phase};
        use async_trait::async_trait;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Recorder { log: Arc<Mutex<Vec<&'static str>>>, tag: &'static str }
        #[async_trait]
        impl Middleware for Recorder {
            async fn handle(&self, call: Call, next: Next) -> Response {
                self.log.lock().unwrap().push(self.tag);
                next.run(call).await
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let mut builder = Churust::server();
        // Install OUT of phase order; expect execution IN phase order.
        builder.add_middleware_in(Phase::Fallback, Arc::new(Recorder { log: log.clone(), tag: "fallback" }));
        builder.add_middleware_in(Phase::Setup, Arc::new(Recorder { log: log.clone(), tag: "setup" }));
        builder.add_middleware_in(Phase::Monitoring, Arc::new(Recorder { log: log.clone(), tag: "monitoring" }));
        let app = builder
            .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
            .build();
        let _ = get(&app, "/").await;
        assert_eq!(*log.lock().unwrap(), vec!["setup", "monitoring", "fallback"]);
    }
```
Note: `add_middleware_in` takes `&mut self`; `Churust::server()` returns an owned `AppBuilder`, so bind it `let mut builder = ...` then call the mutating methods, then chain `.routing(...).build()`. (The chained `.install()` path already uses `&mut self` internally, so this mixed style compiles.)

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core pipeline:: app::`
Then: `cargo test -p churust-core`
Expected: PASS.

---

## Task 2: Per-call extensions + Error response headers

**Files:**
- Modify: `churust-core/src/call.rs`
- Modify: `churust-core/src/error.rs`
- Modify: `churust-core/src/response.rs`

- [ ] **Step 1: Add a typed extensions map to `Call`**

In `churust-core/src/call.rs`:
- Add field to `Call`: `extensions: http::Extensions,`
- In `Call::new`, initialize: `extensions: http::Extensions::new(),`
- Add methods inside `impl Call`:
```rust
    /// Insert a per-call typed value (e.g. an authenticated principal set by a
    /// middleware, read later by an extractor).
    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, value: T) {
        self.extensions.insert(value);
    }

    /// Get a clone of a per-call typed value, if present.
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.extensions.get::<T>().cloned()
    }
```
Note: `Call` derives `Debug`; `http::Extensions` implements `Debug`, so this still compiles.

Add a test in `call.rs` `mod tests`:
```rust
    #[test]
    fn extensions_round_trip() {
        #[derive(Clone, PartialEq, Debug)]
        struct User(u32);
        let mut c = call("/", "");
        assert!(c.get::<User>().is_none());
        c.insert(User(7));
        assert_eq!(c.get::<User>(), Some(User(7)));
    }
```

- [ ] **Step 2: Add response headers to `Error`**

In `churust-core/src/error.rs`:
- Add import: `use http::header::{HeaderName, HeaderValue};`
- Add field to `Error`: `headers: Vec<(HeaderName, HeaderValue)>,`
- Update EVERY constructor (`new`, and the bodies of `bad_request`/`not_found`/`internal` route through `new`) so `new` initializes `headers: Vec::new()`:
```rust
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), source: None, headers: Vec::new() }
    }
```
- Add a builder + accessor:
```rust
    /// Attach a header to the response this error renders into (e.g.
    /// `WWW-Authenticate` on a 401).
    pub fn with_response_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    /// Headers to apply when rendering this error to a response.
    pub fn response_headers(&self) -> &[(HeaderName, HeaderValue)] {
        &self.headers
    }
```
(Leave `with_source` as-is; it only sets `source`.)

Add a test in `error.rs` `mod tests`:
```rust
    #[test]
    fn carries_response_headers() {
        let e = Error::new(StatusCode::UNAUTHORIZED, "no")
            .with_response_header(
                http::header::WWW_AUTHENTICATE,
                http::HeaderValue::from_static("Bearer"),
            );
        assert_eq!(e.response_headers().len(), 1);
    }
```

- [ ] **Step 3: Apply Error headers in `IntoResponse`**

In `churust-core/src/response.rs`, update the `impl IntoResponse for Error`:
```rust
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut res = Response::text(self.message().to_string()).with_status(self.status());
        for (name, value) in self.response_headers() {
            res.headers.insert(name.clone(), value.clone());
        }
        res
    }
}
```

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-core call:: error:: response::`
Then: `cargo test -p churust-core`
Expected: PASS.

---

## Task 3: `churust-json` — `Json<T>` + ContentNegotiation plugin

**Files:**
- Create: `churust-json/Cargo.toml`
- Create: `churust-json/src/lib.rs`
- Modify: `Cargo.toml` (workspace members + serde_json workspace dep)

- [ ] **Step 1: Workspace deps + member**

In root `Cargo.toml`:
- Add `"churust-json"` to `members`.
- Add to `[workspace.dependencies]`: `serde_json = "1"`.

- [ ] **Step 2: Crate manifest**

`churust-json/Cargo.toml`:
```toml
[package]
name = "churust-json"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
churust-core = { path = "../churust-core", version = "0.1.0" }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
http = { workspace = true }
bytes = { workspace = true }
```

- [ ] **Step 3: Implement `Json<T>` (extractor + responder) and the plugin**

`churust-json/src/lib.rs`:
```rust
//! JSON content negotiation for Churust.
//!
//! `Json<T>` is both an extractor (deserializes the request body) and a
//! responder (serializes `T`). The `ContentNegotiation` plugin renders error
//! responses as JSON instead of plain text.

use async_trait::async_trait;
use bytes::Bytes;
use churust_core::{
    App, AppBuilder, Call, Error, FromCall, IntoResponse, Middleware, Next, Phase, Plugin, Response,
    Result,
};
use http::header::CONTENT_TYPE;
use http::{HeaderValue, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;

/// JSON wrapper. As a handler argument it deserializes the body; as a return
/// value it serializes to `application/json`.
#[derive(Debug, Clone)]
pub struct Json<T>(pub T);

#[async_trait]
impl<T> FromCall for Json<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call(mut call: Call) -> Result<Self> {
        let bytes = call.receive_bytes().await;
        let value = serde_json::from_slice::<T>(&bytes)
            .map_err(|e| Error::bad_request(format!("invalid JSON body: {e}")))?;
        Ok(Json(value))
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => Response::bytes("application/json", Bytes::from(bytes)),
            Err(e) => Error::internal(format!("JSON serialization failed: {e}")).into_response(),
        }
    }
}

/// Plugin: renders error responses (status >= 400 with `text/plain` body) as
/// JSON `{"error": "...", "status": N}`.
#[derive(Debug, Clone, Default)]
pub struct ContentNegotiation {
    pretty: bool,
}

impl ContentNegotiation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }
}

impl Plugin for ContentNegotiation {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(Phase::Plugins, Arc::new(JsonErrors { pretty: self.pretty }));
    }
}

struct JsonErrors {
    pretty: bool,
}

#[async_trait]
impl Middleware for JsonErrors {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let mut res = next.run(call).await;
        let is_error = res.status.is_client_error() || res.status.is_server_error();
        let is_text = res
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.starts_with("text/plain"))
            .unwrap_or(false);
        if is_error && is_text {
            let msg = String::from_utf8_lossy(&res.body).into_owned();
            let body = serde_json::json!({ "error": msg, "status": res.status.as_u16() });
            let bytes = if self.pretty {
                serde_json::to_vec_pretty(&body)
            } else {
                serde_json::to_vec(&body)
            }
            .unwrap_or_default();
            res.body = Bytes::from(bytes);
            res.headers
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{Churust, TestClient};
    use serde::Deserialize;

    #[derive(Serialize, Deserialize)]
    struct Echo {
        msg: String,
    }

    fn app() -> App {
        Churust::server()
            .install(ContentNegotiation::new())
            .routing(|r| {
                r.post("/echo", |Json(body): Json<Echo>| async move { Json(body) });
                r.get("/boom", |_c: Call| async {
                    Err::<&str, _>(Error::bad_request("nope"))
                });
            })
            .build()
    }

    #[tokio::test]
    async fn json_round_trips_through_handler() {
        let client = TestClient::new(app());
        let res = client
            .post("/echo")
            .header("content-type", "application/json")
            .body(r#"{"msg":"hi"}"#)
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.header("content-type"), Some("application/json"));
        let parsed: Echo = serde_json::from_slice(res.body_bytes()).unwrap();
        assert_eq!(parsed.msg, "hi");
    }

    #[tokio::test]
    async fn invalid_json_is_400() {
        let client = TestClient::new(app());
        let res = client.post("/echo").body("not json").send().await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn errors_render_as_json() {
        let client = TestClient::new(app());
        let res = client.get("/boom").send().await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(res.header("content-type"), Some("application/json"));
        let v: serde_json::Value = serde_json::from_slice(res.body_bytes()).unwrap();
        assert_eq!(v["error"], "nope");
        assert_eq!(v["status"], 400);
    }
}
```

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-json`
Then: `cargo clippy -p churust-json --all-targets -- -D warnings`
Expected: PASS / clean.

---

## Task 4: `churust-logging` — CallLogging plugin

**Files:**
- Create: `churust-logging/Cargo.toml`
- Create: `churust-logging/src/lib.rs`
- Modify: `Cargo.toml` (members + tracing workspace dep)

- [ ] **Step 1: Workspace deps + member**

In root `Cargo.toml`:
- Add `"churust-logging"` to `members`.
- Add to `[workspace.dependencies]`: `tracing = "0.1"`.

- [ ] **Step 2: Crate manifest**

`churust-logging/Cargo.toml`:
```toml
[package]
name = "churust-logging"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
churust-core = { path = "../churust-core", version = "0.1.0" }
async-trait = { workspace = true }
http = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tracing = { workspace = true, features = ["std"] }
```

- [ ] **Step 3: Implement CallLogging**

`churust-logging/src/lib.rs`:
```rust
//! Per-request logging for Churust (via `tracing`).

use async_trait::async_trait;
use churust_core::{AppBuilder, Call, Middleware, Next, Phase, Plugin, Response};
use std::sync::Arc;
use std::time::Instant;
use tracing::Level;

/// Logs each request: method, path, status, latency. Runs in the `Monitoring`
/// phase so it wraps everything else.
#[derive(Debug, Clone)]
pub struct CallLogging {
    level: Level,
}

impl Default for CallLogging {
    fn default() -> Self {
        Self { level: Level::INFO }
    }
}

impl CallLogging {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }
}

impl Plugin for CallLogging {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(Phase::Monitoring, Arc::new(LogMiddleware { level: self.level }));
    }
}

struct LogMiddleware {
    level: Level,
}

#[async_trait]
impl Middleware for LogMiddleware {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let method = call.method().clone();
        let path = call.path().to_string();
        let start = Instant::now();
        let res = next.run(call).await;
        let latency_ms = start.elapsed().as_millis();
        let status = res.status.as_u16();
        // tracing macros need a const level; branch on the configured level.
        match self.level {
            Level::ERROR => tracing::error!(%method, path, status, latency_ms, "request"),
            Level::WARN => tracing::warn!(%method, path, status, latency_ms, "request"),
            Level::INFO => tracing::info!(%method, path, status, latency_ms, "request"),
            Level::DEBUG => tracing::debug!(%method, path, status, latency_ms, "request"),
            Level::TRACE => tracing::trace!(%method, path, status, latency_ms, "request"),
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};
    use http::StatusCode;

    fn app() -> App {
        Churust::server()
            .install(CallLogging::new())
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build()
    }

    #[tokio::test]
    async fn logging_is_transparent() {
        // The middleware must not alter the response.
        let client = TestClient::new(app());
        let res = client.get("/").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "ok");
    }
}
```

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-logging`
Then: `cargo clippy -p churust-logging --all-targets -- -D warnings`
Expected: PASS / clean.

---

## Task 5: `churust-cors` — CORS plugin

**Files:**
- Create: `churust-cors/Cargo.toml`
- Create: `churust-cors/src/lib.rs`
- Modify: `Cargo.toml` (members)

- [ ] **Step 1: Member**

In root `Cargo.toml` add `"churust-cors"` to `members`.

- [ ] **Step 2: Crate manifest**

`churust-cors/Cargo.toml`:
```toml
[package]
name = "churust-cors"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
churust-core = { path = "../churust-core", version = "0.1.0" }
async-trait = { workspace = true }
http = { workspace = true }
```

- [ ] **Step 3: Implement CORS**

`churust-cors/src/lib.rs`:
```rust
//! CORS support for Churust.

use async_trait::async_trait;
use churust_core::{AppBuilder, Call, Middleware, Next, Phase, Plugin, Response};
use http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN,
    VARY,
};
use http::{HeaderValue, Method, StatusCode};
use std::sync::Arc;

/// Which origins are allowed.
#[derive(Debug, Clone)]
enum AllowOrigin {
    Any,
    List(Vec<String>),
}

/// CORS configuration.
#[derive(Debug, Clone)]
pub struct Cors {
    origin: AllowOrigin,
    methods: Vec<Method>,
    headers: Vec<String>,
    credentials: bool,
    max_age: Option<u64>,
}

impl Cors {
    /// Permissive policy: any origin, common methods, any header. Note: per the
    /// CORS spec, credentials cannot be combined with `*` origin, so
    /// `permissive()` leaves credentials off.
    pub fn permissive() -> Self {
        Self {
            origin: AllowOrigin::Any,
            methods: vec![
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::PATCH,
                Method::OPTIONS,
            ],
            headers: vec!["*".to_string()],
            credentials: false,
            max_age: Some(86_400),
        }
    }

    pub fn new() -> Self {
        Self {
            origin: AllowOrigin::List(Vec::new()),
            methods: vec![Method::GET, Method::POST],
            headers: Vec::new(),
            credentials: false,
            max_age: None,
        }
    }

    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        match &mut self.origin {
            AllowOrigin::List(v) => v.push(origin.into()),
            AllowOrigin::Any => {
                self.origin = AllowOrigin::List(vec![origin.into()]);
            }
        }
        self
    }

    pub fn allow_methods(mut self, methods: Vec<Method>) -> Self {
        self.methods = methods;
        self
    }

    pub fn allow_headers(mut self, headers: Vec<String>) -> Self {
        self.headers = headers;
        self
    }

    pub fn allow_credentials(mut self, yes: bool) -> Self {
        self.credentials = yes;
        self
    }

    pub fn max_age(mut self, seconds: u64) -> Self {
        self.max_age = Some(seconds);
        self
    }

    fn origin_allowed(&self, origin: &str) -> Option<String> {
        match &self.origin {
            AllowOrigin::Any => Some("*".to_string()),
            AllowOrigin::List(list) => {
                if list.iter().any(|o| o == origin) {
                    Some(origin.to_string())
                } else {
                    None
                }
            }
        }
    }

    fn apply_common(&self, res: &mut Response, allow_origin: &str) {
        res.headers.insert(
            ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(allow_origin).unwrap_or(HeaderValue::from_static("*")),
        );
        if self.credentials {
            res.headers.insert(
                ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }
        // Vary: Origin so caches don't serve the wrong CORS headers.
        res.headers.insert(VARY, HeaderValue::from_static("Origin"));
    }
}

impl Default for Cors {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Cors {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(Phase::Plugins, Arc::new(CorsMiddleware { cfg: *self }));
    }
}

struct CorsMiddleware {
    cfg: Cors,
}

#[async_trait]
impl Middleware for CorsMiddleware {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let origin = call.header("origin").map(|s| s.to_string());
        let is_preflight = *call.method() == Method::OPTIONS
            && call.header(ACCESS_CONTROL_REQUEST_METHOD.as_str()).is_some();

        // Preflight: short-circuit with 204 + CORS headers.
        if is_preflight {
            let mut res = Response::new(StatusCode::NO_CONTENT);
            if let Some(o) = origin.as_deref().and_then(|o| self.cfg.origin_allowed(o)) {
                self.cfg.apply_common(&mut res, &o);
                let methods = self
                    .cfg
                    .methods
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if let Ok(v) = HeaderValue::from_str(&methods) {
                    res.headers.insert(ACCESS_CONTROL_ALLOW_METHODS, v);
                }
                if !self.cfg.headers.is_empty() {
                    let hs = self.cfg.headers.join(", ");
                    if let Ok(v) = HeaderValue::from_str(&hs) {
                        res.headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, v);
                    }
                }
                if let Some(age) = self.cfg.max_age {
                    if let Ok(v) = HeaderValue::from_str(&age.to_string()) {
                        res.headers.insert(ACCESS_CONTROL_MAX_AGE, v);
                    }
                }
            }
            return res;
        }

        // Actual request: run the chain, then decorate the response.
        let mut res = next.run(call).await;
        if let Some(o) = origin.as_deref().and_then(|o| self.cfg.origin_allowed(o)) {
            self.cfg.apply_common(&mut res, &o);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};

    fn app() -> App {
        Churust::server()
            .install(Cors::permissive())
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build()
    }

    #[tokio::test]
    async fn actual_request_gets_allow_origin() {
        let client = TestClient::new(app());
        let res = client.get("/").header("origin", "https://example.com").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.header("access-control-allow-origin"), Some("*"));
        assert_eq!(res.header("vary"), Some("Origin"));
    }

    #[tokio::test]
    async fn preflight_returns_204_with_methods() {
        let client = TestClient::new(app());
        let res = client
            .request(Method::OPTIONS, "/")
            .header("origin", "https://example.com")
            .header("access-control-request-method", "POST")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let methods = res.header("access-control-allow-methods").unwrap();
        assert!(methods.contains("POST"));
    }

    #[tokio::test]
    async fn disallowed_origin_gets_no_cors_header() {
        let app = Churust::server()
            .install(Cors::new().allow_origin("https://allowed.com"))
            .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
            .build();
        let client = TestClient::new(app);
        let res = client.get("/").header("origin", "https://evil.com").send().await;
        assert_eq!(res.header("access-control-allow-origin"), None);
    }
}
```

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-cors`
Then: `cargo clippy -p churust-cors --all-targets -- -D warnings`
Expected: PASS / clean.

---

## Task 6: `churust-auth` — Authentication (Bearer / Basic / JWT) + `Principal<P>`

**Files:**
- Create: `churust-auth/Cargo.toml`
- Create: `churust-auth/src/lib.rs`
- Modify: `Cargo.toml` (members + jsonwebtoken, base64 workspace deps)

- [ ] **Step 1: Workspace deps + member**

In root `Cargo.toml`:
- Add `"churust-auth"` to `members`.
- Add to `[workspace.dependencies]`:
```toml
jsonwebtoken = "9"
base64 = "0.22"
```

- [ ] **Step 2: Crate manifest**

`churust-auth/Cargo.toml`:
```toml
[package]
name = "churust-auth"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
churust-core = { path = "../churust-core", version = "0.1.0" }
async-trait = { workspace = true }
http = { workspace = true }
serde = { workspace = true }
jsonwebtoken = { workspace = true }
base64 = { workspace = true }

[dev-dependencies]
serde = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 3: Implement auth schemes + `Principal<P>`**

`churust-auth/src/lib.rs`:
```rust
//! Authentication for Churust.
//!
//! An auth plugin AUTHENTICATES (verifies credentials and, on success, inserts
//! a principal into the call's extensions). It does NOT authorize — that is done
//! by asking for a `Principal<P>`, which returns 401 when no principal is
//! present. This keeps "require auth" type-driven and explicit.

use async_trait::async_trait;
use churust_core::{
    AppBuilder, Call, Error, FromCallParts, Middleware, Next, Phase, Plugin, Response, Result,
};
use http::header::WWW_AUTHENTICATE;
use http::{HeaderValue, StatusCode};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Extracts the authenticated principal of type `P`. 401 (with
/// `WWW-Authenticate`) if no auth plugin inserted one for this call.
#[derive(Debug, Clone)]
pub struct Principal<P>(pub P);

#[async_trait]
impl<P> FromCallParts for Principal<P>
where
    P: Clone + Send + Sync + 'static,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        match call.get::<P>() {
            Some(p) => Ok(Principal(p)),
            None => Err(Error::new(StatusCode::UNAUTHORIZED, "authentication required")
                .with_response_header(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))),
        }
    }
}

// ---------- Bearer ----------

/// Bearer-token auth. `verify` maps a token to an optional principal.
pub struct Bearer<P, F> {
    verify: Arc<F>,
    _p: PhantomData<fn() -> P>,
}

/// `Auth` namespace for constructing scheme plugins.
pub struct Auth;

impl Auth {
    /// Bearer scheme. `verify(token) -> Option<P>`.
    pub fn bearer<P, F, Fut>(verify: F) -> Bearer<P, F>
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<P>> + Send + 'static,
        P: Clone + Send + Sync + 'static,
    {
        Bearer { verify: Arc::new(verify), _p: PhantomData }
    }

    /// Basic scheme. `verify(username, password) -> Option<P>`.
    pub fn basic<P, F, Fut>(verify: F) -> Basic<P, F>
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<P>> + Send + 'static,
        P: Clone + Send + Sync + 'static,
    {
        Basic { verify: Arc::new(verify), _p: PhantomData }
    }
}

impl<P, F, Fut> Plugin for Bearer<P, F>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<P>> + Send + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        let verify = self.verify.clone();
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(BearerMiddleware::<P> {
                verify: Arc::new(move |token: String| {
                    let verify = verify.clone();
                    Box::pin(async move { verify(token).await }) as BoxFut<Option<P>>
                }),
            }),
        );
    }
}

struct BearerMiddleware<P> {
    #[allow(clippy::type_complexity)]
    verify: Arc<dyn Fn(String) -> BoxFut<Option<P>> + Send + Sync>,
}

#[async_trait]
impl<P> Middleware for BearerMiddleware<P>
where
    P: Clone + Send + Sync + 'static,
{
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        if let Some(raw) = call.header("authorization") {
            if let Some(token) = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer ")) {
                if let Some(principal) = (self.verify)(token.trim().to_string()).await {
                    call.insert(principal);
                }
            }
        }
        next.run(call).await
    }
}

// ---------- Basic ----------

pub struct Basic<P, F> {
    verify: Arc<F>,
    _p: PhantomData<fn() -> P>,
}

impl<P, F, Fut> Plugin for Basic<P, F>
where
    F: Fn(String, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<P>> + Send + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        let verify = self.verify.clone();
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(BasicMiddleware::<P> {
                verify: Arc::new(move |u: String, p: String| {
                    let verify = verify.clone();
                    Box::pin(async move { verify(u, p).await }) as BoxFut<Option<P>>
                }),
            }),
        );
    }
}

struct BasicMiddleware<P> {
    #[allow(clippy::type_complexity)]
    verify: Arc<dyn Fn(String, String) -> BoxFut<Option<P>> + Send + Sync>,
}

#[async_trait]
impl<P> Middleware for BasicMiddleware<P>
where
    P: Clone + Send + Sync + 'static,
{
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        if let Some((user, pass)) = call.header("authorization").and_then(decode_basic) {
            if let Some(principal) = (self.verify)(user, pass).await {
                call.insert(principal);
            }
        }
        next.run(call).await
    }
}

fn decode_basic(header: &str) -> Option<(String, String)> {
    use base64::Engine;
    let b64 = header.strip_prefix("Basic ").or_else(|| header.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

// ---------- JWT ----------

/// JWT bearer auth that decodes claims of type `C` and inserts them as the
/// principal. `C` must be `DeserializeOwned + Clone + Send + Sync`.
pub struct Jwt<C> {
    key: jsonwebtoken::DecodingKey,
    validation: jsonwebtoken::Validation,
    _c: PhantomData<fn() -> C>,
}

impl Auth {
    /// JWT scheme using an HMAC secret and default validation (HS256).
    pub fn jwt_hs256<C>(secret: &[u8]) -> Jwt<C>
    where
        C: serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
    {
        Jwt {
            key: jsonwebtoken::DecodingKey::from_secret(secret),
            validation: jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
            _c: PhantomData,
        }
    }
}

impl<C> Plugin for Jwt<C>
where
    C: serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(JwtMiddleware::<C> {
                key: Arc::new(self.key),
                validation: Arc::new(self.validation),
                _c: PhantomData,
            }),
        );
    }
}

struct JwtMiddleware<C> {
    key: Arc<jsonwebtoken::DecodingKey>,
    validation: Arc<jsonwebtoken::Validation>,
    _c: PhantomData<fn() -> C>,
}

#[async_trait]
impl<C> Middleware for JwtMiddleware<C>
where
    C: serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        if let Some(raw) = call.header("authorization") {
            if let Some(token) = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer ")) {
                if let Ok(data) =
                    jsonwebtoken::decode::<C>(token.trim(), &self.key, &self.validation)
                {
                    call.insert(data.claims);
                }
            }
        }
        next.run(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq)]
    struct User {
        name: String,
    }

    fn bearer_app() -> App {
        Churust::server()
            .install(Auth::bearer(|token: String| async move {
                if token == "secret" {
                    Some(User { name: "ana".into() })
                } else {
                    None
                }
            }))
            .routing(|r| {
                r.get("/me", |Principal(u): Principal<User>| async move {
                    format!("hello {}", u.name)
                });
            })
            .build()
    }

    #[tokio::test]
    async fn valid_bearer_reaches_protected_route() {
        let client = TestClient::new(bearer_app());
        let res = client.get("/me").header("authorization", "Bearer secret").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "hello ana");
    }

    #[tokio::test]
    async fn missing_token_is_401_with_challenge() {
        let client = TestClient::new(bearer_app());
        let res = client.get("/me").send().await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    }

    #[tokio::test]
    async fn wrong_token_is_401() {
        let client = TestClient::new(bearer_app());
        let res = client.get("/me").header("authorization", "Bearer nope").send().await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn basic_auth_works() {
        let app = Churust::server()
            .install(Auth::basic(|u: String, p: String| async move {
                if u == "admin" && p == "pw" {
                    Some(User { name: u })
                } else {
                    None
                }
            }))
            .routing(|r| {
                r.get("/me", |Principal(u): Principal<User>| async move { u.name });
            })
            .build();
        let client = TestClient::new(app);
        // base64("admin:pw") = YWRtaW46cHc=
        let res = client.get("/me").header("authorization", "Basic YWRtaW46cHc=").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "admin");
    }

    #[derive(Serialize, Deserialize, Clone, Debug)]
    struct Claims {
        sub: String,
        exp: usize,
    }

    #[tokio::test]
    async fn jwt_decodes_claims() {
        use jsonwebtoken::{encode, EncodingKey, Header};
        let secret = b"topsecret";
        let claims = Claims { sub: "u42".into(), exp: 9_999_999_999 };
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(secret)).unwrap();

        let app = Churust::server()
            .install(Auth::jwt_hs256::<Claims>(secret))
            .routing(|r| {
                r.get("/who", |Principal(c): Principal<Claims>| async move { c.sub });
            })
            .build();
        let client = TestClient::new(app);
        let res = client
            .get("/who")
            .header("authorization", &format!("Bearer {token}"))
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "u42");
    }
}
```

> Implementer note: the dev-test `jwt_decodes_claims` uses `jsonwebtoken::encode`, which is in the default feature set of `jsonwebtoken` 9. If `cargo` resolves a version where `Validation::new` or `Algorithm::HS256` moved, adapt per compiler errors while preserving HS256 + secret-key behavior.

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-auth`
Then: `cargo clippy -p churust-auth --all-targets -- -D warnings`
Expected: PASS / clean.

---

## Task 7: Umbrella features + prelude + `examples/api` + full suite

**Files:**
- Modify: `churust/Cargo.toml`
- Modify: `churust/src/lib.rs`
- Create: `examples/api/Cargo.toml`
- Create: `examples/api/src/main.rs`
- Modify: `Cargo.toml` (members += `examples/api`)

- [ ] **Step 1: Umbrella optional deps + features**

In `churust/Cargo.toml`:
```toml
[dependencies]
churust-core = { path = "../churust-core", version = "0.1.0" }
churust-macros = { path = "../churust-macros", version = "0.1.0" }
http = { workspace = true }
tokio = { workspace = true }
churust-json = { path = "../churust-json", version = "0.1.0", optional = true }
churust-logging = { path = "../churust-logging", version = "0.1.0", optional = true }
churust-cors = { path = "../churust-cors", version = "0.1.0", optional = true }
churust-auth = { path = "../churust-auth", version = "0.1.0", optional = true }

[features]
default = []
json = ["dep:churust-json"]
logging = ["dep:churust-logging"]
cors = ["dep:churust-cors"]
auth = ["dep:churust-auth"]
tls = ["churust-core/tls"]
full = ["json", "logging", "cors", "auth"]
```

- [ ] **Step 2: cfg-gated re-exports + prelude**

In `churust/src/lib.rs`, after the existing `pub use churust_core::*;` and macro re-export, add:
```rust
#[cfg(feature = "json")]
pub use churust_json as json;
#[cfg(feature = "logging")]
pub use churust_logging as logging;
#[cfg(feature = "cors")]
pub use churust_cors as cors;
#[cfg(feature = "auth")]
pub use churust_auth as auth;
```
Extend the `prelude` module with cfg-gated plugin exports (append inside `pub mod prelude { ... }`):
```rust
    #[cfg(feature = "json")]
    pub use churust_json::{ContentNegotiation, Json};
    #[cfg(feature = "logging")]
    pub use churust_logging::CallLogging;
    #[cfg(feature = "cors")]
    pub use churust_cors::Cors;
    #[cfg(feature = "auth")]
    pub use churust_auth::{Auth, Principal};
```

- [ ] **Step 3: `examples/api` — JSON CRUD using all four plugins**

In root `Cargo.toml` add `"examples/api"` to `members`.

`examples/api/Cargo.toml`:
```toml
[package]
name = "api"
version = "0.1.0"
edition.workspace = true
publish = false

[dependencies]
churust = { path = "../../churust", features = ["full"] }
tokio = { workspace = true }
serde = { workspace = true }
```

`examples/api/src/main.rs`:
```rust
use churust::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
struct Note {
    id: u64,
    text: String,
}

#[derive(Deserialize)]
struct NewNote {
    text: String,
}

#[derive(Clone, Debug)]
struct AdminUser {
    name: String,
}

// Simple in-memory store.
struct Store {
    notes: Mutex<Vec<Note>>,
}

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .state(Store { notes: Mutex::new(Vec::new()) })
        // Order of installation; phases make execution deterministic regardless.
        .install(CallLogging::new())
        .install(ContentNegotiation::new())
        .install(Cors::permissive())
        .install(Auth::bearer(|token: String| async move {
            // Demo only: accept a fixed admin token.
            if token == "admin-token" {
                Some(AdminUser { name: "admin".into() })
            } else {
                None
            }
        }))
        .routing(|r| {
            r.get("/notes", |s: State<Store>| async move {
                let notes = s.notes.lock().unwrap().clone();
                Json(notes)
            });
            // Creating a note requires authentication (asking for Principal enforces it).
            r.post("/notes", |Principal(_admin): Principal<AdminUser>,
                              s: State<Store>,
                              Json(input): Json<NewNote>| async move {
                let mut notes = s.notes.lock().unwrap();
                let id = notes.len() as u64 + 1;
                let note = Note { id, text: input.text };
                notes.push(note.clone());
                (StatusCode::CREATED, Json(note))
            });
        })
        .start()
        .await
}
```
> Note: this handler has THREE arguments — two `FromCallParts` (`Principal`, `State`) and a final `FromCall` (`Json`). The arity-3 `HandlerFn` impl from Plan 2 covers it. The `(StatusCode, Json<Note>)` return uses the Plan-1 `(StatusCode, T)` `IntoResponse` impl.

- [ ] **Step 4: Full-suite checkpoint**

Run:
```
cargo test --workspace
cargo test -p churust --features full
cargo build -p api
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p churust --all-targets --features full -- -D warnings
cargo clippy -p churust-core --all-targets --features tls -- -D warnings
```
Expected: all PASS / clean.

Optional manual smoke (no commit):
```
cargo run -p api &
curl -s localhost:8080/notes                                   # []
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8080/notes  # 401 (no token)
curl -s -X POST localhost:8080/notes -H 'authorization: Bearer admin-token' \
     -H 'content-type: application/json' -d '{"text":"hi"}'    # {"id":1,"text":"hi"}
curl -s localhost:8080/notes                                   # [{"id":1,"text":"hi"}]
# stop the process
```

---

## Self-Review

**Spec coverage (vs design spec):**
- §7 Named phases (`Setup→Monitoring→Plugins→Call→Fallback`) → Task 1. ✓
- §8 Plugin system + the four v1 plugins:
  - ContentNegotiation (JSON) → Task 3 (`Json<T>` extractor+responder, JSON error rendering). ✓
  - CallLogging → Task 4 (tracing, Monitoring phase). ✓
  - CORS → Task 5 (preflight + headers, Plugins phase, `permissive()`/strict builders). ✓
  - Authentication (Bearer/JWT + Basic, principal into call attributes, 401 + `WWW-Authenticate`, `require_auth` via `Principal<P>`) → Task 6 (+ core extensions/error-headers in Task 2). ✓
- §5.2 `Json<T>` extractor (deferred from Plan 2) → Task 3. ✓
- §9 engine — unchanged. §10/§11/§12/§13 — delivered in Plans 1–2.

**Placeholder scan:** No "TBD"/"add error handling"/"similar to". Implementer notes (jsonwebtoken version, arity-3 handler) are guidance alongside complete code, not placeholders.

**Type consistency:**
- `Phase` variants/order (T1) are referenced by every plugin's `add_middleware_in(Phase::X, ...)` (T3–T6). ✓
- `AppBuilder::add_middleware_in(Phase, Arc<dyn Middleware>)` (T1) is the exact signature plugins call. ✓
- `Call::insert<T: Clone+Send+Sync+'static>` / `Call::get<T>() -> Option<T>` (T2) match `BearerMiddleware`/`Principal` usage (T6). ✓
- `Error::with_response_header(HeaderName, HeaderValue)` (T2) matches `Principal`'s 401 challenge (T6); `IntoResponse for Error` applies them (T2 Step 3). ✓
- `Json<T>` is `FromCall` (consuming, last-arg) AND `IntoResponse` (T3) — used both ways in the api example (T7). Not `FromCallParts`, so it stays last-arg-only (correct). ✓
- `Plugin::install(self: Box<Self>, &mut AppBuilder)` (Plan 1) is the signature all four plugins implement. ✓
- `Response::bytes(&'static str, impl Into<Bytes>)` (Plan 1) used by `Json` responder. ✓
- `TestClient` API (`get/post/request/header/body/send`, `TestResponse::status/header/text/body_bytes`) from Plan 1 used by all plugin tests. ✓
- Umbrella features `json/logging/cors/auth/full/tls` (T7) gate the re-exports + prelude consistently. ✓

**Risk notes:**
- `Cors` derives `Clone` and is moved into the middleware via `*self` in `install` — `Box<Self>` deref-move requires `Cors: Sized` (it is). Fine.
- The auth middlewares are generic over `P`/`C`; the boxed verify closure (`BoxFut`) keeps the `Plugin`/`Middleware` impls object-safe where stored as `Arc<dyn Middleware>`. The middleware structs are concrete generic types, boxed at install — OK.
- `http::Extensions` requires inserted types to be `Clone + Send + Sync + 'static` for our `get` clone-out; principals/claims in tests satisfy this.
- Feature-gated prelude: building `churust` with no features must still compile (the cfg blocks compile out). The api example uses `features = ["full"]`.

---

## Execution Handoff

Execute sequentially, task by task: implement, then spec review, then quality review. After Plan 3 is green, Churust v1 is feature-complete; finish with a workspace-wide review + README.
