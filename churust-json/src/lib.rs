//! JSON support for the [Churust](https://docs.rs/churust) web framework.
//!
//! This crate provides two things:
//!
//! * [`Json<T>`] — a dual-purpose wrapper that **deserializes** a JSON request
//!   body when used as a handler argument (via [`FromCall`]) and **serializes**
//!   `T` to a `application/json` response when used as a handler return value
//!   (via [`IntoResponse`]).
//!
//! * [`ContentNegotiation`] — an optional [`Plugin`] that intercepts plain-text
//!   error responses (status ≥ 400) and re-encodes them as structured JSON so
//!   that API clients always receive a consistent `{"error":"…","status":N}`
//!   body.
//!
//! # Quick start
//!
//! ```
//! use churust_core::{Churust, Call, TestClient};
//! use churust_json::{Json, ContentNegotiation};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Greeting { name: String }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(ContentNegotiation::new())
//!     .routing(|r| {
//!         r.post("/greet", |Json(body): Json<Greeting>| async move {
//!             Json(Greeting { name: format!("Hello, {}!", body.name) })
//!         });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app)
//!     .post("/greet")
//!     .header("content-type", "application/json")
//!     .body(r#"{"name":"Alice"}"#)
//!     .send()
//!     .await;
//!
//! assert_eq!(res.status().as_u16(), 200);
//! assert_eq!(res.header("content-type"), Some("application/json"));
//! # });
//! ```

#![deny(missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
use churust_core::{
    AppBuilder, Call, Error, FromCall, IntoResponse, Middleware, Next, Phase, Plugin, Response,
    Result,
};
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::{HeaderValue, Method};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;

/// A JSON extractor and responder.
///
/// `Json<T>` wraps any value `T` and grants it two complementary roles inside
/// a Churust handler:
///
/// ## As an extractor (handler argument)
///
/// When placed as the **last** handler argument, `Json<T>` reads the entire
/// request body and attempts to deserialize it as JSON into `T`.  `T` must
/// implement [`serde::de::DeserializeOwned`].
///
/// The request must declare a JSON content type — `application/json`, or any
/// `+json` structured suffix. Anything else is **415 Unsupported Media Type**,
/// which is what keeps a cross-origin HTML form (whose three possible content
/// types send no CORS preflight) from reaching a JSON handler with the
/// visitor's cookies attached.
///
/// If the body is missing or malformed the framework returns an HTTP **400
/// Bad Request** response automatically — the handler is never called.
///
/// ```
/// use churust_core::{Churust, TestClient};
/// use churust_json::Json;
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct CreateUser { username: String }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/users", |Json(body): Json<CreateUser>| async move {
///             format!("created {}", body.username)
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .post("/users")
///     .header("content-type", "application/json")
///     .body(r#"{"username":"bob"}"#)
///     .send()
///     .await;
/// assert_eq!(res.status().as_u16(), 200);
/// # });
/// ```
///
/// ## As a responder (return value)
///
/// Wrapping any `T: Serialize` in `Json` and returning it from a handler
/// serializes `T` to JSON and sets the `Content-Type` header to
/// `application/json`.
///
/// If serialization fails at runtime (extremely rare for well-formed types)
/// the framework returns an HTTP **500 Internal Server Error**.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// use churust_json::Json;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Status { ok: bool }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.get("/status", |_c: Call| async { Json(Status { ok: true }) });
///     })
///     .build();
///
/// let res = TestClient::new(app).get("/status").send().await;
/// assert_eq!(res.status().as_u16(), 200);
/// assert_eq!(res.header("content-type"), Some("application/json"));
/// # });
/// ```
///
/// ## Pattern destructuring
///
/// The inner value is accessed through `.0` or via pattern destructuring:
///
/// ```
/// use churust_json::Json;
/// use serde::Deserialize;
///
/// #[derive(Deserialize, Debug, PartialEq)]
/// struct Point { x: f64, y: f64 }
///
/// // Pattern destructuring extracts the inner value directly.
/// let Json(point) = Json(Point { x: 1.0, y: 2.0 });
/// assert_eq!(point, Point { x: 1.0, y: 2.0 });
/// ```
#[derive(Debug, Clone)]
pub struct Json<T>(
    /// The inner value being wrapped.
    ///
    /// Access it with `.0` or destructure: `let Json(value) = json_wrapper;`.
    pub T,
);

#[async_trait]
impl<T> FromCall for Json<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call(mut call: Call) -> Result<Self> {
        require_json_content_type(&call)?;
        let bytes = call.try_receive_bytes().await?;
        churust_core::check_body_limit(&call, bytes.len())?;
        let value = serde_json::from_slice::<T>(&bytes)
            .map_err(|e| Error::bad_request(format!("invalid JSON body: {e}")))?;
        Ok(Json(value))
    }
}

