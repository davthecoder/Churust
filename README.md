<p align="center">
  <img src="https://raw.githubusercontent.com/davthecoder/Churust/main/img/churust_thumb_logo.png" alt="Churust logo — a churro spiral inside a gear" width="400" />
</p>

<h1 align="center">Churust 🌀</h1>

<p align="center">
  <strong>Churro + Rust</strong> — a backend web framework inspired by Kotlin's <a href="https://ktor.io">Ktor</a>.<br />
  Simple, secure, robust, easy to learn.
</p>

<p align="center">
  <a href="https://crates.io/crates/churust"><img src="https://img.shields.io/crates/v/churust.svg" alt="crates.io" /></a>
  <a href="https://docs.rs/churust"><img src="https://docs.rs/churust/badge.svg" alt="docs.rs" /></a>
  <a href="https://davthecoder.github.io/Churust/"><img src="https://img.shields.io/badge/guide-GitHub%20Pages-orange.svg" alt="User guide" /></a>
  <a href="https://github.com/davthecoder/Churust/actions/workflows/ci.yml"><img src="https://github.com/davthecoder/Churust/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <img src="https://img.shields.io/badge/rust-1.96%2B-blue.svg" alt="MSRV 1.96" />
  <a href="https://github.com/davthecoder/Churust/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/churust.svg" alt="MIT" /></a>
</p>

