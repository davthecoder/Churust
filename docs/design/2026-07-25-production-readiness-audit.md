# Production-readiness audit

**Date:** 2026-07-25
**Baseline:** Churust at `feature/0.3.0-dev`, 358 tests.
**Status:** §1 and §2 are implemented. What remains is §3 (deliberate
non-goals) and §4 (maturity, which no amount of code closes).
**History:** this superseded an earlier ten-finding audit, all of which was
closed. It is now itself a record of work done rather than work outstanding.

## How to read this

A gap here meant *a mature server framework has it and Churust does not* —
which is not the same as *Churust should have it*. Churust owns the ergonomic
layer and leaves HTTP parsing, TLS and scheduling to hyper, rustls and tokio.
§3 lists what stays absent on purpose.

Each entry below records the problem as found, what closed it, and anything the
fix deliberately did not cover.

## 1. Blocked real use — all closed

### 1.1 Request bodies were always buffered

**Was:** `engine.rs` read every request with
`Limited::new(body, max_body).collect().await`. No streaming request body. An
upload could not exceed memory, `Multipart` inherited that ceiling, and
worst-case memory was `max_body_bytes × concurrent uploads`.

**Closed by `9e2da82`.** `Call` holds either a buffered body or a stream. The
engine hands the stream through without collecting; `try_receive_bytes` collects
on demand for extractors that need a whole body, and `Payload` exposes the
stream for handlers that do not. A body that arrived buffered yields a
single-chunk stream, so a handler never has to know which it got.

Two type-level obstacles were in the way and are worth remembering:

- `Call` must stay `Sync` for the handler bounds, but a boxed stream is `Send`
  only. The stream sits behind a mutex, uncontended because it is taken once.
- `HandlerFnAdapter` held `PhantomData<Marker>`, and the marker embeds the
  argument types — so one `Payload` argument would have made every handler
  taking one non-`Sync`. Now `PhantomData<fn() -> Marker>`.

Verified over a socket: 512 KiB written in 64 KiB pieces arrives in many chunks.

### 1.2 No peer address

**Was:** nothing exposed the client's address, which ruled out per-IP rate
limiting, audit logging, IP allowlists, and any sane handling of
`X-Forwarded-For`.

**Closed by `18032b8`.** `Call::peer_addr`, seeded on both the ws and non-ws
paths. It is the *socket* peer: behind a reverse proxy that is the proxy, and
`X-Forwarded-For` is only trustworthy after checking this against addresses you
actually trust.

### 1.3 Server tuning was not configurable

**Was:** no control over keep-alive, listen backlog, or a shutdown grace period.
Invisible in development; immediate when sizing a deployment.

**Closed by `18032b8`.** `keep_alive_ms`, `backlog` and `shutdown_timeout_ms`
through the layered config, so `CHURUST_*` works. Two things fixed alongside,
because they were adjacent and wrong:

- Draining was unbounded, so one slow request delayed exit forever — under an
  orchestrator that means being killed rather than shutting down cleanly.
- Binding moved to `TcpSocket`, which the backlog needed anyway, and sets
  `SO_REUSEADDR` — removing the most common cause of a failed restart.

Worker-thread count is left to the tokio runtime that `#[churust::main]` builds;
configure it there rather than duplicating the knob.

## 2. Real, with workarounds — all closed

### 2.1 `Path<T>` read one parameter, positionally

**Was:** `Path::<u64>` took the first capture only, so a route with several
needed `call.param("name")` for each — the advertised ergonomic path stopped
working exactly when a route got interesting.

**Closed by `8fa5db3`.** `Path<u64>`, `Path<(u64, String)>` and `Path<Struct>`
all work, through a small serde deserializer. Captures moved from a `HashMap` to
an ordered `Params`, since a hash map makes `/users/{id}/posts/{post}` ambiguous
for a tuple — which also gives `params_iter` the deterministic order its docs
had previously disclaimed. serde's own `StrDeserializer` was not enough: it only
calls `visit_str`, so a `u64` target rejects a perfectly good `"42"`.

### 2.2 Sessions were cookie-only with a non-cryptographic signature

**Was:** `CookieStore` signed with a keyed FNV-1a digest. Documented as not a
trust boundary — but a weak default that is only *documented* as weak is still a
footgun.

**Closed by `8329d6b`.** HMAC-SHA256 via `hmac` + `sha2`, two RustCrypto crates
that pull nothing heavy. The FNV path is gone rather than left as an option.

Still true, and still documented: **signed, not encrypted** — the visitor can
read their own session. Still cookie-only; a Redis-backed store remains
unwritten, but `SessionStore` is public so one is an addition rather than a
change.

### 2.3 No blocking-work offload

**Was:** no way to move blocking work off the runtime. Calling a blocking API
from a handler occupies a runtime worker, and nothing said so.

**Closed by `8329d6b`.** `churust_core::block` moves work to tokio's blocking
pool. A panic inside becomes a `500` rather than taking down the worker,
matching how a panicking handler is already treated.

### 2.4 Static files: no directory listing

**Was:** a served directory with no index file was simply a `404`, with no way
to opt into a listing.

**Closed by `8329d6b`.** `StaticFiles::list_directories`, **off by default** —
a listing discloses every filename under the served root, which is how backup
files and forgotten exports get found. Filenames are HTML-escaped: a file named
`<script>.txt` is stored XSS otherwise, and whoever can write to the served
directory is exactly who would try it. An index file still wins.

