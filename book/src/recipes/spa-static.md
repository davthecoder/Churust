# Recipe: SPA & static assets

Serve a frontend build directory and optional dynamic routes.

## Dependencies

```toml
[dependencies]
churust = { version = "0.3", features = ["fs"] }
```

## Pattern

```rust
use churust::prelude::*;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("0.0.0.0")
        .port(8080)
        .routing(|r| {
            // API routes first
            r.get("/api/health", |_c: Call| async { "ok" });

            // Then the SPA / asset root
            r.get(
                "/{path...}",
                StaticFiles::dir("./dist").index("index.html").handler(),
            );
        })
        .start()
        .await
}
```

## Notes

- Register **API routes before** the catch-all `/{path...}`.  
- `index("index.html")` covers directory and SPA entry.  
- For client-side routers that need “fallback to index.html on 404”, confirm
  whether your Churust version’s `StaticFiles` supports a fallback option; if
  not, serve `index.html` from a custom handler for unknown GET paths.  
- Path traversal is rejected; the directory you pass is the whole reachable
  tree (symlinks that escape the root are refused).

## Streaming alongside static

See [`examples/static`](https://github.com/davthecoder/Churust/tree/main/examples/static)
for `Body::from_stream` next to `StaticFiles`.
