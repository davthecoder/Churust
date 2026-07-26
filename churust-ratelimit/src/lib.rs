//! Rate limiting for the [Churust] web framework.
//!
//! [`RateLimit`] admits a bounded number of requests per key per period and
//! answers everything above that with `429 Too Many Requests` and a
//! `Retry-After` header. It is both a [`Plugin`] (server-wide) and a
//! [`Middleware`] (scoped to part of the route tree with
//! `RouteBuilder::intercept`).
//!
//! ```
//! use churust_core::{Call, Churust, TestClient};
//! use churust_ratelimit::RateLimit;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(RateLimit::per_minute(2))
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "ok" });
//!     })
//!     .build();
//!
//! let client = TestClient::new(app);
//! assert_eq!(client.get("/").send().await.status().as_u16(), 200);
//! assert_eq!(client.get("/").send().await.status().as_u16(), 200);
//! let limited = client.get("/").send().await;
//! assert_eq!(limited.status().as_u16(), 429);
//! assert!(limited.header("retry-after").is_some());
//! # });
//! ```
//!
//! # The algorithm
//!
//! A generic cell rate algorithm (GCRA), which is a leaky bucket expressed as
//! one timestamp per key rather than a counter plus a timer. Each key stores a
//! *theoretical arrival time*: the earliest moment at which the next request
//! would be perfectly conforming. A request is admitted when it arrives no more
//! than the burst tolerance ahead of that time, and the timestamp then advances
//! by one emission interval.
//!
//! Two properties follow, and both are why this is preferred over a fixed
//! window counter. Requests are smoothed rather than admitted in a stampede at
//! the top of each window, and `Retry-After` is an exact figure that falls out
//! of the arithmetic instead of an estimate.
//!
//! # Choosing a key
//!
//! The default key is the connection's peer IP, without the port, so several
//! connections from one client share a bucket. Behind a reverse proxy that IP
//! is the proxy, which would put every visitor in one bucket. Use
//! [`RateLimit::by`] there, and read a forwarding header only after checking
//! [`Call::peer_addr`](churust_core::Call::peer_addr) against proxies you
//! actually trust. `X-Forwarded-For` is caller-supplied and trivially spoofed.
//!
//! [Churust]: churust_core::Churust

#![deny(missing_docs)]

use async_trait::async_trait;
use churust_core::{AppBuilder, Call, Error, Middleware, Next, Phase, Plugin, Response};
use http::header::RETRY_AFTER;
use http::{HeaderValue, StatusCode};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How many keys are tracked before the table is pruned.
const DEFAULT_MAX_KEYS: usize = 100_000;

/// A function that derives the bucket key for a call, or `None` to exempt it.
type KeyFn = Arc<dyn Fn(&Call) -> Option<String> + Send + Sync>;

/// The tracked timestamps, behind one lock.
///
/// A `HashMap` under a `Mutex` rather than a sharded or lock-free map: the
/// critical section is a hash lookup and an insert, and a lock held that
/// briefly is not the bottleneck in a pipeline that is about to do I/O.
#[derive(Debug, Default)]
struct Table {
    /// Key to theoretical arrival time.
    tat: HashMap<String, Instant>,
}

impl Table {
    /// Drop keys that have gone idle, then, if the table is still over its cap,
    /// evict the entries nearest to expiry until it is under.
    ///
    /// Evicting a live entry forgives whatever that key had spent, so this is a
    /// memory bound rather than a security boundary. It only engages when the
    /// key space is being flooded, which is the case where the alternative,
    /// unbounded growth, is the more direct denial of service.
    fn prune(&mut self, now: Instant, max_keys: usize) {
        self.tat.retain(|_, tat| *tat > now);
        if self.tat.len() < max_keys {
            return;
        }
        let target = max_keys * 9 / 10;
        let mut by_expiry: Vec<(String, Instant)> =
            self.tat.iter().map(|(k, v)| (k.clone(), *v)).collect();
        by_expiry.sort_by_key(|(_, tat)| *tat);
        for (key, _) in by_expiry.into_iter().take(self.tat.len() - target) {
            self.tat.remove(&key);
        }
    }
}

