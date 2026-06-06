# Churust Ergonomics — Implementation Plan (Plan 2 of 3)

> **Builds on Plan 1** (`2026-06-06-churust-core-kernel.md`), which is fully implemented and green. Do not re-create Plan-1 files; extend them.

**Goal:** Add the developer-ergonomics layer to Churust — typed extractors (hybrid handlers), typed app state/DI, `churust.toml` + env configuration, request timeouts, opt-in rustls TLS, and the `#[churust::main]` macro.

**Architecture:** The `Handler` trait (signature `handle(&self, Call) -> HandlerFuture`) is unchanged, so the router/pipeline are untouched. We **replace the blanket `Handler` impl** with an arity-based family (0..12 args) driven by two extractor traits: `FromCallParts` (borrows `&mut Call`, any position) and `FromCall` (consumes `Call`, last position only). `Call: FromCall`, so Plan-1 call-style handlers keep working. App state lives in a type-keyed `StateMap` injected into each `Call` by `App::process`. Config merges defaults < `churust.toml` < env < code-DSL. TLS and the macro are additive.

**Tech Stack additions:** serde, serde_urlencoded, toml (config + Query), rustls + tokio-rustls + rustls-pemfile (TLS, `tls` feature), syn + quote + proc-macro2 (macro crate).

**Critical compatibility rules for the implementer:**
- Handlers now require `Clone` (closures capturing only `Clone` data — including capture-free closures — satisfy this). All Plan-1 handlers qualify.
- `Call::new(method, uri, headers, body)` signature MUST stay the same (engine, test harness, and many tests call it). Add app-state via a separate `Call::set_state(...)` setter that defaults to empty.
- Never make `Call: FromCallParts` (would conflict with `impl FromCall for Call`).
- Extractor errors render via `IntoResponse` (already implemented for `Error`).

---

## File Structure

```
churust-core/src/extract.rs        NEW — FromCall, FromCallParts, Path, Query, Header, State
churust-core/src/handler.rs        REPLACE blanket impl with arity family (keep trait + boxed types)
churust-core/src/call.rs           ADD state field + set_state + state::<T>() accessor
churust-core/src/state.rs          NEW — StateMap (type-keyed)
churust-core/src/config.rs         NEW — Config, load from toml + env
churust-core/src/app.rs            EXTEND AppBuilder: .state(), .with_config(), .tls(); inject state + timeout
churust-core/src/engine.rs         ADD request timeout + optional TLS acceptor branch
churust-core/src/tls.rs            NEW (feature "tls") — load certs/keys, build TlsAcceptor
churust-core/Cargo.toml            ADD serde, serde_urlencoded, toml; optional rustls deps under [features] tls
churust-macros/                    NEW crate — #[churust::main]
churust/src/lib.rs                 prelude: add extractors, State, Config, macro re-export
churust/Cargo.toml                 ADD churust-macros dep
examples/hello/src/main.rs         UPDATE to showcase extractors + #[churust::main] + state
Cargo.toml                         ADD churust-macros to workspace members
```

---

## Task 1: Extractor traits + arity-based Handler family

> **IMPLEMENTED — STATUS NOTE (post-hoc correction).** This task is complete and green on disk, but the implementation deviated from (and improved on) the macro shown below, **correctly**. The literal `impl<...> Handler for F` macro in Step 2 does NOT compile on stable Rust: the coherence checker treats per-arity `impl Handler for F where F: Fn(..)` blocks as mutually overlapping (E0119), since one closure type could implement several `Fn` arities. The shipped solution uses the standard **axum-style marker pattern**: a `HandlerFn<Marker>` bridge trait (per-arity, marker-disambiguated) + a `HandlerFnAdapter` + an `IntoHandler<Marker>` conversion. Public `Handler`/`BoxHandler`/`boxed<H: Handler>` keep their spec signatures; `impl Handler for BoxHandler` was added so a boxed handler is itself a handler. The router's `get/post/put/delete/method` take `H: IntoHandler<Marker>`, so the **user-facing routing API is unchanged** — `r.get("/x", |Path(id): Path<u64>| async move { .. })` works seamlessly. The only internal change: the low-level `boxed(closure)` now requires `closure.into_handler()` first (the router does this for you). `IntoHandler` is re-exported at the crate root. Steps below are kept for historical context.

**Files:**
- Create: `churust-core/src/extract.rs`
- Modify: `churust-core/src/handler.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write the extractor traits + `Call: FromCall` + a test extractor**

`churust-core/src/extract.rs`:
```rust
//! Extractors: typed handler arguments derived from a `Call`.
//!
//! Two traits, mirroring the proven axum split:
//! - `FromCallParts`: borrows `&mut Call`, usable in ANY argument position.
//! - `FromCall`: consumes the `Call`, usable ONLY as the LAST argument.
//!
//! Every `FromCallParts` is also a `FromCall` (blanket impl), so a parts-style
//! extractor may also appear last. `Call` itself is `FromCall` (consuming), so
//! a `|call: Call|` handler is just the arity-1, last-arg case.

use crate::call::Call;
use crate::error::Result;
use async_trait::async_trait;

/// Extract from a borrowed `&mut Call`. Order-independent; may run before the
/// body is consumed.
#[async_trait]
pub trait FromCallParts: Sized + Send {
    async fn from_call_parts(call: &mut Call) -> Result<Self>;
}

