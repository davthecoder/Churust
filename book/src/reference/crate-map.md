# Crate map

| Crate | Docs | What it is |
| --- | --- | --- |
| `churust` | [docs.rs](https://docs.rs/churust) | Umbrella + `prelude`. **Depend on this.** Plugins behind features. |
| `churust-core` | [docs.rs](https://docs.rs/churust-core) | Engine, routing, pipeline, `Call`, extractors, config, state, TLS, WebSockets, static files, test harness |
| `churust-macros` | [docs.rs](https://docs.rs/churust-macros) | `#[churust::main]` |
| `churust-json` | [docs.rs](https://docs.rs/churust-json) | `Json<T>` + `ContentNegotiation` |
| `churust-logging` | [docs.rs](https://docs.rs/churust-logging) | `CallLogging` |
| `churust-cors` | [docs.rs](https://docs.rs/churust-cors) | `Cors` |
| `churust-auth` | [docs.rs](https://docs.rs/churust-auth) | `Auth` + `Principal` |
| `churust-ratelimit` | [docs.rs](https://docs.rs/churust-ratelimit) | `RateLimit` |
| `churust-compression` | [docs.rs](https://docs.rs/churust-compression) | `Compression` |
| `churust-templates` | [docs.rs](https://docs.rs/churust-templates) | `Templates` + `Renderer` |
| `churust-redis` | [docs.rs](https://docs.rs/churust-redis) | `RedisStore` |
| `churust-client` | [docs.rs](https://docs.rs/churust-client) | Outbound HTTP client |
| `churust-openapi` | [docs.rs](https://docs.rs/churust-openapi) | OpenAPI 3.1 generation |
| `churust-lab` | [docs.rs](https://docs.rs/churust-lab) | Incubator. **Never reaches 1.0.** |

## Dependency graph (conceptual)

```text
your-app
   └── churust (umbrella)
         ├── churust-core
         ├── churust-macros
         └── optional plugin crates (features)
```

Prefer `churust = { features = [...] }` over depending on plugin crates
directly so versions stay aligned.
