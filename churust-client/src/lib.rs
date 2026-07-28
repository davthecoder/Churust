//! An HTTP client for [Churust] applications.
//!
//! A service usually has to call other services: an identity provider, a
//! payment gateway, its own sibling. This is the client for that, built on the
//! same hyper the server side runs on, so a Churust binary carries one HTTP
//! implementation rather than two.
//!
//! ```no_run
//! use churust_client::Client;
//!
//! # async fn example() -> Result<(), churust_client::ClientError> {
//! let client = Client::new();
//! let res = client.get("http://127.0.0.1:8080/health").send().await?;
//!
//! assert_eq!(res.status().as_u16(), 200);
//! println!("{}", res.text()?);
//! # Ok(())
//! # }
//! ```
//!
//! # Scope
//!
//! Deliberately small: build a request, send it, read the response. Retries,
//! circuit breaking, service discovery and tracing propagation are policy, and
//! policy belongs to the application that knows what it is calling. What the
//! client does own is the part that is easy to get wrong on your own: pooled
//! connections, an enforced timeout, a bounded response body, and refusing to
//! follow a redirect into a different scheme.
//!
//! # Responses are bounded
//!
//! A response body is read into memory up to [`Client::max_response_bytes`]
//! (16 MiB by default) and refused past it. An unbounded read from a service
//! you do not control is how one slow dependency becomes your own out-of-memory
//! kill.
//!
//! # Compressed responses
//!
//! By default the client advertises `Accept-Encoding: gzip, deflate` and
//! transparently inflates those encodings, so a peer that compresses — CDNs,
//! many affiliate APIs — is not left as a bag of deflate bits on the caller.
//! The decompressed size is still bounded by [`Client::max_response_bytes`]:
//! a tiny compressed bomb that expands past the ceiling is refused, which is
//! the same limit that protects against an uncompressed flood. Disable with
//! [`Client::auto_decompress`]`(false)` when you need the raw bytes or want to
//! negotiate encoding yourself.
//!
//! [Churust]: https://docs.rs/churust

#![deny(missing_docs)]

use bytes::Bytes;
use flate2::read::{GzDecoder, ZlibDecoder};
use http::header::{
    HeaderName, HeaderValue, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH,
    CONTENT_TYPE, COOKIE, LOCATION, PROXY_AUTHORIZATION, TRANSFER_ENCODING, USER_AGENT,
};
use http::{HeaderMap, Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{self, Read, Write};
use std::time::Duration;

/// Default ceiling on a response body, in bytes.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Default per-request deadline.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default cap on redirects followed in one request.
const DEFAULT_MAX_REDIRECTS: usize = 10;

#[cfg(feature = "tls")]
type Connector = hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;
#[cfg(not(feature = "tls"))]
type Connector = hyper_util::client::legacy::connect::HttpConnector;

/// Everything that can go wrong sending a request.
#[derive(Debug)]
pub enum ClientError {
    /// The URL could not be parsed, or names a scheme this client cannot speak.
    Url(String),
    /// The request could not be built, usually an invalid header name or value.
    Request(String),
    /// The connection failed, or the peer did.
    Transport(String),
    /// The request did not finish inside its deadline.
    Timeout(Duration),
    /// The response body exceeded [`Client::max_response_bytes`].
    BodyTooLarge(usize),
    /// The body could not be read.
    Body(String),
    /// The body was not valid UTF-8, or did not deserialize.
    Decode(String),
    /// More redirects than [`Client::max_redirects`] allows.
    TooManyRedirects(usize),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(why) => write!(f, "invalid url: {why}"),
            Self::Request(why) => write!(f, "could not build request: {why}"),
            Self::Transport(why) => write!(f, "transport failure: {why}"),
            Self::Timeout(after) => write!(f, "request timed out after {after:?}"),
            Self::BodyTooLarge(limit) => {
                write!(f, "response body exceeded the {limit} byte limit")
            }
            Self::Body(why) => write!(f, "could not read response body: {why}"),
            Self::Decode(why) => write!(f, "could not decode response: {why}"),
            Self::TooManyRedirects(limit) => write!(f, "more than {limit} redirects"),
        }
    }
}

impl std::error::Error for ClientError {}

