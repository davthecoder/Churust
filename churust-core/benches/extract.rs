//! Extraction, which runs once per handler argument per request.
//!
//! `Query` is measured against a `Call` built once, outside the measured
//! closure, because it reads from the URI and needs nothing the router
//! provides — the closure measures extraction alone, not `Uri::parse` or
//! `Call::new`'s allocation. `Path` needs captured parameters, which only the
//! router populates — a hand-built `Call` has none, since `Call::params()` is
//! a getter with no public setter — so `Path` goes through `App::process`
//! against a `/users/{id}` route instead. That means the `Path` number
//! carries full dispatch overhead (routing, the pipeline, response assembly)
//! on top of the extraction itself, while the `Query` number is extraction
//! alone; the two are not directly comparable.

use bytes::Bytes;
use churust_core::{App, Call, Churust, FromCallParts, Query};
use criterion::{criterion_group, criterion_main, Criterion};
use futures_util::FutureExt;
use http::{HeaderMap, Method, StatusCode};
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

/// Guards against the one failure mode that would make `query_two_fields`
/// worthless: if the query string stopped parsing (a typo, a field rename
/// that drifted out of sync, a serde_html_form behavior change), extraction
/// would fail on every iteration. A failing extraction is much cheaper than a
/// successful one — it returns as soon as parsing errors instead of
/// populating `Page` — so the benchmark would quietly get faster and read as
/// an improvement rather than fail loudly. Checking the decoded fields, not
/// just `is_ok()`, also rules out a quieter variant of the same problem: a
/// typo that still parses but binds the wrong value (e.g. `page` picking up
/// `per_page`'s value).
///
/// This is deliberately **not** a `#[test]` function. `extract.rs` is a
/// `harness = false` Criterion target, so `criterion_main!` supplies the
/// binary's only `main`, and Cargo never passes rustc's `--test` flag for a
/// `harness = false` target — a `#[test]` item here would compile and then
/// never run, on `cargo test --bench extract` or anywhere else. Calling this
/// unconditionally from `bench_extract` instead is what Cargo *does* run:
/// `cargo bench` reaches it on every real run, and `cargo test --bench
/// extract` reaches it too, because Criterion's own `--test` self-check mode
/// still calls `bench_extract` in full — it only swaps each measured `iter()`
/// closure to run once instead of thousands of times.
fn assert_query_extraction_succeeds(rt: &tokio::runtime::Runtime) {
    let mut call = Call::new(
        Method::GET,
        "/?page=3&per_page=50".parse().expect("a valid uri"),
        HeaderMap::new(),
        Bytes::new(),
    );
    let got = rt.block_on(Query::<Page>::from_call_parts(&mut call));
    let Query(page) = got.expect(
        "query extraction failed; the benchmark would measure the error path, \
         which is cheaper than success and would read as an improvement",
    );
    assert_eq!(page.page, 3, "page field decoded to the wrong value");
    assert_eq!(
        page.per_page, 50,
        "per_page field decoded to the wrong value"
    );
}

/// Guards against the one failure mode that would make
/// `path_param_via_dispatch` worthless: `App::process` never returns `Err`
/// and never panics when a route fails to match — a miss is an ordinary
/// `Response` carrying a 404 status, not an error. If `/users/{id}` ever
/// stopped matching `/users/42` (a routing regression, a path-pattern syntax
/// change, ...), the benchmark would silently measure the 404 path instead
/// of a full dispatch through to the handler and its `Path` extraction — and
/// a 404 is cheaper than a successful dispatch, so this would read as a
/// speedup rather than fail loudly. Criterion's own `--test` self-check mode
/// only catches panics, not response status codes, so an explicit assertion
/// here is the only thing that can catch this; see the module doc comment.
///
/// Deliberately **not** a `#[test]` function, for the same reason given on
/// `assert_query_extraction_succeeds` above: `extract.rs` is a `harness =
/// false` Criterion target, so a `#[test]` item would compile and never run.
/// Calling this unconditionally from `bench_extract` is what actually runs on
/// every `cargo bench` and every `cargo test --bench extract`.
fn assert_path_param_dispatch_succeeds(rt: &tokio::runtime::Runtime, path_app: &App) {
    let res = rt.block_on(path_app.process(
        Method::GET,
        "/users/42".parse().expect("a valid uri"),
        HeaderMap::new(),
        Bytes::new(),
    ));
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /users/42 did not reach the handler; the benchmark would measure \
         a routing miss, which is cheaper than a successful dispatch and would \
         read as a speedup"
    );
    assert_eq!(
        res.body.as_slice(),
        Some(b"ok".as_slice()),
        "GET /users/42 reached a different handler than expected"
    );
}

fn bench_extract(c: &mut Criterion) {
    let rt = rt();

    // Built once, outside every measured closure.
    let path_app = Churust::server()
        .routing(|r| {
            r.get("/users/{id}", |_c: Call| async { "ok" });
        })
        .build();

    // Runs every time this function does, i.e. on every `cargo bench` and
    // every `cargo test --bench extract` — see the doc comments above the
    // guard functions.
    assert_query_extraction_succeeds(&rt);
    assert_path_param_dispatch_succeeds(&rt, &path_app);

    let mut group = c.benchmark_group("extract");

    // Built once, outside the measured closure: `Query::from_call_parts`
    // (`churust-core/src/extract.rs`) only reads `call.query_string()`
    // (`Call::query_string`, a non-mutating `&self` accessor), so the same
    // `Call` can be reused across every iteration without affecting what's
    // measured. Building a fresh `Call` per iteration would fold in
    // `Uri::parse` and `Call::new`'s allocation, which is not what
    // "extraction alone" (see the module doc comment) is meant to measure.
    let mut query_call = Call::new(
        Method::GET,
        "/?page=3&per_page=50".parse().expect("a valid uri"),
        HeaderMap::new(),
        Bytes::new(),
    );

    // A plain (non-async) `b.iter`, not `b.to_async(&rt).iter`: capturing
    // `&mut query_call` in a closure that returns an `async` block hits a
    // hard rustc limitation — "captured variable cannot escape `FnMut`
    // closure body" — because a `&mut` upvar cannot be threaded into a
    // `Future` the closure returns. Wrapping `query_call` in a `RefCell`
    // works around *that*, but then trades it for
    // `clippy::await_holding_refcell_ref` under `-D warnings`, since the
    // `RefMut` would be held across the `.await`. `Query::from_call_parts`'s
    // body (`churust-core/src/extract.rs`) has no `.await` of its own, so the
    // `async-trait`-generated future always completes on its first poll;
    // `now_or_never()` polls it exactly once, synchronously, doing the same
    // work the `.await` would do, and `.expect(...)` turns "it didn't
    // resolve" into a loud failure rather than a silent `None` if that ever
    // stops being true.
    group.bench_function("query_two_fields", |b| {
        b.iter(|| {
            Query::<Page>::from_call_parts(&mut query_call)
                .now_or_never()
                .expect("from_call_parts did not resolve on the first poll")
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
