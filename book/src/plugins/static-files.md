# Static files & streaming

<span class="feature-tag">feature: fs</span> for `StaticFiles` ·
`Body` is always available

## Enable static files

```toml
churust = { version = "0.3", features = ["fs"] }
```

## Serve a directory

```rust
use churust::prelude::*;

r.get(
    "/{path...}",
    StaticFiles::dir("./public").index("index.html").handler(),
);
```

Behavior:

- `Content-Type` from file extension  
- Optional directory `index`  
- Path traversal (`..`, absolute paths, symlink escapes) → **404**  
- Streams file content in chunks  
- Conditional GET (`ETag` / `Last-Modified` / `304`) and byte ranges (`206` / `416`)

```bash
mkdir -p public && echo '<h1>Churust</h1>' > public/index.html
cargo run -p static-example
curl localhost:8080/
# Traversal rejected (curl otherwise normalizes ".." away):
curl -i --path-as-is 'localhost:8080/../Cargo.toml'   # 404
```

A missing static root fails at **startup**, not silently at runtime.

## Streaming dynamic bodies

No feature flag:

```rust
use churust::Body;

r.get("/numbers", |_c: Call| async {
    let chunks = futures_util::stream::iter(
        (1..=5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("{i}\n")))),
    );
    Response::stream("text/plain", Body::from_stream(chunks))
});
```

## Example

[`examples/static`](https://github.com/davthecoder/Churust/tree/main/examples/static) ·
[SPA recipe](../recipes/spa-static.md)
