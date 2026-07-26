//! Who the visitor is: login, logout, and the two deadlines that end a login.
//!
//! Sessions carry arbitrary key/value state. This module is the thin layer that
//! turns "some state" into "signed in as someone", so an application does not
//! reinvent the same three session keys and the same two expiry checks.
//!
//! Install [`Identities`] alongside [`Sessions`](crate::Sessions), then take
//! [`Identity`] in a handler to log a visitor in or out, and
//! [`Authenticated`] to require one.
//!
//! ```
//! use churust_core::{Authenticated, Churust, Identities, Identity, Sessions, TestClient};
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(Sessions::cookie("change-me-in-production"))
//!     .install(Identities::new().visit_deadline(1800))
//!     .routing(|r| {
//!         r.post("/login", |id: Identity| async move {
//!             id.login("user-42");
//!             "welcome"
//!         });
//!         r.get("/me", |Authenticated(who): Authenticated| async move { who });
//!         r.post("/logout", |id: Identity| async move {
//!             id.logout();
//!             "bye"
//!         });
//!     })
//!     .build();
//!
//! let client = TestClient::new(app);
//! // Anonymous visitors are refused by `Authenticated`.
//! assert_eq!(client.get("/me").send().await.status().as_u16(), 401);
//! # });
//! ```
//!
//! # The two deadlines
//!
//! They answer different questions and a serious deployment sets both.
//!
//! - [`login_deadline`](Identities::login_deadline) is an **absolute** lifetime:
//!   how long a login may last no matter how active the visitor is. It bounds
//!   the damage from a session stolen and then used continuously, which an idle
//!   timeout alone never expires.
//! - [`visit_deadline`](Identities::visit_deadline) is an **idle** timeout: how
//!   long a login survives without a request. It is what protects a shared or
//!   unattended machine.
//!
//! Neither is set by default, because a default here would be either too short
//! for an internal tool or too long for a bank, and the framework cannot know
//! which it is looking at.
//!
//! # Reserved session keys
//!
//! This layer keeps its state in three ordinary session keys:
//!
//! - `__churust_uid` — who the visitor is. Writing it *is* logging someone in:
//!   [`Authenticated`] and [`Identity::id`] read nothing else.
//! - `__churust_lin` — when they logged in, as Unix seconds.
//! - `__churust_seen` — when they were last seen, as Unix seconds.
//!
//! They are ordinary keys on purpose. [`Session::set`](crate::Session::set) does
//! not refuse them, because [`Identity::login`] writes them through that very
//! method, and because writing `__churust_uid` by hand is the supported way to
//! adopt this layer over sessions an application was already minting itself.
//! The `__churust` prefix is the reservation, and it covers
//! [`SESSION_ID_KEY`](crate::SESSION_ID_KEY) too.
//!
//! Nothing a visitor sends can reach these keys: a session is server-authored,
//! and [`CookieStore`](crate::CookieStore) verifies its signature before parsing
//! the contents. The one shape that would is an application that writes
//! caller-supplied key *names* — `session.set(form.key, form.value)` — which is
//! mass assignment and hands the visitor its own `role` and `tenant_id` keys as
//! well. If you must do that, reject any key beginning with `__churust` before
//! the write; there is no framework-side filter that could do it for you without
//! breaking login.

use crate::app::{AppBuilder, Plugin};
use crate::call::Call;
use crate::error::{Error, Result};
use crate::extract::FromCallParts;
use crate::pipeline::{Middleware, Next, Phase};
use crate::response::Response;
use crate::session::Session;
use async_trait::async_trait;
use http::header::LOCATION;
use http::{HeaderValue, StatusCode};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Session key holding who the visitor is.
const UID_KEY: &str = "__churust_uid";
/// Session key holding when they logged in, as Unix seconds.
const LOGIN_AT_KEY: &str = "__churust_lin";
/// Session key holding when they were last seen, as Unix seconds.
const SEEN_AT_KEY: &str = "__churust_seen";

/// Seconds since the Unix epoch, or `None` if the clock predates it.
fn now_secs() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// What the [`Identities`] plugin was configured with, put into the call so
/// [`Authenticated`] can render the right refusal.
#[derive(Clone, Debug, Default)]
struct Policy {
    /// Where to send an unauthenticated visitor, if anywhere.
    login_url: Option<String>,
}

/// A handle to the current visitor's identity.
///
/// Extract it in any handler. It is always available: an anonymous visitor
/// simply has no [`id`](Identity::id). Every method operates on the session, so
/// the changes are persisted by the session plugin on the way out.
#[derive(Clone, Debug)]
pub struct Identity {
    session: Session,
}

impl Identity {
    /// Who the visitor is, or `None` when anonymous.
    pub fn id(&self) -> Option<String> {
        self.session.get(UID_KEY)
    }

    /// Whether anyone is logged in.
    pub fn is_authenticated(&self) -> bool {
        self.id().is_some()
    }

