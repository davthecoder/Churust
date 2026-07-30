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

They compare two servers on one machine at one moment. They are not a
ranking, they do not transfer to other hardware, and they say nothing about
an application doing real work — every route here returns a constant.

axum is the comparison because it shares hyper and tokio with Churust, so a
difference points at Churust's own layers rather than at a different
runtime.

`run.sh` refuses to measure unless both apps return equivalent status,
headers (aside from the per-request `date` header, which cannot match) and
body on every route. Two apps doing different work produce a number that
means nothing, and that is the usual reason framework benchmarks cannot be
trusted.

`bench-churust` runs with `.without_security_headers()`, so Churust does not
send the five security headers (`X-Content-Type-Options`, `X-Frame-Options`,
`Referrer-Policy`, `Permissions-Policy`, `Cross-Origin-Resource-Policy`) it
sends by default — `bench-axum`'s bare `Router` sends none of them either,
and adding them there would mean pulling in `tower-http` or hand-rolled
middleware. Removing them here is the cheaper of the two ways to make both
sides do equal work, and it hides nothing: their cost is measured directly,
on its own, in `churust-core/benches/headers.rs` (the `security_headers_on`
vs. `security_headers_off` pair).

## Why this is outside the workspace

It depends on axum. A benchmark's dependencies must not reach `Cargo.lock`,
`cargo test --workspace`, or any CI job that builds the library.
