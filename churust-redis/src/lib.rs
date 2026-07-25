//! Redis-backed sessions for the [Churust] web framework.
//!
//! [`RedisStore`] implements [`SessionStore`], so it drops into
//! [`Sessions::with_store`]:
//!
//! ```no_run
//! use churust_core::{Churust, Sessions};
//! use churust_redis::RedisStore;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let store = RedisStore::connect("redis://127.0.0.1/").await?;
//! let app = Churust::server()
//!     .install(Sessions::with_store(store.ttl(3600)))
//!     .build();
//! # let _ = app;
//! # Ok(())
//! # }
//! ```
//!
//! # What this buys over the cookie store
//!
//! Revocation, and room. [`CookieStore`](churust_core::CookieStore) keeps the
//! whole session in the visitor's cookie: it travels on every request, it is
//! readable by whoever holds it, and a copy taken before logout stays valid
//! until its signed deadline because nothing server-side records that it was
//! withdrawn. Here the cookie carries only an opaque identifier, the contents
//! live in Redis, and logging out deletes the record, so a stolen cookie stops
//! working the moment its owner signs out.
//!
//! The cost is an ordinary one: a network round trip per request that touches
//! the session, and a Redis to keep running.
//!
//! # Session identifiers
//!
//! 256 bits from the operating system's CSPRNG, base64url encoded. A session id
//! is a bearer credential for the whole account, so it is generated the same
//! way a token would be, and identifiers that do not match that shape are
//! refused on load without a round trip rather than passed through into a key.
//!
//! [Churust]: churust_core::Churust
//! [`Sessions::with_store`]: churust_core::Sessions::with_store

#![deny(missing_docs)]

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use churust_core::{SessionStore, SESSION_ID_KEY};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Bytes of entropy in a session identifier.
const ID_BYTES: usize = 32;
/// Encoded length of `ID_BYTES` in base64url without padding.
const ID_CHARS: usize = 43;
/// How long a session lives without being written, in seconds.
const DEFAULT_TTL: u64 = 24 * 60 * 60;
/// What every key is prefixed with, so a shared Redis stays legible.
const DEFAULT_PREFIX: &str = "churust:session:";

/// The key/value operations a session store needs.
///
/// Private on purpose: it exists so the store's own logic can be tested without
/// a running server, not as an extension point. A different backing store
/// should implement [`SessionStore`] directly.
#[async_trait]
trait Backend: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Option<String>;
    async fn set(&self, key: &str, value: &str, ttl: u64);
    async fn touch(&self, key: &str, ttl: u64);
    async fn del(&self, key: &str);
}

/// A [`SessionStore`] that keeps session contents in Redis.
#[derive(Clone)]
pub struct RedisStore {
    backend: Arc<dyn Backend>,
    prefix: String,
    ttl: u64,
    sliding: bool,
}

impl std::fmt::Debug for RedisStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisStore")
            .field("prefix", &self.prefix)
            .field("ttl", &self.ttl)
            .field("sliding", &self.sliding)
            .finish_non_exhaustive()
    }
}

impl RedisStore {
    /// Connect to `url` (for example `redis://127.0.0.1/`).
    ///
    /// The connection is established now and then multiplexed: one socket
    /// carries every session operation concurrently. If it drops, the next
    /// operation reconnects.
    ///
    /// # Errors
    ///
    /// If the URL does not parse or the first connection cannot be made.
    /// Connecting at startup rather than lazily is deliberate: a typo in the
    /// URL should stop the boot, not surface later as everybody being logged
    /// out.
    pub async fn connect(url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(url)?;
        // Prove the endpoint answers before returning a store that claims to
        // work, then keep the connection for the first request to use.
        let connection = client.get_multiplexed_async_connection().await?;
        Ok(Self::from_backend(RedisBackend {
            client,
            connection: tokio::sync::Mutex::new(Some(connection)),
        }))
    }

    /// Use an already-configured client.
    ///
    /// Reach for this when the connection needs settings `connect` does not
    /// expose. No connection is made until the first request touches a session.
    pub fn from_client(client: redis::Client) -> Self {
        Self::from_backend(RedisBackend {
            client,
            connection: tokio::sync::Mutex::new(None),
        })
    }

    fn from_backend(backend: impl Backend) -> Self {
        Self {
            backend: Arc::new(backend),
            prefix: DEFAULT_PREFIX.to_string(),
            ttl: DEFAULT_TTL,
            sliding: true,
        }
    }

