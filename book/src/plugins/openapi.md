# OpenAPI

<span class="feature-tag">feature: openapi</span>

Implies `json`.

## Enable

```toml
churust = { version = "0.3", features = ["openapi"] }
```

## Idea

Generate an **OpenAPI 3.1** document from the router. Paths and parameters come
from routes; prose and schemas come from you. Drift is reported in **both**
directions:

- `undescribed()` — routes missing documentation  
- `stale()` — documented operations that no longer exist on the router  

## Build a document

```rust
use churust::prelude::*;
use http::Method;

let doc = OpenApi::new("Notes API", "0.1.0")
    .description("CRUD notes")
    .server("http://localhost:8080", Some("local"))
    .from_routes(&routes) // or build from your RouteBuilder export
    .operation(
        Method::POST,
        "/notes",
        Operation::new()
            .summary("Create note")
            .accepts("application/json")
            .response(201, "Created"),
    );
```

Mount the JSON document on a path:

```rust
// Inside routing, e.g.:
doc.mount(&mut r, "/openapi.json");
```

Exact mount helpers may take the document by reference — see docs.rs for the
signature on your version.

## Workflow tip

Fail CI if `undescribed()` or `stale()` is non-empty so the document cannot rot
silently.

## API reference

- [docs.rs/churust-openapi](https://docs.rs/churust-openapi)