/// Extract by consuming the `Call`. Last-argument only. Body-consuming
/// extractors (e.g. JSON in Plan 3) implement this directly.
#[async_trait]
pub trait FromCall: Sized + Send {
    async fn from_call(call: Call) -> Result<Self>;
}

/// Any parts extractor can also be the final argument.
#[async_trait]
impl<T: FromCallParts> FromCall for T {
    async fn from_call(mut call: Call) -> Result<Self> {
        T::from_call_parts(&mut call).await
    }
}

/// The whole `Call` as the final argument (Ktor call-style base case).
/// NOTE: deliberately NOT `FromCallParts` — that would conflict with the
/// blanket impl above.
#[async_trait]
impl FromCall for Call {
    async fn from_call(call: Call) -> Result<Self> {
        Ok(call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, Method, Uri};

    // A trivial parts extractor used to prove the machinery.
    struct MethodName(String);

    #[async_trait]
    impl FromCallParts for MethodName {
        async fn from_call_parts(call: &mut Call) -> Result<Self> {
            Ok(MethodName(call.method().as_str().to_string()))
        }
    }

    fn call() -> Call {
        Call::new(Method::GET, "/".parse::<Uri>().unwrap(), HeaderMap::new(), Bytes::new())
    }

    #[tokio::test]
    async fn parts_extractor_runs() {
        let mut c = call();
        let m = MethodName::from_call_parts(&mut c).await.unwrap();
        assert_eq!(m.0, "GET");
    }

    #[tokio::test]
    async fn call_is_from_call() {
        let c = call();
        let back = Call::from_call(c).await.unwrap();
        assert_eq!(back.method(), &Method::GET);
    }
}
```

- [ ] **Step 2: Replace the blanket impl in `handler.rs` with the arity family**

Open `churust-core/src/handler.rs`. **Delete** the existing single blanket impl (the `impl<F, Fut, R> Handler for F where F: Fn(Call) -> Fut ...` block). Keep the `Handler` trait, `HandlerFuture`, `BoxHandler`, and `boxed`. Add the following macro-generated family (place after the trait definition):

```rust
use crate::extract::{FromCall, FromCallParts};

/// Generates `Handler` impls for closures of a given arity. The LAST type
/// parameter is `FromCall` (consuming); all earlier ones are `FromCallParts`
/// (borrowing). Handlers must be `Clone` so the closure can be moved into the
/// `'static` response future.
macro_rules! impl_handler {
    // zero-argument case
    () => {
        impl<F, Fut, R> Handler for F
        where
            F: Fn() -> Fut + Clone + Send + Sync + 'static,
            Fut: std::future::Future<Output = R> + Send + 'static,
            R: IntoResponse + 'static,
        {
            fn handle(&self, _call: Call) -> HandlerFuture {
                let f = self.clone();
                Box::pin(async move { f().await.into_response() })
            }
        }
    };
    // one or more arguments: $($P)* are the parts extractors, $L is the last.
    ($($P:ident),*) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, R, $($P,)* L> Handler for F
        where
            F: Fn($($P,)* L) -> Fut + Clone + Send + Sync + 'static,
            Fut: std::future::Future<Output = R> + Send + 'static,
            R: IntoResponse + 'static,
            $($P: FromCallParts + 'static,)*
            L: FromCall + 'static,
        {
            fn handle(&self, call: Call) -> HandlerFuture {
                let f = self.clone();
                Box::pin(async move {
                    let mut call = call;
                    $(
                        let $P = match <$P as FromCallParts>::from_call_parts(&mut call).await {
                            Ok(v) => v,
                            Err(e) => return e.into_response(),
                        };
                    )*
                    let last = match <L as FromCall>::from_call(call).await {
                        Ok(v) => v,
                        Err(e) => return e.into_response(),
                    };
                    f($($P,)* last).await.into_response()
                })
            }
        }
    };
}

impl_handler!();
impl_handler!(P1);
impl_handler!(P1, P2);
impl_handler!(P1, P2, P3);
impl_handler!(P1, P2, P3, P4);
impl_handler!(P1, P2, P3, P4, P5);
impl_handler!(P1, P2, P3, P4, P5, P6);
impl_handler!(P1, P2, P3, P4, P5, P6, P7);
impl_handler!(P1, P2, P3, P4, P5, P6, P7, P8);
```

Ensure `use crate::response::{IntoResponse, Response};` and `use crate::call::Call;` remain imported at the top of `handler.rs` (they were present in Plan 1).

- [ ] **Step 3: Update the existing handler tests**

In `handler.rs` the Plan-1 tests use `boxed(|_call: Call| async move { "hello" })` and a `Call`-arg closure. These remain valid (arity-1, `L = Call`). Add one extractor-style test to prove the family:
```rust
    // (inside `mod tests`)
    use crate::extract::FromCallParts;

