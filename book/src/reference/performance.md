# Performance

Churust is fast, and this page is about being precise rather than loud: what was
measured, against what, on what, and what the number does not cover.

![Requests per second, HTTP/1.1 pipelined at depth 64: Churust 3.10M, actix-web 3.04M, Ktor 1.37M, axum 251k, Go net/http 98k](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-throughput.svg)

| framework | req/s | vs. Churust | server CPU µs/req |
|---|---:|---:|---:|
| **Churust** | **3,101,706** | — | 1.87 |
| actix-web 4.14 | 3,038,968 | 0.98× | **1.03** |
| Ktor 3.5 (Netty, JDK 21) | 1,366,326 | 0.44× | 7.23 |
| axum 0.8 | 251,405 | 0.08× | 40.51 |
| Go 1.26 `net/http` | 97,619 | 0.03× | 32.67 |

Apple M2 Max, 12 cores. Three routes — `/plaintext`, `/json`, `/user/{id}` —
returning constants. HTTP/1.1 pipelined at depth 64, median of three interleaved
rounds. Full method and every caveat:
[`benchmarks/README.md`](https://github.com/davthecoder/Churust/blob/main/benchmarks/README.md).

## Read this before quoting the table

**Churust is in actix-web's class, not ahead of it.** The margin changes hands
with pipeline depth:

![Churust and actix-web across pipeline depths 16, 64, 128 and 256](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-depth-sweep.svg)

| pipeline depth | Churust | actix-web | leader |
|---:|---:|---:|---|
| 16 | 772,040 | 795,389 | actix-web, +3% |
| 64 | 3,086,219 | 3,015,567 | **Churust, +2%** |
| 128 | 4,524,352 | 5,116,388 | actix-web, +13% |
| 256 | 5,030,548 | 5,677,437 | actix-web, +13% |

actix-web also gets there on half the CPU per request, which is headroom
Churust does not have. What is left of the gap is not in Churust's code: under
load the profile is 79% `kevent` — workers idle on I/O — and the CPU that *is*
spent goes to `memmove` in hyper's write buffering and to `sendto`/`recvfrom`.
Closing it would mean not using hyper's HTTP/1 write path.

**The other four figures move between runs**, and the ratios move with them.
axum measured between 251k and 357k across full runs; Ktor between 859k and
1.37M. Read the comparison as "Churust is roughly 8.7–12× axum, 2.3–3.6× Ktor and
~32× Go `net/http` *on this workload*", not as four significant figures.

**Most of the axum gap is one switch.** hyper answers each request in a
pipelined batch with its own flush — a batch of 64 costs 64 write syscalls.
Churust can now aggregate them (`pipeline_flush`); `axum::serve` exposes no way
to. That is a real difference in what the two frameworks let you ask for, and it
is not a difference in routing or extraction.

**Under load a browser would generate, all five are the same.** Plain
keep-alive on this machine puts every server between 45k and 52k req/s, because
the loopback path saturates there — two *different* servers measured at the same
time get 22.4k each. On that workload the framework is not the bottleneck and
this page cannot rank anything.

## Getting the throughput in your own application

The measured configuration is not the default, on purpose. Three knobs:

### `App::run_sharded` — one runtime per core

The default [`App::start`] runs every connection on one shared work-stealing
runtime. That balances perfectly, and it lets a connection's read wakeup, its
handler and its write each land on a different thread — an atomic, a cache miss
and usually a syscall per hop. `run_sharded` gives each worker its own
single-threaded runtime and pins a connection to one for its life.

```rust,no_run
# use churust::prelude::*;
fn main() -> std::io::Result<()> {
    let app = Churust::server()
        .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
        .build();
    app.run_sharded(0) // 0 = one worker per core
}
```

Note there is no `#[churust::main]`: `run_sharded` builds and owns its runtimes,
and wrapping it in another one leaves a multi-threaded runtime idling underneath
the single-threaded ones doing the work.

The trade is real, which is why it is not the default: with per-connection
affinity, a worker whose connections are busy cannot borrow an idle worker's
core. Choose it for many short, uniform requests; keep `start` for uneven or
long-running work.

### `pipeline_flush` — only if your clients pipeline

```rust,no_run
# use churust::prelude::*;
# let _ =
Churust::server().pipeline_flush(true)
# ;
```

Answers a batch of pipelined requests with one flush instead of one per
response. Off by default because a client that does *not* pipeline then waits
for a flush that only comes once the connection has nothing left to read:
measured on loopback with one request in flight, a median 90µs instead of 56µs.
Turn it on only if you know the traffic pipelines.

### `tcp_nodelay` — on by default, leave it

Nagle's algorithm holds a small write waiting for more to send while the peer's
delayed ACK holds the acknowledgement waiting for a response; the standoff
breaks on a timer. Churust disables it on every accepted connection. The only
workload that wants it back is pipelining, and `pipeline_flush` gets the same
coalescing without giving up the latency guarantee.

## Where the 8.8× came from

Churust served 353,000 req/s on this benchmark before 0.3.4 — level with axum,
which is where a framework sharing hyper and tokio with axum belongs.

![Churust before and after: 353k to 3.10M requests per second](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-before-after.svg)

None of it was in routing or extraction, which is where a profiler points first:

1. **Aggregating pipelined flushes** — 4.1×.
2. **Removing per-request atomics from shared cache lines** — 1.45×, then a
   further 1.29×. The request path cloned the application handle five times per
   request; each clone is an atomic read-modify-write on one cache line that
   every core is also writing to. Twelve workers sharing one application reached
   1.29M req/s where twelve *processes* not sharing it reached 2.07M — the same
   code on the same cores, differing only in whether one integer was contended.
3. **One runtime per core with connections pinned** — 1.4×.
4. **Dropping a per-request allocation** in the extension map — 1.29×.

## Running it yourself

```sh
cargo install oha --locked
brew install wrk gradle          # or your platform's equivalent
cd benchmarks && ./run.sh
```

`APPS="churust axum" ./run.sh` runs only the two that need nothing but cargo.
The harness refuses to measure unless all the apps return byte-identical
responses on every route, so a result you get is a result about dispatch and
not about two servers doing different work.

[`App::start`]: https://docs.rs/churust-core/latest/churust_core/struct.App.html#method.start
