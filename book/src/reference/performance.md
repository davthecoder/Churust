# Performance

Churust is fast, and this page is about being precise rather than loud: what was
measured, against what, on what, and what the number does not cover.

![Requests per second under keep-alive: Churust 758k, actix-web 644k, axum 464k, Ktor 316k, Go net/http 315k](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-throughput.svg)

| framework | req/s | vs. Churust | server CPU µs/req | p99 latency |
|---|---:|---:|---:|---:|
| **Churust** | **757,930** | — | 8.39 | 246 µs |
| actix-web 4.14 | 644,067 | 0.85× | **7.91** | **240 µs** |
| axum 0.8 | 464,042 | 0.61× | 8.82 | 292 µs |
| Ktor 3.5 (Netty, JDK 21) | 315,978 | 0.42× | 20.03 | 1.72 ms |
| Go 1.26 `net/http` | 315,160 | 0.42× | 12.88 | 2.49 ms |

Linux 6.12, server pinned to eight CPUs and the load generator to four, 64
keep-alive connections, one server running at a time, order rotated each round,
median of five. Three routes returning constants. Full method:
[`benchmarks/README.md`](https://github.com/davthecoder/Churust/blob/main/benchmarks/README.md).

## Read this before quoting the table

**That table compares two defaults, not two frameworks.** Both servers default
to one worker per core, and on this machine that setting is near Churust's
optimum and well away from actix-web's. Swept properly:

| each at its own best (9 rounds) | req/s | CPU µs/req | p99 |
|---|---:|---:|---:|
| **actix-web**, 4 workers | **914,498** | **4.21** | **107 µs** |
| Churust, 6 workers | 880,352 | 6.49 | 183 µs |

**Tuned against tuned, actix-web leads throughput, CPU and tail latency.**
Churust is within 4% on throughput and spends about half again as much CPU to
get there. It remains comfortably ahead of axum, Ktor and Go on all three.

Worker count is the largest single lever in this whole page — larger than any
change made to Churust itself — and `run_sharded(0)`'s one-per-core default is a
starting point rather than an answer.

Nine rounds because fewer is not enough. Three consecutive five-round runs of
identical code put Churust 3.6% ahead, then 11% ahead, then 5.9% behind; the
between-run spread on this machine is wider than most of what is being
measured.

![99th-percentile latency: actix-web 0.240ms, Churust 0.246ms, axum 0.292ms, Ktor 1.72ms, Go 2.49ms](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-p99-latency.svg)

**That tail is the price of `run_sharded`, and it is a choice you make.** The
same code on the same cores, with and without connection pinning:

| build | req/s | server CPU µs/req | p99 latency |
|---|---:|---:|---:|
| shared runtime (`App::start`, the default) | 390,772 | 12.94 | 444 µs |
| one runtime per core (`App::run_sharded`) | **757,930** | 8.39 | **246 µs** |

1.94× the throughput, and better tail latency too once the per-connection
handoff and the connection loop's per-wake bookkeeping were removed. Pinning a connection to one runtime for
its life means a request waits for *that* worker rather than being picked up by
whichever is idle.

**With pipelining the order changes.** actix-web reaches 5.96M requests a second
against Churust's 4.45M — 1.34× — and does it on 1.14 µs of CPU per request
against 1.70. That gap is not in Churust's own code: profiled in isolation, its
whole dispatch layer costs 393 ns, so the other 1.31 µs is hyper and the kernel —
already more than actix-web's entire per-request budget. Pipelining is not the shape most traffic has, which is why it is
not the headline, but it is where Churust's dispatch path is furthest behind.

![Pipelined throughput: actix-web 5.96M, Churust 4.45M, Ktor 1.23M, Go 352k, axum 24k](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-pipelined.svg)

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

Measured against the shared runtime: **1.94× the throughput** (391k → 758k), and
tail latency improved as well (444 µs → 246 µs). With
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

![Churust before and after: 391k to 758k requests per second](https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-before-after.svg)

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
1.94× above is the one to plan around.

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
