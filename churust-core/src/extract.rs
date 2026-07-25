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
///
/// `do_not_recommend`: when a *body* extractor is wrongly placed in a leading
/// position, rustc would otherwise suggest implementing `FromCallParts` for it
/// — which is the one thing that must not happen, since it would let the body
/// be consumed twice.
#[async_trait]
#[diagnostic::do_not_recommend]
impl<T: FromCallParts> FromCall for T {
    async fn from_call(mut call: Call) -> Result<Self> {
        T::from_call_parts(&mut call).await
    }
}

/// An extractor that can distinguish *absent* from *malformed*.
///
/// Implement this and `Option<Self>` becomes usable as a handler argument. That
/// is the whole design: one trait rather than a parallel `OptionalQuery`,
/// `OptionalPath`, `OptionalHeader` type per extractor. The parallel-type
/// approach doubles the API surface and its documentation, and it is the design
/// axum-extra shipped, deprecated, and replaced with exactly this.
///
/// **`Ok(None)` means the input was not supplied. It does not mean extraction
/// failed.** A malformed value must still be an `Err`, or a typo'd query string
/// becomes a silent `None` and the handler quietly does the wrong thing with a
/// default. The distinction is the reason the trait exists — without it,
/// `Option<T>` could only ever mean "swallow every error".
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
///         r.get("/list", |p: Option<Query<Pager>>| async move {
///             match p {
///                 Some(Query(p)) => format!("page {}", p.page),
///                 None => "unpaged".to_string(),
///             }
///         });
///     })
///     .build();
/// let client = TestClient::new(app);
/// assert_eq!(client.get("/list?page=2").send().await.text(), "page 2");
/// assert_eq!(client.get("/list").send().await.text(), "unpaged");
/// # });
/// ```
#[async_trait]
pub trait OptionalFromCallParts: Sized + Send {
    /// Build `Self`, or `Ok(None)` when the input is absent. Reserve `Err` for
    /// input that was supplied and is wrong.
    async fn from_call_parts_opt(call: &mut Call) -> Result<Option<Self>>;
}

/// `Option<T>` extracts wherever `T` opts into [`OptionalFromCallParts`].
#[async_trait]
impl<T: OptionalFromCallParts> FromCallParts for Option<T> {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        T::from_call_parts_opt(call).await
    }
}

/// Absent means no query string at all. A query string that is present but does
/// not fit `T` is an error: the caller tried and got it wrong, which is not the
/// same as not trying.
#[async_trait]
impl<T> OptionalFromCallParts for Query<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call_parts_opt(call: &mut Call) -> Result<Option<Self>> {
        if call.query_string().is_empty() {
            return Ok(None);
        }
        Query::<T>::from_call_parts(call).await.map(Some)
    }
}

/// Absent means the route captured no parameters. A parameter that is present
/// but does not parse into `T` is an error — reporting `None` there would claim
/// the URL had no such parameter at all.
#[async_trait]
impl<T> OptionalFromCallParts for Path<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call_parts_opt(call: &mut Call) -> Result<Option<Self>> {
        if call.params().is_empty() {
            return Ok(None);
        }
        Path::<T>::from_call_parts(call).await.map(Some)
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
    T: serde::de::DeserializeOwned + Send,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        crate::path_de::from_params::<T>(call.params())
            .map(Path)
            .map_err(|e| Error::bad_request(format!("bad path parameters: {e}")))
    }
}

/// Deserializes the URL query string into `T`.
///
/// `T` must implement [`serde::Deserialize`]. Missing required fields or
/// otherwise malformed query strings fail with `400 Bad Request`.
///
/// A repeated key fills a `Vec<T>` field: `?tag=a&tag=b` deserializes into
/// `Vec<String>`, which is what `<select multiple>` and a repeated checkbox
/// send.
///
/// A key repeated against a *scalar* field is **rejected**, not resolved.
/// Browsers take the last occurrence and some servers take the first, so
/// picking a winner silently means a proxy and an origin can disagree about
/// what the request said. Declare the field as `Vec<T>` to accept repetition
/// deliberately.
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
        let value = serde_html_form::from_str::<T>(q)
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
        // RFC 7235 §2.1 makes the auth scheme case-insensitive. Matching the
        // two literals `Bearer ` and `bearer ` rejected `BEARER `, which real
        // clients do send, with a 401 for a perfectly valid credential.
        let token = raw
            .split_once(' ')
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token)
            .ok_or_else(|| Error::new(http::StatusCode::UNAUTHORIZED, "expected Bearer scheme"))?;
        Ok(BearerToken(token.trim().to_string()))
    }
}

