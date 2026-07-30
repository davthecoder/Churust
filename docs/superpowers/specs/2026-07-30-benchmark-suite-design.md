# Benchmark suite

**Date:** 2026-07-30
**Status:** approved, not yet implemented

## Why

Churust has no benchmarks. Two concrete gaps follow from that.

The first is regressions. Release 0.3.3 changed code on the path every request
takes — `Host` validation now runs on every HTTP/1.1 request, an idle-close
notification replaced a timer, `IntoResponse for Error` walks its header list
twice in the worst case. Each was reviewed for correctness and none was measured.
There is no way to answer "did that cost anything" except by argument.

The second is that the project already makes performance claims it cannot
support. Release 0.3.2 shipped two changes justified purely on cost — security
headers "parse once at config time" instead of per response, and `CallLogging`
prepares its header value "before dispatch, avoiding a second string parse" — and
the source carries a dozen more comments of the same kind. They are probably
right. Nothing demonstrates it.

The README makes no speed claims and this work does not add any. The comparison
half exists to know where the project stands, not to market it.

## Shape

Two halves, deliberately separate, because they answer different questions and
only one of them produces a number worth trusting on shared hardware.

| | regression suite | comparison |
|---|---|---|
| question | did we get slower than we were | how do we compare to a peer |
| lives in | `churust-core/benches/` | `benchmarks/` (outside the workspace) |
| harness | Criterion | `oha` against a real socket |
| transport | none — in-process | HTTP/1.1 over TCP |
| runs | every PR, in CI | by hand, on a quiet machine |

## Regression suite

### Scope

`churust-core` only, and within it only code on the per-request path. That is
where a regression costs something, and it is where 0.3.3 landed.

Explicitly excluded: crates whose cost is dominated by I/O or by a third-party
engine (`churust-redis`, `churust-client`, `churust-templates`). A benchmark
there measures someone else's code and moves for reasons we cannot act on.

### Files

One file per concern rather than one per module, so each stays small enough to
read in full when a number moves.

- **`routing.rs`** — route-shape sensitivity: static hit, single param,
  wildcard, deep path, miss, and a backtracking case. Measured through
  `App::process` rather than against `Router` directly, because `RouteBuilder`'s
  constructor is `pub(crate)` and a benchmark is an external crate — it cannot
  populate a bare `Router`. Every number therefore carries the same constant
  dispatch overhead, so these show relative movement between route shapes, not
  routing's absolute cost. The app is built once outside the measured closure.
- **`dispatch.rs`** — `App::process` end to end: a bare `200`, the same with
  three middleware installed, one driving extractors, and a `404`. This is the
  same entry point `TestClient` uses, so it exercises the real pipeline without a
  socket.

  Note what this does *not* cover: the `Host` validation added in 0.3.3 lives in
  `engine.rs`, on the raw hyper request, and runs before `process_call` is
  reached. `App::process` is downstream of it, so no in-process benchmark can
  measure that check. It would need a bench that drives a real socket, which the
  comparison half does and this half deliberately does not.
- **`headers.rs`** — `Vary` merge, cookie render and parse, and
  `Error → Response` carrying one header versus three; the last is the 0.3.3
  change from `insert` to first-replaces-then-appends. Security headers are
  measured as `App::process` with them on versus `without_security_headers()`,
  since `SecurityHeaders::apply_to` is `pub(crate)`.
- **`extract.rs`** — `Path`, `Query` and `Json` decode, since an extractor runs
  per request per handler argument.

### What a benchmark can reach

A `benches/` target compiles as its own crate, so it sees only `churust-core`'s
public API. That rules out two things the suite would otherwise measure directly
— `RouteBuilder::new` and `SecurityHeaders::apply_to` are both `pub(crate)` — and
both are measured through `App::process` instead. Widening the public API to suit
a benchmark was considered and rejected: a pre-1.0 crate should not make a
permanent API commitment for a testing convenience.

### Harness

Criterion 0.8 with `async_tokio`, as a dev-dependency of `churust-core`, with
`harness = false` on each `[[bench]]`.