/// Refuse a body that is not declared as JSON.
///
/// Not merely tidiness. The three content types an HTML form can send —
/// `text/plain`, `application/x-www-form-urlencoded`, `multipart/form-data` —
/// make a *simple* cross-origin request: no preflight, so the CORS layer is
/// never consulted, and the browser attaches the victim's cookies. Deserialising
/// such a body as JSON makes every state-changing endpoint executable from any
/// site the victim visits. Requiring a JSON type forces a preflight, which is
/// what puts CORS back in the path.
///
/// `Form<T>` and `Multipart` have always done this; only the JSON path did not.
///
/// Structured suffixes (`application/problem+json`, `application/vnd.api+json`)
/// are JSON by definition and are accepted — they cannot be produced by a form.
fn require_json_content_type(call: &Call) -> Result<()> {
    let raw = call
        .header(http::header::CONTENT_TYPE.as_str())
        .unwrap_or("");
    // Compare the media type only: a charset parameter is legitimate.
    let media = raw.split(';').next().unwrap_or("").trim();
    let ok = media.eq_ignore_ascii_case("application/json")
        || media
            .rsplit_once('+')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("json"));
    if ok {
        return Ok(());
    }
    Err(Error::new(
        http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "expected a JSON content type",
    ))
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => Response::bytes("application/json", Bytes::from(bytes)),
            Err(e) => Error::internal(format!("JSON serialization failed: {e}")).into_response(),
        }
    }
}

/// Plugin that converts plain-text error responses into structured JSON.
///
/// When installed, `ContentNegotiation` wraps the request pipeline with a
/// middleware that inspects every outgoing response.  If the status is a
/// client error (4xx) or server error (5xx) **and** the `Content-Type` is
/// `text/plain`, it re-encodes the body as:
///
/// ```json
/// {"error": "<original message>", "status": <status code>}
/// ```
///
/// This ensures that API clients always receive a machine-readable JSON error
/// body rather than an opaque string, regardless of where in the framework the
/// error originated.
///
/// # Configuration
///
/// | Builder method | Default | Description |
/// |---|---|---|
/// | [`pretty`](ContentNegotiation::pretty) | `false` | Pretty-print the JSON error body |
///
/// # Example
///
/// ```
/// use churust_core::{Churust, Call, Error, TestClient};
/// use churust_json::ContentNegotiation;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(ContentNegotiation::new())
///     .routing(|r| {
///         r.get("/fail", |_c: Call| async {
///             Err::<&str, _>(Error::bad_request("something went wrong"))
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app).get("/fail").send().await;
/// assert_eq!(res.status().as_u16(), 400);
/// assert_eq!(res.header("content-type"), Some("application/json"));
///
/// let body: serde_json::Value = serde_json::from_slice(res.body_bytes()).unwrap();
/// assert_eq!(body["error"], "something went wrong");
/// assert_eq!(body["status"], 400);
/// # });
/// ```
#[derive(Debug, Clone, Default)]
pub struct ContentNegotiation {
    pretty: bool,
}

impl ContentNegotiation {
    /// Creates a new `ContentNegotiation` plugin with default settings.
    ///
    /// By default, JSON error bodies are compact (not pretty-printed).  Call
    /// [`pretty`](Self::pretty) on the returned value to change this.
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, Error, TestClient};
    /// use churust_json::ContentNegotiation;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(ContentNegotiation::new())
    ///     .routing(|r| {
    ///         r.get("/boom", |_c: Call| async {
    ///             Err::<&str, _>(Error::not_found("no such resource"))
    ///         });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app).get("/boom").send().await;
    /// assert_eq!(res.header("content-type"), Some("application/json"));
    /// # });
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Controls whether JSON error bodies are pretty-printed.
    ///
    /// When `pretty` is `true`, error responses are formatted with newlines and
    /// indentation, which is helpful during development or when error responses
    /// may be read by humans.  For production APIs, leave this at the default
    /// `false` to keep response sizes small.
    ///
    /// # Parameters
    ///
    /// * `pretty` — `true` to enable pretty-printing; `false` (the default) for
    ///   compact output.
    ///
    /// # Example
    ///
    /// ```
    /// use churust_core::{Churust, Call, Error, TestClient};
    /// use churust_json::ContentNegotiation;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(ContentNegotiation::new().pretty(true))
    ///     .routing(|r| {
    ///         r.get("/oops", |_c: Call| async {
    ///             Err::<&str, _>(Error::internal("disk full"))
    ///         });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app).get("/oops").send().await;
    /// assert_eq!(res.status().as_u16(), 500);
    /// // Pretty-printed JSON contains newlines.
    /// let text = res.text();
    /// assert!(text.contains('\n'));
    /// # });
    /// ```
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }
}

