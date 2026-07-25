//! Sessions: per-visitor state carried by a cookie.
//!
//! A [`SessionStore`] trait plus a cookie-backed
//! implementation. The session is a string map, seeded into the call by
//! [`Sessions`] and read with the [`Session`] extractor.
//!
//! ```
//! use churust_core::{Call, Churust, Session, Sessions, TestClient};
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(Sessions::cookie("change-me-in-production"))
//!     .routing(|r| {
//!         r.get("/visit", |s: Session| async move {
//!             let n: u32 = s.get("count").and_then(|v| v.parse().ok()).unwrap_or(0);
//!             s.set("count", (n + 1).to_string());
//!             format!("visit {}", n + 1)
//!         });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app).get("/visit").send().await;
//! assert_eq!(res.text(), "visit 1");
//! assert!(res.header("set-cookie").is_some());
//! # });
//! ```

use crate::call::Call;
use crate::cookie::Cookie;
use crate::error::Result;
use crate::extract::FromCallParts;
use crate::pipeline::{Middleware, Next};
use crate::response::Response;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The session's contents, plus whether they changed.
#[derive(Debug, Default)]
struct Inner {
    data: BTreeMap<String, String>,
    dirty: bool,
}

/// A handle to the current visitor's session.
///
/// Cloneable and cheap; all clones share one map. Extract it as a handler
/// argument.
#[derive(Clone, Debug, Default)]
pub struct Session(Arc<Mutex<Inner>>);

impl Session {
    /// Read a value.
    pub fn get(&self, key: &str) -> Option<String> {
        self.0.lock().ok()?.data.get(key).cloned()
    }

    /// Insert or replace a value.
    pub fn set(&self, key: impl Into<String>, value: impl Into<String>) {
        if let Ok(mut g) = self.0.lock() {
            g.data.insert(key.into(), value.into());
            g.dirty = true;
        }
    }

    /// Remove a value.
    pub fn remove(&self, key: &str) {
        if let Ok(mut g) = self.0.lock() {
            if g.data.remove(key).is_some() {
                g.dirty = true;
            }
        }
    }

    /// Drop every value.
    ///
    /// Call this on logout. Rotating rather than reusing a session is what
    /// stops a fixated session id from surviving a privilege change.
    pub fn clear(&self) {
        if let Ok(mut g) = self.0.lock() {
            g.data.clear();
            g.dirty = true;
        }
    }

    /// Keep the contents but ask the store for a new identifier.
    ///
    /// This is the session fixation defence for a **server-side** store: an
    /// attacker who plants a known session id must not still hold a valid one
    /// after the victim's privileges change. Call it on login, and on any other
    /// privilege change, when you want to keep what is already in the session
    /// (a cart, a locale) rather than dropping it the way [`clear`](Self::clear)
    /// does.
    ///
    /// It works by removing [`SESSION_ID_KEY`], which a server-side store uses
    /// to remember which record this session is, so the next write mints a new
    /// one. [`CookieStore`] keeps no identifier and is unaffected: its payload
    /// is re-signed on every change already, so a planted cookie never carries
    /// the post-login contents.
    pub fn rotate(&self) {
        if let Ok(mut g) = self.0.lock() {
            g.data.remove(SESSION_ID_KEY);
            g.dirty = true;
        }
    }

    fn changed(&self) -> bool {
        self.0.lock().map(|g| g.dirty).unwrap_or(false)
    }

    fn snapshot(&self) -> BTreeMap<String, String> {
        self.0.lock().map(|g| g.data.clone()).unwrap_or_default()
    }

    fn from_map(data: BTreeMap<String, String>) -> Self {
        Session(Arc::new(Mutex::new(Inner { data, dirty: false })))
    }
}

#[async_trait]
impl FromCallParts for Session {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        // Absent only when the Sessions plugin is not installed; an empty
        // session is friendlier than an error, and nothing is persisted.
        Ok(call.get::<Session>().unwrap_or_default())
    }
}

/// The reserved key under which a server-side [`SessionStore`] records which
/// row, document or Redis key this session is.
///
/// Stores that keep everything client-side, [`CookieStore`] among them, have no
/// identifier and ignore it. It is named in the public API only so that
/// [`Session::rotate`] has something well-defined to remove, and so a store
/// implementor knows which key is spoken for.
pub const SESSION_ID_KEY: &str = "__churust_sid";

