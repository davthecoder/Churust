# Parity and hardening plan — measured against axum and actix-web

**Date:** 2026-07-25
**Baseline:** Churust `feature/0.3.0-dev` @ `47c6dc6`, 357 tests green.
**Compared against:** axum 0.8.x (tokio-rs/axum), actix-web 4.x (actix/actix-web),
tower-http 0.6.x, hyper 1.x.
**Status:** **implemented.** Every item in §7 is done, on this branch, each with
the acceptance test the section named. Test count 357 → 445.

Three items landed differently from the proposal, each for a reason found while
building; the deviations are recorded inline where they occur:

- **§3.1 needed no work** — the extractor split already existed. The plan was
  wrong; see the correction in that section.
- **§1.6 rejects a repeated scalar rather than taking the last value.** The
  proposal said last-wins "matching the ecosystem"; `serde_html_form` refuses
  the ambiguity instead, which is the same call §7 item 8 makes for
  `Transfer-Encoding` + `Content-Length` and is the safer of the two.
- **§1.8 leaves a trailing slash alone**, canonicalising only interior repeated
  slashes. Stripping it broke directory listings, whose relative links resolve
  against that slash — found by the existing `fs` tests, and the better
  argument won.

One defect was found *during* implementation rather than by the audit: an idle
HTTP/2 connection held shutdown for the entire grace period, because winding one
down means sending GOAWAY and waiting for a peer that never speaks. See the
`Fixed` section of the changelog.
**Supersedes:** nothing. Complements
[`2026-07-25-production-readiness-audit.md`](2026-07-25-production-readiness-audit.md),
which records work already done; this records work not yet done.

---

## 0. How to read this, and what the evidence is worth

Every item is tagged with how it was established. This matters more than usual
here, because a plan that mixes "I measured this failing" with "a blog post
said" produces a backlog nobody can prioritise.

| Tag | Meaning |
| --- | --- |
| **MEASURED** | Reproduced against this tree with a test written for this document. Exact commands and output are inline. Not opinion. |
| **SOURCED** | Verified against upstream source, docs, or an advisory, by adversarial 3-vote checking. Cited. |
| **UNASSESSED** | Named because the comparison raised it, but not established either way. Do not act without checking first. |

Three things this document is **not**:

- Not a demand for feature parity. Churust owns the ergonomic layer and leaves
  HTTP framing, TLS and scheduling to hyper, rustls and tokio. Several things
  the comparison surfaced are correctly absent, and §5 lists them.
- Not a list of everything the other frameworks have. It is a list of things
  where their behaviour is *better than ours in a way that is demonstrable*, or
  where their mistakes are worth not repeating.
- Not ordered by how interesting the work is. It is ordered by what breaks in
  production, which puts a five-line engine fix above the entire extractor
  redesign.

### The headline

Six defects were reproduced against this tree. Two of them mean a documented
Churust feature does not work at all. Neither is exotic — both are single-line
mistakes in `engine.rs` sitting behind config knobs that are tested for their
*values* and never for their *effects*. That is the real lesson of this
document: the test suite grew to 357 tests while two shipped guarantees were
inert, because nothing asserted the guarantee end to end over a socket.

---

## 1. Confirmed defects

Ordered by blast radius. Each has a reproduction, a root cause, a fix, and the
regression test that must exist before the fix is called done.

### 1.1 CRITICAL — graceful shutdown drains nothing

**MEASURED.** `serve()` returns **378µs** after the shutdown signal with a
request mid-flight. `shutdown_timeout_ms` is dead config: it has never delayed
an exit by a single millisecond.

Reproduction (a request that takes 800ms; shutdown fired 150ms in):

```
serve() returned Ok(()) after 378.125µs
```

**Root cause.** `churust-core/src/engine.rs:342`:

```rust
let _ = &watcher;                       // <- borrow, then drop
tokio::spawn(async move {
    let _ = builder.serve_connection_with_upgrades(io, svc).await;
});
```

`GracefulShutdown::shutdown()` waits for every `Watcher` to be dropped. This
one is borrowed to silence an unused-variable warning and then dropped at the
end of `serve_stream`, before the spawned connection has done anything. With
zero live watchers, `shutdown()` is `async {}`. The connection future is never
wrapped in `watcher.watch(...)`.

The comment above it is honest about *why* — hyper-util 0.1.x does not
implement `GracefulConnection` for the upgradeable connection type — but the
consequence was not followed through. In-process the requests still finish,
because the tokio tasks outlive `serve()`; in a real binary `serve().await`
returns, `main` returns, and the process tears down mid-response.

**Why the tests missed it.** The shutdown tests assert `serve()` *returns*.
Returning is exactly the bug.

