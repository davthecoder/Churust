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
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/davthecoder/Churust/main/img/churust_logo.png"
)]

pub mod body;
pub use body::Body;

pub mod error;
pub use error::{Error, IntoError, Result};

pub mod response;
pub use response::{IntoResponse, Response};

pub mod call;
pub use call::{BodyStream, Call, Params, PeerAddr};

pub mod handler;
pub use handler::{boxed, BoxHandler, Handler, IntoHandler};

pub mod state;
pub use state::StateMap;

pub mod extract;
pub use extract::{
    check_body_limit, BearerToken, Either, Form, FromCall, FromCallParts, Header, HeaderName,
    OptionalFromCallParts, Path, Payload, Query, RouteBodyLimit, State,
};

#[cfg(feature = "tower")]
pub mod tower;

pub mod router;
pub use path::PathPolicy;
pub use router::{allow_header_value, Match, RouteBuilder, Router};

pub mod pipeline;
pub use pipeline::{Endpoint, Middleware, Next, Phase};

pub mod config;
pub use config::{Config, ServerSection, TlsSection};

pub mod app;
pub use app::{App, AppBuilder, Churust, Plugin, ServerConfig};

pub mod engine;

/// Security response headers applied by default. See [`SecurityHeaders`].
pub mod security;
pub use security::SecurityHeaders;

// Percent-decoding for path segments. Internal: the decoding rules are a
// routing detail, and exposing them would invite decoding at the wrong point in
// the pipeline.
/// Percent-decoding and canonicalisation for URL path segments.
pub mod path;
mod path_de;

/// Cookies: reading a request's, and building `Set-Cookie`.
pub mod cookie;
pub use cookie::{Cookie, SameSite};

/// `multipart/form-data` bodies (feature `multipart`).
#[cfg(feature = "multipart")]
pub mod multipart;
#[cfg(feature = "multipart")]
pub use multipart::{Field, Multipart, MultipartStream, Part};

/// Sessions carried by a cookie.
pub mod session;
pub use session::{CookieStore, Session, SessionStore, Sessions, SESSION_ID_KEY};

/// Login, logout, and the two deadlines that end a login.
pub mod identity;
pub use identity::{Authenticated, Identities, Identity};

/// Route guards — predicates that select among routes sharing a method and path.
pub mod guard;
pub use guard::{BoxGuard, Guard};

/// Run a blocking operation without stalling the async runtime.
///
/// Calling a blocking API directly from a handler occupies a runtime worker for
/// its duration; enough concurrent calls and the server stops answering
/// anything. This moves the work to tokio's blocking pool, which is what that
/// pool is for.
///
/// Reach for it around synchronous file I/O, a blocking database driver, or
/// CPU-heavy work such as password hashing.
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let sum = churust_core::block(|| (1..=1000).sum::<u64>()).await.unwrap();
/// assert_eq!(sum, 500_500);
/// # });
/// ```
///
/// A panic inside `f` becomes a `500` rather than taking down the worker,
/// matching how a panicking handler is already treated.
pub async fn block<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| {
        // A `JoinError` embeds the panic payload, and `Error`'s message is what
        // is rendered to the client — so formatting it into the message published whatever an
        // `.expect("connecting to postgres://user:pw@host")` happened to say.
        // Keep it for the operator, send the client the same bare message a
        // panicking handler already produces.
        tracing::error!(error = %e, "blocking task failed");
        Error::internal("Internal Server Error").with_source(e)
    })
}

/// Compare two secrets without leaking their contents through timing.
///
/// A plain `==` on strings returns as soon as it finds a differing byte, so the
/// time it takes reveals how much of a guess was correct. Over enough requests
/// that is enough to recover a token or password a byte at a time.
///
/// Use this in [`Auth::basic`](../churust_auth/index.html) callbacks and
/// anywhere else a request-supplied value is checked against a secret.
///
/// The comparison is constant-time **in the contents**, not in the length: an
/// early length check short-circuits, which reveals only the length. That is
/// the same trade every practical implementation makes.
///
/// ```
/// use churust_core::secure_compare;
///
/// assert!(secure_compare("hunter2", "hunter2"));
/// assert!(!secure_compare("hunter2", "hunter3"));
/// ```
pub fn secure_compare(a: impl AsRef<[u8]>, b: impl AsRef<[u8]>) -> bool {
    let (a, b) = (a.as_ref(), b.as_ref());
    if a.len() != b.len() {
        return false;
    }
    // Fold every byte into the accumulator so the loop cannot exit early.
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(feature = "tls")]
mod pem;

#[cfg(feature = "tls")]
pub mod tls;

/// HTTP/3 over QUIC (feature `http3`).
#[cfg(feature = "http3")]
pub mod http3;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub use ws::{WebSocket, WebSocketUpgrade};

#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "fs")]
pub use fs::StaticFiles;

pub mod test;
pub use test::{TestClient, TestRequest, TestResponse};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
