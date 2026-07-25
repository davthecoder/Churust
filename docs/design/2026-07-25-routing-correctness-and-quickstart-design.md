# Churust Routing Correctness + One-Dependency Quickstart — Design Spec (v0.2.0)

**Date:** 2026-07-25
**Status:** Approved (design), pending implementation plan
**Builds on:** Churust 0.1.1, all seven crates published, full test suite green.
**Phase:** 1 of 3. Phases 2 (hard to misuse) and 3 (secure by default) are
scoped in §12 and get their own specs.

## 1. Summary

Four changes that make the documented happy path work as a reader would expect:

1. **One dependency.** `churust = "0.2"` alone is enough. Today it does not
   compile — `#[churust::main]` names a crate the user must declare separately.
2. **Wildcard fallback.** A `{path...}` route is currently unreachable whenever
   a static sibling shares its prefix.
3. **HEAD and OPTIONS.** `HEAD` on a `GET` route returns `405` today.
4. **Path percent-decoding.** Path parameters are never decoded, while query
   parameters are.

No new public concepts, no API removals. Every change either fixes a defect or
removes a required step from the quickstart.

## 2. Evidence

Each defect was reproduced before being specified, not inferred from reading.

A throwaway integration test against 0.1.1 produced:

```
GET /files/special      -> 404 Not Found      # /files/{path...} IS registered
HEAD /                  -> 405 Method Not Allowed
GET /u/John%20Doe       -> "name=John%20Doe"
GET /q?name=John%20Doe  -> "q=John Doe"       # query decodes, path does not
```

A crate depending only on `churust` fails to build:

```
error[E0433]: cannot find `tokio` in the crate root
 --> src/main.rs:3:1        # the #[churust::main] line
```

## 3. Scope

**In scope:** the four changes above, their tests, and the docs that describe
them (`README.md`, `CHANGELOG.md`, crate-level docs).

**Explicitly out of scope**, deferred to later phases: security response
headers, request limits (path segments, header count, WebSocket frame size),
`Cors::permissive` gating, `StaticFiles` root validation at build time,
constant-time comparison helpers, and route-conflict detection.

## 4. C1 — One-dependency quickstart

### 4.1 Problem

`churust/Cargo.toml` already depends on `tokio` with `features = ["full"]`, so
every user compiles all of tokio. The umbrella does not re-export it, and
`churust-macros/src/lib.rs:149` emits an absolute `::tokio::runtime::Builder`
path. Users therefore pay tokio's full compile cost *and* must declare it.

### 4.2 Design

`churust/src/lib.rs` gains two re-exports:

```rust
/// The tokio runtime Churust is built on, re-exported so applications do not
/// need their own dependency on it.
pub use tokio;

#[doc(hidden)]
pub mod __private {
    pub use tokio;
}
```

`churust-macros` emits `::churust::__private::tokio::runtime::Builder`. The
`__private` path is what the macro commits to; the public `churust::tokio` is
for application code and can evolve independently.

### 4.3 Macro doctest consequence

`churust-macros` cannot itself compile the new expansion: `::churust` does not
resolve from inside `churust-macros`, because `churust` depends on
`churust-macros` and not the reverse.