/// Absent means no `Authorization` header. A header that is present but is not
/// a Bearer credential stays a `401`: the client did attempt to authenticate,
/// and treating a malformed scheme as "anonymous" is how an auth check gets
/// skipped by accident.
#[async_trait]
impl OptionalFromCallParts for BearerToken {
    async fn from_call_parts_opt(call: &mut Call) -> Result<Option<Self>> {
        if call.header("authorization").is_none() {
            return Ok(None);
        }
        BearerToken::from_call_parts(call).await.map(Some)
    }
}

/// Extracts a single named header, parsed into `T`.
///
/// The name is supplied by a type implementing [`HeaderName`], so the header a
/// handler reads is part of its signature rather than a string repeated at the
/// call site.
///
/// ```
/// use churust_core::{Churust, Header, HeaderName, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// struct ApiVersion;
/// impl HeaderName for ApiVersion {
///     const NAME: &'static str = "x-api-version";
/// }
///
/// let app = Churust::server()
///     .routing(|r| {
///         r.get("/", |Header(v, _): Header<u32, ApiVersion>| async move {
///             format!("v{v}")
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app).get("/").header("x-api-version", "2").send().await;
/// assert_eq!(res.text(), "v2");
/// # });
/// ```
///
/// Fails with `400 Bad Request` when the header is absent or does not parse
/// into `T`. For an optional header, use `Header<Option<T>, N>`, or read it
/// directly with [`Call::header`](crate::Call::header).
///
/// # Why a marker type
///
/// The v1 design named `Header<T>` without saying which header it reads. A
/// const string parameter is not expressible on stable Rust, deserializing the
/// whole header map would silently accept junk, and a `headers`-crate typed
/// trait would pull in a dependency for a small win. A marker type keeps the
/// name in the type system, costs nothing at runtime, and needs no dependency.
pub struct Header<T, N: HeaderName>(
    /// The parsed header value.
    pub T,
    /// Zero-sized marker naming the header. Ignore it.
    pub std::marker::PhantomData<N>,
);

/// Names the header that a [`Header`] extractor reads.
pub trait HeaderName: Send + Sync + 'static {
    /// The header name, lower-case.
    const NAME: &'static str;
}

#[async_trait]
impl<T, N> FromCallParts for Header<T, N>
where
    T: std::str::FromStr + Send,
    T::Err: std::fmt::Display,
    N: HeaderName,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        let raw = call
            .header(N::NAME)
            .ok_or_else(|| Error::bad_request(format!("missing header `{}`", N::NAME)))?;
        raw.parse::<T>()
            .map(|v| Header(v, std::marker::PhantomData))
            .map_err(|e| Error::bad_request(format!("bad header `{}`: {e}", N::NAME)))
    }
}

/// Absent means the header was not sent. A header that was sent but does not
/// parse into `T` is an error, since the client did state a value.
#[async_trait]
impl<T, N> OptionalFromCallParts for Header<T, N>
where
    T: std::str::FromStr + Send,
    T::Err: std::fmt::Display,
    N: HeaderName,
{
    async fn from_call_parts_opt(call: &mut Call) -> Result<Option<Self>> {
        if call.header(N::NAME).is_none() {
            return Ok(None);
        }
        Header::<T, N>::from_call_parts(call).await.map(Some)
    }
}