/// A pooled HTTP client.
///
/// Cloning is cheap and shares the connection pool, so build one per process
/// and clone it into whatever needs it. Building one per request would open a
/// fresh connection every time, which is the single most common way to make an
/// outbound call slow.
#[derive(Clone, Debug)]
pub struct Client {
    inner: HyperClient<Connector, Full<Bytes>>,
    timeout: Duration,
    max_response_bytes: usize,
    max_redirects: usize,
    user_agent: HeaderValue,
    default_headers: HeaderMap,
    /// When true (the default), advertise and decode gzip/deflate responses.
    auto_decompress: bool,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client with pooled connections, a 30 second timeout, a 16 MiB response
    /// ceiling, and redirect following up to 10 hops.
    pub fn new() -> Self {
        #[cfg(feature = "tls")]
        let connector = {
            let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
            // The HTTPS connector needs to be handed absolute URIs including
            // plaintext ones, so http stays enforceable rather than impossible.
            http.enforce_http(false);
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_or_http()
                .enable_all_versions()
                .wrap_connector(http)
        };
        #[cfg(not(feature = "tls"))]
        let connector = hyper_util::client::legacy::connect::HttpConnector::new();

        Self {
            inner: HyperClient::builder(TokioExecutor::new()).build(connector),
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            user_agent: HeaderValue::from_static(concat!("churust/", env!("CARGO_PKG_VERSION"))),
            default_headers: HeaderMap::new(),
            auto_decompress: true,
        }
    }

    /// Fail a request that has not finished after `d`.
    ///
    /// The deadline covers the whole request including redirects, not each hop,
    /// so a redirect chain cannot multiply the time a caller waits.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Refuse a response body larger than `bytes`.
    ///
    /// The ceiling applies to the **payload the caller sees**: after transparent
    /// decompression when that is enabled, so a compressed response cannot
    /// expand past this limit either.
    pub fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// Follow at most `n` redirects. Zero returns the redirect response itself.
    pub fn max_redirects(mut self, n: usize) -> Self {
        self.max_redirects = n;
        self
    }

    /// Advertise and decode `gzip` / `deflate` response bodies.
    ///
    /// Enabled by default. Set to `false` to leave `Content-Encoding` bodies
    /// untouched and stop sending `Accept-Encoding`.
    pub fn auto_decompress(mut self, enabled: bool) -> Self {
        self.auto_decompress = enabled;
        self
    }

    /// Replace the `User-Agent` sent with every request.
    ///
    /// # Errors
    ///
    /// If the value is not a valid header value.
    pub fn user_agent(mut self, value: &str) -> Result<Self, ClientError> {
        self.user_agent =
            HeaderValue::from_str(value).map_err(|e| ClientError::Request(e.to_string()))?;
        Ok(self)
    }

    /// Send a header with every request from this client.
    ///
    /// # Errors
    ///
    /// If the name or value is not valid.
    pub fn default_header(mut self, name: &str, value: &str) -> Result<Self, ClientError> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| ClientError::Request(e.to_string()))?;
        let value =
            HeaderValue::from_str(value).map_err(|e| ClientError::Request(e.to_string()))?;
        self.default_headers.insert(name, value);
        Ok(self)
    }

    /// Start a request with an arbitrary method.
    pub fn request(&self, method: Method, url: impl Into<String>) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            timeout: None,
            error: None,
        }
    }

    /// Start a `GET`.
    pub fn get(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Start a `POST`.
    pub fn post(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    /// Start a `PUT`.
    pub fn put(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::PUT, url)
    }

    /// Start a `PATCH`.
    pub fn patch(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }

    /// Start a `DELETE`.
    pub fn delete(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::DELETE, url)
    }

    /// Start a `HEAD`.
    pub fn head(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }
}

/// A request under construction. Finish it with [`send`](RequestBuilder::send).
#[derive(Debug)]
pub struct RequestBuilder {
    client: Client,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Bytes,
    timeout: Option<Duration>,
    /// The first construction error, reported by `send` rather than by every
    /// builder method, so a chain reads without a `?` at each link.
    error: Option<ClientError>,
}