    /// Record `id` as the logged-in visitor.
    ///
    /// The session identifier is rotated first (see [`Session::rotate`]), so a
    /// server-side store mints a fresh record rather than promoting one an
    /// attacker may have planted. Session contents other than the identity are
    /// preserved, which is what keeps a pre-login cart or locale across the
    /// boundary. Call [`logout`](Identity::logout) first if you would rather
    /// start empty.
    pub fn login(&self, id: impl Into<String>) {
        self.session.rotate();
        self.session.set(UID_KEY, id.into());
        if let Some(now) = now_secs() {
            self.session.set(LOGIN_AT_KEY, now.to_string());
            self.session.set(SEEN_AT_KEY, now.to_string());
        }
    }

    /// Log the visitor out and drop the whole session.
    ///
    /// Everything goes, not just the identity keys: anything put in the session
    /// while signed in was put there for a signed-in visitor.
    ///
    /// With a client-side store this clears the visitor's copy but cannot
    /// revoke a copy taken beforehand, which stays valid until its signed
    /// deadline. Revocation needs server-side state.
    pub fn logout(&self) {
        self.session.clear();
    }

    /// When the visitor logged in, as Unix seconds.
    pub fn logged_in_at(&self) -> Option<i64> {
        self.session.get(LOGIN_AT_KEY).and_then(|v| v.parse().ok())
    }

    /// When the visitor was last seen, as Unix seconds.
    ///
    /// Only maintained when [`Identities::visit_deadline`] is set, and only
    /// written periodically rather than on every request. See that method for
    /// why.
    pub fn last_seen_at(&self) -> Option<i64> {
        self.session.get(SEEN_AT_KEY).and_then(|v| v.parse().ok())
    }
}

#[async_trait]
impl FromCallParts for Identity {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        // An absent session means the Sessions plugin is not installed. An
        // empty, unpersisted identity is friendlier than an error, and matches
        // how `Session` itself behaves.
        Ok(Identity {
            session: call.get::<Session>().unwrap_or_default(),
        })
    }
}

/// An extractor that requires a logged-in visitor, yielding their id.
///
/// An anonymous visitor never reaches the handler: they get `401 Unauthorized`,
/// or a redirect when [`Identities::login_url`] is configured.
///
/// ```
/// use churust_core::Authenticated;
///
/// async fn profile(Authenticated(user): Authenticated) -> String {
///     format!("signed in as {user}")
/// }
/// ```
#[derive(Clone, Debug)]
pub struct Authenticated(
    /// The logged-in visitor's id.
    pub String,
);

#[async_trait]
impl FromCallParts for Authenticated {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let session = call.get::<Session>().unwrap_or_default();
        if let Some(id) = session.get(UID_KEY) {
            return Ok(Authenticated(id));
        }

        let policy = call.get::<Policy>().unwrap_or_default();
        match policy.login_url {
            // 303 rather than 302: it makes the follow-up a GET regardless of
            // what was attempted, which is what a login page wants.
            Some(url) => {
                let mut error = Error::new(StatusCode::SEE_OTHER, "authentication required");
                if let Ok(value) = HeaderValue::from_str(&url) {
                    error = error.with_response_header(LOCATION, value);
                }
                Err(error)
            }
            // No `WWW-Authenticate`, deliberately. RFC 9110 §15.5.2 asks for
            // one, but there is no registered scheme for a cookie session, and
            // naming `Basic` here would make a browser open its own login
            // dialog instead of the application's.
            None => Err(Error::new(
                StatusCode::UNAUTHORIZED,
                "authentication required",
            )),
        }
    }
}

/// Installs the identity layer. See the [module docs](self).
///
/// Order does not matter: the middleware runs in [`Phase::Call`], which is
/// inside the phase the session plugin uses, so the session is always loaded by
/// the time the deadlines are checked.
#[derive(Clone, Debug, Default)]
pub struct Identities {
    login_deadline: Option<i64>,
    visit_deadline: Option<i64>,
    login_url: Option<String>,
}

impl Identities {
    /// No deadlines, and `401` for an unauthenticated visitor.
    pub fn new() -> Self {
        Self::default()
    }

    /// End a login `secs` after it started, however active the visitor is.
    ///
    /// # Panics
    ///
    /// If `secs` is not positive. A deadline of zero or less would log every
    /// visitor out on the request after they signed in, which is never the
    /// intent.
    pub fn login_deadline(mut self, secs: i64) -> Self {
        assert!(
            secs > 0,
            "login_deadline must be a positive number of seconds"
        );
        self.login_deadline = Some(secs);
        self
    }

    /// End a login after `secs` with no requests.
    ///
    /// Enabling this makes the layer maintain a last-seen timestamp in the
    /// session. It is refreshed at most once per tenth of the deadline, not on
    /// every request: the session plugin only re-issues a cookie when the
    /// session changed, and touching a timestamp on every request would rewrite
    /// the cookie every time and quietly extend the expiry of a session nobody
    /// is really using. The cost of that thrift is that the idle timeout is
    /// accurate to within a tenth of itself.
    ///
    /// # Panics
    ///
    /// If `secs` is not positive.
    pub fn visit_deadline(mut self, secs: i64) -> Self {
        assert!(
            secs > 0,
            "visit_deadline must be a positive number of seconds"
        );
        self.visit_deadline = Some(secs);
        self
    }

