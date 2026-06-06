//! Authentication plugins for the [Churust](churust_core) web framework.
//!
//! This crate provides ready-made authentication schemes — [`Bearer`] tokens,
//! HTTP [`Basic`] credentials, and [`Jwt`] bearer tokens — plus the
//! [`Principal<P>`] extractor used to require and read the authenticated user
//! inside a handler.
//!
//! # Authentication vs. authorization
//!
//! An auth plugin only *authenticates*: it inspects the incoming `Authorization`
//! header, verifies the credentials, and — on success — inserts a *principal*
//! value (your own user/claims type) into the call's extensions. It never
//! rejects a request on its own. If the credentials are missing or invalid, the
//! plugin simply leaves no principal behind.
//!
//! *Authorization* — actually requiring a logged-in user — is type-driven and
//! explicit: a handler asks for a [`Principal<P>`] argument. When a principal of
//! type `P` is present the handler runs; when it is absent the extractor returns
//! `401 Unauthorized` with a `WWW-Authenticate: Bearer` challenge before the
//! handler body is ever entered. Routes that do not ask for a `Principal` stay
//! public.
//!
//! All schemes are constructed through the [`Auth`] namespace and installed with
//! [`AppBuilder::install`](churust_core::AppBuilder::install). The principal type
//! `P` you choose is what later handlers extract; only one principal type can be
//! resolved per call (the most recently inserted value of a given type wins).
//!
//! # Example
//!
//! Protect a route with bearer-token auth. The `/me` handler only runs when a
//! valid token resolved to a `User` principal; otherwise the [`Principal`]
//! extractor short-circuits with `401`.
//!
//! ```
//! use churust_core::{Churust, TestClient};
//! use churust_auth::{Auth, Principal};
//!
//! #[derive(Clone)]
//! struct User {
//!     name: String,
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(Auth::bearer(|token: String| async move {
//!         (token == "s3cret").then(|| User { name: "ana".into() })
//!     }))
//!     .routing(|r| {
//!         r.get("/me", |Principal(u): Principal<User>| async move {
//!             format!("hello {}", u.name)
//!         });
//!     })
//!     .build();
//!
//! // A valid token reaches the handler.
//! let ok = TestClient::new(app.clone())
//!     .get("/me")
//!     .header("authorization", "Bearer s3cret")
//!     .send()
//!     .await;
//! assert_eq!(ok.status().as_u16(), 200);
//! assert_eq!(ok.text(), "hello ana");
//!
//! // No credentials => 401 with a challenge, handler never runs.
//! let denied = TestClient::new(app).get("/me").send().await;
//! assert_eq!(denied.status().as_u16(), 401);
//! assert_eq!(denied.header("www-authenticate"), Some("Bearer"));
//! # });
//! ```
#![deny(missing_docs)]

use async_trait::async_trait;
use churust_core::{
    AppBuilder, Call, Error, FromCallParts, Middleware, Next, Phase, Plugin, Response, Result,
};
use http::header::WWW_AUTHENTICATE;
use http::{HeaderValue, StatusCode};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Handler argument that requires and yields the authenticated principal of
/// type `P`.
///
/// `Principal<P>` is the bridge between *authenticating* (a scheme plugin
/// inserting a principal into the call) and *authorizing* (a handler demanding
/// one). Add it as a handler parameter to make that route require auth: the
/// extractor looks up a value of type `P` previously stored on the call by an
/// auth plugin such as [`Auth::bearer`], [`Auth::basic`], or [`Auth::jwt_hs256`].
///
/// The inner value is exposed as the public tuple field, so you typically
/// destructure it directly in the parameter list, e.g.
/// `|Principal(user): Principal<User>|`.
///
/// # Type parameter
///
/// * `P` — the principal type you chose when installing the auth scheme. It must
///   be `Clone + Send + Sync + 'static`. This is the same type the plugin's
///   `verify` closure returns (or, for [`Jwt`], the decoded claims type).
///
/// # Errors
///
/// If no principal of type `P` was inserted for this call (no/invalid/expired
/// credentials, or a mismatched principal type) the extractor returns
/// `401 Unauthorized` with a `WWW-Authenticate: Bearer` response header, and the
/// handler body never runs.
///
/// # Examples
///
/// ```
/// use churust_core::{Churust, TestClient};
/// use churust_auth::{Auth, Principal};
///
/// #[derive(Clone)]
/// struct User {
///     id: u64,
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(Auth::bearer(|tok: String| async move {
///         (tok == "ok").then(|| User { id: 7 })
///     }))
///     .routing(|r| {
///         r.get("/id", |Principal(u): Principal<User>| async move {
///             u.id.to_string()
///         });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .get("/id")
///     .header("authorization", "Bearer ok")
///     .send()
///     .await;
/// assert_eq!(res.status().as_u16(), 200);
/// assert_eq!(res.text(), "7");
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct Principal<P>(
    /// The authenticated principal value inserted by the auth scheme.
    pub P,
);

#[async_trait]
impl<P> FromCallParts for Principal<P>
where
    P: Clone + Send + Sync + 'static,
{
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        match call.get::<P>() {
            Some(p) => Ok(Principal(p)),
            None => Err(
                Error::new(StatusCode::UNAUTHORIZED, "authentication required")
                    .with_response_header(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer")),
            ),
        }
    }
}

// ---------- Bearer ----------

/// Bearer-token authentication plugin produced by [`Auth::bearer`].
///
/// On every request this plugin reads the `Authorization: Bearer <token>`
/// header (the scheme word is matched case-insensitively), trims the token, and
/// calls your `verify` closure. If the closure returns `Some(principal)`, that
/// principal is inserted into the call so a later [`Principal<P>`] extractor can
/// pick it up. A missing header, a non-bearer scheme, or a `None` result simply
/// leaves the call unauthenticated — it is never rejected here; rejection is the
/// job of [`Principal<P>`].
///
/// You rarely name this type directly; construct it with [`Auth::bearer`] and
/// hand it to [`AppBuilder::install`](churust_core::AppBuilder::install).
///
/// # Type parameters
///
/// * `P` — the principal type the closure resolves to.
/// * `F` — the verifier closure type (inferred from the argument).
///
/// # Examples
///
/// ```
/// use churust_core::{Churust, TestClient};
/// use churust_auth::{Auth, Principal};
///
/// #[derive(Clone)]
/// struct User;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(Auth::bearer(|t: String| async move { (t == "ok").then_some(User) }))
///     .routing(|r| {
///         r.get("/", |_p: Principal<User>| async { "in" });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .get("/")
///     .header("authorization", "Bearer ok")
///     .send()
///     .await;
/// assert_eq!(res.status().as_u16(), 200);
/// # });
/// ```
pub struct Bearer<P, F> {
    verify: Arc<F>,
    _p: PhantomData<fn() -> P>,
}

/// Constructor namespace for the authentication scheme plugins.
///
/// `Auth` is a zero-sized type that groups the factory functions for every
/// scheme this crate offers. You never instantiate it; call its associated
/// functions and pass the result to
/// [`AppBuilder::install`](churust_core::AppBuilder::install):
///
/// * [`Auth::bearer`] — opaque `Authorization: Bearer` tokens verified by a closure.
/// * [`Auth::basic`] — HTTP Basic `username:password` credentials verified by a closure.
/// * [`Auth::jwt_hs256`] — HS256-signed JWTs decoded into a claims principal.
///
/// # Examples
///
/// ```
/// use churust_core::Churust;
/// use churust_auth::Auth;
///
/// #[derive(Clone)]
/// struct User;
///
/// // Each factory yields a plugin ready to `.install(..)`.
/// let _app = Churust::server()
///     .install(Auth::bearer(|_t: String| async { Some(User) }))
///     .build();
/// ```
pub struct Auth;

impl Auth {
    /// Builds a [`Bearer`] plugin that authenticates `Authorization: Bearer`
    /// tokens.
    ///
    /// `verify` is invoked with the raw token string (without the `Bearer `
    /// prefix, trimmed) and returns a future resolving to `Some(principal)` when
    /// the token is valid or `None` when it is not. The returned principal `P`
    /// is what handlers later read via [`Principal<P>`].
    ///
    /// # Parameters
    ///
    /// * `verify` — async closure `Fn(String) -> impl Future<Output = Option<P>>`,
    ///   run once per request that carries a bearer token.
    ///
    /// # Examples
    ///
    /// ```
    /// use churust_core::{Churust, TestClient};
    /// use churust_auth::{Auth, Principal};
    ///
    /// #[derive(Clone)]
    /// struct User { name: String }
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(Auth::bearer(|token: String| async move {
    ///         (token == "letmein").then(|| User { name: "ada".into() })
    ///     }))
    ///     .routing(|r| {
    ///         r.get("/me", |Principal(u): Principal<User>| async move { u.name });
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .get("/me")
    ///     .header("authorization", "Bearer letmein")
    ///     .send()
    ///     .await;
    /// assert_eq!(res.text(), "ada");
    /// # });
    /// ```
    pub fn bearer<P, F, Fut>(verify: F) -> Bearer<P, F>
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<P>> + Send + 'static,
        P: Clone + Send + Sync + 'static,
    {
        Bearer {
            verify: Arc::new(verify),
            _p: PhantomData,
        }
    }

    /// Builds a [`Basic`] plugin that authenticates HTTP Basic credentials.
    ///
    /// On each request the plugin decodes the `Authorization: Basic <base64>`
    /// header into a `username` and `password` and calls `verify`. A return of
    /// `Some(principal)` authenticates the call; `None`, a malformed header, or a
    /// missing header leaves it unauthenticated. The decoded principal `P` is
    /// read by handlers via [`Principal<P>`].
    ///
    /// # Parameters
    ///
    /// * `verify` — async closure `Fn(String, String) -> impl Future<Output = Option<P>>`
    ///   receiving `(username, password)`.
    ///
    /// # Gotcha
    ///
    /// HTTP Basic transmits the password in reversibly-encoded (base64) form, so
    /// only use it over TLS.
    ///
    /// # Examples
    ///
    /// ```
    /// use churust_core::{Churust, TestClient};
    /// use churust_auth::{Auth, Principal};
    ///
    /// #[derive(Clone)]
    /// struct User { name: String }
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .install(Auth::basic(|u: String, p: String| async move {
    ///         (u == "admin" && p == "pw").then(|| User { name: u })
    ///     }))
    ///     .routing(|r| {
    ///         r.get("/me", |Principal(u): Principal<User>| async move { u.name });
    ///     })
    ///     .build();
    ///
    /// // base64("admin:pw") == "YWRtaW46cHc="
    /// let res = TestClient::new(app)
    ///     .get("/me")
    ///     .header("authorization", "Basic YWRtaW46cHc=")
    ///     .send()
    ///     .await;
    /// assert_eq!(res.text(), "admin");
    /// # });
    /// ```
    pub fn basic<P, F, Fut>(verify: F) -> Basic<P, F>
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<P>> + Send + 'static,
        P: Clone + Send + Sync + 'static,
    {
        Basic {
            verify: Arc::new(verify),
            _p: PhantomData,
        }
    }
}

impl<P, F, Fut> Plugin for Bearer<P, F>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<P>> + Send + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        let verify = self.verify.clone();
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(BearerMiddleware::<P> {
                verify: Arc::new(move |token: String| {
                    let verify = verify.clone();
                    Box::pin(async move { verify(token).await }) as BoxFut<Option<P>>
                }),
            }),
        );
    }
}

struct BearerMiddleware<P> {
    #[allow(clippy::type_complexity)]
    verify: Arc<dyn Fn(String) -> BoxFut<Option<P>> + Send + Sync>,
}

#[async_trait]
impl<P> Middleware for BearerMiddleware<P>
where
    P: Clone + Send + Sync + 'static,
{
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        if let Some(raw) = call.header("authorization") {
            if let Some(token) = raw
                .strip_prefix("Bearer ")
                .or_else(|| raw.strip_prefix("bearer "))
            {
                if let Some(principal) = (self.verify)(token.trim().to_string()).await {
                    call.insert(principal);
                }
            }
        }
        next.run(call).await
    }
}

// ---------- Basic ----------

/// HTTP Basic authentication plugin produced by [`Auth::basic`].
///
/// On every request this plugin reads `Authorization: Basic <base64>` (scheme
/// matched case-insensitively), base64-decodes it, splits the `username:password`
/// pair on the first `:`, and calls your `verify` closure. `Some(principal)`
/// authenticates the call by inserting the principal for a later
/// [`Principal<P>`] extractor; `None`, a header that is not valid base64, or one
/// without a `:` leaves the call unauthenticated. As with all schemes in this
/// crate it never rejects the request itself.
///
/// Construct it with [`Auth::basic`] rather than naming this type directly. Note
/// that Basic credentials are only base64-encoded, not encrypted, so serve such
/// routes over TLS.
///
/// # Type parameters
///
/// * `P` — the principal type the closure resolves to.
/// * `F` — the verifier closure type (inferred from the argument).
///
/// # Examples
///
/// ```
/// use churust_core::{Churust, TestClient};
/// use churust_auth::{Auth, Principal};
///
/// #[derive(Clone)]
/// struct User { name: String }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(Auth::basic(|u: String, p: String| async move {
///         (p == "hunter2").then(|| User { name: u })
///     }))
///     .routing(|r| {
///         r.get("/me", |Principal(u): Principal<User>| async move { u.name });
///     })
///     .build();
///
/// // base64("ada:hunter2") == "YWRhOmh1bnRlcjI="
/// let res = TestClient::new(app)
///     .get("/me")
///     .header("authorization", "Basic YWRhOmh1bnRlcjI=")
///     .send()
///     .await;
/// assert_eq!(res.text(), "ada");
/// # });
/// ```
pub struct Basic<P, F> {
    verify: Arc<F>,
    _p: PhantomData<fn() -> P>,
}

impl<P, F, Fut> Plugin for Basic<P, F>
where
    F: Fn(String, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<P>> + Send + 'static,
    P: Clone + Send + Sync + 'static,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        let verify = self.verify.clone();
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(BasicMiddleware::<P> {
                verify: Arc::new(move |u: String, p: String| {
                    let verify = verify.clone();
                    Box::pin(async move { verify(u, p).await }) as BoxFut<Option<P>>
                }),
            }),
        );
    }
}

struct BasicMiddleware<P> {
    #[allow(clippy::type_complexity)]
    verify: Arc<dyn Fn(String, String) -> BoxFut<Option<P>> + Send + Sync>,
}

#[async_trait]
impl<P> Middleware for BasicMiddleware<P>
where
    P: Clone + Send + Sync + 'static,
{
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        if let Some((user, pass)) = call.header("authorization").and_then(decode_basic) {
            if let Some(principal) = (self.verify)(user, pass).await {
                call.insert(principal);
            }
        }
        next.run(call).await
    }
}

fn decode_basic(header: &str) -> Option<(String, String)> {
    use base64::Engine;
    let b64 = header
        .strip_prefix("Basic ")
        .or_else(|| header.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

// ---------- JWT ----------

/// JWT bearer-token authentication plugin produced by [`Auth::jwt_hs256`].
///
/// This plugin reads `Authorization: Bearer <jwt>` (scheme matched
/// case-insensitively), verifies the token's signature and standard claims
/// against the configured key and validation, and on success inserts the decoded
/// claims of type `C` as the principal. Handlers then read those claims via
/// [`Principal<C>`]. A missing token, a bad signature, or a failed validation
/// (e.g. an expired `exp`) leaves the call unauthenticated rather than erroring.
///
/// Unlike [`Bearer`] there is no `verify` closure: trust comes from the
/// cryptographic signature, and the claims are deserialized directly into `C`.
/// Construct it with [`Auth::jwt_hs256`].
///
/// # Type parameter
///
/// * `C` — the claims type, which must be
///   `serde::de::DeserializeOwned + Clone + Send + Sync + 'static`. By default
///   `jsonwebtoken` validates `exp`, so include an `exp` field in `C`.
///
/// # Examples
///
/// ```
/// use churust_core::{Churust, TestClient};
/// use churust_auth::{Auth, Principal};
/// use jsonwebtoken::{encode, EncodingKey, Header};
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, Clone)]
/// struct Claims { sub: String, exp: usize }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let secret = b"shhh";
/// let token = encode(
///     &Header::default(),
///     &Claims { sub: "u1".into(), exp: 9_999_999_999 },
///     &EncodingKey::from_secret(secret),
/// ).unwrap();
///
/// let app = Churust::server()
///     .install(Auth::jwt_hs256::<Claims>(secret))
///     .routing(|r| {
///         r.get("/who", |Principal(c): Principal<Claims>| async move { c.sub });
///     })
///     .build();
///
/// let res = TestClient::new(app)
///     .get("/who")
///     .header("authorization", &format!("Bearer {token}"))
///     .send()
///     .await;
/// assert_eq!(res.text(), "u1");
/// # });
/// ```
pub struct Jwt<C> {
    key: jsonwebtoken::DecodingKey,
    validation: jsonwebtoken::Validation,
    _c: PhantomData<fn() -> C>,
}

impl Auth {
    /// Builds a [`Jwt`] plugin that verifies HS256-signed JWT bearer tokens with
    /// an HMAC secret.
    ///
    /// Uses the default `jsonwebtoken` validation for the `HS256` algorithm
    /// (which, among other things, requires and checks an `exp` claim). Valid
    /// tokens are decoded into the claims type `C` and inserted as the principal
    /// for handlers to read with [`Principal<C>`].
    ///
    /// # Parameters
    ///
    /// * `secret` — the shared HMAC secret bytes used to sign and verify tokens.
    ///   The same bytes must be used by whatever issues the tokens.
    ///
    /// # Gotcha
    ///
    /// Because default validation enforces `exp`, a claims type without an `exp`
    /// field (or with one in the past) will fail to validate and leave the call
    /// unauthenticated.
    ///
    /// # Examples
    ///
    /// ```
    /// use churust_core::Churust;
    /// use churust_auth::Auth;
    /// use serde::{Deserialize, Serialize};
    ///
    /// #[derive(Serialize, Deserialize, Clone)]
    /// struct Claims { sub: String, exp: usize }
    ///
    /// let _app = Churust::server()
    ///     .install(Auth::jwt_hs256::<Claims>(b"shared-secret"))
    ///     .build();
    /// ```
    pub fn jwt_hs256<C>(secret: &[u8]) -> Jwt<C>
    where
        C: serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
    {
        Jwt {
            key: jsonwebtoken::DecodingKey::from_secret(secret),
            validation: jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
            _c: PhantomData,
        }
    }
}

impl<C> Plugin for Jwt<C>
where
    C: serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(
            Phase::Plugins,
            Arc::new(JwtMiddleware::<C> {
                key: Arc::new(self.key),
                validation: Arc::new(self.validation),
                _c: PhantomData,
            }),
        );
    }
}

struct JwtMiddleware<C> {
    key: Arc<jsonwebtoken::DecodingKey>,
    validation: Arc<jsonwebtoken::Validation>,
    _c: PhantomData<fn() -> C>,
}

#[async_trait]
impl<C> Middleware for JwtMiddleware<C>
where
    C: serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
{
    async fn handle(&self, mut call: Call, next: Next) -> Response {
        if let Some(raw) = call.header("authorization") {
            if let Some(token) = raw
                .strip_prefix("Bearer ")
                .or_else(|| raw.strip_prefix("bearer "))
            {
                if let Ok(data) =
                    jsonwebtoken::decode::<C>(token.trim(), &self.key, &self.validation)
                {
                    call.insert(data.claims);
                }
            }
        }
        next.run(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use churust_core::{App, Churust, TestClient};
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq)]
    struct User {
        name: String,
    }

    fn bearer_app() -> App {
        Churust::server()
            .install(Auth::bearer(|token: String| async move {
                if token == "secret" {
                    Some(User { name: "ana".into() })
                } else {
                    None
                }
            }))
            .routing(|r| {
                r.get("/me", |Principal(u): Principal<User>| async move {
                    format!("hello {}", u.name)
                });
            })
            .build()
    }

    #[tokio::test]
    async fn valid_bearer_reaches_protected_route() {
        let client = TestClient::new(bearer_app());
        let res = client
            .get("/me")
            .header("authorization", "Bearer secret")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "hello ana");
    }

    #[tokio::test]
    async fn missing_token_is_401_with_challenge() {
        let client = TestClient::new(bearer_app());
        let res = client.get("/me").send().await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    }

    #[tokio::test]
    async fn wrong_token_is_401() {
        let client = TestClient::new(bearer_app());
        let res = client
            .get("/me")
            .header("authorization", "Bearer nope")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn basic_auth_works() {
        let app = Churust::server()
            .install(Auth::basic(|u: String, p: String| async move {
                if u == "admin" && p == "pw" {
                    Some(User { name: u })
                } else {
                    None
                }
            }))
            .routing(|r| {
                r.get("/me", |Principal(u): Principal<User>| async move { u.name });
            })
            .build();
        let client = TestClient::new(app);
        // base64("admin:pw") = YWRtaW46cHc=
        let res = client
            .get("/me")
            .header("authorization", "Basic YWRtaW46cHc=")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "admin");
    }

    #[derive(Serialize, Deserialize, Clone, Debug)]
    struct Claims {
        sub: String,
        exp: usize,
    }

    #[tokio::test]
    async fn jwt_decodes_claims() {
        use jsonwebtoken::{encode, EncodingKey, Header};
        let secret = b"topsecret";
        let claims = Claims {
            sub: "u42".into(),
            exp: 9_999_999_999,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap();

        let app = Churust::server()
            .install(Auth::jwt_hs256::<Claims>(secret))
            .routing(|r| {
                r.get(
                    "/who",
                    |Principal(c): Principal<Claims>| async move { c.sub },
                );
            })
            .build();
        let client = TestClient::new(app);
        let res = client
            .get("/who")
            .header("authorization", &format!("Bearer {token}"))
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "u42");
    }
}
