//! Run a `tower::Service` as Churust [`Middleware`].
//!
//! Requires the `tower` feature.
//!
//! Churust has its own middleware model — [`Middleware`] + [`Next`], an onion
//! where each layer awaits the next — and that stays the native way to write
//! one. This adapter exists so an application does not have to reimplement
//! something the ecosystem already ships as a `Layer`: response compression,
//! structured tracing, request-id propagation, header manipulation, request
//! validation, metrics.
//!
//! The adapter is deliberately **one-directional**. Churust middleware is not
//! exposed as a tower `Service`; only the reverse.
//!
//! # What this does not do
//!
//! **Backpressure is not propagated.** tower's `poll_ready` contract — a
//! service signalling that it cannot accept work yet — has no counterpart in
//! `Middleware::handle`, which is handed a request that has already been
//! accepted. The adapter drives `poll_ready` to readiness before calling, so a
//! layer that uses it for load-shedding will *wait* rather than shed. Bound
//! concurrency with `max_connections` instead.
//!
//! **The inner service must be called from the same task.** The request's
//! continuation is held in a task-local, so a layer that hands the request to a
//! worker task (`tower::buffer::Buffer`, `spawn_ready`, and anything built on
//! them) cannot reach it and gets an error rather than a response.
//!
//! **The request method is not written back.** Routing has already happened by
//! the time a layer runs, so a layer that rewrites the method would leave the
//! call disagreeing with the handler it is about to reach. Header and URI
//! rewrites *are* carried back.
//!
//! **A `Service` error becomes a `500`.** This keeps Churust's invariant that a
//! response is always produced, without importing axum's tax for it: axum
//! requires every composed service to have `Error = Infallible`, which is a
//! genuine compile-time guarantee but forces a `HandleErrorLayer` around any
//! fallible layer — adding a timeout to a route literally requires wrapping it.
//! Here the error is absorbed through [`IntoError`](crate::IntoError), so the
//! guarantee holds and the ergonomics stay Churust's.
//!
//! ```no_run
//! use churust_core::{Churust, Call, tower::TowerMiddleware};
//! # use std::sync::Arc;
//! # fn some_layer() -> tower_layer::Identity { tower_layer::Identity::new() }
//! let app = Churust::server()
//!     .install(TowerMiddleware::new(some_layer()))
//!     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
//!     .build();
//! ```

use crate::app::{AppBuilder, Plugin};
use crate::body::Body;
use crate::call::Call;
use crate::pipeline::{Middleware, Next, Phase};
use crate::response::{IntoResponse, Response};
use async_trait::async_trait;
use std::sync::Arc;
use tower_service::Service;

tokio::task_local! {
    /// The call and continuation for the request currently passing through the
    /// adapter.
    ///
    /// A task-local rather than a request extension: `http::Extensions` requires
    /// `Clone`, and [`Next`] is a one-shot continuation that must not be
    /// cloneable. A task-local also lets the layered service be built **once**,
    /// at install time, so a stateful layer keeps its state across requests
    /// instead of being rebuilt per call.
    static IN_FLIGHT: std::cell::RefCell<Option<(Call, Next)>>;
}

/// The innermost service: hands the request back to the Churust pipeline.
///
/// Everything a `Layer` wraps around this eventually calls it, which is the
/// point at which the rest of the onion — including the route handler — runs.
#[derive(Clone, Copy, Default)]
pub struct NextService;

impl Service<http::Request<Body>> for NextService {
    type Response = http::Response<Body>;
    type Error = crate::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // Always ready: the request has already been accepted by the time any
        // of this runs, so there is nothing left to refuse.
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        // Recover the pair stashed by the adapter. Taking it here means a layer
        // that calls the inner service twice gets a clear error rather than a
        // silently duplicated request.
        // `try_with`, not `with`: `with` panics when the task-local is unset,
        // which happens whenever a layer drives the inner service from a
        // *different* task (tower::buffer::Buffer, spawn_ready, anything that
        // hands the request to a worker). A panic there is caught upstream and
        // rendered as a bare 500 with no hint of the cause; an error at least
        // names the restriction.
        let taken = match IN_FLIGHT.try_with(|slot| slot.borrow_mut().take()) {
            Ok(taken) => taken,
            Err(_) => {
                return Box::pin(async {
                    Err(crate::Error::internal(
                        "this tower layer drove the inner service from another task; \
                         the Churust adapter cannot cross task boundaries",
                    ))
                })
            }
        };
        Box::pin(async move {
            let Some((mut call, next)) = taken else {
                return Err(crate::Error::internal(
                    "tower layer called the inner service more than once per request",
                ));
            };
            // Header *and* URI changes made by the layers on the way in are the
            // reason this adapter is worth having, so carry both onto the call.
            // Copying only the headers meant a path-rewriting layer silently did
            // nothing. The method is deliberately not written back: routing has
            // already happened by this point, so changing it here would leave
            // the call disagreeing with the handler it is about to reach.
            *call.headers_mut() = req.headers().clone();
            call.set_uri(req.uri().clone());
            let res = next.run(call).await;
            let mut out = http::Response::builder().status(res.status);
            if let Some(h) = out.headers_mut() {
                *h = res.headers;
            }
            Ok(out.body(res.body).expect("response build is infallible"))
        })
    }
}

