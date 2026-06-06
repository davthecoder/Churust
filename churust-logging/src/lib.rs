//! Per-request structured logging for the Churust web framework.
//!
//! This crate provides [`CallLogging`], a [`Plugin`] that installs a middleware
//! in the [`Phase::Monitoring`] phase.  The middleware records the HTTP method,
//! request path, response status code, and wall-clock latency (in milliseconds)
//! for every request that passes through the application.  Logging is
//! implemented on top of the [`tracing`] crate, so it integrates with any
//! `tracing`-compatible subscriber (e.g. `tracing-subscriber`'s `fmt` layer).
//!
//! # Quick start
//!
//! ```
//! use churust_core::{Churust, Call, TestClient};
//! use churust_logging::CallLogging;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(CallLogging::new())            // add structured request logs
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "hello" });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app).get("/").send().await;
//! assert_eq!(res.status().as_u16(), 200);
//! # });
//! ```
//!
//! # Log output
//!
//! Each completed request emits a single `tracing` event at the configured
//! level with the following structured fields:
//!
//! | field        | type   | description                         |
//! |--------------|--------|-------------------------------------|
//! | `method`     | string | HTTP method (GET, POST, …)          |
//! | `path`       | string | Request path (/api/users, …)        |
//! | `status`     | u16    | Response status code (200, 404, …)  |
//! | `latency_ms` | u128   | Round-trip time in milliseconds     |
//!
//! A typical log line produced by a `fmt` subscriber might look like:
//!
//! ```text
//! INFO  churust_logging: request method=GET path=/api/users status=200 latency_ms=3
//! ```
//!
//! # Middleware phase
//!
//! [`CallLogging`] runs in [`Phase::Monitoring`], which is the outermost phase
//! in Churust's middleware pipeline.  This means the timer starts before any
//! authentication, CORS, or routing middleware runs, giving an accurate
//! end-to-end latency for every request.
//!
//! [`tracing`]: https://docs.rs/tracing

#![deny(missing_docs)]

use async_trait::async_trait;
use churust_core::{AppBuilder, Call, Middleware, Next, Phase, Plugin, Response};
use std::sync::Arc;
use std::time::Instant;
use tracing::Level;

/// A [`Plugin`] that adds structured per-request logging to a Churust application.
///
/// `CallLogging` installs a single middleware in the [`Phase::Monitoring`] phase.
/// For every request it records the HTTP method, request path, response status
/// code, and elapsed wall-clock time in milliseconds, emitting them as a single
/// [`tracing`] event.
///
/// # Default log level
///
/// Unless overridden with [`CallLogging::level`], events are emitted at
/// [`Level::INFO`].
///
/// # Examples
///
/// ## Basic usage (default INFO level)
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// use churust_logging::CallLogging;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(CallLogging::new())
///     .routing(|r| {
///         r.get("/ping", |_c: Call| async { "pong" });
///     })
///     .build();
///
/// let res = TestClient::new(app).get("/ping").send().await;
/// assert_eq!(res.status().as_u16(), 200);
/// # });
/// ```
///
/// ## Custom log level (DEBUG)
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// use churust_logging::CallLogging;
/// use tracing::Level;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(CallLogging::new().level(Level::DEBUG))
///     .routing(|r| {
///         r.get("/", |_c: Call| async { "hi" });
///     })
///     .build();
///
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.status().as_u16(), 200);
/// # });
/// ```
///
/// [`tracing`]: https://docs.rs/tracing
/// [`Level::INFO`]: tracing::Level::INFO
#[derive(Debug, Clone)]
pub struct CallLogging {
    level: Level,
}

impl Default for CallLogging {
    /// Creates a [`CallLogging`] plugin with [`Level::INFO`] as the default log level.
    ///
    /// This is equivalent to calling [`CallLogging::new()`].
    ///
    /// [`Level::INFO`]: tracing::Level::INFO
    fn default() -> Self {
        Self { level: Level::INFO }
    }
}

impl CallLogging {
    /// Creates a new `CallLogging` plugin with the default log level ([`Level::INFO`]).
    ///
    /// Use [`CallLogging::level`] on the returned value to select a different
    /// severity level before passing the plugin to [`AppBuilder::install`].
    ///
    /// # Examples
    ///
    /// ```
    /// use churust_logging::CallLogging;
    ///
    /// let plugin = CallLogging::new();
    /// ```
    ///
    /// [`Level::INFO`]: tracing::Level::INFO
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the [`tracing`] level at which request events are emitted.
    ///
    /// All five standard [`tracing`] levels are supported:
    /// [`Level::ERROR`], [`Level::WARN`], [`Level::INFO`] (default),
    /// [`Level::DEBUG`], and [`Level::TRACE`].
    ///
    /// Choosing a lower level (e.g. `DEBUG` or `TRACE`) is useful in
    /// development where you want request logs without cluttering production
    /// output.  Choosing a higher level (`WARN` or `ERROR`) lets you capture
    /// only unexpected or slow requests when combined with a filtered subscriber.
    ///
    /// # Arguments
    ///
    /// * `level` — The [`tracing::Level`] to use for the emitted event.
    ///
    /// # Examples
    ///
    /// ```
    /// use churust_logging::CallLogging;
    /// use tracing::Level;
    ///
    /// // Emit request logs at TRACE level (very verbose).
    /// let plugin = CallLogging::new().level(Level::TRACE);
    ///
    /// // Emit request logs at WARN level (production-quiet).
    /// let plugin = CallLogging::new().level(Level::WARN);
    /// ```
    ///
    /// [`tracing`]: https://docs.rs/tracing
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }
}

impl Plugin for CallLogging {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(
            Phase::Monitoring,
            Arc::new(LogMiddleware { level: self.level }),
        );
    }
}

struct LogMiddleware {
    level: Level,
}

#[async_trait]
impl Middleware for LogMiddleware {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let method = call.method().clone();
        let path = call.path().to_string();
        let start = Instant::now();
        let res = next.run(call).await;
        let latency_ms = start.elapsed().as_millis();
        let status = res.status.as_u16();
        // tracing macros need a const level; branch on the configured level.
        match self.level {
            Level::ERROR => tracing::error!(%method, path, status, latency_ms, "request"),
            Level::WARN => tracing::warn!(%method, path, status, latency_ms, "request"),
            Level::INFO => tracing::info!(%method, path, status, latency_ms, "request"),
            Level::DEBUG => tracing::debug!(%method, path, status, latency_ms, "request"),
            Level::TRACE => tracing::trace!(%method, path, status, latency_ms, "request"),
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};
    use http::StatusCode;

    fn app() -> App {
        Churust::server()
            .install(CallLogging::new())
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build()
    }

    #[tokio::test]
    async fn logging_is_transparent() {
        // The middleware must not alter the response.
        let client = TestClient::new(app());
        let res = client.get("/").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "ok");
    }
}