Resolution: the doctests in `churust-macros` that currently compile the
expansion become non-compiling illustrations (` ```text `), and real macro
coverage moves to `churust/tests/`, where `::churust` resolves. This is a
deliberate trade — coverage moves, it does not disappear, and the new location
tests the macro the way users actually invoke it.

### 4.4 Tokio feature set

Reduced from `full` to the features the code actually uses, determined by
auditing every `tokio::` path in the workspace:

| Feature | Needed by |
| --- | --- |
| `rt-multi-thread` | `#[churust::main]`, `Runtime` |
| `net` | `TcpListener` |
| `io-util` | `AsyncReadExt` |
| `time` | request timeouts |
| `sync` | `oneshot` in graceful shutdown |
| `signal` | `ctrl_c` in graceful shutdown |
| `macros` | `select!`, `pin!` |
| `fs` | `StaticFiles` — enabled only under Churust's `fs` feature |

Dev-dependencies may enable more (`#[tokio::test]` needs `macros`, `rt`).

Users who need a tokio API outside this set add `tokio` themselves; Cargo
feature unification merges it. This is standard and must be documented.

### 4.5 Acceptance

- A crate whose only dependency is `churust`, using `#[churust::main]`, builds.
- All four `examples/*` drop their `tokio` dependency and still build and run.
- `README.md` install snippet loses its `tokio` line.

## 5. C2 — Router wildcard fallback

### 5.1 Problem

`churust-core/src/router.rs:171-191`: when `walk` reaches a node with no handler
for the method, `Router::route` returns `MethodNotAllowed` or `NotFound`
immediately and never calls `walk_wildcard`. Registering `/files/{path...}`
alongside `/files/special/x` makes `GET /files/special` a 404.

This breaks `StaticFiles` in its most ordinary deployment: a catch-all asset
route mounted beside any other route sharing its prefix.

### 5.2 Design

`Router::route` resolves in this precedence order:

1. Exact node has a handler for the method → `Found`.
2. Otherwise attempt `walk_wildcard` → `Found` if it has the method.
3. Otherwise `MethodNotAllowed`, where `allow` is the **union** of the exact
   node's methods and the wildcard's methods.
4. Otherwise `NotFound`.

Static beats param beats wildcard, unchanged.

### 5.3 Param-map hygiene

`walk` writes captures into the param map as it descends. In the new flow it can
*succeed* structurally and then be rejected on method, leaving captures behind.
The map must be cleared before the wildcard attempt, or a stale `{id}` from the
abandoned branch leaks into the wildcard handler's params. This is a specific
failure mode and gets a specific test.

## 6. C3 — HEAD and OPTIONS

Both are implemented at the dispatch site, `churust-core/src/app.rs:442-460`,
not inside `Router`. The router stays a pure lookup structure with no knowledge
of HTTP method semantics.

### 6.1 HEAD

RFC 9110 requires HEAD wherever GET is available. When a `HEAD` request finds no
registered HEAD route, dispatch retries the lookup as `GET`. If that matches,
the handler runs and the response has its body dropped, preserving status and
all headers.

Body handling by kind:

- **Buffered** — `Content-Length` is retained, body emptied.
- **Streamed** — no body is sent and `Content-Length` is omitted. The stream is
  dropped rather than drained; draining to compute a length would do the
  server's work for an attacker who never wanted the bytes.

An explicitly registered `HEAD` route always takes precedence.

### 6.2 OPTIONS

An `OPTIONS` request matching no registered OPTIONS handler returns `204` with
an `Allow` header listing the registered methods, plus a synthesized `HEAD`
where `GET` exists, plus `OPTIONS` itself.

CORS preflight must keep working. `Cors` runs in the `Plugins` phase and the
router in `Fallback`, so preflight short-circuits before auto-OPTIONS is
reached. That is an assumption about phase ordering rather than a guarantee, so
it is covered by a test that installs `Cors` and asserts preflight behaviour is
unchanged.

## 7. C4 — Path percent-decoding

The security-sensitive change. Ordering is normative, not incidental.

### 7.1 Split before decode

Paths are split on `/` **first**, then each segment is decoded individually.
Decoding before splitting is what allows `%2F` to manufacture separators and
create phantom segments; it must not happen anywhere in the pipeline.

### 7.2 A separate decoder

Path decoding does not reuse `form_urlencoded_first`'s `percent_decode`
(`churust-core/src/call.rs:350`). Two reasons, both defects if carried over:

- `call.rs:357` maps `+` to a space. Correct for `application/x-www-form-
  urlencoded` query strings, wrong in a path, where `+` is a literal character.
- `call.rs:381` ends in `String::from_utf8_lossy`. Replacement characters can
  collapse distinct byte sequences into the same string, which is unacceptable
  input to a security decision.

`decode_path_segment` therefore performs `%XX` decoding only, leaves `+`
untouched, and returns an error on invalid UTF-8.

### 7.3 Rejections

Malformed encoding (`%zz`, truncated `%4`) and non-UTF-8 results both produce
`400 Bad Request`. Neither silently passes through.

### 7.4 Matching

Matching runs on decoded segments, so a route registered as `/a b` is reachable
via `/a%20b`. A decoded segment containing `/` or `\` can never match a static
segment and is preserved verbatim when captured as a parameter.

### 7.5 StaticFiles ordering

Fixed, and each step is independently testable:

```
decode segments
  -> reject any segment containing "/" or "\"
  -> sanitize() rejects "..", root, and prefix components   (existing)
  -> join
  -> canonicalize                                           (existing)
  -> assert containment within canonical root               (existing)
```

Concretely, `StaticFiles` rejects any request whose raw path contains `%2f` or
`%5c` in any case. This is stricter than strictly necessary, chosen so that the
rejoined wildcard parameter is unambiguous by construction rather than by
argument. Filenames containing a literal encoded slash are not servable; that is
an accepted limitation.

### 7.6 Why this is security work

Today there is no traversal hole *because* nothing decodes: `%2e%2e%2f` stays
literal and simply fails to match a file. Adding decoding without fixing the
ordering would convert a correctness bug into a directory-traversal
vulnerability. The ordering above, and the tests in §9, are the reason that does
not happen.

## 8. Error handling

No changes to the `Error` type or the error-response format.

| Condition | Status |
| --- | --- |
| Malformed percent-encoding in path | `400` |
| Path segment decodes to invalid UTF-8 | `400` |
| `%2f` / `%5c` in a `StaticFiles` path | `404` |
| Path matches no route | `404` (unchanged) |
| Path matches, method does not | `405` + `Allow` (unchanged, union'd) |

`404` rather than `400` for the `StaticFiles` rejection: a static file server
should not disclose whether a rejected path would have existed.

## 9. Testing

**Decoder** — table-driven over `%20`, `+`, `%2e%2e`, `%252e`, `%zz`, truncated
`%4`, invalid UTF-8, empty segments, and plain ASCII.

**Router precedence** — a matrix over exact/param/wildcard × method present or
absent, including the `allow`-union case and the stale-param-leak case from
§5.3.

**HEAD/OPTIONS** — via `TestClient`: HEAD falls back to GET, an explicit HEAD
route wins, streamed bodies send no body and no `Content-Length`, auto-OPTIONS
returns the right `Allow`, and CORS preflight is unaffected when `Cors` is
installed.

**Traversal** — a dedicated suite firing `%2e%2e%2f`, `..%2f`, `%2e%2e/`,
`%252e%252e%252f`, `%2f`, `%5c`, and backslash variants at `StaticFiles`,
asserting `404`/`400` and that no byte from outside the root is ever returned.
A file is planted outside the root so a passing traversal would be detectable
rather than merely absent.

**Quickstart regression** — a test crate depending only on `churust` and using
`#[churust::main]`, guarding the `E0433` from §2.

All 189 existing tests stay green. Work is test-first: each defect gets a
failing test that reproduces §2's output before any fix.

## 10. Version and release

`0.2.0`, lockstep across all seven crates via `cargo release minor --execute`.

Not API-breaking, but HEAD, OPTIONS, and path decoding all change observable
behaviour, and the tokio feature reduction can require a user who relied on
transitively-enabled tokio features to declare them. A minor bump pre-1.0 is
correct, and `CHANGELOG.md` calls out the feature-set change explicitly.

## 11. Decisions taken

Recorded so they are not silently revisited during implementation:

- **`%2f` is rejected rather than supported** in `StaticFiles` paths.
  Strictness over flexibility, because the alternative requires reasoning about
  rejoined segments to prove safety.
- **Streamed bodies under HEAD omit `Content-Length`** rather than being drained
  to compute one.
- **Macro doctests move** rather than introducing a dev-dependency cycle.
- **Matching happens on decoded segments**, so encoded requests reach routes
  registered with literal spaces.
- **`StaticFiles` rejections are `404`, not `400`**, to avoid disclosure.

## 12. Later phases

**Phase 2 — hard to misuse.** `StaticFiles` validates its root at build time
rather than per request; `Cors::permissive` is gated or loudly documented;
constant-time comparison helpers for Basic auth callbacks; route-conflict
detection at registration.

**Phase 3 — secure by default.** `X-Content-Type-Options`, `X-Frame-Options`,
`Referrer-Policy` on by default with opt-out, `Strict-Transport-Security` under
TLS; caps on path segments, header count and size, and WebSocket frame and
message size; slow-loris read timeout.