**Fix.** Take the connection future through the watcher, and accept that
upgraded connections are excluded:

```rust
tokio::spawn(async move {
    // `watch` takes ownership of the watcher, which is what keeps
    // `GracefulShutdown::shutdown()` waiting on this connection.
    let conn = builder.serve_connection_with_upgrades(io, svc);
    let _ = watcher.watch(conn).await;
});
```

`Watcher::watch` requires `GracefulConnection`. If the upgradeable connection
still does not implement it in the pinned hyper-util, the fallback is a
`tokio::sync::mpsc` guard channel: hand each connection task a `Sender` clone,
drop the original in `serve`, and `recv().await` returning `None` means every
connection has finished. That is ~15 lines and depends on nothing upstream.

**Regression test (must fail before the fix):** issue a request to a handler
that sleeps 800ms, signal shutdown 150ms in, assert both that the client reads
a complete response *and* that `serve()` took ≥500ms to return. The second
assertion is the one that matters — without it the test passes today.

**Second test:** set `shutdown_timeout_ms(200)` against a 5s handler and assert
`serve()` returns in 200–400ms. That pins the timeout to an observable effect
rather than to a struct field.

---

### 1.2 HIGH — `keep_alive_ms` is used as a boolean; idle connections are unbounded

**MEASURED.** With `keep_alive_ms(300)`, a connection idle for 1200ms is still
open:

```
after 1200ms idle (keep_alive_ms=300): read -> Err(Elapsed(()))
VERDICT: connection still open — keep_alive_ms duration ignored
```

**Root cause.** `engine.rs:321`:

```rust
builder.http1().keep_alive(cfg.keep_alive_ms > 0);
```

hyper's `http1::Builder::keep_alive` takes a `bool` — whether to reuse the
connection at all. There is no idle-timeout knob on it. So `keep_alive_ms` has
exactly two distinct behaviours, `0` and non-zero, while its name, its type
(`u64` milliseconds), its builder method and its `CHURUST_KEEP_ALIVE_MS`
environment variable all promise a duration. Setting `5000` and `86_400_000`
are the same program.

**Impact.** An idle keep-alive connection is held until the client goes away.
Each costs a file descriptor and a task. This is the cheapest possible way to
exhaust a Churust server: open connections, send one request each, go quiet.
`backlog` does not help — these are accepted, not pending.

**Fix.** hyper will not do this for us, so the timeout has to wrap the
connection. Race the connection future against an idle deadline that the
service resets on each request:

```rust
// Shared between the service and the connection task. The service bumps it
// on every request; the watchdog closes the connection when it goes stale.
let last_activity = Arc::new(AtomicU64::new(now_millis()));
```

with the connection task selecting over `conn` and a `tokio::time::sleep_until`
that is re-armed whenever `last_activity` moves. On expiry, call
`Connection::graceful_shutdown` (pinned) rather than dropping — dropping mid
response truncates it.

**Naming.** Whatever ships, the knob and the behaviour must agree. If the idle
timeout is not implemented in this release, rename to `keep_alive: bool` and
delete `keep_alive_ms` with a deprecation, because a duration knob that ignores
its duration is worse than no knob.

**Regression test:** `keep_alive_ms(300)`, one request, then assert the socket
reads `Ok(0)` (peer closed) within ~1s. Plus the inverse: with a long
keep-alive, a second request on the same socket succeeds — so the fix cannot be
"close everything immediately".

---

### 1.3 HIGH — no connection cap, no TLS handshake timeout, and a hot accept loop

**MEASURED** by inspection of `engine.rs:113-147`; three distinct problems in
one loop.

**(a) Unbounded accepted connections.** Nothing limits how many connections are
live. actix-web caps this by default and caps in-flight TLS handshakes
separately, precisely because a handshake is CPU-expensive and asymmetric —
cheap for the attacker, expensive for the server. Churust has neither.
(The *existence* of Churust's gap is measured; actix's specific default values
are **UNASSESSED** — this research pass did not verify them, so check
`HttpServer::max_connections` / `max_connection_rate` in actix's docs before
quoting numbers or copying them as our defaults.)

**(b) TLS handshakes have no timeout.** `engine.rs:131-137` spawns
`acceptor.accept(stream).await` with no deadline. `header_read_timeout` cannot
help: no TLS session, no HTTP layer, no timer. A client that completes a TCP
handshake and then sends one TLS byte per minute holds a task and an fd
indefinitely. This is slowloris with the defence bypassed.

**(c) Accept errors spin.** `Err(_) => continue` — on a persistent error
(`EMFILE` when the fd table is full, which is exactly the state an attacker is
driving toward) this becomes a busy loop that burns a core and prevents the
recovery it is spinning for.

**Fix.** All three in one place:

