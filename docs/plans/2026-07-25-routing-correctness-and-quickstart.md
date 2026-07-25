# Churust Routing Correctness + One-Dependency Quickstart — Implementation Plan (v0.2.0)

> **Builds on** Churust 0.1.1 (published, 189 tests green). Implements
> [`docs/design/2026-07-25-routing-correctness-and-quickstart-design.md`](../design/2026-07-25-routing-correctness-and-quickstart-design.md).
> Phase 1 of 3.

**Goal:** Make `churust = "0.2"` compile on its own, and make the router
HTTP-conformant — wildcards reachable, `HEAD`/`OPTIONS` handled, path parameters
percent-decoded without opening a traversal hole.

**Architecture:** Four independent changes over the existing crates. The umbrella
re-exports `tokio` and the `main` macro targets that re-export. `Router::route`
gains a wildcard fallback stage. `HEAD`/`OPTIONS` synthesis lives at the dispatch
site in `app.rs`, keeping `Router` a pure lookup structure. Path decoding is a
new segment-scoped function applied after splitting, never before.

**Tech Stack:** Rust 2021 (MSRV 1.96), tokio, hyper 1.x, syn/quote.

## Global Constraints

- MSRV **1.96**; CI pins `1.96.0`. Do not use newer language features.
- `RUSTFLAGS: -D warnings` and `RUSTDOCFLAGS: -D warnings` — warnings fail.
- `#![deny(missing_docs)]` is set on `churust` and `churust-core`; every new
  public item needs a doc comment.
- All seven crates share one version from `[workspace.package]`. **Never edit a
  crate version by hand**; the release tooling owns it.
- **No git commits.** Every task ends with a Checkpoint — run the tests, confirm
  green, stop. Staging and committing are the user's.
- No new dependencies without an explicit note in the task.
- Path decoding order is normative: split → decode → reject separator →
  sanitize → canonicalize. Never decode before splitting.

## File Structure

```
churust/src/lib.rs                    pub use tokio + __private module
churust/tests/macro_main.rs           NEW — macro coverage moved from macros crate
churust-macros/src/lib.rs             emit ::churust::__private::tokio; doctests -> text
churust/Cargo.toml                    tokio features, fs gating
churust-core/Cargo.toml               tokio features
Cargo.toml                            workspace tokio features
examples/*/Cargo.toml                 drop tokio dependency
churust-core/src/router.rs            wildcard fallback in route()
churust-core/src/app.rs               HEAD fallback + auto-OPTIONS at dispatch
churust-core/src/path.rs              NEW — decode_path_segment
churust-core/src/lib.rs               register the path module
churust-core/src/fs.rs                separator rejection before sanitize
churust-core/tests/head_options.rs    NEW — wire-level HEAD/OPTIONS
churust-core/tests/traversal.rs       NEW — encoded traversal suite
README.md, CHANGELOG.md               docs
```

---

## Task 1: One-dependency quickstart

**Files:**
- Modify: `churust/src/lib.rs:97-101`
- Modify: `churust-macros/src/lib.rs:105-155`
- Modify: `examples/hello/Cargo.toml`
- Create: `churust/tests/macro_main.rs`

**Interfaces:**
- Produces: `churust::tokio` (public re-export) and `churust::__private::tokio`
  (hidden; the path the macro expands to). Task 2 depends on the feature set
  behind these.

- [ ] **Step 1: Write the failing test by removing the crutch**

The defect only reproduces in a crate that does *not* depend on tokio. Inside
`churust`'s own tests `::tokio` resolves, so the guard has to be a downstream
crate. `examples/hello` is exactly that.

In `examples/hello/Cargo.toml`, delete the tokio line so it reads:

```toml
[dependencies]
churust = { workspace = true }
serde = { workspace = true }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo build -p hello`
Expected: FAIL with

```
error[E0433]: cannot find `tokio` in the crate root
 --> examples/hello/src/main.rs:3:1
```

- [ ] **Step 3: Re-export tokio from the umbrella**

In `churust/src/lib.rs`, immediately after the `pub use churust_macros::main;`
block (line 101), add:

```rust
/// The tokio runtime Churust is built on, re-exported so applications do not
/// need their own dependency on it.
///
/// ```
/// # async fn example() {
/// churust::tokio::time::sleep(std::time::Duration::from_millis(1)).await;
/// # }
/// ```
pub use tokio;

/// Implementation detail: the path `#[churust::main]` expands to.
///
/// Not a stable API. Use [`tokio`] instead.
#[doc(hidden)]
pub mod __private {
    /// Re-export used by the generated runtime in `#[churust::main]`.
    pub use tokio;
}
```

- [ ] **Step 4: Point the macro at the re-export**

In `churust-macros/src/lib.rs`, change the expansion (line 149) from
`::tokio::runtime::Builder` to:

```rust
    let expanded = quote! {
        #(#attrs)*
        #vis fn #ident() #output {
            let __rt = ::churust::__private::tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            __rt.block_on(async move #body)
        }
    };