    struct Greeting(&'static str);
    #[async_trait::async_trait]
    impl FromCallParts for Greeting {
        async fn from_call_parts(_call: &mut Call) -> crate::Result<Self> {
            Ok(Greeting("hi"))
        }
    }

    #[tokio::test]
    async fn extractor_style_handler_works() {
        let h = boxed(|g: Greeting| async move { g.0 });
        let res = h.handle(sample_call()).await;
        assert_eq!(res.body, bytes::Bytes::from("hi"));
    }
```

- [ ] **Step 4: Wire module**

In `churust-core/src/lib.rs`:
```rust
pub mod extract;
pub use extract::{FromCall, FromCallParts};
```

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core extract:: handler::`
Then: `cargo test -p churust-core` (ensure router/app/test still green — Plan-1 handlers must still compile).
Expected: PASS.

---

## Task 2: `Path<T>` extractor

**Files:**
- Modify: `churust-core/src/extract.rs`

- [ ] **Step 1: Write `Path<T>` + tests**

Add to `churust-core/src/extract.rs` (after the trait impls):
```rust
use crate::error::Error;

/// Extracts a single path parameter, parsed to `T`. For a route
/// `"/users/{id}"`, `Path::<u64>` reads `{id}`. When a handler has exactly one
/// path param this reads it positionally; for multiple params, use
/// `call.param::<T>("name")` directly in Plan 2.
#[derive(Debug, Clone)]
pub struct Path<T>(pub T);

#[async_trait]
impl<T> FromCallParts for Path<T>
where
    T: std::str::FromStr + Send,
    T::Err: std::fmt::Display,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let mut params = call.params_iter();
        let (_name, raw) = params
            .next()
            .ok_or_else(|| Error::bad_request("no path parameter to extract"))?;
        let value = raw
            .parse::<T>()
            .map_err(|e| Error::bad_request(format!("bad path param: {e}")))?;
        Ok(Path(value))
    }
}
```

This needs a `params_iter()` accessor on `Call`. Add it in **Task 4? No — add now**. In `churust-core/src/call.rs`, inside `impl Call`, add:
```rust
    /// Iterate captured path params in insertion order (name, value).
    pub fn params_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.params.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
```
Because `params` is a `HashMap`, ordering is not guaranteed; for single-param `Path<T>` this is fine. (Multi-param positional extraction is intentionally out of scope; `call.param("name")` covers it.)

Add tests to `extract.rs`:
```rust
    use std::collections::HashMap;

    #[tokio::test]
    async fn path_extracts_single_param() {
        let mut c = call();
        let mut p = HashMap::new();
        p.insert("id".to_string(), "42".to_string());
        c.set_params(p);
        let Path(id) = Path::<u64>::from_call_parts(&mut c).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn path_bad_value_is_400() {
        let mut c = call();
        let mut p = HashMap::new();
        p.insert("id".to_string(), "notnum".to_string());
        c.set_params(p);
        let err = Path::<u64>::from_call_parts(&mut c).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::BAD_REQUEST);
    }
```

- [ ] **Step 2: Export**

In `churust-core/src/lib.rs` update:
```rust
pub use extract::{FromCall, FromCallParts, Path};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core extract::`
Expected: PASS.

---

## Task 3: `Query<T>` extractor

**Files:**
- Modify: `churust-core/Cargo.toml`
- Modify: `churust-core/src/extract.rs`

- [ ] **Step 1: Add deps**

In `churust-core/Cargo.toml` `[dependencies]` add:
```toml
serde = { version = "1", features = ["derive"] }
serde_urlencoded = "0.7"
```
Run: `cd churust-core && cargo add serde --features derive && cargo add serde_urlencoded; cd ..`

- [ ] **Step 2: Write `Query<T>` + tests**

Add to `churust-core/src/extract.rs`:
```rust
use serde::de::DeserializeOwned;

/// Deserializes the URL query string into `T` (via serde_urlencoded).
#[derive(Debug, Clone)]
pub struct Query<T>(pub T);

#[async_trait]
impl<T> FromCallParts for Query<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let q = call.query_string();
        let value = serde_urlencoded::from_str::<T>(q)
            .map_err(|e| Error::bad_request(format!("invalid query string: {e}")))?;
        Ok(Query(value))
    }
}
```

Tests in `extract.rs`:
```rust
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Pager { page: u32, q: String }

    fn call_with_query(qs: &str) -> Call {
        Call::new(
            Method::GET,
            format!("/s?{qs}").parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
    }

    #[tokio::test]
    async fn query_deserializes() {
        let mut c = call_with_query("page=2&q=rust");
        let Query(p) = Query::<Pager>::from_call_parts(&mut c).await.unwrap();
        assert_eq!(p, Pager { page: 2, q: "rust".into() });
    }

    #[tokio::test]
    async fn query_missing_field_is_400() {
        let mut c = call_with_query("q=rust");
        let err = Query::<Pager>::from_call_parts(&mut c).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::BAD_REQUEST);
    }
```

- [ ] **Step 3: Export**
```rust
pub use extract::{FromCall, FromCallParts, Path, Query};
```

- [ ] **Step 4: Checkpoint**

Run: `cargo test -p churust-core extract::`
Expected: PASS.

---

## Task 4: App state (`StateMap` + `State<T>`)

**Files:**
- Create: `churust-core/src/state.rs`
- Modify: `churust-core/src/call.rs`
- Modify: `churust-core/src/app.rs`
- Modify: `churust-core/src/extract.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write `StateMap` + tests**

`churust-core/src/state.rs`:
```rust
//! Type-keyed shared application state (a minimal DI registry).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// A registry mapping a type to a single shared value of that type.
#[derive(Clone, Default)]
pub struct StateMap {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl StateMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) the value for type `T`.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Get a shared handle to the value for type `T`, if present.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|arc| arc.clone().downcast::<T>().ok())
    }
}