```rust
// (a) A permit per connection; the task holds it until the connection ends.
let conns = Arc::new(Semaphore::new(cfg.max_connections));
// (b) A second, much smaller permit set for handshakes only.
let handshakes = Arc::new(Semaphore::new(cfg.max_tls_handshakes));

// (c) Distinguish fatal from transient, and back off rather than spin.
Err(e) => {
    if is_fatal(&e) { return Err(e); }
    tracing::warn!(error = %e, "accept failed; backing off");
    tokio::time::sleep(Duration::from_millis(backoff.next())).await;
    continue;
}
```

and around the handshake:

```rust
let Ok(Ok(tls_stream)) =
    tokio::time::timeout(cfg.tls_handshake_timeout, acceptor.accept(stream)).await
else {
    tracing::debug!(%peer, "TLS handshake failed or timed out");
    return;
};
```

Defaults: `max_connections` 25_000, `max_tls_handshakes` 256,
`tls_handshake_timeout` 10s. Exposed through the existing layered config so
`CHURUST_MAX_CONNECTIONS` works like every other knob.

**Note the shape of the backoff:** capped exponential, reset on success. An
uncapped one turns a transient blip into a minute of unavailability.

---

### 1.4 MEDIUM — a failed TLS handshake is silent

**MEASURED** at `engine.rs:133`, comment included:

```rust
// A failed TLS handshake silently drops the connection.
if let Ok(tls_stream) = acceptor.accept(stream).await {
```

Certificate problems, protocol-version mismatches and SNI failures are the
single most common "the server is up but I cannot connect" class, and Churust
produces no signal for any of them. The operator sees a closed connection and
nothing in the logs.

**Fix.** `tracing::debug!` per failure with peer and error — debug, not warn,
because internet-facing servers see scanner noise constantly and a `warn` per
dropped probe is its own denial of service on the log pipeline. Add a counter
if metrics land later. Folded into 1.3's rewrite.

---

### 1.5 MEDIUM — `405 Allow` and `OPTIONS Allow` disagree on the same path

**MEASURED**, raw wire, one route registered (`GET /`):

```
OPTIONS /  ->  204 No Content       allow: GET, HEAD, OPTIONS
DELETE  /  ->  405 Method Not Allowed  allow: GET
```

Both headers describe the same resource; they must list the same methods. RFC
9110 §15.5.6 requires the `405` response to generate an `Allow` field
containing the *supported* methods — and this server demonstrably supports
`HEAD` and `OPTIONS` on that path, as its own `OPTIONS` response says.

**Root cause.** `Match::MethodNotAllowed { allow }` is populated from
`BoxHandlers::methods_matching`, which reports explicitly registered methods.
The `OPTIONS` path separately adds the implicit `HEAD` and `OPTIONS`. Two code
paths, one truth.

**Fix.** One function, used by both:

```rust
/// Every method this path answers, implicit ones included. The single source
/// of truth for `Allow`, whether it is generated for `405` or for `OPTIONS`.
fn allowed_methods(&self, call: &Call) -> Vec<Method> {
    let mut m = self.methods_matching(call);
    if m.contains(&Method::GET) && !m.contains(&Method::HEAD) {
        m.push(Method::HEAD);       // a GET route answers HEAD
    }
    if !m.is_empty() {
        m.push(Method::OPTIONS);    // ...and we synthesize OPTIONS
    }
    m.sort_by_key(|x| x.as_str().to_owned());   // deterministic for tests
    m
}
```

**Regression test:** for a path with any set of registered methods, assert the
`Allow` on a `405` is byte-identical to the `Allow` on an `OPTIONS`. Property
style, over several route shapes — the bug is precisely that two paths drift.

---

### 1.6 MEDIUM — `Query<T>` and `Form<T>` reject repeated parameters

**MEASURED.** This is the one with the widest user-facing blast radius, because
it breaks ordinary HTML.

```
/multi?tag=a&tag=b&tag=c   -> 400  invalid type: string "a", expected a sequence
/multi?tag[]=a&tag[]=b     -> 400  missing field `tag`
/one?q=a&q=b               -> 400  duplicate field `q`
```

and, worse, the same for form bodies — this is a checkbox group, the most
common multi-value control on the web:

```
POST opt=email&opt=sms  into  Form<{ opt: Vec<String> }>
  -> 400 invalid form body: invalid type: string "email", expected a sequence
```

**Root cause.** Both extractors use `serde_urlencoded`, which maps each key
once. `Vec<T>` fields are not supported by design; the crate's own docs say so.

**Impact.** `<input type="checkbox" name="opt">` repeated, `<select multiple>`,
and `?tag=a&tag=b` filter lists are all standard, and all produce a `400` that
blames the user. There is no workaround inside the extractor — the handler must
drop to `call.query_string()` and parse by hand, which is exactly the
boilerplate the extractor exists to remove.

