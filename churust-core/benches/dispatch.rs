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
use churust_core::{App, Call, Churust, Middleware, Next, Response};
use criterion::{criterion_group, criterion_main, Criterion};
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, Method, StatusCode};
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
///
/// It appends a marker header on the way out — the cheapest possible thing
/// that still leaves a trace in the response. Without that trace, `bare_200`
/// and `three_middleware` would produce byte-identical responses, and the
/// guard below (which exists specifically to catch `three_middleware`
/// collapsing onto `bare_200`) would have nothing to check.
struct Noop;

const NOOP_MARKER: HeaderName = HeaderName::from_static("x-noop-ran");

#[async_trait::async_trait]
impl Middleware for Noop {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let mut res = next.run(call).await;
        res.headers
            .append(NOOP_MARKER, HeaderValue::from_static("1"));
        res
    }
}

/// Guards against the two failure modes that would make this file actively
/// misleading rather than merely wrong. Unlike the other three bench files,
/// nothing here was checked before: this is the only file in the suite with
/// no guard at all, and it is the most dangerous one to leave that way,
/// because the CI gate (`.github/scripts/check-bench-regression.py`) only
/// fires on a *regression* past 20% — it never looks at an improvement. A
/// bench that starts silently measuring less work is not just unverified
/// here, it is actively reported as a win.
///
/// 1. If `/hello` stopped matching — a typo in the route string, a routing
///    regression — `bare_200` would silently become the `not_found` path.
///    Measured, that path is about 13% cheaper than a real dispatch, which
///    is *under* the 20% gate, so nothing would flag it; the benchmark would
///    just quietly start reporting a made-up speedup forever.
/// 2. If `add_middleware` stopped installing anything — the pipeline wiring
///    breaking, the `Vec` of middleware never being consulted —
///    `three_middleware` would collapse onto exactly what `bare_200` does.
///    Measured, that is about 23% cheaper, again comfortably under the gate.
///
/// Checking status and body alone cannot catch case 2: a bare 200 and a
/// 200-behind-three-middleware both return `hello` with the same status, so
/// a collapse there is invisible to `status`/`body` assertions. `Noop`
/// therefore appends `x-noop-ran` on the way out (see its doc comment), and
/// the guard asserts it shows up exactly three times — once per installed
/// middleware — which is the one thing that cannot be true if the pipeline
/// silently stopped running them.
///
/// This is deliberately **not** a `#[test]` function, for the reason
/// documented at length on `routing.rs`'s
/// `assert_every_route_reaches_its_own_handler`: `dispatch.rs` is a `harness
/// = false` Criterion target, so a `#[test]` item here would compile and
/// then never run, under `cargo test --bench dispatch` or anywhere else.
/// Calling this unconditionally from `bench_dispatch` is what Cargo *does*
/// run: every `cargo bench` and every `cargo test --bench dispatch` reaches
/// it, because Criterion's own `--test` self-check mode still calls
/// `bench_dispatch` in full — it only swaps each measured `iter()` closure to
/// run once instead of thousands of times.
async fn assert_dispatch_cases_do_real_work(bare: &App, with_middleware: &App) {
    let bare_ok = bare
        .process(
            Method::GET,
            "/hello".parse().expect("a valid uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await;
    assert_eq!(
        bare_ok.status,
        StatusCode::OK,
        "GET /hello on the bare app did not return 200; bare_200 would be \
         measuring a failure path instead of a successful dispatch"
    );
    assert_eq!(
        bare_ok.body.as_slice(),
        Some(b"hello".as_slice()),
        "GET /hello on the bare app returned an unexpected body"
    );
    assert!(
        !bare_ok.headers.contains_key(NOOP_MARKER),
        "the bare app's response carries the middleware marker header; \
         bare_200 and three_middleware would be measuring the same pipeline"
    );

    let miss = bare
        .process(
            Method::GET,
            "/nothing-here".parse().expect("a valid uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await;
    assert_eq!(
        miss.status,
        StatusCode::NOT_FOUND,
        "GET /nothing-here did not 404; not_found would be measuring \
         something other than the miss path it claims to"
    );

    let with_mw = with_middleware
        .process(
            Method::GET,
            "/hello".parse().expect("a valid uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await;
    assert_eq!(
        with_mw.status,
        StatusCode::OK,
        "GET /hello on the middleware app did not return 200"
    );
    assert_eq!(
        with_mw.body.as_slice(),
        Some(b"hello".as_slice()),
        "GET /hello on the middleware app returned an unexpected body"
    );
    let noop_runs = with_mw.headers.get_all(NOOP_MARKER).iter().count();
    assert_eq!(
        noop_runs, 3,
        "expected exactly 3 middleware layers to have run (one marker header \
         per layer), got {noop_runs}; three_middleware would be measuring \
         fewer layers than it claims to, or the same pipeline as bare_200"
    );
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

    // Runs every time this function does, i.e. on every `cargo bench` and
    // every `cargo test --bench dispatch` — see the doc comment above.
    rt.block_on(assert_dispatch_cases_do_real_work(&bare, &with_middleware));

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
