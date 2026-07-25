# Changelog

All notable changes to Churust are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

All seven crates — `churust`, `churust-core`, `churust-macros`, `churust-json`,
`churust-logging`, `churust-cors`, `churust-auth` — share one version and are
released together, so every entry below applies to the whole set.

## [Unreleased]

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
