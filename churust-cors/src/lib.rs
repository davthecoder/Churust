//! Cross-Origin Resource Sharing (CORS) plugin for the [Churust] web framework.
//!
//! This crate provides a [`Cors`] plugin that intercepts every incoming HTTP
//! request and attaches the appropriate `Access-Control-*` response headers.
//! Preflight `OPTIONS` requests are short-circuited with an HTTP 204 response
//! so they never reach your route handlers.
//!
//! # Quick start
//!
//! Install the plugin via [`churust_core::Churust::server`] before calling
//! `.build()`.  Use [`Cors::permissive`] for development or use [`Cors::new`]
//! to build a precise policy for production.
//!
//! ```
//! use churust_core::{Churust, Call, TestClient};
//! use churust_cors::Cors;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(Cors::permissive())
//!     .routing(|r| {
//!         r.get("/api/data", |_c: Call| async { "hello" });
//!     })
//!     .build();
//!
//! // Actual cross-origin GET: response carries the CORS header.
//! let res = TestClient::new(app)
//!     .get("/api/data")
//!     .header("origin", "https://example.com")
//!     .send()
//!     .await;
//!
//! assert_eq!(res.status().as_u16(), 200);
//! assert_eq!(res.header("access-control-allow-origin"), Some("*"));
//! # });
//! ```
//!
//! [Churust]: churust_core::Churust

#![deny(missing_docs)]

use async_trait::async_trait;
use churust_core::{AppBuilder, Call, Middleware, Next, Phase, Plugin, Response};
use http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_METHOD, VARY,
};
use http::{HeaderValue, Method, StatusCode};
use std::sync::Arc;

/// Which origins are allowed.
#[derive(Debug, Clone)]
enum AllowOrigin {
    Any,
    List(Vec<String>),
}

/// CORS configuration and plugin entry point.
///
/// `Cors` holds the policy that governs which origins, HTTP methods, and
/// request headers are permitted for cross-origin requests.  It implements
/// [`Plugin`], so you pass it directly to [`AppBuilder::install`] — the plugin
/// system registers a [`Middleware`] that runs on every request.
///
/// # Choosing a constructor
///
/// | Situation | Constructor |
/// |-----------|-------------|
/// | Local development, all origins OK | [`Cors::permissive`] |
/// | Staging / production, specific origins | [`Cors::new`] + builder methods |
///
/// # Example
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// use churust_cors::Cors;
/// use http::Method;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(
///         Cors::new()
///             .allow_origin("https://app.example.com")
///             .allow_methods(vec![Method::GET, Method::POST])
///             .allow_headers(vec!["Content-Type".into(), "Authorization".into()])
///             .allow_credentials(true)
///             .max_age(3600),
///     )
///     .routing(|r| {
///         r.get("/", |_c: Call| async { "ok" });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .get("/")
///     .header("origin", "https://app.example.com")
///     .send()
///     .await;
///
/// assert_eq!(res.status().as_u16(), 200);
/// assert_eq!(
///     res.header("access-control-allow-origin"),
///     Some("https://app.example.com")
/// );
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct Cors {
    origin: AllowOrigin,
    methods: Vec<Method>,
    headers: Vec<String>,
    credentials: bool,
    max_age: Option<u64>,
}

