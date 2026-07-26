//! The per-request [`Call`] context — the single object every handler receives.

use crate::error::{Error, Result};
use crate::response::Response;
use crate::state::StateMap;
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use std::sync::Arc;
/// Captured path parameters, in the order the route captured them.
///
/// Order matters: `Path<(A, B)>` destructures positionally, so a `HashMap`
/// would make `/users/{id}/posts/{post}` ambiguous.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params(Vec<(String, String)>);

impl Params {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace `name`.
    pub fn insert(&mut self, name: String, value: String) {
        match self.0.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = value,
            None => self.0.push((name, value)),
        }
    }

    /// Drop `name`, if present.
    pub fn remove(&mut self, name: &str) {
        self.0.retain(|(k, _)| k != name);
    }

    /// The value for `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Pairs in capture order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Whether `name` was captured.
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// The value at `index`, in capture order.
    pub fn nth(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(|(_, v)| v.as_str())
    }

    /// How many were captured.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether none were captured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Discard every capture.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// The address the connection came from, seeded by the engine.
///
/// A newtype rather than a bare `SocketAddr` so it can live in the call's typed
/// extension map without colliding with anything else of that type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerAddr(pub std::net::SocketAddr);

/// A request body still arriving, as a stream of chunks.
pub type BodyStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes>> + Send + 'static>>;

/// Where a request body currently is: already in memory, or still arriving.
enum RequestBody {
    Buffered(Bytes),
    /// Behind a mutex so `Call` stays `Sync`. A boxed stream is `Send` but not
    /// `Sync`, and handler bounds require `Sync`; the mutex is uncontended
    /// because the stream is taken exactly once.
    Stream(std::sync::Mutex<Option<BodyStream>>),
}

impl std::fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestBody::Buffered(b) => write!(f, "Buffered({} bytes)", b.len()),
            RequestBody::Stream(_) => f.write_str("Stream"),
        }
    }
}

/// Per-request context: the single object a handler receives (Ktor-style).
///
/// A `Call` bundles the request method, URI, headers, captured path
/// parameters, the (buffered) body, a handle to shared application
/// [state](crate::StateMap), and a per-call extension map. Handlers receive it
/// either directly (`|c: Call| async { ... }`) or indirectly through
/// [extractors](crate::extract) such as [`Path`](crate::Path) and
/// [`Query`](crate::Query), which read from the `Call` for you.
///
/// Read-only accessors ([`method`](Call::method), [`uri`](Call::uri),
/// [`path`](Call::path), [`header`](Call::header), [`query`](Call::query),
/// [`param`](Call::param)) borrow `&self`; body consumption
/// ([`receive_bytes`](Call::receive_bytes), [`receive_text`](Call::receive_text))
/// takes `&mut self`. The [`insert`](Call::insert)/[`get`](Call::get) pair
/// passes typed values between middleware and downstream handlers.
///
/// Construct one with [`Call::new`] in unit tests; in production the engine and
/// the [`TestClient`](crate::TestClient) build it for you.
///
/// ```
/// use churust_core::Call;
/// use http::{HeaderMap, Method};
/// use bytes::Bytes;
///
/// let call = Call::new(
///     Method::GET,
///     "/search?q=rust".parse().unwrap(),
///     HeaderMap::new(),
///     Bytes::new(),
/// );
/// assert_eq!(call.method(), &Method::GET);
/// assert_eq!(call.path(), "/search");
/// assert_eq!(call.query("q").as_deref(), Some("rust"));
/// ```
#[derive(Debug)]
pub struct Call {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    params: Params,
    body: RequestBody,
    state: Arc<StateMap>,
    extensions: http::Extensions,
}

