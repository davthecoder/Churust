# Comparison harness

Churust against actix-web, axum, Ktor and Go on identical work. Run by hand, on
a machine that is otherwise idle.

```sh
cargo install oha --locked
brew install wrk gradle          # or your platform's equivalent
./run.sh
```

Knobs: `DURATION` (default `10s`), `WRK_DURATION` (`8s`), `CONNECTIONS` (`64`),
`DEPTH` (`64`), `ROUNDS` (`3`), `APPS` (all five), `BENCH_JAVA_HOME`, and one
`*_PORT` per app.

`APPS` is how you cope with a missing toolchain: `APPS="churust axum" ./run.sh`
runs the two that need nothing but cargo.

## What these numbers are worth

They compare five servers on one machine at one moment. They are not a ranking,
they do not transfer to other hardware, and they say nothing about an
application doing real work — every route here returns a constant.

The frameworks are the ones a Churust user would otherwise have picked:
actix-web and axum in Rust, Ktor because Churust's whole shape is borrowed from
it, and Go's `net/http` because that is what "a Go backend" means to almost
everyone writing one.

## The gate: same work, or no measurement

`run.sh` refuses to measure unless all five apps return equivalent status,
headers and body on every route. Two apps doing different work produce a number
that means nothing, and that is the usual reason framework benchmarks cannot be
trusted.

Two things are normalised before comparing, and nothing else:

- **`date`**, which legitimately differs on every request. Ktor omits it
  entirely, which is one header of work it does not do — noted in the results
  rather than hidden.
- **Header name case and header order**, because RFC 9110 §5.1 makes field names
  case-insensitive and §5.3 makes the order of differently-named fields
  insignificant. Go writes `Content-Type` where hyper writes `content-type`, and
  a gate that called that a difference could never admit a Go server at all.

A stray header, a wrong `content-length` or a missing `content-type` is a real
difference and fails the gate.

## Why the load is pipelined

**Because on this class of machine the unpipelined measurement cannot tell the
five apart, and reporting it as though it could is how the first version of this
harness came to publish a table where every row was between 50.6k and 51.7k and
call it a comparison.**

macOS's loopback path saturates at roughly 45,000 small round-trips per second
on the machine these were run on. Every server here sits on that ceiling, so
every server reports the same number. The proof is not an inference:

- one server at 16 / 64 / 256 / 512 connections: 44.9k, 45.4k, 45.7k, 46.0k.
  More concurrency does not move it.
- **two different servers, two load generators, two ports, at the same time:
  22.4k each.** They split one budget. The limit is in neither of them.
- the server process used under one core of twelve during those runs, while
  `kernel_task` used a full one.

HTTP/1.1 pipelining puts many requests in one segment, so the kernel does a
fraction of the per-request work and the server's own parse-route-respond path
becomes the thing being measured. At depth 64 the same hardware reports 2.9M
req/s for the fastest app — sixty times the unpipelined ceiling, from the same
five servers, over the same wire.

Depth 64 rather than TechEmpower's 16, because at 16 the servers are *still* on
the packet ceiling here: the top two both report about 700k, which is 44k
batches per second — the same 44k as before, in a different costume.

Pipelining is not a simulation of browser traffic. `run.sh` reports the
unpipelined run next to it for exactly that reason, and the results file says
plainly that its ranking is noise.

## The charts

`charts.py` renders the figures in `../docs/assets/` from the numbers in
`results/`. It has no dependencies and it does not parse the results files:
the numbers are transcribed into the script with the file they came from named
beside them, because a chart that silently redraws itself when a results file
changes is a chart nobody can date.

```sh
python3 charts.py
```

## Server CPU per request

Both modes record how much CPU each server process spent, divided by the
requests it served. It is the one number that survives a saturated wire: when
every server is pinned to the same kernel limit, what still separates them is
how much CPU each burned getting there.

## Configuration that is not the default

Two settings in `bench-churust` differ from what `Churust::server()` gives you,
both so that the five apps do equal work:

- **`.without_security_headers()`**. Churust sends five security headers by
  default (`X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`,
  `Permissions-Policy`, `Cross-Origin-Resource-Policy`); none of the other four
  apps sends any. Of the two honest ways to equalise — add them everywhere else,
  or drop them here — this is the cheaper one, and it hides nothing: their cost
  is measured on its own in `churust-core/benches/headers.rs`, as the
  `security_headers_on` / `security_headers_off` pair.
- **`.pipeline_flush(true)`** for the pipelined pass, off for the unpipelined
  one. Answering a batch of 64 pipelined requests with 64 flushes instead of one
  is 64 write syscalls where one would do; answering a client that does *not*
  pipeline with an aggregated flush is a deliberate delay on every response. So
  it is a per-application choice rather than a default, and `run.sh` restarts
  Churust between the two passes rather than measuring one configuration in
  both. actix-http aggregates unconditionally; `axum::serve` exposes no way to
  ask for it, which is most of why axum's pipelined number is what it is.

`bench-churust` also uses `App::run_sharded`: one runtime per core, with each
connection pinned to a worker for its whole life. That is what actix-web's
`HttpServer` does by default and roughly what Go's runtime approximates.
`WORKERS=1` collapses it to a single runtime for a side-by-side.

## Why this is outside the workspace

It depends on axum and actix-web. A benchmark's dependencies must not reach
`Cargo.lock`, `cargo test --workspace`, or any CI job that builds the library.
