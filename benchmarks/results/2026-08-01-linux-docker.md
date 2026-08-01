# Churust vs actix-web, axum, Ktor and Go — Linux, 2026-08-01

- kernel: `Linux 6.12.76` · 12 vCPU, Docker Desktop VM on an Apple M2 Max
- server pinned to CPUs `0-7`, load generator to `8-11`, one server alive at a time
- load: `wrk -t4 -c64 -d10s`, 64 keep-alive connections
- keep-alive and pipelined: median of **5** rounds each
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
| **churust** | **699,200** | — | 8.59 | 1.12 ms | 398,596–727,948 |
| actix-web | 675,053 | 0.97× | **7.34** | 412 µs | 380,122–680,507 |
| axum | 457,647 | 0.65× | 8.74 | **342 µs** | 339,400–466,992 |
| ktor | 307,924 | 0.44× | 19.79 | 1.95 ms | 270,857–314,216 |
| go | 306,546 | 0.44× | 13.19 | 2.55 ms | 300,400–312,912 |

**Churust is first on throughput**: 1.04× actix-web, 1.53× axum, 2.27× Ktor,
2.28× Go's `net/http`. No round was flagged bad; each framework's low round is
its first of the run, a cold start, which is what taking a median is for.

**And it is third on latency, which matters more for most services.** axum
answers its 99th-percentile request in 342 µs and actix-web in 412 µs, where
Churust takes 1.12 ms. A service that cares about the slowest one request in a
hundred should read the latency column first and the throughput column second.

## Pipelined, depth 16 — dispatch headroom

![Requests per second with pipelining](../../docs/assets/benchmark-pipelined.svg)

| framework | req/s | vs. best | server CPU µs/req |
|---|---:|---:|---:|
| actix-web | 5,803,188 | 1.00× | 1.14 |
| churust | 4,156,521 | 0.72× | 1.76 |
| ktor | 1,189,703 | 0.21× | 6.17 |
| go | 376,534 | 0.06× | 9.65 |
| axum | 24,667 | 0.004× | 6.70 |

actix-web wins this mode by 1.40×, and its spread across rounds was under 4%.
Pipelining is not what most traffic looks like; it is included because it
removes the network from the measurement almost entirely and shows what the
dispatch path can do.

axum is last by two orders of magnitude because it cannot aggregate pipelined
writes — hyper has the switch, `axum::serve` exposes no way to reach it — so a
batch of 16 requests costs it 16 write syscalls. That is a property of the
serving API, not of axum's routing, which the keep-alive table shows is fine.

## Server CPU per request

![Server CPU per request](../../docs/assets/benchmark-cpu-per-request.svg)

Churust buys its throughput lead with more CPU than actix-web spends: 8.59 µs
against 7.34, 17% more per request. Against the JVM and Go the gap runs the
other way — Ktor spends 19.79 µs and Go 13.19.

**Where that CPU goes, measured rather than guessed.** Churust's own dispatch
path — routing, extraction, the pipeline, response building, with the wire
app's configuration — costs **398 ns per request** when driven in isolation with
no socket and no hyper underneath it. The wire figure is 8.59 µs. So the
framework Churust actually is accounts for **4.6%** of what a request costs, and
the other 95% is hyper and the kernel. Deleting Churust's layer entirely would
close under a third of the gap to actix-web.

Syscalls are not the difference either. Under identical load, `strace -c`
counted 174,147 syscalls for Churust against 172,871 for actix-web, with the
same shape — both batch about twelve requests per write. The gap is userspace
work inside the two HTTP/1 implementations.

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
| before (`3daaddf`, shared runtime) | 390,772 | 12.94 | **444 µs** |
| after (`App::run_sharded`) | **699,200** | 8.59 | 1.12 ms |

**1.79× the throughput for 2.5× the tail latency.** That is the trade
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

## What was tried and did not work

**Skipping protocol detection.** actix-web's `HttpServer::bind` uses
actix-http's `.tcp()`, which hard-codes `Protocol::Http1`; h2c there needs the
opt-in `bind_auto_h2c`. Churust sniffs every plaintext connection for the
HTTP/2 preface and carries a rewind buffer under every read, so it was doing
strictly more work per byte. Adding an HTTP/1-only serving path to equalise
that made Churust **slower** — 458k against 627k on keep-alive, 2.62M against
4.14M pipelined — so hyper-util's detecting path is better optimised than
hyper's bare HTTP/1 builder for this shape, and the change was reverted rather
than kept as a knob nobody should turn.

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
4. **The p99 column was computed by sorting strings.** wrk prints latency with a
   unit attached, so `"1.16ms"` sorted before `"405.00us"` and the median of a
   set spanning two units was whichever value landed in the middle
   alphabetically. Every p99 this harness reported before the fix was wrong —
   including the ones published in the first version of this file, which
   overstated the `run_sharded` tail by a factor of three. Parsed to a number
   now, with a unit test's worth of care in the parser.
