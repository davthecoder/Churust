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
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        let method = call.method().clone();
        // Owned before dispatch: `next.run` consumes `call`.
        let path = call.path().to_owned();

        // Continue an inbound trace when the caller supplied one, so a request
        // crossing service boundaries keeps a single trace id. Otherwise start
        // one, which is what makes correlation possible at all.
        let id = RequestId::from_call(&call);
        let request_id = id.request_id.clone();
        let trace_id = id.trace_id.clone();
        // Build the response header once; hex ids are always valid header values.
        let request_id_header = http::HeaderValue::from_str(&request_id).ok();
        call.insert(id);

        let start = Instant::now();
        let mut res = next.run(call).await;
        let latency_ms = start.elapsed().as_millis();
        let status = res.status.as_u16();

        // Echo it back so a client can quote the id in a bug report.
        if let Some(v) = request_id_header {
            res.headers
                .insert(http::header::HeaderName::from_static("x-request-id"), v);
        }

        // tracing macros need a const level; branch on the configured level.
        match self.level {
            Level::ERROR => {
                tracing::error!(%method, path, status, latency_ms, request_id, trace_id, "request")
            }
            Level::WARN => {
                tracing::warn!(%method, path, status, latency_ms, request_id, trace_id, "request")
            }
            Level::INFO => {
                tracing::info!(%method, path, status, latency_ms, request_id, trace_id, "request")
            }
            Level::DEBUG => {
                tracing::debug!(%method, path, status, latency_ms, request_id, trace_id, "request")
            }
            Level::TRACE => {
                tracing::trace!(%method, path, status, latency_ms, request_id, trace_id, "request")
            }
        }
        res
    }
}

/// Correlation identifiers for one request.
///
/// Seeded by [`CallLogging`] and readable from a handler with
/// [`Call::get`](churust_core::Call::get). The `x-request-id` response header
/// carries the request id back to the client.
///
/// ```
/// use churust_core::{Call, Churust, TestClient};
/// use churust_logging::{CallLogging, RequestId};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(CallLogging::new())
///     .routing(|r| {
///         r.get("/", |c: Call| async move {
///             c.get::<RequestId>().map(|id| id.request_id).unwrap_or_default()
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app).get("/").send().await;
/// assert!(!res.text().is_empty());
/// assert_eq!(res.header("x-request-id"), Some(res.text().as_str()));
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct RequestId {
    /// Unique to this request.
    pub request_id: String,
    /// Shared by every request in the same distributed trace. Taken from an
    /// inbound W3C `traceparent` when present.
    pub trace_id: String,
}

impl RequestId {
    fn from_call(call: &Call) -> Self {
        // W3C traceparent: version-traceid-spanid-flags, e.g.
        // 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
        let inbound = call.header("traceparent").and_then(|v| {
            let parts: Vec<&str> = v.split('-').collect();
            // Validate rather than trust: a malformed value must not become a
            // trace id, or one bad caller poisons the whole correlation index.
            (parts.len() >= 3
                && parts[1].len() == 32
                && parts[1].chars().all(|c| c.is_ascii_hexdigit())
                && parts[1].chars().any(|c| c != '0'))
            .then(|| parts[1].to_string())
        });

        Self {
            request_id: gen_hex(16),
            trace_id: inbound.unwrap_or_else(|| gen_hex(32)),
        }
    }
}

/// A hex identifier of `n` characters.
///
/// Deliberately not cryptographic and deliberately dependency-free: this is a
/// correlation id, not a secret. Uniqueness comes from a nanosecond clock, a
/// per-process counter and the pid, which is enough to keep log lines apart.
fn gen_hex(n: usize) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id() as u64;

    let mut out = String::with_capacity(n);
    let mut state = nanos ^ (seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ (pid << 32);
    while out.len() < n {
        // xorshift64*, plenty for log correlation.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.push_str(&format!("{v:016x}"));
    }
    out.truncate(n);
    out
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
