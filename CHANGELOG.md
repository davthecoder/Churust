# Changelog

All notable changes to Churust are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

All seven crates — `churust`, `churust-core`, `churust-macros`, `churust-json`,
`churust-logging`, `churust-cors`, `churust-auth` — share one version and are
released together, so every entry below applies to the whole set.

## [Unreleased]

### Changed — breaking

- **`//a` and `/a//b` no longer serve `/a/b`.** Interior empty segments were
  collapsed silently, so one resource had several URLs. Two things follow from
  that: any middleware, guard or proxy rule keyed on a literal prefix
  (`path.starts_with("/admin")`) is bypassable with `//admin`, and a cache keys
  on the URL, so one resource occupies several entries while an intermediary and
  the origin disagree about identity.

  The new `PathPolicy` states the behaviour instead of leaving it emergent:
  `Strict` (**the default**) answers `404`, `Redirect` answers `308` to the
  canonical form — `308` not `301`, so a `POST` stays a `POST` — and `Collapse`
  restores the old behaviour for one release. Set it with
  `AppBuilder::path_policy`, `CHURUST_SERVER_PATH_POLICY`, or `path_policy` in
  `churust.toml`.

  **URLs that stop working under the default:** any request whose path contains
  a repeated slash. A **trailing** slash is *not* affected and is not treated as
  an alias — a directory listing at `/files/` contains relative links that a
  browser resolves against that slash, and stripping it would serve HTML whose
  every link resolves one level too high.

- **`Query<T>` and `Form<T>` reject a repeated key on a scalar field** rather
  than silently picking one. `?q=a&q=b` into a `String` is ambiguous — browsers
  take the last occurrence, some servers take the first — and resolving it
  quietly means a proxy and an origin can disagree about what was asked. Declare
  the field as `Vec<T>` to accept repetition deliberately.

### Fixed

- **A saturated connection budget no longer blocks shutdown.** The accept loop
  awaited a `max_connections` permit *outside* the shutdown race, so once every
  slot was held the shutdown signal was never polled — `serve()` did not return
  and the process had to be `SIGKILL`ed, with `shutdown_timeout_ms` unable to
  bound it. Long-lived connections (WebSockets, SSE, idle h2) made that the
  normal case, not the edge one. Slot acquisition and `accept` are now one
  cancellable step raced against shutdown, in both `serve` and `serve_unix`.
- **A directory listing no longer follows a symlink out of the served root.**
  The symlink-escape guard ran *after* the directory dispatch, and
  `metadata` follows symlinks — so a link inside the root pointing at, say,
  `/etc` reported `is_dir()`, took the listing branch, and returned before the
  guard. Every filename under the target was disclosed. The file path was
  already guarded; only the listing was not. Confinement now runs before
  anything decides what to do with the target, and again after an index file is
  joined.
- **Directory listing links resolve.** They were prefixed with the
  request-relative path, which is right at a subdirectory *without* a trailing
  slash and wrong everywhere else — at `/files/sub/` every link resolved to
  `/files/sub/sub/…`. A directory URL now canonicalises to the trailing-slash
  form (`308` otherwise) and links are bare, so the same markup works at any
  depth. This also makes the `PathPolicy` docs' justification for preserving
  trailing slashes true, which it was not before.
- **A shallow `{name...}` no longer shadows a deeper one.** `walk_wildcard`
  consulted a node's own wildcard before descending, so registering
  `/files/{p...}` and `/files/img/{q...}` made the second unreachable for
  everything under `/files/img/` — silently. The deepest wildcard now wins, a
  `405` found deeper does not pre-empt a real handler found shallower, and when
  neither depth serves the method the `Allow` header unions both.
- **Conflicting path parameter names fail at startup.** `/users/{id}` followed
  by `/users/{name}/profile` silently reused `id` for both, so the second
  handler looked up a key that was never captured and returned `400` with no
  clue why. Now a panic at registration, like a misplaced wildcard or a
  duplicate route.
