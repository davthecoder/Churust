//! How much the *shape* of a route costs.
//!
//! Measured through `App::process` rather than against `Router` directly: a
//! benchmark is an external crate, `RouteBuilder::new` is `pub(crate)`, and so a
//! bare `Router` cannot be populated from here. Every number therefore carries
//! the same constant dispatch overhead that `dispatch.rs` measures separately.
//! That makes these numbers useful for comparing shapes against each other and
//! for catching a regression over time, and NOT a measurement of routing's
//! absolute cost — they include everything `App::process` does, not just the
//! router's match step.
//!
//! The route table is deliberately larger than the routes under test, so a
//! change that makes matching scale with table size shows up.

use bytes::Bytes;
use churust_core::{App, Call, Churust};
use criterion::{criterion_group, criterion_main, Criterion};
use http::{HeaderMap, Method};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

/// The one route table shared by the benchmark and by
/// `assert_every_route_reaches_its_own_handler`, so the two can never drift
/// apart: whatever the guard below exercises is exactly what `bench_routing`
/// measures.
fn build_app() -> App {
    Churust::server()
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
        .build()
}

/// Guards against the one failure mode that would make this whole file
/// worthless: if every registered path 404s — for instance because the
/// `{name...}` wildcard syntax is wrong and quietly became a literal path
/// segment — every "shape" number in `bench_routing` would collapse to the
/// same not-found cost. That reads as a suspiciously flat, fast benchmark
/// rather than an obviously broken one, so it must not be allowed to pass
/// silently.
///
/// This is deliberately **not** a `#[test]` function. `routing.rs` is a
/// `harness = false` Criterion target, so `criterion_main!` supplies the
/// binary's only `main`, and Cargo never passes rustc's `--test` flag for a
/// `harness = false` target (confirmed with `cargo test -p churust-core
/// --bench routing -v`, and separately by deliberately breaking a `#[test]`
/// function's assertion here and observing `cargo test --bench routing`
/// still exit 0 — the function was silently never called). A `#[test]` item
/// in this file compiles but has no harness to run it, which is exactly the
/// silent-pass failure mode this guard exists to prevent, so writing one
/// here would be worse than writing nothing.
///
/// Calling this unconditionally from `bench_routing` instead is what Cargo
/// *does* run: `cargo bench` reaches it as part of a real run, and
/// `cargo test --bench routing` reaches it too, because Criterion's own
/// `--test` self-check mode still calls `bench_routing` in full — it only
/// swaps each measured `iter()` closure to run once instead of thousands of
/// times. Both are exercised below; see the report for the transcripts.
fn assert_every_route_reaches_its_own_handler(rt: &tokio::runtime::Runtime, app: &App) {
    for (name, path, expect) in [
        ("static", "/static/path/here", "static"),
        ("param", "/users/42", "param"),
        ("wildcard", "/files/some/nested/thing.txt", "wildcard"),
        ("deep", "/a/b/c/d/e/f/g/h", "deep"),
    ] {
        let res = rt.block_on(app.process(
            Method::GET,
            path.parse().expect("a valid uri"),
            HeaderMap::new(),
            Bytes::new(),
        ));
        assert_eq!(
            res.body,
            Bytes::from(expect),
            "{name} ({path}) did not reach its own handler"
        );
    }
}

fn bench_routing(c: &mut Criterion) {
    let rt = rt();

    // Built once, outside every measured closure.
    let app = build_app();

    // Runs every time this function does, i.e. on every `cargo bench` and
    // every `cargo test --bench routing` — see the doc comment above.
    assert_every_route_reaches_its_own_handler(&rt, &app);

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
