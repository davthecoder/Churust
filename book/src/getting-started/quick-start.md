# Quick start

Get a Churust server running in a few minutes.

## Prerequisites

- Rust **1.96+** (`rustc --version`)
- A terminal and `cargo`

## 1. Create a project

```bash
cargo new hello-churust
cd hello-churust
```

## 2. Add Churust

```bash
cargo add churust@0.3
```

Or in `Cargo.toml`:

```toml
[dependencies]
churust = "0.3"
```

That is the whole dependency list for a minimal server. Churust re-exports the
runtime it is built on as `churust::tokio`, and `#[churust::main]` uses that
re-export — no separate `tokio` entry is required.

## 3. Write the server

Replace `src/main.rs` with:

```rust
use churust::prelude::*;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .routing(|r| {
            r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
            r.get("/users/{id}", |Path(id): Path<u64>| async move {
                format!("user #{id}")
            });
        })
        .start()
        .await
}
```

## 4. Run it

```bash
cargo run
```

In another terminal:

```bash
curl localhost:8080/
# Hello from Churust 🌀

curl localhost:8080/users/7
# user #7
```

## What just happened?

1. **`#[churust::main]`** — builds a multi-threaded tokio runtime and runs your
   async `main` (the Churust equivalent of `#[tokio::main]`).
2. **`Churust::server()`** — starts the builder for an HTTP application.
3. **`.routing(|r| { ... })`** — registers routes; path segments in `{braces}`
   become extractors.
4. **`.start().await`** — binds and serves until the process exits.

## Try the official example

From a clone of the framework:

```bash
git clone https://github.com/davthecoder/Churust.git
cd Churust
cargo run -p hello
```

That example also shows query parameters and typed app state. See
[`examples/hello`](https://github.com/davthecoder/Churust/tree/main/examples/hello).

## Next steps

- Enable plugins and transports → [Installation & features](installation.md)
- Learn extractors in depth → [Handlers & extractors](../fundamentals/handlers-extractors.md)
- Build a JSON API → [JSON API recipe](../recipes/json-api.md)
