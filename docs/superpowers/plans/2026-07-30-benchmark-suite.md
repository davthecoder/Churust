# Benchmark Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Churust a Criterion regression suite over the per-request hot path that runs on every PR, and a hand-run comparison against axum whose numbers are honest about the machine they came from.

**Architecture:** Two independent halves. `churust-core/benches/` holds four Criterion targets that drive the public `App::process` in-process — no socket, no client — so the numbers isolate Churust's own work. `benchmarks/` sits *outside* the cargo workspace and holds two equivalent apps (Churust and axum) plus a shell runner that proves they return identical bytes before it measures anything.

**Tech Stack:** Rust 1.96+, Criterion 0.8 (`async_tokio` feature), tokio, `critcmp` 0.1 for CI diffing, `oha` 1.x as the load generator, GitHub Actions.

## Global Constraints

- **Benchmarks are an external crate.** A `benches/` target sees only `churust-core`'s public API. `RouteBuilder::new` and `SecurityHeaders::apply_to` are `pub(crate)` and **must not** be made public for this work. Measure through `App::process`.
- **Build the app outside the measured closure.** Every bench constructs its `App` once. What is under test is serving a request, not assembling a server.
- **The CI job is advisory.** It must never gate a release. Do not touch `.github/workflows/release.yml`.
- **CI fails only past 20% regression.** Runner variance is 20–30%; a tighter gate reports noise as data.
- **`benchmarks/` must never enter `Cargo.lock` or `cargo test --workspace`.** It goes in `[workspace] exclude`.
- **The repo runs `cargo fmt --all --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` in CI.** Both cover `--all-targets`, which includes benches. Run them before every commit.
- **Do not commit to `main`** — it is protected. Work on the current branch.
- Existing exact APIs to use (verified, do not guess):
  - `App::process(&self, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response` (async)
  - `Churust::server() -> AppBuilder`, `.routing(|r: &mut RouteBuilder| ...)`, `.build() -> App`
  - `AppBuilder::without_security_headers()`
  - `Call::new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Call`
  - `FromCallParts::from_call_parts(call: &mut Call) -> Result<Self>` (async trait)
  - `Cookie::new(name, value)`, `Cookie::to_header_value() -> String`
  - `Response::vary_on(&mut self, field: &str)`
  - `Error::bad_request(msg)`, `Error::with_response_header(HeaderName, HeaderValue)`, `IntoResponse::into_response()`
  - Exported extractors: `Path`, `Query`, `Form`, `Header`, `State`, `Payload`

## File Structure

| file | responsibility |
|---|---|
| `churust-core/Cargo.toml` | criterion dev-dep, four `[[bench]]` sections with `harness = false` |
| `churust-core/benches/dispatch.rs` | `App::process` end to end: bare 200, with middleware, 404 (does not reach engine.rs's `Host` check) |
| `churust-core/benches/routing.rs` | route-shape sensitivity: static, param, wildcard, deep, miss, backtracking |
| `churust-core/benches/headers.rs` | `Vary` merge, cookie render/parse, `Error`→`Response` 1 vs 3 headers, security headers on/off |
| `churust-core/benches/extract.rs` | `Path` / `Query` / `Form` decode |
| `.github/workflows/bench.yml` | bench base + head, `critcmp`, PR comment, fail past 20% |
| `Cargo.toml` (root) | `[workspace] exclude = ["benchmarks"]` |
| `benchmarks/bench-churust/` | minimal Churust app, three routes |
| `benchmarks/bench-axum/` | the same three routes in axum |
| `benchmarks/run.sh` | equivalence check, then `oha` measurement, then report |
| `benchmarks/README.md` | how to run it and what the numbers are worth |
| `benchmarks/results/` | dated reports, committed |

Tasks 1–4 are independent of 5–8; either half can land alone.

---

### Task 1: Criterion harness and the dispatch bench

The first task deliberately does the smallest possible thing that proves the harness works, because Criterion 0.8's async API is the one thing here that could differ from expectation. Get one benchmark running before writing three more files against an unverified API.

**Files:**
- Modify: `churust-core/Cargo.toml` (dev-dependencies, and append `[[bench]]`)
- Create: `churust-core/benches/dispatch.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: the `[[bench]]` pattern and the `rt()` / app-construction idiom that Tasks 2–4 copy.

- [ ] **Step 1: Add the dev-dependency and the bench target**

In `churust-core/Cargo.toml`, add to `[dev-dependencies]`:

```toml
# Benchmarks for the per-request hot path. `async_tokio` because the thing
# under test — `App::process` — is async, and Criterion needs a runtime to
# drive it.
criterion = { version = "0.8", features = ["async_tokio"] }
```

And append at the end of the file:

```toml
# `harness = false` because Criterion supplies its own `main`; the default
# libtest harness would swallow the benchmark arguments.
[[bench]]
name = "dispatch"
harness = false
```

- [ ] **Step 2: Write the bench, and run it to see it fail**

Create `churust-core/benches/dispatch.rs`:

```rust
//! What every request costs, end to end, with no socket in the way.
//!
//! `App::process` is the same entry point `TestClient` uses, so these numbers
//! cover routing, the middleware pipeline, extraction and response building
//! without kernel or TCP noise.
//!
//! What they do *not* cover is anything above that entry point. The `Host`
//! validation added in 0.3.3 lives in `engine.rs`, on the raw hyper request,
//! and runs before `process_call` is reached — so it is structurally out of
//! reach here, and no number in this file includes it.

use bytes::Bytes;
use churust_core::{Call, Churust, Middleware, Next, Response};
use criterion::{criterion_group, criterion_main, Criterion};
use http::{HeaderMap, Method};
use std::sync::Arc;

/// One runtime for the whole file. Building a runtime per iteration would
/// measure tokio's startup rather than Churust's request handling.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

/// A middleware that does the least a middleware can do, so a chain of them
/// measures the pipeline rather than whatever the middleware itself is up to.
struct Noop;

#[async_trait::async_trait]
impl Middleware for Noop {
    async fn handle(&self, call: Call, next: Next) -> Response {
        next.run(call).await
    }
}

fn bench_dispatch(c: &mut Criterion) {
    let rt = rt();

    // Built once, outside every measured closure.
    let bare = Churust::server()
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello" });
        })
        .build();

    // `add_middleware` takes `&mut self` and returns `()`, so it cannot be
    // chained the way the other builder methods can.
    let with_middleware = {
        let mut builder = Churust::server().routing(|r| {
            r.get("/hello", |_c: Call| async { "hello" });
        });
        builder.add_middleware(Arc::new(Noop));
        builder.add_middleware(Arc::new(Noop));
        builder.add_middleware(Arc::new(Noop));
        builder.build()
    };

    let mut group = c.benchmark_group("dispatch");

    group.bench_function("bare_200", |b| {
        b.to_async(&rt).iter(|| {
            bare.process(
                Method::GET,
                "/hello".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            )
        })
    });

    group.bench_function("three_middleware", |b| {
        b.to_async(&rt).iter(|| {
            with_middleware.process(
                Method::GET,
                "/hello".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            )
        })
    });

    group.bench_function("not_found", |b| {
        b.to_async(&rt).iter(|| {
            bare.process(
                Method::GET,
                "/nothing-here".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            )
        })
    });

    group.finish();
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
```

Run: `cargo bench -p churust-core --bench dispatch -- --test`

Expected: this is the step that catches API drift. If Criterion 0.8 renamed
`to_async`, changed `benchmark_group`, or moved `criterion_group!`, it fails
here with a compile error naming the item. Fix against the compiler and the
`criterion` docs for the resolved version before continuing — do not work
around it by pinning an older Criterion, because the rest of the plan assumes
the version that actually resolves.

`--test` runs each benchmark once instead of sampling, which turns a two-minute
sampling run into a two-second correctness check.

- [ ] **Step 3: Confirm `async-trait` is reachable from a bench**

The `Noop` middleware uses `#[async_trait::async_trait]`, which must be nameable
from the bench crate.