- **`Call::cookie` reads every `Cookie` header field.** HTTP/2 permits cookie
  crumbs as separate fields (RFC 9113 §8.2.3). Reading only the first meant a
  session cookie in the second was invisible, so every h2 request looked freshly
  anonymous — a silent logout on each request.
- **Session cookies carry a signed expiry.** `Max-Age` on `Set-Cookie` is only a
  hint to a well-behaved client, so a captured cookie stayed valid for as long
  as the signing key did. `Sessions::max_age` now signs the deadline into the
  payload and `CookieStore` enforces it on load. Revocation still needs
  server-side state, and the docs now say so.
- **`on_error` no longer drops protocol-required headers.** It replaced the
  whole response, so installing a custom error page silently removed `Allow`
  from every `405` (RFC 9110 §15.5.6 requires it), and likewise
  `WWW-Authenticate` and `Content-Range`. Headers the renderer did not set are
  now carried over.
- **`serve_many` aborts at bind, as documented.** Binding happened inside each
  spawned accept loop, so a failure surfaced only after shutdown: the process
  served the addresses that worked and said nothing. Every address is now bound
  before any starts serving.
- **`receive_json` and `receive_text` report an over-limit body as `413`.** Both
  used `receive_bytes`, which swallows a read error into an empty payload, so an
  oversized body became `400 invalid JSON body: EOF while parsing a value` — and
  the per-route cap was skipped entirely.
- **The tower adapter preserves `Content-Length`.** It rewrapped every layered
  response as a stream, so any response through any layer — even `Identity` —
  lost its exact length and was chunked, and a synthesized `HEAD` could no
  longer report a size. It also now writes the URI back alongside the headers,
  so a path-rewriting layer is not silently a no-op, and a layer that drives the
  inner service from another task gets a named error instead of a panic.
- **`BearerToken` accepts any casing of the scheme.** RFC 7235 §2.1 makes it
  case-insensitive; matching the literals `Bearer ` and `bearer ` rejected
  `BEARER ` with a `401` for a valid credential.
- **`413` is selected by error type, not by message text.** The oversized-body
  path matched on `http-body-util`'s `Display` output, which a patch release
  could change and silently turn every `413` into a `400`.
- **A malformed `churust.toml` is reported.** A parse error silently reverted
  every setting to defaults — host, port, limits and TLS paths — over a single
  typo, with nothing logged. A missing file remains the ordinary, quiet case.

- **Graceful shutdown now drains.** It never had. The engine took a shutdown
  watcher and dropped it immediately, so `GracefulShutdown::shutdown()` had
  nothing to wait on and returned in microseconds — measured at 378µs with a
  request in flight. `shutdown_timeout_ms` had never delayed an exit. In a
  binary this meant `main` returned and the process tore down mid-response.
  Connection tracking is now Churust's own, because hyper-util's sealed
  `GracefulConnection` covers the plain connection but not the upgradeable one
  WebSockets need.
- **`keep_alive_ms` is a duration again.** It was read as `keep_alive_ms > 0` —
  a boolean — so `5000` and `86400000` were the same program and idle
  connections were never closed. Each cost a file descriptor and a task, which
  made "open connections and go quiet" the cheapest way to exhaust a server.
  Idle means *no request in flight*, so a slow handler is never cut off, and an
  upgraded WebSocket is never truncated.

- **`Allow` agreed with itself.** A path with one `GET` route told `OPTIONS` it
  supported `GET, HEAD, OPTIONS` and simultaneously told a `DELETE` that it
  supported only `GET`. RFC 9110 §15.5.6 and §9.3.7 describe the same fact about
  the same resource; it was generated in two places, and the two drifted. There
  is now one generator, and the header is sorted so both responses are
  byte-identical. A `405` whose `Allow` would be empty is now a `404`.
