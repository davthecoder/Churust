//! Handlers: the [`Handler`] trait, handler closures, and the glue that turns
//! extractor closures into stored handlers.
//!
//! Most route handlers are plain async closures (`|c: Call| async { ... }`, or
//! a closure taking [extractors](crate::extract)). They reach the [`Router`] via
//! [`IntoHandler`], which adapts a closure into a real [`Handler`] and lets it
//! be [`boxed`] into a [`BoxHandler`] for storage.
//!
//! [`Router`]: crate::Router

use crate::call::Call;
use crate::response::{IntoResponse, Response};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A boxed, `'static` future returned by a [`Handler`].
///
/// It is `'static` because a handler owns its [`Call`] (the call is moved in),
/// so nothing is borrowed across the await point.
pub type HandlerFuture = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

/// Anything that can handle a [`Call`] and produce a [`Response`].
///
/// This is the type-erased shape the [`Router`](crate::Router) stores (as a
/// [`BoxHandler`]). You rarely implement it by hand — async closures become
/// `Handler`s through [`IntoHandler`] — but you can implement it directly for a
/// type that needs to carry state.
///
/// ```
/// use churust_core::{Call, Handler, Response, IntoResponse, handler::HandlerFuture};
/// use http::{HeaderMap, Method};
/// use bytes::Bytes;
///
/// struct Greeter;
/// impl Handler for Greeter {
///     fn handle(&self, _call: Call) -> HandlerFuture {
///         Box::pin(async { "hi".into_response() })
///     }
/// }
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let call = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
/// let res = Greeter.handle(call).await;
/// assert_eq!(res.body.as_slice(), Some(&b"hi"[..]));
/// # });
/// ```
pub trait Handler: Send + Sync + 'static {
    /// Handle the call, returning a future that resolves to the response.
    fn handle(&self, call: Call) -> HandlerFuture;
}

use crate::extract::{FromCall, FromCallParts};

/// Bridge trait implemented by handler closures of a given arity. The LAST
/// closure argument is `FromCall` (consuming); all earlier ones are
/// `FromCallParts` (borrowing). Handlers must be `Clone` so the closure can be
/// moved into the `'static` response future.
///
/// IMPLEMENTATION NOTE (why this is not the spec's literal macro): the plan's
/// Step 2 shows a macro that emits `impl<...> Handler for F` directly for each
/// arity. That design does NOT compile on stable Rust: the coherence checker
/// treats `impl Handler for F where F: Fn()`, `... F: Fn(A)`, `... F: Fn(A, B)`
/// etc. as mutually overlapping, because a single closure type could in
/// principle implement several `Fn` traits of different arities (E0119 on every
/// arity). A disambiguating `Marker` type parameter is required to keep the
/// per-arity impls disjoint — the standard axum-style pattern. The public
/// `Handler` trait, `BoxHandler`, and `boxed` keep the spec signatures; this
/// `HandlerFn` family is the closure-side bridge that is adapted into a
/// `Handler` via [`IntoHandler`] (see below). This trait is `pub` only because
/// it appears in the bound of `IntoHandler`/the router's method builders; it is
/// intentionally NOT re-exported from the crate root.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a Churust handler",
    label = "not a handler",
    note = "a handler is an `async fn` or a closure returning a future, taking up to 8 arguments",
    note = "every argument except the last must implement `FromCallParts` (it sees only the request head)",
    note = "the last argument may implement `FromCall` and consume the body — `Json`, `Form`, `Bytes`, `String`, `Payload`, `Multipart`, `Either`",
    note = "two body-consuming arguments cannot compile: the body is a one-shot stream",
    note = "the return type must implement `IntoResponse`"
)]
pub trait HandlerFn<Marker>: Clone + Send + Sync + 'static {
    /// Run the closure against the call: extract each argument from the call in
    /// order, then invoke the closure and convert its return value into a
    /// [`Response`].
    fn call(self, call: Call) -> HandlerFuture;
}

