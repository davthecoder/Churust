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
| churust | 757,930 | — | 8.39 | 246 µs | 711,304–771,400 |
| actix-web | 644,067 | 0.85× | **7.91** | **240 µs** | 616,886–658,537 |
| axum | 464,042 | 0.61× | 8.82 | 292 µs | 457,051–480,883 |
| ktor | 315,978 | 0.42× | 20.03 | 1.72 ms | 297,110–325,366 |
| go | 315,160 | 0.42× | 12.88 | 2.49 ms | 298,633–319,477 |

**That table compares two defaults, not two frameworks — and reading it as the
latter was the biggest error in this file's history.**

Both servers default to one worker per core, so running both at eight looked
like the definition of a fair comparison. It is not. Swept against worker count,
the two peak in different places:

| workers | Churust | actix-web |
|---:|---|---|
| 4 | 778,814 · 5.11 µs · 138 µs | **914,498 · 4.21 µs · 107 µs** |
| 6 | **880,352 · 6.49 µs · 183 µs** | 793,302 · 6.02 µs · 210 µs |
| 8 (both defaults) | 757,930 · 8.39 µs · 246 µs | 644,067 · 7.91 µs · 240 µs |

Nine rounds each, ranges disjoint within a row.

**Tuned against tuned — actix-web at four workers, Churust at six — actix-web
leads all three: throughput 914,498 against 880,352, CPU 4.21 µs against 6.49,
and p99 107 µs against 183.** Churust's apparent throughput lead in the default
table exists because eight workers is near its optimum and far from actix-web's,
not because it is the faster server.

What Churust can claim on this workload: within 4% of actix-web on throughput,
1.9× axum, 2.8× Ktor and Go — while spending about half again as much CPU per
request as actix-web does.

**Worker count is the largest lever measured anywhere in this file** — larger
than every change made to Churust's own code put together. Churust moves 757,930
to 880,352 between eight workers and six. Any comparison that does not sweep it
is reporting the distance between two defaults.

### Why the worker sweep was added late

Every other confound in this comparison was found by asking "is each app doing
the same work" — the security headers, the h2c sniffing, the JVM warm-up, the
`pipeline_flush` setting. Worker count is the same class of variable and it went
unswept until the end, because "both at their default" reads as fair and is not
when the two defaults sit at different distances from their optima. The
resulting error ran in Churust's favour for most of this file's life.

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

**Where that CPU goes — corrected by a later measurement.** Churust's own
dispatch path — routing, extraction, the pipeline, response building, with the
wire app's configuration — was published as **393 ns** when driven in isolation
with no socket and no hyper underneath it, against a wire figure of 8.39 µs.
(672/688 ns is the *default-configuration* bench, `bare_200`. The wire shape
this sentence describes is `wire_200`, measured at 385 ns. See below.) This
section used to conclude from that pair that Churust's layer was 4.6% of a
request and "the other 95% is hyper and the kernel", and that the residue was
userspace work inside the two HTTP/1 implementations.

That conclusion was reached by subtraction and it does not survive being
measured. `benchmarks/bench-hyper` — hyper and tokio with no framework on top,
same routes, same responses, same shape — was added afterwards precisely because
nothing here had ever tested the assumption:

| keep-alive, 4 workers, 9 rounds | req/s | server CPU µs/req | p99 | spread |
|---|---:|---:|---:|---|
| actix-web | 902,907 | 4.20 | 111 µs | 888,549–923,627 |
| **bare hyper (the floor)** | 902,481 | **4.07** | 110 µs | 887,031–920,204 |
| Churust | 814,690 | 4.87 | 131 µs | 798,035–845,139 |

hyper serves a request for *less* CPU than actix-http does. It was never the
thing costing Churust the gap. Churust's layer costs **0.80 µs per request** over
the hyper it runs on, and that accounts for essentially all of the distance to
actix-web.

**What the profile bought, in one line of code.** With `perf` restricted to
user-space cycles — `cycles:u`, without which every stack begins in a kernel
frame that frame-pointer unwinding cannot walk, and the call graph comes back
empty — `__memcpy_generic` was 20.4% of Churust's user cycles, over half of it
attributed to the engine's own service closure. The cause was
`tokio::time::timeout`, which takes its future **by value**: that future is the
whole request, and it measures **2,616 bytes**. Every request copied all of it
into the wrapper. `std::pin::pin!` before the call hands over a `Pin<&mut _>`
instead:

| | before | after |
|---|---:|---:|
| `__memcpy_generic`, share of user cycles | 20.37% | 14.07% |
| server CPU µs/req | 5.10 | 4.87 |
| p99 | 139 µs | 131 µs |
| req/s (median of 9) | 781,872 | 814,690 |
| spread | 680,850–791,152 | 798,035–845,139 |

The two spreads do not overlap — the confirming run's worst round beat the old
best one — and a second nine-round run reproduced the median to within 0.3%.

A follow-up removed the *other* copy of the same future. `std::pin::pin!(x)`
takes `x` by value, so pinning a future that already exists in a local still
moves it once — the first fix removed the copy into `Timeout` and left the copy
into the pin. Building the future directly in the pinned slot removes both.
`__memcpy_generic` fell again, 14.07% to 12.55%, but **the end-to-end numbers
did not move**: 816,986 req/s at 4.87 µs, against 814,690 at 4.87 before it.
1.5% of user-space cycles is roughly 30 ns, which this harness cannot resolve.
It is kept because it deletes a local and a line rather than adding either, not
as a measured win, and it is not counted in any figure above.

The same fix applied one layer down, to the `catch_unwind` in
`App::process_call`, which also takes its future by value. It measured 14.07%
against 14.15%: nothing. The compiler already elides that move. Reverted.

**The per-request timer is not worth removing, and that was measured before
anything was built.** `__kernel_clock_gettime` sits at 7.3% of user-space cycles
against roughly 1% for the floor, and the obvious suspect was
`tokio::time::timeout`: it reads `Instant::now()` for a deadline and puts an
entry in the timer wheel on every request. The tempting fix is to poll the
request future once and only arm a timer if it is still pending, since a handler
that finishes without yielding never needs one.

Running the benchmark app with `request_timeout_ms(0)` — the timeout branch
skipped entirely, which is the ceiling for any such optimisation — gives 824,582
req/s at 4.81 µs against 816,986 at 4.87. About 1%, with a spread
(664,319–836,802) wide enough to doubt even that. So the clock reads are mostly
the runtime's own I/O driver, not the request timeout, and the change was never
written: it would have complicated the semantics of a security control, and
helped only handlers that complete without awaiting anything.

**Dropping `#[async_trait]` from the extractor traits measured nothing either,
and it was the last item on the reachable list.**

The prediction was 25–47 ns from a microbenchmark, on solid-looking reasoning:
`impl FromCall for Call` is literally `Ok(call)` behind a `Pin<Box<dyn Future>>`,
so every request heap-allocated to hand back a value it already held. The
conversion was done properly — the three trait methods redeclared as
`-> impl Future<Output = ..> + Send` (RPITIT, since `async fn` in a trait cannot
promise `Send`), 22 attributes removed across `extract.rs` and implementors in
six crates, `async-trait` dropped from the extractor path entirely. DX was
checked first and is unchanged: an impl may satisfy an RPITIT method with a
plain `async fn`, so implementors delete an attribute and an import and write
the same code.

Measured at four workers over nine rounds, normalised to the bare-hyper floor
because the whole run drifted:

| | Churust | above the floor |
|---|---:|---:|
| before | 816,986 req/s · 4.87 µs | +0.81 µs |
| after  | 801,915 req/s · 4.95 µs | +0.85 µs |
| after, re-measured on a verified-fresh image | 806,768 req/s · 4.93 µs | +0.83 µs |

Unchanged. The third row exists because the second could not be trusted: the
image was built with `docker build -q … >/dev/null 2>&1`, Docker's disk had
filled, and the command still reported success — so that run may have measured
the binary it was meant to be compared against. The re-measurement was taken
after pruning, against an image whose build output was read rather than
discarded, and it agrees. **Never build the benchmark image with its output
suppressed; a benchmark that silently measures the wrong binary is worse than
no benchmark, and this one nearly retired a change on a number that had not
tested it.** **Reverted** — the change was justified by
performance, the performance is not there, and it breaks every downstream
`#[async_trait] impl`. The work is real and the dependency removal is a genuine
good on its own; that is a deliberate decision about dependency hygiene and
modern Rust, not something to ship as the residue of a failed optimisation.