impl std::fmt::Debug for StateMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateMap").field("len", &self.map.len()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_retrieves_by_type() {
        let mut m = StateMap::new();
        m.insert(7u32);
        m.insert(String::from("hello"));
        assert_eq!(*m.get::<u32>().unwrap(), 7);
        assert_eq!(m.get::<String>().unwrap().as_str(), "hello");
        assert!(m.get::<i64>().is_none());
    }
}
```

- [ ] **Step 2: Add state handle to `Call`**

In `churust-core/src/call.rs`:
- Add import: `use crate::state::StateMap;` and `use std::sync::Arc;` (Arc may already be needed; add if missing).
- Add field to the struct: `state: Arc<StateMap>,`
- In `Call::new`, initialize it: `state: Arc::new(StateMap::default()),`
- Add methods inside `impl Call`:
```rust
    /// Injected by `App::process` before the pipeline runs.
    pub(crate) fn set_state(&mut self, state: Arc<StateMap>) {
        self.state = state;
    }

    /// Shared handle to application state of type `T`, if registered.
    pub fn state<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.state.get::<T>()
    }
```
Add a test in `call.rs` `mod tests`:
```rust
    #[test]
    fn state_round_trips() {
        let mut c = call("/", "");
        let mut sm = StateMap::default();
        sm.insert(99u32);
        c.set_state(std::sync::Arc::new(sm));
        assert_eq!(*c.state::<u32>().unwrap(), 99);
    }
```
(Add `use crate::state::StateMap;` to the test module if not already in scope.)

- [ ] **Step 3: `State<T>` extractor**

Add to `churust-core/src/extract.rs`:
```rust
use std::sync::Arc;

/// Extracts a shared handle to application state of type `T`. 500 if the state
/// was never registered via `AppBuilder::state`.
#[derive(Debug, Clone)]
pub struct State<T>(pub Arc<T>);

#[async_trait]
impl<T> FromCallParts for State<T>
where
    T: Send + Sync + 'static,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        match call.state::<T>() {
            Some(v) => Ok(State(v)),
            None => Err(Error::internal(format!(
                "missing application state: {}",
                std::any::type_name::<T>()
            ))),
        }
    }
}

impl<T> std::ops::Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
```

- [ ] **Step 4: `AppBuilder::state()` + injection in `process`**

In `churust-core/src/app.rs`:
- Add `use crate::state::StateMap;` and ensure `use std::sync::Arc;` present.
- Add field to `AppBuilder`: `state: StateMap,` and initialize in `AppBuilder::new`: `state: StateMap::default(),`.
- Add builder method:
```rust
    /// Register a shared application-state value (retrieved later via `State<T>`).
    pub fn state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state.insert(value);
        self
    }
```
- Add field to `AppInner`: `state: Arc<StateMap>,` and in `build()` set `state: Arc::new(self.state)`.
- In `App::process`, inject state into the `Call` before running the pipeline. Modify the inner async block so that after `let call = Call::new(...)` you do:
```rust
            let mut call = Call::new(method, uri, headers, body);
            call.set_state(app.inner.state.clone());
            app.run_pipeline(call).await
```

- [ ] **Step 5: Export**

In `churust-core/src/lib.rs`:
```rust
pub mod state;
pub use state::StateMap;
pub use extract::{FromCall, FromCallParts, Path, Query, State};
```

- [ ] **Step 6: Add an end-to-end state test**

Append to `churust-core/src/app.rs` `mod tests`:
```rust
    #[tokio::test]
    async fn state_extractor_end_to_end() {
        use crate::extract::State;
        #[derive(Clone)]
        struct Counter(u32);

        let app = Churust::server()
            .state(Counter(5))
            .routing(|r| {
                r.get("/n", |s: State<Counter>| async move {
                    format!("n={}", s.0 .0)
                });
            })
            .build();
        let res = get(&app, "/n").await;
        assert_eq!(res.body, Bytes::from("n=5"));
    }
```

- [ ] **Step 7: Checkpoint**

Run: `cargo test -p churust-core state:: extract:: call:: app::`
Then: `cargo test -p churust-core`
Expected: PASS.

---

## Task 5: `Header<T>` extractor

**Files:**
- Modify: `churust-core/src/extract.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Write `Header<T>` + tests**

Add to `churust-core/src/extract.rs`. This is a typed single-header extractor keyed by a const name supplied via a small trait, kept simple for v1: extract a named header into a `String`-parseable `T` using a wrapper that carries the header name.

Use a simpler, ergonomic form — a `TypedHeader` is overkill for v1; provide `Header` that reads a specific header by a `&'static str` name through a generic marker is complex. Instead provide a concrete, dead-simple API: `Header(pub HeaderMap)` is redundant (Call already exposes headers). **Decision:** ship `BearerToken` as the one useful typed header for v1 and leave generic typed headers to Plan 3's auth plugin.