impl Call {
    /// Construct a Call from already-parsed request parts (used by the engine
    /// and the test harness alike).
    /// Build a call whose body is already in memory.
    ///
    /// The engine instead attaches the body as a stream, so a handler can read
    /// a large one incrementally — see [`Call::body_stream`].
    pub fn new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Self {
            method,
            uri,
            headers,
            params: Params::new(),
            body: RequestBody::Buffered(body),
            state: Arc::new(StateMap::default()),
            extensions: http::Extensions::new(),
        }
    }

    /// The request HTTP method.
    pub fn method(&self) -> &Method {
        &self.method
    }
    /// The full request URI (path, query, and any authority).
    pub fn uri(&self) -> &Uri {
        &self.uri
    }
    /// The request path, without the query string (e.g. `/users/42`).
    pub fn path(&self) -> &str {
        self.uri.path()
    }
    /// The full request header map.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The request's host, without the port.
    ///
    /// Reads the `Host` header first, falling back to the URI authority. Both
    /// spellings matter: HTTP/1.1 carries the host in a header, and HTTP/2
    /// removed that header in favour of the `:authority` pseudo-header, which
    /// lands in the URI. Anything that makes a decision about *which site* a
    /// request is for must consult both, or it silently stops matching on the
    /// protocol most clients now negotiate.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, HeaderValue, Method, header::HOST};
    /// use bytes::Bytes;
    ///
    /// let mut headers = HeaderMap::new();
    /// headers.insert(HOST, HeaderValue::from_static("example.com:8443"));
    /// let call = Call::new(Method::GET, "/".parse().unwrap(), headers, Bytes::new());
    /// assert_eq!(call.host().as_deref(), Some("example.com"));
    ///
    /// // HTTP/2: no Host header, authority in the URI.
    /// let h2 = Call::new(
    ///     Method::GET,
    ///     "https://example.com/".parse().unwrap(),
    ///     HeaderMap::new(),
    ///     Bytes::new(),
    /// );
    /// assert_eq!(h2.host().as_deref(), Some("example.com"));
    /// ```
    pub fn host(&self) -> Option<String> {
        let raw = self
            .header(http::header::HOST.as_str())
            .map(str::to_string)
            .or_else(|| self.uri.authority().map(|a| a.as_str().to_string()))?;
        // Strip any userinfo first.
        let after_at = raw.rsplit('@').next().unwrap_or(&raw);
        // An IPv6 literal is bracketed and full of colons, so the port cannot
        // be found by splitting on `:` — that yielded `"[2001"` and made every
        // host comparison fail for v6. The brackets are authority syntax, so
        // they come off with the port.
        let host = match after_at.strip_prefix('[') {
            Some(rest) => rest.split(']').next().unwrap_or(rest),
            None => after_at.split(':').next().unwrap_or(after_at),
        };
        (!host.is_empty()).then(|| host.to_string())
    }

    /// Replace the request URI.
    ///
    /// For middleware that rewrites the target — a path normaliser, a rewrite
    /// rule. Note that routing has already happened by the time middleware
    /// runs, so this changes what handlers and extractors *read*, not which
    /// handler was selected.
    pub fn set_uri(&mut self, uri: Uri) {
        self.uri = uri;
    }

    /// Mutable access to the request headers.
    ///
    /// Middleware that rewrites the request head needs this — a request-id
    /// layer that injects a header, or a proxy-header normaliser. Handlers see
    /// whatever the pipeline left here, so a middleware that edits headers is
    /// editing what every extractor downstream will read.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    /// The value of header `name` as a UTF-8 string, or `None` if the header is
    /// absent or its value is not valid UTF-8. Header name matching is
    /// case-insensitive.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method, header::ACCEPT, HeaderValue};
    /// use bytes::Bytes;
    ///
    /// let mut headers = HeaderMap::new();
    /// headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    /// let call = Call::new(Method::GET, "/".parse().unwrap(), headers, Bytes::new());
    /// assert_eq!(call.header("accept"), Some("application/json"));
    /// assert_eq!(call.header("x-missing"), None);
    /// ```
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The raw query string with the leading `?` stripped, or `""` if there is
    /// none. To deserialize the whole query into a struct, prefer the
    /// [`Query`](crate::Query) extractor.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    ///
    /// let call = Call::new(Method::GET, "/s?a=1&b=2".parse().unwrap(), HeaderMap::new(), Bytes::new());
    /// assert_eq!(call.query_string(), "a=1&b=2");
    /// ```
    pub fn query_string(&self) -> &str {
        self.uri.query().unwrap_or("")
    }

    /// The first value for query key `key`, percent- and `+`-decoded, or `None`
    /// if the key is absent.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    ///
    /// let call = Call::new(Method::GET, "/s?q=hello+world".parse().unwrap(), HeaderMap::new(), Bytes::new());
    /// assert_eq!(call.query("q").as_deref(), Some("hello world"));
    /// assert_eq!(call.query("missing"), None);
    /// ```
    pub fn query(&self, key: &str) -> Option<String> {
        form_urlencoded_first(self.query_string(), key)
    }

    /// Set by the router after a successful match.
    pub(crate) fn set_params(&mut self, params: Params) {
        self.params = params;
    }

    /// Injected by `App::process` before the pipeline runs.
    pub(crate) fn set_state(&mut self, state: Arc<StateMap>) {
        self.state = state;
    }

    /// The address the connection came from.
    ///
    /// `None` when the call did not come from the engine — a `TestClient`
    /// request, for instance, has no socket behind it.
    ///
    /// This is the *socket* peer. Behind a reverse proxy it is the proxy, not
    /// the client: consult `X-Forwarded-For` only after checking this against
    /// the addresses you actually trust, since the header is client-supplied
    /// and trivially forged.
    pub fn peer_addr(&self) -> Option<std::net::SocketAddr> {
        self.get::<PeerAddr>().map(|p| p.0)
    }

    /// The value of request cookie `name`, percent-decoded.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, HeaderValue, Method};
    /// use bytes::Bytes;
    /// let mut headers = HeaderMap::new();
    /// headers.insert(http::header::COOKIE, HeaderValue::from_static("a=1; b=two"));
    /// let call = Call::new(Method::GET, "/".parse().unwrap(), headers, Bytes::new());
    /// assert_eq!(call.cookie("b").as_deref(), Some("two"));
    /// assert_eq!(call.cookie("missing"), None);
    /// ```
    pub fn cookie(&self, name: &str) -> Option<String> {
        // Every `Cookie` field, not just the first. HTTP/2 permits a client to
        // send cookie crumbs as several separate header fields (RFC 9113
        // §8.2.3), and an HTTP/1.1 client may send more than one `Cookie` line
        // too. Reading only the first meant a session cookie that happened to
        // land in the second field was invisible, so every request looked
        // freshly anonymous — a silent logout on each request.
        self.headers
            .get_all(http::header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|raw| crate::cookie::find(raw, name))
            .map(crate::cookie::decode)
    }

    /// A copy of this call carrying `body`.
    ///
    /// Used by [`Either`](crate::Either), which must be able to hand the same
    /// request to a second extractor after the first declines it.
    pub(crate) fn clone_with_body(&self, body: Bytes) -> Call {
        let mut c = Call::new(
            self.method.clone(),
            self.uri.clone(),
            self.headers.clone(),
            body,
        );
        c.params = self.params.clone();
        c.state = self.state.clone();
        c.extensions = self.extensions.clone();
        c
    }

    /// A lightweight copy of the request for an error renderer.
    ///
    /// [`on_error`](crate::AppBuilder::on_error) needs to inspect the request,
    /// but running the rest of the pipeline consumes the call. This keeps the
    /// parts an error page can reasonably ask about — method, URI and headers —
    /// and deliberately not the body, which has usually been consumed by then
    /// and would be misleading to hand back.
    pub(crate) fn snapshot_for_error(&self) -> Call {
        Call::new(
            self.method.clone(),
            self.uri.clone(),
            self.headers.clone(),
            Bytes::new(),
        )
    }

    /// Merge externally-built extensions into this call (used by the engine to
    /// inject per-connection data such as a pending WebSocket upgrade). Existing
    /// entries of the same type are overwritten.
    pub(crate) fn seed_extensions(&mut self, ext: http::Extensions) {
        self.extensions.extend(ext);
    }

    /// A shared handle to application state of type `T`, or `None` if no value
    /// of that type was registered with
    /// [`AppBuilder::state`](crate::AppBuilder::state). For handler arguments,
    /// the [`State`](crate::State) extractor is usually more convenient.
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #[derive(Clone)]
    /// struct AppName(&'static str);
    ///
    /// let app = Churust::server()
    ///     .state(AppName("churust"))
    ///     .routing(|r| {
    ///         r.get("/", |c: Call| async move {
    ///             format!("app={}", c.state::<AppName>().unwrap().0)
    ///         });
    ///     })
    ///     .build();
    /// let res = TestClient::new(app).get("/").send().await;
    /// assert_eq!(res.text(), "app=churust");
    /// # });
    /// ```
    pub fn state<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.state.get::<T>()
    }

    /// Iterate over the captured path parameters as `(name, value)` pairs, in
    /// capture order.
    /// Pairs come back in the order the route captured them.
    pub fn params_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.params.iter()
    }

    /// The captured path parameters, in capture order.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// The raw, unparsed value of path parameter `name`, or `None` if the route
    /// has no such parameter. Use [`param`](Call::param) to parse it into a
    /// typed value.
    pub fn param_raw(&self, name: &str) -> Option<&str> {
        self.params.get(name)
    }

    /// Parse path parameter `name` into `T`.
    ///
    /// Returns a `400 Bad Request` [`Error`] if the parameter is
    /// missing or fails to parse.
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.get("/users/{id}", |c: Call| async move {
    ///             let id: u64 = c.param("id")?;
    ///             Ok::<_, churust_core::Error>(format!("user {id}"))
    ///         });
    ///     })
    ///     .build();
    /// let res = TestClient::new(app).get("/users/42").send().await;
    /// assert_eq!(res.text(), "user 42");
    /// # });
    /// ```
    pub fn param<T>(&self, name: &str) -> Result<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        let raw = self
            .param_raw(name)
            .ok_or_else(|| Error::bad_request(format!("missing path param `{name}`")))?;
        raw.parse::<T>()
            .map_err(|e| Error::bad_request(format!("bad path param `{name}`: {e}")))
    }

    /// Take the request body as raw [`Bytes`], leaving the call's body empty.
    ///
    /// This consumes the body: a second call returns an empty buffer.
    ///
    /// # Prefer [`try_receive_bytes`](Call::try_receive_bytes)
    ///
    /// **An empty return does not mean an empty body.** The body now arrives as
    /// a stream, and this method has no error channel, so a read that fails —
    /// most importantly one that exceeds `max_body_bytes` — is reported as zero
    /// bytes. A handler built on this answers `200` with whatever an empty body
    /// produces, where the caller should have seen `413 Payload Too Large`.
    ///
    /// Use [`try_receive_bytes`](Call::try_receive_bytes) and let `?` turn the
    /// failure into the right status. This method is kept for the case where
    /// the distinction genuinely does not matter.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let mut call = Call::new(Method::POST, "/".parse().unwrap(), HeaderMap::new(), Bytes::from("data"));
    /// assert_eq!(&call.receive_bytes().await[..], b"data");
    /// assert!(call.receive_bytes().await.is_empty()); // already consumed
    /// # });
    /// ```
    pub async fn receive_bytes(&mut self) -> Bytes {
        self.try_receive_bytes().await.unwrap_or_default()
    }

    /// Read the whole body, surfacing the size limit as an error.
    ///
    /// [`receive_bytes`](Call::receive_bytes) exists for callers that cannot
    /// report failure and yields an empty buffer instead; extractors should
    /// prefer this so an oversized body becomes `413` rather than a confusing
    /// deserialization error.
    pub async fn try_receive_bytes(&mut self) -> Result<Bytes> {
        match std::mem::replace(&mut self.body, RequestBody::Buffered(Bytes::new())) {
            RequestBody::Buffered(b) => Ok(b),
            RequestBody::Stream(cell) => {
                let Some(mut s) = cell.lock().ok().and_then(|mut g| g.take()) else {
                    return Ok(Bytes::new());
                };
                use futures_util::StreamExt;
                let mut buf = bytes::BytesMut::new();
                while let Some(chunk) = s.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(buf.freeze())
            }
        }
    }

    /// Take the body as a stream, without buffering it.
    ///
    /// This is how a large upload is processed without holding it in memory.
    /// Returns `None` if the body has already been consumed. A body that
    /// arrived buffered — from [`Call::new`] or a test client — yields a
    /// single-chunk stream, so a handler need not care which it got.
    ///
    /// The server-wide `max_body_bytes` still applies: exceeding it surfaces as
    /// an error item in the stream rather than a `413`, because the response
    /// has usually begun by then.
    pub fn body_stream(&mut self) -> Option<BodyStream> {
        match std::mem::replace(&mut self.body, RequestBody::Buffered(Bytes::new())) {
            RequestBody::Stream(cell) => cell.lock().ok().and_then(|mut g| g.take()),
            RequestBody::Buffered(b) if !b.is_empty() => {
                Some(Box::pin(futures_util::stream::once(async move { Ok(b) })))
            }
            RequestBody::Buffered(_) => None,
        }
    }

    /// Attach a streaming body. Used by the engine.
    pub(crate) fn with_body_stream(mut self, stream: BodyStream) -> Self {
        self.body = RequestBody::Stream(std::sync::Mutex::new(Some(stream)));
        self
    }

    /// Take the request body decoded as UTF-8, consuming it.
    ///
    /// Returns a `400 Bad Request` [`Error`] if the body is not
    /// valid UTF-8.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let mut call = Call::new(Method::POST, "/".parse().unwrap(), HeaderMap::new(), Bytes::from("ping"));
    /// assert_eq!(call.receive_text().await.unwrap(), "ping");
    /// # });
    /// ```
    pub async fn receive_text(&mut self) -> Result<String> {
        // `try_receive_bytes`, not `receive_bytes`: the latter swallows a read
        // error into an empty payload, which would turn an over-limit body into
        // an empty string rather than a `413`.
        let bytes = self.try_receive_bytes().await?;
        crate::extract::check_body_limit(self, bytes.len())?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::bad_request(format!("body is not valid UTF-8: {e}")))
    }

    /// Insert a per-call typed value into the call's extension map, keyed by its
    /// type. Typically used by a middleware to attach context (e.g. an
    /// authenticated principal) that a downstream extractor or handler reads via
    /// [`get`](Call::get). Inserting a second value of the same type replaces
    /// the first.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    ///
    /// #[derive(Clone, PartialEq, Debug)]
    /// struct UserId(u32);
    ///
    /// let mut call = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
    /// call.insert(UserId(7));
    /// assert_eq!(call.get::<UserId>(), Some(UserId(7)));
    /// ```
    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, value: T) {
        self.extensions.insert(value);
    }

    /// Get a clone of the per-call value of type `T` previously stored with
    /// [`insert`](Call::insert), or `None` if none was stored.
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    ///
    /// let call = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
    /// assert_eq!(call.get::<u32>(), None);
    /// ```
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.extensions.get::<T>().cloned()
    }

    // ---- response convenience (sync; body is in memory) ----

    /// Build a `200 OK` `text/plain` [`Response`] — a shorthand for
    /// [`Response::text`].
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    ///
    /// let call = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
    /// let res = call.respond_text("ok");
    /// assert_eq!(res.body.as_slice(), Some(&b"ok"[..]));
    /// ```
    pub fn respond_text(&self, body: impl Into<String>) -> Response {
        Response::text(body)
    }

    /// Build an empty-bodied [`Response`] with the given status — a shorthand
    /// for [`Response::new`].
    ///
    /// ```
    /// use churust_core::Call;
    /// use http::{HeaderMap, Method, StatusCode};
    /// use bytes::Bytes;
    ///
    /// let call = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
    /// assert_eq!(call.respond_status(StatusCode::ACCEPTED).status, StatusCode::ACCEPTED);
    /// ```
    pub fn respond_status(&self, status: StatusCode) -> Response {
        Response::new(status)
    }
}