This matters for how the remaining estimates should be read. The reachable set
was put at 60–110 ns, and its highest-confidence item — the one with a
mechanism you could point at in the source — delivered zero at the wire. Treat
the rest of that list as unproven until each is measured the same way.

**Five of the seven things tried in this round measured nothing.** They are all
written down — the `max_headers` skip, the `catch_unwind` pin, the in-place
pin's absent throughput effect, and the timer ceiling above — because a page
that records only what worked makes the next person repeat the rest.

**The 393 ns reproduces after all — the retraction above was itself a
configuration mismatch, and this is the fourth number in this file to go wrong
the same way.**

There is now a benchmark case for each shape, so this can be read off instead
of triangulated:

| `cargo bench -p churust-core --bench dispatch`, Linux, `lto = true` | ns |
|---|---:|
| `bare_200` — the **default** configuration | 688 |
| **`wire_200` — the shape `bench-churust` serves** | **385** |
| originally published | 393 |

`wire_200` and the published figure agree to within 2%. The two cases measure
different applications:

- `benches/dispatch.rs:158` builds `Churust::server()` — the *default* builder,
  and `app.rs:213` defaults `security` to `Some(..)`, so a
  `SecurityHeadersMiddleware` is installed at `Phase::Setup`.
  `benchmarks/bench-churust/src/main.rs:43` calls `.without_security_headers()`.
  That layer alone measures 268–301 ns.
- The bench parses `"/hello".parse::<Uri>()` *inside* the measured closure; hyper
  hands the wire app a `Uri` already built.
- The bench returns `&'static str`, which goes through `Response::text` and a
  `String` allocation. The wire app returns `Response::bytes` from a
  `&'static`, which does not allocate.

The line above this section already said the 393 ns was measured "with the wire
app's configuration". It was right for the app it described, and comparing it to
a default-configuration bench was the error.

`wire_200`'s guard asserts the two cases differ the way they are meant to — it
must carry no security headers, `bare_200` must carry them. `wire_200` is 44%
cheaper *by design*, and a bench that is cheaper because it silently does less
is exactly what `check-bench-regression.py` cannot catch: it only fires on
regressions.

**So the 85% attribution published here was wrong too.** Corrected:

| | ns/req |
|---|---:|
| Churust's overhead above the bare-hyper floor | ~790 |
| of which: dispatch, wire-shaped (`wire_200`) | **385 (49%)** |
| of which: the engine above `App::process` | ~405 (51%) |

The engine half — `respond`'s pre-dispatch header checks, `Call` construction
from hyper's parts, the timeout wrapper, `EngineBody`, the connection guard —
**has never been profiled.** Every optimisation in this round landed there by
accident rather than by aim.

**And first place is not reachable through dispatch.** To match actix-web,
Churust must shed 790 − 120 = 670 ns. Wire dispatch is ~400 ns, so deleting the
dispatch layer *entirely* — routing, extraction, pipeline, all of it, at zero
cost — leaves 4.47 µs against actix's 4.20. The target was never inside the
budget being argued over.

Monomorphised dispatch, the change that would supposedly buy it, was prototyped
and measured rather than assumed: replacing `Arc<dyn Handler>` +
`Pin<Box<dyn Future>>` with static dispatch is worth **20 ns**, and it inlines
every handler's state machine into the request future — pushing the one thing
that has measurably worked this session (making that future smaller and moving
it less) in the wrong direction. actix-web pays *more* vtable dispatches and
*more* allocations per request than Churust and is still faster, which is the
existence proof that erasure is not where the money is.

The reachable set, discounted for the habit of microbenchmarks overstating
allocator wins, is **60–110 ns**: dropping `#[async_trait]` from the extractor
traits (25–40), router work — FxHash, inline segments, fusing the path scans
(35–55), and smaller items below the harness's resolution. That is ~1.7% on the
wire: 817k → ~831k against actix's 916k.

