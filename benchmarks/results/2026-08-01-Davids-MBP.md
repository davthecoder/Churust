# Churust vs actix-web, axum, Ktor and Go — macOS, 2026-08-01

> **Superseded by [`2026-08-01-linux-docker.md`](2026-08-01-linux-docker.md).**
> Kept because it documents a mistake worth not repeating, not because its
> ranking should be quoted.
>
> The keep-alive table below is a measurement of macOS. Its loopback path
> saturates around 45–50k round-trips a second — below what any of these
> servers can answer — so all five report the same number and the table ranks
> nothing. The same Churust binary that reports 47k here reports 691k on Linux.
>
> The pipelined tables are real measurements, but they were the workaround for
> the ceiling above rather than a workload anyone runs, and the depth at which
> Churust leads actix-web here does not survive the move to Linux. The 8.8×
> improvement figure is pipelined-only; on keep-alive load the honest figure is
> 1.76×.

- host: `Davids-MBP`, Apple M2 Max, 12 cores, 96 GB — `Darwin 25.5.0 arm64`
- rustc: `1.97.1 (8bab26f4f 2026-07-14)` · churust: `0.3.3` (this branch)
- actix-web: `4.14.0` · axum: `0.8.9`
- Ktor: `3.5.2` (Netty) on `OpenJDK 21.0.5 LTS` · Go: `1.26.4` (`net/http`)
- load: `wrk -t4 -c64 -d8s` at pipeline depth 64; `oha -c64 -z10s` unpipelined
- three rounds, apps interleaved within each round, median reported

Numbers from one machine at one moment. They are not a ranking of these
frameworks in general, they do not transfer to other hardware, and every route
here returns a constant. Read `benchmarks/README.md` before quoting any of it.

## Pipelined (depth 64, median of 3 rounds)

| framework | req/s | vs. best | server CPU µs/req |
|---|---:|---:|---:|
| **churust** | **3,101,706** | **1.00x** | 1.87 |
| actix-web | 3,038,968 | 0.98x | 1.03 |
| ktor | 1,366,326 | 0.44x | 7.23 |
| axum | 251,405 | 0.08x | 40.51 |
| go | 97,619 | 0.03x | 32.67 |

Churust started this branch at **353,000 req/s** on this measurement — level
with axum, which is where a framework sharing hyper and tokio with axum would be
expected to sit. It now answers 3.10M.

**The 2% margin over actix-web at this depth is real but small, and it does not
hold at every depth.** Repeating just those two for five rounds, alternating
which went first, Churust won all five: 3.08-3.12M against 2.99-3.09M. But
sweeping the pipeline depth shows the ranking changing hands:

| pipeline depth | churust | actix-web | winner |
|---:|---:|---:|---|
| 16 | 772,040 | 795,389 | actix-web, +3% |
| 64 | 3,086,219 | 3,015,567 | **churust, +2%** |
| 128 | 4,524,352 | 5,116,388 | actix-web, +13% |
| 256 | 5,030,548 | 5,677,437 | actix-web, +13% |

So the honest claim is not "Churust is the fastest server here". It is that
Churust went from a sixth of actix-web's throughput to within about ten percent
of it either way — ahead at one depth, behind at others — while pulling
decisively ahead of every other framework in the comparison. Note also that
actix-web reaches its number on **half the CPU per request** (1.03 µs against
1.87), which is headroom Churust does not have.

Depth 64 is `run.sh`'s default because at 16 every server is still pinned to the
kernel's packet rate (see below). That choice was made before any of these
numbers existed, and the sweep above is published so it cannot be mistaken for a
depth picked to flatter.

### Where the 8.8x came from

None of it was in the routing or the extractors a profiler points at first.
In order of size:

1. **`pipeline_flush`** — 4.1x on its own. hyper answers each request in a
   pipelined batch with its own flush, so a batch of 64 cost 64 write syscalls.
   hyper has always had the switch; Churust never set it. axum's row above is
   what this measurement looks like without it — `axum::serve` exposes no way to
   turn it on, and that, rather than anything about axum's routing, is most of
   why its number is what it is.
2. **Not sharing reference counts** — 1.45x, then a further 1.29x. The request
   path cloned the application handle five times per request, plus the
   middleware chain, the state map and the matched handler. Each is an atomic
   read-modify-write on one cache line that all twelve cores were writing to.
   Measured directly: twelve workers sharing one `AppInner` reached 1.29M where
   twelve *processes* not sharing it reached 2.07M — same code, same cores,
   differing only in whether one integer was contended.