/// Generates a `HandlerFn` impl whose closure args are `($($P,)* L)` — the
/// `$P` are the leading `FromCallParts` extractors and `L` is the final
/// `FromCall` extractor. `impl_handler!()` (empty) produces the arity-1
/// (last-arg-only) impl; the arity-0 impl is written out separately below.
macro_rules! impl_handler {
    ($($P:ident),*) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, R, $($P,)* L> HandlerFn<($($P,)* L,)> for F
        where
            F: Fn($($P,)* L) -> Fut + Clone + Send + Sync + 'static,
            Fut: std::future::Future<Output = R> + Send + 'static,
            R: IntoResponse + 'static,
            $($P: FromCallParts + 'static,)*
            L: FromCall + 'static,
        {
            fn call(self, call: Call) -> HandlerFuture {
                let f = self;
                Box::pin(async move {
                    let mut call = call;
                    $(
                        let $P = match <$P as FromCallParts>::from_call_parts(&mut call).await {
                            Ok(v) => v,
                            Err(e) => return e.into_response(),
                        };
                    )*
                    let last = match <L as FromCall>::from_call(call).await {
                        Ok(v) => v,
                        Err(e) => return e.into_response(),
                    };
                    f($($P,)* last).await.into_response()
                })
            }
        }
    };
}

// Arity 0: a parameterless closure.
impl<F, Fut, R> HandlerFn<()> for F
where
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = R> + Send + 'static,
    R: IntoResponse + 'static,
{
    fn call(self, _call: Call) -> HandlerFuture {
        let f = self;
        Box::pin(async move { f().await.into_response() })
    }
}

// Arity 1 (last arg only) through arity 9 (eight leading parts + last).
impl_handler!();
impl_handler!(P1);
impl_handler!(P1, P2);
impl_handler!(P1, P2, P3);
impl_handler!(P1, P2, P3, P4);
impl_handler!(P1, P2, P3, P4, P5);
impl_handler!(P1, P2, P3, P4, P5, P6);
impl_handler!(P1, P2, P3, P4, P5, P6, P7);
impl_handler!(P1, P2, P3, P4, P5, P6, P7, P8);

/// A type-erased, shared handler — the form the [`Router`](crate::Router)
/// stores. Create one with [`boxed`].
pub type BoxHandler = Arc<dyn Handler>;

/// Adapter that wraps a marker-disambiguated [`HandlerFn`] closure into a real
/// [`Handler`].
///
/// This is the bridge that lets the [`boxed`] / router APIs accept extractor
/// closures: the closure is wrapped here, then boxed. The `Marker` type
/// parameter only encodes the closure's argument shape (so per-arity impls stay
/// disjoint) and is erased once wrapped, so it never escapes into
/// [`BoxHandler`]. You normally obtain one via [`IntoHandler::into_handler`]
/// rather than naming it directly.
pub struct HandlerFnAdapter<Marker, H> {
    handler: H,
    _marker: std::marker::PhantomData<fn() -> Marker>,
}

