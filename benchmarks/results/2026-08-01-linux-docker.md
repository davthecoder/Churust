# Churust vs actix-web, axum, Ktor and Go — Linux, 2026-08-01

- kernel: `Linux 6.12.76` · 12 vCPU, Docker Desktop VM on an Apple M2 Max
- server pinned to CPUs `0-7`, load generator to `8-11`, one server alive at a time
- load: `wrk -t4 -c64 -d10s`, 64 keep-alive connections
- keep-alive and pipelined: median of **9** rounds each (see "How many rounds is enough" in `../README.md` — five was not)
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
| **churust** | **757,930** | — | 8.39 | 246 µs | 711,304–771,400 |
| actix-web | 644,067 | 0.85× | **7.91** | **240 µs** | 616,886–658,537 |
| axum | 464,042 | 0.61× | 8.82 | 292 µs | 457,051–480,883 |
| ktor | 315,978 | 0.42× | 20.03 | 1.72 ms | 297,110–325,366 |
| go | 315,160 | 0.42× | 12.88 | 2.49 ms | 298,633–319,477 |

**Churust is first on throughput, and this one is solid**: 1.18× actix-web,
1.63× axum, 2.40× Ktor and Go. Across nine rounds Churust's slowest (711,304)
beat actix-web's fastest (658,537) — the ranges do not overlap at all.

**Tail latency is a tie, not a win.** 246 µs against actix-web's 240 µs, and the
two overlap round for round. Both clear axum's 292 µs; both are an order of
magnitude ahead of Ktor and Go. The ordering of the first two rows in that
column should not be read as a result.

**actix-web leads CPU per request**, 7.91 µs against 8.39 — Churust buys its
throughput with 6% more CPU.

### Why nine rounds

Because five was not enough, and looked like it was. Three consecutive
five-round runs of *identical code* reported Churust 3.6% ahead of actix-web,
then 11% ahead, then 5.9% behind. Any one of those, published alone, would have
read as a finding. The between-run spread on this machine is wider than most of
what is being measured, and the fix is more rounds and a look at whether the
ranges overlap — not a more confident sentence. This file's headline claim
clears that bar; its tail-latency ordering does not, and says so.

## Pipelined, depth 16 — dispatch headroom

![Requests per second with pipelining](../../docs/assets/benchmark-pipelined.svg)

| framework | req/s | vs. best | server CPU µs/req |
|---|---:|---:|---:|
| actix-web | 5,955,207 | 1.00× | 1.14 |
| churust | 4,449,755 | 0.75× | 1.70 |
| ktor | 1,227,741 | 0.21× | 6.04 |
| go | 352,427 | 0.06× | 9.35 |
| axum | 24,379 | 0.004× | 8.07 |

actix-web wins this mode by 1.34×.
Pipelining is not what most traffic looks like; it is included because it
removes the network from the measurement almost entirely and shows what the
dispatch path can do.

axum is last by two orders of magnitude because it cannot aggregate pipelined
writes — hyper has the switch, `axum::serve` exposes no way to reach it — so a
batch of 16 requests costs it 16 write syscalls. That is a property of the
serving API, not of axum's routing, which the keep-alive table shows is fine.

## Server CPU per request

![Server CPU per request](../../docs/assets/benchmark-cpu-per-request.svg)

Churust buys its throughput lead with more CPU than actix-web spends: 8.39 µs
against 7.91, 6% more per request. Against the JVM and Go the gap runs the
other way — Ktor spends 20.03 µs and Go 12.88.

**Where that CPU goes, measured rather than guessed.** Churust's own dispatch
path — routing, extraction, the pipeline, response building, with the wire
app's configuration — costs a few hundred nanoseconds when driven in isolation with
no socket and no hyper underneath it — **393 ns**. The wire figure is 8.39 µs. So the
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
| after (`App::run_sharded`) | **757,930** | 8.39 | **246 µs** |

**1.94× the throughput, and better tail latency with it.** An earlier version of
this engine did trade tail for throughput, and the trade was published here as
2.5×; the cause turned out to be the per-connection handoff rather than the
affinity, and removing it took p99 below the shared runtime's. The affinity
trade is still real in principle — a request landing on a busy worker waits for
that worker — so `run_sharded` stays opt-in and `App::start` remains the
default for uneven or long-running handlers.

An application choosing between them is choosing between those two rows. Many
short uniform requests where throughput is the constraint: `run_sharded`.
Anything where the slowest percentile is a user-visible number: `start`.

The earlier macOS file quoted 8.8×. That figure was for *pipelined* load, where
`pipeline_flush` alone is worth 4.1×; it was never the keep-alive number and
should not be read as one.

## What changed after the first Linux run

The sharded engine originally accepted on one thread and handed each socket to a
worker over a channel, because `SO_REUSEPORT` does not distribute on macOS. On
Linux it does, and the handoff was pure overhead — a channel per connection, a
socket re-registered with a second runtime, and an acceptor thread competing for
the workers' cores. Each worker now owns a `SO_REUSEPORT` listener and runs the
same accept loop the shared engine uses. That, plus removing three pieces of
per-wake bookkeeping from the connection loop — a shutdown watcher rebuilt on
every `select!` pass, a notification sent to a listener that only exists at
`keep_alive_ms == 0`, and a negotiation timer polled long after the protocol was
settled — took keep-alive from 390,772 to 757,930 and pipelined throughput up by
about 10%.

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
