//! The per-request [`Call`] context — the single object every handler receives.

use crate::error::{Error, Result};
use crate::response::Response;
use crate::state::StateMap;
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use std::collections::HashMap;
use std::sync::Arc;

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
    params: HashMap<String, String>,
    body: Bytes,
    state: Arc<StateMap>,
    extensions: http::Extensions,
}

impl Call {
    /// Construct a Call from already-parsed request parts (used by the engine
    /// and the test harness alike).
    pub fn new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Self {
            method,
            uri,
            headers,
            params: HashMap::new(),
            body,
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
    pub(crate) fn set_params(&mut self, params: HashMap<String, String>) {
        self.params = params;
    }

    /// Injected by `App::process` before the pipeline runs.
    pub(crate) fn set_state(&mut self, state: Arc<StateMap>) {
        self.state = state;
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

    /// Iterate over the captured path parameters as `(name, value)` pairs.
    /// Iteration order is unspecified (the parameters are stored in a hash map).
    pub fn params_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.params.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The raw, unparsed value of path parameter `name`, or `None` if the route
    /// has no such parameter. Use [`param`](Call::param) to parse it into a
    /// typed value.
    pub fn param_raw(&self, name: &str) -> Option<&str> {
        self.params.get(name).map(|s| s.as_str())
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
    /// This consumes the body: a second call returns an empty buffer. It is
    /// `async` to leave room for future streaming bodies.
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
        std::mem::take(&mut self.body)
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
        let bytes = self.receive_bytes().await;
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
    /// assert_eq!(&res.body[..], b"ok");
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
        let mut p = HashMap::new();
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