fn form_urlencoded_first(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        if k == key {
            let v = it.next().unwrap_or("");
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    // Minimal `+` and %XX decoding sufficient for v1 query parsing.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(path: &str, body: &str) -> Call {
        Call::new(
            Method::GET,
            path.parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::from(body.to_string()),
        )
    }

    #[test]
    fn reads_method_and_path() {
        let c = call("/a/b?x=1", "");
        assert_eq!(c.method(), &Method::GET);
        assert_eq!(c.path(), "/a/b");
    }

    #[test]
    fn parses_query() {
        let c = call("/s?q=hello+world&n=5", "");
        assert_eq!(c.query("q").as_deref(), Some("hello world"));
        assert_eq!(c.query("n").as_deref(), Some("5"));
        assert_eq!(c.query("missing"), None);
    }

    #[test]
    fn parses_query_percent_encoded() {
        let c = call("/s?q=hello%20world", "");
        assert_eq!(c.query("q").as_deref(), Some("hello world"));
    }

    #[test]
    fn parses_path_param() {
        let mut c = call("/users/42", "");
        let mut p = crate::call::Params::new();
        p.insert("id".to_string(), "42".to_string());
        c.set_params(p);
        let id: u64 = c.param("id").unwrap();
        assert_eq!(id, 42);
        assert!(c.param::<u64>("missing").is_err());
    }

    #[tokio::test]
    async fn receives_text_body() {
        let mut c = call("/", "payload");
        assert_eq!(c.receive_text().await.unwrap(), "payload");
    }

    #[test]
    fn state_round_trips() {
        use crate::state::StateMap;
        let mut c = call("/", "");
        let mut sm = StateMap::default();
        sm.insert(99u32);
        c.set_state(std::sync::Arc::new(sm));
        assert_eq!(*c.state::<u32>().unwrap(), 99);
    }

    #[test]
    fn extensions_round_trip() {
        #[derive(Clone, PartialEq, Debug)]
        struct User(u32);
        let mut c = call("/", "");
        assert!(c.get::<User>().is_none());
        c.insert(User(7));
        assert_eq!(c.get::<User>(), Some(User(7)));
    }
}
