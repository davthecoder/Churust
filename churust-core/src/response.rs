//! The buffered [`Response`] type and the [`IntoResponse`] conversion trait.
//!
//! A [`Response`] is the in-memory result of handling a request: a status, a
//! header map, and a body buffered as [`Bytes`]. Handlers rarely build one by
//! hand — instead they return any value implementing [`IntoResponse`] (a
//! `&str`, `String`, [`StatusCode`], a `(StatusCode, T)` tuple, an
//! [`Error`], or a [`Result`](crate::error::Result)) and the framework
//! converts it for you.

use crate::body::Body;
use crate::error::Error;
use bytes::Bytes;
use http::header::{HeaderName, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, StatusCode};

/// A fully-buffered HTTP response: status line, headers, and an in-memory body.
///
/// The three fields are public so middleware can post-process a response (for
/// example inserting a header). Construct one with [`Response::new`] (status
/// only), [`Response::text`] (a `text/plain` body), or [`Response::bytes`] (an
/// arbitrary content type), then refine it with the chainable
/// [`with_status`](Response::with_status) and
/// [`with_header`](Response::with_header) builders. Most handlers return an
/// [`IntoResponse`] value instead of building this directly.
///
/// ```
/// use churust_core::Response;
/// use http::StatusCode;
///
/// let res = Response::text("created").with_status(StatusCode::CREATED);
/// assert_eq!(res.status, StatusCode::CREATED);
/// assert_eq!(res.body.as_slice(), Some(&b"created"[..]));
/// ```
#[derive(Debug)]
pub struct Response {
    /// The HTTP status code of the response.
    pub status: StatusCode,
    /// The response headers.
    pub headers: HeaderMap,
    /// The response body — buffered bytes or a lazy stream.
    pub body: Body,
}

impl Response {
    /// Create an empty-bodied response with the given `status` and no headers.
    ///
    /// ```
    /// use churust_core::Response;
    /// use http::StatusCode;
    ///
    /// let res = Response::new(StatusCode::NO_CONTENT);
    /// assert_eq!(res.status, StatusCode::NO_CONTENT);
    /// assert!(res.body.is_empty());
    /// ```
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: Body::empty(),
        }
    }

    /// Create a `200 OK` response with a `text/plain; charset=utf-8` body.
    ///
    /// ```
    /// use churust_core::Response;
    /// use http::StatusCode;
    ///
    /// let res = Response::text("hello");
    /// assert_eq!(res.status, StatusCode::OK);
    /// assert_eq!(
    ///     res.headers.get(http::header::CONTENT_TYPE).unwrap(),
    ///     "text/plain; charset=utf-8"
    /// );
    /// ```
    pub fn text(body: impl Into<String>) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        r.body = Body::from(body.into());
        r
    }

    /// Create a `200 OK` response with a raw byte body and an explicit
    /// `Content-Type`. Use this for non-text payloads (JSON, images, etc.); the
    /// `content_type` must be a `'static` string.
    ///
    /// ```
    /// use churust_core::Response;
    ///
    /// let res = Response::bytes("application/octet-stream", vec![1u8, 2, 3]);
    /// assert_eq!(res.body.as_slice(), Some(&[1u8, 2, 3][..]));
    /// assert_eq!(
    ///     res.headers.get(http::header::CONTENT_TYPE).unwrap(),
    ///     "application/octet-stream"
    /// );
    /// ```
    pub fn bytes(content_type: &'static str, body: impl Into<Bytes>) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        r.body = Body::from(body.into());
        r
    }

    /// Create a `200 OK` response whose body is produced lazily from `stream`,
    /// with an explicit `Content-Type`. Use for large or dynamic payloads.
    ///
    /// ```
    /// use churust_core::{Body, Response};
    /// use bytes::Bytes;
    ///
    /// let chunks = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from("hi"))]);
    /// let res = Response::stream("text/plain", Body::from_stream(chunks));
    /// assert!(res.body.as_bytes().is_none());
    /// ```
    pub fn stream(content_type: &'static str, body: Body) -> Self {
        let mut r = Self::new(StatusCode::OK);
        r.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        r.body = body;
        r
    }

    /// Override the status code, returning `self` for chaining.
    ///
    /// ```
    /// use churust_core::Response;
    /// use http::StatusCode;
    ///
    /// let res = Response::text("teapot").with_status(StatusCode::IM_A_TEAPOT);
    /// assert_eq!(res.status, StatusCode::IM_A_TEAPOT);
    /// ```
    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Insert (or replace) a header, returning `self` for chaining.
    ///
    /// ```
    /// use churust_core::Response;
    /// use http::{header::LOCATION, HeaderValue};
    ///
    /// let res = Response::new(http::StatusCode::FOUND)
    ///     .with_header(LOCATION, HeaderValue::from_static("/login"));
    /// assert_eq!(res.headers.get(LOCATION).unwrap(), "/login");
    /// ```
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }
}

/// Convert a handler return value into a [`Response`].
///
/// This is what lets handlers return ergonomic values instead of building a
/// [`Response`] by hand. The crate implements it for the common cases:
///
/// - `()` — an empty `200 OK`.
/// - `&'static str` / [`String`] — a `text/plain` body.
/// - [`StatusCode`] — an empty body with that status.
/// - `(StatusCode, T)` where `T: IntoResponse` — `T`'s response with the status
///   overridden.
/// - [`Error`] — rendered using its own status, message, and
///   headers.
/// - [`Result<T>`](crate::Result) where `T: IntoResponse` — the `Ok` value's
///   response, or the `Err`'s.
/// - [`Response`] — returned unchanged (identity).
///
/// Implement it for your own types to make them returnable from handlers.
///
/// ```
/// use churust_core::{IntoResponse, Response};
/// use http::StatusCode;
///
/// // The `(StatusCode, &str)` tuple impl in action:
/// let res = (StatusCode::CREATED, "made").into_response();
/// assert_eq!(res.status, StatusCode::CREATED);
/// assert_eq!(res.body.as_slice(), Some(&b"made"[..]));
/// ```
pub trait IntoResponse {
    /// Consume `self` and produce the [`Response`] to send.
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for () {
    fn into_response(self) -> Response {
        Response::new(StatusCode::OK)
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        Response::text(self)
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        Response::text(self)
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        Response::new(self)
    }
}

impl<T: IntoResponse> IntoResponse for (StatusCode, T) {
    fn into_response(self) -> Response {
        let (status, inner) = self;
        inner.into_response().with_status(status)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut res = Response::text(self.message().to_string()).with_status(self.status());
        for (name, value) in self.response_headers() {
            res.headers.insert(name.clone(), value.clone());
        }
        res
    }
}

/// A `Result` whose `Ok`/`Err` both render to a response.
impl<T: IntoResponse> IntoResponse for crate::error::Result<T> {
    fn into_response(self) -> Response {
        match self {
            Ok(v) => v.into_response(),
            Err(e) => e.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sets_content_type_and_body() {
        let r = Response::text("hi");
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.body, Bytes::from("hi"));
        assert_eq!(
            r.headers.get(CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    fn status_tuple_overrides_status() {
        let r = (StatusCode::CREATED, "made").into_response();
        assert_eq!(r.status, StatusCode::CREATED);
        assert_eq!(r.body, Bytes::from("made"));
    }

    #[test]
    fn error_renders_with_its_status() {
        let r = Error::bad_request("x").into_response();
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
}
