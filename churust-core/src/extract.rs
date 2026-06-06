//! Extractors: typed handler arguments derived from a `Call`.
//!
//! Two traits, mirroring the proven axum split:
//! - `FromCallParts`: borrows `&mut Call`, usable in ANY argument position.
//! - `FromCall`: consumes the `Call`, usable ONLY as the LAST argument.
//!
//! Every `FromCallParts` is also a `FromCall` (blanket impl), so a parts-style
//! extractor may also appear last. `Call` itself is `FromCall` (consuming), so
//! a `|call: Call|` handler is just the arity-1, last-arg case.

use crate::call::Call;
use crate::error::{Error, Result};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// Extract a value from a borrowed `&mut Call`.
///
/// Implementors borrow the call rather than consuming it, so a `FromCallParts`
/// extractor may appear in **any** handler argument position and several may be
/// used together. Returning an `Err` short-circuits the handler and renders the
/// [`Error`] as the response. The built-in [`Path`], [`Query`], [`State`], and
/// [`BearerToken`] extractors implement this trait.
///
/// Implement it to define your own borrowing extractor:
///
/// ```
/// use churust_core::{Call, Result, FromCallParts};
/// use async_trait::async_trait;
///
/// struct MethodName(String);
///
/// #[async_trait]
/// impl FromCallParts for MethodName {
///     async fn from_call_parts(call: &mut Call) -> Result<Self> {
///         Ok(MethodName(call.method().as_str().to_string()))
///     }
/// }
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// # use http::{HeaderMap, Method};
/// # use bytes::Bytes;
/// # let mut c = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
/// # assert_eq!(MethodName::from_call_parts(&mut c).await.unwrap().0, "GET");
/// # });
/// ```
#[async_trait]
pub trait FromCallParts: Sized + Send {
    /// Build `Self` from a mutable borrow of the call, or return an [`Error`]
    /// (which the handler renders as the response).
    async fn from_call_parts(call: &mut Call) -> Result<Self>;
}

/// Extract a value by consuming the whole [`Call`].
///
/// Because it takes the `Call` by value, a `FromCall` extractor may appear only
/// as the **last** handler argument. Body-consuming extractors (such as a JSON
/// body) implement this directly. Every [`FromCallParts`] type is automatically
/// a `FromCall` via a blanket impl, and [`Call`] itself is `FromCall` (so a
/// `|c: Call|` handler is just the last-argument case).
///
/// ```
/// use churust_core::{Call, FromCall};
/// use http::{HeaderMap, Method};
/// use bytes::Bytes;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let c = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
/// // `Call` is itself a `FromCall`:
/// let back = Call::from_call(c).await.unwrap();
/// assert_eq!(back.method(), &Method::GET);
/// # });
/// ```
#[async_trait]
pub trait FromCall: Sized + Send {
    /// Build `Self` by consuming the call, or return an [`Error`] (which the
    /// handler renders as the response).
    async fn from_call(call: Call) -> Result<Self>;
}

/// Any parts extractor can also be the final argument.
#[async_trait]
impl<T: FromCallParts> FromCall for T {
    async fn from_call(mut call: Call) -> Result<Self> {
        T::from_call_parts(&mut call).await
    }
}

/// The whole `Call` as the final argument (Ktor call-style base case).
/// NOTE: deliberately NOT `FromCallParts` — that would conflict with the
/// blanket impl above.
#[async_trait]
impl FromCall for Call {
    async fn from_call(call: Call) -> Result<Self> {
        Ok(call)
    }
}

/// Extracts a single path parameter, parsed into `T`.
///
/// For a route such as `"/users/{id}"`, `Path::<u64>` reads the first captured
/// parameter (`{id}`). It reads positionally, so it is intended for routes with
/// exactly one path parameter; for routes with several, read each by name with
/// [`Call::param`](crate::Call::param). Extraction fails with `400 Bad Request`
/// if there is no parameter or the value does not parse into `T`.
///
/// ```
/// use churust_core::{Churust, Path, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.get("/double/{n}", |Path(n): Path<i64>| async move {
///             format!("{}", n * 2)
///         });
///     })
///     .build();
/// let res = TestClient::new(app).get("/double/21").send().await;
/// assert_eq!(res.text(), "42");
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct Path<T>(
    /// The parsed path parameter value.
    pub T,
);

#[async_trait]
impl<T> FromCallParts for Path<T>
where
    T: std::str::FromStr + Send,
    T::Err: std::fmt::Display,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let mut params = call.params_iter();
        let (_name, raw) = params
            .next()
            .ok_or_else(|| Error::bad_request("no path parameter to extract"))?;
        let value = raw
            .parse::<T>()
            .map_err(|e| Error::bad_request(format!("bad path param: {e}")))?;
        Ok(Path(value))
    }
}

/// Deserializes the URL query string into `T` via `serde_urlencoded`.
///
/// `T` must implement [`serde::Deserialize`]. Missing required fields or
/// otherwise malformed query strings fail with `400 Bad Request`.
///
/// ```
/// use churust_core::{Churust, Query, TestClient};
/// use serde::Deserialize;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// #[derive(Deserialize)]
/// struct Pager { page: u32 }
///
/// let app = Churust::server()
///     .routing(|r| {
///         r.get("/list", |Query(p): Query<Pager>| async move {
///             format!("page {}", p.page)
///         });
///     })
///     .build();
/// let res = TestClient::new(app).get("/list?page=3").send().await;
/// assert_eq!(res.text(), "page 3");
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct Query<T>(
    /// The deserialized query value.
    pub T,
);

