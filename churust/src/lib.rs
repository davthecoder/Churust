//! # Churust 🌀
//!
//! A Ktor-inspired, secure, easy-to-learn web framework for Rust
//! (**Churro + Rust**).
//!
//! Churust gives you Ktor's developer experience on a battle-tested async stack
//! (tokio + hyper + rustls): an application engine, a routing DSL, an
//! `install(plugin)` system, a phased interceptor pipeline, hybrid handlers
//! (call-style *and* typed extractors), typed app state, layered configuration,
//! and secure-by-default behavior (body limits, request timeouts, panic
//! isolation, opt-in TLS).
//!
//! This is the umbrella crate: depend on it and enable plugins via Cargo
//! features. Core types come from [`churust_core`] (re-exported here); the
//! `#[churust::main]` attribute comes from `churust-macros`.
//!
//! ## Quick start
//!
//! ```no_run
//! use churust::prelude::*;
//!
//! #[churust::main]
//! async fn main() -> std::io::Result<()> {
//!     Churust::server()
//!         .routing(|r| {
//!             r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
//!             r.get("/users/{id}", |Path(id): Path<u64>| async move {
//!                 format!("user #{id}")
//!             });
//!         })
//!         .start()
//!         .await
//! }
//! ```
//!
//! ## Testing without a socket
//!
//! Any app can be driven in-process with [`TestClient`] — no port binding, so
//! tests are fast and deterministic:
//!
//! ```
//! use churust::prelude::*;
//! use churust::TestClient;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .routing(|r| {
//!         r.get("/ping", |_c: Call| async { "pong" });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app).get("/ping").send().await;
//! assert_eq!(res.status(), StatusCode::OK);
//! assert_eq!(res.text(), "pong");
//! # });
//! ```
//!
//! ## Feature flags
//!
//! Plugins live behind Cargo features (all off by default):
//!
//! | Feature   | Enables                                              |
//! |-----------|------------------------------------------------------|
//! | `json`    | `churust_json` — `Json<T>` + `ContentNegotiation`    |
//! | `logging` | `churust_logging` — `CallLogging`                    |
//! | `cors`    | `churust_cors` — `Cors`                              |
//! | `auth`    | `churust_auth` — `Auth` + `Principal<P>`             |
//! | `tls`     | rustls TLS support in [`churust_core`]               |
//! | `full`    | all four plugins                                     |
//!
//! ```toml
//! [dependencies]
//! churust = { version = "0.1", features = ["full"] }
//! tokio = { version = "1", features = ["full"] }
//! ```
//!
//! Bring the common items into scope with [`prelude`].
//!
//! ## The `#[churust::main]` attribute
//!
//! Builds a multi-threaded tokio runtime and blocks on the async body — the
//! Churust equivalent of `#[tokio::main]`:
//!
//! ```no_run
//! #[churust::main]
//! async fn main() -> std::io::Result<()> {
//!     use churust::prelude::*;
//!     let _app = Churust::server().build();
//!     Ok(())
//! }
//! ```
#![deny(missing_docs)]

pub use churust_core::*;

/// The async entry-point attribute (see the crate-level docs). Wraps
/// `async fn main` in a tokio runtime.
pub use churust_macros::main;

/// WebSocket types (`WebSocket`, `WebSocketUpgrade`, `ws::Message`). Enabled by
/// the `ws` feature.
#[cfg(feature = "ws")]
pub use churust_core::ws;

/// Authentication plugin crate (`Auth`, `Principal<P>`). Enabled by the `auth`
/// feature.
#[cfg(feature = "auth")]
pub use churust_auth as auth;
/// CORS plugin crate (`Cors`). Enabled by the `cors` feature.
#[cfg(feature = "cors")]
pub use churust_cors as cors;
/// JSON plugin crate (`Json<T>`, `ContentNegotiation`). Enabled by the `json`
/// feature.
#[cfg(feature = "json")]
pub use churust_json as json;
/// Request-logging plugin crate (`CallLogging`). Enabled by the `logging`
/// feature.
#[cfg(feature = "logging")]
pub use churust_logging as logging;

/// Common imports for everyday Churust apps.
///
/// Glob-import this (`use churust::prelude::*;`) to get the server builder,
/// the `Call` context, the response traits, the built-in extractors, the
/// `#[churust::main]` macro, and — when their Cargo features are enabled — the
/// plugin types (`Json`, `Cors`, `CallLogging`, `Auth`, `Principal`).
pub mod prelude {
    pub use crate::main; // #[churust::main]
    pub use churust_core::{
        App, AppBuilder, BearerToken, Call, Churust, Config, Error, FromCall, FromCallParts,
        IntoHandler, IntoResponse, Middleware, Next, Path, Plugin, Query, Response, Result, Router,
        State,
    };
    pub use http::{Method, StatusCode};

    #[cfg(feature = "auth")]
    pub use churust_auth::{Auth, Principal};
    #[cfg(feature = "ws")]
    pub use churust_core::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
    #[cfg(feature = "cors")]
    pub use churust_cors::Cors;
    #[cfg(feature = "json")]
    pub use churust_json::{ContentNegotiation, Json};
    #[cfg(feature = "logging")]
    pub use churust_logging::CallLogging;
}
