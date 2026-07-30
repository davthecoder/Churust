//! What every request costs, end to end, with no socket in the way.
//!
//! `App::process` is the same entry point `TestClient` uses, so these numbers
//! cover routing, the middleware pipeline, extraction and response building
//! without kernel or TCP noise. The `Host` validation added in 0.3.3 is on this
//! path.

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