impl RequestBuilder {
    /// Set a header, replacing any previous value.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(name), Ok(value)) => {
                self.headers.insert(name, value);
            }
            _ => self.fail(ClientError::Request(format!("invalid header {name}"))),
        }
        self
    }

    /// Set `Authorization: Bearer <token>`.
    pub fn bearer(self, token: &str) -> Self {
        self.header(AUTHORIZATION.as_str(), &format!("Bearer {token}"))
    }

    /// Append a query string built from `pairs`.
    ///
    /// Existing query parameters on the URL are kept; these are added after
    /// them.
    pub fn query<T: Serialize>(mut self, pairs: &T) -> Self {
        match serde_html_form::to_string(pairs) {
            Ok(encoded) if encoded.is_empty() => {}
            Ok(encoded) => {
                let separator = if self.url.contains('?') { '&' } else { '?' };
                self.url.push(separator);
                self.url.push_str(&encoded);
            }
            Err(e) => self.fail(ClientError::Request(e.to_string())),
        }
        self
    }

    /// Send `body` verbatim, with no `Content-Type` of its own.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Serialize `value` as JSON and set `Content-Type: application/json`.
    pub fn json<T: Serialize>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(encoded) => {
                self.body = Bytes::from(encoded);
                self.headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            }
            Err(e) => self.fail(ClientError::Request(e.to_string())),
        }
        self
    }

    /// Serialize `value` as a URL-encoded form body.
    pub fn form<T: Serialize>(mut self, value: &T) -> Self {
        match serde_html_form::to_string(value) {
            Ok(encoded) => {
                self.body = Bytes::from(encoded);
                self.headers.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
            }
            Err(e) => self.fail(ClientError::Request(e.to_string())),
        }
        self
    }

    /// Override the client's timeout for this request only.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// Record the first construction failure and keep the rest of the chain
    /// building, so the caller sees one error at `send` rather than a compile
    /// error at every link.
    fn fail(&mut self, error: ClientError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// Send the request and read the whole response.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]. A non-2xx status is **not** an error: it is a
    /// response, and whether a `404` is a failure is the caller's judgement.
    /// Use [`Response::error_for_status`] to opt into the other convention.
    pub async fn send(self) -> Result<Response, ClientError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let deadline = self.timeout.unwrap_or(self.client.timeout);
        let send = self.send_following();
        match tokio::time::timeout(deadline, send).await {
            Ok(result) => result,
            Err(_) => Err(ClientError::Timeout(deadline)),
        }
    }

    /// The request itself, redirects included. Wrapped in one timeout by
    /// [`send`](Self::send).
    async fn send_following(self) -> Result<Response, ClientError> {
        let RequestBuilder {
            client,
            method,
            url,
            headers,
            body,
            ..
        } = self;
        // Mutable because a cross-origin redirect drops credentials from it.
        let mut headers = headers;

        let mut uri: Uri = url.parse().map_err(|e| ClientError::Url(format!("{e}")))?;
        let mut method = method;
        let mut body = body;
        let mut hops = 0usize;

        // Header names dropped for the remainder of this exchange: a
        // credential once a redirect crossed an origin, an entity header once
        // a redirect flipped the method and emptied the body. Recorded rather
        // than merely removed, since the default headers are re-applied on
        // every hop and would otherwise put back whatever was taken out.
        let mut stripped: std::collections::HashSet<http::HeaderName> =
            std::collections::HashSet::new();

        loop {
            check_scheme(&uri)?;

            let mut request = Request::builder()
                .method(method.clone())
                .uri(uri.clone())
                .body(Full::new(body.clone()))
                .map_err(|e| ClientError::Request(e.to_string()))?;

            let target = request.headers_mut();
            for (name, value) in &client.default_headers {
                // A default header is still a credential if it is one, and
                // still describes a body if it is one, so anything this
                // exchange has decided to drop stays dropped here too.
                if stripped.contains(name) {
                    continue;
                }
                target.insert(name, value.clone());
            }
            for (name, value) in &headers {
                target.insert(name, value.clone());
            }
            target
                .entry(USER_AGENT)
                .or_insert_with(|| client.user_agent.clone());
            // Only advertise encodings we can actually inflate. Without this,
            // a peer that compresses by default hands back bytes the caller
            // cannot read; with it, decompression is the common path.
            if client.auto_decompress && !stripped.contains(&ACCEPT_ENCODING) {
                target
                    .entry(ACCEPT_ENCODING)
                    .or_insert_with(|| HeaderValue::from_static("gzip, deflate"));
            }

            let response = client
                .inner
                .request(request)
                .await
                .map_err(|e| ClientError::Transport(e.to_string()))?;

            let status = response.status();
            let redirect = matches!(
                status,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            );

            if redirect && client.max_redirects > 0 {
                if hops >= client.max_redirects {
                    return Err(ClientError::TooManyRedirects(client.max_redirects));
                }
                if let Some(next) = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string())
                {
                    let previous = uri.clone();
                    uri = resolve(&uri, &next)?;
                    // A credential is scoped to the origin it was issued for.
                    // Re-sending it because a server said `Location:` hands it
                    // to whatever host that server named — which is a
                    // credential-harvesting primitive, not a redirect. curl and
                    // reqwest both strip on a change of origin.
                    if !same_origin(&previous, &uri) {
                        for name in [AUTHORIZATION, COOKIE, PROXY_AUTHORIZATION] {
                            headers.remove(&name);
                            stripped.insert(name);
                        }
                    }
                    // A redirect must not walk a secure request down to
                    // plaintext, which would send whatever survived in the
                    // clear. `check_scheme` sees only the target, so the
                    // comparison has to happen here.
                    if previous.scheme_str() == Some("https") && uri.scheme_str() == Some("http") {
                        return Err(ClientError::Url(
                            "refusing a redirect from https to http".into(),
                        ));
                    }
                    // 303 always becomes a GET; 301 and 302 do in practice,
                    // which every browser and every other client settled on
                    // long ago. 307 and 308 preserve the method, which is the
                    // whole reason they exist.
                    if matches!(
                        status,
                        StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
                    ) && method != Method::HEAD
                    {
                        method = Method::GET;
                        body = Bytes::new();
                        // The headers that described the body have to leave
                        // with it. `json()` and `form()` set `Content-Type`,
                        // and re-applying it on the next hop sends a `GET`
                        // that announces `application/json` for a payload that
                        // no longer exists — a request contradicting itself,
                        // and one no other client sends: the Fetch standard
                        // deletes the request-body-header-names on precisely
                        // this transition, which is what curl, reqwest and
                        // tower-http all implement.
                        for name in [
                            CONTENT_TYPE,
                            CONTENT_LENGTH,
                            CONTENT_ENCODING,
                            TRANSFER_ENCODING,
                        ] {
                            headers.remove(&name);
                            stripped.insert(name);
                        }
                    }
                    hops += 1;
                    continue;
                }
            }

            let (parts, incoming) = response.into_parts();
            let collected = Limited::new(incoming, client.max_response_bytes)
                .collect()
                .await
                .map_err(|e| {
                    // `Limited` reports the overrun through its own error type,
                    // and the distinction matters: one is the peer misbehaving,
                    // the other is the network.
                    if e.downcast_ref::<http_body_util::LengthLimitError>()
                        .is_some()
                    {
                        ClientError::BodyTooLarge(client.max_response_bytes)
                    } else {
                        ClientError::Body(e.to_string())
                    }
                })?;

            let mut headers = parts.headers;
            let body = if client.auto_decompress {
                maybe_decompress(
                    &mut headers,
                    collected.to_bytes(),
                    client.max_response_bytes,
                )?
            } else {
                collected.to_bytes()
            };

            return Ok(Response {
                status: parts.status,
                headers,
                body,
            });
        }
    }
}