impl Plugin for ContentNegotiation {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(JsonErrors {
                pretty: self.pretty,
            }),
        );
    }
}

struct JsonErrors {
    pretty: bool,
}

#[async_trait]
impl Middleware for JsonErrors {
    async fn handle(&self, call: Call, next: Next) -> Response {
        // A `HEAD` with no handler of its own is answered by running the `GET`
        // route and then discarding the body, and that discarding happens at
        // the endpoint — inside this middleware, not outside it. So a `HEAD`
        // arrives back here already emptied, and the message this plugin exists
        // to re-encode is gone before it can be read. The method has to be
        // remembered now because `call` is consumed by the rest of the chain.
        let is_head = call.method() == Method::HEAD;
        let mut res = next.run(call).await;
        let is_error = res.status.is_client_error() || res.status.is_server_error();
        let is_text = res
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.starts_with("text/plain"))
            .unwrap_or(false);
        if is_error && is_text {
            match res.body.as_bytes() {
                // The ordinary case: a buffered plain-text message that becomes
                // a JSON envelope. The envelope is always longer than the text
                // it wraps, so any `Content-Length` already on the response now
                // describes a body that no longer exists. The endpoint sets one
                // when it strips a `HEAD`, and a handler is free to set one by
                // hand, so it is removed here and hyper is left to frame the
                // bytes actually being sent. Leaving the stale value behind is
                // not a cosmetic slip: hyper's HTTP/1 encoder asserts that a
                // supplied `Content-Length` matches the payload it is handed,
                // panics on the mismatch in a debug build, and the connection
                // dies with the task.
                Some(buffered) if !buffered.is_empty() => {
                    let msg = String::from_utf8_lossy(buffered).into_owned();
                    let body = serde_json::json!({ "error": msg, "status": res.status.as_u16() });
                    let bytes = if self.pretty {
                        serde_json::to_vec_pretty(&body)
                    } else {
                        serde_json::to_vec(&body)
                    }
                    .unwrap_or_default();
                    res.headers.remove(CONTENT_LENGTH);
                    res.body = churust_core::Body::from(bytes);
                    res.headers
                        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                }
                // A synthesized `HEAD`, whose body was stripped further in. Its
                // headers must still describe the representation the matching
                // `GET` would return, so the media type is corrected to the one
                // this middleware would have produced, but no body is invented:
                // re-encoding the empty string would give a `HEAD` reply the
                // `{"error":"","status":N}` payload it never had, and would
                // announce a message the `GET` does not contain. The stale
                // `Content-Length` goes too. It counts the plain text, and the
                // JSON length cannot be derived from it because escaping is not
                // length-preserving. RFC 9110 §9.3.2 lets a `HEAD` omit a field
                // it could only learn by generating the content, which is
                // exactly this; it does not let it quote a size the `GET` will
                // not deliver.
                Some(_) if is_head => {
                    res.headers.remove(CONTENT_LENGTH);
                    res.headers
                        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                }
                // An error that really did carry an empty plain-text body, or a
                // streamed one whose bytes were never buffered. Neither has a
                // message to lift into an envelope, so both are passed through
                // exactly as the handler wrote them.
                _ => {}
            }
        }
        res
    }
}

/// Call-style JSON helpers — v1 design §5.1.
///
/// The extractor [`Json<T>`] covers the typed-handler style. This trait covers
/// the `call`-style one, so both halves of Churust's hybrid API can speak JSON:
///
/// ```
/// use churust_core::{Call, Churust, TestClient};
/// use churust_json::CallJson;
/// use serde::{Deserialize, Serialize};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// #[derive(Deserialize, Serialize)]
/// struct Note { text: String }
///
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/echo", |mut call: Call| async move {
///             let note: Note = call.receive_json().await?;
///             Ok::<_, churust_core::Error>(call.respond_json(&note))
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .post("/echo")
///     .header("content-type", "application/json")
///     .body(r#"{"text":"hi"}"#)
///     .send()
///     .await;
/// assert_eq!(res.text(), r#"{"text":"hi"}"#);
/// # });
/// ```
///
/// Lives here rather than in `churust-core` so the core keeps no `serde_json`
/// dependency; bring it into scope with `use churust_json::CallJson`.
#[async_trait::async_trait]
pub trait CallJson {
    /// Deserialize the request body as JSON.
    ///
    /// Consumes the body. Fails with `400 Bad Request` if it does not parse
    /// into `T`.
    async fn receive_json<T: serde::de::DeserializeOwned>(&mut self) -> churust_core::Result<T>;