    /// Send unauthenticated visitors to `url` instead of answering `401`.
    ///
    /// Applies to the [`Authenticated`] extractor. An API generally wants the
    /// `401`; a server-rendered application generally wants the redirect.
    pub fn login_url(mut self, url: impl Into<String>) -> Self {
        self.login_url = Some(url.into());
        self
    }
}

impl Plugin for Identities {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(
            Phase::Call,
            Arc::new(IdentityMiddleware {
                login_deadline: self.login_deadline,
                visit_deadline: self.visit_deadline,
                policy: Policy {
                    login_url: self.login_url.clone(),
                },
            }),
        );
    }
}

struct IdentityMiddleware {
    login_deadline: Option<i64>,
    visit_deadline: Option<i64>,
    policy: Policy,
}

impl IdentityMiddleware {
    /// Drop the identity keys, leaving the rest of the session alone.
    ///
    /// An expired login is not the same event as a logout: whatever else the
    /// session holds was not necessarily privileged, and discarding it would
    /// make a timeout also empty a cart.
    fn expire(session: &Session) {
        session.remove(UID_KEY);
        session.remove(LOGIN_AT_KEY);
        session.remove(SEEN_AT_KEY);
    }
}

#[async_trait]
impl Middleware for IdentityMiddleware {
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        call.insert(self.policy.clone());

        if let (Some(session), Some(now)) = (call.get::<Session>(), now_secs()) {
            if session.get(UID_KEY).is_some() {
                let started: Option<i64> = session.get(LOGIN_AT_KEY).and_then(|v| v.parse().ok());
                let seen: Option<i64> = session.get(SEEN_AT_KEY).and_then(|v| v.parse().ok());

                // A session carrying an identity but no timestamps predates
                // this layer, or was hand-built. Treat the absent timestamp as
                // "now" rather than as "infinitely old": expiring it would log
                // out every existing visitor the moment the layer is deployed.
                let expired_absolute = self
                    .login_deadline
                    .zip(started)
                    .is_some_and(|(limit, at)| now.saturating_sub(at) >= limit);
                let expired_idle = self
                    .visit_deadline
                    .zip(seen)
                    .is_some_and(|(limit, at)| now.saturating_sub(at) >= limit);

                if expired_absolute || expired_idle {
                    Self::expire(&session);
                } else if let Some(limit) = self.visit_deadline {
                    // Refresh at most once per tenth of the deadline. See
                    // `Identities::visit_deadline`.
                    let granularity = (limit / 10).max(1);
                    let stale = seen.is_none_or(|at| now.saturating_sub(at) >= granularity);
                    if stale {
                        session.set(SEEN_AT_KEY, now.to_string());
                    }
                }
            }
        }

        next.run(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    fn identity() -> Identity {
        Identity {
            session: Session::default(),
        }
    }

    #[test]
    fn a_fresh_visitor_is_anonymous() {
        let id = identity();
        assert!(!id.is_authenticated());
        assert_eq!(id.id(), None);
    }

    #[test]
    fn login_records_who_and_when() {
        let id = identity();
        id.login("user-1");
        assert_eq!(id.id().as_deref(), Some("user-1"));
        assert!(id.is_authenticated());
        assert!(id.logged_in_at().is_some());
        assert!(id.last_seen_at().is_some());
    }

    #[test]
    fn logout_empties_the_session() {
        let id = identity();
        id.session.set("cart", "3 items");
        id.login("user-1");
        id.logout();
        assert!(!id.is_authenticated());
        assert_eq!(
            id.session.get("cart"),
            None,
            "logout drops everything, not only the identity"
        );
    }

    #[test]
    fn login_keeps_other_session_state() {
        let id = identity();
        id.session.set("cart", "3 items");
        id.login("user-1");
        assert_eq!(id.session.get("cart").as_deref(), Some("3 items"));
    }

    #[test]
    fn expiring_leaves_the_rest_of_the_session_alone() {
        let id = identity();
        id.session.set("cart", "3 items");
        id.login("user-1");
        IdentityMiddleware::expire(&id.session);
        assert!(!id.is_authenticated());
        assert_eq!(id.session.get("cart").as_deref(), Some("3 items"));
    }

    #[test]
    #[should_panic(expected = "login_deadline must be a positive")]
    fn a_zero_login_deadline_is_refused() {
        let _ = Identities::new().login_deadline(0);
    }

    #[test]
    #[should_panic(expected = "visit_deadline must be a positive")]
    fn a_negative_visit_deadline_is_refused() {
        let _ = Identities::new().visit_deadline(-1);
    }
}