/// A rate limiter, usable as a plugin or as scoped middleware.
///
/// Construct with [`per_second`](RateLimit::per_second),
/// [`per_minute`](RateLimit::per_minute), [`per_hour`](RateLimit::per_hour) or
/// [`per`](RateLimit::per), then refine with [`burst`](RateLimit::burst) and
/// [`by`](RateLimit::by).
///
/// Cloning shares the underlying table, so the same limiter can be installed in
/// several places and still count one budget.
#[derive(Clone)]
pub struct RateLimit {
    /// The spacing between two perfectly conforming requests.
    emission: Duration,
    /// How far ahead of its schedule a key may run.
    tolerance: Duration,
    /// The advertised allowance, used only for the error message.
    limit: u32,
    /// The advertised period, used only for the error message.
    period: Duration,
    max_keys: usize,
    key: KeyFn,
    table: Arc<Mutex<Table>>,
}

impl std::fmt::Debug for RateLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimit")
            .field("limit", &self.limit)
            .field("period", &self.period)
            .field("emission", &self.emission)
            .field("tolerance", &self.tolerance)
            .field("max_keys", &self.max_keys)
            .finish_non_exhaustive()
    }
}

impl RateLimit {
    /// Allow `limit` requests per `period`, per key.
    ///
    /// The initial burst equals `limit`: a key that has been quiet may spend
    /// its whole allowance at once and then refills at `limit / period`. Narrow
    /// that with [`burst`](RateLimit::burst).
    ///
    /// # Panics
    ///
    /// If `limit` is zero, or `period` is zero. Both describe a limiter that
    /// can never admit anything, which is a configuration mistake rather than a
    /// policy, and failing at startup is the repo's rule for those.
    pub fn per(limit: u32, period: Duration) -> Self {
        assert!(limit > 0, "rate limit must admit at least one request");
        assert!(
            !period.is_zero(),
            "rate limit period must be greater than zero"
        );
        let emission = period / limit;
        Self {
            emission,
            tolerance: emission * (limit - 1),
            limit,
            period,
            max_keys: DEFAULT_MAX_KEYS,
            key: Arc::new(default_key),
            table: Arc::new(Mutex::new(Table::default())),
        }
    }

    /// Allow `limit` requests per second, per key.
    pub fn per_second(limit: u32) -> Self {
        Self::per(limit, Duration::from_secs(1))
    }

    /// Allow `limit` requests per minute, per key.
    pub fn per_minute(limit: u32) -> Self {
        Self::per(limit, Duration::from_secs(60))
    }

    /// Allow `limit` requests per hour, per key.
    pub fn per_hour(limit: u32) -> Self {
        Self::per(limit, Duration::from_secs(3600))
    }

    /// Cap the instantaneous burst at `burst` requests.
    ///
    /// The sustained rate is unchanged. `burst(1)` admits no burst at all:
    /// requests must be spaced by a full emission interval.
    ///
    /// # Panics
    ///
    /// If `burst` is zero.
    pub fn burst(mut self, burst: u32) -> Self {
        assert!(burst > 0, "burst must be at least one request");
        self.tolerance = self.emission * (burst - 1);
        self
    }

    /// Derive the bucket key from the call instead of using the peer IP.
    ///
    /// Returning `None` exempts the request from limiting entirely, which is
    /// how you let health checks or an authenticated internal caller through.
    ///
    /// ```
    /// use churust_ratelimit::RateLimit;
    ///
    /// // Per API key, falling back to no limit for unauthenticated callers.
    /// let limiter = RateLimit::per_minute(60)
    ///     .by(|call| call.header("x-api-key").map(str::to_owned));
    /// ```
    pub fn by<F>(mut self, f: F) -> Self
    where
        F: Fn(&Call) -> Option<String> + Send + Sync + 'static,
    {
        self.key = Arc::new(f);
        self
    }

    /// Set how many keys are tracked before the table is pruned.
    ///
    /// The table holds one string key and one timestamp per active client, so
    /// the default of 100,000 is a few megabytes. Raise it for a large fleet,
    /// lower it for a memory-constrained deployment.
    ///
    /// # Panics
    ///
    /// If `n` is zero.
    pub fn max_keys(mut self, n: usize) -> Self {
        assert!(n > 0, "max_keys must be at least one");
        self.max_keys = n;
        self
    }