#[async_trait]
impl<T> FromCallParts for Query<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let q = call.query_string();
        let value = serde_urlencoded::from_str::<T>(q)
            .map_err(|e| Error::bad_request(format!("invalid query string: {e}")))?;
        Ok(Query(value))
    }
}

/// Extracts a shared handle to application state of type `T`.
///
/// The state must have been registered with
/// [`AppBuilder::state`](crate::AppBuilder::state); if no value of type `T` was
/// registered, extraction fails with `500 Internal Server Error`. `State<T>`
/// derefs to `T`, so the inner value can be used directly.
///
/// ```
/// use churust_core::{Churust, State, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// #[derive(Clone)]
/// struct Config { greeting: &'static str }
///
/// let app = Churust::server()
///     .state(Config { greeting: "hi" })
///     .routing(|r| {
///         r.get("/", |cfg: State<Config>| async move { cfg.greeting });
///     })
///     .build();
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.text(), "hi");
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct State<T>(
    /// The shared handle to the registered state value.
    pub Arc<T>,
);

#[async_trait]
impl<T> FromCallParts for State<T>
where
    T: Send + Sync + 'static,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        match call.state::<T>() {
            Some(v) => Ok(State(v)),
            None => Err(Error::internal(format!(
                "missing application state: {}",
                std::any::type_name::<T>()
            ))),
        }
    }
}

impl<T> std::ops::Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Extracts the token from an `Authorization: Bearer <token>` header.
///
/// The `Bearer` scheme prefix is matched case-insensitively and stripped; the
/// remaining token is trimmed. Extraction fails with `401 Unauthorized` if the
/// `Authorization` header is missing or does not use the `Bearer` scheme.
///
/// ```
/// use churust_core::{Churust, BearerToken, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.get("/me", |BearerToken(t): BearerToken| async move {
///             format!("token={t}")
///         });
///     })
///     .build();
/// let res = TestClient::new(app)
///     .get("/me")
///     .header("authorization", "Bearer abc123")
///     .send()
///     .await;
/// assert_eq!(res.text(), "token=abc123");
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct BearerToken(
    /// The extracted bearer token (without the `Bearer ` prefix).
    pub String,
);

#[async_trait]
impl FromCallParts for BearerToken {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let raw = call.header("authorization").ok_or_else(|| {
            Error::new(
                http::StatusCode::UNAUTHORIZED,
                "missing Authorization header",
            )
        })?;
        let token = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))
            .ok_or_else(|| Error::new(http::StatusCode::UNAUTHORIZED, "expected Bearer scheme"))?;
        Ok(BearerToken(token.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, Method, Uri};

    // A trivial parts extractor used to prove the machinery.
    struct MethodName(String);

    #[async_trait]
    impl FromCallParts for MethodName {
        async fn from_call_parts(call: &mut Call) -> Result<Self> {
            Ok(MethodName(call.method().as_str().to_string()))
        }
    }

    fn call() -> Call {
        Call::new(
            Method::GET,
            "/".parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
    }

    #[tokio::test]
    async fn parts_extractor_runs() {
        let mut c = call();
        let m = MethodName::from_call_parts(&mut c).await.unwrap();
        assert_eq!(m.0, "GET");
    }

    #[tokio::test]
    async fn call_is_from_call() {
        let c = call();
        let back = Call::from_call(c).await.unwrap();
        assert_eq!(back.method(), &Method::GET);
    }

    use std::collections::HashMap;

    #[tokio::test]
    async fn path_extracts_single_param() {
        let mut c = call();
        let mut p = HashMap::new();
        p.insert("id".to_string(), "42".to_string());
        c.set_params(p);
        let Path(id) = Path::<u64>::from_call_parts(&mut c).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn path_bad_value_is_400() {
        let mut c = call();
        let mut p = HashMap::new();
        p.insert("id".to_string(), "notnum".to_string());
        c.set_params(p);
        let err = Path::<u64>::from_call_parts(&mut c).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::BAD_REQUEST);
    }

    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Pager {
        page: u32,
        q: String,
    }

    fn call_with_query(qs: &str) -> Call {
        Call::new(
            Method::GET,
            format!("/s?{qs}").parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
    }

    #[tokio::test]
    async fn query_deserializes() {
        let mut c = call_with_query("page=2&q=rust");
        let Query(p) = Query::<Pager>::from_call_parts(&mut c).await.unwrap();
        assert_eq!(
            p,
            Pager {
                page: 2,
                q: "rust".into()
            }
        );
    }

    #[tokio::test]
    async fn query_missing_field_is_400() {
        let mut c = call_with_query("q=rust");
        let err = Query::<Pager>::from_call_parts(&mut c).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::BAD_REQUEST);
    }

    fn call_with_auth(value: &str) -> Call {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(value).unwrap(),
        );
        Call::new(
            Method::GET,
            "/".parse::<Uri>().unwrap(),
            headers,
            Bytes::new(),
        )
    }

    #[tokio::test]
    async fn bearer_token_extracted() {
        let mut c = call_with_auth("Bearer abc123");
        let BearerToken(t) = BearerToken::from_call_parts(&mut c).await.unwrap();
        assert_eq!(t, "abc123");
    }

    #[tokio::test]
    async fn missing_bearer_is_401() {
        let mut c = call();
        let err = BearerToken::from_call_parts(&mut c).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::UNAUTHORIZED);
    }
}