/// Churust [`Middleware`] wrapping a tower `Layer` applied over [`NextService`].
///
/// Build one with [`TowerMiddleware::new`] and install it like any other
/// plugin. The layered service is constructed once, so layer state persists
/// across requests.
pub struct TowerMiddleware<S> {
    /// `Service::call` needs `&mut self`, but `Middleware::handle` takes
    /// `&self` and runs concurrently. The mutex is held only long enough to
    /// clone the service, which is what tower expects callers to do.
    service: std::sync::Mutex<S>,
    phase: Phase,
}

impl<S> TowerMiddleware<S> {
    /// Apply `layer` over the Churust pipeline.
    ///
    /// Runs in [`Phase::Plugins`]; use [`TowerMiddleware::in_phase`] to place
    /// it earlier or later in the onion.
    pub fn new<L>(layer: L) -> Self
    where
        L: tower_layer::Layer<NextService, Service = S>,
    {
        TowerMiddleware {
            service: std::sync::Mutex::new(layer.layer(NextService)),
            phase: Phase::Plugins,
        }
    }

    /// Run this layer in a specific [`Phase`] rather than [`Phase::Plugins`].
    pub fn in_phase(mut self, phase: Phase) -> Self {
        self.phase = phase;
        self
    }
}

#[async_trait]
impl<S, ResBody> Middleware for TowerMiddleware<S>
where
    S: Service<http::Request<Body>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Error: std::fmt::Display + Send,
    S::Future: Send,
    ResBody: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    ResBody::Error: std::fmt::Display,
{
    async fn handle(&self, call: Call, next: Next) -> Response {
        // Clone per request: tower services are cheap to clone by design, and
        // `call` needs `&mut`. State that matters lives behind the clone.
        let mut service = match self.service.lock() {
            Ok(s) => s.clone(),
            // A poisoned mutex means some other request panicked while cloning.
            // Nothing here is left inconsistent, so carry on rather than
            // failing every subsequent request.
            Err(poisoned) => poisoned.into_inner().clone(),
        };

        let req = {
            let mut b = http::Request::builder()
                .method(call.method().clone())
                .uri(call.uri().clone());
            if let Some(h) = b.headers_mut() {
                *h = call.headers().clone();
            }
            // The body is not handed to the layer: it belongs to the call, and
            // moving it here would consume it before the handler runs. Layers
            // that rewrite request bodies are out of scope for this adapter.
            b.body(Body::empty()).expect("request build is infallible")
        };

        let slot = std::cell::RefCell::new(Some((call, next)));
        let outcome = IN_FLIGHT
            .scope(slot, async move {
                futures_util::future::poll_fn(|cx| service.poll_ready(cx))
                    .await
                    .map_err(|e| format!("tower service not ready: {e}"))?;
                service
                    .call(req)
                    .await
                    .map_err(|e| format!("tower service failed: {e}"))
            })
            .await;

        match outcome {
            Ok(res) => {
                let (parts, body) = res.into_parts();
                let mut out = Response::new(parts.status);
                out.headers = parts.headers;
                // Read the length before rewrapping. Streaming everything is
                // right for a compression layer, whose output size is unknown —
                // but doing it unconditionally threw away the exact size of an
                // already-buffered body, so every response through any layer
                // (even `Identity`) lost its `Content-Length` and got chunked,
                // and a synthesized `HEAD` could no longer report a length.
                let exact = http_body::Body::size_hint(&body).exact();
                out.body = Body::from_stream(http_body_util::BodyDataStream::new(body));
                if let Some(len) = exact {
                    if !out.headers.contains_key(http::header::CONTENT_LENGTH) {
                        if let Ok(value) = http::HeaderValue::from_str(&len.to_string()) {
                            out.headers.insert(http::header::CONTENT_LENGTH, value);
                        }
                    }
                }
                out
            }
            // The invariant holds: a response is always produced.
            Err(message) => crate::Error::internal(message).into_response(),
        }
    }
}

impl<S, ResBody> Plugin for TowerMiddleware<S>
where
    S: Service<http::Request<Body>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Error: std::fmt::Display + Send,
    S::Future: Send,
    ResBody: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    ResBody::Error: std::fmt::Display,
{
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        let phase = self.phase;
        app.add_middleware_in(phase, Arc::new(*self));
    }
}