    /// Expire a session `secs` after it was last touched (default 24 hours).
    ///
    /// # Panics
    ///
    /// If `secs` is zero, which would delete every session as it was written.
    pub fn ttl(mut self, secs: u64) -> Self {
        assert!(secs > 0, "session ttl must be at least one second");
        self.ttl = secs;
        self
    }

    /// Prefix every Redis key (default `churust:session:`).
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Whether reading a session extends its expiry (default `true`).
    ///
    /// Sliding keeps an active visitor signed in without writing the session on
    /// every request, at the cost of one `EXPIRE` per read. Turn it off for an
    /// absolute lifetime, where a session ends a fixed time after it started no
    /// matter how busy the visitor is.
    pub fn sliding(mut self, yes: bool) -> Self {
        self.sliding = yes;
        self
    }

    fn key(&self, id: &str) -> String {
        format!("{}{}", self.prefix, id)
    }
}

/// A fresh session identifier: 256 bits of OS entropy, base64url encoded.
///
/// # Panics
///
/// If the operating system cannot supply randomness. There is no safe fallback:
/// a predictable session id is a hijacked account, and continuing with a weak
/// one would be worse than not starting.
fn new_id() -> String {
    let mut bytes = [0u8; ID_BYTES];
    getrandom::fill(&mut bytes).expect("the OS must be able to supply randomness for a session id");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether `raw` has the shape this store issues.
///
/// Checked before the key is built so a hostile cookie cannot smuggle a colon,
/// a newline or a glob into a Redis key, and so junk costs no round trip.
fn is_well_formed(raw: &str) -> bool {
    raw.len() == ID_CHARS
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[async_trait]
impl SessionStore for RedisStore {
    async fn load(&self, raw: &str) -> Option<BTreeMap<String, String>> {
        if !is_well_formed(raw) {
            return None;
        }
        let key = self.key(raw);
        let stored = self.backend.get(&key).await?;
        let mut data: BTreeMap<String, String> = serde_json::from_str(&stored).ok()?;

        if self.sliding {
            self.backend.touch(&key, self.ttl).await;
        }

        // Put the identifier back so `store` knows which record it is
        // replacing, and so `Session::rotate` has something to remove.
        data.insert(SESSION_ID_KEY.to_string(), raw.to_string());
        Some(data)
    }

    async fn store(
        &self,
        data: &BTreeMap<String, String>,
        previous: Option<&str>,
    ) -> Option<String> {
        // An emptied session is a logout. Delete the record rather than writing
        // an empty one: the point of a server-side store is that the withdrawal
        // is real, not advisory.
        if data.is_empty() {
            if let Some(old) = previous.filter(|raw| is_well_formed(raw)) {
                self.backend.del(&self.key(old)).await;
            }
            return None;
        }

        let carried = data.get(SESSION_ID_KEY).filter(|id| is_well_formed(id));
        let id = match carried {
            Some(id) => id.clone(),
            None => new_id(),
        };

        // A rotated session (`Session::rotate`, which `Identity::login` calls)
        // arrives without its identifier, so a new one was just minted above.
        // The record it came from is withdrawn here, which is what stops a
        // planted session id from surviving a privilege change.
        if let Some(old) = previous.filter(|raw| is_well_formed(raw) && *raw != id) {
            self.backend.del(&self.key(old)).await;
        }

        // The identifier is the key; storing it in the value as well would be
        // one more thing that can disagree with itself.
        let mut payload = data.clone();
        payload.remove(SESSION_ID_KEY);
        let encoded = serde_json::to_string(&payload).ok()?;

        self.backend.set(&self.key(&id), &encoded, self.ttl).await;
        Some(id)
    }
}

/// The real backend: one multiplexed connection, reconnected on failure.
struct RedisBackend {
    client: redis::Client,
    /// `None` until the first successful connection, and set back to `None`
    /// whenever an operation fails so the next one redials.
    connection: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
}

impl RedisBackend {
    /// Run `cmd`, reconnecting once if the cached connection has gone away.
    ///
    /// Redis being down must not take the application with it: a session that
    /// cannot be read is an anonymous visitor, and a session that cannot be
    /// written is a visitor who has to sign in again. Both are worse than
    /// working and much better than a 500 on every route.
    async fn run<T: redis::FromRedisValue>(&self, cmd: &redis::Cmd) -> Option<T> {
        for attempt in 0..2 {
            let mut guard = self.connection.lock().await;
            if guard.is_none() {
                match self.client.get_multiplexed_async_connection().await {
                    Ok(fresh) => *guard = Some(fresh),
                    Err(_) => return None,
                }
            }
            // Cloning the multiplexed connection is how concurrent commands
            // share the socket, so the lock is released before the round trip.
            let mut conn = guard.as_ref()?.clone();
            drop(guard);

            match cmd.query_async::<T>(&mut conn).await {
                Ok(value) => return Some(value),
                Err(_) if attempt == 0 => {
                    // Drop the connection so the retry dials a new one.
                    *self.connection.lock().await = None;
                }
                Err(_) => return None,
            }
        }
        None
    }
}

#[async_trait]
impl Backend for RedisBackend {
    async fn get(&self, key: &str) -> Option<String> {
        self.run::<Option<String>>(redis::cmd("GET").arg(key))
            .await
            .flatten()
    }

    async fn set(&self, key: &str, value: &str, ttl: u64) {
        // SET with EX rather than SET then EXPIRE: one round trip, and no
        // window in which a session exists without a deadline.
        let _ = self
            .run::<()>(redis::cmd("SET").arg(key).arg(value).arg("EX").arg(ttl))
            .await;
    }

    async fn touch(&self, key: &str, ttl: u64) {
        let _ = self.run::<()>(redis::cmd("EXPIRE").arg(key).arg(ttl)).await;
    }

    async fn del(&self, key: &str) {
        let _ = self.run::<()>(redis::cmd("DEL").arg(key)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// An in-process stand-in for Redis, with expiry, so the store's own logic
    /// is covered without a server in the loop.
    #[derive(Default)]
    struct MemoryBackend {
        entries: Mutex<BTreeMap<String, (String, Instant)>>,
    }

    impl MemoryBackend {
        fn live(&self, key: &str) -> Option<String> {
            let entries = self.entries.lock().unwrap();
            let (value, expires) = entries.get(key)?;
            (*expires > Instant::now()).then(|| value.clone())
        }

        fn len(&self) -> usize {
            let now = Instant::now();
            self.entries
                .lock()
                .unwrap()
                .values()
                .filter(|(_, expires)| *expires > now)
                .count()
        }
    }

    #[async_trait]
    impl Backend for Arc<MemoryBackend> {
        async fn get(&self, key: &str) -> Option<String> {
            self.live(key)
        }

        async fn set(&self, key: &str, value: &str, ttl: u64) {
            self.entries.lock().unwrap().insert(
                key.to_string(),
                (value.to_string(), Instant::now() + Duration::from_secs(ttl)),
            );
        }

        async fn touch(&self, key: &str, ttl: u64) {
            if let Some(entry) = self.entries.lock().unwrap().get_mut(key) {
                entry.1 = Instant::now() + Duration::from_secs(ttl);
            }
        }

        async fn del(&self, key: &str) {
            self.entries.lock().unwrap().remove(key);
        }
    }

    fn store() -> (RedisStore, Arc<MemoryBackend>) {
        let backend = Arc::new(MemoryBackend::default());
        let store = RedisStore {
            backend: Arc::new(backend.clone()),
            prefix: DEFAULT_PREFIX.to_string(),
            ttl: DEFAULT_TTL,
            sliding: true,
        };
        (store, backend)
    }

    fn data(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn a_session_round_trips_through_the_backend() {
        let (store, _) = store();
        let id = store
            .store(&data(&[("user", "ana")]), None)
            .await
            .expect("a new session gets an id");

        let back = store.load(&id).await.expect("it must load again");
        assert_eq!(back.get("user").map(String::as_str), Some("ana"));
    }

    #[tokio::test]
    async fn the_cookie_carries_only_an_identifier() {
        let (store, backend) = store();
        let id = store
            .store(&data(&[("user", "ana"), ("secret", "hunter2")]), None)
            .await
            .unwrap();

        assert!(is_well_formed(&id));
        assert!(
            !id.contains("ana") && !id.contains("hunter2"),
            "session contents must not travel in the cookie: {id}"
        );
        let raw = backend.live(&format!("{DEFAULT_PREFIX}{id}")).unwrap();
        assert!(raw.contains("hunter2"), "the value lives server side");
    }

    #[tokio::test]
    async fn identifiers_are_unpredictable_and_distinct() {
        let (store, _) = store();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let id = store.store(&data(&[("k", "v")]), None).await.unwrap();
            assert_eq!(id.len(), ID_CHARS);
            assert!(seen.insert(id), "a session id was reused");
        }
    }

    #[tokio::test]
    async fn logging_out_deletes_the_record() {
        let (store, backend) = store();
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();
        assert_eq!(backend.len(), 1);

        // An emptied session is what `Session::clear` produces.
        let reissued = store.store(&BTreeMap::new(), Some(&id)).await;
        assert_eq!(reissued, None, "there is no new cookie to set on logout");
        assert_eq!(
            backend.len(),
            0,
            "the record must be gone, not merely stale"
        );
        assert!(
            store.load(&id).await.is_none(),
            "a cookie copied before logout must stop working"
        );
    }

    #[tokio::test]
    async fn rotating_mints_a_new_id_and_withdraws_the_old_one() {
        let (store, backend) = store();
        let first = store.store(&data(&[("cart", "3")]), None).await.unwrap();

        // What `Session::rotate` leaves behind: the contents, minus the id.
        let mut rotated = store.load(&first).await.unwrap();
        rotated.remove(SESSION_ID_KEY);
        rotated.insert("user".into(), "ana".into());

        let second = store.store(&rotated, Some(&first)).await.unwrap();
        assert_ne!(first, second, "a rotated session must change identifier");
        assert!(
            store.load(&first).await.is_none(),
            "the pre-login identifier must not still resolve"
        );
        let carried = store.load(&second).await.unwrap();
        assert_eq!(carried.get("cart").map(String::as_str), Some("3"));
        assert_eq!(carried.get("user").map(String::as_str), Some("ana"));
        assert_eq!(backend.len(), 1, "the old record was not left behind");
    }

    #[tokio::test]
    async fn an_unchanged_session_keeps_its_identifier() {
        let (store, backend) = store();
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();

        let mut loaded = store.load(&id).await.unwrap();
        loaded.insert("theme".into(), "dark".into());
        let again = store.store(&loaded, Some(&id)).await.unwrap();

        assert_eq!(id, again, "an ordinary write must not rotate the session");
        assert_eq!(backend.len(), 1);
    }

    #[tokio::test]
    async fn a_malformed_identifier_is_refused_without_a_lookup() {
        let (store, _) = store();
        for hostile in [
            "",
            "short",
            "../../etc/passwd",
            "churust:session:*",
            "a b",
            &"x".repeat(4096),
        ] {
            assert!(!is_well_formed(hostile), "{hostile:?} should not be valid");
            assert!(store.load(hostile).await.is_none());
        }
    }

    #[tokio::test]
    async fn an_unknown_identifier_loads_nothing() {
        let (store, _) = store();
        assert!(store.load(&new_id()).await.is_none());
    }

    #[tokio::test]
    async fn the_stored_value_does_not_repeat_the_identifier() {
        let (store, backend) = store();
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();
        let raw = backend.live(&format!("{DEFAULT_PREFIX}{id}")).unwrap();
        assert!(
            !raw.contains(SESSION_ID_KEY),
            "the key is the identifier; storing it twice invites disagreement: {raw}"
        );
    }

    #[tokio::test]
    async fn expiry_removes_a_session() {
        let (mut store, _) = store();
        store.ttl = 1;
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();
        assert!(store.load(&id).await.is_some());

        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(
            store.load(&id).await.is_none(),
            "a session past its ttl must not load"
        );
    }

    #[tokio::test]
    async fn sliding_expiry_extends_on_read() {
        let (mut store, backend) = store();
        store.ttl = 2;
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();

        // Read across more than the original ttl, in steps shorter than it.
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(800)).await;
            assert!(store.load(&id).await.is_some());
        }
        assert_eq!(backend.len(), 1);
    }

    #[tokio::test]
    async fn absolute_expiry_does_not_extend_on_read() {
        let (mut store, _) = store();
        store.ttl = 1;
        store.sliding = false;
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();

        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(store.load(&id).await.is_some());
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            store.load(&id).await.is_none(),
            "reading must not have extended the deadline"
        );
    }

    #[tokio::test]
    async fn a_custom_prefix_is_applied() {
        let (mut store, backend) = store();
        store.prefix = "app:sess:".into();
        let id = store.store(&data(&[("user", "ana")]), None).await.unwrap();
        assert!(backend.live(&format!("app:sess:{id}")).is_some());
    }

    #[test]
    #[should_panic(expected = "at least one second")]
    fn a_zero_ttl_is_refused() {
        let (store, _) = store();
        let _ = store.ttl(0);
    }
}