/// Where session contents live between requests.
///
/// Both operations are `async` because a store backed by server state has to
/// talk to it. A client-side store such as [`CookieStore`] never awaits
/// anything and simply returns.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Recover a session from the cookie value, if it is intact.
    async fn load(&self, raw: &str) -> Option<BTreeMap<String, String>>;

    /// Persist the session and return the new cookie value, or `None` to leave
    /// the visitor's cookie untouched.
    ///
    /// `previous` is the cookie value this request arrived with, if any. A
    /// server-side store needs it to delete the record it is replacing: without
    /// it, logging out would leave the old record readable until its own TTV
    /// elapsed, which is exactly the revocation a server-side store exists to
    /// provide. A client-side store ignores it.
    async fn store(
        &self,
        data: &BTreeMap<String, String>,
        previous: Option<&str>,
    ) -> Option<String>;

    /// Adopt the session lifetime configured on [`Sessions::max_age`].
    ///
    /// Returning a new boxed store opts in; the default returns `None`, which
    /// means "I manage my own expiry" — the right answer for a store backed by
    /// server state, which can simply delete the record.
    ///
    /// This exists because `Max-Age` on a cookie is only a hint to a
    /// well-behaved client. A store that keeps its state client-side has to
    /// enforce the deadline itself, and it can only do that if it is told what
    /// the deadline is.
    fn with_session_max_age(&self, _secs: i64) -> Option<Box<dyn SessionStore>> {
        None
    }
}

/// Keeps the whole session in the cookie, signed with HMAC-SHA256 so a client
/// cannot edit it.
///
/// No server-side state, which makes it the simplest thing that works. Two
/// properties are worth being explicit about:
///
/// - **Signed, not encrypted.** The values are readable by whoever holds the
///   cookie. Do not put anything in it the visitor should not see.
/// - **The contents travel on every request.** This suits a user id and a flash
///   message, not a shopping cart.
/// - **Expiry is carried inside the signature.** `Set-Cookie`'s `Max-Age` is
///   only a hint to a well-behaved client; a copied cookie ignores it. When the
///   session plugin sets a max age, the deadline is signed into the payload and
///   checked on load, so a captured cookie stops working.
/// - **There is still no revocation.** Logging out clears the client's copy,
///   but a copy taken beforehand stays valid until its deadline, because
///   nothing server-side records that it was withdrawn. A store backed by
///   server state is what fixes that.
///
/// Use a key with at least 32 bytes of entropy, and keep it out of source
/// control.
pub struct CookieStore {
    key: Vec<u8>,
    /// Signed into the payload when set, so the deadline cannot be edited or
    /// ignored by the client.
    max_age: Option<i64>,
}

/// Reserved payload key holding the expiry as a Unix timestamp.
///
/// Prefixed so it cannot collide with an application key: `esc` percent-escapes
/// the separators but leaves `_`, so a caller *could* write this name — hence
/// it is stripped on load and overwritten on store rather than trusted.
const EXPIRY_KEY: &str = "__churust_exp";

impl CookieStore {
    /// Build a store from a signing key.
    pub fn new(key: impl Into<Vec<u8>>) -> Self {
        Self {
            key: key.into(),
            max_age: None,
        }
    }

    /// Sign an expiry `secs` in the future into every cookie this store issues.
    ///
    /// Set by [`Sessions::max_age`]; without it a cookie is valid for as long
    /// as the signing key is.
    pub fn with_max_age(mut self, secs: i64) -> Self {
        self.max_age = Some(secs);
        self
    }

    /// Seconds since the Unix epoch, or `None` if the clock is before it.
    fn now_secs() -> Option<i64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }

    /// HMAC-SHA256 over the payload, hex-encoded.
    fn sign(&self, payload: &str) -> String {
        use hmac::Mac;
        // `new_from_slice` accepts any key length for HMAC, so this cannot fail.
        let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(&self.key)
            .expect("HMAC accepts a key of any length");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        tag.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[async_trait]
impl SessionStore for CookieStore {
    fn with_session_max_age(&self, secs: i64) -> Option<Box<dyn SessionStore>> {
        // Keyed off the store, not off how `Sessions` happened to be built:
        // wiring this through a key that only `Sessions::cookie` records meant
        // `Sessions::with_store(CookieStore::new(k)).max_age(n)` signed no
        // deadline at all, so a captured cookie replayed until the key rotated.
        Some(Box::new(CookieStore {
            key: self.key.clone(),
            max_age: Some(secs),
        }))
    }

    async fn load(&self, raw: &str) -> Option<BTreeMap<String, String>> {
        let (sig, payload) = raw.split_once('.')?;
        // Constant-time so a forged signature cannot be refined byte by byte.
        if !crate::secure_compare(sig, self.sign(payload)) {
            return None;
        }
        let mut out = BTreeMap::new();
        for pair in payload.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair.split_once('=')?;
            out.insert(crate::cookie::decode(k), crate::cookie::decode(v));
        }

        // The signature covers the deadline, so this cannot be edited by the
        // client — only presented or not. A cookie that carries one and is past
        // it is treated as absent, which starts a fresh session.
        if let Some(raw_exp) = out.remove(EXPIRY_KEY) {
            let expires_at = raw_exp.parse::<i64>().ok()?;
            if Self::now_secs().is_some_and(|now| now >= expires_at) {
                return None;
            }
        }
        Some(out)
    }

    /// `previous` is unused: the whole session lives in the cookie, so there is
    /// no server-side record to withdraw.
    async fn store(
        &self,
        data: &BTreeMap<String, String>,
        _previous: Option<&str>,
    ) -> Option<String> {
        let mut pairs: Vec<String> = data
            .iter()
            // Never let an application key masquerade as the deadline.
            .filter(|(k, _)| k.as_str() != EXPIRY_KEY)
            .map(|(k, v)| format!("{}={}", esc(k), esc(v)))
            .collect();
        if let (Some(age), Some(now)) = (self.max_age, Self::now_secs()) {
            pairs.push(format!("{}={}", esc(EXPIRY_KEY), now + age));
        }
        let payload = pairs.join("&");
        Some(format!("{}.{}", self.sign(&payload), payload))
    }
}