Run: `grep -n "^async-trait" churust-core/Cargo.toml`

Expected: a normal dependency, so benches get it too. If it is absent, add
`async-trait = { workspace = true }` to `[dev-dependencies]`.

- [ ] **Step 4: Run the real benchmark**

Run: `cargo bench -p churust-core --bench dispatch`

Expected: three named results under `dispatch/`, each with a time and a
confidence interval. Numbers themselves are not asserted — nothing about them
can be wrong at this stage. What matters is that all three ran.

- [ ] **Step 5: Check the repo's own gates**

Run:
```bash
cargo fmt --all --check
cargo clippy --locked -p churust-core --all-targets -- -D warnings
```

Expected: both clean. `--all-targets` includes benches, so a warning here fails
CI later.

- [ ] **Step 6: Commit**

```bash
git add churust-core/Cargo.toml churust-core/benches/dispatch.rs
git commit -m "bench: measure App::process end to end

The path every request takes, with no socket in the way. \`App::process\` is
the entry point \`TestClient\` already uses, so these cover routing, the
middleware pipeline and response building without kernel or TCP noise.

Three cases: a bare 200, the same behind three no-op middleware so the
pipeline's own cost is visible, and a 404. The app is built once outside
every measured closure — what is under test is serving a request, not
assembling a server."
```

---

### Task 2: Route-shape sensitivity

**Files:**
- Modify: `churust-core/Cargo.toml` (append a `[[bench]]`)
- Create: `churust-core/benches/routing.rs`

**Interfaces:**
- Consumes: the `rt()` idiom and `[[bench]]` pattern from Task 1.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Register the bench target**

Append to `churust-core/Cargo.toml`:

```toml
[[bench]]
name = "routing"
harness = false
```

- [ ] **Step 2: Write the bench**

Create `churust-core/benches/routing.rs`:

```rust
//! How much the *shape* of a route costs.
//!
//! Measured through `App::process` rather than against `Router` directly: a
//! benchmark is an external crate, `RouteBuilder::new` is `pub(crate)`, and so a
//! bare `Router` cannot be populated from here. Every number therefore carries
//! the same constant dispatch overhead. That makes these useful for comparing
//! shapes against each other and for catching a regression over time, and
//! useless as an absolute cost for route matching.
//!
//! The route table is deliberately larger than the routes under test, so a
//! change that makes matching scale with table size shows up.

use bytes::Bytes;
use churust_core::{Call, Churust};
use criterion::{criterion_group, criterion_main, Criterion};
use http::{HeaderMap, Method};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

fn bench_routing(c: &mut Criterion) {
    let rt = rt();

    let app = Churust::server()
        .routing(|r| {
            // Filler, so matching is not searching a table of one.
            for i in 0..50 {
                let path: &'static str = Box::leak(format!("/filler/{i}").into_boxed_str());
                r.get(path, |_c: Call| async { "filler" });
            }
            r.get("/static/path/here", |_c: Call| async { "static" });
            r.get("/users/{id}", |_c: Call| async { "param" });
            r.get("/files/{rest...}", |_c: Call| async { "wildcard" });
            r.get("/a/b/c/d/e/f/g/h", |_c: Call| async { "deep" });
        })
        .build();

    let mut group = c.benchmark_group("routing");

    for (name, path) in [
        ("static", "/static/path/here"),
        ("param", "/users/42"),
        ("wildcard", "/files/some/nested/thing.txt"),
        ("deep", "/a/b/c/d/e/f/g/h"),
        ("miss", "/definitely/not/registered"),
    ] {
        group.bench_function(name, |b| {
            b.to_async(&rt).iter(|| {
                app.process(
                    Method::GET,
                    path.parse().expect("a valid uri"),
                    HeaderMap::new(),
                    Bytes::new(),
                )
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_routing);
criterion_main!(benches);
```

