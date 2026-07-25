# Churust Hardening + Static-File Correctness — Design Spec (v0.3.0)

**Date:** 2026-07-25
**Status:** Approved — implementing. Ordered by
[`2026-07-25-roadmap-to-parity.md`](2026-07-25-roadmap-to-parity.md).
**Builds on:** Churust 0.2.0, published, 218 tests green.
**Supersedes:** the Phase 2 and Phase 3 sketches in
[`2026-07-25-routing-correctness-and-quickstart-design.md`](2026-07-25-routing-correctness-and-quickstart-design.md) §12,
merged here and re-prioritised against a competitive gap analysis.

## 1. Summary

Six changes, in one release, under one theme: **be correct under load and hard
to hold wrong.**

1. Conditional GET and byte ranges for static files.
2. Security response headers, on by default.
3. Request limits beyond the existing body cap.
4. Make the dangerous things require deliberate effort.
5. Let user error types flow through `?`.
6. A `Form<T>` extractor for HTML form posts, and `Header<T>`.

## 2. Evidence

A gap analysis against the established Rust web frameworks produced ten
verified differences. Each claim below was re-checked directly against this
working tree rather than taken from the analysis.

| Finding | Verification |
| --- | --- |
| No conditional GET, no ranges | `grep -riE 'etag\|last-modified\|304\|range\|partial_content'` over every non-`target` `.rs` — **zero hits** |
| `fs.rs` advertises media types it cannot serve properly | `fs.rs` maps `mp4 → video/mp4`, `mp3 → audio/mpeg`; Safari refuses `<video>` when a range probe gets `200` |
| No cookie primitive | only hit for `cookie` is a doc mention in `churust-cors` |
| No multipart, no urlencoded body extractor | zero hits for `multipart`; `serde_urlencoded` used only by `Query` |
| `?` works only for Churust's own error | `response.rs:237` has `impl<T: IntoResponse> IntoResponse for Result<T>`, but the workspace has **zero** `impl From<_> for Error` |
| HTTP/2 absent and mis-documented | `Cargo.toml` requests only hyper `http1`; design doc claimed HTTP/2 — corrected in `9e989bb` |

## 3. Scope

### In scope

The six items in §1, detailed in §5–§10.

### Deferred, with reasons

**HTTP/2 — its own release.** `engine.rs:52-55` uses `http1::Builder` directly,
with a comment recording that `auto::Builder`'s `serve_connection` returned a
future that could not be owned by the spawn closure. `with_upgrades()` at
`engine.rs:156` — the WebSocket path — is HTTP/1-specific. Supporting HTTP/2
means reworking connection handling and re-proving the upgrade path, plus ALPN
for h2-over-TLS. Bundling that with six unrelated changes would make a
regression impossible to bisect.

**Cookies, then sessions — 0.4.0.** Cookies are the missing layer beneath
sessions; building sessions without them is impossible and building cookies
badly is worse than not having them. Wants its own design pass covering
`SameSite`, `Secure`, signing, and encryption.

**Multipart — 0.4.0 or later.** The largest single item. Needs a streaming
parser, a disk-spooling strategy, and its own limits. A rushed multipart
implementation is a denial-of-service surface.

**Guards, route-scoped middleware, per-route extractor config, request-id and
OpenTelemetry propagation.** Real gaps, none of them blocking. They belong
together in an ergonomics release once the safety work has landed.

## 4. Non-goals

Response compression, HTTP/3, an HTTP client, and OpenAPI generation. Churust
owns the ergonomic layer; compression belongs behind a reverse proxy for most
deployments and can be reconsidered when someone presents a case.

## 5. Conditional GET and byte ranges (`fs` feature)

The highest-value item: it fixes a feature that is advertised and broken.

### 5.1 Validators

Every `StaticFiles` response gains `Last-Modified` from the file's mtime and a
weak `ETag` derived from `(mtime, len)`. Weak rather than strong, because the
bytes are not hashed — a strong ETag would be a lie about what was compared.

Requests are then evaluated in RFC 9110 §13.2 precedence order:

| Request header | Condition | Response |
| --- | --- | --- |
| `If-None-Match` | matches | `304`, no body, validators retained |
| `If-Modified-Since` | not modified, and no `If-None-Match` | `304` |
| `If-Match` | does not match | `412` |
| `If-Unmodified-Since` | modified | `412` |

`If-None-Match` takes precedence over `If-Modified-Since` when both are
present. A `304` carries the validators and no body.

### 5.2 Ranges

`Accept-Ranges: bytes` on every file response. A `Range: bytes=…` request
yields `206` with `Content-Range` and only the requested span; an unsatisfiable
range yields `416` with `Content-Range: bytes */<len>`.

**Single range only.** A multi-range request is answered with the whole entity
as `200`, which RFC 9110 permits. `multipart/byteranges` is not worth its
complexity, and mature static-file servers commonly make the same call.

`If-Range` is honoured: if the validator does not match, the full entity is
returned rather than a partial one.

### 5.3 Interaction with HEAD

Task 4 of 0.2.0 preserves `Content-Length` for buffered bodies on synthesized
`HEAD`. Static files are streamed, so `HEAD` currently omits it. With a known
file length available before streaming, `HEAD` on a static file should now
report the real `Content-Length`. This is a behaviour change to an existing
feature and needs a test.

## 6. Security headers, on by default

A new always-on middleware in the `Setup` phase:

| Header | Value | Notes |
| --- | --- | --- |
| `X-Content-Type-Options` | `nosniff` | always |
| `X-Frame-Options` | `DENY` | always |
| `Referrer-Policy` | `no-referrer` | always |
| `Strict-Transport-Security` | `max-age=31536000` | **only when TLS is enabled** |

Sending HSTS over plaintext is meaningless and can be actively harmful behind a
terminating proxy, so it is conditioned on the `tls` feature being active on
the running server, not merely compiled.

Each header is skipped if the handler already set it — the application wins.
Opt out per-header or entirely via the plugin's builder. No Content-Security-
Policy: a default CSP either breaks applications or is so permissive it
misleads.

This changes every response, which is why it is a minor bump.

## 7. Request limits

Beyond the existing 1 MiB body cap and request timeout:

| Limit | Default | Rationale |
| --- | --- | --- |
| Max header count | 100 | hyper has its own cap; this makes it explicit and configurable |
| Max total header bytes | 16 KiB | |
| Max path segments | 64 | `Router::walk` recurses per segment; deep paths are a stack-depth question |
| WebSocket max frame | 1 MiB | `ws` feature |
| WebSocket max message | 4 MiB | `ws` feature; guards reassembly of fragmented frames |
| Header read timeout | 10 s | slow-loris: a client that dribbles headers holds a connection open |

All configurable through the existing layered config, so `CHURUST_*` env vars
work without new machinery.

The path-segment cap deserves justification: `walk` and `walk_wildcard` recurse
once per segment with backtracking. hyper caps the request line, which bounds
this in practice, but the bound should be Churust's own and stated rather than
inherited by accident.

## 8. Hard to misuse

**`StaticFiles::dir` validates its root when built**, not on each request.
Today a nonexistent root produces `500` per request via
`Error::internal("static root does not exist")` — a configuration error
reported as a runtime fault, once per request, forever.

**`Cors::permissive` is renamed `Cors::allow_any_origin_insecure`**, with
`permissive` kept as a `#[deprecated]` alias. It currently appears in the `api`
example and in `README`; both move to a restrictive configuration, since an
example is the thing people copy.

**A constant-time comparison helper** for `Auth::basic` callbacks, which today
invite `==` on secrets. The helper is exported and used in the docs; the
callback stays user-supplied, so this is a sharp-edge guard rather than a
guarantee.

**Route-conflict detection.** Registering the same `(method, path)` twice
silently replaces the first handler (`router.rs:148`, `HashMap::insert`).
Registration now panics on an exact duplicate, which surfaces the mistake at
startup rather than as a route that mysteriously does nothing.

## 9. Error ergonomics

`response.rs:237` already provides `impl<T: IntoResponse> IntoResponse for
Result<T>`, so handlers return `Result` and `?` works — but the workspace has
**zero** `impl From<_> for Error`, so `?` works only on Churust's own error. A
handler holding `Result<T, sqlx::Error>` needs `map_err` at every call site,
which is precisely the boilerplate a framework selling ergonomics should absorb.

Add a trait that maps a foreign error onto a response:

```rust
/// Implement this so your error type can be returned from a handler with `?`.
pub trait IntoError {
    /// The status this error should produce. Defaults to 500.
    fn status(&self) -> StatusCode { StatusCode::INTERNAL_SERVER_ERROR }
    /// The client-facing message. Defaults to the status' canonical reason,
    /// deliberately *not* the Display output, so internal detail is not leaked
    /// by accident.
    fn message(&self) -> String { ... }
}

impl<E: IntoError> From<E> for Error { ... }
```

The message default matters: defaulting to `Display` would leak database errors
and file paths to clients the first time anyone used it. Opting into detail is
safe; opting out of a leak is not.

Blanket impls for `std::io::Error` and `serde_json::Error` are **not** provided
— a blanket `impl<E: std::error::Error>` would conflict with the trait, and
guessing a status for someone else's error type is how frameworks leak `500`s
with useful attacker information.

## 10. `Form<T>` extractor

`serde_urlencoded` is already a dependency, used by `Query`. A `Form<T>`
body extractor is roughly the same code against the body instead of the query
string: `FromCall` (consumes the body, last position only), `415` when the
content type is not `application/x-www-form-urlencoded`, `400` on
deserialisation failure — matching `Json<T>`'s behaviour exactly so the two are
learnable as one thing.

Lives in `churust-core` beside `Query`, not behind a feature: it adds no
dependency.

## 11. Testing

Static files get the largest share: a validator matrix over `If-None-Match`,
`If-Modified-Since`, `If-Match`, `If-Unmodified-Since` and their precedence; a
range matrix covering `bytes=0-`, `bytes=0-0`, suffix ranges, `bytes=5-2`,
past-EOF, and multi-range falling back to `200`; and `If-Range` both matching
and not. The existing traversal suite must stay green — range handling reads
files by offset and must not become a second path into the filesystem.

Security headers: present by default, absent when opted out, not overwritten
when the handler sets its own, and HSTS only under TLS.

Limits: each one tripped individually, asserting the status and that the
connection is not left hanging.

Error ergonomics: a custom error type reaching a client with the right status
and **without** its `Display` text.

The full 0.2.0 suite (218 tests) stays green.

## 12. Version

`0.3.0`, lockstep across all seven crates.

Breaking: default response headers change; `Cors::permissive` is deprecated;
duplicate route registration now panics where it previously replaced silently.
Each is a deliberate correction, and each is called out in `CHANGELOG.md`.