/// The raw request body as text.
///
/// Fails with `400 Bad Request` if the body is not valid UTF-8. For bytes that
/// may not be text, extract [`Bytes`](bytes::Bytes) instead.
#[async_trait]
impl FromCall for String {
    async fn from_call(mut call: Call) -> Result<Self> {
        let b = call.try_receive_bytes().await?;
        check_body_limit(&call, b.len())?;
        String::from_utf8(b.to_vec())
            .map_err(|_| Error::bad_request("request body is not valid UTF-8"))
    }
}

/// The raw request body as bytes.
#[async_trait]
impl FromCall for bytes::Bytes {
    async fn from_call(mut call: Call) -> Result<Self> {
        let b = call.try_receive_bytes().await?;
        check_body_limit(&call, b.len())?;
        Ok(b)
    }
}

/// Accept one of two extractors, whichever succeeds.
///
/// The usual case is an endpoint that takes either JSON or a form:
///
/// ```
/// use churust_core::{Churust, Either, Form, TestClient};
/// use serde::Deserialize;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// #[derive(Deserialize)]
/// struct Note { text: String }
///
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/notes", |body: Either<Form<Note>, String>| async move {
///             match body {
///                 Either::Left(Form(n)) => format!("form: {}", n.text),
///                 Either::Right(raw) => format!("raw: {raw}"),
///             }
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .post("/notes")
///     .header("content-type", "application/x-www-form-urlencoded")
///     .body("text=hi")
///     .send()
///     .await;
/// assert_eq!(res.text(), "form: hi");
/// # });
/// ```
///
/// `Left` is tried first. If it fails, the call is handed to `Right` — which is
/// why the call is cloned rather than moved: a failed first attempt must not
/// have consumed the body. When both fail, the **second** error is reported,
/// since the right-hand side is the fallback and its complaint is usually the
/// more informative one.
pub enum Either<L, R> {
    /// The first extractor succeeded.
    Left(L),
    /// The first failed and the second succeeded.
    Right(R),
}

#[async_trait]
impl<L, R> FromCall for Either<L, R>
where
    L: FromCall,
    R: FromCall,
{
    async fn from_call(call: Call) -> Result<Self> {
        // The body can only be read once, so buffer it up front and give each
        // attempt its own copy.
        let mut call = call;
        let body = call.try_receive_bytes().await?;

        let left_call = call.clone_with_body(body.clone());
        if let Ok(l) = L::from_call(left_call).await {
            return Ok(Either::Left(l));
        }
        R::from_call(call.clone_with_body(body))
            .await
            .map(Either::Right)
    }
}

/// The request body as a stream, without buffering it.
///
/// Consumes the body, so it must be the last handler argument. Use this for a
/// large upload that should not be held in memory — the counterpart to
/// [`Json<T>`](../churust_json/index.html) and [`Form<T>`], which buffer.
///
/// ```
/// use churust_core::{Churust, Payload, TestClient};
/// use futures_util::StreamExt;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/count", |Payload(mut body): Payload| async move {
///             let mut n = 0usize;
///             while let Some(Ok(chunk)) = body.next().await {
///                 n += chunk.len();
///             }
///             format!("{n} bytes")
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app).post("/count").body("hello").send().await;
/// assert_eq!(res.text(), "5 bytes");
/// # });
/// ```
///
/// Both the server-wide `max_body_bytes` and any per-route
/// [`max_body_bytes`](crate::RouteBuilder::max_body_bytes) apply. Because the
/// response may already have begun, exceeding either arrives as an error item
/// in the stream rather than as a `413` — propagate it with `?` to turn it back
/// into the right status.
///
/// The stream is bounded; **collecting it into memory is your allocation, not
/// the framework's**. A handler that gathers the whole body allocates up to
/// whichever limit is tighter, so tighten the route where a large ceiling was
/// raised for some other route's benefit.
pub struct Payload(
    /// The body's chunks.
    pub crate::call::BodyStream,
);

