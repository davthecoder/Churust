# Churust Roadmap to Parity — Release Train

**Date:** 2026-07-25
**Status:** Ordering document. Each release gets its own spec before implementation.
**Baseline:** Churust 0.2.0 published, 218 tests green.

## Why this exists

Three separate backlogs turned out to overlap:

1. **v1 design debt** — items in
   [`2026-06-06-churust-framework-design.md`](2026-06-06-churust-framework-design.md)
   that were specified but never built.
2. **The 0.3.0 hardening spec** —
   [`2026-07-25-hardening-and-static-correctness-design.md`](2026-07-25-hardening-and-static-correctness-design.md).
3. **The competitive gap analysis** — ten verified differences against the
   established Rust web frameworks.

Route-scoped middleware appears in all three. Per-route limits appear in two.
This document merges them into one ordered train so nothing is built twice and
nothing is silently dropped.

## Audit: v1 spec items never implemented

Verified against the working tree on 2026-07-25:

| v1 § | Item | State | Lands in |
| --- | --- | --- | --- |
| 5.1 | `call.receive_json()` / `call.respond_json()` | **done** (0.4.0) | — |
| 5.2 | `Header<T>` extractor | **done** (0.4.0) | — |
| 5.3 | `on_error` status→response hook | **done** (0.4.0) | — |
| 6 | Scope-local middleware (`r.intercept`) | **done** (0.4.0) | — |
| 9 | SIGTERM + grace period | present | — |
| 12 | Read / idle timeouts | **done** (0.3.0) | — |

## Ordering principle

Dependencies first, then value-per-risk, with the engine rework last because it
is the only change that can break every request path at once.

- Cookies must precede sessions — sessions without a cookie primitive is not a
  thing that can be built.
- Limits precede multipart — multipart without limits is a denial-of-service
  surface, and the limits work builds the config plumbing multipart needs.
- Scoped middleware precedes guards — both touch route registration, and doing
  them in the same release means one migration for users rather than two.
- HTTP/2 is last and alone.

## The train

### 0.3.0 — Correct under load, hard to misuse

Spec: [`2026-07-25-hardening-and-static-correctness-design.md`](2026-07-25-hardening-and-static-correctness-design.md)

1. Conditional GET (`ETag` / `Last-Modified` / 304 / 412) and byte ranges
   (206 / 416 / `If-Range`) for static files
2. Security response headers, on by default, opt-out
3. Request limits: header count and bytes, path segments, WebSocket frame and
   message caps, header read timeout (slow-loris), plus v1 §12 read/idle
   timeouts
4. Hard to misuse: `StaticFiles` root validated at build, `Cors::permissive`
   deprecated, constant-time compare helper, duplicate-route panic
5. `IntoError` so `?` works on foreign error types
6. `Form<T>` urlencoded body extractor

**Why first:** item 1 fixes an advertised feature that is broken today, and the
rest are safety properties that get harder to add once more surface exists.

### 0.4.0 — Ergonomics parity — **complete**

7. Route-scoped middleware — v1 §6 debt
8. Routing guards / predicates — a `Guard` trait, `guard::Host`/`Header`,
   `All`/`Any`/`Not` combinators
9. Per-route extractor configuration — size limits and error handlers per route,
   replacing the single global body cap as the only knob
10. `on_error` status→response hook — v1 §5.3 debt
11. Request id and W3C `traceparent` propagation
12. `call.receive_json()` / `call.respond_json()` — v1 §5.1 debt
13. `Header<T, N>` extractor — v1 §5.2 debt. **Design decision resolved:** the
    v1 doc named `Header<T>` without saying which header it reads. A const
    string parameter is not expressible on stable Rust; deserializing the whole
    header map would silently accept junk; a `headers`-crate typed trait would
    add a dependency for a small win. Settled on a marker type implementing
    `HeaderName`, so the header is part of the handler signature, costs nothing
    at runtime, and needs no dependency.

**Why together:** 7, 8 and 9 all change route registration. One release means
one migration.

### 0.5.0 — Web application essentials

13. Cookie primitive: parsing, `Set-Cookie` building, `SameSite`, `Secure`,
    `HttpOnly`, signing and encryption
14. Sessions on top of cookies: a `SessionStore` trait plus a cookie-backed
    store
15. Multipart form data with streaming parse, disk spooling, and its own limits

**Why after limits:** 15 without 0.3.0's limits work is an unbounded-memory
upload endpoint.

### 0.6.0 — HTTP/2 — **complete**

16. Move the engine from `http1::Builder` to a builder that negotiates both,
    add ALPN for h2-over-TLS, re-prove the WebSocket upgrade path

**Why last and alone.** `engine.rs:52-55` records that `auto::Builder`'s
`serve_connection` returned a future the spawn closure could not own — the
original author hit this and chose HTTP/1. `with_upgrades()` at `engine.rs:156`
is HTTP/1-specific, so WebSockets must be re-proven. This is the one change
that can break every request path simultaneously, and it deserves a release
where nothing else is moving.

## After the train

All ten findings from the gap analysis are closed. A fresh audit of what
remains — verified against the tree rather than carried over — is in
[`2026-07-25-production-readiness-audit.md`](2026-07-25-production-readiness-audit.md).
The headline item is that request bodies are always buffered, which caps upload
size at memory and is the ceiling `Multipart` inherits.

## Explicit non-goals

Response compression (belongs behind a reverse proxy for most deployments),
HTTP/3, an HTTP client, built-in templating, OpenAPI generation, and actor
integration. Churust owns the ergonomic layer.

## Documentation, continuously

`README.md`, `SECURITY.md`, `CONTRIBUTING.md` and `CHANGELOG.md` are updated
**within** each release, not afterwards. `SECURITY.md`'s "areas worth your
attention" list in particular must gain each new surface as it lands — cookies,
multipart, and range handling each add an attack surface it currently does not
mention.
