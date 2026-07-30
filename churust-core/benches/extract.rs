//! Extraction, which runs once per handler argument per request.
//!
//! `Query` is measured against a `Call` built directly, because it reads from
//! the URI and needs nothing the router provides. `Path` needs captured
//! parameters, which only the router populates — a hand-built `Call` has none,
//! since `Call::params()` is a getter with no public setter — so `Path` goes
//! through `App::process` against a `/users/{id}` route instead. That means
//! the `Path` number carries full dispatch overhead (routing, the pipeline,
//! response assembly) on top of the extraction itself, while the `Query`
//! number is extraction alone; the two are not directly comparable.

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

fn bench_extract(c: &mut Criterion) {
    let rt = rt();

    // Built once, outside every measured closure.
    let path_app = Churust::server()
        .routing(|r| {
            r.get("/users/{id}", |_c: Call| async { "ok" });
        })
        .build();

    // Runs every time this function does, i.e. on every `cargo bench` and
    // every `cargo test --bench extract` — see the doc comment above.
    assert_query_extraction_succeeds(&rt);

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
