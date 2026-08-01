# Performance

Churust is fast, and this page is about being precise rather than loud: what was
measured, against what, on what, and what the number does not cover.

![Requests per second under keep-alive: Churust 691k, actix-web 654k, axum 395k, Ktor 303k, Go net/http 289k](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-throughput.svg)

| framework | req/s | vs. Churust | server CPU µs/req | p99 latency |
|---|---:|---:|---:|---:|
| **Churust** | **691,046** | — | 8.49 | 3.13 ms |
| actix-web 4.14 | 653,543 | 0.95× | **7.27** | **553 µs** |
| axum 0.8 | 394,652 | 0.57× | 11.74 | 9.14 ms |
| Ktor 3.5 (Netty, JDK 21) | 303,490 | 0.44× | 19.79 | 2.56 ms |
| Go 1.26 `net/http` | 288,846 | 0.42× | 13.64 | 5.69 ms |

Linux 6.12, server pinned to eight CPUs and the load generator to four, 64
keep-alive connections, one server running at a time, order rotated each round,
median of five. Three routes returning constants. Full method:
[`benchmarks/README.md`](https://github.com/davthecoder/Churust/blob/main/benchmarks/README.md).

## Read this before quoting the table

**Churust is first on throughput and third on latency.** For most services the
second of those matters more. actix-web answers its slowest one-in-a-hundred
request in 553 µs where Churust takes 3.13 ms, and spends 17% less CPU per
request doing it.

![99th-percentile latency: actix-web 0.553ms, Ktor 2.56ms, Churust 3.13ms, Go 5.69ms, axum 9.14ms](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-p99-latency.svg)

**That tail is the price of `run_sharded`, and it is a choice you make.** The
same code on the same cores, with and without connection pinning:

| build | req/s | server CPU µs/req | p99 latency |
|---|---:|---:|---:|
| shared runtime (`App::start`, the default) | 393,165 | 12.82 | **379 µs** |
| one runtime per core (`App::run_sharded`) | **691,046** | 8.49 | 3.13 ms |

1.76× the throughput for 8.3× the tail. Pinning a connection to one runtime for
its life means a request waits for *that* worker rather than being picked up by
whichever is idle.

**With pipelining the order changes.** actix-web reaches 5.67M requests a second
against Churust's 4.00M — 1.42× — and does it on 1.11 µs of CPU per request
against 1.74. Pipelining is not the shape most traffic has, which is why it is
not the headline, but it is where Churust's dispatch path is furthest behind.

![Pipelined throughput: actix-web 5.67M, Churust 4.00M, Ktor 1.14M, Go 377k, axum 25k](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-pipelined.svg)

axum's last place there is not about its routing — the keep-alive table shows
that is fine. hyper answers each request in a pipelined batch with its own
flush, and `axum::serve` exposes no way to turn the aggregation on, so a batch
of 16 costs it 16 write syscalls. Churust can be asked (`pipeline_flush`).

**Numbers move between runs, so read the ranges.** The published figures are
medians of five rounds with the running order rotated; the results file carries
the spread for every row, and any row whose rounds differed by more than 3× is
flagged rather than averaged into respectability.

**Every route returns a constant.** This measures dispatch overhead and says
nothing about an application that talks to a database.

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

The trade is measured, not hypothetical: **1.76× the throughput, 8.3× the
99th-percentile latency** (379 µs → 3.13 ms on the comparison harness). With
per-connection affinity a worker whose connections are busy cannot borrow an
idle worker's core, so a request that lands on a busy worker waits.

Choose `run_sharded` when throughput is the constraint and the requests are many,
short and uniform. Keep `start` — the default — when the slowest percentile is a
number anyone looks at.

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

## Where the speed-up came from

![Churust before and after: 393k to 691k requests per second](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-before-after.svg)

None of it was in routing or extraction, which is where a profiler points first:

1. **Removing per-request atomics from shared cache lines.** The request path
   cloned the application handle five times per request; each clone is an
   atomic read-modify-write on one cache line that every core is also writing
   to. Twelve workers sharing one application reached 1.29M req/s where twelve
   *processes* not sharing it reached 2.07M — the same code on the same cores,
   differing only in whether one integer was contended.
2. **One runtime per core with connections pinned** (`run_sharded`), with the
   latency cost documented above.
3. **Dropping a per-request allocation** in the extension map, plus the router,
   pipeline and response-body allocations listed in the changelog.
4. **`TCP_NODELAY` on by default**, which it should always have been.

On *pipelined* load the multiple is far larger — aggregating flushes is worth
4.1× on its own — but that is a workload most services do not have, and the
1.76× above is the one to plan around.

## Running it yourself

```sh
docker build -f benchmarks/Dockerfile -t churust-bench .   # from the repo root
docker run --rm churust-bench
```

Docker is the supported path because the kernel matters more than anything else
here: on macOS the loopback saturates around 45–50k round-trips a second, below
what any of these servers can answer, and every framework reports the same
number. The same Churust binary measures 47k there and 691k on Linux.

The harness refuses to measure unless all five apps return byte-identical
responses on every route, so a result you get is a result about dispatch and not
about servers doing different work. `docker run -e APPS="churust axum" …` runs a
subset.

[`App::start`]: https://docs.rs/churust-core/latest/churust_core/struct.App.html#method.start