impl<Marker, H> HandlerFnAdapter<Marker, H>
where
    H: HandlerFn<Marker>,
{
    fn new(handler: H) -> Self {
        Self {
            handler,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<Marker, H> Handler for HandlerFnAdapter<Marker, H>
where
    Marker: 'static,
    H: HandlerFn<Marker>,
{
    fn handle(&self, call: Call) -> HandlerFuture {
        self.handler.clone().call(call)
    }
}

/// Bridge from a closure (or anything that is already a `Handler`) into a value
/// implementing `Handler`, so it can be passed to `boxed` and the router.
///
/// Two disjoint impls:
/// - extractor closures (`HandlerFn<Marker>`) wrap into a `HandlerFnAdapter`;
/// - anything already implementing `Handler` (including a `BoxHandler`) passes
///   through unchanged.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be used as a route handler",
    label = "not a valid handler",
    note = "check each argument: all but the last must implement `FromCallParts`, and only the last may consume the body",
    note = "a body-consuming extractor (`Json`, `Form`, `Bytes`, `String`, `Payload`, `Multipart`, `Either`) must come last, and there can be only one",
    note = "`Option<T>` works where `T` implements `OptionalFromCallParts`",
    note = "the closure's return type must implement `IntoResponse`; `Result<T, E>` works when both `T: IntoResponse` and `E: IntoError`"
)]
pub trait IntoHandler<Marker> {
    /// The concrete [`Handler`] type this value converts into.
    type Handler: Handler;
    /// Convert `self` into its [`Handler`] representation.
    fn into_handler(self) -> Self::Handler;
}

/// Marker for the "already a `Handler`" pass-through impl, kept distinct from
/// the closure-arity tuple markers so the two impls never overlap.
#[doc(hidden)]
pub struct IsHandler;

// Without this, a closure that fails its bounds is reported against the
// pass-through impl — "the trait `Handler` is not implemented for
// `{closure}`" — which sends the reader to implement `Handler` by hand rather
// than to the argument that is actually wrong.
#[diagnostic::do_not_recommend]
impl<H: Handler> IntoHandler<IsHandler> for H {
    type Handler = H;
    fn into_handler(self) -> H {
        self
    }
}

/// Closures of any supported arity convert via the adapter. The wrapping marker
/// keeps this disjoint from the `IsHandler` pass-through (closures do not
/// implement `Handler` directly, so no closure can match both impls).
impl<Marker, H> IntoHandler<(IsHandler, Marker)> for H
where
    Marker: 'static,
    H: HandlerFn<Marker>,
{
    type Handler = HandlerFnAdapter<Marker, H>;
    fn into_handler(self) -> HandlerFnAdapter<Marker, H> {
        HandlerFnAdapter::new(self)
    }
}

/// `Arc<dyn Handler>` (i.e. `BoxHandler`) is itself a `Handler`, so a
/// pre-boxed handler can be re-boxed or passed to the router builders.
impl Handler for BoxHandler {
    fn handle(&self, call: Call) -> HandlerFuture {
        (**self).handle(call)
    }
}

/// Box a [`Handler`] into a shareable [`BoxHandler`] for storage in the
/// [`Router`](crate::Router).
///
/// This accepts any `H: Handler`. Extractor closures are not `Handler`s
/// directly — convert them first with [`IntoHandler::into_handler`] (the router
/// builders do this for you). A [`BoxHandler`] is itself a `Handler`, so it may
/// be re-boxed.
///
/// ```
/// use churust_core::{boxed, Call, IntoHandler};
/// use http::{HeaderMap, Method};
/// use bytes::Bytes;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let handler = boxed((|_c: Call| async { "hi" }).into_handler());
/// let call = Call::new(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new());
/// let res = handler.handle(call).await;
/// assert_eq!(res.body.as_slice(), Some(&b"hi"[..]));
/// # });
/// ```
pub fn boxed<H: Handler>(h: H) -> BoxHandler {
    Arc::new(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, Method, StatusCode, Uri};

    fn sample_call() -> Call {
        Call::new(
            Method::GET,
            "/".parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
    }

    #[tokio::test]
    async fn closure_returning_str_is_a_handler() {
        let h = boxed((|_call: Call| async move { "hello" }).into_handler());
        let res = h.handle(sample_call()).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, Bytes::from("hello"));
    }

    #[tokio::test]
    async fn closure_returning_result_is_a_handler() {
        let h = boxed(
            (|call: Call| async move {
                let _ = call.path();
                Ok::<_, crate::Error>((StatusCode::CREATED, "made"))
            })
            .into_handler(),
        );
        let res = h.handle(sample_call()).await;
        assert_eq!(res.status, StatusCode::CREATED);
    }

    use crate::extract::FromCallParts;

    struct Greeting(&'static str);
    #[async_trait::async_trait]
    impl FromCallParts for Greeting {
        async fn from_call_parts(_call: &mut Call) -> crate::Result<Self> {
            Ok(Greeting("hi"))
        }
    }

    #[tokio::test]
    async fn extractor_style_handler_works() {
        let h = boxed((|g: Greeting| async move { g.0 }).into_handler());
        let res = h.handle(sample_call()).await;
        assert_eq!(res.body, bytes::Bytes::from("hi"));
    }

    // Regression guard for the spec's `boxed<H: Handler>` signature: an explicit
    // `impl Handler for MyType` must be passable to `boxed()` directly.
    struct ManualHandler;
    impl Handler for ManualHandler {
        fn handle(&self, _call: Call) -> HandlerFuture {
            Box::pin(async move { "manual".into_response() })
        }
    }

    #[tokio::test]
    async fn manual_impl_handler_is_boxable() {
        let h = boxed(ManualHandler);
        let res = h.handle(sample_call()).await;
        assert_eq!(res.body, Bytes::from("manual"));
    }

    // Regression guard: a pre-boxed `BoxHandler` is itself a `Handler`, so it can
    // be re-boxed and (in the router) re-registered.
    #[tokio::test]
    async fn box_handler_is_a_handler() {
        let boxed_once: BoxHandler = boxed(ManualHandler);
        let boxed_twice = boxed(boxed_once);
        let res = boxed_twice.handle(sample_call()).await;
        assert_eq!(res.body, Bytes::from("manual"));
    }
}