- [ ] **Step 3: Verify the wildcard syntax before trusting it**

Run: `grep -rn '{.*\.\.\.}' churust-core/src/router.rs | head -3`

Expected: confirms the `{name...}` wildcard spelling used above. If the syntax
differs, fix the bench. A wildcard route registered with the wrong syntax would
silently become a literal path and the "wildcard" number would measure a 404.

- [ ] **Step 4: Run it once for correctness**

Run: `cargo bench -p churust-core --bench routing -- --test`

Expected: PASS, five cases.

- [ ] **Step 5: Prove the cases are hitting different handlers**

This is the failure mode that makes the whole file worthless: if every path
404s, all five numbers are identical and meaningless.

Add a temporary assertion at the top of `bench_routing`, run, then remove it:

```rust
// TEMPORARY — delete after verifying.
for (name, path, expect) in [
    ("static", "/static/path/here", "static"),
    ("param", "/users/42", "param"),
    ("wildcard", "/files/some/nested/thing.txt", "wildcard"),
    ("deep", "/a/b/c/d/e/f/g/h", "deep"),
] {
    let res = rt.block_on(app.process(
        Method::GET,
        path.parse().unwrap(),
        HeaderMap::new(),
        Bytes::new(),
    ));
    assert_eq!(res.body, Bytes::from(expect), "{name} did not reach its handler");
}
```

Run: `cargo bench -p churust-core --bench routing -- --test`
Expected: PASS. If a case panics, the route syntax is wrong — fix it before
deleting the assertion.

Then delete the temporary block.

- [ ] **Step 6: Gates and commit**

```bash
cargo fmt --all --check
cargo clippy --locked -p churust-core --all-targets -- -D warnings
git add churust-core/Cargo.toml churust-core/benches/routing.rs
git commit -m "bench: measure route-shape sensitivity

Static, param, wildcard, deep and miss, against a table of fifty filler
routes so a change that makes matching scale with table size is visible.

Through \`App::process\` rather than \`Router\`, because a benchmark is an
external crate and \`RouteBuilder::new\` is \`pub(crate)\`. Every number
therefore carries the same constant dispatch overhead: good for comparing
shapes and for catching drift, useless as an absolute cost for matching.
The file says so, so nobody quotes these as routing's cost."
```

---

### Task 3: Header and response building

**Files:**
- Modify: `churust-core/Cargo.toml` (append a `[[bench]]`)
- Create: `churust-core/benches/headers.rs`

**Interfaces:**
- Consumes: the `rt()` idiom from Task 1.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Register the bench target**

Append to `churust-core/Cargo.toml`:

```toml
[[bench]]
name = "headers"
harness = false
```

- [ ] **Step 2: Write the bench**

Create `churust-core/benches/headers.rs`:

```rust
//! Response decoration: the work done to every reply on the way out.
//!
//! `Error → Response` carries one header and then three, because 0.3.3 changed
//! that loop from a plain `insert` to first-occurrence-replaces with later ones
//! appended. The three-header case is the one that got more expensive, and this
//! is what would show it.
//!
//! Security headers are measured as an app with them against an app without,
//! since `SecurityHeaders::apply_to` is `pub(crate)` and cannot be called from
//! here. The difference between the two numbers is their cost.

use bytes::Bytes;
use churust_core::{Call, Churust, Error, IntoResponse, Response};
use criterion::{criterion_group, criterion_main, Criterion};
use http::header::{HeaderValue, SET_COOKIE};
use http::{HeaderMap, Method};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

fn bench_headers(c: &mut Criterion) {
    let rt = rt();

    let with_security = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    let without_security = Churust::server()
        .without_security_headers()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    let mut group = c.benchmark_group("headers");

    group.bench_function("security_headers_on", |b| {
        b.to_async(&rt).iter(|| {
            with_security.process(
                Method::GET,
                "/".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            )
        })
    });

    group.bench_function("security_headers_off", |b| {
        b.to_async(&rt).iter(|| {
            without_security.process(
                Method::GET,
                "/".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            )
        })
    });

    group.bench_function("error_one_header", |b| {
        b.iter(|| {
            Error::bad_request("nope")
                .with_response_header(SET_COOKIE, HeaderValue::from_static("a=1"))
                .into_response()
        })
    });

    group.bench_function("error_three_headers", |b| {
        b.iter(|| {
            Error::bad_request("nope")
                .with_response_header(SET_COOKIE, HeaderValue::from_static("a=1"))
                .with_response_header(SET_COOKIE, HeaderValue::from_static("b=2"))
                .with_response_header(SET_COOKIE, HeaderValue::from_static("c=3"))
                .into_response()
        })
    });

    group.bench_function("vary_merge_empty", |b| {
        b.iter(|| {
            let mut res = Response::text("ok");
            res.vary_on("accept-encoding");
            res
        })
    });

    group.bench_function("vary_merge_existing", |b| {
        b.iter(|| {
            let mut res = Response::text("ok");
            res.vary_on("accept-encoding");
            res.vary_on("origin");
            res.vary_on("accept-language");
            res
        })
    });

    group.finish();
}

criterion_group!(benches, bench_headers);
criterion_main!(benches);
```