```rust
/// Extracts the bearer token from the `Authorization: Bearer <token>` header.
/// 401 if absent or malformed.
#[derive(Debug, Clone)]
pub struct BearerToken(pub String);

#[async_trait]
impl FromCallParts for BearerToken {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let raw = call
            .header("authorization")
            .ok_or_else(|| Error::new(http::StatusCode::UNAUTHORIZED, "missing Authorization header"))?;
        let token = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))
            .ok_or_else(|| Error::new(http::StatusCode::UNAUTHORIZED, "expected Bearer scheme"))?;
        Ok(BearerToken(token.trim().to_string()))
    }
}
```

Tests in `extract.rs`:
```rust
    fn call_with_auth(value: &str) -> Call {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(value).unwrap(),
        );
        Call::new(Method::GET, "/".parse::<Uri>().unwrap(), headers, Bytes::new())
    }

    #[tokio::test]
    async fn bearer_token_extracted() {
        let mut c = call_with_auth("Bearer abc123");
        let BearerToken(t) = BearerToken::from_call_parts(&mut c).await.unwrap();
        assert_eq!(t, "abc123");
    }

    #[tokio::test]
    async fn missing_bearer_is_401() {
        let mut c = call();
        let err = BearerToken::from_call_parts(&mut c).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::UNAUTHORIZED);
    }
```

- [ ] **Step 2: Export**
```rust
pub use extract::{BearerToken, FromCall, FromCallParts, Path, Query, State};
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core extract::`
Expected: PASS.

---

## Task 6: Configuration (`churust.toml` + env)

**Files:**
- Create: `churust-core/src/config.rs`
- Modify: `churust-core/Cargo.toml`
- Modify: `churust-core/src/app.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Add deps**

In `churust-core/Cargo.toml` add:
```toml
toml = "0.8"
```
(serde already added in Task 3.) Run: `cd churust-core && cargo add toml; cd ..`

- [ ] **Step 2: Write `Config` + load logic + tests**

`churust-core/src/config.rs`:
```rust
//! Layered configuration: defaults < `churust.toml` < env (`CHURUST_*`) < code.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerSection,
    pub tls: Option<TlsSection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSection {
    pub host: String,
    pub port: u16,
    pub max_body_bytes: usize,
    pub request_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsSection {
    pub cert: String,
    pub key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { server: ServerSection::default(), tls: None }
    }
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            max_body_bytes: 1 << 20,
            request_timeout_ms: 30_000,
        }
    }
}

impl Config {
    /// Load defaults, overlay `churust.toml` (if present at `path`), then
    /// overlay environment variables (`CHURUST_*`).
    pub fn load(path: &str) -> Self {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Config>(&text).unwrap_or_default(),
            Err(_) => Config::default(),
        };
        cfg.apply_env(|k| std::env::var(k).ok());
        cfg
    }

    /// Default file path used by `Config::load_default`.
    pub fn load_default() -> Self {
        Self::load("churust.toml")
    }

    /// Apply `CHURUST_*` overrides using the provided lookup (injected for tests).
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) {
        if let Some(v) = get("CHURUST_SERVER_HOST") {
            self.server.host = v;
        }
        if let Some(v) = get("CHURUST_SERVER_PORT").and_then(|s| s.parse().ok()) {
            self.server.port = v;
        }
        if let Some(v) = get("CHURUST_SERVER_MAX_BODY_BYTES").and_then(|s| s.parse().ok()) {
            self.server.max_body_bytes = v;
        }
        if let Some(v) = get("CHURUST_SERVER_REQUEST_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            self.server.request_timeout_ms = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.server.port, 8080);
        assert_eq!(c.server.max_body_bytes, 1 << 20);
        assert!(c.tls.is_none());
    }

    #[test]
    fn parses_toml() {
        let text = r#"
            [server]
            host = "0.0.0.0"
            port = 9090
        "#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.server.port, 9090);
        // unspecified fields fall back to defaults
        assert_eq!(c.server.max_body_bytes, 1 << 20);
    }

    #[test]
    fn env_overrides_file() {
        let mut c = Config::default();
        let env: HashMap<&str, &str> =
            [("CHURUST_SERVER_PORT", "7000")].into_iter().collect();
        c.apply_env(|k| env.get(k).map(|s| s.to_string()));
        assert_eq!(c.server.port, 7000);
    }
}
```

- [ ] **Step 3: Integrate `Config` into `AppBuilder`/`ServerConfig`**

In `churust-core/src/app.rs`:
- Extend `ServerConfig` (from Plan 1) with `request_timeout_ms: u64` and `tls: Option<crate::config::TlsSection>`. Update its `Default` to `request_timeout_ms: 30_000, tls: None`.
- Add builder methods:
```rust
    /// Apply a fully-resolved `Config` (lowest precedence vs subsequent DSL calls).
    pub fn with_config(mut self, cfg: crate::config::Config) -> Self {
        self.config.host = cfg.server.host;
        self.config.port = cfg.server.port;
        self.config.max_body_bytes = cfg.server.max_body_bytes;
        self.config.request_timeout_ms = cfg.server.request_timeout_ms;
        self.config.tls = cfg.tls;
        self
    }

    pub fn request_timeout_ms(mut self, ms: u64) -> Self {
        self.config.request_timeout_ms = ms;
        self
    }
```
- Add a convenience on `Churust`:
```rust
    /// Build a server pre-loaded from `churust.toml` + env (then chain DSL setters).
    pub fn from_config() -> AppBuilder {
        AppBuilder::new().with_config(crate::config::Config::load_default())
    }