/// Inflate a gzip/deflate body when `Content-Encoding` says so, then strip the
/// headers that no longer describe the payload.
///
/// Unknown encodings are left alone so a future `br` peer still works once a
/// decoder is added; the caller then sees the raw stream and the header.
fn maybe_decompress(
    headers: &mut HeaderMap,
    body: Bytes,
    max_bytes: usize,
) -> Result<Bytes, ClientError> {
    let Some(raw) = headers.get(CONTENT_ENCODING).and_then(|v| v.to_str().ok()) else {
        return Ok(body);
    };
    // Take the first coding only — multi-layer stacks are rare on the open web
    // and supporting them without streaming is more code than they are worth.
    let coding = raw
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let decoded = match coding.as_str() {
        "gzip" | "x-gzip" => inflate_limited(GzDecoder::new(body.as_ref()), max_bytes)?,
        // RFC 9110 §8.4.1.2: "deflate" is the zlib wrapper (RFC 1950), not raw
        // DEFLATE. ZlibDecoder matches that; a peer that sends raw DEFLATE is
        // non-conformant and fails here rather than silently corrupting data.
        "deflate" => inflate_limited(ZlibDecoder::new(body.as_ref()), max_bytes)?,
        "identity" | "" => return Ok(body),
        _ => return Ok(body),
    };
    headers.remove(CONTENT_ENCODING);
    // Length described the compressed wire form; keep it and every length
    // check on the body is wrong by exactly the compression ratio.
    headers.remove(CONTENT_LENGTH);
    Ok(Bytes::from(decoded))
}