- **`OPTIONS *` answers.** RFC 9110 §9.3.7 defines the asterisk-form target as
  a question about the server, not a resource. It was routed as a path and
  answered `404` — the one reply that is certainly wrong to a capability probe.
  Now `204` with the methods registered anywhere in the router.
- **Requests carrying both `Transfer-Encoding` and `Content-Length` are refused**
  with `400` and `Connection: close`. hyper frames such a message by the
  transfer coding, which RFC 9112 §6.3 permits — but the risk is upstream: a
  proxy that believed the `Content-Length` forwards a different number of body
  bytes than this server consumes, and the leftovers become the next request on
  a reused connection. Since what sits in front cannot be known, the ambiguity
  is refused.
- **Shutdown no longer waits out the grace period for idle connections.** An
  idle HTTP/2 connection is wound down by sending GOAWAY and waiting for the
  *peer* to close, which an idle peer never does — so every shutdown cost the
  full `shutdown_timeout_ms` and a rolling restart paid it on every instance.
  A connection with no request in flight now gets a brief window to flush its
  GOAWAY and is then dropped. Connections with work in flight are unaffected.

### Added

- **`tower` feature: run a `tower::Service` as Churust middleware.** The
  ecosystem's `Layer`s — compression, tracing, request ids, header manipulation,
  validation, metrics — become reachable without reimplementing any of them
  here. The adapter is one-directional; `Middleware`/`Next` stays the native way
  to write one. The layered service is built once at install time, so a stateful
  layer keeps its state across requests. Two documented limits: backpressure is
  not propagated (`poll_ready` has no counterpart in a pipeline handed an
  already-accepted request), and a `Service` error becomes a `500` — which keeps
  the always-produce-a-response invariant without axum's `Infallible` boundary,
  where adding a timeout layer to a route requires wrapping it in a
  `HandleErrorLayer`.
- **`Body` implements `http_body::Body`**, which is what lets it cross the
  `http`/`http-body` boundary the adapter needs.
- **`Call::headers_mut`**, so middleware can rewrite the request head.
- **`churust-lab`**, an incubation crate that will never reach 1.0. Ideas worth
  trying in public but not worth freezing into `churust-core`'s API live there
  first, and graduate by being deprecated in place rather than deleted, so users
  migrate by dropping the prefix. First inhabitant: `BodyLimit<T, LIMIT>`, a
  body cap written in the type. It shares the workspace version for now; the
  crate docs record why, and when that must be revisited.
- **`Option<T>` extracts optionally**, via a new `OptionalFromCallParts` trait
  that `Query`, `Path`, `Header` and `BearerToken` implement. One trait rather
  than a parallel `OptionalQuery`/`OptionalPath`/`OptionalHeader` type per
  extractor — the design axum-extra shipped, deprecated, and replaced with this
  one. **Absent is `None`; malformed is still an error**, so a typo'd query
  string does not silently become a default.
- **Handler type errors say what is wrong.** A closure that does not satisfy the
  handler bounds produced a bare `the trait bound ...: IntoHandler<_> is not
  satisfied`, which names the trait and nothing else. `#[diagnostic::on_unimplemented]`
  now states the actual rules — every argument but the last is `FromCallParts`,
  only the last may consume the body, there can be only one — and
  `#[diagnostic::do_not_recommend]` stops rustc suggesting the blanket impls,
  which pointed at implementing `Handler` by hand instead of at the wrong
  argument.
- **Repeated query and form keys fill a `Vec<T>`.** `?tag=a&tag=b` and a
  checkbox group posting `opt=email&opt=sms` are ordinary HTML — a repeated
  checkbox and `<select multiple>` produce exactly this — and both previously
  failed with `400 invalid type: string "a", expected a sequence`, blaming the
  caller for the framework's parser choice with no workaround inside the
  extractor. `Query` and `Form` now parse with `serde_html_form`.
