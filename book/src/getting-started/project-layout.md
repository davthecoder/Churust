# Project layout

## Your application (typical)

```text
my-api/
├── Cargo.toml
├── churust.toml          # optional layered config
├── src/
│   └── main.rs
└── public/               # if you serve static files
    └── index.html
```

Minimal `Cargo.toml`:

```toml
[package]
name = "my-api"
version = "0.1.0"
edition = "2021"

[dependencies]
churust = { version = "0.3", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

## Workspace crates (the framework itself)

If you browse or contribute to Churust:

| Crate | Role |
| --- | --- |
| `churust` | Umbrella + `prelude`. **Depend on this.** |
| `churust-core` | Engine, routing, pipeline, extractors, config, TLS, WS, static, tests |
| `churust-macros` | `#[churust::main]` |
| `churust-json` | JSON + content negotiation |
| `churust-logging` | Request logging |
| `churust-cors` | CORS |
| `churust-auth` | Auth + `Principal` |
| `churust-ratelimit` | Rate limiting |
| `churust-compression` | Response compression |
| `churust-templates` | HTML templates |
| `churust-redis` | Redis session store |
| `churust-client` | Outbound HTTP client |
| `churust-openapi` | OpenAPI generation |
| `churust-lab` | Incubator — **never reaches 1.0**; expect breaks |

Runnable examples live under `examples/`:

| Example | Command | Shows |
| --- | --- | --- |
| `hello` | `cargo run -p hello` | Routes, path/query, state |
| `api` | `cargo run -p api` | JSON + plugins + auth |
| `chat` | `cargo run -p chat` | WebSockets |
| `static` | `cargo run -p static-example` | Static files + streamed body |

## Where docs live

| Path | Audience |
| --- | --- |
| `book/` (this site) | Users learning the framework |
| [docs.rs](https://docs.rs/churust) | API reference |
| `docs/design/`, `docs/plans/` | Maintainers / design history |

## Next

→ [Routing & guards](../fundamentals/routing.md)
