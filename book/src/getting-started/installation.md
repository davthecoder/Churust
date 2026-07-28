# Installation & features

## Depend on the umbrella crate

```toml
[dependencies]
churust = "0.3"
```

Plugins and transports are **opt-in features** on `churust`. Prefer enabling
features on the umbrella crate rather than depending on plugin crates directly:

```toml
churust = { version = "0.3", features = ["full", "ws", "fs", "tls"] }
```

Default features are empty: plain `churust = "0.3"` compiles the core engine and
nothing else.

Every Churust crate is released in **lockstep** on one version number, so
`churust-core`, `churust-json`, and the rest always match the umbrella version.

## Feature matrix

| Feature | Pulls in | Gives you |
| --- | --- | --- |
| `json` | `churust-json` | `Json<T>` extractor/responder, content negotiation |
| `logging` | `churust-logging` | `CallLogging` via `tracing` |
| `cors` | `churust-cors` | Preflight + CORS headers |
| `auth` | `churust-auth` | Bearer / Basic / JWT, `Principal<P>` |
| `ratelimit` | `churust-ratelimit` | `RateLimit`, GCRA, keyed on the peer address |
| `compression` | `churust-compression` | brotli / gzip / deflate response bodies |
| `templates` | `churust-templates` | `Templates` + `Renderer` (minijinja) |
| `full` | the seven above | The whole plugin set |
| `redis` | `churust-redis` | `RedisStore`: server-side sessions, revocable |
| `client` | `churust-client` | Outbound HTTP client (`client-tls` for HTTPS) |
| `openapi` | `churust-openapi` | OpenAPI 3.1 document from the router (implies `json`) |
| `ws` | `churust-core/ws` | WebSocket upgrade + `WebSocket` / `Message` |
| `fs` | `churust-core/fs` | `StaticFiles`, conditional GET, byte ranges |
| `multipart` | `churust-core/multipart` | `multipart/form-data`, buffered or streamed |
| `tls` | `churust-core/tls` | rustls-backed HTTPS |
| `http3` | `churust-core/http3` | HTTP/3 over QUIC (implies `tls`) |

`full` enables the **plugin** set. Transports (`ws`, `fs`, `tls`, `http3`,
`multipart`) stay opt-in individually — each is a protocol surface, not only an
ergonomic helper.

## Common combinations

```toml
# JSON API with auth and logging
churust = { version = "0.3", features = ["json", "logging", "cors", "auth"] }
# same as:
churust = { version = "0.3", features = ["full"] }

# Real-time chat
churust = { version = "0.3", features = ["ws"] }

# Static site + streaming
churust = { version = "0.3", features = ["fs"] }

# Production HTTPS
churust = { version = "0.3", features = ["tls"] }
```

## Serde

Typed query/path/JSON bodies need Serde where you define types:

```toml
serde = { version = "1", features = ["derive"] }
```

## Tokio

Usually **do not** add `tokio` yourself. Use `churust::tokio` for channels,
timers, and `select!`. If you need a tokio feature Churust does not enable, add
`tokio` with that feature — Cargo unifies the two.

## MSRV

Churust requires **Rust 1.96+**. CI pins that toolchain.

## API reference

- [docs.rs/churust](https://docs.rs/churust)
- [Feature flags reference](../reference/feature-flags.md)
- [Crate map](../reference/crate-map.md)