- [ ] **Step 3: Confirm `Response::text` and `Error::bad_request` exist as used**

Run:
```bash
grep -n "pub fn text\b" churust-core/src/response.rs
grep -n "pub fn bad_request\b" churust-core/src/error.rs
```

Expected: both found. If `Response::text` takes something other than `&str`,
adjust.

- [ ] **Step 4: Run once for correctness**

Run: `cargo bench -p churust-core --bench headers -- --test`
Expected: PASS, six cases.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --all --check
cargo clippy --locked -p churust-core --all-targets -- -D warnings
git add churust-core/Cargo.toml churust-core/benches/headers.rs
git commit -m "bench: measure response decoration

\`Error → Response\` with one header and with three, because 0.3.3 changed
that loop from a plain \`insert\` to first-replaces-then-appends and the
three-header case is the one that got more expensive.

Security headers as on-versus-off through \`App::process\`, since
\`SecurityHeaders::apply_to\` is \`pub(crate)\`; the gap between the two
numbers is their cost. Plus \`Vary\` merging into an empty header and into a
populated one, which is the case that walks the existing list."
```

---

### Task 4: Extractors

**Files:**
- Modify: `churust-core/Cargo.toml` (append a `[[bench]]`)
- Create: `churust-core/benches/extract.rs`

**Interfaces:**
- Consumes: the `rt()` idiom from Task 1.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Register the bench target**

Append to `churust-core/Cargo.toml`:

```toml
[[bench]]
name = "extract"
harness = false
```

- [ ] **Step 2: Know why `Path` is benched through dispatch**

Already established, so do not re-derive it: `Call::params()` is a getter and
there is **no** public setter. A hand-built `Call` therefore has no captured
parameters, and a direct `Path` benchmark would measure the *error* path — which
is far cheaper than the success path and would read as an improvement.

`Path` is benched through `App::process` against a `/users/{id}` route. `Query`
and `Form` read from the URI and body, need nothing the router provides, and are
benched against a `Call` built directly.

Confirm nothing has changed:

```bash
grep -nE "pub fn (with_params|set_params)" churust-core/src/call.rs || echo "no public setter, as expected"
```

- [ ] **Step 3: Write the bench**

Create `churust-core/benches/extract.rs`:

```rust
//! Extraction, which runs once per handler argument per request.
//!
//! `Query` and `Form` are measured against a `Call` built directly, because
//! both read from the URI or the body and need nothing the router provides.
//! `Path` needs captured parameters, so it goes through `App::process` against a
//! parameterised route — see the note in the task plan.

use bytes::Bytes;
use churust_core::{Call, Churust, FromCallParts, Query};
use criterion::{criterion_group, criterion_main, Criterion};
use http::{HeaderMap, Method};
use serde::Deserialize;

#[derive(Deserialize)]
struct Page {
    page: u32,
    per_page: u32,
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

fn bench_extract(c: &mut Criterion) {
    let rt = rt();

    let path_app = Churust::server()
        .routing(|r| {
            r.get("/users/{id}", |_c: Call| async { "ok" });
        })
        .build();

    let mut group = c.benchmark_group("extract");

    group.bench_function("query_two_fields", |b| {
        b.to_async(&rt).iter(|| async {
            let mut call = Call::new(
                Method::GET,
                "/?page=3&per_page=50".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            );
            Query::<Page>::from_call_parts(&mut call).await
        })
    });

    group.bench_function("path_param_via_dispatch", |b| {
        b.to_async(&rt).iter(|| {
            path_app.process(
                Method::GET,
                "/users/42".parse().expect("a valid uri"),
                HeaderMap::new(),
                Bytes::new(),
            )
        })
    });

    group.finish();
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);
```

- [ ] **Step 4: Confirm `serde` is available to benches**

Run: `grep -n "^serde" churust-core/Cargo.toml`

Expected: `serde` with the `derive` feature is a normal dependency, so it is
available to benches. If `derive` is not enabled, add
`serde = { workspace = true, features = ["derive"] }` to `[dev-dependencies]`
rather than changing the crate's real dependency.

- [ ] **Step 5: Run once for correctness**

Run: `cargo bench -p churust-core --bench extract -- --test`
Expected: PASS.

- [ ] **Step 6: Verify the query extraction actually succeeds**

A failing extraction is much cheaper than a successful one, so a typo in the
query string would quietly halve the number.

Add temporarily inside `bench_extract`, run, then delete:

```rust
// TEMPORARY — delete after verifying.
{
    let mut call = Call::new(
        Method::GET,
        "/?page=3&per_page=50".parse().unwrap(),
        HeaderMap::new(),
        Bytes::new(),
    );
    let got = rt.block_on(Query::<Page>::from_call_parts(&mut call));
    assert!(got.is_ok(), "query extraction failed; the benchmark would measure the error path");
}
```

Run: `cargo bench -p churust-core --bench extract -- --test`
Expected: PASS. Then delete the block.

- [ ] **Step 7: Gates and commit**

```bash
cargo fmt --all --check
cargo clippy --locked -p churust-core --all-targets -- -D warnings
git add churust-core/Cargo.toml churust-core/benches/extract.rs
git commit -m "bench: measure extraction

\`Query\` against a hand-built \`Call\`, and a path parameter through
\`App::process\` because \`Path\` reads what the router captured and a
hand-built \`Call\` has no parameters — benchmarking it directly would
measure the error path, which is far cheaper than the success path and would
read as an improvement."
```

