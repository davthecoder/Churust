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
