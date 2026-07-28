# Compression

<span class="feature-tag">feature: compression</span>

## Enable

```toml
churust = { version = "0.3", features = ["compression"] }
```

## Install

Negotiates `Accept-Encoding` and compresses response bodies (including streams)
with brotli, gzip, and/or deflate:

```rust
.install(
    Compression::new()
        .min_size(1024)
        .level(Level::Default)
)
```

## Options

| Method | Purpose |
| --- | --- |
| `.min_size(bytes)` | Skip small responses |
| `.level(Level)` | Compression level |
| `.encodings([...])` | Restrict encodings |
| `.compressible(f)` | Custom content-type predicate |

`compressible_by_default(content_type)` documents which media types compress by
default.

## API reference

- [docs.rs/churust-compression](https://docs.rs/churust-compression)
