# Rate limiting

<span class="feature-tag">feature: ratelimit</span>

## Enable

```toml
churust = { version = "0.3", features = ["ratelimit"] }
```

## Install

GCRA-based limiting — bursts are smoothed and `Retry-After` is exact rather than
estimated:

```rust
use std::time::Duration;

.install(
    RateLimit::per_minute(60)
        .burst(10)
)
```

Helpers: `RateLimit::per_second`, `per_minute`, `per_hour`, or
`RateLimit::per(limit, period)`.

## Keying

By default keys use the **peer address**. IPv6 peers are bucketed by network
prefix (typically `/64`), because a subscriber is handed a whole prefix and
limiting each address separately limits nobody.

Custom keys (return `None` to skip limiting for that request):

```rust
.install(
    RateLimit::per_minute(60).by(|call| {
        // e.g. API key header
        call.header("x-api-key").map(str::to_owned)
    })
)
```

## Tuning

| Method | Purpose |
| --- | --- |
| `.burst(n)` | Burst capacity |
| `.by(f)` | Custom key function |
| `.max_keys(n)` | Cap on tracked keys |

## API reference

- [docs.rs/churust-ratelimit](https://docs.rs/churust-ratelimit)
