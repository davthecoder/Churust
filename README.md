<p align="center">
  <img src="https://raw.githubusercontent.com/davthecoder/Churust/main/img/churust_logo.png" alt="Churust logo — a churro spiral inside a gear" width="180" />
</p>

<h1 align="center">Churust 🌀</h1>

<p align="center">
  <strong>Churro + Rust</strong> — a backend web framework inspired by Kotlin's <a href="https://ktor.io">Ktor</a>.<br />
  Simple, secure, robust, easy to learn.
</p>

<p align="center">
  <a href="https://crates.io/crates/churust"><img src="https://img.shields.io/crates/v/churust.svg" alt="crates.io" /></a>
  <a href="https://docs.rs/churust"><img src="https://docs.rs/churust/badge.svg" alt="docs.rs" /></a>
  <a href="https://github.com/davthecoder/Churust/actions/workflows/ci.yml"><img src="https://github.com/davthecoder/Churust/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <img src="https://img.shields.io/badge/rust-1.96%2B-blue.svg" alt="MSRV 1.96" />
  <a href="https://github.com/davthecoder/Churust/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/churust.svg" alt="MIT" /></a>
</p>

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
churust = "0.2"
```

That is the whole dependency list. Churust re-exports the runtime it is built
on as `churust::tokio`, and `#[churust::main]` uses that re-export, so no
separate `tokio` entry is needed. Add one only if you want a tokio feature
Churust does not enable — Cargo unifies the two.

Plugins and transports are opt-in features on the umbrella crate — depend on
`churust` and enable what you need rather than pulling in the plugin crates
directly:

```toml
churust = { version = "0.2", features = ["full", "ws", "fs", "tls"] }
```

| Feature | Pulls in | Gives you |
| --- | --- | --- |
| `json` | `churust-json` | `Json<T>` extractor/responder, content negotiation |
| `logging` | `churust-logging` | `CallLogging` via `tracing` |
| `cors` | `churust-cors` | preflight + CORS headers |
| `auth` | `churust-auth` | Bearer/Basic/JWT, `Principal<P>` |
| `full` | all four above | the whole plugin set |
| `ws` | `churust-core/ws` | WebSocket upgrade + `WebSocket`/`Message` |
| `fs` | `churust-core/fs` | `StaticFiles` directory handler |
| `tls` | `churust-core/tls` | rustls-backed HTTPS |

Default features are empty, so a plain `churust = "0.2"` compiles the core
engine and nothing else.

Every Churust crate is released in lockstep on one version number, so
`churust-core`, `churust-json`, and the rest always match the umbrella version.

## Features

- **Hybrid handlers** — write call-style (`|call: Call|`) *or* with typed
  extractors (`Path<T>`, `Query<T>`, `State<T>`, `Json<T>`, `BearerToken`,
  `Principal<P>`) — mix freely in one handler.
- **`install(plugin)`** — Ktor-style plugins composed over a named pipeline
  (`Setup → Monitoring → Plugins → Call → Fallback`) for deterministic order.
- **Built-in plugins** — JSON content negotiation, request logging, CORS, and
  authentication (Bearer / Basic / JWT).
- **Typed app state / DI** — `.state(T)` then extract `State<T>`.
- **Layered config** — defaults < `churust.toml` < env (`CHURUST_*`) < code DSL.
- **Secure by default** — body-size limits, request timeouts, panic isolation
  (a panicking handler returns 500, never crashes the server), no version
  banner, opt-in rustls TLS.
- **Fast, in-process tests** — `TestClient` drives the full pipeline without
  binding a socket.

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
        .install(Cors::permissive())
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
churust = { version = "0.2", features = ["ws"] }
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
churust = { version = "0.2", features = ["fs"] }
```

## Core concepts

### Handlers & extractors
A handler is an `async` closure/fn returning anything that implements
`IntoResponse`. Arguments are **extractors**:

- `FromCallParts` (borrow the request) — any position: `Path<T>`, `Query<T>`,
  `State<T>`, `BearerToken`, `Principal<P>`.
- `FromCall` (consume the body) — last argument only: `Json<T>`, `Call`.

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

Deferred on purpose (YAGNI): sessions, response compression, HTTP/3,
route-scoped middleware sugar. Want one of them? Make the case in
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
