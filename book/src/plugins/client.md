# HTTP client

<span class="feature-tag">feature: client</span>
<span class="feature-tag">client-tls for HTTPS</span>

## Enable

```toml
churust = { version = "0.3", features = ["client"] }
# HTTPS:
churust = { version = "0.3", features = ["client-tls"] }
```

## Overview

Outbound client built on the same **hyper** stack as the server: pooled,
bounded, with transparent gzip/deflate decode and a decompression-bomb ceiling.

```rust
use churust::client::Client; // path may be re-exported via prelude when feature is on

let client = Client::new()
    .timeout(std::time::Duration::from_secs(10))
    .user_agent("my-app/0.1")?;

let res = client
    .get("https://httpbin.org/get")
    .header("x-request-id", "demo")
    .send()
    .await?;

println!("{} {}", res.status(), res.text()?);
```

## Builder highlights

| Method | Purpose |
| --- | --- |
| `.timeout(d)` | Overall / request timeout |
| `.max_response_bytes(n)` | Response size cap |
| `.max_redirects(n)` | Redirect limit |
| `.auto_decompress(bool)` | Transparent decode |
| `.default_header` / `.user_agent` | Defaults for all requests |

## Request builder

```rust
client.post("https://api.example.com/items")
    .bearer(token)
    .json(&payload)
    .send()
    .await?;
```

Also: `.query`, `.form`, `.body`, `.put`, `.patch`, `.delete`, `.head`.

## API reference

- [docs.rs/churust-client](https://docs.rs/churust-client)