/// Read from `r` into a buffer that refuses to grow past `max_bytes`.
fn inflate_limited<R: Read>(mut r: R, max_bytes: usize) -> Result<Vec<u8>, ClientError> {
    let mut out = LimitedWriter {
        buf: Vec::new(),
        max: max_bytes,
    };
    match std::io::copy(&mut r, &mut out) {
        Ok(_) => Ok(out.buf),
        Err(e) if e.kind() == io::ErrorKind::Other && e.to_string().contains("body too large") => {
            Err(ClientError::BodyTooLarge(max_bytes))
        }
        Err(e) => Err(ClientError::Decode(format!("decompress: {e}"))),
    }
}

/// A [`Write`] that errors once the accumulated length would exceed `max`.
struct LimitedWriter {
    buf: Vec<u8>,
    max: usize,
}

impl Write for LimitedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len().saturating_add(data.len()) > self.max {
            return Err(io::Error::other("body too large"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Whether two URIs address the same origin: scheme, host and effective port.
///
/// The port is compared after defaulting, so `http://h` and `http://h:80` are
/// one origin, matching the web's own definition rather than a string compare.
fn same_origin(a: &Uri, b: &Uri) -> bool {
    fn port(u: &Uri) -> Option<u16> {
        u.port_u16().or(match u.scheme_str() {
            Some("http") => Some(80),
            Some("https") => Some(443),
            _ => None,
        })
    }
    a.scheme_str() == b.scheme_str() && a.host() == b.host() && port(a) == port(b)
}

/// Refuse a scheme the client cannot speak, before it reaches the connector.
fn check_scheme(uri: &Uri) -> Result<(), ClientError> {
    match uri.scheme_str() {
        Some("http") => Ok(()),
        #[cfg(feature = "tls")]
        Some("https") => Ok(()),
        #[cfg(not(feature = "tls"))]
        Some("https") => Err(ClientError::Url(
            "https needs the `tls` feature on churust-client".into(),
        )),
        Some(other) => Err(ClientError::Url(format!("unsupported scheme: {other}"))),
        None => Err(ClientError::Url("no scheme in url".into())),
    }
}

/// Whether a `Location` value opens with a scheme of its own, and is therefore
/// an absolute target to be taken whole rather than joined onto the current one.
///
/// This used to be `location.contains("://")`, which searches the entire value
/// for a substring that is perfectly ordinary *inside a query*. The return-to
/// parameter that every login flow carries — `/login?next=https://host/page` —
/// was therefore read as absolute, handed to `http` as-is, and parsed into a
/// scheme-less origin-form URI, which `check_scheme` then rejected with "no
/// scheme in url". A redirect the client should simply have followed became a
/// hard error, and it did so on exactly the shape a client meets most.
///
/// RFC 3986 §4.2 decides this structurally instead: a reference is absolute
/// only when its first segment, everything before the first `/`, `?` or `#`,
/// is a scheme followed by a colon. A leading delimiter means there is no
/// scheme at all, and a colon appearing after one belongs to the path, the
/// query or the fragment and says nothing about this reference.
fn names_its_own_scheme(location: &str) -> bool {
    let Some(boundary) = location.find([':', '/', '?', '#']) else {
        return false;
    };
    if location.as_bytes()[boundary] != b':' {
        return false;
    }
    // RFC 3986 §3.1: scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ). The
    // leading-ALPHA rule is what keeps a bare `8080:...` from being read as a
    // scheme, and it is why the grammar is worth spelling out rather than just
    // testing for a colon.
    let mut scheme = location[..boundary].chars();
    scheme.next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Resolve a `Location` value against the URL it came from.
///
/// Relative targets are joined onto the current origin. A target that names its
/// own scheme is taken as-is and then re-checked by [`check_scheme`], so a
/// redirect cannot walk an `https` request down to `http`, or into `file:`.
fn resolve(current: &Uri, location: &str) -> Result<Uri, ClientError> {
    if names_its_own_scheme(location) {
        return location
            .parse()
            .map_err(|e| ClientError::Url(format!("bad redirect target: {e}")));
    }

    let authority = current
        .authority()
        .ok_or_else(|| ClientError::Url("cannot resolve a relative redirect".into()))?;
    let scheme = current.scheme_str().unwrap_or("http");

    let joined = if location.starts_with('/') {
        format!("{scheme}://{authority}{location}")
    } else if location.starts_with('?') {
        // RFC 3986 §5.3: a reference that is only a query replaces the query
        // and keeps the path whole. Falling through to the relative-path arm
        // below would drop the last path segment instead, which used to be
        // masked for the `?next=https://...` case because the old `://` scan
        // sent it down the absolute branch and failed the request outright.
        format!("{scheme}://{authority}{}{location}", current.path())
    } else {
        let base = current
            .path()
            .rsplit_once('/')
            .map(|(head, _)| head)
            .unwrap_or("");
        format!("{scheme}://{authority}{base}/{location}")
    };

    joined
        .parse()
        .map_err(|e| ClientError::Url(format!("bad redirect target: {e}")))
}

/// A response, with its body already read.
#[derive(Clone, Debug)]
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Response {
    /// The status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// All response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// One header as a string, when it is valid UTF-8.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The raw body.
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }

    /// The body as text.
    ///
    /// # Errors
    ///
    /// If the body is not valid UTF-8.
    pub fn text(&self) -> Result<String, ClientError> {
        // Validate in place, then allocate once — avoid `to_vec` + from_utf8.
        std::str::from_utf8(&self.body)
            .map(str::to_owned)
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// The body deserialized from JSON.
    ///
    /// # Errors
    ///
    /// If the body is not valid JSON for `T`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, ClientError> {
        serde_json::from_slice(&self.body).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Turn a 4xx or 5xx into an error, keeping 2xx and 3xx as values.
    ///
    /// # Errors
    ///
    /// [`ClientError::Transport`] carrying the status and the first part of the
    /// body, which is usually where the reason is.
    pub fn error_for_status(self) -> Result<Self, ClientError> {
        if self.status.is_client_error() || self.status.is_server_error() {
            let preview: String = self.text().unwrap_or_default().chars().take(200).collect();
            return Err(ClientError::Transport(format!(
                "{}: {preview}",
                self.status
            )));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_redirect_resolves_against_the_current_path() {
        let current: Uri = "http://example.com/a/b".parse().unwrap();
        assert_eq!(
            resolve(&current, "c").unwrap().to_string(),
            "http://example.com/a/c"
        );
    }

    #[test]
    fn an_absolute_path_redirect_replaces_the_path() {
        let current: Uri = "http://example.com/a/b".parse().unwrap();
        assert_eq!(
            resolve(&current, "/z").unwrap().to_string(),
            "http://example.com/z"
        );
    }

    #[test]
    fn an_absolute_redirect_is_taken_whole() {
        let current: Uri = "http://example.com/a".parse().unwrap();
        assert_eq!(
            resolve(&current, "http://other.test/x")
                .unwrap()
                .to_string(),
            "http://other.test/x"
        );
    }

    #[test]
    fn a_relative_redirect_whose_query_contains_a_url_still_resolves() {
        // The `next=` return-to parameter is how every login flow remembers
        // where the visitor was going, and its value is a whole URL. Deciding
        // absoluteness by scanning the entire `Location` for `://` mistook this
        // for an absolute target and then failed the request outright.
        let current: Uri = "http://api.example.com/dashboard".parse().unwrap();
        assert_eq!(
            resolve(&current, "/login?next=https://api.example.com/dashboard")
                .unwrap()
                .to_string(),
            "http://api.example.com/login?next=https://api.example.com/dashboard"
        );
    }

    #[test]
    fn a_redirect_naming_a_scheme_without_a_double_slash_is_not_joined_onto_the_origin() {
        // `mailto:` carries no `//`, so the old `://` scan called it relative
        // and pasted it onto the current origin, turning a target the client
        // must refuse into a genuine request for
        // `http://example.com/mailto:ops@example.com`. Structurally it is
        // absolute, so it is now taken whole; `http` parses it as the
        // scheme-less, authority-only URI it is, and `check_scheme` refuses it
        // — which is the outcome a non-HTTP redirect target should have.
        let current: Uri = "http://example.com/a".parse().unwrap();
        let target = resolve(&current, "mailto:ops@example.com").unwrap();
        assert_eq!(target.to_string(), "mailto:ops@example.com");
        assert!(matches!(check_scheme(&target), Err(ClientError::Url(_))));
    }

    #[test]
    fn a_query_only_redirect_keeps_the_path_it_came_from() {
        // RFC 3986 §5.3: a reference that is nothing but a query replaces the
        // query and leaves the path whole, rather than being treated as the
        // last segment of a relative path.
        let current: Uri = "http://example.com/a/b".parse().unwrap();
        assert_eq!(
            resolve(&current, "?page=2").unwrap().to_string(),
            "http://example.com/a/b?page=2"
        );
    }

    #[test]
    fn a_scheme_the_client_cannot_speak_is_refused() {
        let uri: Uri = "ftp://example.com/x".parse().unwrap();
        assert!(matches!(check_scheme(&uri), Err(ClientError::Url(_))));
    }

    #[tokio::test]
    async fn a_file_url_never_reaches_the_connector() {
        // `file:///etc/passwd` does not even parse as an HTTP URI, so it is
        // refused before any of this crate's own checks. Worth pinning: a
        // client that reads local files on request is an SSRF primitive.
        let err = Client::new()
            .get("file:///etc/passwd")
            .send()
            .await
            .expect_err("a file url must not be fetched");
        assert!(matches!(err, ClientError::Url(_)), "{err:?}");
    }

    #[test]
    fn plain_http_is_allowed() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        assert!(check_scheme(&uri).is_ok());
    }

    #[cfg(not(feature = "tls"))]
    #[test]
    fn https_without_the_tls_feature_says_so() {
        let uri: Uri = "https://example.com/".parse().unwrap();
        match check_scheme(&uri) {
            Err(ClientError::Url(why)) => assert!(why.contains("tls")),
            other => panic!("expected a url error, got {other:?}"),
        }
    }

    #[test]
    fn query_pairs_are_appended_to_an_existing_query() {
        let client = Client::new();
        let req = client
            .get("http://example.com/search?a=1")
            .query(&[("b", "2")]);
        assert_eq!(req.url, "http://example.com/search?a=1&b=2");
    }

    #[test]
    fn query_pairs_open_a_query_when_there_is_none() {
        let client = Client::new();
        let req = client.get("http://example.com/search").query(&[("b", "2")]);
        assert_eq!(req.url, "http://example.com/search?b=2");
    }

    #[test]
    fn an_invalid_header_surfaces_at_send_not_at_the_builder() {
        let client = Client::new();
        let req = client.get("http://example.com/").header("bad header", "x");
        assert!(matches!(req.error, Some(ClientError::Request(_))));
    }

    #[test]
    fn json_sets_the_content_type() {
        let client = Client::new();
        let req = client
            .post("http://example.com/")
            .json(&serde_json::json!({"a": 1}));
        assert_eq!(req.headers.get(CONTENT_TYPE).unwrap(), "application/json");
        assert_eq!(req.body, Bytes::from(r#"{"a":1}"#));
    }

    #[test]
    fn error_for_status_keeps_success() {
        let ok = Response {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from("fine"),
        };
        assert!(ok.error_for_status().is_ok());
    }

    #[test]
    fn error_for_status_reports_the_body_of_a_failure() {
        let bad = Response {
            status: StatusCode::BAD_REQUEST,
            headers: HeaderMap::new(),
            body: Bytes::from("missing field: name"),
        };
        match bad.error_for_status() {
            Err(ClientError::Transport(why)) => {
                assert!(why.contains("400"));
                assert!(why.contains("missing field"));
            }
            other => panic!("expected a transport error, got {other:?}"),
        }
    }
}
