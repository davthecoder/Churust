//! # Static — file serving and streamed responses
//!
//! Shows the `fs` feature (`StaticFiles`) and the always-available `Body` type
//! for responses produced lazily instead of buffered in memory.
//!
//! ## Run it
//!
//! `StaticFiles` serves from `./public`, relative to where you start the
//! process, so create it first:
//!
//! ```text
//! mkdir -p public && echo '<h1>Churust</h1>' > public/index.html
//! cargo run -p static-example
//! ```
//!
//! ## Try it
//!
//! ```text
//! curl localhost:8080/                # public/index.html, via the index fallback
//! curl localhost:8080/index.html      # the same file directly
//! curl localhost:8080/numbers         # 1..5, streamed one chunk at a time
//!
//! # Traversal is rejected. --path-as-is is required, otherwise curl
//! # normalizes the ".." away before the request is ever sent.
//! curl -i --path-as-is 'localhost:8080/../Cargo.toml'   # 404
//! ```
//!
//! ## In your own project
//!
//! ```toml
//! [dependencies]
//! churust      = { version = "0.2", features = ["fs"] }
//! bytes        = "1"
//! futures-util = "0.3"
//! ```
//!
//! `bytes` and `futures-util` are needed only for the hand-built stream in
//! `/numbers`. Serving files needs `churust` alone.
//!
//! `StaticFiles` resolves symlinks and refuses anything that escapes the root,
//! so the directory you pass is the whole of what it can reach.

use churust::prelude::*;
use churust::Body;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .routing(|r| {
            // Serve files from ./public (create it with an index.html to try).
            r.get(
                "/{path...}",
                StaticFiles::dir("./public").index("index.html").handler(),
            );
            // A streamed dynamic response.
            r.get("/numbers", |_c: Call| async {
                let chunks = futures_util::stream::iter(
                    (1..=5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("{i}\n")))),
                );
                Response::stream("text/plain", Body::from_stream(chunks))
            });
        })
        .start()
        .await
}