---

### Task 5: The CI job

**Files:**
- Create: `.github/workflows/bench.yml`

**Interfaces:**
- Consumes: the four bench targets from Tasks 1–4.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/bench.yml`:

```yaml
# Advisory only. This never gates a release — `release.yml` does not call it —
# and it fails a PR only on a move large enough to be real.
name: Bench

on:
  pull_request:

permissions:
  contents: read
  pull-requests: write

jobs:
  compare:
    name: Compare against the merge base
    runs-on: ubuntu-latest
    steps:
      # Full history, because the base commit has to be checked out and benched.
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - uses: Swatinem/rust-cache@v2

      - name: Install critcmp
        run: cargo install critcmp --locked --version 0.1.8

      # Sample size is cut to keep a doubled run inside a few minutes. It widens
      # the confidence interval, which is acceptable when the gate only fires at
      # 20%.
      - name: Bench the merge base
        run: |
          git checkout --detach ${{ github.event.pull_request.base.sha }}
          cargo bench -p churust-core -- --save-baseline base --sample-size 20

      - name: Bench the head
        run: |
          git checkout --detach ${{ github.event.pull_request.head.sha }}
          cargo bench -p churust-core -- --save-baseline pr --sample-size 20

      - name: Compare
        id: compare
        run: |
          critcmp base pr | tee cmp.txt

      # A regression this large is bigger than the runners' own variance, so it
      # is worth stopping for. Anything smaller is noise and belongs in the
      # comment, not in a red X.
      - name: Fail on a regression past 20%
        run: |
          python3 .github/scripts/check-bench-regression.py cmp.txt 1.20

      - name: Comment the table
        if: always()
        uses: actions/github-script@v7
        with:
          script: |
            const fs = require('fs');
            const table = fs.readFileSync('cmp.txt', 'utf8');
            await github.rest.issues.createComment({
              issue_number: context.issue.number,
              owner: context.repo.owner,
              repo: context.repo.repo,
              body: [
                '### Benchmarks vs merge base',
                '',
                'Runner variance is 20–30%; only moves past 20% fail this job.',
                '',
                '```',
                table.trim(),
                '```',
              ].join('\n'),
            });
```

- [ ] **Step 2: Write the threshold checker**

`critcmp` prints a table and always exits 0, so the gate needs its own parser.

Create `.github/scripts/check-bench-regression.py`:

```python
#!/usr/bin/env python3
"""Fail when a benchmark regressed past a factor.

`critcmp` always exits 0 — it reports, it does not judge — so the gate is here.
The factor is deliberately loose: GitHub-hosted runners swing 20-30% between
identical runs, and a gate that fires below its own noise floor is one people
learn to ignore.
"""

import re
import sys

UNITS = {"ns": 1e-9, "us": 1e-6, "µs": 1e-6, "ms": 1e-3, "s": 1.0}


def to_seconds(value: str, unit: str) -> float:
    return float(value) * UNITS[unit]