**SOURCED context:** axum hit this too and the ecosystem answer is
`serde_html_form`, which supports repeated keys while remaining a drop-in for
the `serde_urlencoded` surface. (The research verifier refuted the specific
claim about how axum-extra packages it, so treat the *packaging* as unverified
— the underlying behavioural defect above is measured locally and is not in
doubt.)

**Fix.** Swap the parser in `Query` and `Form` to `serde_html_form`. It is one
dependency, pure Rust, and the change is source-compatible for every currently
working shape: scalar fields keep working, `Vec<T>` starts working, and
`?q=a&q=b` into a scalar takes the last value rather than erroring.

**Decide and document one thing:** what a repeated key means for a *scalar*
target. Options are last-wins (what browsers and most servers do), first-wins,
or reject. Recommend **last-wins**, matching the ecosystem, and say so in the
`Query` docs — silent ambiguity here is a request-smuggling-adjacent surface
when a proxy and an origin disagree about which value counts.

**Regression tests:** checkbox group into `Vec<String>`; `<select multiple>`
into `Vec<u32>`; repeated scalar resolving last-wins; empty `?tag=` into
`Vec<String>` yielding one empty string, not a parse error.

---

### 1.7 LOW — `OPTIONS *` returns 404

**MEASURED:**

```
OPTIONS * HTTP/1.1  ->  404 Not Found
```

RFC 9110 §9.3.7 defines the asterisk-form request-target as applying to the
server as a whole rather than to a resource. It is what a client uses to probe
server capabilities. `404` is the wrong answer — the server exists.

**Fix.** Intercept in the dispatcher before routing: if the method is `OPTIONS`
and the target is `*`, answer `204` with `Allow` listing every method the
router has registered anywhere. Cheap, and it removes a conformance foot-gun
that scanners and proxies notice.

---

### 1.8 POLICY GAP — path normalisation is implicit, lossy and unspecified

**MEASURED.** One handler at `/admin/secret`; four distinct URLs reach it:

```
/admin/secret          -> 200 SECRET
//admin/secret         -> 200 SECRET
/admin//secret         -> 200 SECRET
/admin/secret/         -> 200 SECRET
/./admin/secret        -> 404
/admin/../admin/secret -> 404
/ADMIN/secret          -> 404
```

Empty segments are silently collapsed. Dot segments are not resolved. Neither
behaviour is written down anywhere, and the combination is the worst of both:
lenient enough to create URL aliases, strict enough that callers cannot rely on
normalisation.

**Why it matters.** Not a traversal bug — §2.1 shows traversal defence holds.
The risk is *aliasing*:

- Any middleware, guard, or reverse-proxy rule that keys on a literal path
  prefix (`path.starts_with("/admin")`) is bypassable with `//admin`. Churust's
  own route-scoped middleware is safe here, because it wraps the handler rather
  than matching a prefix at runtime — but user-written auth middleware is the
  common case, and it is not safe.
- Caches key on the URL. Four URLs for one resource is four cache entries and a
  cache-poisoning surface where a proxy and Churust disagree about identity.

**SOURCED:** axum is strict — `//admin/secret` is simply a different, unmatched
route (404) — and normalisation is an opt-in layer. actix-web ships
`NormalizePath` middleware that *redirects* (301) to the canonical form rather
than silently serving the alias. Both make the choice explicit. Churust makes
it silently, and does not document it.

**Fix — pick a policy and enforce it in one place.** Recommended:

