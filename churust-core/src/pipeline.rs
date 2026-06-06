//! The request pipeline: [`Middleware`], the [`Phase`] ordering, the [`Next`]
//! continuation, and the [`Endpoint`] terminal.
//!
//! Middleware runs as an onion: each layer may inspect/modify the [`Call`] on
//! the way in, calls `next.run(call)` to proceed, and may post-process the
//! [`Response`] on the way out — or short-circuit by returning a response
//! without calling `next`. Layers execute in [`Phase`] order; the matched route
//! handler (the [`Endpoint`]) sits at the center.

use crate::call::Call;
use crate::response::Response;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The ordered insertion points for middleware (Ktor-style).
///
/// Lower-ordered variants run further "outside" in the onion — first on the way
/// in, last on the way out. The [`Ord`] derivation follows declaration order,
/// and [`AppBuilder::build`](crate::AppBuilder::build) sorts installed
/// middleware by phase (stably, so install order is preserved within a phase).
///
/// ```
/// use churust_core::Phase;
/// assert!(Phase::Setup < Phase::Monitoring);
/// assert!(Phase::Monitoring < Phase::Plugins);
/// assert!(Phase::Plugins < Phase::Call);
/// assert!(Phase::Call < Phase::Fallback);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Earliest phase, for foundational setup that must wrap everything else.
    Setup,
    /// Observability concerns (logging, metrics, tracing).
    Monitoring,
    /// Default phase for installed plugins/middleware.
    Plugins,
    /// Closest to the route handler (e.g. per-call last-mile logic).
    Call,
    /// Latest phase, for catch-all fallbacks.
    Fallback,
}

/// The terminal of the pipeline: a function that routes the call and runs the
/// matched handler.
///
/// It returns a `'static` boxed future because the [`Call`] is owned (moved in).
/// The framework constructs the endpoint internally; middleware reaches it by
/// exhausting the [`Next`] chain.
pub type Endpoint =
    Arc<dyn Fn(Call) -> Pin<Box<dyn Future<Output = Response> + Send + 'static>> + Send + Sync>;

/// A pipeline interceptor — one layer of the onion.
///
/// An implementor owns the [`Call`], may inspect or modify it, calls
/// `next.run(call)` to proceed inward, and may post-process the resulting
/// [`Response`]. Returning a response *without* calling `next` short-circuits
/// the remaining layers and the handler. Install a middleware with
/// [`AppBuilder::add_middleware`](crate::AppBuilder::add_middleware) or, for a
/// specific [`Phase`], [`AppBuilder::add_middleware_in`](crate::AppBuilder::add_middleware_in).
///
/// ```
/// use churust_core::{Call, Churust, Middleware, Next, Response, TestClient};
/// use churust_core::pipeline::Phase;
/// use async_trait::async_trait;
/// use std::sync::Arc;
/// use http::{header::HeaderName, HeaderValue};
///
/// struct Stamp;
/// #[async_trait]
/// impl Middleware for Stamp {
///     async fn handle(&self, call: Call, next: Next) -> Response {
///         let mut res = next.run(call).await; // proceed inward
///         res.headers.insert(HeaderName::from_static("x-stamp"), HeaderValue::from_static("1"));
///         res // post-process on the way out
///     }
/// }
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let mut builder = Churust::server();
/// builder.add_middleware_in(Phase::Plugins, Arc::new(Stamp));
/// let app = builder.routing(|r| { r.get("/", |_c: Call| async { "ok" }); }).build();
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.header("x-stamp"), Some("1"));
/// # });
/// ```
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    /// Handle the call: optionally inspect/modify it, call `next.run(call)` to
    /// proceed, and return the (possibly post-processed) response.
    async fn handle(&self, call: Call, next: Next) -> Response;
}

/// The remaining middleware chain plus the terminal [`Endpoint`].
///
/// It is owned (via `Arc` clones) and carries no lifetime parameters, so it
/// threads cleanly through `async_trait` futures. A [`Middleware`] advances the
/// pipeline by calling [`Next::run`].
pub struct Next {
    chain: VecDeque<Arc<dyn Middleware>>,
    endpoint: Endpoint,
}

impl Next {
    pub(crate) fn new(chain: VecDeque<Arc<dyn Middleware>>, endpoint: Endpoint) -> Self {
        Self { chain, endpoint }
    }

    /// Run the next middleware in the chain, or the [`Endpoint`] if the chain is
    /// exhausted, consuming `self`. Call this from a [`Middleware`] to proceed
    /// inward; not calling it short-circuits the rest of the pipeline.
    pub async fn run(mut self, call: Call) -> Response {
        match self.chain.pop_front() {
            Some(mw) => mw.handle(call, self).await,
            None => (self.endpoint)(call).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::IntoResponse;

    #[test]
    fn phases_are_ordered() {
        assert!(Phase::Setup < Phase::Monitoring);
        assert!(Phase::Monitoring < Phase::Plugins);
        assert!(Phase::Plugins < Phase::Call);
        assert!(Phase::Call < Phase::Fallback);
    }
    use bytes::Bytes;
    use http::header::HeaderName;
    use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};

    fn sample_call() -> Call {
        Call::new(
            Method::GET,
            "/".parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
    }

    fn endpoint() -> Endpoint {
        Arc::new(|_call: Call| Box::pin(async { "inner".into_response() }) as _)
    }

    struct AddHeader;
    #[async_trait]
    impl Middleware for AddHeader {
        async fn handle(&self, call: Call, next: Next) -> Response {
            let mut res = next.run(call).await;
            res.headers.insert(
                HeaderName::from_static("x-mw"),
                HeaderValue::from_static("1"),
            );
            res
        }
    }

    struct ShortCircuit;
    #[async_trait]
    impl Middleware for ShortCircuit {
        async fn handle(&self, _call: Call, _next: Next) -> Response {
            Response::new(StatusCode::FORBIDDEN)
        }
    }

    #[tokio::test]
    async fn runs_endpoint_when_chain_empty() {
        let next = Next::new(VecDeque::new(), endpoint());
        let res = next.run(sample_call()).await;
        assert_eq!(res.body, Bytes::from("inner"));
    }

    #[tokio::test]
    async fn middleware_post_processes_response() {
        let mut chain: VecDeque<Arc<dyn Middleware>> = VecDeque::new();
        chain.push_back(Arc::new(AddHeader));
        let res = Next::new(chain, endpoint()).run(sample_call()).await;
        assert_eq!(res.headers.get("x-mw").unwrap(), "1");
        assert_eq!(res.body, Bytes::from("inner"));
    }

    #[tokio::test]
    async fn middleware_can_short_circuit() {
        let mut chain: VecDeque<Arc<dyn Middleware>> = VecDeque::new();
        chain.push_back(Arc::new(ShortCircuit));
        chain.push_back(Arc::new(AddHeader)); // should never run
        let res = Next::new(chain, endpoint()).run(sample_call()).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN);
        assert!(res.headers.get("x-mw").is_none());
    }
}
