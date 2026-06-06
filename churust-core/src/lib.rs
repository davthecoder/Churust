//! Churust core kernel: the engine, routing, request pipeline, and extractors
//! that power the [Churust](https://crates.io/crates/churust) web framework.
//!
//! Churust is a Ktor-inspired, async-first web framework for Rust. This crate
//! (`churust-core`) is the foundation every other Churust crate builds on. It
//! provides:
//!
//! - A fluent [`Churust::server`] builder ([`AppBuilder`]) for assembling an
//!   [`App`] from routes, shared state, middleware, and configuration.
//! - A trie-based [`Router`] supporting static segments, `{param}` captures, and
//!   trailing `{name...}` wildcards.
//! - The per-request [`Call`] context — the single object every handler receives.
//! - An onion-style middleware [pipeline] ordered by [`Phase`].
//! - Type-safe [extractors](crate::extract) ([`Path`], [`Query`], [`State`],
//!   [`BearerToken`]) plus the [`FromCall`]/[`FromCallParts`] traits that let
//!   handlers take typed arguments.
//! - A flexible [`Response`]/[`IntoResponse`] model and a status-carrying
//!   [`Error`] type.
//! - Layered [`Config`] loading (defaults < `churust.toml` < `CHURUST_*` env <
//!   code) and optional TLS (feature `tls`).
//! - An in-process [`TestClient`] for fast, socket-free integration tests.
//!
//! # Example
//!
//! Build an app, register a route, and exercise it with the in-process test
//! client (no socket is bound, so this runs in any environment):
//!
//! ```
//! use churust_core::{Churust, Call, TestClient};
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "Hello, Churust!" });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app).get("/").send().await;
//! assert_eq!(res.status().as_u16(), 200);
//! assert_eq!(res.text(), "Hello, Churust!");
//! # });
//! ```
//!
//! To actually serve traffic, call [`App::start`] (binds a socket and serves
//! until Ctrl-C) or [`AppBuilder::start`].

#![deny(missing_docs)]

pub mod error;
pub use error::{Error, Result};

pub mod response;
pub use response::{IntoResponse, Response};

pub mod call;
pub use call::Call;

pub mod handler;
pub use handler::{boxed, BoxHandler, Handler, IntoHandler};

pub mod state;
pub use state::StateMap;

pub mod extract;
pub use extract::{BearerToken, FromCall, FromCallParts, Path, Query, State};

pub mod router;
pub use router::{Match, RouteBuilder, Router};

pub mod pipeline;
pub use pipeline::{Endpoint, Middleware, Next, Phase};

pub mod config;
pub use config::{Config, ServerSection, TlsSection};

pub mod app;
pub use app::{App, AppBuilder, Churust, Plugin, ServerConfig};

pub mod engine;

#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub use ws::{WebSocket, WebSocketUpgrade};

pub mod test;
pub use test::{TestClient, TestRequest, TestResponse};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
