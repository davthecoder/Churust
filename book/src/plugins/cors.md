# CORS

<span class="feature-tag">feature: cors</span>

## Enable

```toml
churust = { version = "0.3", features = ["cors"] }
```

## Restrictive (recommended for production)

`Cors::new()` starts with **no** allowed origins, `GET`/`POST` only, no
credentials. Name the origins your browser client is actually served from:

```rust
.install(
    Cors::new()
        .allow_origin("http://localhost:3000")
        .allow_origin("https://app.example.com")
)
```

Chain builder methods for methods, headers, credentials, and `max-age` as needed
(see docs.rs).

## Development / public read-only APIs

```rust
.install(Cors::allow_any_origin_insecure())
```

The name is deliberate: reflecting **any** origin is fine for a public,
unauthenticated, read-only API and convenient locally. It is dangerous if
ambient credentials (cookies, extension-injected tokens) can authorize
requests.

> **Note:** `Access-Control-Allow-Origin: *` cannot be combined with credentials.
> Use an explicit origin list and `.allow_credentials(true)` when you need
> cookies on cross-origin calls.

## Preflight

The plugin handles CORS preflight (`OPTIONS`) according to the configured
policy.

## API reference

- [docs.rs/churust-cors](https://docs.rs/churust-cors)
