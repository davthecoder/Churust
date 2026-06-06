//! The status-carrying [`Error`] type and the crate-wide [`Result`] alias.
//!
//! Churust errors render directly into HTTP responses: each [`Error`] carries
//! the [`StatusCode`] to send, a human-readable message used as the response
//! body, an optional source error for diagnostics, and any headers to attach
//! (for example `WWW-Authenticate` on a `401`). Handlers and extractors return
//! `Result<T>`; when an `Err` reaches the pipeline it is converted via
//! [`IntoResponse`](crate::IntoResponse).

use http::header::{HeaderName, HeaderValue};
use http::StatusCode;
use std::fmt;

/// Crate-wide result type, defaulting the error to [`Error`].
///
/// Handlers and extractors typically return `Result<T>` (i.e.
/// `Result<T, Error>`); the second type parameter exists only so the alias can
/// be reused with a different error type when desired.
///
/// ```
/// use churust_core::{Error, Result};
/// use http::StatusCode;
///
/// fn parse_age(raw: &str) -> Result<u8> {
///     raw.parse()
///         .map_err(|_| Error::bad_request("age must be a number"))
/// }
///
/// assert!(parse_age("30").is_ok());
/// assert_eq!(parse_age("nope").unwrap_err().status(), StatusCode::BAD_REQUEST);
/// ```
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A handler/framework error carrying the HTTP status to respond with.
///
/// An `Error` bundles everything needed to render a failed request: the
/// [`StatusCode`], a message (sent as the response body), an optional source
/// error, and headers to attach to the rendered response. Return one from any
/// handler or extractor and the pipeline turns it into a [`Response`](crate::Response) for you
/// via the [`IntoResponse`](crate::IntoResponse) impl.
///
/// Use the constructors ([`Error::bad_request`], [`Error::not_found`],
/// [`Error::internal`], or [`Error::new`] for an arbitrary status) and chain
/// [`with_source`](Error::with_source) /
/// [`with_response_header`](Error::with_response_header) as needed.
///
/// ```
/// use churust_core::Error;
/// use http::StatusCode;
///
/// let err = Error::not_found("user 42 does not exist");
/// assert_eq!(err.status(), StatusCode::NOT_FOUND);
/// assert_eq!(err.message(), "user 42 does not exist");
/// ```
#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    headers: Vec<(HeaderName, HeaderValue)>,
}

impl Error {
    /// Create an error with an explicit `status` and `message`.
    ///
    /// Prefer the named constructors ([`bad_request`](Error::bad_request),
    /// [`not_found`](Error::not_found), [`internal`](Error::internal)) for the
    /// common cases; use `new` when you need any other status (e.g. `401` or
    /// `409`).
    ///
    /// ```
    /// use churust_core::Error;
    /// use http::StatusCode;
    ///
    /// let err = Error::new(StatusCode::CONFLICT, "already exists");
    /// assert_eq!(err.status(), StatusCode::CONFLICT);
    /// ```
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            source: None,
            headers: Vec::new(),
        }
    }
    /// Create a `400 Bad Request` error — use for malformed input such as an
    /// unparseable path parameter or invalid query string.
    ///
    /// ```
    /// use churust_core::Error;
    /// use http::StatusCode;
    ///
    /// assert_eq!(Error::bad_request("nope").status(), StatusCode::BAD_REQUEST);
    /// ```
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    /// Create a `404 Not Found` error — use when a requested resource does not
    /// exist.
    ///
    /// ```
    /// use churust_core::Error;
    /// use http::StatusCode;
    ///
    /// assert_eq!(Error::not_found("gone").status(), StatusCode::NOT_FOUND);
    /// ```
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    /// Create a `500 Internal Server Error` — use for unexpected server-side
    /// failures (e.g. a missing application-state dependency).
    ///
    /// ```
    /// use churust_core::Error;
    /// use http::StatusCode;
    ///
    /// assert_eq!(Error::internal("boom").status(), StatusCode::INTERNAL_SERVER_ERROR);
    /// ```
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
    /// Attach an underlying source error for diagnostics (exposed via
    /// [`std::error::Error::source`]). The source does not change the rendered
    /// response; it is for logging and error chaining. Returns `self` so it can
    /// be chained.
    ///
    /// ```
    /// use churust_core::Error;
    /// use std::error::Error as _;
    ///
    /// let io = std::io::Error::new(std::io::ErrorKind::Other, "disk gone");
    /// let err = Error::internal("write failed").with_source(io);
    /// assert!(err.source().is_some());
    /// ```
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
    /// The HTTP status this error renders to.
    ///
    /// ```
    /// use churust_core::Error;
    /// use http::StatusCode;
    ///
    /// assert_eq!(Error::bad_request("x").status(), StatusCode::BAD_REQUEST);
    /// ```
    pub fn status(&self) -> StatusCode {
        self.status
    }
    /// The human-readable message, used as the response body when rendered.
    ///
    /// ```
    /// use churust_core::Error;
    ///
    /// assert_eq!(Error::not_found("missing").message(), "missing");
    /// ```
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Attach a header to the response this error renders into (e.g.
    /// `WWW-Authenticate` on a `401`). May be called repeatedly to add several
    /// headers; returns `self` for chaining.
    ///
    /// ```
    /// use churust_core::Error;
    /// use http::{StatusCode, header::WWW_AUTHENTICATE, HeaderValue};
    ///
    /// let err = Error::new(StatusCode::UNAUTHORIZED, "login required")
    ///     .with_response_header(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    /// assert_eq!(err.response_headers().len(), 1);
    /// ```
    pub fn with_response_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    /// The headers to apply when rendering this error to a
    /// [`Response`](crate::Response).
    /// Returns an empty slice unless
    /// [`with_response_header`](Error::with_response_header) was called.
    ///
    /// ```
    /// use churust_core::Error;
    ///
    /// assert!(Error::bad_request("x").response_headers().is_empty());
    /// ```
    pub fn response_headers(&self) -> &[(HeaderName, HeaderValue)] {
        &self.headers
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|s| s.as_ref() as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_status_and_message() {
        let e = Error::not_found("nope");
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        assert_eq!(e.message(), "nope");
    }

    #[test]
    fn display_includes_status() {
        let e = Error::bad_request("bad");
        assert!(format!("{e}").contains("400"));
    }

    #[test]
    fn carries_response_headers() {
        let e = Error::new(StatusCode::UNAUTHORIZED, "no").with_response_header(
            http::header::WWW_AUTHENTICATE,
            http::HeaderValue::from_static("Bearer"),
        );
        assert_eq!(e.response_headers().len(), 1);
    }
}
