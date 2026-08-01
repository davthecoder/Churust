# Churust vs actix-web, axum, Ktor and Go — Linux, 2026-08-01

- kernel: `Linux 6.12.76` · 12 vCPU, Docker Desktop VM on an Apple M2 Max
- server pinned to CPUs `0-7`, load generator to `8-11`, one server alive at a time
- load: `wrk -t4 -c64 -d10s`, 64 keep-alive connections
- keep-alive: median of **5** rounds · pipelined: median of **3** rounds
- churust `0.3.4-dev` · actix-web `4.14.0` · axum `0.8.9` · Ktor `3.5.2` (Netty,
  JDK 21) · Go `1.26.4` (`net/http`)
- reproduce: `docker build -f benchmarks/Dockerfile -t churust-bench . && docker run --rm churust-bench`

This supersedes `2026-08-01-Davids-MBP.md`, which was run on macOS. That file's
keep-alive table was measuring the host: macOS's loopback saturates around
45–50k round-trips a second, below what any of these servers can answer, so all
five reported the same number. The same Churust binary that reported **47k**
there reports **691k** here. Nothing about Churust changed; the kernel did.

## Keep-alive, no pipelining — the realistic shape

![Requests per second by framework](../../docs/assets/benchmark-throughput.svg)

| framework | req/s | vs. Churust | server CPU µs/req | p99 latency | spread across 5 rounds |
|---|---:|---:|---:|---:|---|
| **churust** | **691,046** | — | 8.49 | 3.13 ms | 487,601–704,585 |
| actix-web | 653,543 | 0.95× | **7.27** | **553 µs** | 644,325–661,592 |
| axum | 394,652 | 0.57× | 11.74 | 9.14 ms | 390,170–466,434 |
| ktor | 303,490 | 0.44× | 19.79 | 2.56 ms | 298,833–304,190 |
| go | 288,846 | 0.42× | 13.64 | 5.69 ms | 284,847–302,369 |

**Churust is first on throughput**: 1.06× actix-web, 1.75× axum, 2.28× Ktor,
2.39× Go's `net/http`. No round was flagged bad. Churust's low round is its
first of the run (487k against a steady 685–705k afterwards) — a cold start,
which is what taking a median is for.

**And it is third on latency, which matters more for most services.** actix-web
answers its 99th-percentile request in 553 µs where Churust takes 3.13 ms. A
service that cares about the slowest one request in a hundred should read the
latency column first and the throughput column second.

## Pipelined, depth 16 — dispatch headroom

![Requests per second with pipelining](../../docs/assets/benchmark-pipelined.svg)

| framework | req/s | vs. best | server CPU µs/req | p99 latency |
|---|---:|---:|---:|---:|
| actix-web | 5,669,946 | 1.00× | 1.11 | 0.89 ms |
| churust | 3,996,416 | 0.70× | 1.74 | — |
| ktor | 1,137,799 | 0.20× | 6.26 | 4.07 ms |
| go | 377,301 | 0.07× | 9.22 | 14.31 ms |
| axum | 24,542 | 0.004× | 6.54 | 42.54 ms |

actix-web wins this mode by 1.42×, and its spread across rounds was under 2%.
Pipelining is not what most traffic looks like; it is included because it
removes the network from the measurement almost entirely and shows what the
dispatch path can do.

axum is last by two orders of magnitude because it cannot aggregate pipelined
writes — hyper has the switch, `axum::serve` exposes no way to reach it — so a
batch of 16 requests costs it 16 write syscalls. That is a property of the
serving API, not of axum's routing, which the keep-alive table shows is fine.

## Server CPU per request

![Server CPU per request](../../docs/assets/benchmark-cpu-per-request.svg)

Churust buys its throughput lead with more CPU than actix-web spends: 8.49 µs
against 7.27, 17% more per request. Against the JVM and Go the gap runs the
other way — Ktor spends 19.79 µs and Go 13.64.

## Tail latency

![99th-percentile latency](../../docs/assets/benchmark-p99-latency.svg)

This is Churust's weakest result and it has a specific cause, measured below.

## Before and after, on the same kernel

![Churust before and after](../../docs/assets/benchmark-before-after.svg)

The same harness, the same pinned cores, the same warm-up — only the binary
differs. "Before" is `3daaddf`, the commit before the performance work, built
for Linux and substituted into the benchmark image.

| build | req/s | server CPU µs/req | p99 latency |
|---|---:|---:|---:|
| before (`3daaddf`, shared runtime) | 393,165 | 12.82 | **379 µs** |
| after (`App::run_sharded`) | **691,046** | 8.49 | 3.13 ms |

**1.76× the throughput for 8.3× the tail latency.** That is the trade
`run_sharded` makes, stated as a measurement rather than as a caveat: pinning a
connection to one runtime for its life means a request waits for *that* worker
instead of being picked up by whichever is idle. It is why `run_sharded` is
opt-in and `App::start` — the shared work-stealing runtime, which is what
"before" is running — remains the default.

An application choosing between them is choosing between those two rows. Many
short uniform requests where throughput is the constraint: `run_sharded`.
Anything where the slowest percentile is a user-visible number: `start`.

The earlier macOS file quoted 8.8×. That figure was for *pipelined* load, where
`pipeline_flush` alone is worth 4.1×; it was never the keep-alive number and
should not be read as one.

## Fairness notes

- **Churust sends five security headers by default and this build does not.**
  `bench-churust` calls `.without_security_headers()` because no other app here
  sends any; their cost is measured separately in
  `churust-core/benches/headers.rs`.
- **`pipeline_flush` is on for the pipelined pass and off for keep-alive**,
  because it is the right setting for one and the wrong setting for the other —
  it costs a non-pipelining client a median 90 µs instead of 56 µs. `run.sh`
  restarts Churust between the passes. actix-http aggregates unconditionally;
  axum cannot be asked to.
- **Ktor sends no `Date` header** where the other four do — one header of work
  it does not do. The gate strips `date` before comparing, so this passes.
- **Go is `net/http`**, not fasthttp, which would post a larger number while
  answering a different question.
- **The JVM is warmed for 25 seconds** before Ktor is measured; the others get 5.
- **This is a VM on a laptop.** Its vCPUs are timeshared by a host that is also
  running a desktop, so absolute numbers would be higher on dedicated hardware
  and tail latency is the figure most affected by that. Pinning, one-server-at-
  a-time, rotation and medians are what make the *comparison* survive it; they
  do not make the machine a server.
- **Every route returns a constant.** This measures dispatch overhead and says
  nothing about an application that talks to a database.

## What was wrong before this file

Three defects in the earlier method, each caught by the harness rather than by
inspection, and each corrected here:

1. **The host was the bottleneck.** Fixed by running on Linux.
2. **wrk's 2 s default timeout** scored the connection-open burst as errors,
   which silently depressed whichever server accepted them slowest. Raised to
   10 s, with p99 reported so that raising it does not hide real stalls.
3. **The load generator was starved.** An early pinned configuration gave wrk
   two CPUs and four threads; it thrashed, and every framework's number
   collapsed and scattered at once. One thread per client CPU now, and the
   harness flags any row whose rounds differ by more than 3×.
