# Plugins & features overview

Everything below is **opt-in**. Core routing and handlers work with zero
features. Enable only what you need.

## Plugin crates (via `churust` features)

| Page | Feature | Install API |
| --- | --- | --- |
| [JSON](json.md) | `json` | `ContentNegotiation`, `Json<T>` |
| [Logging](logging.md) | `logging` | `CallLogging` |
| [CORS](cors.md) | `cors` | `Cors` |
| [Auth](auth.md) | `auth` | `Auth::bearer` / `basic` / `jwt` |
| [Rate limit](ratelimit.md) | `ratelimit` | `RateLimit` |
| [Compression](compression.md) | `compression` | `Compression` |
| [Templates](templates.md) | `templates` | `Templates` + `Renderer` |
| [Redis](redis.md) | `redis` | `RedisStore` |
| [Client](client.md) | `client` | `Client` |
| [OpenAPI](openapi.md) | `openapi` | `OpenApi` |

`features = ["full"]` enables: `json`, `logging`, `cors`, `auth`, `ratelimit`,
`compression`, `templates`.

## Transport / core features

| Page | Feature |
| --- | --- |
| [WebSockets](websockets.md) | `ws` |
| [Static files & streaming](static-files.md) | `fs` (+ always-on `Body`) |
| [Multipart](multipart.md) | `multipart` |
| [TLS](tls.md) | `tls` |
| [HTTP/3](http3.md) | `http3` |

## Pattern

```toml
churust = { version = "0.3", features = ["json", "auth"] }
```

```rust
use churust::prelude::*;

Churust::server()
    .install(/* plugins */)
    .routing(|r| { /* routes that use extractors from those plugins */ })
```

Full matrix: [Feature flags](../reference/feature-flags.md).