    /// Build a `200 OK` JSON response from `value`.
    ///
    /// Serialization failure yields a `500` rather than panicking: a type that
    /// cannot serialize is a bug in the application, not in the request.
    fn respond_json<T: serde::Serialize + Sync>(&self, value: &T) -> churust_core::Response;
}

#[async_trait::async_trait]
impl CallJson for churust_core::Call {
    async fn receive_json<T: serde::de::DeserializeOwned>(&mut self) -> churust_core::Result<T> {
        // Same rule as the `Json<T>` extractor: the two must not disagree about
        // what counts as a JSON request.
        require_json_content_type(self)?;
        // `try_receive_bytes` and the route limit, matching the `Json<T>`
        // extractor. `receive_bytes` swallows a read error into an empty
        // payload, which turned an over-limit body into
        // `400 invalid JSON body: EOF while parsing a value` instead of `413`,
        // and skipped the per-route cap entirely.
        let bytes = self.try_receive_bytes().await?;
        churust_core::check_body_limit(self, bytes.len())?;
        serde_json::from_slice::<T>(&bytes)
            .map_err(|e| churust_core::Error::bad_request(format!("invalid JSON body: {e}")))
    }

    fn respond_json<T: serde::Serialize + Sync>(&self, value: &T) -> churust_core::Response {
        match serde_json::to_vec(value) {
            Ok(body) => churust_core::Response::bytes("application/json", body),
            Err(_) => churust_core::Error::internal("failed to serialize response").into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};
    use http::header::CONTENT_LENGTH;
    use http::{Method, StatusCode};
    use serde::Deserialize;

    #[derive(Serialize, Deserialize)]
    struct Echo {
        msg: String,
    }

    fn app() -> App {
        Churust::server()
            .install(ContentNegotiation::new())
            .routing(|r| {
                r.post("/echo", |Json(body): Json<Echo>| async move { Json(body) });
                r.get("/boom", |_c: Call| async {
                    Err::<&str, _>(Error::bad_request("nope"))
                });
            })
            .build()
    }

    #[tokio::test]
    async fn json_round_trips_through_handler() {
        let client = TestClient::new(app());
        let res = client
            .post("/echo")
            .header("content-type", "application/json")
            .body(r#"{"msg":"hi"}"#)
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.header("content-type"), Some("application/json"));
        let parsed: Echo = serde_json::from_slice(res.body_bytes()).unwrap();
        assert_eq!(parsed.msg, "hi");
    }

    #[tokio::test]
    async fn invalid_json_is_400() {
        // Declared as JSON and malformed: the type was right, the content was
        // not. A body with no declared type is a different failure — see
        // `tests/content_type.rs` — and answers 415.
        let client = TestClient::new(app());
        let res = client
            .post("/echo")
            .header("content-type", "application/json")
            .body("not json")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn errors_render_as_json() {
        let client = TestClient::new(app());
        let res = client.get("/boom").send().await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(res.header("content-type"), Some("application/json"));
        let v: serde_json::Value = serde_json::from_slice(res.body_bytes()).unwrap();
        assert_eq!(v["error"], "nope");
        assert_eq!(v["status"], 400);
    }

    /// A `HEAD` reply is synthesized from the `GET` route, so whatever this
    /// middleware does to the `GET` representation has to be reflected in the
    /// headers the `HEAD` carries. RFC 9110 §9.3.2 lets a `HEAD` omit fields
    /// that are only determined while generating the content, but it never
    /// lets it describe a representation the matching `GET` would not send.
    #[tokio::test]
    async fn a_head_error_reply_agrees_with_the_get_it_stands_in_for() {
        let client = TestClient::new(app());
        let get = client.get("/boom").send().await;
        let head = client.request(Method::HEAD, "/boom").send().await;

        assert_eq!(head.status(), get.status());
        assert_eq!(
            head.text(),
            "",
            "a HEAD response must not carry a body at all"
        );
        assert_eq!(
            head.header("content-type"),
            get.header("content-type"),
            "HEAD advertised a different media type than the GET it stands in for"
        );
        if let Some(len) = head.header(CONTENT_LENGTH.as_str()) {
            assert_eq!(
                len.parse::<usize>().unwrap(),
                get.body_bytes().len(),
                "HEAD promised a size the GET does not deliver"
            );
        }
    }
}
