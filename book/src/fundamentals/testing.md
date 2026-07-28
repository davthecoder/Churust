# Testing with TestClient

Drive the full pipeline **in-process** without binding a socket.

## Basic usage

Build an `App` with `.build()` instead of `.start()`, then wrap it:

```rust
use churust::prelude::*;
use churust::TestClient;

#[churust::main]
async fn main() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/ping", |_c: Call| async { "pong" });
        })
        .build();

    let res = TestClient::new(app).get("/ping").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "pong");
}
```

In real tests, use your test harness’s async runtime (for example
`#[tokio::test]` or Churust’s main only in binaries).

## Requests

```rust
let client = TestClient::new(app);

let res = client
    .post("/notes")
    .header("authorization", "Bearer admin-token")
    .header("content-type", "application/json")
    .body(r#"{"text":"hi"}"#)
    .send()
    .await;

assert_eq!(res.status(), StatusCode::CREATED);
```

Helpers include `get`, `post`, `put`, `delete`, and a generic `request(method, uri)`.

## Assertions

```rust
res.status()
res.headers()
res.header("content-type")
res.body_bytes()
res.text()
```

## Tips

- Install the same plugins your production app uses so tests exercise CORS,
  auth, and content negotiation.
- Prefer small pure handlers plus a thin `fn app() -> App` factory shared by
  binary and tests.
- Failures at **startup** (duplicate routes, missing static root) surface when
  you call `.build()` / `.start()` — keep that in CI.

## Next

→ [Plugins overview](../plugins/overview.md)
