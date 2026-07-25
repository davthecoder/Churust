//! Route guards: predicates that decide whether a registered route may serve a
//! request, beyond matching its method and path.
//!
//! Several routes may share a `(method, path)` as long as guards distinguish
//! them. Resolution takes the first whose guards all pass, in registration
//! order, so an unguarded route registered last acts as the fallback.
//!
//! ```
//! use churust_core::{guard, Call, Churust, TestClient};
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "beta" })
//!             .guard(guard::header("x-beta", "1"));
//!         r.get("/", |_c: Call| async { "stable" });
//!     })
//!     .build();
//!
//! let client = TestClient::new(app);
//! assert_eq!(client.get("/").header("x-beta", "1").send().await.text(), "beta");
//! assert_eq!(client.get("/").send().await.text(), "stable");
//! # });
//! ```

use crate::call::Call;
use std::sync::Arc;

/// A predicate over the request. See the [module docs](self).
pub trait Guard: Send + Sync + 'static {
    /// Whether this route may serve the request.
    fn check(&self, call: &Call) -> bool;
}

/// A boxed guard, as stored by the router.
pub type BoxGuard = Arc<dyn Guard>;

struct HeaderGuard {
    name: String,
    value: String,
}

impl Guard for HeaderGuard {
    fn check(&self, call: &Call) -> bool {
        call.header(&self.name) == Some(self.value.as_str())
    }
}

/// Passes when the request carries `name` with exactly `value`.
pub fn header(name: impl Into<String>, value: impl Into<String>) -> BoxGuard {
    Arc::new(HeaderGuard {
        name: name.into(),
        value: value.into(),
    })
}

struct HostGuard(String);

impl Guard for HostGuard {
    fn check(&self, call: &Call) -> bool {
        // `Call::host` reads the `Host` header *and* the URI authority. HTTP/2
        // dropped the header in favour of `:authority`, so reading only the
        // header meant every host-guarded route silently stopped matching over
        // h2 — which ALPN prefers — and traffic fell through to whatever
        // unguarded sibling was registered as the fallback. The port is not
        // part of the identity the caller asked about, and `host` strips it.
        call.host().is_some_and(|h| h.eq_ignore_ascii_case(&self.0))
    }
}

/// Passes when the request's host matches, ignoring any port and case.
///
/// Reads the `Host` header over HTTP/1.1 and the `:authority` pseudo-header
/// over HTTP/2, so a vhost-scoped route matches on both protocols. See
/// [`Call::host`](crate::Call::host).
pub fn host(name: impl Into<String>) -> BoxGuard {
    Arc::new(HostGuard(name.into()))
}

struct FnGuard<F>(F);

impl<F: Fn(&Call) -> bool + Send + Sync + 'static> Guard for FnGuard<F> {
    fn check(&self, call: &Call) -> bool {
        (self.0)(call)
    }
}

/// Passes when the closure returns `true`.
pub fn fn_guard<F: Fn(&Call) -> bool + Send + Sync + 'static>(f: F) -> BoxGuard {
    Arc::new(FnGuard(f))
}

struct All(Vec<BoxGuard>);

impl Guard for All {
    fn check(&self, call: &Call) -> bool {
        self.0.iter().all(|g| g.check(call))
    }
}

/// Passes only when every inner guard passes. An empty list passes.
pub fn all(guards: Vec<BoxGuard>) -> BoxGuard {
    Arc::new(All(guards))
}

struct Any(Vec<BoxGuard>);

impl Guard for Any {
    fn check(&self, call: &Call) -> bool {
        self.0.iter().any(|g| g.check(call))
    }
}

/// Passes when at least one inner guard passes. An empty list never passes.
pub fn any(guards: Vec<BoxGuard>) -> BoxGuard {
    Arc::new(Any(guards))
}

struct Not(BoxGuard);

impl Guard for Not {
    fn check(&self, call: &Call) -> bool {
        !self.0.check(call)
    }
}

/// Inverts a guard.
pub fn not(g: BoxGuard) -> BoxGuard {
    Arc::new(Not(g))
}