/// Escape the separators the payload format uses.
fn esc(s: &str) -> String {
    s.replace('%', "%25")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('.', "%2E")
}

/// Installs session handling. See the [module docs](self).
pub struct Sessions {
    store: Arc<dyn SessionStore>,
    cookie_name: String,
    secure: bool,
    max_age: Option<i64>,
}

impl Sessions {
    /// Sessions kept entirely in a signed cookie.
    pub fn cookie(key: impl Into<Vec<u8>>) -> Self {
        Self::with_store(CookieStore::new(key))
    }

    /// Sessions kept wherever `store` puts them.
    pub fn with_store<S: SessionStore>(store: S) -> Self {
        Self {
            store: Arc::new(store),
            cookie_name: "churust_session".into(),
            secure: false,
            max_age: None,
        }
    }

    /// Change the cookie name (default `churust_session`).
    pub fn cookie_name(mut self, n: impl Into<String>) -> Self {
        self.cookie_name = n.into();
        self
    }

    /// Mark the cookie `Secure`. Turn this on in production.
    pub fn secure(mut self, yes: bool) -> Self {
        self.secure = yes;
        self
    }

    /// Expire the session after `secs`.
    ///
    /// This sets `Max-Age` on the cookie *and*, for the built-in cookie store,
    /// signs the deadline into the payload. `Max-Age` alone is only a hint to a
    /// well-behaved client, so a copied cookie would otherwise stay valid
    /// forever; the signed deadline is what actually stops replay.
    ///
    /// A custom [`SessionStore`] owns its own expiry policy and is untouched.
    pub fn max_age(mut self, secs: i64) -> Self {
        self.max_age = Some(secs);
        // Ask the store to adopt the deadline. A client-side store must sign it
        // in to enforce it; a server-side store declines and manages its own.
        if let Some(updated) = self.store.with_session_max_age(secs) {
            self.store = Arc::from(updated);
        }
        self
    }
}

impl crate::app::Plugin for Sessions {
    fn install(self: Box<Self>, app: &mut crate::app::AppBuilder) {
        app.add_middleware(Arc::new(SessionMiddleware {
            store: self.store.clone(),
            cookie_name: self.cookie_name.clone(),
            secure: self.secure,
            max_age: self.max_age,
        }));
    }
}

struct SessionMiddleware {
    store: Arc<dyn SessionStore>,
    cookie_name: String,
    secure: bool,
    max_age: Option<i64>,
}

#[async_trait]
impl Middleware for SessionMiddleware {
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        let arrived_with = call.cookie(&self.cookie_name);
        let loaded = match arrived_with.as_deref() {
            Some(raw) => self.store.load(raw).await.unwrap_or_default(),
            None => BTreeMap::new(),
        };

        let session = Session::from_map(loaded);
        call.insert(session.clone());

        let mut res = next.run(call).await;

        // Only re-issue when something changed: rewriting an unchanged cookie
        // on every response is noise, and it would extend the expiry of a
        // session the visitor is not actually using.
        if session.changed() {
            if let Some(value) = self
                .store
                .store(&session.snapshot(), arrived_with.as_deref())
                .await
            {
                let mut c = Cookie::new(self.cookie_name.clone(), value).secure(self.secure);
                if let Some(age) = self.max_age {
                    c = c.max_age(age);
                }
                res = res.with_cookie(c);
            }
        }
        res
    }
}
