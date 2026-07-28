# Feature flags

All features on the `churust` crate. **Default features are empty.**

## Plugins

| Feature | Crate | Notes |
| --- | --- | --- |
| `json` | `churust-json` | `Json<T>`, `ContentNegotiation` |
| `logging` | `churust-logging` | `CallLogging` |
| `cors` | `churust-cors` | `Cors` |
| `auth` | `churust-auth` | `Auth`, `Principal<P>` |
| `ratelimit` | `churust-ratelimit` | `RateLimit` (GCRA) |
| `compression` | `churust-compression` | brotli / gzip / deflate |
| `templates` | `churust-templates` | minijinja templates |
| `full` | *(meta)* | All seven plugins above |

## Data & docs

| Feature | Crate | Notes |
| --- | --- | --- |
| `redis` | `churust-redis` | `RedisStore` sessions |
| `client` | `churust-client` | Outbound HTTP |
| `client-tls` | `churust-client` | HTTPS for the client (enables `client`) |
| `openapi` | `churust-openapi` | OpenAPI 3.1 (enables `json`) |

## Transports

| Feature | Enables | Notes |
| --- | --- | --- |
| `ws` | `churust-core/ws` | WebSockets |
| `fs` | `churust-core/fs` | `StaticFiles` |
| `multipart` | `churust-core/multipart` | Multipart parsers |
| `tls` | `churust-core/tls` | rustls HTTPS |
| `http3` | `churust-core/http3` | QUIC / HTTP/3 (implies TLS stack) |

## Examples

```toml
churust = "0.3"
churust = { version = "0.3", features = ["full"] }
churust = { version = "0.3", features = ["full", "ws", "fs", "tls"] }
churust = { version = "0.3", features = ["client-tls", "openapi"] }
```

Lockstep versioning: every `churust-*` crate shares the umbrella version.