**The one large win is invisible to this benchmark.** The default configuration
— what every real deployment runs, and what `bench-churust` alone turns off —
pays 268–301 ns for the security-headers layer. On the plaintext TCP path
`apply_to` runs *twice*: once in the middleware (`security.rs:266`) and again in
the transport (`engine.rs:2169`), the second call reserving six slots on a map
that already holds them and forcing a grow and rehash to find nothing to do.
Fixing that helps every real user and moves no number in this file.

## What first place would actually require

The floor makes this computable rather than arguable, and the answer is worth
writing down so the investigation is not repeated.

**Bare hyper beats actix-web.** 4.17 µs against 4.24 in the six-app run. So a
framework built on hyper *can* be the fastest thing here — it is not blocked by
its HTTP implementation. It simply has to cost almost nothing:

```
actix-web                                  4.24 µs/req
bare hyper, the floor                      4.17
budget for an entire framework layer       ~0.07   (70 ns)

Churust's layer today                       0.75   (750 ns)
```

**First place needs a 10x reduction in everything Churust does.** Not in one
hot spot — in the total.

Summing every Churust-attributable symbol from `profile.sh` with `cycles:u`:
malloc/free 7.68%, the engine's service closure 4.78%, `Next::run` 2.17%, the
connection loop 1.40%, the handler call 1.18%, the path scan 1.15%,
`Response::bytes` 1.14%, `process_call` 1.08%, SipHash 0.70%, and Churust's
share of memcpy ~5.9% — about **27% of user-space cycles**, which reconciles
with the 750 ns measured against the floor. Getting to 70 ns means deleting
roughly nine tenths of that.

Nothing available does that. The structural candidate — monomorphised dispatch,
replacing `Arc<dyn Handler>` and the per-layer boxed futures with static
dispatch — was prototyped and measured at **20 ns**, because erasure is not
where the cost is: actix-web pays more vtable dispatches and more allocations
per request than Churust does and is still faster. The API-preserving set
(dropping `#[async_trait]` from the extractor traits, FxHash, inline path
segments, fusing the path scans) totals 60–110 ns. Both together are ~2%.

So: **Churust is second of six on this workload and cannot be made first by
optimisation.** It is 1.8x axum, 2.6x Go and 2.7x Ktor, within 11% of
actix-web, and it spends 16% more CPU per request than actix does. Those are
the honest claims.

Two things this does *not* say. It does not say the remaining 750 ns is
uninteresting — the largest single defect found in this whole effort was 1.10 µs
on the default configuration, and it was found by profiling after the throughput
work had been declared finished. And it does not say Churust is second on
everything: it is the only server here that sends a security header set by
default, which is precisely why its default configuration is slower than its
benchmark configuration, and why `bench-churust` has a `SECURITY=1` toggle now.

**A caveat on the throughput ranking itself.** CPU/req × req/s gives cores
consumed: bare hyper 3.65, actix 3.85, Churust 3.98, all on four workers.
Churust's workers are pegged; the other two have 4–9% headroom, which means
their throughput may be bounded by the four-thread load generator rather than by
the server. The CPU-per-request figures are directly measured and do not depend
on saturation; the throughput ordering may partly be an artifact of the harness.
Re-running at `-t8 -c256` from a separate container would settle it.

Syscalls are not the difference. Under identical load, `strace -c` counted
174,147 syscalls for Churust against 172,871 for actix-web, with the same shape
— both batch about twelve requests per write.

**A negative result worth recording:** the engine calls
`http1().max_headers(cfg.max_headers)` with a default of 100, which is also
hyper's own `DEFAULT_MAX_HEADERS`. Setting it routes hyper's per-request header
scratch space through `smallvec!` instead of `smallvec_inline!`, which looked
like it should cost ~6.4KB of writes per request. Skipping the call when the
values agree measured **no change at all** (781,872 vs 775,771 req/s — inside the
spread), because at exactly the inline capacity `SmallVec::from_elem` stays
inline and the `MaybeUninit::uninit()` stores compile away. The change was
reverted rather than kept as a coupling to a dependency's private constant for a
gain that does not exist. The wire-level tests it prompted were kept:
`churust-core/tests/header_limit.rs` asserts the enforced limit over a socket,
which nothing did before.

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