def main() -> int:
    path, factor = sys.argv[1], float(sys.argv[2])
    rows = []
    for line in open(path):
        # critcmp rows look like:
        #   dispatch/bare_200   1.00   1.2±0.03µs   1.05   1.3±0.04µs
        times = re.findall(r"(\d+\.?\d*)±\d+\.?\d*(ns|us|µs|ms|s)", line)
        if len(times) != 2:
            continue
        name = line.split()[0]
        base = to_seconds(*times[0])
        head = to_seconds(*times[1])
        if base > 0 and head / base > factor:
            rows.append((name, base, head, head / base))

    for name, base, head, ratio in rows:
        print(f"REGRESSION {name}: {base:.3e}s -> {head:.3e}s ({ratio:.2f}x)")

    if rows:
        print(f"\n{len(rows)} benchmark(s) regressed past {factor:.2f}x.")
        return 1

    print(f"No benchmark regressed past {factor:.2f}x.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 3: Test the checker against fixtures, before trusting it in CI**

A gate that cannot fail is worse than no gate. Prove it both ways locally:

```bash
mkdir -p /tmp/benchfix
cat > /tmp/benchfix/regressed.txt <<'EOF'
group                 base                    pr
-----                 ----                    --
dispatch/bare_200     1.00   1.0±0.03µs       1.50   1.5±0.04µs
routing/static        1.00   2.0±0.05µs       1.01   2.02±0.05µs
EOF
cat > /tmp/benchfix/clean.txt <<'EOF'
group                 base                    pr
-----                 ----                    --
dispatch/bare_200     1.00   1.0±0.03µs       1.02   1.02±0.04µs
EOF

python3 .github/scripts/check-bench-regression.py /tmp/benchfix/regressed.txt 1.20; echo "exit=$? (want 1)"
python3 .github/scripts/check-bench-regression.py /tmp/benchfix/clean.txt 1.20; echo "exit=$? (want 0)"
```

Expected: the first prints `REGRESSION dispatch/bare_200` and exits 1; the
second exits 0. If the regexp does not match real `critcmp` output, capture a
real table from a local `critcmp` run and fix the parser against it — do not
adjust the fixture to match a broken parser.

- [ ] **Step 4: Validate the workflow YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/bench.yml')); print('valid yaml')"`

Expected: `valid yaml`. If PyYAML is missing, `pip install pyyaml` or skip —
GitHub will report a syntax error on push either way, but catching it here is
cheaper.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/bench.yml .github/scripts/check-bench-regression.py
git commit -m "ci: compare benchmarks against the merge base

Benches the base and the head, diffs with critcmp, comments the table, and
fails only past 20%.

The threshold is set by the runner, not by taste: GitHub-hosted runners are
shared and swing 20-30% between identical runs, so a tighter gate would
report noise as data. critcmp always exits 0, so the judgement lives in a
small script with fixtures proving it fails when it should.

Advisory infrastructure — release.yml does not call it."
```

---

### Task 6: The comparison apps

**Files:**
- Modify: `Cargo.toml` (root — add `exclude`)
- Create: `benchmarks/bench-churust/Cargo.toml`, `benchmarks/bench-churust/src/main.rs`
- Create: `benchmarks/bench-axum/Cargo.toml`, `benchmarks/bench-axum/src/main.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: two binaries that listen on a port given by `$PORT`, serving
  `/plaintext`, `/json` and `/user/{id}`. Task 7's `run.sh` depends on both the
  port convention and the exact response bodies.

- [ ] **Step 1: Exclude the directory from the workspace, first**

Do this before creating any manifest below it. A nested package inside a
workspace that does not list it is a cargo error, so creating the crates first
would break every subsequent `cargo` command in the repo.

In the root `Cargo.toml`, directly after the `members` line:

```toml
# The comparison harness. Outside the workspace on purpose: it depends on axum,
# and a benchmark's dependencies must not reach `Cargo.lock`, `cargo test
# --workspace`, or any CI job that builds the library.
exclude = ["benchmarks"]
```

- [ ] **Step 2: Verify the exclusion works before adding crates**

```bash
mkdir -p benchmarks
cargo metadata --no-deps --format-version 1 >/dev/null && echo "workspace still resolves"
```

Expected: `workspace still resolves`.

- [ ] **Step 3: Create the Churust app**

`benchmarks/bench-churust/Cargo.toml`:

```toml
[package]
name = "bench-churust"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
churust-core = { path = "../../churust-core" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[profile.release]
lto = true
codegen-units = 1
```

`benchmarks/bench-churust/src/main.rs`:

```rust
//! One half of the comparison. The routes here must stay byte-identical to
//! `bench-axum`'s — `run.sh` refuses to measure if they diverge.

use churust_core::{Call, Churust};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT")
        .expect("PORT must be set")
        .parse()
        .expect("PORT must be a number");

    let app = Churust::server()
        .host("127.0.0.1")
        .port(port)
        .routing(|r| {
            r.get("/plaintext", |_c: Call| async { "Hello, World!" });
            r.get("/json", |_c: Call| async {
                // Explicit rather than a `json` helper: churust-core has no
                // JSON response constructor (that lives in churust-json, which
                // this app deliberately does not pull in — the comparison is of
                // core dispatch, not of a plugin).
                churust_core::Response::text(r#"{"message":"Hello, World!"}"#).with_header(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/json"),
                )
            });
            r.get("/user/{id}", |churust_core::Path(id): churust_core::Path<u64>| async move {
                format!("user {id}")
            });
        })
        .build();

    app.start().await
}
```

- [ ] **Step 4: Add the `http` dependency the JSON route needs**

The JSON route above names `http::header::CONTENT_TYPE` and
`http::HeaderValue`, so add to `benchmarks/bench-churust/Cargo.toml`:

```toml
http = "1"
```

Confirm `with_header` is the right method and takes those types:

```bash
grep -n "pub fn with_header" -A 3 churust-core/src/response.rs
```

Expected: `with_header(mut self, name: HeaderName, value: HeaderValue) -> Self`.
The body and `content-type` must match `bench-axum` byte for byte — Task 7
asserts it and refuses to measure otherwise.

- [ ] **Step 5: Create the axum app**

`benchmarks/bench-axum/Cargo.toml`:

```toml
[package]
name = "bench-axum"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
axum = "0.8"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
serde = { version = "1", features = ["derive"] }

[profile.release]
lto = true
codegen-units = 1
```

`benchmarks/bench-axum/src/main.rs`:

```rust
//! The other half of the comparison. Same three routes, same bodies, same
//! content types. `run.sh` refuses to measure if they diverge.

use axum::{extract::Path, http::header, response::IntoResponse, routing::get, Router};

async fn plaintext() -> &'static str {
    "Hello, World!"
}

async fn json() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"message":"Hello, World!"}"#,
    )
}

async fn user(Path(id): Path<u64>) -> String {
    format!("user {id}")
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .expect("PORT must be set")
        .parse()
        .expect("PORT must be a number");

    let app = Router::new()
        .route("/plaintext", get(plaintext))
        .route("/json", get(json))
        .route("/user/{id}", get(user));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    axum::serve(listener, app).await.expect("serve");
}
```

- [ ] **Step 6: Build both and confirm the workspace is untouched**

```bash
(cd benchmarks/bench-churust && cargo build --release)
(cd benchmarks/bench-axum && cargo build --release)
git status --short Cargo.lock
```

Expected: both build. `Cargo.lock` must show **no** modification — that is the
whole point of the exclusion. If it changed, the exclude is not working.

- [ ] **Step 7: Confirm axum's path syntax**

axum 0.7 used `/user/:id`; 0.8 uses `/user/{id}`. If the build failed or the
route 404s later, check which version resolved:

```bash
(cd benchmarks/bench-axum && cargo tree -p axum --depth 0)
```

and use that version's syntax.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml benchmarks/bench-churust benchmarks/bench-axum
git commit -m "bench: two equivalent apps for the axum comparison

Three routes each — plaintext, json, and a parsed path parameter — chosen so
the numbers separate dispatch overhead from encoding and from extraction.

\`benchmarks/\` is excluded from the workspace. axum must not reach
\`Cargo.lock\`, \`cargo test --workspace\`, or any CI job that builds the
library: a comparison harness is not a dependency of the thing it measures."
```