impl Cors {
    /// Creates a permissive CORS policy suitable for development and public APIs.
    ///
    /// The policy allows **any** origin (`*`), the six most common HTTP methods
    /// (`GET`, `POST`, `PUT`, `DELETE`, `PATCH`, `OPTIONS`), any request header
    /// (`*`), and caches the preflight response for 24 hours (86 400 seconds).
    ///
    /// > **Note:** Per the CORS specification, `Access-Control-Allow-Origin: *`
    /// > cannot be combined with `Access-Control-Allow-Credentials: true`.
    /// > Therefore `permissive()` intentionally leaves credentials **disabled**.
    /// > If your application needs cookies or HTTP authentication on cross-origin
    /// > requests, use [`Cors::new`] with an explicit origin list and call
    /// > [`.allow_credentials(true)`](Cors::allow_credentials).
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(Cors::permissive())
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "hello" });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .get("/")
    ///     .header("origin", "https://any-origin.example")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(res.header("access-control-allow-origin"), Some("*"));
    /// # });
    /// ```
    pub fn permissive() -> Self {
        Self {
            origin: AllowOrigin::Any,
            methods: vec![
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::PATCH,
                Method::OPTIONS,
            ],
            headers: vec!["*".to_string()],
            credentials: false,
            max_age: Some(86_400),
        }
    }

    /// Creates a restrictive CORS policy with safe defaults.
    ///
    /// The initial policy allows **no origins**, permits only `GET` and `POST`,
    /// exposes no extra headers, disables credentials, and sets no `max-age`.
    /// Use the builder methods to refine the policy before passing it to
    /// [`AppBuilder::install`].
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// // Without adding any allowed origins, cross-origin requests receive
    /// // no CORS headers and the browser will block the response.
    /// let app = Churust::server()
    ///     .install(Cors::new().allow_origin("https://trusted.example.com"))
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "ok" });
    ///     })
    ///     .build();
    ///
    /// // Unlisted origin → no CORS header.
    /// let res = TestClient::new(app)
    ///     .get("/")
    ///     .header("origin", "https://untrusted.example.com")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(res.header("access-control-allow-origin"), None);
    /// # });
    /// ```
    pub fn new() -> Self {
        Self {
            origin: AllowOrigin::List(Vec::new()),
            methods: vec![Method::GET, Method::POST],
            headers: Vec::new(),
            credentials: false,
            max_age: None,
        }
    }

    /// Adds a single origin that is allowed to make cross-origin requests.
    ///
    /// Call this method multiple times to whitelist several origins.  The value
    /// should be a fully-qualified origin string such as
    /// `"https://app.example.com"` (scheme + host + optional port, **no**
    /// trailing slash).
    ///
    /// If the policy was previously set to [`Cors::permissive`] (wildcard
    /// origin), calling `allow_origin` switches back to an explicit list
    /// containing only the supplied origin.
    ///
    /// # Parameters
    ///
    /// - `origin` — any type that converts to [`String`], e.g. `&str` or
    ///   `String`.  The value is compared verbatim against the `Origin` request
    ///   header.
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(
    ///         Cors::new()
    ///             .allow_origin("https://frontend.example.com")
    ///             .allow_origin("https://mobile.example.com"),
    ///     )
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "ok" });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .get("/")
    ///     .header("origin", "https://frontend.example.com")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(
    ///     res.header("access-control-allow-origin"),
    ///     Some("https://frontend.example.com")
    /// );
    /// # });
    /// ```
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        match &mut self.origin {
            AllowOrigin::List(v) => v.push(origin.into()),
            AllowOrigin::Any => {
                self.origin = AllowOrigin::List(vec![origin.into()]);
            }
        }
        self
    }

    /// Replaces the list of HTTP methods advertised in preflight responses.
    ///
    /// The supplied `methods` are joined with `", "` and sent as the
    /// `Access-Control-Allow-Methods` header in response to `OPTIONS` preflight
    /// requests.  This call **replaces** the current list entirely — it does
    /// not append.
    ///
    /// The default (from [`Cors::new`]) is `[GET, POST]`.
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    /// use http::Method;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(
    ///         Cors::new()
    ///             .allow_origin("https://example.com")
    ///             .allow_methods(vec![Method::GET, Method::POST, Method::DELETE]),
    ///     )
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "ok" });
    ///     })
    ///     .build();
    ///
    /// // Send a preflight for DELETE.
    /// let res = TestClient::new(app)
    ///     .request(Method::OPTIONS, "/")
    ///     .header("origin", "https://example.com")
    ///     .header("access-control-request-method", "DELETE")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(res.status().as_u16(), 204);
    /// let allowed = res.header("access-control-allow-methods").unwrap_or("");
    /// assert!(allowed.contains("DELETE"));
    /// # });
    /// ```
    pub fn allow_methods(mut self, methods: Vec<Method>) -> Self {
        self.methods = methods;
        self
    }

    /// Replaces the list of request headers advertised in preflight responses.
    ///
    /// The supplied header names are joined with `", "` and sent as the
    /// `Access-Control-Allow-Headers` header in response to `OPTIONS` preflight
    /// requests.  Pass `["*"]` to allow any header (note that this is a literal
    /// wildcard string, not a glob pattern — its meaning is defined by the
    /// browser's CORS implementation).
    ///
    /// An empty list (the default from [`Cors::new`]) omits the
    /// `Access-Control-Allow-Headers` header entirely from preflight responses.
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    /// use http::Method;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(
    ///         Cors::new()
    ///             .allow_origin("https://example.com")
    ///             .allow_headers(vec!["Content-Type".into(), "X-Api-Key".into()]),
    ///     )
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "ok" });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .request(Method::OPTIONS, "/")
    ///     .header("origin", "https://example.com")
    ///     .header("access-control-request-method", "GET")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(res.status().as_u16(), 204);
    /// let hdrs = res.header("access-control-allow-headers").unwrap_or("");
    /// assert!(hdrs.contains("X-Api-Key"));
    /// # });
    /// ```
    pub fn allow_headers(mut self, headers: Vec<String>) -> Self {
        self.headers = headers;
        self
    }

    /// Controls whether the `Access-Control-Allow-Credentials: true` header is
    /// sent.
    ///
    /// Set this to `true` when your API relies on cookies, HTTP authentication,
    /// or TLS client certificates for cross-origin requests.  The browser only
    /// forwards credentials when **both** the server sets this header **and**
    /// the client sets `XMLHttpRequest.withCredentials = true` (or the
    /// `fetch` `credentials: "include"` option).
    ///
    /// > **CORS spec gotcha:** Credentials are incompatible with a wildcard
    /// > (`*`) `Allow-Origin`.  If you enable credentials, make sure the
    /// > policy lists explicit origins via [`allow_origin`](Cors::allow_origin)
    /// > rather than using [`Cors::permissive`].
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(
    ///         Cors::new()
    ///             .allow_origin("https://trusted.example.com")
    ///             .allow_credentials(true),
    ///     )
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "ok" });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .get("/")
    ///     .header("origin", "https://trusted.example.com")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(res.header("access-control-allow-credentials"), Some("true"));
    /// # });
    /// ```
    pub fn allow_credentials(mut self, yes: bool) -> Self {
        self.credentials = yes;
        self
    }

    /// Sets the `Access-Control-Max-Age` value (in seconds) for preflight caching.
    ///
    /// Browsers may cache a successful preflight response for up to `seconds`
    /// seconds, avoiding repeated `OPTIONS` round-trips for subsequent requests
    /// to the same endpoint.  The practical upper limit varies by browser (e.g.
    /// Chrome caps it at 7200 seconds; Firefox caps it at 86 400 seconds).
    ///
    /// If this method is not called (the default for [`Cors::new`]), the
    /// `Access-Control-Max-Age` header is omitted and the browser applies its
    /// own default (typically 5 seconds).
    ///
    /// # Parameters
    ///
    /// - `seconds` — cache duration as a non-negative integer.  A value of `0`
    ///   is legal and instructs browsers not to cache the preflight at all.
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use churust_cors::Cors;
    /// use http::Method;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(
    ///         Cors::new()
    ///             .allow_origin("https://example.com")
    ///             .max_age(3600),
    ///     )
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "ok" });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .request(Method::OPTIONS, "/")
    ///     .header("origin", "https://example.com")
    ///     .header("access-control-request-method", "GET")
    ///     .send()
    ///     .await;
    ///
    /// assert_eq!(res.header("access-control-max-age"), Some("3600"));
    /// # });
    /// ```
    pub fn max_age(mut self, seconds: u64) -> Self {
        self.max_age = Some(seconds);
        self
    }

    fn origin_allowed(&self, origin: &str) -> Option<String> {
        match &self.origin {
            AllowOrigin::Any => Some("*".to_string()),
            AllowOrigin::List(list) => {
                if list.iter().any(|o| o == origin) {
                    Some(origin.to_string())
                } else {
                    None
                }
            }
        }
    }

    fn apply_common(&self, res: &mut Response, allow_origin: &str) {
        res.headers.insert(
            ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(allow_origin).unwrap_or(HeaderValue::from_static("*")),
        );
        if self.credentials {
            res.headers.insert(
                ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }
        // Vary: Origin so caches don't serve the wrong CORS headers.
        res.headers.insert(VARY, HeaderValue::from_static("Origin"));
    }
}

