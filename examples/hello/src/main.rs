//! # Hello — the smallest useful Churust server
//!
//! Start here. Shows the three things every app needs: a route, a typed path
//! parameter, and shared application state.
//!
//! ## Run it
//!
//! ```text
//! cargo run -p hello
//! ```
//!
//! ## Try it
//!
//! ```text
//! curl localhost:8080/                    # Churust 🌀
//! curl localhost:8080/users/7             # user #7
//! curl 'localhost:8080/greet?q=world'     # Hello, world!
//! ```
//!
//! ## In your own project
//!
//! ```toml
//! [dependencies]
//! churust = "0.2"
//! serde = { version = "1", features = ["derive"] }
//! ```
//!
//! No `tokio` entry is needed — Churust re-exports the runtime it is built on,
//! and `#[churust::main]` uses that re-export.

use churust::prelude::*;
use serde::Deserialize;

#[derive(Clone)]
struct Greeter {
    prefix: String,
}

#[derive(Deserialize)]
struct Search {
    q: String,
}

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::from_config() // loads churust.toml + env, then DSL overrides below
        .host("127.0.0.1")
        .port(8080)
        .state(Greeter {
            prefix: "Hello".into(),
        })
        .routing(|r| {
            // call-style (Plan 1 still works)
            r.get("/", |_call: Call| async { "Churust 🌀" });

            // extractor-style: path param
            r.get("/users/{id}", |Path(id): Path<u64>| async move {
                format!("user #{id}")
            });

            // extractor-style: query + state
            r.get(
                "/greet",
                |Query(s): Query<Search>, g: State<Greeter>| async move {
                    format!("{}, {}!", g.prefix, s.q)
                },
            );
        })
        .start()
        .await
}