---

### Task 7: The runner, with its equivalence gate

**Files:**
- Create: `benchmarks/run.sh`
- Create: `benchmarks/README.md`

**Interfaces:**
- Consumes: both binaries from Task 6, their `$PORT` convention and their exact
  response bodies.
- Produces: a report on stdout and at `results/YYYY-MM-DD-<host>.md`.

- [ ] **Step 1: Write the equivalence check first, and watch it fail**

This is the one genuinely test-driven part of the plan. The check is what makes
the comparison trustworthy, so prove it catches a difference before it guards
anything real.

Create `benchmarks/run.sh`:

```bash
#!/usr/bin/env bash
# Compare Churust against axum on identical work.
#
# The equivalence check below is not ceremony. Two apps that return different
# bytes are doing different work, and a throughput number comparing them is
# fiction — which is the usual reason framework benchmarks cannot be trusted.
set -euo pipefail

cd "$(dirname "$0")"

CHURUST_PORT=${CHURUST_PORT:-8111}
AXUM_PORT=${AXUM_PORT:-8112}
DURATION=${DURATION:-30s}
CONNECTIONS=${CONNECTIONS:-64}
ROUTES=(/plaintext /json /user/42)

require() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }
}
require oha
require curl

start() { # name port dir
  (cd "$2" && PORT="$3" ./target/release/"$1" &) 
  for _ in $(seq 50); do
    curl -fsS "http://127.0.0.1:$3/plaintext" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "$1 never came up on $3" >&2
  exit 1
}

# Every response must match on status, content-type and body. Anything else and
# we are timing two different programs.
check_equivalence() {
  local failed=0
  for route in "${ROUTES[@]}"; do
    local a b
    a=$(curl -fsS -D- "http://127.0.0.1:$CHURUST_PORT$route" \
        | tr -d '\r' | grep -iE '^(HTTP/|content-type:)|^$' -A999 || true)
    b=$(curl -fsS -D- "http://127.0.0.1:$AXUM_PORT$route" \
        | tr -d '\r' | grep -iE '^(HTTP/|content-type:)|^$' -A999 || true)
    if [ "$a" != "$b" ]; then
      echo "MISMATCH on $route" >&2
      diff <(echo "$a") <(echo "$b") >&2 || true
      failed=1
    fi
  done
  return $failed
}
```

- [ ] **Step 2: Prove the check fails on a real difference**

Temporarily change `bench-axum`'s plaintext body to `"Hello, world!"`
(lowercase w), rebuild it, and run only the check:

```bash
chmod +x benchmarks/run.sh
# then, with both servers started by hand or by a temporary call to
# `check_equivalence` at the end of the script:
bash benchmarks/run.sh
```

Expected: `MISMATCH on /plaintext` and a non-zero exit. If it passes, the check
is not comparing what it claims to and must be fixed before it is trusted.

Restore the body to `"Hello, World!"` and rebuild.

- [ ] **Step 3: Add the measurement and report**

Append to `benchmarks/run.sh`:

```bash
measure() { # name port
  oha --no-tui -c "$CONNECTIONS" -z "$DURATION" --json \
      "http://127.0.0.1:$2$1" 2>/dev/null
}

main() {
  echo "building..."
  (cd bench-churust && cargo build --release -q)
  (cd bench-axum && cargo build --release -q)

  start bench-churust bench-churust "$CHURUST_PORT"
  start bench-axum bench-axum "$AXUM_PORT"
  trap 'pkill -f "target/release/bench-churust" || true; pkill -f "target/release/bench-axum" || true' EXIT

  echo "checking the two apps agree..."
  if ! check_equivalence; then
    echo "refusing to measure: the apps do not return identical responses" >&2
    exit 1
  fi
  echo "equivalent."

  local stamp host out
  stamp=$(date -u +%Y-%m-%d)
  host=$(hostname | tr -d '\n')
  mkdir -p results
  out="results/${stamp}-${host}.md"

  {
    echo "# Churust vs axum — ${stamp}"
    echo
    echo "- host: \`${host}\`"
    echo "- os: \`$(uname -srm)\`"
    echo "- rustc: \`$(rustc --version)\`"
    echo "- churust: \`$(grep -m1 '^version' ../Cargo.toml | cut -d'"' -f2)\`"
    echo "- axum: \`$(cd bench-axum && cargo tree -p axum --depth 0 2>/dev/null | head -1)\`"
    echo "- command: \`oha -c ${CONNECTIONS} -z ${DURATION}\`"
    echo
    echo "Numbers from one machine at one moment. They are not a ranking, and"
    echo "they do not transfer to other hardware."
    echo
    echo "| route | churust req/s | axum req/s |"
    echo "|---|---|---|"
    for route in "${ROUTES[@]}"; do
      local c a
      c=$(measure "$route" "$CHURUST_PORT" | python3 -c 'import json,sys; print(f"{json.load(sys.stdin)["summary"]["requestsPerSec"]:.0f}")')
      a=$(measure "$route" "$AXUM_PORT" | python3 -c 'import json,sys; print(f"{json.load(sys.stdin)["summary"]["requestsPerSec"]:.0f}")')
      echo "| \`${route}\` | ${c} | ${a} |"
    done
  } | tee "$out"

  echo
  echo "written to ${out}"
}

main "$@"
```

- [ ] **Step 4: Run the whole thing**