```

- [ ] **Step 4: Export**

In `churust-core/src/lib.rs`:
```rust
pub mod config;
pub use config::{Config, ServerSection, TlsSection};
```

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p churust-core config:: app::`
Then: `cargo test -p churust-core`
Expected: PASS.

---

## Task 7: Request timeout in the engine

**Files:**
- Modify: `churust-core/src/engine.rs`
- Modify: `churust-core/src/app.rs` (pass timeout into engine via config)

- [ ] **Step 1: Apply a per-request timeout in the handler adapter**

In `churust-core/src/engine.rs`, read the timeout from config and wrap `app.process(...)`:
- Near where `max_body` is read in `serve`, also capture `let timeout_ms = app.config().request_timeout_ms;` and move it into the `service_fn` closure alongside `max_body`.
- Change the `handle` signature to `async fn handle(app: App, req: HyperRequest<Incoming>, max_body: usize, timeout_ms: u64) -> Result<HyperResponse<Full<Bytes>>, Infallible>`.
- Replace the `let res = app.process(...).await;` line with:
```rust
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
```
- Update the `service_fn` call site to pass `timeout_ms`.

- [ ] **Step 2: Test the timeout via TestClient-independent path**

The engine timeout is integration-level. Add an integration test `churust-core/tests/timeout.rs`:
```rust
use churust_core::{Call, Churust};
use std::time::Duration;

#[tokio::test]
async fn slow_handler_times_out() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .request_timeout_ms(100)
        .routing(|r| {
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                "done"
            });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_with_shutdown(async move { let _ = rx.await; }).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET /slow HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 408"), "expected 408, got: {text}");

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}
```

- [ ] **Step 3: Checkpoint**

Run: `cargo test -p churust-core --test timeout`
Then: `cargo test -p churust-core`
Expected: PASS.

---

## Task 8: Opt-in TLS (rustls) behind a `tls` feature

**Files:**
- Create: `churust-core/src/tls.rs`
- Modify: `churust-core/Cargo.toml`
- Modify: `churust-core/src/engine.rs`
- Modify: `churust-core/src/app.rs`
- Modify: `churust-core/src/lib.rs`

- [ ] **Step 1: Add optional deps + feature**

In `churust-core/Cargo.toml`:
```toml
[dependencies]
# ... existing ...
rustls = { version = "0.23", optional = true }
tokio-rustls = { version = "0.26", optional = true }
rustls-pemfile = { version = "2", optional = true }

[features]
default = []
tls = ["dep:rustls", "dep:tokio-rustls", "dep:rustls-pemfile"]
```
Run: `cd churust-core && cargo add rustls --optional && cargo add tokio-rustls --optional && cargo add rustls-pemfile --optional; cd ..` then hand-edit the `[features]` block as above.

- [ ] **Step 2: Write the cert/key loader + acceptor**

`churust-core/src/tls.rs`:
```rust
//! TLS support (feature `tls`). Loads a PEM cert chain + private key and builds
//! a `tokio_rustls::TlsAcceptor` with rustls' safe defaults.

#![cfg(feature = "tls")]

use std::io::{self, BufReader};
use std::sync::Arc;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

pub fn acceptor_from_pem(cert_path: &str, key_path: &str) -> io::Result<TlsAcceptor> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

fn load_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no private key found"))
}
```

- [ ] **Step 3: Engine TLS branch**

In `churust-core/src/engine.rs`, gate a TLS-aware accept path. Add at the top:
```rust
#[cfg(feature = "tls")]
use crate::tls::acceptor_from_pem;
```
After binding the listener in `serve`, build an optional acceptor:
```rust
    #[cfg(feature = "tls")]
    let tls_acceptor = match &app.config().tls {
        Some(t) => Some(acceptor_from_pem(&t.cert, &t.key)?),
        None => None,
    };
```
In the accept arm, wrap the stream when TLS is configured. Replace the `let io = TokioIo::new(stream);` + service/connection block with a helper that serves either a plain or TLS stream. The simplest robust form: factor the "serve this AsyncRead+AsyncWrite stream" into a closure used by both branches. For Plan 2, implement:
```rust
                #[cfg(feature = "tls")]
                {
                    if let Some(acceptor) = tls_acceptor.clone() {
                        let app = app.clone();
                        let conn_builder_fut = async move {
                            match acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    serve_stream(app, TokioIo::new(tls_stream), max_body, timeout_ms, &graceful).await;
                                }
                                Err(_) => {}
                            }
                        };
                        tokio::spawn(conn_builder_fut);
                        continue;
                    }
                }
                serve_stream(app.clone(), TokioIo::new(stream), max_body, timeout_ms, &graceful).await;
```
And extract a `serve_stream` free function:
```rust
async fn serve_stream<I>(
    app: App,
    io: I,
    max_body: usize,
    timeout_ms: u64,
    graceful: &hyper_util::server::graceful::GracefulShutdown,
) where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let svc = hyper::service::service_fn(move |req: HyperRequest<Incoming>| {
        let app = app.clone();
        async move { handle(app, req, max_body, timeout_ms).await }
    });
    let conn = hyper::server::conn::http1::Builder::new().serve_connection(io, svc);
    let fut = graceful.watch(conn);
    tokio::spawn(async move {
        let _ = fut.await;
    });
}
```
Refactor the existing plaintext accept block to call `serve_stream(app.clone(), TokioIo::new(stream), max_body, timeout_ms, &graceful).await;` so both branches share it. (The `graceful.watch` borrow is short-lived; passing `&graceful` is fine because `serve_stream` spawns and returns.)