```rust
pub enum PathPolicy {
    /// Reject aliases: `//a` and `/a/` are not `/a`. Fewest surprises,
    /// strongest cache identity. Recommended, and the default.
    Strict,
    /// 308-redirect an alias to its canonical form. Kind to hand-typed URLs.
    /// 308 rather than 301 so a POST stays a POST.
    Redirect,
    /// Today's behaviour: collapse silently. Available for compatibility,
    /// documented as creating URL aliases.
    Collapse,
}
```

Whichever ships, `%2F` must not be decoded into a path separator before
matching — an encoded slash is data inside one segment, and decoding it early
is how normalisation bugs turn into traversal bugs.

**This is a breaking change** under `Strict`. It belongs in a minor version
with a changelog entry that says exactly which URLs stop working, and it should
land with `Collapse` still selectable for one release.

---

## 2. Verified correct — do not "fix" these

Recorded because a plan listing only defects invites re-litigating settled
ground, and because each of these took a test to establish.

### 2.1 Static-file traversal defence holds

**MEASURED.** Eight encodings, all `404`, no leak:

```
/s/../secret.txt          /s/%2e%2e/secret.txt      /s/%2e%2e%2fsecret.txt
/s/..%2fsecret.txt        /s/....//secret.txt       /s/%252e%252e/secret.txt
/s/.%2e/secret.txt
```

Double-encoding and mixed-encoding included. Keep `tests/traversal.rs` as a
gate on any future path-handling change — especially §1.8's, which touches
exactly this code.

### 2.2 HEAD, 100-continue, chunked and header limits are right

**MEASURED** over a raw socket:

- `HEAD /` returns `content-length: 5` with no body — RFC 9110 §9.3.2 wants the
  headers a `GET` would produce. Correct, and the case young frameworks most
  often get wrong by dropping `Content-Length` with the body.
- `Expect: 100-continue` gets a `100 Continue` before the body. hyper does
  this; Churust inherits it.
- Chunked bodies with a trailer parse fine. Request trailers are not exposed to
  handlers — acceptable, worth one documentation line rather than code.
- **SOURCED:** hyper 1.x caps headers at 100 and synthesizes an RFC-compliant
  `431` itself; `header_read_timeout` defaults to 30s but is **inert without a
  Timer, and panics if configured without one**. Churust already wires
  `TokioTimer` (`engine.rs:327`), which is the non-obvious half. Note hyper
  keeps a *separate* HTTP/2 header knob — worth exposing alongside
  `max_headers`, which today configures HTTP/1 only.

### 2.3 Panic isolation is on by default, and that is a deliberate divergence

**SOURCED.** actix-web does not catch handler panics in core — panic isolation
lives in `actix-web-lab` as opt-in middleware, and maintainers have rejected
default-on catching on the record. actix's actual behaviour on panic is: no
HTTP response, connection dropped, worker thread survives.

Churust's `catch_unwind` in `app.rs` is therefore a considered difference, not
an accident. Document it as such, and describe actix's behaviour accurately
rather than implying it crashes the server, because that claim is wrong and
readers will check.

---

## 3. Architecture gaps

Not bugs. Places where the other frameworks make a stronger guarantee, and what
adopting it costs.

### 3.1 Split the extractor trait so double-body-consumption cannot compile

> **CORRECTION (implementation pass).** This was already done. `handler.rs`
> has had the split since v1: `FromCallParts` for every leading argument,
> `FromCall` for the last, a `Marker` generic keeping the per-arity impls
> disjoint, the blanket `impl<T: FromCallParts> FromCall for T`, and an arity
> cap of eight. Verified by compiling `|_a: Payload, _b: Payload|`, which is a
> compile error, not a runtime one.
>
> The claim below that it "compiles and fails at runtime" was wrong — inferred
> from the research about axum without reading Churust's `handler.rs` closely
> enough. The section is kept for the two implementation notes, which are
> accurate and which the existing code already reflects. **No work required.**

**SOURCED — high confidence, verified at source level in axum's
`handler/mod.rs`.**

axum splits extraction into two traits and enforces the split in the `Handler`
impl: every argument except the last must be `FromRequestParts` (sees only
`http::request::Parts`, which structurally contains no body); the last argument
may be `FromRequest` and may consume the body. Extractors run strictly
left-to-right and the leftmost rejection short-circuits.

**Why Churust needs it.** The body is a one-shot stream. Today
`|a: Json<A>, b: Json<B>|` is a type error nowhere — it compiles and fails at
runtime, or worse, half-works because `Either` clones with a buffered body.
Encoding the rule in the type system converts a runtime mystery into a compile
error.

**Mapping onto Churust's current extractors:**

| Parts-only (`FromRequestParts`) | Body-consuming (`FromRequest`) |
| --- | --- |
| `Path<T>`, `Query<T>`, `Header<T, N>`, `State<T>`, `BearerToken`, `Session`, `PeerAddr`, `Call` metadata | `Json<T>`, `Form<T>`, `Bytes`, `String`, `Payload`, `Multipart`, `Either<A, B>` |

**Two implementation details that are not optional**, both learned from axum's
source rather than its docs:

1. A marker generic plus a blanket impl —
   `impl<S, T: FromRequestParts<S>> FromRequest<S, ViaParts> for T` — otherwise
   a handler taking *only* `Path<T>` (a parts extractor in last position) does
   not compile. Without the marker the two impls overlap and coherence fails.
2. An arity cap. axum stops at 16 arguments. The macro expansion is quadratic
   in argument count; pick a number and document it.

**Cost.** This is the largest change in this document — it touches
`handler.rs`, `extract.rs`, and every extractor. It is a breaking change for
anyone who implemented an extractor by hand. Schedule it alone, in its own
release, exactly as the HTTP/2 change was.

### 3.2 Handler-mismatch diagnostics are going to be terrible; plan for it now

**SOURCED.** axum's own docs concede Rust "gives poor error messages" for
tuple-generic handler traits, and that the resulting `E0277` "doesn't tell you
*why* your function doesn't implement `Handler`". Their mitigations:

- `#[diagnostic::on_unimplemented]` on the `Handler` trait — a custom note
  instead of a raw trait-bound dump.