    /// Test one request against the limiter.
    ///
    /// `Ok(())` admits it. `Err(delay)` rejects it, where `delay` is how long
    /// the caller must wait before a request would conform.
    fn check(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut table = self.table.lock().unwrap_or_else(|p| p.into_inner());

        if table.tat.len() >= self.max_keys {
            table.prune(now, self.max_keys);
        }

        let tat = table.tat.get(key).copied().unwrap_or(now);
        // The earliest arrival this key may make. `checked_sub` failing means
        // the tolerance reaches back past the process's own clock origin, which
        // can only mean the key is idle, so the request conforms.
        let earliest = tat.checked_sub(self.tolerance).unwrap_or(now);
        if now < earliest {
            return Err(earliest - now);
        }

        // A key that has fallen behind schedule resumes from now rather than
        // accumulating credit for the time it was quiet. That credit is what
        // the burst tolerance already expresses.
        let base = if tat > now { tat } else { now };
        table.tat.insert(key.to_string(), base + self.emission);
        Ok(())
    }

    /// The response for a rejected request.
    ///
    /// `Retry-After` is whole seconds per RFC 9110 §10.2.3, rounded up so the
    /// advertised moment is never earlier than the conforming one, and floored
    /// at one so a sub-second wait does not read as "retry immediately".
    fn too_many(&self, delay: Duration) -> Response {
        let secs = delay.as_secs_f64().ceil().max(1.0) as u64;
        let message = format!(
            "rate limit exceeded: {} requests per {} seconds",
            self.limit,
            self.period.as_secs_f64()
        );
        let mut error = Error::new(StatusCode::TOO_MANY_REQUESTS, message);
        if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
            error = error.with_response_header(RETRY_AFTER, value);
        }
        churust_core::IntoResponse::into_response(error)
    }
}

/// The peer IP without its port, so several connections from one client share a
/// bucket.
///
/// A call with no peer address (the in-process test client, or a transport that
/// does not carry one) falls into a single shared bucket rather than being
/// exempted. Exempting would make "arrive without an address" the way around
/// the limiter.
fn default_key(call: &Call) -> Option<String> {
    Some(match call.peer_addr() {
        Some(addr) => addr.ip().to_string(),
        None => "unknown".to_string(),
    })
}

#[async_trait]
impl Middleware for RateLimit {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let Some(key) = (self.key)(&call) else {
            return next.run(call).await;
        };
        match self.check(&key) {
            Ok(()) => next.run(call).await,
            Err(delay) => self.too_many(delay),
        }
    }
}

impl Plugin for RateLimit {
    /// Installed in [`Phase::Plugins`], so a rejected request is still logged by
    /// a `CallLogging` plugin sitting in [`Phase::Monitoring`] outside it.
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(Phase::Plugins, Arc::new(*self));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_key_may_spend_the_whole_allowance_at_once() {
        let limiter = RateLimit::per(3, Duration::from_secs(10));
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err(), "the fourth exceeds the burst");
    }

    #[test]
    fn keys_are_independent() {
        let limiter = RateLimit::per(1, Duration::from_secs(10));
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
        assert!(limiter.check("b").is_ok(), "b has its own budget");
    }

    #[test]
    fn burst_one_admits_a_single_request() {
        let limiter = RateLimit::per(100, Duration::from_secs(1)).burst(1);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
    }

    #[test]
    fn the_reported_delay_never_exceeds_the_period() {
        let limiter = RateLimit::per(2, Duration::from_secs(4));
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
        let delay = limiter.check("a").expect_err("should be limited");
        assert!(
            delay <= Duration::from_secs(4),
            "waiting longer than the period would be wrong: {delay:?}"
        );
    }

    #[test]
    fn clones_share_one_budget() {
        let limiter = RateLimit::per(1, Duration::from_secs(10));
        let clone = limiter.clone();
        assert!(limiter.check("a").is_ok());
        assert!(
            clone.check("a").is_err(),
            "a clone must not reset the table"
        );
    }

    #[test]
    fn pruning_bounds_the_table() {
        let limiter = RateLimit::per(1, Duration::from_millis(1)).max_keys(8);
        for i in 0..64 {
            let _ = limiter.check(&format!("key-{i}"));
        }
        let len = limiter.table.lock().unwrap().tat.len();
        assert!(len <= 8, "table grew past its cap: {len}");
    }

    #[test]
    #[should_panic(expected = "at least one request")]
    fn a_zero_limit_is_a_configuration_error() {
        let _ = RateLimit::per(0, Duration::from_secs(1));
    }

    #[test]
    #[should_panic(expected = "greater than zero")]
    fn a_zero_period_is_a_configuration_error() {
        let _ = RateLimit::per(1, Duration::ZERO);
    }
}