> Implementer note: if borrow/lifetime issues arise passing `&graceful`, change `serve_stream` to take ownership of the watched future instead — i.e. build `conn` in `serve_stream` and `graceful.watch` at the call site. Preserve behavior: every connection is watched for graceful drain.

- [ ] **Step 4: `AppBuilder::tls()`**

In `churust-core/src/app.rs`:
```rust
    /// Enable TLS from PEM files (requires the `tls` feature at build time).
    pub fn tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.config.tls = Some(crate::config::TlsSection {
            cert: cert_path.into(),
            key: key_path.into(),
        });
        self
    }
```

- [ ] **Step 5: Wire module + test cert loading**

In `churust-core/src/lib.rs`:
```rust
#[cfg(feature = "tls")]
pub mod tls;
```
Add a feature-gated unit test that generates a self-signed cert at runtime is heavy; instead test the **error path** (no extra deps) in `tls.rs`:
```rust
#[cfg(all(test, feature = "tls"))]
mod tests {
    use super::*;

    #[test]
    fn missing_files_error_cleanly() {
        let err = acceptor_from_pem("/nonexistent/cert.pem", "/nonexistent/key.pem").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
```

- [ ] **Step 6: Checkpoint**

Run: `cargo test -p churust-core` (default features — TLS code compiles out).
Then: `cargo test -p churust-core --features tls` (TLS path compiles + error test passes).
Then: `cargo clippy -p churust-core --all-targets --features tls -- -D warnings`.
Expected: PASS.

---

## Task 9: `#[churust::main]` macro crate

**Files:**
- Create: `churust-macros/Cargo.toml`
- Create: `churust-macros/src/lib.rs`
- Modify: `Cargo.toml` (workspace members + deps)
- Modify: `churust/Cargo.toml` + `churust/src/lib.rs` (re-export)

- [ ] **Step 1: Add workspace members + deps**

In root `Cargo.toml`:
```toml
members = ["churust-core", "churust-macros", "churust", "examples/hello"]
```
Add to `[workspace.dependencies]`:
```toml
syn = { version = "2", features = ["full"] }
quote = "1"
proc-macro2 = "1"
```

- [ ] **Step 2: Macro crate manifest**

`churust-macros/Cargo.toml`:
```toml
[package]
name = "churust-macros"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lib]
proc-macro = true

[dependencies]
syn.workspace = true
quote.workspace = true
proc-macro2.workspace = true
```

- [ ] **Step 3: Implement `#[churust::main]`**

`churust-macros/src/lib.rs`:
```rust
//! Procedural macros for Churust.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// Marks the async entry point. Expands `async fn main()` into a synchronous
/// `main` that builds a multi-threaded tokio runtime and blocks on the body.
///
/// Requires `tokio` to be a dependency of the using crate.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    if input.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input.sig.fn_token,
            "#[churust::main] requires an `async fn`",
        )
        .to_compile_error()
        .into();
    }

    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;
    let output = &sig.output;
    let ident = &sig.ident;

    // Rebuild a non-async signature with the same name/return type.
    let expanded = quote! {
        #(#attrs)*
        #vis fn #ident() #output {
            let __rt = ::tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            __rt.block_on(async move #body)
        }
    };

    expanded.into()
}
```

- [ ] **Step 4: Re-export from the umbrella crate**

In `churust/Cargo.toml` add:
```toml
churust-macros = { path = "../churust-macros", version = "0.1.0" }
```
In `churust/src/lib.rs` add (top-level):
```rust
/// The async entry-point attribute. See `churust-macros`.
pub use churust_macros::main;
```

- [ ] **Step 5: Compile-and-run test via the example (Task 10 also exercises it)**

Add an integration test `churust/tests/macro_main.rs` that uses the macro indirectly is awkward (the macro generates `main`). Instead verify expansion compiles via a doc-test in `churust/src/lib.rs`:
```rust
//! ```no_run
//! #[churust::main]
//! async fn main() -> std::io::Result<()> {
//!     use churust::prelude::*;
//!     let _app = Churust::server().build();
//!     Ok(())
//! }
//! ```
```
(Add this as an additional doc-comment block in `churust/src/lib.rs`.) The `hello` example (Task 10) provides the real run-time exercise.

- [ ] **Step 6: Checkpoint**

Run: `cargo build -p churust-macros && cargo build -p churust && cargo test -p churust --doc`
Expected: PASS.

---

## Task 10: Showcase extractors + macro + state in the hello example; prelude + full suite

**Files:**
- Modify: `churust/src/lib.rs` (prelude exports)
- Modify: `examples/hello/src/main.rs`

- [ ] **Step 1: Expand the prelude**

In `churust/src/lib.rs`, update the `prelude` module:
```rust
pub mod prelude {
    pub use churust_core::{
        App, AppBuilder, BearerToken, Call, Churust, Config, Error, FromCall, FromCallParts,
        IntoHandler, IntoResponse, Middleware, Next, Path, Plugin, Query, Response, Result,
        Router, State,
    };
    pub use crate::main; // #[churust::main]
    pub use http::{Method, StatusCode};
}
```

- [ ] **Step 2: Rewrite the hello example to show the hybrid API**

`examples/hello/src/main.rs`:
```rust
use churust::prelude::*;
use serde::Deserialize;

