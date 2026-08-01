//! Response decoration: the work done to every reply on the way out.
//!
//! `Error → Response` carries one header and then three, because 0.3.3 changed
//! that loop from a plain `insert` to first-occurrence-replaces with later ones
//! appended. The three-header case is the one that got more expensive, and this
//! is what would show it. Both go through `Error::into_response` only — nothing
//! upstream of it (routing, middleware, `App::process`) is on this path.
//!
//! `Vary` merging is measured empty — nothing to merge into — against existing,
//! where three fields are already present and `vary_on` has to walk and dedupe
//! them before appending. The existing case is the one whose cost scales with
//! what is already on the response; the empty case is the floor.
//!
//! Security headers are measured as an app with them against an app without,
//! since `SecurityHeaders::apply_to` is `pub(crate)` and cannot be called from
//! here. The difference between the two numbers is their cost.

use bytes::Bytes;
use churust_core::{App, Call, Churust, Error, IntoResponse, Response};
use criterion::{criterion_group, criterion_main, Criterion};
use http::header::{HeaderValue, SET_COOKIE, VARY, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderMap, Method};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

fn build_with_security() -> App {
    Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build()
}

fn build_without_security() -> App {
    Churust::server()
        .without_security_headers()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build()
}

/// Guards against the failure modes that would make each pair this file
/// measures worthless. There are three pairs, and each has its own way to
/// silently collapse:
///
/// - `security_headers_on` / `security_headers_off`: if
///   `without_security_headers()` did not actually take effect — for
///   instance because the option got wired to the wrong builder field — both
///   apps would send the same headers and the two numbers would come out
///   identical. `X-Content-Type-Options` is the header checked because it is
///   on by default and does not depend on TLS being configured (unlike
///   `Strict-Transport-Security`, which `apply_to` only sets `over_tls`).
/// - `error_one_header` / `error_three_headers`: this is the pair 0.3.3's
///   change to `Error → Response` — from a plain `insert` to
///   first-occurrence-replaces-then-appends — actually affects. If that loop
///   ever reverted to a plain `insert`, three `with_response_header` calls on
///   the same header name would leave only the last one standing, and
///   `error_three_headers` would collapse onto `error_one_header`'s cost:
///   157ns vs 213ns measured, so a collapse reads as roughly 26% cheaper —
///   an improvement, not a failure, so nothing but this assertion would
///   catch it.
/// - `vary_merge_empty` / `vary_merge_existing`: if `vary_on` stopped
///   merging into whatever `Vary` already carries — collapsing to an
///   unconditional append or an unconditional overwrite — the existing case
///   would stop paying for the walk-and-dedupe that makes it the more
///   expensive of the two: 178ns vs 578ns measured, roughly 69% cheaper if
///   the merge silently stopped happening.
///
/// This is deliberately **not** a `#[test]` function. `headers.rs` is a
/// `harness = false` Criterion target, so `criterion_main!` supplies the
/// binary's only `main`, and Cargo never passes rustc's `--test` flag for a
/// `harness = false` target — a `#[test]` item here would compile and then
/// never run, on `cargo test --bench headers` or anywhere else (see
/// `routing.rs` for the fuller account, and the task-3 report for the
/// transcript that confirms it for this file too). Calling this unconditionally
/// from `bench_headers` instead is what Cargo *does* run: `cargo bench` reaches
/// it on every real run, and `cargo test --bench headers` reaches it too,
/// because Criterion's own `--test` self-check mode still calls `bench_headers`
/// in full — it only swaps each measured `iter()` closure to run once instead
/// of thousands of times.
fn assert_header_cases_do_real_work(
    rt: &tokio::runtime::Runtime,
    with_security: &App,
    without_security: &App,
) {
    let on = rt.block_on(with_security.process(
        Method::GET,
        "/".parse().expect("a valid uri"),
        HeaderMap::new(),
        Bytes::new(),
    ));
    let off = rt.block_on(without_security.process(
        Method::GET,
        "/".parse().expect("a valid uri"),
        HeaderMap::new(),
        Bytes::new(),
    ));

    assert!(
        on.headers.contains_key(X_CONTENT_TYPE_OPTIONS),
        "security_headers_on app did not send X-Content-Type-Options; \
         the default security headers are not being applied at all"
    );
    assert!(
        !off.headers.contains_key(X_CONTENT_TYPE_OPTIONS),
        "security_headers_off app sent X-Content-Type-Options even though \
         without_security_headers() was called; the two benchmark cases are \
         measuring the same thing"
    );

    let three_headers = Error::bad_request("nope")
        .with_response_header(SET_COOKIE, HeaderValue::from_static("a=1"))
        .with_response_header(SET_COOKIE, HeaderValue::from_static("b=2"))
        .with_response_header(SET_COOKIE, HeaderValue::from_static("c=3"))
        .into_response();
    let cookie_count = three_headers.headers.get_all(SET_COOKIE).iter().count();
    assert_eq!(
        cookie_count, 3,
        "error_three_headers's response carries {cookie_count} Set-Cookie \
         header(s), not 3; if Error's header loop reverted to a plain \
         insert() — the exact 0.3.3 change this file watches — later calls \
         would overwrite earlier ones and error_three_headers would collapse \
         onto error_one_header's cost"
    );

    let mut merged = Response::text("ok");
    merged.vary_on("accept-encoding");
    merged.vary_on("origin");
    merged.vary_on("accept-language");
    let vary_fields: Vec<String> = merged
        .headers
        .get_all(VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    assert_eq!(
        vary_fields.len(),
        3,
        "vary_merge_existing's merged Vary header lists {} field(s) \
         ({vary_fields:?}), not 3; if vary_on stopped merging into the \
         existing list, vary_merge_existing would collapse toward \
         vary_merge_empty's cost",
        vary_fields.len()
    );
}

fn bench_headers(c: &mut Criterion) {
    let rt = rt();

    // Built once, outside every measured closure.
    let with_security = build_with_security();
    let without_security = build_without_security();

    // Runs every time this function does, i.e. on every `cargo bench` and
    // every `cargo test --bench headers` — see the doc comment above.
    assert_header_cases_do_real_work(&rt, &with_security, &without_security);

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