**Guides:** [User documentation](https://davthecoder.github.io/Churust/) (quick start, plugins, recipes) · **API:** [docs.rs/churust](https://docs.rs/churust)

Churust gives you Ktor's developer experience in Rust: an application engine, a
routing DSL, an `install(plugin)` system, and a phased interceptor pipeline —
built on a battle-tested async stack (**tokio + hyper + rustls**). Churust owns
the ergonomic layer; it does not reinvent HTTP parsing or TLS.

```rust
use churust::prelude::*;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .port(8080)
        .routing(|r| {
            r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
            r.get("/users/{id}", |Path(id): Path<u64>| async move {
                format!("user #{id}")
            });
        })
        .start()
        .await
}
```

## Install

```toml
[dependencies]
churust = "0.3"
```

That is the whole dependency list. Churust re-exports the runtime it is built
on as `churust::tokio`, and `#[churust::main]` uses that re-export, so no
separate `tokio` entry is needed. Add one only if you want a tokio feature
Churust does not enable — Cargo unifies the two.

Plugins and transports are opt-in features on the umbrella crate — depend on
`churust` and enable what you need rather than pulling in the plugin crates
directly:

```toml
churust = { version = "0.3", features = ["full", "ws", "fs", "tls"] }
```

| Feature | Pulls in | Gives you |
| --- | --- | --- |
| `json` | `churust-json` | `Json<T>` extractor/responder, content negotiation |
| `logging` | `churust-logging` | `CallLogging` via `tracing` |
| `cors` | `churust-cors` | preflight + CORS headers |
| `auth` | `churust-auth` | Bearer/Basic/JWT, `Principal<P>` |
| `ratelimit` | `churust-ratelimit` | `RateLimit`, GCRA, keyed on the peer address (IPv6 by prefix) |
| `compression` | `churust-compression` | brotli / gzip / deflate response bodies |
| `templates` | `churust-templates` | `Templates` + `Renderer`, minijinja, HTML-escaped whatever the template is named |
| `full` | the seven above | the whole plugin set |
| `redis` | `churust-redis` | `RedisStore`: server-side sessions, revocable |
| `client` | `churust-client` | outbound HTTP client with transparent gzip/deflate decode (`client-tls` for HTTPS) |
| `openapi` | `churust-openapi` | an OpenAPI 3.1 document from the router |
| `ws` | `churust-core/ws` | WebSocket upgrade + `WebSocket`/`Message` |
| `fs` | `churust-core/fs` | `StaticFiles`, conditional GET, byte ranges |
| `multipart` | `churust-core/multipart` | `multipart/form-data`, buffered or streamed |
| `tls` | `churust-core/tls` | rustls-backed HTTPS |
| `http3` | `churust-core/http3` | HTTP/3 over QUIC (implies `tls`) |

Default features are empty, so a plain `churust = "0.3"` compiles the core
engine and nothing else.

Every Churust crate is released in lockstep on one version number, so
`churust-core`, `churust-json`, and the rest always match the umbrella version.

## Features

- **Hybrid handlers** — write call-style (`|call: Call|`) *or* with typed
  extractors (`Path<T>` — including tuples and structs — `Query<T>`, `Form<T>`,
  `Header<T, N>`, `State<T>`, `Json<T>`, `Payload`, `Either<A, B>`,
  `BearerToken`, `Principal<P>`) — mix freely in one handler. Wrap any of them
  in `Option<T>` when the input is optional; absent is `None`, malformed is
  still an error.
- **`install(plugin)`** — Ktor-style plugins composed over a named pipeline
  (`Setup → Monitoring → Plugins → Call → Fallback`) for deterministic order,
  plus `intercept` for middleware scoped to part of the route tree.
- **Route guards** — several routes may share a path when `guard::header`,
  `guard::host` or a custom predicate distinguishes them.
- **Your own error pages** — `on_error` renders any `4xx`/`5xx`, including
  routing failures.
- **Request correlation** — a request id on every log line, echoed as
  `x-request-id`, continuing an inbound W3C `traceparent`.
- **Built-in plugins** — JSON content negotiation, request logging, CORS, and
  authentication (Bearer / Basic / JWT).
- **Cookies and sessions** — a cookie primitive with safe defaults
  (`HttpOnly`, `SameSite=Lax`), and HMAC-SHA256 signed cookie sessions.
- **Streaming request bodies** — `Payload` reads an upload incrementally, so it
  is not capped by memory.
- **Peer address** — `call.peer_addr()` for rate limiting and audit logs.
- **Deployment knobs** — idle keep-alive, listen backlog, connection cap, TLS
  handshake cap and a deadline that bounds queueing for it as well as the
  handshake itself, HTTP/2 stream and header limits, a bounded shutdown grace
  period that actually drains, several bind addresses, and Unix domain sockets
  that refuse to unlink a live one.
- **File uploads** — `Multipart` buffers the whole body; `MultipartStream` reads
  fields and their content incrementally, so memory stops scaling with upload
  size. Both behind the `multipart` feature.
- **Typed app state / DI** — `.state(T)` then extract `State<T>`.
- **Layered config** — defaults < `churust.toml` < env (`CHURUST_*`) < code DSL.
- **Secure by default** — security headers on every response, including the
  `413`s, `400`s and `408`s a transport writes for itself without reaching a
  handler; body-size limits; request and header-read timeouts, the latter
  covering a connection from the moment it is accepted rather than from the
  moment a protocol is chosen, so a peer that connects and says nothing is still
  bounded (slow-loris); header and path-depth caps; WebSocket frame and message
  caps; panic isolation (a panicking handler returns 500, never crashes the
  server); no version banner; opt-in rustls TLS.
- **HTTP/1.1, HTTP/2 and HTTP/3** — h2 over TLS via ALPN, h2c by prior knowledge
  in plaintext, negotiated per connection; h3 over QUIC on its own listener
  behind the `http3` feature, with `advertise_http3` emitting the `Alt-Svc`
  header clients need in order to try it.
- **Response compression** — brotli, gzip and deflate, negotiated from
  `Accept-Encoding`, streaming bodies included (feature `compression`).
- **Rate limiting** — GCRA, so bursts are smoothed and `Retry-After` is exact
  rather than estimated. IPv6 peers are bucketed by network prefix, because a
  subscriber is handed a whole `/64` and limiting each address in it separately
  limits nobody (feature `ratelimit`).
- **Templating** — minijinja, parsed at startup, HTML-escaped whatever the
  template is called, since the rendered output is served as HTML either way
  (feature `templates`).
- **Server-side sessions** — Redis-backed, so logging out actually revokes, and
  a revocation that could not be carried out is an error rather than a cheerful
  `200` over a session that is still live (feature `redis`).
- **An HTTP client** — pooled, bounded, on the same hyper the server runs on;
  transparent gzip/deflate decode with a decompression-bomb ceiling
  (feature `client`).
- **OpenAPI 3.1** — paths and parameters from the router, prose and schemas from
  you, with drift reported in both directions (feature `openapi`).
- **Login and logout** — an identity layer over sessions with both an absolute
  and an idle deadline.
- **Correct HTTP** — automatic `HEAD` and `OPTIONS` (including `OPTIONS *`),
  one `Allow` header that both `405` and `OPTIONS` agree on, conditional GET
  (`ETag`/`Last-Modified`/`304`) and byte ranges (`206`/`416`) for static files.
- **One URL per resource** — `PathPolicy` decides what happens to `//a` and
  `/a//b`: refuse (default), `308` to the canonical form, or collapse. Silent
  collapsing makes prefix-based auth checks bypassable and cache identity
  ambiguous.
- **Ambiguity is refused, not guessed** — a request carrying both
  `Transfer-Encoding` and `Content-Length` never keeps its connection,
  whichever order the two headers arrive in, so no leftover byte becomes the
  next request behind a proxy that read the length differently. This is why
  hyper `1.11` is a floor rather than a preference: earlier versions delete the
  `Content-Length` while parsing and leave the connection open, and no check
  above hyper can see what was deleted. A repeated query key on a scalar field
  is an error rather than a silent first-or-last-wins.
- **Borrow the ecosystem** — the `tower` feature runs any `tower::Service` as
  middleware, so `tower-http`'s layers work without Churust reimplementing
  them.
- **Errors that carry** — implement `IntoError` on your own error type and `?`
  works in handlers, without leaking its `Display` text to clients.
- **Hard to misuse** — a missing static root or a duplicate route fails at
  startup rather than silently at runtime; `secure_compare` for secrets.
- **Fast, in-process tests** — `TestClient` drives the full pipeline without
  binding a socket.

## Performance

<p align="center">
  <img src="https://raw.githubusercontent.com/davthecoder/Churust/main/docs/assets/benchmark-throughput.svg" alt="Requests per second under ordinary keep-alive HTTP/1.1: Churust 691k, actix-web 654k, axum 395k, Ktor 303k, Go net/http 289k" width="900" />
</p>

Five servers, three routes, byte-identical responses, on a Linux kernel. One
server runs at a time, the order rotates each round, the server is pinned to
eight CPUs and the load generator to four, and [the harness](benchmarks/)
refuses to measure at all unless every app returns the same bytes. Median of
nine rounds — fewer is not enough to tell a real difference from this machine's
run-to-run spread, which is a lesson the harness README records the hard way.

| framework | req/s | vs. Churust | server CPU µs/req | p99 latency |
|---|---:|---:|---:|---:|
| **Churust** | **757,930** | — | 8.39 | 246 µs |
| actix-web 4.14 | 644,067 | 0.85× | **7.91** | **240 µs** |
| axum 0.8 | 464,042 | 0.61× | 8.82 | 292 µs |
| Ktor 3.5 (Netty) | 315,978 | 0.42× | 20.03 | 1.72 ms |
| Go 1.26 `net/http` | 315,160 | 0.42× | 12.88 | 2.49 ms |

**Churust is first on throughput** for this shape of load — 1.18× actix-web,
1.63× axum, 2.40× Ktor and Go — and that one is solid: across nine rounds its
slowest (711k) still beat actix-web's fastest (659k). Four things a reader
deserves before quoting any of it:

- **actix-web spends 6% less CPU per request** — 7.91 µs against 8.39. Churust
  does not lead that column, and buys its throughput with the difference.
- **Tail latency is a tie, not a win.** 246 µs against actix-web's 240 µs, with
  the two overlapping round for round. Both clear axum's 292 µs and both are an
  order of magnitude ahead of Ktor and Go. Do not read the ordering of the first
  two as a result.
- **`App::run_sharded` is what buys the throughput.** Pinning a connection to one
  runtime for its life is worth 1.94× against the shared runtime — 391k to 758k — measured, [both rows](benchmarks/results/2026-08-01-linux-docker.md).
  That is exactly why it is opt-in and the shared work-stealing runtime is still
  the default.
- **With HTTP/1.1 pipelining the order changes:** actix-web 5.96M against
  Churust's 4.45M. That gap is not in Churust's own code — its whole dispatch
  layer costs 393 ns of the 1.70 µs a pipelined request spends — it is in the
  HTTP/1 implementation underneath, and closing it would mean not using hyper.
- **Every route returns a constant.** This is dispatch overhead. It says nothing
  about an application that talks to a database.

Run it yourself — it needs nothing but Docker:

```sh
docker build -f benchmarks/Dockerfile -t churust-bench . && docker run --rm churust-bench
```

Full method, every caveat, and the numbers behind the charts:
[`benchmarks/README.md`](benchmarks/README.md) ·
[`benchmarks/results/`](benchmarks/results/)

## Workspace layout

| Crate | Docs | What it is |
|-------|------|------------|
| `churust` | [docs.rs](https://docs.rs/churust) | Umbrella crate + `prelude`. Depend on this. Plugins behind features. |
| `churust-core` | [docs.rs](https://docs.rs/churust-core) | Engine, routing, pipeline, `Call`, extractors, config, state, TLS, WebSockets, static files, test harness. |
| `churust-macros` | [docs.rs](https://docs.rs/churust-macros) | `#[churust::main]`. |
| `churust-json` | [docs.rs](https://docs.rs/churust-json) | `Json<T>` extractor/responder + `ContentNegotiation` plugin (feature `json`). |
| `churust-logging` | [docs.rs](https://docs.rs/churust-logging) | `CallLogging` plugin via `tracing` (feature `logging`). |
| `churust-cors` | [docs.rs](https://docs.rs/churust-cors) | `Cors` plugin — preflight + headers (feature `cors`). |
| `churust-auth` | [docs.rs](https://docs.rs/churust-auth) | `Auth` (Bearer/Basic/JWT) + `Principal<P>` (feature `auth`). |
| `churust-ratelimit` | [docs.rs](https://docs.rs/churust-ratelimit) | `RateLimit` plugin, GCRA (feature `ratelimit`). |
| `churust-compression` | [docs.rs](https://docs.rs/churust-compression) | `Compression` plugin — brotli/gzip/deflate (feature `compression`). |
| `churust-templates` | [docs.rs](https://docs.rs/churust-templates) | `Templates` + `Renderer` over minijinja (feature `templates`). |
| `churust-redis` | [docs.rs](https://docs.rs/churust-redis) | `RedisStore`, a revocable server-side `SessionStore` (feature `redis`). |
| `churust-client` | [docs.rs](https://docs.rs/churust-client) | Outbound HTTP client (feature `client`, HTTPS via `client-tls`). |
| `churust-openapi` | [docs.rs](https://docs.rs/churust-openapi) | OpenAPI 3.1 document generation (feature `openapi`). |
| `churust-lab` | [docs.rs](https://docs.rs/churust-lab) | Incubator. **Never reaches 1.0**; expect breaking changes on most releases. |

Runnable examples:

| Example | Run it | Shows |
|---------|--------|-------|
| [`examples/hello`](https://github.com/davthecoder/Churust/tree/main/examples/hello) | `cargo run -p hello` | Minimal server, path params. |
| [`examples/api`](https://github.com/davthecoder/Churust/tree/main/examples/api) | `cargo run -p api` | JSON CRUD with all four plugins + auth-gated routes. |
| [`examples/chat`](https://github.com/davthecoder/Churust/tree/main/examples/chat) | `cargo run -p chat` | WebSocket echo endpoint and a broadcast room. |
| [`examples/static`](https://github.com/davthecoder/Churust/tree/main/examples/static) | `cargo run -p static-example` | `StaticFiles` plus a streamed response body. |

## A fuller example

```rust
use churust::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
struct Note { id: u64, text: String }
#[derive(Deserialize)]
struct NewNote { text: String }
#[derive(Clone)]
struct Admin { name: String }
struct Store { notes: Mutex<Vec<Note>> }

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .state(Store { notes: Mutex::new(Vec::new()) })
        .install(CallLogging::new())
        .install(ContentNegotiation::new())  // renders errors as JSON
        .install(Cors::new().allow_origin("http://localhost:3000"))
        .install(Auth::bearer(|token: String| async move {
            (token == "admin-token").then(|| Admin { name: "admin".into() })
        }))
        .routing(|r| {
            r.get("/notes", |s: State<Store>| async move {
                Json(s.notes.lock().unwrap().clone())
            });
            // Asking for `Principal<Admin>` enforces auth (401 otherwise).
            r.post("/notes", |Principal(_a): Principal<Admin>,
                              s: State<Store>,
                              Json(input): Json<NewNote>| async move {
                let mut notes = s.notes.lock().unwrap();
                let note = Note { id: notes.len() as u64 + 1, text: input.text };
                notes.push(note.clone());
                (StatusCode::CREATED, Json(note))
            });
        })
        .start()
        .await
}
```

```
$ curl localhost:8080/notes
[]
$ curl -X POST localhost:8080/notes -d '{"text":"hi"}'
{"error":"authentication required","status":401}
$ curl -X POST localhost:8080/notes -H 'authorization: Bearer admin-token' \
       -H 'content-type: application/json' -d '{"text":"hi"}'
{"id":1,"text":"hi"}
```

## WebSockets (feature `ws`)

Opt in with `features = ["ws"]`. A handler takes the `WebSocketUpgrade`
extractor and calls `on_upgrade` to switch the request into a bidirectional
socket (a plain GET to the route is rejected with `426 Upgrade Required`):

```rust
use churust::prelude::*;
use churust::ws::{Message, WebSocketUpgrade};

r.get("/echo", |ws: WebSocketUpgrade| async move {
    ws.on_upgrade(|mut sock| async move {
        while let Some(Ok(msg)) = sock.recv().await {
            if matches!(msg, Message::Close) || sock.send(msg).await.is_err() {
                break;
            }
        }
    })
});
```

```toml
churust = { version = "0.3", features = ["ws"] }
```

See [`examples/chat`](https://github.com/davthecoder/Churust/tree/main/examples/chat) for an echo endpoint plus a broadcast room.

## Static files & streaming (feature `fs`)

Response bodies are a `Body` — either buffered bytes or a lazy stream — so large
or dynamic payloads never have to be fully materialized in memory. `Body` is
always available; `StaticFiles` is behind the opt-in `fs` feature.

```rust
use churust::prelude::*;        // brings StaticFiles into scope under `fs`
use churust::Body;

r.get(
    "/{path...}",
    StaticFiles::dir("./public").index("index.html").handler(),
);
r.get("/numbers", |_c: Call| async {
    let chunks = futures_util::stream::iter(
        (1..=5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("{i}\n")))),
    );
    Response::stream("text/plain", Body::from_stream(chunks))
});
```

`StaticFiles` detects the `Content-Type` from the file extension, serves an
optional `index` file for directories, rejects path traversal (`..`, absolute
paths, symlink escapes) with `404`, and streams the file in chunks. See
[`examples/static`](https://github.com/davthecoder/Churust/tree/main/examples/static).

```toml
churust = { version = "0.3", features = ["fs"] }
```

## Core concepts

### Handlers & extractors
A handler is an `async` closure/fn returning anything that implements
`IntoResponse`. Arguments are **extractors**:

- `FromCallParts` (borrow the request head) — any position: `Path<T>`,
  `Query<T>`, `Header<T, N>`, `State<T>`, `BearerToken`, `Principal<P>`.
- `FromCall` (consume the body) — last argument only: `Json<T>`, `Form<T>`,
  `Bytes`, `String`, `Payload`, `Multipart`, `Either<A, B>`, `Call`.

The split is enforced by the compiler, not by convention: the body is a
one-shot stream, so two body-consuming arguments do not compile. Every
`FromCallParts` is also usable last.

`Option<T>` works for any extractor implementing `OptionalFromCallParts`
(`Query`, `Path`, `Header`, `BearerToken`). **Absent yields `None`; malformed is
still an error** — so a typo'd query string does not quietly become a default.

`Call` itself is the call-style base case (`|call: Call| ...`).

### Pipeline & plugins
A `Plugin` registers `Middleware` into a `Phase`. Middleware own the `Call`,
may mutate it, call `next.run(call)`, and post-process the `Response` (the onion
model). Phase order is fixed and deterministic regardless of install order.

### Configuration
```toml
# churust.toml
[server]
host = "0.0.0.0"
port = 8080
max_body_bytes = 1048576
request_timeout_ms = 30000
keep_alive_ms = 75000          # idle connections; 0 answers and closes
max_connections = 25000        # connections served at once; 0 is unlimited
max_tls_handshakes = 256       # much smaller: a handshake is asymmetric work
tls_handshake_timeout_ms = 10000
shutdown_timeout_ms = 30000    # bounded drain, so exit is never held hostage
path_policy = "strict"         # strict | redirect | collapse

[tls]            # requires the `tls` feature
cert = "cert.pem"
key  = "key.pem"
```
`Churust::from_config()` loads `churust.toml` + `CHURUST_*` env vars; chained
DSL setters override. Example: `CHURUST_SERVER_PORT=9090`.

### Testing
```rust
use churust::TestClient;

let app = build_app();
let client = TestClient::new(app);
let res = client.get("/users/1").send().await;
assert_eq!(res.status(), StatusCode::OK);
```

## Development

Requires Rust **1.96+** (the MSRV, and what CI pins).

```bash
git clone https://github.com/davthecoder/Churust.git
cd Churust
cargo test --workspace
```

Warnings are errors in CI. Before opening a PR, run the full gate — the exact
command list is in [CONTRIBUTING.md](CONTRIBUTING.md#before-you-open-a-pr), and
it covers `fmt`, the clippy feature matrix, the test feature matrix, the
examples, and a docs build.

## Status

Published on crates.io — the badge above carries the current version. Pre-1.0,
so the API is settling rather than settled: expect breaking changes in minor
releases until 1.0.

Everything documented above works today:

- **Core** — routing, hybrid extractors, the four plugins, named phases, layered
  config, typed state, timeouts, TLS, `#[churust::main]`
- **WebSockets** — opt-in `ws` feature
- **Streaming bodies** — the always-on `Body` type
- **Static files** — `StaticFiles`, opt-in `fs` feature

Also working, each behind its own feature: response compression, rate limiting,
templating, HTTP/3 over QUIC, Redis-backed sessions, a login/logout layer, an
outbound HTTP client, OpenAPI generation, and a streaming multipart parser.
Several of those were listed as deliberate non-goals until 2026-07-25; the
[production-readiness audit](https://github.com/davthecoder/Churust/blob/main/docs/design/2026-07-25-production-readiness-audit.md)
records what the original objection was and what the implementation does about
it, because "we changed our mind" is more useful to a reader than a silently
edited list.

**Still not supported, on purpose.** Actor integration.
`multipart/byteranges` for multi-range requests, which RFC 9110 permits omitting.
WebSockets over HTTP/3, which upgrade through Extended CONNECT (RFC 9220) rather
than the HTTP/1.1 handshake the `ws` feature implements. Revocation from a
client-side session store, which has no server-side record to withdraw — that is
what `churust-redis` is for. And `max_body_bytes` still bounds every request:
streaming changed what an upload costs in memory, not what it is allowed to
weigh.

Those are tracked and ordered in
[`docs/design/2026-07-25-roadmap-to-parity.md`](https://github.com/davthecoder/Churust/blob/main/docs/design/2026-07-25-roadmap-to-parity.md),
which also says which are deliberate scope choices and which are simply not
built yet. Want one sooner? Make the case in
[Discussions](https://github.com/davthecoder/Churust/discussions/categories/ideas).

Design specs live in
[`docs/design/`](https://github.com/davthecoder/Churust/tree/main/docs/design)
and implementation plans in
[`docs/plans/`](https://github.com/davthecoder/Churust/tree/main/docs/plans).
Those documents label work as v1 / v2.0 / v2.1 — those are internal build
milestones, not published versions.

## Contributing

Contributions are welcome — read [CONTRIBUTING.md](CONTRIBUTING.md) first. The
short version: Churust keeps a narrow scope, tests come first, and anything
optional is feature-gated so default builds never change.

| | |
| --- | --- |
| Ask a question | [Discussions → Q&A](https://github.com/davthecoder/Churust/discussions/categories/q-a) |
| Propose a feature | [Discussions → Ideas](https://github.com/davthecoder/Churust/discussions/categories/ideas) |
| Report a bug | [Issues](https://github.com/davthecoder/Churust/issues/new/choose) |
| Report a vulnerability | [SECURITY.md](SECURITY.md) — privately, never a public issue |
| Get help | [SUPPORT.md](SUPPORT.md) |
| Cut a release (maintainers) | [RELEASING.md](RELEASING.md) |

Everyone taking part is held to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Sponsor

Churust is built and maintained in spare time. If it saves you some, you can
support the work through [GitHub Sponsors](https://github.com/sponsors/davthecodercom).

Sponsorship funds maintenance — issue triage, security response, keeping up with
tokio/hyper/rustls releases — not feature bounties. Features are decided on
merit in [Discussions](https://github.com/davthecoder/Churust/discussions),
and that stays true regardless of who is sponsoring.

## License

MIT — see [LICENSE](LICENSE).
