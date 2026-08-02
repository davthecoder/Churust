//! One of five apps in the comparison. The routes here must stay
//! byte-identical to the other four's — `run.sh` refuses to measure if they
//! diverge.

use churust_core::{Call, Churust};

// No `#[tokio::main]`: `run_sharded` builds and owns its own runtimes, and a
// multi-threaded runtime wrapped around it would sit idle underneath the
// single-threaded ones doing the work.
fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT")
        .expect("PORT must be set")
        .parse()
        .expect("PORT must be a number");

    // `0` means one worker per core, which is what actix-web's `HttpServer`
    // and the Go runtime default to as well. `WORKERS=1` collapses it to a
    // single runtime, which is how the sharded and shared shapes are compared
    // side by side in the results.
    let workers: usize = std::env::var("WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut builder = Churust::server().host("127.0.0.1").port(port);

    // This comparison measures dispatch overhead between frameworks. Churust's
    // default builder sends five security headers (`X-Content-Type-Options`,
    // `X-Frame-Options`, `Referrer-Policy`, `Permissions-Policy`,
    // `Cross-Origin-Resource-Policy`) that none of the other five apps send;
    // left in place, the apps would be doing different work, and any throughput
    // gap would partly be measuring that difference rather than the frameworks'
    // dispatch paths. Of the two honest ways to equalise — add the headers to
    // every other app, or drop them here — this is the cheaper one. Their cost
    // is measured directly in `churust-core/benches/headers.rs`, as the
    // `security_headers_on` vs. `security_headers_off` pair.
    //
    // Do not make this unconditional "to be safe": that reintroduces the exact
    // confound described above. Do not delete the toggle either — with the
    // headers always off, nothing here could measure the configuration real
    // deployments actually run, which is how a per-request `HeaderMap` grow and
    // rehash on the default path went unnoticed until it was found by profiling
    // rather than by any number this harness prints. `APPS=churust SECURITY=1`
    // benchmarks the shape users actually get.
    if std::env::var("SECURITY").as_deref() != Ok("1") {
        builder = builder.without_security_headers();
    }

    let app = builder
        // The load generator pipelines (see benchmarks/pipeline.lua and the
        // note in README.md about why). Answering a batch of 64 requests with
        // 64 flushes rather than one is 64 write syscalls where one would do,
        // and it is the single largest cost in this measurement — so the app
        // is told that its client pipelines. This is a per-application choice
        // in every framework here; actix-http does the same aggregation and
        // does not make it optional.
        .pipeline_flush(std::env::var("PIPELINE_FLUSH").as_deref() != Ok("0"))
        .routing(|r| {
            r.get("/plaintext", |_c: Call| async {
                // `Response::bytes` rather than the `&'static str` `IntoResponse`
                // impl (which goes through `Response::text`'s `impl Into<String>`
                // and therefore `str::to_owned()` — a heap allocation and copy on
                // every request for a compile-time literal). `bytes` takes the
                // literal straight into a zero-copy `Bytes::from_static`, matching
                // axum's own zero-copy `&'static str` path
                // (`Cow::Borrowed` -> `Bytes::from_static`). Without this, the
                // comparison would charge Churust a malloc + memcpy + free that
                // axum never pays, for reasons that have nothing to do with either
                // framework's dispatch path.
                churust_core::Response::bytes("text/plain; charset=utf-8", "Hello, World!")
            });
            r.get("/json", |_c: Call| async {
                // Explicit rather than a `json` helper: churust-core has no
                // JSON response constructor (that lives in churust-json, which
                // this app deliberately does not pull in — the comparison is of
                // core dispatch, not of a plugin). `bytes`, not `text` +
                // `with_header`, for the same zero-copy reason as `/plaintext`
                // above.
                churust_core::Response::bytes("application/json", r#"{"message":"Hello, World!"}"#)
            });
            r.get("/user/{id}", |churust_core::Path(id): churust_core::Path<u64>| async move {
                format!("user {id}")
            });
        })
        .build();

    // `SHARDED=0` serves on the default shared work-stealing runtime instead, so
    // one binary can be measured both ways. Throughput and tail latency both
    // move between them; see benchmarks/results/.
    if std::env::var("SHARDED").as_deref() == Ok("0") {
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(app.start());
    }

    // No `#[tokio::main]`: `run_sharded` builds and owns its own runtimes, and a
    // multi-threaded runtime wrapped around it would sit idle underneath the
    // single-threaded ones doing the work.
    app.run_sharded(workers)
}