- `#[diagnostic::do_not_recommend]` on blanket impls, so rustc stops suggesting
  the unhelpful one.
- `#[debug_handler]`, a proc-macro that re-checks the handler with concrete
  types and points at the offending argument.

Churust has `HandlerFnAdapter<Marker, H>` with exactly the same shape, so it
inherits exactly the same diagnostics problem, and §3.1 makes it worse by
adding a second trait to fail. `churust-macros` already exists, which is where
`#[churust::debug_handler]` belongs.

Do the two attributes first — they are annotations on existing traits, an
afternoon of work, and they cover the common case. The proc-macro is a
follow-up; note that axum's has open false-positive bugs, so treat it as a
diagnostic aid rather than a source of truth.

### 3.3 The default body limit is a security control, and `Payload` is a hole in it

**SOURCED.** axum applies a 2 MB default limit to `Bytes` and everything built
on it, configurable through a layer. The default exists specifically as
remediation for **RUSTSEC-2022-0055**, a filed DoS advisory. The important
detail is that the limit is *extractor-local*: it does not cover extractors
that read the body directly.

Churust's `max_body_bytes` is server-wide, which is stronger in one way. But
§1 of the audit moved bodies to streaming, and `Payload` hands the stream to
the handler. A handler that does `payload.collect().await` is allocating up to
`max_body_bytes`, and if an operator raised that limit for a legitimate upload
route, every `Payload` handler inherits the raised ceiling.

**Fix.** Two parts:

- `Payload` carries its own limit, defaulting to the route limit and settable
  per route. Collecting past it errors rather than allocating.
- The `Payload` docs state the invariant in one line: *the stream is bounded by
  the route limit; collecting it into memory is your allocation, not the
  framework's.*

### 3.4 Type-level limits beat runtime-keyed config

**SOURCED.** `actix-web-lab` encodes limits in the type — `Json<T, LIMIT>`,
`BodyLimit<T, LIMIT>` — explicitly because actix-web core's
`JsonConfig`/`PayloadConfig` approach resolves config by `TypeId` at runtime
and **silently falls back to a hardcoded default when registered on the wrong
scope**. A limit that silently is not applied is worse than no limit.

Churust's `RouteBuilder::max_body_bytes` is builder-attached rather than
`TypeId`-keyed, so the specific footgun does not apply. Worth adding as an
option for the ergonomics:

```rust
r.post("/avatar", |Json::<Avatar, { 1 << 20 }>(a)| async move { … });
```

Low priority. Recorded mainly as the reason **not** to add a
`JsonConfig`-style app-data mechanism later: it looks convenient and it fails
silently.

### 3.5 Scope-level state

**SOURCED, medium confidence (2-1).** actix-web's `Scope::app_data` pushes an
additional container; lookup walks the stack innermost-first, so same-type data
is shadowed by the nearest scope while different-type parent data stays
visible.

Churust's `StateMap` is flat and single-level. Route-scoped middleware
inserting into per-request extensions emulates it partially. Worth doing only
if a concrete need appears — the flat map is simpler and there is no
demonstrated pain. Listed so the absence is a decision.

### 3.6 `Option<Path<T>>`, not `OptionalPath<T>`

**SOURCED.** axum-extra deprecated `OptionalPath` and replaced it with
`OptionalFromRequest`/`OptionalFromRequestParts` traits so that `Option<Path<T>>`
is the supported spelling. A maintainer has publicly disowned the parallel-type
design.

Churust has not made this mistake yet. The cheap prevention is to add the
optional-extraction trait at the same time as §3.1's split, so nobody ever
proposes `OptionalQuery`.

---

## 4. Ecosystem interop — the strategic question

**SOURCED.** axum does not implement most of its middleware; it inherits it.
`tower-http` is written against the `http`/`http-body` crates and is portable
across hyper, tonic, warp and axum. axum's own docs say it "doesn't have its
own bespoke middleware system."

Churust *does* have a bespoke middleware system: `Middleware` + `Phase` +
`Next`. It is nicer to write against than `Service`/`Poll::Ready`, and that is
a real ergonomic win consistent with the Ktor inspiration. The cost is that
every middleware must be written by us or by our users.

**The proposal is not to replace the pipeline.** It is a one-way adapter:

```rust
/// Run any `tower::Service` as Churust middleware.
///
/// The adapter is deliberately one-directional: Churust middleware stays the
/// native way to write one, and this exists so an application does not have to
/// reimplement something that already exists as a `Layer`.
pub struct TowerLayer<L>(L);
```

**What that inherits**, assuming the adapter works (each individually
UNASSESSED — the research verifier declined to confirm the packaging claims, so
confirm the module list against tower-http's docs before committing): response
compression, request/response decompression, structured tracing, timeouts,
request-id propagation, header manipulation, request validation, and metrics.

**Judgement.** The adapter is worth building and belongs behind a feature flag.
Two cautions from the same research:

- **Do not import axum's `Infallible`-at-the-boundary contract.** axum requires
  every composed `Service` to have `Error = Infallible`, which makes "a response
  is always produced" a compiler guarantee — good — but taxes every fallible
  tower middleware with a `HandleErrorLayer`. Adding a `TimeoutLayer` to a route
  literally requires wrapping it. **Keep the invariant, skip the friction:** the
  adapter absorbs a `Service` error into a `500` through the existing
  `IntoError`, so the ergonomics stay Churust's and the guarantee stays intact.
- The `Poll::Ready` backpressure contract does not map onto `Middleware::run`.
  Document the adapter as *not* propagating backpressure rather than pretending.

**Response compression specifically is UNASSESSED.** It is currently a
deliberate Churust non-goal ("belongs behind a reverse proxy"). The research
refuted the framing that it is the largest gap, and produced nothing either
way. If the adapter lands, compression arrives for free and the non-goal can be
retired quietly — which is a good argument for doing the adapter before
arguing about compression.

---

## 5. Deliberately not taking

Unchanged from the production-readiness audit, plus what this comparison added.
Listed so absence reads as decision.

| | Why |
| --- | --- |
| A bespoke `Service`/`Layer` core | The `Middleware`/`Next` pipeline is the ergonomic point of the framework. §4 adapts inward; it does not convert. |
| `Infallible`-typed service boundary | Keep the guarantee, skip the `HandleErrorLayer` tax. See §4. |
| `TypeId`-keyed extractor config | Silently falls back to defaults on the wrong scope. See §3.4. |
| Parallel `Optional*` extractor types | Upstream deprecated theirs. See §3.6. |
| Catching panics as opt-in | Churust catches by default. Divergence, documented. See §2.3. |
| HTTP/3, actor integration, templating, OpenAPI, an HTTP client | Unchanged. Out of scope for a server-side ergonomic layer. |

---

## 6. Process practices worth copying

**SOURCED, one finding.** actix-web ships experimental extractors and
middleware in `actix-web-lab`, a crate that **explicitly will never reach 1.0**,
expects breaking changes on most `0.x` bumps, and graduates features into core
by deprecating the lab item so users migrate by dropping the suffix.

For Churust — seven crates already released in lockstep, and a §3.1 that is a
breaking redesign — a `churust-lab` crate is the mechanism that lets the
extractor split be tried in public without destabilising the core. It also
gives the multi-value `Query` fix somewhere to prove itself first.

**One caveat on the lockstep rule:** a lab crate that never reaches 1.0 cannot
share `workspace.package.version` with crates that will. Either exempt it
explicitly in `Cargo.toml` with a comment saying why, or accept that lab
versions jump with everything else — decide before creating it, not after the
first release that trips over it.

Research question 5 was otherwise **not answered**: nothing survived
verification about CI matrices, MSRV policy, semver tooling, fuzzing, or
h2spec-style conformance suites. §8 keeps it open rather than inventing an
answer.

---

## 7. Ordered plan

Sequenced by risk, not by interest. Each release is independently shippable and
each acceptance test must fail before its fix.

### 0.3.1 — the engine is lying (patch, urgent)

Everything here is a shipped guarantee that does not hold. No API changes.

1. **§1.1** graceful drain actually drains — *acceptance: `serve()` blocks ≥500ms
   with a request in flight; `shutdown_timeout_ms(200)` returns in 200–400ms.*
2. **§1.2** idle keep-alive timeout, or the knob is renamed to `bool` — *acceptance:
   `keep_alive_ms(300)` closes an idle socket within 1s; a long keep-alive still
   serves a second request on the same socket.*
3. **§1.3** connection cap, TLS handshake timeout, accept backoff — *acceptance:
   connection count plateaus at the cap; a stalled TLS client is dropped at the
   deadline; a simulated `EMFILE` does not spin the loop.*
4. **§1.4** log handshake failures at debug.

**Why first, and together:** 1, 2 and 3 are the same subsystem, and each is a
way for a Churust process to be killed rather than to shut down, or to be
exhausted by a client doing nothing clever.

### 0.4.0 — protocol correctness (minor)

5. **§1.5** one `Allow` generator — *acceptance: `405` and `OPTIONS` produce
   byte-identical `Allow` across several route shapes.*
6. **§1.7** `OPTIONS *` → `204` + server-wide `Allow`.
7. **§2.2** expose the HTTP/2 header knob next to `max_headers`.
8. **SOURCED, medium (2-1):** a conformance test for RFC 9112 §6.3 body-length
   precedence — a request carrying both `Content-Length` and `Transfer-Encoding`
   must be framed by `Transfer-Encoding`, and the connection closed afterward.
   hyper owns framing, so this is a **test, not an implementation**; it pins
   behaviour we depend on and would otherwise notice only after an upgrade
   changed it.

### 0.5.0 — the extractors users actually hit (minor, one breaking behaviour)

9. **§1.6** `serde_html_form` in `Query` and `Form`; last-wins for repeated
   scalars, documented — *acceptance: checkbox group, `<select multiple>`,
   repeated scalar, empty value.*
10. **§3.3** `Payload` carries its own limit.
11. **§1.8** `PathPolicy`, defaulting to `Strict`, with `Collapse` available for
    one release and a changelog entry naming the URLs that stop working —
    *acceptance: the §1.8 URL table, asserted explicitly per policy; the §2.1
    traversal suite still green under every policy.*

### 0.6.0 — the extractor split (major-ish, alone)

12. **§3.1** `FromRequestParts` / `FromRequest`, marker generic, blanket impl,
    documented arity cap.
13. **§3.6** optional-extraction trait so `Option<Path<T>>` works, shipped in
    the same release so parallel `Optional*` types never get proposed.
14. **§3.2** `#[diagnostic::on_unimplemented]` and `#[diagnostic::do_not_recommend]`
    — *before* the split lands, so the split's new failure modes are legible.

Alone, for the same reason HTTP/2 was: it can break every handler in the
workspace simultaneously.

### 0.7.0 — ecosystem (minor, feature-flagged)

15. **§4** the `tower::Service` adapter behind a feature flag, absorbing service
    errors into `IntoError`, documented as not propagating backpressure.
16. Revisit compression once the adapter exists and the trade is concrete.
17. **§6** `churust-lab`, with the versioning question settled first.

### Continuous

18. **§3.2** `#[churust::debug_handler]` in `churust-macros` when the split's
    error messages prove insufficient in practice.
19. **§3.5** scope-level state only if a real need appears.

---

## 8. Open questions

Honest gaps. Answering these changes the plan; guessing at them corrupts it.

1. **Advisory history.** Beyond RUSTSEC-2022-0055, what do axum's and
   actix-web's advisory records contain — and which HTTP/2 DoS classes (Rapid
   Reset, CONTINUATION flood) does Churust inherit through hyper and h2? Which
   need framework mitigation versus a dependency bump? Churust shipped HTTP/2 in
   `3c56665` and has never audited this.