```

- [ ] **Step 5: Convert the macro crate's doctests to illustrations**

`churust-macros` cannot compile the new expansion — `::churust` does not resolve
from inside it, because the dependency runs the other way. Change every fenced
block in `churust-macros/src/lib.rs` that invokes `#[churust::main]` or shows the
expansion (the blocks at roughly lines 12, 70, 80, 97, 109) from ` ```rust ` /
` ```no_run ` to ` ```text `.

Update the expansion illustration at line 109-120 to show the real output:

```text
fn main() -> std::io::Result<()> {
    let __rt = ::churust::__private::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    __rt.block_on(async move {
        do_something().await
    })
}
```

- [ ] **Step 6: Restore the lost coverage in the umbrella**

Create `churust/tests/macro_main.rs`:

```rust
//! Coverage for `#[churust::main]`, which cannot be doctested from
//! `churust-macros` because the expansion names `::churust`.

#[churust::main]
async fn returns_unit() {
    let v = churust::tokio::task::spawn(async { 21 * 2 }).await.unwrap();
    assert_eq!(v, 42);
}

#[churust::main]
async fn returns_result() -> std::io::Result<()> {
    Ok(())
}

#[test]
fn macro_builds_a_runtime_and_runs_the_body() {
    returns_unit();
    returns_result().expect("result-returning main should succeed");
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo build -p hello`
Expected: PASS — this is the E0433 regression guard.

Run: `cargo test -p churust --test macro_main`
Expected: PASS, 1 test.

Run: `cargo test -p churust-macros`
Expected: PASS. Doctest count drops as blocks became `text`.

- [ ] **Step 8: Checkpoint**

Run: `cargo test --workspace`
Expected: all green. Do not commit.

---

## Task 2: Right-size the tokio feature set

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `churust-core/Cargo.toml`
- Modify: `churust/Cargo.toml`
- Modify: `examples/api/Cargo.toml`, `examples/chat/Cargo.toml`, `examples/static/Cargo.toml`

**Interfaces:**
- Consumes: `churust::tokio` from Task 1.
- Produces: nothing new; narrows what `churust::tokio` exposes.

- [ ] **Step 1: Narrow the workspace default**

In the root `Cargo.toml`, replace the tokio line in `[workspace.dependencies]`:

```toml
tokio = { version = "1", features = [
  "rt-multi-thread",
  "net",
  "io-util",
  "time",
  "sync",
  "signal",
  "macros",
] }
```

Every feature is used: `rt-multi-thread` by the `main` macro and `Runtime`,
`net` by `TcpListener`, `io-util` by `AsyncReadExt`, `time` by request timeouts,
`sync` by the shutdown `oneshot`, `signal` by `ctrl_c`, `macros` by `select!`.

- [ ] **Step 2: Gate tokio's fs feature behind Churust's**

`StaticFiles` uses `tokio::fs`. In `churust-core/Cargo.toml`, keep the
workspace tokio dependency and add the feature only under `fs`:

```toml
[features]
default = []
tls = ["dep:rustls", "dep:tokio-rustls", "dep:rustls-pemfile"]
ws = ["dep:tokio-tungstenite"]
fs = ["tokio/fs"]
```

In `churust/Cargo.toml` the `fs` feature already forwards to
`churust-core/fs`, so no change is needed there.

- [ ] **Step 3: Restore what dev-dependencies need**

`#[tokio::test]` needs `macros` and `rt`, already in the narrowed set. If
`cargo test` reports a missing tokio feature in a dev context, add it to that
crate's `[dev-dependencies]` only — never widen the main dependency to satisfy a
test.

- [ ] **Step 4: Drop tokio from the remaining examples**

Remove the `tokio` line from `examples/api/Cargo.toml`,
`examples/chat/Cargo.toml`, and `examples/static/Cargo.toml`, exactly as Task 1
did for `hello`.

- [ ] **Step 5: Verify the whole feature matrix**

Each of these must pass — a missing feature usually shows up in only one:

```bash
cargo build -p hello -p api
cargo build -p chat
cargo build -p static-example
cargo test --workspace
cargo test -p churust --features full
cargo test -p churust-core --features tls
cargo test -p churust-core --features ws
cargo test -p churust-core --features fs
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS. A failure naming a tokio item means that item's feature is
missing from Step 1's list — add it and note why in the task.

- [ ] **Step 6: Checkpoint**

Run: `cargo test --workspace`
Expected: green. Do not commit.

---

## Task 3: Router wildcard fallback

**Files:**
- Modify: `churust-core/src/router.rs:168-192`
- Test: `churust-core/src/router.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `Router::route` with the precedence exact → wildcard → 405(union) →
  404. Signature unchanged: `pub fn route(&self, method: &Method, path: &str) -> Match`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `churust-core/src/router.rs`:

```rust
    fn build_shadowed() -> Router {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/files/{path...}", |c: Call| async move {
                format!("wild:{}", c.param_raw("path").unwrap_or(""))
            });
            b.get("/files/special/x", |_c: Call| async { "static" });
            b.post("/files/only-post", |_c: Call| async { "posted" });
        }
        r
    }

    #[test]
    fn wildcard_is_reachable_through_a_static_sibling() {
        let r = build_shadowed();
        match run(&r, Method::GET, "/files/special") {
            Match::Found { params, .. } => {
                assert_eq!(params.get("path").unwrap(), "special");
            }
            _ => panic!("wildcard should serve /files/special"),
        }
    }

    #[test]
    fn exact_match_still_wins_over_wildcard() {
        let r = build_shadowed();
        match run(&r, Method::GET, "/files/special/x") {
            Match::Found { params, .. } => {
                assert!(params.get("path").is_none(), "static route captured a wildcard param");
            }
            _ => panic!("expected the static route"),
        }
    }

    #[test]
    fn allow_header_unions_exact_and_wildcard_methods() {
        let r = build_shadowed();
        match run(&r, Method::DELETE, "/files/only-post") {
            Match::MethodNotAllowed { allow } => {
                assert!(allow.contains(&Method::POST), "missing exact method");
                assert!(allow.contains(&Method::GET), "missing wildcard method");
            }
            other => panic!("expected 405, got {}", match other {
                Match::Found { .. } => "Found",
                Match::NotFound => "NotFound",
                _ => "?",
            }),
        }
    }

    #[test]
    fn abandoned_branch_params_do_not_leak_into_the_wildcard() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            // `/u/{id}/edit` matches structurally for /u/7/edit but has no GET.
            b.post("/u/{id}/edit", |_c: Call| async { "edit" });
            b.get("/u/{rest...}", |_c: Call| async { "wild" });
        }
        match r.route(&Method::GET, "/u/7/edit") {
            Match::Found { params, .. } => {
                assert_eq!(params.get("rest").unwrap(), "7/edit");
                assert!(params.get("id").is_none(), "stale `id` leaked from the abandoned walk");
            }
            _ => panic!("wildcard should have matched"),
        }
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p churust-core --lib router::tests`
Expected: FAIL. `wildcard_is_reachable_through_a_static_sibling` panics with
"wildcard should serve /files/special".

- [ ] **Step 3: Rewrite `Router::route`**

Replace the body of `route` (lines 168-192) with:

```rust
    pub fn route(&self, method: &Method, path: &str) -> Match {
        let segments = split_segments(path);
        let mut params = HashMap::new();

        // 1. Exact walk. Static beats param, unchanged.
        let exact = Self::walk(&self.root, &segments, 0, &mut params);
        if let Some(node) = exact {
            if let Some(h) = node.handlers.0.get(method) {
                return Match::Found {
                    handler: h.clone(),
                    params,
                };
            }
        }
        let exact_allow: Vec<Method> = exact
            .map(|n| n.handlers.0.keys().cloned().collect())
            .unwrap_or_default();

        // 2. Wildcard fallback. The exact walk above may have written captures
        //    into `params`; they belong to the branch we just abandoned and
        //    must not reach the wildcard handler.
        params.clear();
        match Self::walk_wildcard(&self.root, &segments, 0, method, &mut params) {
            Some(found @ Match::Found { .. }) => found,
            Some(Match::MethodNotAllowed { allow: wild_allow }) => {
                let mut allow = exact_allow;
                for m in wild_allow {
                    if !allow.contains(&m) {
                        allow.push(m);
                    }
                }
                Match::MethodNotAllowed { allow }
            }
            _ if !exact_allow.is_empty() => Match::MethodNotAllowed { allow: exact_allow },
            _ => Match::NotFound,
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p churust-core --lib router::tests`
Expected: PASS, including the four pre-existing router tests.

- [ ] **Step 5: Checkpoint**

Run: `cargo test --workspace`
Expected: green. Do not commit.

---

## Task 4: HEAD falls back to GET

**Files:**
- Modify: `churust-core/src/app.rs:436-466`
- Create: `churust-core/tests/head_options.rs`

**Interfaces:**
- Consumes: `Router::route` from Task 3.
- Produces: `fn strip_body(res: Response) -> Response` — private to `app.rs`.

**Note on `Content-Length`:** Churust sets it nowhere; hyper derives it from the
body it is handed. `TestClient` bypasses hyper entirely, so a `TestClient`
assertion cannot tell you what goes on the wire. Step 5 verifies at the socket.

- [ ] **Step 1: Write the failing test**

Create `churust-core/tests/head_options.rs`:

```rust
//! HEAD and OPTIONS synthesis, exercised through TestClient.

use churust_core::{Call, Churust, TestClient};
use http::{Method, StatusCode};

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hello" });
            r.post("/submit", |_c: Call| async { "posted" });
            r.method(Method::HEAD, "/explicit", |_c: Call| async {
                (StatusCode::NO_CONTENT, "")
            });
            r.get("/explicit", |_c: Call| async { "get body" });
        })
        .build()
}

#[tokio::test]
async fn head_falls_back_to_get() {
    let res = TestClient::new(app()).request(Method::HEAD, "/").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "", "HEAD must not carry a body");
}

#[tokio::test]
async fn explicit_head_route_wins() {
    let res = TestClient::new(app())
        .request(Method::HEAD, "/explicit")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn head_on_a_post_only_route_is_still_405() {
    let res = TestClient::new(app())
        .request(Method::HEAD, "/submit")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p churust-core --test head_options`
Expected: FAIL — `head_falls_back_to_get` gets `405`, not `200`.

- [ ] **Step 3: Implement the fallback**

In `churust-core/src/app.rs`, replace the endpoint closure body (lines 441-460):

```rust
                let path = call.path().to_string();
                let method = call.method().clone();
                let mut lookup = inner.router.route(&method, &path);

                // RFC 9110 §9.3.2: HEAD must be available wherever GET is.
                // Only synthesize when no HEAD route was registered.
                let mut synthesized_head = false;
                if method == Method::HEAD && !matches!(lookup, Match::Found { .. }) {
                    if let m @ Match::Found { .. } = inner.router.route(&Method::GET, &path) {
                        lookup = m;
                        synthesized_head = true;
                    }
                }

                match lookup {
                    Match::Found { handler, params } => {
                        call.set_params(params);
                        let res = handler.handle(call).await;
                        if synthesized_head {
                            strip_body(res)
                        } else {
                            res
                        }
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
```

Add above `impl` in the same file, as a private free function:

```rust
/// Drop a response body for a synthesized `HEAD` reply, preserving status and
/// headers.
///
/// A streamed body is dropped rather than drained: draining it would do exactly
/// the work the client declined to ask for.
fn strip_body(mut res: Response) -> Response {
    res.body = crate::body::Body::empty();
    res
}
```

Add `use http::Method;` to the imports at the top of `app.rs` if not present.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p churust-core --test head_options`
Expected: PASS, 3 tests.

- [ ] **Step 5: Verify on the wire, not just through TestClient**

Add to `churust-core/tests/head_options.rs`:

Follow the existing convention in `churust-core/tests/engine_serve.rs`: bind an
ephemeral port with std, drop the listener, reuse the address.

```rust
/// TestClient never touches hyper, so it cannot show what is actually written
/// to the socket. This drives a real listener.
#[tokio::test]
async fn head_over_a_real_socket_sends_no_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/", |_c: Call| async { "hello" });
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
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("HEAD / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    assert_eq!(body, "", "HEAD response carried a body: {body:?}");
    println!("--- wire response ---\n{text}");

    let _ = tx.send(());
    let _ = server.await;
}
```

Run: `cargo test -p churust-core --test head_options -- --nocapture`

Read the printed response and record the `Content-Length` value in the task
notes. Three outcomes:

- **No `Content-Length`** — acceptable. RFC 9110 §9.3.2 permits omitting header
  fields determined while generating content. Done.
- **`Content-Length: 0`** — misleading for size probes. Fix by setting the real
  length before dropping the body: in `strip_body`, when
  `res.body.as_bytes()` is `Some(b)`, insert
  `http::header::CONTENT_LENGTH` with `b.len()` first. Re-run and confirm.
- **`Content-Length: 5`** with no body — hyper is suppressing the body itself.
  Best case. Simplify `strip_body` to a no-op for buffered bodies and note it.

Whatever the outcome, record it in `CHANGELOG.md` in Task 9 — clients that size
a resource with `HEAD` care about this.

- [ ] **Step 6: Checkpoint**

Run: `cargo test --workspace`
Expected: green. Do not commit.

---

## Task 5: Automatic OPTIONS

**Files:**
- Modify: `churust-core/src/app.rs` (the endpoint closure from Task 4)
- Modify: `churust-core/tests/head_options.rs`
- Modify: `churust-core/src/router.rs` (add `methods_for`)

**Interfaces:**
- Consumes: the endpoint closure shape from Task 4.
- Produces: `Router::methods_for(&self, path: &str) -> Vec<Method>` — the
  registered methods for a path, empty if the path matches nothing.

- [ ] **Step 1: Write the failing tests**

Append to `churust-core/tests/head_options.rs`:

```rust
#[tokio::test]
async fn options_reports_allowed_methods() {
    let res = TestClient::new(app())
        .request(Method::OPTIONS, "/")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let allow = res.header("allow").unwrap_or_default();
    assert!(allow.contains("GET"), "allow was {allow:?}");
    assert!(allow.contains("HEAD"), "HEAD is synthesized where GET exists: {allow:?}");
    assert!(allow.contains("OPTIONS"), "allow was {allow:?}");
}

#[tokio::test]
async fn options_on_an_unknown_path_is_404() {
    let res = TestClient::new(app())
        .request(Method::OPTIONS, "/nope")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p churust-core --test head_options`
Expected: FAIL — `options_reports_allowed_methods` gets `405`.

- [ ] **Step 3: Expose the registered methods from the router**

Add to `impl Router` in `churust-core/src/router.rs`:

```rust
    /// The methods registered for `path`, including any that a trailing
    /// wildcard would serve. Empty when the path matches no route.
    ///
    /// Used by the dispatcher to build the `Allow` header for an `OPTIONS`
    /// request that has no handler of its own.
    pub fn methods_for(&self, path: &str) -> Vec<Method> {
        let segments = split_segments(path);
        let mut params = HashMap::new();
        let mut out: Vec<Method> = Self::walk(&self.root, &segments, 0, &mut params)
            .map(|n| n.handlers.0.keys().cloned().collect())
            .unwrap_or_default();

        params.clear();
        if let Some(Match::MethodNotAllowed { allow }) =
            Self::walk_wildcard(&self.root, &segments, 0, &Method::TRACE, &mut params)
        {
            for m in allow {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }
```

`Method::TRACE` is a probe: no route registers it, so `walk_wildcard` always
returns `MethodNotAllowed` carrying the wildcard's full method list.

- [ ] **Step 4: Synthesize the response at dispatch**

In `app.rs`, immediately after the HEAD block added in Task 4 Step 3:

```rust
                // Automatic OPTIONS, only when no handler claimed it. CORS runs
                // in the Plugins phase and short-circuits preflight before
                // reaching this endpoint, so installed CORS keeps priority.
                if method == Method::OPTIONS && !matches!(lookup, Match::Found { .. }) {
                    let mut allow = inner.router.methods_for(&path);
                    if !allow.is_empty() {
                        if allow.contains(&Method::GET) && !allow.contains(&Method::HEAD) {
                            allow.push(Method::HEAD);
                        }
                        allow.push(Method::OPTIONS);
                        let value = allow
                            .iter()
                            .map(|m| m.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Response::new(StatusCode::NO_CONTENT).with_header(
                            ALLOW,
                            HeaderValue::from_str(&value)
                                .unwrap_or(HeaderValue::from_static("")),
                        );
                    }
                }
```

The closure returns `Response`, so `return` exits the closure only.

- [ ] **Step 5: Prove CORS preflight still wins**

Add to `churust-cors/src/lib.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn preflight_takes_priority_over_automatic_options() {
        let app = Churust::server()
            .install(Cors::permissive())
            .routing(|r| {
                r.get("/", |_c: Call| async { "hi" });
            })
            .build();

        let res = TestClient::new(app)
            .request(Method::OPTIONS, "/")
            .header("origin", "https://example.com")
            .header("access-control-request-method", "GET")
            .send()
            .await;

        assert!(
            res.header("access-control-allow-origin").is_some(),
            "CORS preflight was swallowed by the automatic OPTIONS handler"
        );
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p churust-core --test head_options`
Expected: PASS, 6 tests.

Run: `cargo test -p churust-cors`
Expected: PASS including the new preflight test.

- [ ] **Step 7: Checkpoint**

Run: `cargo test --workspace && cargo test -p churust --features full`
Expected: green. Do not commit.

---

## Task 6: The path segment decoder

**Files:**
- Create: `churust-core/src/path.rs`
- Modify: `churust-core/src/lib.rs`

**Interfaces:**
- Produces: `pub(crate) fn decode_path_segment(raw: &str) -> Option<String>` —
  `None` on malformed encoding or invalid UTF-8. Tasks 7 and 8 consume it.

- [ ] **Step 1: Write the failing tests**

Create `churust-core/src/path.rs`:

```rust
//! Percent-decoding for URL path segments.
//!
//! Separate from the query-string decoder in [`crate::call`] on purpose. Two
//! differences matter:
//!
//! - `+` is a literal plus in a path. Only `application/x-www-form-urlencoded`
//!   query strings treat it as a space.
//! - Invalid UTF-8 is an error, not `U+FFFD`. Replacement characters can
//!   collapse distinct byte sequences into one string, which is unsound input
//!   to a path-safety decision.
//!
//! Callers must split on `/` **before** decoding. Decoding first would let
//! `%2F` manufacture separators.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(decode_path_segment("John%20Doe").unwrap(), "John Doe");
        assert_eq!(decode_path_segment("a%2Eb").unwrap(), "a.b");
        assert_eq!(decode_path_segment("%E2%9C%93").unwrap(), "✓");
    }

    #[test]
    fn leaves_plus_alone() {
        assert_eq!(
            decode_path_segment("a+b").unwrap(),
            "a+b",
            "`+` is a literal in a path; only query strings map it to a space"
        );
    }

    #[test]
    fn passes_through_plain_text() {
        assert_eq!(decode_path_segment("users").unwrap(), "users");
        assert_eq!(decode_path_segment("").unwrap(), "");
    }

    #[test]
    fn decodes_dot_segments_without_interpreting_them() {
        // Decoding produces "..". Rejecting it is the caller's job.
        assert_eq!(decode_path_segment("%2e%2e").unwrap(), "..");
        assert_eq!(decode_path_segment("%2E%2E").unwrap(), "..");
    }

    #[test]
    fn single_decode_only() {
        // %252e is "%2e" once decoded, not "." — no double decoding.
        assert_eq!(decode_path_segment("%252e").unwrap(), "%2e");
    }

    #[test]
    fn rejects_malformed_escapes() {
        assert!(decode_path_segment("%zz").is_none());
        assert!(decode_path_segment("%4").is_none());
        assert!(decode_path_segment("%").is_none());
        assert!(decode_path_segment("ok%").is_none());
    }

    #[test]
    fn rejects_invalid_utf8() {
        // 0xFF is not valid UTF-8 in any position.
        assert!(decode_path_segment("%FF").is_none());
        // Lone continuation byte.
        assert!(decode_path_segment("%80").is_none());
    }

    #[test]
    fn decodes_an_encoded_separator_to_a_literal() {
        // The decoder's job is to decode. Rejecting separators is the caller's.
        assert_eq!(decode_path_segment("a%2Fb").unwrap(), "a/b");
    }
}
```

- [ ] **Step 2: Register the module and run to verify failure**

In `churust-core/src/lib.rs`, add alongside the other module declarations:

```rust
mod path;
```

Run: `cargo test -p churust-core --lib path::tests`
Expected: FAIL to compile — `cannot find function decode_path_segment`.

- [ ] **Step 3: Implement the decoder**

Add above `mod tests` in `churust-core/src/path.rs`:

```rust
/// Percent-decode one path segment.
///
/// Returns `None` when the input contains a malformed escape (`%zz`, a
/// truncated `%4`) or decodes to bytes that are not valid UTF-8. Callers turn
/// `None` into `400 Bad Request`.
pub(crate) fn decode_path_segment(raw: &str) -> Option<String> {
    // Fast path: nothing to do.
    if !raw.contains('%') {
        return Some(raw.to_string());
    }

    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Need two more bytes for a complete escape.
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    // Reject rather than replace: U+FFFD would collapse distinct byte
    // sequences into the same string.
    String::from_utf8(out).ok()
}
```

Note the guard is `i + 2 >= bytes.len()`, so `bytes[i + 2]` is always in range.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p churust-core --lib path::tests`
Expected: PASS, 8 tests.

- [ ] **Step 5: Checkpoint**

Run: `cargo test --workspace`
Expected: green. Do not commit.

---

## Task 7: Decode path parameters during routing

**Files:**
- Modify: `churust-core/src/router.rs` (`split_segments` callers, `route`)
- Modify: `churust-core/src/app.rs` (400 on malformed)
- Test: `churust-core/src/router.rs` (`mod tests`)

**Interfaces:**
- Consumes: `decode_path_segment` from Task 6, `Router::route` from Task 3.
- Produces: `Router::route` returns `Match::BadPath` for undecodable input.
  `Match` gains a variant — update every `match` over it.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `churust-core/src/router.rs`:

```rust
    #[test]
    fn path_params_are_percent_decoded() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/u/{name}", |_c: Call| async { "" });
        }
        match r.route(&Method::GET, "/u/John%20Doe") {
            Match::Found { params, .. } => assert_eq!(params.get("name").unwrap(), "John Doe"),
            _ => panic!("expected a match"),
        }
    }

    #[test]
    fn encoded_slash_does_not_create_a_segment() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/u/{name}", |_c: Call| async { "" });
        }
        // Two segments would mean %2F manufactured a separator.
        match r.route(&Method::GET, "/u/a%2Fb") {
            Match::Found { params, .. } => assert_eq!(params.get("name").unwrap(), "a/b"),
            _ => panic!("%2F must stay inside one segment"),
        }
    }

    #[test]
    fn a_route_with_a_literal_space_is_reachable_encoded() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/a b", |_c: Call| async { "" });
        }
        assert!(matches!(r.route(&Method::GET, "/a%20b"), Match::Found { .. }));
    }

    #[test]
    fn malformed_encoding_is_a_bad_path() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/u/{name}", |_c: Call| async { "" });
        }
        assert!(matches!(r.route(&Method::GET, "/u/%zz"), Match::BadPath));
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p churust-core --lib router::tests`
Expected: FAIL to compile — no `Match::BadPath`; then assertion failures.

- [ ] **Step 3: Add the variant**

In the `Match` enum in `router.rs`, add:

```rust
    /// A path segment could not be percent-decoded — malformed escape or
    /// invalid UTF-8. The dispatcher turns this into `400 Bad Request`.
    BadPath,
```

- [ ] **Step 4: Decode after splitting, inside `route`**

At the top of `Router::route`, replace `let segments = split_segments(path);`:

```rust
        // Split first, decode second. Decoding before splitting would let %2F
        // manufacture separators and forge extra path segments.
        let raw_segments = split_segments(path);
        let mut decoded: Vec<String> = Vec::with_capacity(raw_segments.len());
        for raw in &raw_segments {
            match crate::path::decode_path_segment(raw) {
                Some(s) => decoded.push(s),
                None => return Match::BadPath,
            }
        }
        let segments: Vec<&str> = decoded.iter().map(|s| s.as_str()).collect();
```

The rest of `route` is unchanged — it already operates on `&[&str]`.

Apply the same decode loop at the top of `methods_for` from Task 5, returning an
empty `Vec` instead of `Match::BadPath` when a segment fails.

- [ ] **Step 5: Handle the variant at dispatch**

In `app.rs`, add an arm to the `match lookup` block:

```rust
                    Match::BadPath => Response::text("Bad Request")
                        .with_status(StatusCode::BAD_REQUEST),
```

Compile and fix every other `match` over `Match` the compiler flags — the
exhaustiveness check is what finds them.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p churust-core --lib router::tests`
Expected: PASS.

Run: `cargo test --workspace`
Expected: PASS. If a test asserted a raw-encoded param value, it was asserting
the bug — update it and note which.

- [ ] **Step 7: Checkpoint**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green. Do not commit.

---

## Task 8: StaticFiles separator rejection and the traversal suite

**Files:**
- Modify: `churust-core/src/fs.rs:63-103`
- Create: `churust-core/tests/traversal.rs`

**Interfaces:**
- Consumes: decoded params from Task 7.
- Produces: no new public API. `StaticFiles::serve` gains a rejection step.

- [ ] **Step 1: Write the failing traversal suite**

Create `churust-core/tests/traversal.rs`:

```rust
//! Directory-traversal attempts against StaticFiles.
//!
//! A canary file is planted outside the served root, so a successful traversal
//! is *detected* rather than merely not-asserted.

#![cfg(feature = "fs")]

use churust_core::{fs::StaticFiles, Churust, TestClient};
use http::StatusCode;

struct Tree {
    root: std::path::PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("churust-traversal-{}-{tag}", std::process::id()));
        let root = base.join("public");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ok.txt"), "public file").unwrap();
        std::fs::write(base.join("secret.txt"), "CANARY-SHOULD-NEVER-BE-SERVED").unwrap();
        Self { root }
    }
}

fn app(root: &std::path::Path) -> churust_core::App {
    let root = root.to_path_buf();
    Churust::server()
        .routing(move |r| {
            r.get("/s/{path...}", StaticFiles::dir(root.clone()).handler());
        })
        .build()
}

#[tokio::test]
async fn serves_a_normal_file() {
    let t = Tree::new("normal");
    let res = TestClient::new(app(&t.root)).get("/s/ok.txt").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "public file");
}

#[tokio::test]
async fn every_traversal_shape_is_refused() {
    let t = Tree::new("traversal");
    let client = TestClient::new(app(&t.root));

    let attempts = [
        "/s/../secret.txt",
        "/s/%2e%2e/secret.txt",
        "/s/%2e%2e%2fsecret.txt",
        "/s/..%2fsecret.txt",
        "/s/%252e%252e%252fsecret.txt",
        "/s/%2fsecret.txt",
        "/s/..%5csecret.txt",
        "/s/....//secret.txt",
        "/s/a/../../secret.txt",
    ];

    for attempt in attempts {
        let res = client.get(attempt).send().await;
        assert!(
            res.status() == StatusCode::NOT_FOUND || res.status() == StatusCode::BAD_REQUEST,
            "{attempt} returned {}",
            res.status()
        );
        assert!(
            !res.text().contains("CANARY"),
            "{attempt} SERVED THE CANARY FILE — traversal is possible"
        );
    }
}
```

- [ ] **Step 2: Run it to verify the current state**

Run: `cargo test -p churust-core --features fs --test traversal`
Expected: `serves_a_normal_file` PASSES. `every_traversal_shape_is_refused` may
already pass — Task 7 introduced decoding, so record which attempts now reach
`sanitize` with a decoded `..`. Either way the suite must be green before Step 3
is considered done, and it is the regression guard for the change below.

- [ ] **Step 3: Reject encoded separators before sanitizing**

In `churust-core/src/fs.rs`, inside `serve`, between reading `rel` and calling
`sanitize` (currently line 71):

```rust
        // A decoded segment containing a separator would change the meaning of
        // the rejoined wildcard value. Refuse rather than reason about it.
        // Deliberately stricter than necessary: filenames with an encoded slash
        // are not servable, and that is an accepted limitation.
        if rel.contains('\\') || call.path().to_ascii_lowercase().contains("%2f")
            || call.path().to_ascii_lowercase().contains("%5c")
        {
            return Err(Error::not_found("not found"));
        }

        let safe = sanitize(&rel).ok_or_else(|| Error::not_found("not found"))?;
```

`404` and not `400`: a static file server should not reveal whether a rejected
path would have resolved.

- [ ] **Step 4: Run the suite to verify it passes**

Run: `cargo test -p churust-core --features fs --test traversal`
Expected: PASS, 2 tests, no canary in any response.

- [ ] **Step 5: Confirm ordering with a targeted unit test**

Add to `mod tests` in `churust-core/src/fs.rs`:

```rust
    #[test]
    fn sanitize_rejects_decoded_parent_segments() {
        // Task 7 hands sanitize already-decoded text, so ".." arrives literal.
        assert!(sanitize("../secret").is_none());
        assert!(sanitize("a/../../secret").is_none());
        assert!(sanitize("ok/file.txt").is_some());
    }
```

Run: `cargo test -p churust-core --features fs --lib fs::tests`
Expected: PASS.

- [ ] **Step 6: Checkpoint**

Run: `cargo test --workspace && cargo test -p churust --features full && cargo test -p churust-core --features fs`
Expected: green. Do not commit.

---

## Task 9: Documentation and release preparation

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`
- Modify: `churust/src/lib.rs` (crate docs)

- [ ] **Step 1: Drop tokio from the install instructions**

In `README.md`, the Install section becomes:

```toml
[dependencies]
churust = "0.2"
```

and the features example:

```toml
churust = { version = "0.2", features = ["full", "ws", "fs", "tls"] }
```

Add below the feature table:

> Churust re-exports the runtime as `churust::tokio`, so applications do not
> need their own tokio dependency. Add one only if you need a tokio feature
> Churust does not enable.

Update the three per-feature snippets in the WebSockets and Static-files
sections from `version = "0.1"` to `version = "0.2"`.

- [ ] **Step 2: Write the changelog entry**

In `CHANGELOG.md`, under `## [Unreleased]`:

```markdown
### Added

- `churust::tokio` re-export. Applications no longer need their own tokio
  dependency; `churust = "0.2"` compiles on its own.
- Automatic `HEAD` handling. A `HEAD` request to a route with only a `GET`
  handler now runs that handler and returns headers without a body, per
  RFC 9110 §9.3.2. An explicitly registered `HEAD` route still wins.
- Automatic `OPTIONS` responses with an `Allow` header when no `OPTIONS`
  handler is registered. CORS preflight continues to take priority.

### Fixed

- A `{path...}` wildcard route was unreachable whenever a static route shared
  its prefix: `/files/special` returned 404 despite `/files/{path...}` being
  registered. This broke `StaticFiles` mounted alongside any other route.
- Path parameters are now percent-decoded, matching query-parameter behaviour.
  `/u/John%20Doe` yields `John Doe` rather than `John%20Doe`. Segments are split
  before decoding, so `%2F` cannot forge a path separator, and `StaticFiles`
  rejects encoded separators before sanitizing.
- Malformed percent-encoding and non-UTF-8 path segments return `400` instead
  of being passed through.

### Changed

- **Tokio features narrowed** from `full` to the set Churust uses
  (`rt-multi-thread`, `net`, `io-util`, `time`, `sync`, `signal`, `macros`,
  plus `fs` under Churust's `fs` feature). If you relied on a tokio feature
  reaching you transitively, declare `tokio` yourself with the features you
  need; Cargo unifies them.
- `#[churust::main]` expands to `::churust::__private::tokio` rather than
  `::tokio`. Invoking the macro from `churust-macros` directly is no longer
  supported; depend on `churust`.
```

- [ ] **Step 3: Update the crate-level docs**

In `churust/src/lib.rs`, find the quickstart in the module docs and remove any
`tokio = ...` line from the shown `Cargo.toml`.

- [ ] **Step 4: Verify the docs build and the examples still run**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo run -p hello &
sleep 2 && curl -sS -i http://127.0.0.1:8080/ | head -1
curl -sS -i -X HEAD http://127.0.0.1:8080/ | head -1
kill %1
```

Expected: docs build clean; `GET /` returns `200`; `HEAD /` returns `200` and no
body.

- [ ] **Step 5: Full gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p churust --all-targets --features full -- -D warnings
cargo clippy -p churust-core --all-targets --features tls -- -D warnings
cargo test --workspace
cargo test -p churust --features full
cargo test -p churust-core --features tls
cargo build -p hello -p api
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo package --workspace
```

Expected: all PASS. This is exactly what CI runs plus the packaging check.

- [ ] **Step 6: Checkpoint**

Report the final test count against the 189 baseline and stop. The release
itself (`cargo release minor --execute`) is the user's call, not part of this
plan.

---

## Verification Summary

| Spec section | Task |
| --- | --- |
| §4 C1 one-dependency | 1 |
| §4.4 tokio feature set | 2 |
| §5 C2 wildcard fallback | 3 |
| §5.3 param-map hygiene | 3, step 1 test 4 |
| §6.1 HEAD | 4 |
| §6.2 OPTIONS + CORS coexistence | 5 |
| §7.2 path-specific decoder | 6 |
| §7.1, §7.3, §7.4 split-before-decode, 400s, matching | 7 |
| §7.5 StaticFiles ordering | 8 |
| §9 traversal suite with canary | 8 |
| §10 version and release notes | 9 |