**Multi-range stays unimplemented on purpose.** A multi-range request returns
the whole representation, which RFC 9110 permits and which mature static-file
servers commonly do. That was listed as a difference rather than a defect, and
it remains one.

### 2.5 No `Either<A, B>` extractor

**Was:** accepting "JSON or form" in one handler needed two routes and a guard.

**Closed by `8329d6b`**, along with `String` and `Bytes` raw-body extractors,
which `Either` needs. The call is cloned with a buffered body rather than moved,
because a failed first attempt must not have consumed the body.

### 2.6 One bind address, TCP only

**Was:** no Unix sockets, no binding several addresses from one server.

**Closed by `8329d6b`.** `serve_unix` / `start_unix`, unlinking a stale socket
file that a crashed process leaves behind and that would otherwise make bind
fail forever. `AppBuilder::bind` and `engine::serve_many` for several addresses
at once; an empty address list is an error rather than a silent no-op, since a
server that came up on none of its addresses should say so.

## 3. Deliberate non-goals

Unchanged, and listed so the absence is not mistaken for an oversight.

| | Why |
| --- | --- |
| Actor integration | Not a framework requirement. |
| `multipart/byteranges` | RFC-permitted to omit. See §2.4. |
| WebSockets over HTTP/3 | Extended CONNECT (RFC 9220) is a different handshake from the HTTP/1.1 upgrade the `ws` feature implements. Claiming support would produce a connection that fails at the first frame. |

## 3a. Former non-goals, now built

**Reversed 2026-07-25.** Everything below was listed in §3 as a deliberate
absence. All of it is implemented, each piece behind its own opt-in feature, so
a default build is unchanged and nobody pays for what they do not enable.

The reasoning that put these on the list was not wrong, and it is kept here
rather than quietly deleted: compression really does belong in a proxy when
there is a proxy, and an OpenAPI schema really cannot be inferred from erased
handler types. What changed is the answer to "should Churust have it", not the
analysis of what it costs.

| | Original objection | How it was resolved |
| --- | --- | --- |
| Response compression | Belongs behind a reverse proxy for most deployments. | `churust-compression`, opt-in. The objection stands and the crate documentation says so; it is for deployments with no such proxy, or where a streamed response should be compressed before the hop the proxy sits on. |
| HTTP/3 | Not shipped in stable by the frameworks Churust sits next to. | `http3` feature on `churust-core`: a separate QUIC listener over the same pipeline, plus the `Alt-Svc` advertisement without which clients would never try it. |
| HTTP client | Churust owns the server-side ergonomic layer. | `churust-client`, built on the hyper already in the tree, so this is one HTTP implementation used twice rather than a second one. |
| Built-in templating | Application choice. | `churust-templates`, wrapping minijinja rather than inventing a template language. |
| OpenAPI generation | Third-party across the ecosystem. | `churust-openapi`. Generates only what the router actually knows, and refuses to pretend about schemas. |
| Rate limiting | Third-party. | `churust-ratelimit`, using the peer address §1.2 added. |
| Streaming multipart (§5) | The bounded parser is the safe default; an unbounded one is a memory-exhaustion surface. | `MultipartStream`, alongside `Multipart` rather than replacing it. Memory stops scaling with upload size; the `max_body_bytes` ceiling is unchanged, so the unbounded surface never opens. |
| Redis session store (§5) | Not written. | `churust-redis`. `SessionStore` became async to allow it, and gained the previous cookie value so logout can actually revoke. |
| Identity layer (§5) | The pieces are there, the layer is not. | `Identities` / `Identity` / `Authenticated`, with absolute and idle deadlines. |

### What this did to the dependency tree

Nothing, for a default build: every addition is feature-gated and
`default = []` is unchanged. Enabled, they cost what they cost — brotli and
flate2 for compression, minijinja for templates, quinn and h3 for HTTP/3, redis
for the session store.

One change does reach every build with TLS on: `rustls` and `tokio-rustls` are
now pinned to the `ring` provider with default features off. rustls 0.23 defaults
to aws-lc-rs, quinn's QUIC crypto is ring-backed, and two providers in one binary
makes rustls refuse to choose a provider at runtime rather than at compile time.

## 4. Where age is the remaining difference

Not a gap in kind, and the one thing on this page that code cannot close: the
established Rust web frameworks have years of production exposure, large
third-party middleware ecosystems, and published benchmark results. Churust has
358 tests and no production users. Feature parity on a list is not equivalence
in trust.

## 5. What the fixes did not cover

All three entries that stood here — a buffering-only multipart parser,
cookie-only sessions, and no identity layer — were closed on 2026-07-25. See
§3a.

What remains uncovered is narrower:

- **`Multipart` still buffers**, deliberately. It is the default for form fields
  and small attachments because it cannot surprise anyone; `MultipartStream` is
  the answer when an upload is large enough that buffering it is the problem.
- **`max_body_bytes` still bounds every request**, streamed or not. Streaming
  changed what an upload costs in memory, not what it is allowed to weigh.
- **A session cannot be revoked from a client-side store.** `CookieStore` has no
  record to withdraw; that is what `churust-redis` is for, and both crates say
  so.