2. **Is compression worth it, and at what layer?** Unassessed in both
   directions. §4 makes the question cheap to answer later.
3. **What do their CI, MSRV, semver-checking and conformance practices actually
   look like?** Unanswered. `cargo-semver-checks` and an h2spec run are the two
   most likely wins for a framework this young.
4. **Which remaining RFC behaviours does hyper already guarantee for us?** §2.2
   settled HEAD, 100-continue, chunked and header limits. Pipelining was probed
   and the result was inconclusive — one response in the first read, which is
   equally consistent with correct behaviour and with TCP segmentation. Needs a
   real test before any claim is made.
5. **What is the `Payload` limit's interaction with `Multipart`?** Multipart
   still buffers. If §3.3 lands, the two limits must not contradict.

---

## Appendix — reproducing the measurements

Every MEASURED claim came from a throwaway integration test written against
this tree at `47c6dc6` and deleted afterward. To re-derive:

- **§1.1 drain:** `engine::serve` on an ephemeral port, handler sleeping 800ms,
  shutdown fired 150ms after the request, timing `serve()`'s return.
- **§1.2 keep-alive:** `keep_alive_ms(300)`, one request, then `read()` with a
  500ms timeout after 1200ms idle.
- **§1.5, §1.7, §2.2:** raw `TcpStream`, hand-written request bytes, printed
  response head — the only way to see `Allow`, `100 Continue` and `HEAD`
  framing, since `TestClient` bypasses the wire.
- **§1.6 query/form:** `TestClient` against `Query<Vec<String>>` and
  `Form<Vec<String>>` targets.
- **§1.8 normalisation, §2.1 traversal:** `TestClient` over the URL tables as
  printed above.

These should not stay throwaway. Each acceptance test in §7 is the permanent
version of one of them, which is the actual remedy for how 357 tests coexisted
with §1.1 and §1.2.