- **`Payload` respects the per-route body cap.** Buffering extractors checked
  it; the streaming one did not, so a route that tightened its limit was not
  actually tightened, and a handler collecting the stream allocated up to the
  server-wide ceiling — which an operator may have raised for one legitimate
  upload route, leaving every other `Payload` route to inherit it.
- **`h2_max_header_list_size`** (default `16384`) and
  **`h2_max_concurrent_streams`** (default `200`, `0` unlimited). `max_headers`
  configures HTTP/1 only — it counts headers, and HTTP/2 has no count, only an
  encoded size. An h2 connection multiplexes many requests, so without a stream
  cap one connection is an unbounded amount of concurrent work.
- **`max_connections`** (default `25000`, `0` unlimited) — a bound on
  connections *being served*, acquired before `accept` so excess load waits in
  the kernel backlog rather than as memory and descriptors in the process.
- **`max_tls_handshakes`** (default `256`) and **`tls_handshake_timeout_ms`**
  (default `10000`). A handshake is asymmetric work — cheap to request,
  expensive to answer — so it gets its own tighter bound, and
  `header_read_timeout_ms` cannot cover it because until the handshake finishes
  there is no HTTP layer to time out.
- Accept errors now back off (capped exponential, reset on success) instead of
  spinning. A persistent `EMFILE` — the state a connection flood drives toward —
  previously became a busy loop that starved the tasks that would free a
  descriptor.
- Failed and timed-out TLS handshakes are logged at debug. They were silent,
  which made certificate and protocol-version problems invisible to operators.
  Debug rather than warn: an internet-facing port sees constant scanner noise.

- **Streaming request bodies.** The engine hands the body to the handler as a
  stream instead of collecting it. `Payload` exposes the stream;
  `Call::try_receive_bytes` collects on demand for extractors that need a whole
  body. Previously `max_body_bytes` was a hard ceiling on upload size and memory
  scaled as `max_body_bytes × concurrent uploads`.
- **`Call::peer_addr`** — the connection's socket address, which per-IP rate
  limiting and audit logging had no way to obtain. Behind a reverse proxy this
  is the proxy; check it before trusting `X-Forwarded-For`.
- **Deployment tuning** — `keep_alive_ms`, `backlog` and `shutdown_timeout_ms`
  through the layered config. Graceful shutdown was previously unbounded, so one
  slow request delayed exit forever, which under an orchestrator means being
  killed rather than exiting cleanly. Binding now sets `SO_REUSEADDR`, removing
  the most common cause of a failed restart.
- **`Path<T>` destructuring** — `Path<(u64, String)>` and `Path<Struct>`, not
  just a single positional parameter. Captures are now ordered, which also gives
  `params_iter` the deterministic order its docs had disclaimed.
- **`churust_core::block`** for blocking work, so a synchronous call does not
  occupy a runtime worker. A panic inside becomes a `500`.
- **`Either<A, B>`** to accept one of two extractors — JSON or form in one
  handler — plus `String` and `Bytes` raw-body extractors.
- **`StaticFiles::list_directories`**, opt-in. Filenames are HTML-escaped: a
  file named `<script>.txt` is stored XSS otherwise.
- **Unix domain sockets** via `serve_unix` / `start_unix`, unlinking a stale
  socket file that would otherwise make bind fail forever.
- **Several bind addresses** via `AppBuilder::bind` and `engine::serve_many` —
  IPv4 and IPv6, or a public and an admin port.
- **HTTP/2.** The engine now uses hyper-util's `auto` builder, negotiating h2
  over TLS via ALPN (advertised `h2`, then `http/1.1`) and h2c by prior
  knowledge in plaintext. HTTP/1.1 behaviour is unchanged.

  The v1 engine used `http1::Builder` directly with a note that `auto::Builder`
  returned a future the spawn closure could not own. That was accurate:
  `serve_connection_with_upgrades` borrows the builder. Moving the builder into
  the spawned task resolves it, and the upgrades variant is what keeps
  WebSockets working.

  Graceful drain does not cover upgraded connections — hyper-util 0.1.x does
  not implement `GracefulConnection` for them, and a WebSocket has no request
  boundary at which to drain anyway.
