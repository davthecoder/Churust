# Responses & errors

## Building responses

Handlers return types that implement `IntoResponse`:

```rust
// 200 text/plain-ish defaults for strings
r.get("/ping", |_c: Call| async { "pong" });

// Explicit status + body
r.post("/items", |_c: Call| async {
    (StatusCode::CREATED, "created")
});

// JSON (feature `json`)
r.get("/note", |_c: Call| async {
    Json(serde_json::json!({ "ok": true }))
});
```

### Streaming

`Body` is always available (no feature flag). Stream large or dynamic payloads
without buffering everything in memory:

```rust
use churust::Body;

r.get("/numbers", |_c: Call| async {
    let chunks = futures_util::stream::iter(
        (1..=5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("{i}\n")))),
    );
    Response::stream("text/plain", Body::from_stream(chunks))
});
```

## Custom error pages

Use `on_error` on the app builder to render any `4xx` / `5xx`, including routing
failures, with your own HTML or JSON.

With `ContentNegotiation` installed (feature `json`), errors can be rendered as
JSON for API clients.

## `IntoError`

Implement `IntoError` on your domain error type so handlers can use `?`. Churust
maps the error to an HTTP status and a safe client-facing body without exposing
internal details from `Display` by default.

## Transport-level errors

Security headers and limits apply even when a handler never runs — for example
`413` (body too large), `400`, and `408` produced by the transport. See
[Security defaults](../reference/security.md).

## Correct HTTP semantics

- Conditional GET for static files (`ETag` / `Last-Modified` / `304`)
- Byte ranges (`206` / `416`) for static files
- Ambiguous framing (both `Transfer-Encoding` and `Content-Length`) is refused
  so leftover bytes cannot become the next request behind a proxy
- Repeated query keys on a scalar field are an error, not silent first/last wins

## Next

→ [Pipeline & plugins](pipeline-plugins.md)
