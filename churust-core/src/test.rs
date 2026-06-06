//! In-process test harness. Drives `App::process` directly — no socket bind.

use crate::app::App;
use crate::response::Response;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};

/// An in-process test client bound to an assembled [`App`].
///
/// `TestClient` drives [`App::process`](crate::App) directly — no socket is
/// bound and no runtime port is used — so tests are fast and hermetic. Build a
/// request with [`get`](TestClient::get) / [`post`](TestClient::post) /
/// [`put`](TestClient::put) / [`delete`](TestClient::delete) (or the generic
/// [`request`](TestClient::request)), then call
/// [`send`](TestRequest::send) to get a [`TestResponse`].
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "home" }); })
///     .build();
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.status().as_u16(), 200);
/// assert_eq!(res.text(), "home");
/// # });
/// ```
pub struct TestClient {
    app: App,
}

/// A builder for a single in-process test request.
///
/// Created by the [`TestClient`] verb methods. Chain [`header`](TestRequest::header)
/// and [`body`](TestRequest::body) to refine the request, then await
/// [`send`](TestRequest::send) to run it through the pipeline.
pub struct TestRequest<'c> {
    client: &'c TestClient,
    method: Method,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

/// The response returned by the in-process pipeline, with inspection helpers.
///
/// Returned by [`TestRequest::send`]. Inspect it with [`status`](TestResponse::status),
/// [`header`](TestResponse::header), [`text`](TestResponse::text), and
/// [`body_bytes`](TestResponse::body_bytes).
pub struct TestResponse {
    inner: Response,
}

impl TestClient {
    /// Create a client that drives the given assembled [`App`].
    pub fn new(app: App) -> Self {
        Self { app }
    }

    /// Begin building a request with an arbitrary `method` and `uri`. The verb
    /// helpers ([`get`](TestClient::get), [`post`](TestClient::post), etc.) call
    /// this for the common methods.
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// use http::Method;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
    ///     .build();
    /// let res = TestClient::new(app).request(Method::GET, "/").send().await;
    /// assert_eq!(res.text(), "ok");
    /// # });
    /// ```
    pub fn request(&self, method: Method, uri: impl Into<String>) -> TestRequest<'_> {
        TestRequest {
            client: self,
            method,
            uri: uri.into(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    /// Begin building a `GET` request to `uri`.
    pub fn get(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::GET, uri)
    }
    /// Begin building a `POST` request to `uri`.
    pub fn post(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::POST, uri)
    }
    /// Begin building a `PUT` request to `uri`.
    pub fn put(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::PUT, uri)
    }
    /// Begin building a `DELETE` request to `uri`.
    pub fn delete(&self, uri: impl Into<String>) -> TestRequest<'_> {
        self.request(Method::DELETE, uri)
    }
}

impl<'c> TestRequest<'c> {
    /// Set a request header, returning `self` for chaining.
    ///
    /// # Panics
    ///
    /// Panics if `value` is not a valid header value (this is a test helper, so
    /// it favors a clear failure over a `Result`).
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.get("/", |c: Call| async move {
    ///             c.header("x-test").unwrap_or("none").to_string()
    ///         });
    ///     })
    ///     .build();
    /// let res = TestClient::new(app).get("/").header("x-test", "yes").send().await;
    /// assert_eq!(res.text(), "yes");
    /// # });
    /// ```
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("valid header value"),
        );
        self
    }

    /// Set the request body, returning `self` for chaining.
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.post("/echo", |mut c: Call| async move {
    ///             c.receive_text().await.unwrap_or_default()
    ///         });
    ///     })
    ///     .build();
    /// let res = TestClient::new(app).post("/echo").body("ping").send().await;
    /// assert_eq!(res.text(), "ping");
    /// # });
    /// ```
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Run the request through the application pipeline and return the
    /// [`TestResponse`], consuming the builder.
    ///
    /// # Panics
    ///
    /// Panics if the configured URI fails to parse.
    pub async fn send(self) -> TestResponse {
        let uri = self.uri.parse::<Uri>().expect("valid URI");
        let res = self
            .client
            .app
            .process(self.method, uri, self.headers, self.body)
            .await;
        TestResponse { inner: res }
    }
}

impl TestResponse {
    /// The response status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status
    }
    /// The value of response header `name` as a string, or `None` if absent or
    /// not valid UTF-8. Matching is case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.headers.get(name).and_then(|v| v.to_str().ok())
    }
    /// The raw response body bytes.
    pub fn body_bytes(&self) -> &Bytes {
        &self.inner.body
    }
    /// The response body decoded as UTF-8 (lossily, so this never fails).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.inner.body).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::Call;
    use crate::Churust;

    fn app() -> App {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "home" });
                r.post("/echo", |mut c: Call| async move {
                    c.receive_text().await.unwrap_or_default()
                });
            })
            .build()
    }

    #[tokio::test]
    async fn get_returns_body_and_status() {
        let client = TestClient::new(app());
        let res = client.get("/").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "home");
    }

    #[tokio::test]
    async fn post_echoes_body() {
        let client = TestClient::new(app());
        let res = client.post("/echo").body("ping").send().await;
        assert_eq!(res.text(), "ping");
    }

    #[tokio::test]
    async fn missing_route_is_404() {
        let client = TestClient::new(app());
        let res = client.get("/nope").send().await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