#[derive(Clone)]
struct Greeter {
    prefix: String,
}

#[derive(Deserialize)]
struct Search {
    q: String,
}

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::from_config() // loads churust.toml + env, then DSL overrides below
        .host("127.0.0.1")
        .port(8080)
        .state(Greeter { prefix: "Hello".into() })
        .routing(|r| {
            // call-style (Plan 1 still works)
            r.get("/", |_call: Call| async { "Churust 🌀" });

            // extractor-style: path param
            r.get("/users/{id}", |Path(id): Path<u64>| async move {
                format!("user #{id}")
            });

            // extractor-style: query + state
            r.get("/greet", |Query(s): Query<Search>, g: State<Greeter>| async move {
                format!("{}, {}!", g.prefix, s.q)
            });
        })
        .start()
        .await
}
```
Add `serde` to `examples/hello/Cargo.toml`:
```toml
serde = { workspace = true }
```
(Ensure `serde` is in `[workspace.dependencies]` from Task 3; it is.)

- [ ] **Step 3: Full-suite checkpoint**

Run:
```
cargo test
cargo test -p churust-core --features tls
cargo clippy --all-targets -- -D warnings
cargo clippy -p churust-core --all-targets --features tls -- -D warnings
cargo build -p hello
```
Expected: all PASS / clean.

Optional manual smoke (no commit): `cargo run -p hello &` then `curl -s "localhost:8080/greet?q=World"` → `Hello, World!`; `curl -s localhost:8080/users/7` → `user #7`; stop the process.

---

## Self-Review

**Spec coverage (vs design spec §5–§13):**
- §5.2 Hybrid handlers / extractors (`FromCall`, blanket impls, `Path`/`Query`/`State`) → Tasks 1–5. `Json<T>` extractor is **Plan 3** (json plugin). `Header<T>` generalized → shipped as `BearerToken` for v1 (documented narrowing; full typed headers deferred). ✓ (with noted narrowing)
- §10 App state / DI (`StateMap`, `State<T>`, `.state()`) → Task 4. ✓
- §11 Config (`churust.toml` + env + DSL precedence) → Task 6. ✓
- §12 Security: request timeout → Task 7; TLS via rustls → Task 8. (Body limit + panic isolation already in Plan 1.) ✓
- §13 Macros (`#[churust::main]`) → Task 9. ✓
- §3 edition/MSRV unchanged. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to". The Task-5 narrative explains a scope decision (BearerToken over generic typed headers) and ships complete code. Task-8 Step-3 includes an explicit implementer fallback note for the `graceful` borrow — that is guidance with working primary code, not a placeholder.

**Type consistency:**
- `FromCallParts::from_call_parts(&mut Call) -> Result<Self>` and `FromCall::from_call(Call) -> Result<Self>` (T1) are used identically by `Path`/`Query`/`State`/`BearerToken` (T2–T5) and by the handler family (T1). ✓
- `Handler::handle(&self, Call) -> HandlerFuture` is UNCHANGED from Plan 1 → router/pipeline untouched. Only the blanket impl is replaced. ✓
- `Call::new(Method, Uri, HeaderMap, Bytes)` signature preserved (T4 adds a field with a default + `set_state` setter). Existing call sites unaffected. ✓
- `StateMap::insert<T>/get<T>` (T4 state.rs) match `Call::state<T>()` and `State<T>` extractor usage. ✓
- `ServerConfig` gains `request_timeout_ms`/`tls`; `Config` → `with_config` maps fields 1:1 (host/port/max_body_bytes/request_timeout_ms/tls). ✓
- Engine `handle(app, req, max_body, timeout_ms)` (T7) matches the `service_fn` call sites in T7 and the `serve_stream` helper in T8. ✓
- `Churust::server()` / `Churust::from_config()` / `AppBuilder` chain methods (`.state`, `.with_config`, `.request_timeout_ms`, `.tls`, `.routing`, `.start`) are all defined before use in the hello example (T10). ✓

**Risk notes:**
- The arity-based `Handler` family compiles only if the LAST generic is `FromCall` and earlier ones are `FromCallParts`, AND `Call` is NOT `FromCallParts`. The implementer must not add `impl FromCallParts for Call`.
- `async-trait` is required for `FromCall`/`FromCallParts` (object-safe-free here, but trait `async fn` needs it on stable until the resolved toolchain supports `async fn in trait` for these bounds; using `async-trait` is the safe choice and is already a dependency).
- rustls 0.23 / tokio-rustls 0.26 API (`pki_types`, `ServerConfig::builder().with_no_client_auth().with_single_cert`) is current; if `cargo add` resolves a different minor, adapt per compiler errors while preserving "safe defaults, single cert, no client auth".
- `#[churust::main]` output references `::tokio`; the using crate must depend on `tokio` (the hello example does). Document this for end users.

---

## Execution Handoff

Execute sequentially, task by task: implement, then spec review, then quality review. After Plan 2 is green, Plan 3 (json/logging/cors/auth plugins, named pipeline phases) follows.
