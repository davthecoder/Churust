# JSON & content negotiation

<span class="feature-tag">feature: json</span>

## Enable

```toml
churust = { version = "0.3", features = ["json"] }
```

## `Json<T>`

Extractor (request body) and responder (response body):

```rust
use churust::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct NewNote { text: String }

#[derive(Serialize)]
struct Note { id: u64, text: String }

r.post("/notes", |Json(input): Json<NewNote>| async move {
    let note = Note { id: 1, text: input.text };
    (StatusCode::CREATED, Json(note))
});
```

`Json<T>` consumes the body — use it as the **last** handler argument (or only
body consumer).

## `ContentNegotiation`

Install so errors and negotiated responses can render as JSON for API clients:

```rust
.install(ContentNegotiation::new())
```

## With auth (typed principal)

```rust
r.post(
    "/notes",
    |Principal(_a): Principal<Admin>,
     Json(input): Json<NewNote>| async move {
        Json(input)
    },
);
```

Requires `auth` as well as `json`. Full walkthrough:
[JSON API recipe](../recipes/json-api.md).

## API reference

- [docs.rs/churust-json](https://docs.rs/churust-json)