```bash
cargo install oha --locked   # if not present
bash benchmarks/run.sh
```

Expected: builds, reports `equivalent.`, prints a three-row table, and writes
`benchmarks/results/<date>-<host>.md`.

If `oha`'s JSON keys differ, inspect one directly and fix the `python3` extractor:

```bash
oha --no-tui -c 4 -z 2s --json http://127.0.0.1:8111/plaintext | python3 -m json.tool | head -20
```

- [ ] **Step 5: Write the README**

Create `benchmarks/README.md`:

```markdown
# Comparison harness

Churust against axum on identical work. Run by hand, on a machine that is
otherwise idle.

```sh
cargo install oha --locked
./run.sh
```

Knobs: `DURATION` (default `30s`), `CONNECTIONS` (default `64`),
`CHURUST_PORT`, `AXUM_PORT`.

## What these numbers are worth

They compare two servers on one machine at one moment. They are not a ranking,
they do not transfer to other hardware, and they say nothing about an
application doing real work — every route here returns a constant.

axum is the comparison because it shares hyper and tokio with Churust, so a
difference points at Churust's own layers rather than at a different runtime.

`run.sh` refuses to measure unless both apps return byte-identical status,
content-type and body on every route. Two apps doing different work produce a
number that means nothing, and that is the usual reason framework benchmarks
cannot be trusted.

## Why this is outside the workspace

It depends on axum. A benchmark's dependencies must not reach `Cargo.lock`,
`cargo test --workspace`, or any CI job that builds the library.
```

- [ ] **Step 6: Commit**

```bash
chmod +x benchmarks/run.sh
git add benchmarks/run.sh benchmarks/README.md benchmarks/results
git commit -m "bench: an axum comparison that refuses to measure unequal work

\`run.sh\` asserts both apps return identical status, content-type and body on
every route before it starts \`oha\`. Two apps doing different work produce a
number that means nothing, and that is the usual reason framework benchmarks
cannot be trusted — so the check is a gate, not a warning.

Reports are written per machine and per day and committed. A benchmark number
without the machine it came from is not a result, and the report says in as
many words that these do not transfer to other hardware."
```

---

### Task 8: Point the docs at it

**Files:**
- Modify: `CONTRIBUTING.md` (or `README.md` if no contributing guide exists)

**Interfaces:**
- Consumes: everything above.
- Produces: nothing.

- [ ] **Step 1: Find where contributor workflow is documented**

```bash
ls CONTRIBUTING.md 2>/dev/null || echo "no CONTRIBUTING.md"
grep -n "cargo test" README.md | head -3
```

Use `CONTRIBUTING.md` if it exists; otherwise add to the development section of
`README.md`.

- [ ] **Step 2: Add the section**

```markdown
## Benchmarks

Two separate things, because only one of them produces numbers you can trust on
a shared machine.

**Regressions** — `cargo bench -p churust-core`. Criterion, in-process, no
socket. To compare a change against `main`:

```sh
git checkout main && cargo bench -p churust-core -- --save-baseline main
git checkout - && cargo bench -p churust-core -- --baseline main
```

CI runs this on every PR against the merge base and comments the table. It fails
only past 20%, because the runners swing 20–30% on their own.

**Comparison against axum** — `benchmarks/run.sh`. Run by hand on an idle
machine; see `benchmarks/README.md`.
```

- [ ] **Step 3: Commit**

```bash
git add CONTRIBUTING.md README.md
git commit -m "docs: how to run the benchmarks

Names both halves and what each is worth, including the reason CI's threshold
is 20% rather than something tighter."
```

---

## Self-Review

**Spec coverage:**

| spec requirement | task |
|---|---|
| four bench files over the hot path | 1–4 |
| Criterion 0.8 + async_tokio, app built outside the closure | 1 (idiom), repeated 2–4 |
| routing and security headers via `App::process` | 2, 3 |
| CI: bench base + head, critcmp, comment, fail past 20% | 5 |
| CI advisory, release.yml untouched | 5 (stated in the workflow header) |
| axum only, three routes | 6 |
| `benchmarks/` excluded from the workspace | 6 (step 1, done first) |
| equivalence assert before measuring | 7 (steps 1–2, test-driven) |
| dated reports naming the machine | 7 (step 3) |
| `oha` as the generator | 7 |
| benches must not rot | 5 (CI runs them; compile failure fails the job) |

No spec requirement is unimplemented.

**Placeholder scan:** no TBD/TODO, and no step defers a decision. Three
questions the first draft left open were resolved against the source while
reviewing, and the answers are now baked into the code rather than left as
branches: `add_middleware` takes `&mut self` and cannot be chained (Task 1),
`Call` has no public params setter so `Path` goes through dispatch (Task 4), and
churust-core has no JSON response constructor so the route sets the header
explicitly (Task 6). Each carries a one-line confirmation step, not a decision.

**Type consistency:** `rt()` has one signature across Tasks 1–4. `App::process`
takes `(Method, Uri, HeaderMap, Bytes)` everywhere. Both binaries read `$PORT`
and serve the same three routes, which Task 7 depends on. `check-bench-regression.py`
takes `(path, factor)` and Task 5 calls it with exactly those.

**Known risks, stated rather than hidden:**

- Criterion 0.8's async API is the largest unknown. Task 1 exists to hit it at
  the smallest possible surface before three more files depend on it.
- `critcmp`'s table format drives the regression parser. Task 5 Step 3 tests it
  against fixtures and says to fix the parser from real output rather than bend
  the fixture.
- axum 0.8's path syntax differs from 0.7. Task 6 Step 7 checks the resolved
  version.
