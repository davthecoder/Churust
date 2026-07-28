# Multipart uploads

<span class="feature-tag">feature: multipart</span>

## Enable

```toml
churust = { version = "0.3", features = ["multipart"] }
```

## Two APIs

| Type | Behavior |
| --- | --- |
| `Multipart` | Buffers the whole body |
| `MultipartStream` | Reads fields and content **incrementally** so memory does not scale with upload size |

Both parse `multipart/form-data`. Prefer the streaming API for large uploads.

## Body size still applies

Streaming changed what an upload **costs in memory**, not what it is **allowed
to weigh**. `max_body_bytes` still bounds every request.

## Handler sketch

```rust
// Buffered
r.post("/upload", |form: Multipart| async move {
    // iterate fields...
    StatusCode::NO_CONTENT
});

// Streamed — keep MultipartStream as the body extractor (last arg)
// r.post("/upload", |stream: MultipartStream| async move { ... });
```

Check docs.rs for the exact field iteration APIs on your version.

## Non-goal

`multipart/byteranges` for multi-range requests is intentionally unsupported
(RFC 9110 permits omitting it). See [non-goals](../reference/non-goals.md).

## API reference

- [docs.rs/churust](https://docs.rs/churust) / `churust-core` multipart module