#[async_trait]
impl FromCall for Payload {
    async fn from_call(mut call: Call) -> Result<Self> {
        let route_limit = call.get::<RouteBodyLimit>().map(|RouteBodyLimit(n)| n);
        // An absent body is an empty stream rather than an error: a POST with
        // no body is a legitimate request, not a malformed one.
        let stream = call
            .body_stream()
            .unwrap_or_else(|| Box::pin(futures_util::stream::empty()));

        let Some(max) = route_limit else {
            // No route cap: the engine's server-wide `Limited` is the only
            // bound, and it is already wrapped around this stream.
            return Ok(Payload(stream));
        };

        // The engine enforces the server-wide cap before the stream is built,
        // so this only ever *tightens*. Counting here rather than collecting
        // keeps the streaming property: a body is refused at the byte that
        // crosses the line, not after it has all been read.
        use futures_util::StreamExt;
        let mut seen = 0usize;
        let counted = stream.map(move |chunk| match chunk {
            Ok(bytes) => {
                seen += bytes.len();
                if seen > max {
                    Err(Error::new(
                        http::StatusCode::PAYLOAD_TOO_LARGE,
                        "request body too large",
                    ))
                } else {
                    Ok(bytes)
                }
            }
            Err(e) => Err(e),
        });
        Ok(Payload(Box::pin(counted)))
    }
}

/// A per-route body cap, seeded into the call by
/// [`RouteBuilder::max_body_bytes`](crate::RouteBuilder::max_body_bytes).
#[derive(Debug, Clone, Copy)]
pub struct RouteBodyLimit(pub usize);

/// Reject a body larger than the route's cap, if it set one.
///
/// Body extractors call this after reading. Public so plugin crates such as
/// `churust-json` can honour the same per-route configuration.
pub fn check_body_limit(call: &Call, len: usize) -> Result<()> {
    match call.get::<RouteBodyLimit>() {
        Some(RouteBodyLimit(max)) if len > max => Err(Error::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        )),
        _ => Ok(()),
    }
}

/// Deserializes an `application/x-www-form-urlencoded` request body into `T`.
///
/// The body counterpart to [`Query<T>`], and the classic HTML form POST. Like
/// [`Json<T>`](../churust_json/index.html) it consumes the body, so it must be
/// the last handler argument.
///
/// Fails with `415 Unsupported Media Type` when the content type is not
/// `application/x-www-form-urlencoded`, and `400 Bad Request` when the body
/// does not deserialize into `T`.
///
/// Note `+` means a space here — this is form encoding. In a *path* segment `+`
/// is a literal plus, which is why path decoding uses its own decoder.
///
/// ```
/// use churust_core::{Churust, Form, TestClient};
/// use serde::Deserialize;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// #[derive(Deserialize)]
/// struct Login { user: String }
///
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/login", |Form(l): Form<Login>| async move { l.user });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .post("/login")
///     .header("content-type", "application/x-www-form-urlencoded")
///     .body("user=ana")
///     .send()
///     .await;
/// assert_eq!(res.text(), "ana");
/// # });
/// ```
pub struct Form<T>(
    /// The deserialized form body.
    pub T,
);

#[async_trait]
impl<T> FromCall for Form<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_call(mut call: Call) -> Result<Self> {
        let ct = call
            .header(http::header::CONTENT_TYPE.as_str())
            .unwrap_or("")
            .to_string();
        // Compare only the media type: a charset parameter is legitimate.
        let media = ct.split(';').next().unwrap_or("").trim();
        if !media.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
            return Err(Error::new(
                http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected application/x-www-form-urlencoded",
            ));
        }

        let body = call.try_receive_bytes().await?;
        check_body_limit(&call, body.len())?;
        serde_html_form::from_bytes::<T>(&body)
            .map(Form)
            .map_err(|e| Error::bad_request(format!("invalid form body: {e}")))
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

    #[tokio::test]
    async fn path_extracts_single_param() {
        let mut c = call();
        let mut p = crate::call::Params::new();
        p.insert("id".to_string(), "42".to_string());
        c.set_params(p);
        let Path(id) = Path::<u64>::from_call_parts(&mut c).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn path_bad_value_is_400() {
        let mut c = call();
        let mut p = crate::call::Params::new();
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