- **Cookies.** `Call::cookie` reads a named cookie percent-decoded;
  `Response::with_cookie` appends a `Set-Cookie` header — appends rather than
  replaces, since several cookies each need their own header. Defaults are the
  safe ones: `HttpOnly`, `SameSite=Lax`, `Path=/`, because a cookie is a
  credential until proven otherwise.
- **Sessions** (`Session`, `Sessions`, `SessionStore`, `CookieStore`).
  An unchanged session is not re-issued, which avoids quietly
  extending the expiry of a session the visitor is not using. `CookieStore`
  **signs but does not encrypt** — the visitor can read their own session — and
  the signature is HMAC-SHA256, compared in constant time.
- **`Multipart`** (`multipart` feature) — `multipart/form-data` uploads, with
  `field`, `file` and `parts` accessors. Parsing runs over the buffered body,
  so an upload is bounded by `max_body_bytes` and by any per-route cap; a
  part-count limit guards a body that is small overall but made of very many
  parts. Not a streaming parser, which is the deliberately safe choice and why
  it landed after the limits work.
- **`TestResponse::headers`** for assertions on headers that repeat, such as
  `Set-Cookie`.
- **Route-scoped middleware.** `RouteBuilder::intercept` applies middleware to
  routes registered later in the scope, including nested ones. Nested scopes
  inherit a clone of the parent chain, so a child cannot affect its parent, and
  a route registered *before* the `intercept` call is deliberately not covered —
  the reading order of the block matches the behaviour. Completes v1 design §6.
- **Route guards.** `RouteBuilder::guard` attaches a predicate to the route just
  registered; several routes may share a `(method, path)` when guards
  distinguish them, and the first whose guards pass serves. Ships
  `guard::header`, `guard::host`, `guard::fn_guard`, and the `all`/`any`/`not`
  combinators. A request matching no candidate is `404`, **not** `405` — the
  route does not match, and advertising the method as allowed would tell a
  client to retry something that can never succeed.
- **Per-route body limits.** `RouteBuilder::max_body_bytes` tightens the
  server-wide cap for one route, so an upload endpoint can be generous while
  the rest of the API stays strict. Enforced by `Json<T>` and `Form<T>` with
  `413`.
- **`on_error` status pages.** Render your own `4xx`/`5xx` responses; returning
  `None` keeps the default, so a hook can take over only the statuses it cares
  about. Covers routing failures such as `404` and `405` that never produced an
  `Error`. Runs inside the security-header layer, so a replaced error page is
  still protected. Completes v1 design §5.3.
- **`Header<T, N>` extractor.** Reads one named header, parsed into `T`, with
  the name carried by a marker type implementing `HeaderName` so it is part of
  the handler signature. Completes v1 design §5.2.
- **`CallJson` trait** (`json` feature) — `call.receive_json()` and
  `call.respond_json()` for call-style handlers, completing the hybrid API.
  Lives in `churust-json` so the core keeps no `serde_json` dependency.
  Completes v1 design §5.1.
- **Request correlation** (`logging` feature). `CallLogging` seeds a
  `RequestId`, emits `request_id` and `trace_id` on every line, and echoes
  `x-request-id`. An inbound W3C `traceparent` is continued so a trace survives
  a service boundary — validated rather than trusted, since an all-zero or
  malformed trace id from one caller would otherwise poison the correlation
  index.
- **`AppBuilder::install_middleware`** — the chainable counterpart to
  `add_middleware`.

- **Conditional GET for static files.** Every `StaticFiles` response now carries
  a weak `ETag` (from mtime and length) and `Last-Modified`. `If-None-Match`
  and `If-Modified-Since` produce `304`; `If-Match` and `If-Unmodified-Since`
  produce `412`. Per RFC 9110 §13.1.3, `If-None-Match` suppresses
  `If-Modified-Since` entirely rather than acting as a tie-breaker. Without
  validators a cache could never revalidate — every repeat request refetched
  the whole file.
