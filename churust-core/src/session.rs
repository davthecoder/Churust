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

/// Where session contents live between requests.
pub trait SessionStore: Send + Sync + 'static {
    /// Recover a session from the cookie value, if it is intact.
    fn load(&self, raw: &str) -> Option<BTreeMap<String, String>>;
    /// Render the session into a cookie value.
    fn store(&self, data: &BTreeMap<String, String>) -> Option<String>;
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

impl SessionStore for CookieStore {
    fn load(&self, raw: &str) -> Option<BTreeMap<String, String>> {
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

    fn store(&self, data: &BTreeMap<String, String>) -> Option<String> {
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
    /// Kept so [`max_age`](Sessions::max_age) can rebuild the cookie store with
    /// the deadline signed in. `None` for a custom store, which owns its own
    /// expiry policy.
    cookie_key: Option<Vec<u8>>,
}

impl Sessions {
    /// Sessions kept entirely in a signed cookie.
    pub fn cookie(key: impl Into<Vec<u8>>) -> Self {
        let key = key.into();
        let mut s = Self::with_store(CookieStore::new(key.clone()));
        s.cookie_key = Some(key);
        s
    }

    /// Sessions kept wherever `store` puts them.
    pub fn with_store<S: SessionStore>(store: S) -> Self {
        Self {
            store: Arc::new(store),
            cookie_name: "churust_session".into(),
            secure: false,
            max_age: None,
            cookie_key: None,
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
        if let Some(key) = &self.cookie_key {
            self.store = Arc::new(CookieStore::new(key.clone()).with_max_age(secs));
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
        let loaded = call
            .cookie(&self.cookie_name)
            .and_then(|raw| self.store.load(&raw))
            .unwrap_or_default();

        let session = Session::from_map(loaded);
        call.insert(session.clone());

        let mut res = next.run(call).await;

        // Only re-issue when something changed: rewriting an unchanged cookie
        // on every response is noise, and it would extend the expiry of a
        // session the visitor is not actually using.
        if session.changed() {
            if let Some(value) = self.store.store(&session.snapshot()) {
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