Chosen over iai-callgrind despite the latter's determinism: the dispatch benches
are async, which is where instruction counting is least natural, and Criterion's
`--save-baseline` / `--baseline` workflow is what a contributor already knows.

Each bench builds its `App` or `Router` once, outside the measured closure. What
is under test is serving a request, not assembling a server.

### CI

Full comparison, on every pull request:

1. Check out the merge base, run the suite, `--save-baseline base`.
2. Check out the head, run the suite, `--save-baseline pr`.
3. `critcmp base pr` and post the table as a PR comment.
4. Fail the job only when a benchmark regresses by more than **20%**.

The threshold is set by the runner, not by taste. GitHub-hosted runners are
shared, and swings of 20–30% between identical runs are ordinary. A tighter
threshold reports noise as though it were data, and a gate that cries wolf is one
people learn to ignore. Anything under 20% is for a human to look at in the
comment, not for CI to block on.

Sample size is reduced in CI (`--sample-size 20`) to keep the doubled run within
a few minutes. The reduced sample widens the confidence interval, which is
acceptable when the gate only fires at 20%.

This job is advisory infrastructure. It must never gate a release, and
`release.yml` does not call it.

## Comparison

### Targets

axum only.

It shares hyper and tokio with Churust, so a difference is attributable to
Churust's own layers rather than to a different runtime or HTTP core — which
makes the number diagnostic rather than merely a scoreboard. actix-web would
answer "what is the ceiling" and not "what should we change", and a Ktor baseline
would need a JVM in the harness and warmup semantics that cannot be made fair.

### Layout

```
benchmarks/
  bench-churust/     a minimal Churust app
  bench-axum/        the same routes in axum
  run.sh             equivalence check, then measurement
  results/           YYYY-MM-DD-<machine>.md, committed
```

`benchmarks/` is added to `[workspace] exclude`. Without that, axum enters
`Cargo.lock`, `cargo test --workspace`, and every CI job — a comparison harness
must not become a dependency of the library.

### Routes

Three, identical in both apps:

- `GET /plaintext` — a fixed string. Isolates dispatch overhead.
- `GET /json` — a small struct serialised per request. Adds encoding.
- `GET /user/{id}` — a path parameter parsed to `u64`. Adds routing and extraction.

### Method

`run.sh`:

1. Builds both apps `--release`.
2. Starts each in turn and **asserts the two return byte-identical bodies and
   status for every route.** If they differ, it stops. Two apps doing different
   work produce a number that means nothing, and this is the failure mode that
   makes framework comparisons untrustworthy.
3. Warms up, then measures with `oha` at a fixed connection count and duration.
4. Writes a report naming the machine, the OS, the toolchain, both crate
   versions, and the exact command.

`oha` because it is a single Rust binary with JSON output, installs with
`cargo install`, and needs no runtime of its own.

Results are committed. A benchmark number without the machine it came from is
not a result, and a directory of dated reports shows drift over time that a
single overwritten file would hide.

## Testing

Benchmarks assert nothing about performance — that is the point of them. What is
tested is that they keep working:

- CI runs the suite on every PR, so a bench cannot rot against an API change
  while nobody is looking. A bench that fails to compile, or panics, fails the
  job regardless of the 20% threshold — that threshold governs *regressions*, not
  breakage.
- The comparison's equivalence check is itself the test for that half: it fails
  loudly when the two apps stop doing the same work.
- The bench apps' routes are exercised by the equivalence check before any
  measurement, so a comparison can never silently measure a `404` path.

## Not doing

- **No perf gate on release.** `release.yml` is untouched.
- **No other frameworks.** axum only, for the reason above.
- **No HTTP/2 or HTTP/3 comparison yet.** h3 load tooling is immature enough that
  the result would describe the tool more than the server.
- **No benches for I/O-dominated crates.**
- **No historical tracking store.** `critcmp` against the merge base answers "did
  this PR change anything". Charting drift over months needs a stable machine to
  be worth reading, which is a separate decision.

## Open question deferred

Whether the 20% CI threshold is right can only be answered by watching the
suite's actual variance on the runners for a few weeks. It is deliberately loose
to start. Tightening it is a one-line change once there is evidence.