- **Byte-range requests for static files.** `Accept-Ranges: bytes` on every file
  response, `206` with `Content-Range` for a single span, `416` with
  `bytes */<len>` when unsatisfiable, and `If-Range` honoured. This fixes media
  playback: Churust maps `video/mp4` and `audio/mpeg`, and Safari refuses to
  play a `<video>` whose range probe is answered `200`. Multi-range requests
  return the whole entity rather than `multipart/byteranges`, which RFC 9110
  permits.
- **Security response headers by default** — `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, and
  `Strict-Transport-Security` **only when TLS is configured**. A handler that
  sets one of these itself wins. Configure with
  `AppBuilder::security_headers`, or disable with `without_security_headers`.
  There is no default `Content-Security-Policy`: a generic one either breaks
  pages or implies protection it does not give.
- **Request limits**, all configurable via `churust.toml` and `CHURUST_*`:
  header read timeout (10s, the slow-loris defence), max headers (100), max
  path segments (64, rejected with `414`), and WebSocket frame (1 MiB) and
  message (4 MiB) caps. Closes v1 design §12, which specified read timeouts
  that were never built.
- **`IntoError`** so `?` works on foreign error types. Its `message` defaults to
  the status' canonical reason and **not** `Display` — error types routinely
  render connection strings and file paths, and forwarding those to clients by
  default would turn every adopter's first `?` into an information disclosure.
- **`Form<T>`** extractor for `application/x-www-form-urlencoded` bodies. `415`
  on the wrong content type, `400` when the body does not deserialize.
- **`secure_compare`** for checking a request-supplied value against a secret
  without leaking its contents through timing.
- **`StaticFiles::try_handler`** for callers who would rather handle an unusable
  root than have it panic.

### Changed

- **`Router::route` now takes the request**, which guards need in order to
  decide. Breaking for code driving `Router` directly; applications on the
  routing DSL are unaffected.
- **`StaticFiles::dir(..).handler()` panics if the root is missing or is not a
  directory.** It previously returned `500` on every request, forever — a
  configuration mistake reported as a runtime fault.
- **Registering the same `(method, path)` twice panics.** It previously replaced
  the first handler silently, so a copy-paste typo produced a route that
  mysteriously did nothing.
- `HEAD` on a static file now reports the real `Content-Length`, since the
  length is known before any bytes are read.

### Deprecated

- **`Cors::permissive`** → **`Cors::allow_any_origin_insecure`**. The new name
  is deliberately uncomfortable: reflecting every origin means any site a user
  visits can read authenticated responses from an API that answers on an
  ambient credential. The old name still works and forwards to the new one.

### Security

- **Session cookies are signed with HMAC-SHA256** (`hmac` + `sha2`) rather than
  a keyed FNV digest, and verified in constant time. The previous digest was
  documented as non-cryptographic, but a weak default that is only *documented*
  as weak is still a footgun. Still signed rather than encrypted: the visitor
  can read their own session, and the docs say so.
- Cookie values are percent-encoded on the way out. The cookie-value grammar
  permits a literal `%`, but since values are decoded on read, letting one
  through made the round trip lossy — a stored `%3D` came back as `=`, which
  corrupted a session payload before its signature was checked.
- Encoded path separators (`%2F`, `%5C`) are refused by `StaticFiles`. Traversal
  was already blocked by the `..` rejection, but `%2F` silently became a real
  separator once a wildcard's segments were rejoined, which made the safety
  argument depend on reasoning about rejoining.

## [0.2.0] - 2026-07-25

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

[Unreleased]: https://github.com/davthecoder/Churust/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/davthecoder/Churust/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/davthecoder/Churust/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/davthecoder/Churust/releases/tag/v0.1.0
