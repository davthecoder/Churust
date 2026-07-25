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
use http::header::CONTENT_TYPE;
use http::HeaderValue;
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
        let bytes = call.try_receive_bytes().await?;
        churust_core::check_body_limit(&call, bytes.len())?;
        let value = serde_json::from_slice::<T>(&bytes)
            .map_err(|e| Error::bad_request(format!("invalid JSON body: {e}")))?;
        Ok(Json(value))
    }
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
        let mut res = next.run(call).await;
        let is_error = res.status.is_client_error() || res.status.is_server_error();
        let is_text = res
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.starts_with("text/plain"))
            .unwrap_or(false);
        if is_error && is_text {
            if let Some(buffered) = res.body.as_bytes() {
                let msg = String::from_utf8_lossy(buffered).into_owned();
                let body = serde_json::json!({ "error": msg, "status": res.status.as_u16() });
                let bytes = if self.pretty {
                    serde_json::to_vec_pretty(&body)
                } else {
                    serde_json::to_vec(&body)
                }
                .unwrap_or_default();
                res.body = churust_core::Body::from(bytes);
                res.headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
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
    use http::StatusCode;
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
        let client = TestClient::new(app());
        let res = client.post("/echo").body("not json").send().await;
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
}
