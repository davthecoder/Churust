# Redis sessions

<span class="feature-tag">feature: redis</span>

## Enable

```toml
churust = { version = "0.3", features = ["redis"] }
```

## Why Redis?

Cookie-only sessions cannot be **revoked** from the server without a shared
store. `RedisStore` is a revocable server-side `SessionStore`: logging out
actually invalidates the session, and a revocation that could not be carried
out is an **error** rather than a cheerful `200` over a session that is still
live.

## Construct

```rust
use churust::prelude::*;
// Redis client type comes from the redis crate dependency of churust-redis

let client = redis::Client::open("redis://127.0.0.1/")?;
let store = RedisStore::from_client(client)
    .ttl(3600)
    .prefix("churust:sess:")
    .sliding(true);
```

| Method | Purpose |
| --- | --- |
| `.ttl(secs)` | Session time-to-live |
| `.prefix(...)` | Key prefix in Redis |
| `.sliding(bool)` | Sliding expiration on activity |

Wire the store into your session / login configuration as documented on
docs.rs for the version you ship.

## API reference

- [docs.rs/churust-redis](https://docs.rs/churust-redis)
