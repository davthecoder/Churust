//! Response decoration: the work done to every reply on the way out.
//!
//! `Error → Response` carries one header and then three, because 0.3.3 changed
//! that loop from a plain `insert` to first-occurrence-replaces with later ones
//! appended. The three-header case is the one that got more expensive, and this
//! is what would show it. Both go through `Error::into_response` only — nothing
//! upstream of it (routing, middleware, `App::process`) is on this path.
//!
//! Security headers are measured as an app with them against an app without,
//! since `SecurityHeaders::apply_to` is `pub(crate)` and cannot be called from
//! here. The difference between the two numbers is their cost.

use bytes::Bytes;
use churust_core::{App, Call, Churust, Error, IntoResponse, Response};
use criterion::{criterion_group, criterion_main, Criterion};
use http::header::{HeaderValue, SET_COOKIE, X_CONTENT_TYPE_OPTIONS};
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

/// Guards against the one failure mode that would make the
/// `security_headers_on` / `security_headers_off` pair worthless: if
/// `without_security_headers()` did not actually take effect — for instance
/// because the option got wired to the wrong builder field — both apps would
/// send the same headers and the two benchmark numbers would come out
/// identical. That reads as "security headers are free", which is a wrong
/// conclusion delivered silently rather than a build failure, so it must not
/// be allowed to pass unnoticed.
///
/// `X-Content-Type-Options` is the header to check because it is on by
/// default and does not depend on TLS being configured (unlike
/// `Strict-Transport-Security`, which `apply_to` only sets `over_tls`).
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
fn assert_security_headers_toggle_actually_toggles(
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
}

fn bench_headers(c: &mut Criterion) {
    let rt = rt();

    // Built once, outside every measured closure.
    let with_security = build_with_security();
    let without_security = build_without_security();

    // Runs every time this function does, i.e. on every `cargo bench` and
    // every `cargo test --bench headers` — see the doc comment above.
    assert_security_headers_toggle_actually_toggles(&rt, &with_security, &without_security);

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
