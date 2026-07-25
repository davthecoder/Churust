# Changelog

All notable changes to Churust are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

All seven crates — `churust`, `churust-core`, `churust-macros`, `churust-json`,
`churust-logging`, `churust-cors`, `churust-auth` — share one version and are
released together, so every entry below applies to the whole set.

## [Unreleased]

### Added

- **`churust::tokio`.** The runtime is re-exported, so `churust = "0.2"` is a
  complete dependency list. Previously a crate depending only on `churust`
  failed to compile with `E0433: cannot find tokio in the crate root`, because
  `#[churust::main]` expanded to an absolute `::tokio` path.
- **Automatic `HEAD`.** A `HEAD` request to a route with only a `GET` handler
  now runs that handler and returns its status and headers without a body, per
  RFC 9110 §9.3.2. Previously `405`. An explicitly registered `HEAD` route still
  wins, and `HEAD` on a route with no `GET` is still `405`. `Content-Length` is
  preserved for buffered bodies so a client sizing a resource gets the same
  answer `GET` would give; streamed bodies are dropped rather than drained and
  omit the header.
- **Automatic `OPTIONS`.** An `OPTIONS` request no handler claims returns `204`
  with `Allow`, listing the registered methods plus a synthesized `HEAD` where
  `GET` exists. CORS preflight keeps priority.
- `Router::methods_for`, which reports the methods registered for a path
  including any a trailing wildcard would serve.

### Fixed

- **A `{path...}` wildcard was unreachable whenever a static route shared its
  prefix.** With `/files/{path...}` and `/files/special/x` both registered,
  `GET /files/special` returned `404`. This broke `StaticFiles` in its most
  ordinary deployment — a catch-all asset route beside any other route.
  Resolution is now exact match, then wildcard, then `405` with the union of
  both allow sets, then `404`.
- **Path parameters are percent-decoded**, matching query parameters, which
  already were. `/u/John%20Doe` now yields `John Doe` rather than
  `John%20Doe`. Segments are split before decoding, so `%2F` cannot forge a
  path separator, and `+` stays literal in a path rather than becoming a space.
- Malformed percent-encoding and non-UTF-8 path segments return `400` instead
  of being passed through.
- `StaticFiles` refuses encoded separators (`%2F`, `%5C`). They previously
  became real separators once a wildcard's segments were rejoined. Traversal
  was already blocked by the existing `..` rejection; this removes the need to
  reason about rejoining to establish that.

### Changed

- **Tokio features narrowed** from `full` to the set Churust uses:
  `rt-multi-thread`, `net`, `io-util`, `time`, `sync`, `signal`, `macros`, plus
  `fs` under Churust's own `fs` feature. If you relied on a tokio feature
  arriving transitively, declare `tokio` yourself with the features you need;
  Cargo unifies them.
- `#[churust::main]` expands to `::churust::__private::tokio` rather than
  `::tokio`. Invoking the macro directly from `churust-macros` is no longer
  supported; depend on `churust`.

### Breaking

- `Match` gained a `BadPath` variant and is now `#[non_exhaustive]`, so a
  `match` on `churust_core::Match` needs a `_` arm. This affects code driving
  `Router` directly; applications built on the routing DSL are unaffected.

## [0.1.1] - 2026-07-25

### Fixed

- `churust-core`: removed a redundant explicit link target in the `ws.rs` docs.
  It broke `cargo doc` under `-D warnings`, which meant docs failed to build
  under a denying rustdoc configuration.

No API changes. `0.1.1` is a drop-in replacement for `0.1.0`.

## [0.1.0] - 2026-07-25

First public release.

### Added

- **Engine and routing** — application engine over tokio + hyper (HTTP/1.1),
  a trie router with path parameters, and a routing DSL.
- **Hybrid handlers** — call-style (`|call: Call|`) or typed extractors
  (`Path<T>`, `Query<T>`, `State<T>`, `Json<T>`, `BearerToken`, `Principal<P>`),
  mixable in a single handler. `FromCallParts` borrows in any position;
  `FromCall` consumes the body in the last position.
- **Phased pipeline** — `install(plugin)` composing middleware over a named,
  deterministic order: `Setup < Monitoring < Plugins < Call < Fallback`.
- **Plugins** — JSON content negotiation (`json`), request logging over
  `tracing` (`logging`), CORS (`cors`), and Bearer/Basic/JWT authentication
  (`auth`). `full` enables all four.
- **Typed state** — `.state(T)` and the `State<T>` extractor.
- **Layered config** — defaults < `churust.toml` < `CHURUST_*` env < code DSL.
- **Streaming bodies** — the always-on `Body` type, buffered bytes or a lazy
  stream.
- **Static files** (`fs`) — `StaticFiles` with MIME detection, optional
  directory index, chunked streaming, and rejection of path traversal via `..`,
  absolute paths, and symlink escapes.
- **WebSockets** (`ws`) — `WebSocketUpgrade` extractor and `on_upgrade`.
- **TLS** (`tls`) — rustls-backed HTTPS.
- **`#[churust::main]`** — entrypoint macro.
- **Test harness** — `TestClient` drives the full pipeline in-process without
  binding a socket.
- **Secure defaults** — body-size limits, request timeouts, panic isolation (a
  panicking handler returns 500 rather than killing the server), and no version
  banner.

[Unreleased]: https://github.com/davthecoder/Churust/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/davthecoder/Churust/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/davthecoder/Churust/releases/tag/v0.1.0