3. **One runtime per core, connections pinned** (`App::run_sharded`) — 1.4x.
4. **Dropping the per-request `http::Extensions` allocation** — 1.29x. The
   engine's only insert on an ordinary request was the peer address; the map
   allocates on first insert, so that was a malloc and free per request to carry
   sixteen bytes the `Call` could hold in a field.

Under load the profile is now 79% `kevent` — workers idle, waiting on I/O — and
of the CPU that is spent, `memmove` in hyper's write buffering plus
`sendto`/`recvfrom` dominate. What is left is not Churust's own code.

## Keep-alive, no pipelining (median of 3 rounds)

| framework | req/s | vs. best | server CPU µs/req |
|---|---:|---:|---:|
| axum | 51,941 | 1.00x | 24.29 |
| actix-web | 47,833 | 0.92x | 26.00 |
| churust | 47,139 | 0.91x | 28.87 |
| go | 45,525 | 0.88x | 55.15 |
| ktor | 45,060 | 0.87x | 52.90 |

**This table ranks nothing, and the 13% spread across it is noise.** macOS's
loopback path saturates at roughly 45,000-50,000 small round-trips per second on
this machine, and all five servers are sitting on that ceiling. The evidence:

- one server at 16 / 64 / 256 / 512 connections: 44.9k, 45.4k, 45.7k, 46.0k.
  More concurrency does not move it.
- **two different servers, two load generators, two ports, at the same time:
  22.4k each.** They split one budget. The limit is in neither of them.
- during those runs the server process used under one core of twelve, while
  `kernel_task` used a full one.

This is why the pipelined table exists at all, and why the first version of this
harness — which reported 50,736 for Churust against 51,527 for axum and read as
a narrow loss — was measuring macOS rather than either framework. Ranking this
table on a different machine, with a real network between client and server, is
the measurement that would mean something, and it is not one this repository can
make.

The CPU column is the one thing this mode still says honestly, because it does
not depend on how fast the wire is: the three Rust servers spend 24-29 µs of CPU
per request where Go and Ktor spend 53-55. Under the pipelined load, where the
wire is not the constraint, the same measure separates the five cleanly — and
puts actix-web first:



## Fairness notes

Everything that could bias this, stated rather than buried:

- **Churust sends five security headers by default and this build does not.**
  `bench-churust` calls `.without_security_headers()`, because no other app here
  sends them and leaving them on would measure that difference instead of
  dispatch. Their cost is measured on its own in
  `churust-core/benches/headers.rs`.
- **`pipeline_flush` is off by default in Churust and on for the pipelined
  pass.** It is right for a pipelining client and wrong for everything else —
  measured on loopback with one request in flight at a time, it costs a median
  90µs instead of 56µs, 61% more latency on every response. So it is a
  per-application choice rather than a default, and `run.sh` restarts Churust
  without it for the keep-alive pass. actix-http aggregates
  unconditionally. axum cannot be told to, which costs it heavily here.
- **Ktor sends no `Date` header**, where the other four do — roughly 37 bytes
  and one formatting step per response that it does not pay. The gate strips
  `date` before comparing, so this passes; it is a small advantage to Ktor.
- **Go is `net/http`**, not fasthttp. fasthttp would post a larger number while
  answering a different question: it is not `net/http`-compatible, so most of
  the Go ecosystem cannot be used with it.
- **The JVM is warmed for 20 seconds** before Ktor is measured. Cold it reports
  roughly a tenth of the number above, which would be a measurement of the
  bytecode interpreter. Ktor is the noisiest row here even so — it ranged from
  859k to 1.37M across runs during this work.
- **axum and Go are the next noisiest**, for the opposite reason: neither
  aggregates pipelined writes, so both are syscall-bound and move with whatever
  else the machine is doing. axum measured 357k in one full run and 251k in
  another.
- **The load generator shares the machine with the server.** Both want all
  twelve cores. This compresses every number in the pipelined table, and
  compresses the fastest ones most.
- **Chrome was running throughout**, using about one core. Interleaving is what
  makes the comparison survive that; absolute numbers would be higher on an idle
  machine.