impl Default for Cors {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Cors {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(Phase::Plugins, Arc::new(CorsMiddleware { cfg: *self }));
    }
}

struct CorsMiddleware {
    cfg: Cors,
}

#[async_trait]
impl Middleware for CorsMiddleware {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let origin = call.header("origin").map(|s| s.to_string());
        let is_preflight = *call.method() == Method::OPTIONS
            && call
                .header(ACCESS_CONTROL_REQUEST_METHOD.as_str())
                .is_some();

        // Preflight: short-circuit with 204 + CORS headers.
        if is_preflight {
            let mut res = Response::new(StatusCode::NO_CONTENT);
            if let Some(o) = origin.as_deref().and_then(|o| self.cfg.origin_allowed(o)) {
                self.cfg.apply_common(&mut res, &o);
                let methods = self
                    .cfg
                    .methods
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if let Ok(v) = HeaderValue::from_str(&methods) {
                    res.headers.insert(ACCESS_CONTROL_ALLOW_METHODS, v);
                }
                if !self.cfg.headers.is_empty() {
                    let hs = self.cfg.headers.join(", ");
                    if let Ok(v) = HeaderValue::from_str(&hs) {
                        res.headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, v);
                    }
                }
                if let Some(age) = self.cfg.max_age {
                    if let Ok(v) = HeaderValue::from_str(&age.to_string()) {
                        res.headers.insert(ACCESS_CONTROL_MAX_AGE, v);
                    }
                }
            }
            return res;
        }

        // Actual request: run the chain, then decorate the response.
        let mut res = next.run(call).await;
        if let Some(o) = origin.as_deref().and_then(|o| self.cfg.origin_allowed(o)) {
            self.cfg.apply_common(&mut res, &o);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};

    fn app() -> App {
        Churust::server()
            .install(Cors::permissive())
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build()
    }

    #[tokio::test]
    async fn actual_request_gets_allow_origin() {
        let client = TestClient::new(app());
        let res = client
            .get("/")
            .header("origin", "https://example.com")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.header("access-control-allow-origin"), Some("*"));
        assert_eq!(res.header("vary"), Some("Origin"));
    }

    #[tokio::test]
    async fn preflight_returns_204_with_methods() {
        let client = TestClient::new(app());
        let res = client
            .request(Method::OPTIONS, "/")
            .header("origin", "https://example.com")
            .header("access-control-request-method", "POST")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let methods = res.header("access-control-allow-methods").unwrap();
        assert!(methods.contains("POST"));
    }

    /// The core dispatcher answers an unclaimed `OPTIONS` with `204` plus an
    /// `Allow` header. That must not shadow CORS preflight, which needs to
    /// respond with `access-control-*` headers instead.
    ///
    /// Cors sits in the `Plugins` phase and the router in `Fallback`, so
    /// preflight short-circuits first — but that is an assumption about phase
    /// ordering, and this test is what keeps it true.
    #[tokio::test]
    async fn preflight_takes_priority_over_automatic_options() {
        let client = TestClient::new(app());
        let res = client
            .request(Method::OPTIONS, "/")
            .header("origin", "https://example.com")
            .header("access-control-request-method", "GET")
            .send()
            .await;

        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(
            res.header("access-control-allow-origin").is_some(),
            "CORS preflight was swallowed by the automatic OPTIONS handler"
        );
    }

    #[tokio::test]
    async fn disallowed_origin_gets_no_cors_header() {
        let app = Churust::server()
            .install(Cors::new().allow_origin("https://allowed.com"))
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build();
        let client = TestClient::new(app);
        let res = client
            .get("/")
            .header("origin", "https://evil.com")
            .send()
            .await;
        assert_eq!(res.header("access-control-allow-origin"), None);
    }
}
