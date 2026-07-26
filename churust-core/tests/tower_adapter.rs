#![cfg(feature = "tower")]
//! Running a tower `Service` as Churust middleware.
//!
//! The point of the adapter is that an application can reach the existing
//! `Layer` ecosystem — compression, tracing, request ids, header manipulation —
//! without Churust reimplementing any of it. These tests use hand-written
//! layers rather than a `tower-http` dependency, so they check the *adapter*
//! rather than someone else's middleware.

use churust_core::tower::{NextService, TowerMiddleware};
use churust_core::{Body, Call, Churust, Phase, TestClient};
use http::{HeaderValue, StatusCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tower_service::Service;

/// A layer that stamps a response header and counts the requests it has seen.
///
/// The counter is the interesting part: it lives in the *layer*, so it proves
/// the layered service is built once rather than rebuilt per request.
#[derive(Clone)]
struct Stamp<S> {
    inner: S,
    seen: Arc<AtomicUsize>,
}

struct StampLayer(Arc<AtomicUsize>);

impl<S> tower_layer::Layer<S> for StampLayer {
    type Service = Stamp<S>;
    fn layer(&self, inner: S) -> Stamp<S> {
        Stamp {
            inner,
            seen: self.0.clone(),
        }
    }
}

impl<S, B> Service<http::Request<Body>> for Stamp<S>
where
    S: Service<http::Request<Body>, Response = http::Response<B>> + Send,
    S::Future: Send + 'static,
{
    type Response = http::Response<B>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, S::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<Body>) -> Self::Future {
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        // Prove inbound header edits reach the handler.
        req.headers_mut()
            .insert("x-stamped", HeaderValue::from_static("in"));
        let fut = self.inner.call(req);
        Box::pin(async move {
            let mut res = fut.await?;
            res.headers_mut()
                .insert("x-seen", HeaderValue::from_str(&n.to_string()).unwrap());
            Ok(res)
        })
    }
}

/// A layer that never calls the inner service — short-circuiting on the way in.
#[derive(Clone)]
struct Block<S>(#[allow(dead_code)] S);
struct BlockLayer;

impl<S> tower_layer::Layer<S> for BlockLayer {
    type Service = Block<S>;
    fn layer(&self, inner: S) -> Block<S> {
        Block(inner)
    }
}

impl<S> Service<http::Request<Body>> for Block<S> {
    type Response = http::Response<Body>;
    type Error = churust_core::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: http::Request<Body>) -> Self::Future {
        Box::pin(async {
            Ok(http::Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("blocked"))
                .unwrap())
        })
    }
}

/// A layer that fails, to prove an error still produces a response.
#[derive(Clone)]
struct Boom<S>(#[allow(dead_code)] S);
struct BoomLayer;

impl<S> tower_layer::Layer<S> for BoomLayer {
    type Service = Boom<S>;
    fn layer(&self, inner: S) -> Boom<S> {
        Boom(inner)
    }
}

impl<S> Service<http::Request<Body>> for Boom<S> {
    type Response = http::Response<Body>;
    type Error = churust_core::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: http::Request<Body>) -> Self::Future {
        Box::pin(async { Err(churust_core::Error::internal("layer exploded")) })
    }
}

fn app_with_stamp(seen: Arc<AtomicUsize>) -> churust_core::App {
    Churust::server()
        .install(TowerMiddleware::new(StampLayer(seen)))
        .routing(|r| {
            r.get("/", |c: Call| async move {
                // Echo the inbound header the layer injected.
                c.header("x-stamped").unwrap_or("absent").to_string()
            });
        })
        .build()
}

#[tokio::test]
async fn a_layer_sees_the_request_and_stamps_the_response() {
    let seen = Arc::new(AtomicUsize::new(0));
    let res = TestClient::new(app_with_stamp(seen.clone()))
        .get("/")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.header("x-seen"), Some("1"));
    assert_eq!(
        res.text(),
        "in",
        "a header the layer set on the way in must reach the handler"
    );
}

#[tokio::test]
async fn layer_state_persists_across_requests() {
    // The whole reason the layered service is built once at install time rather
    // than per request: a stateful layer must keep its state.
    let seen = Arc::new(AtomicUsize::new(0));
    let app = app_with_stamp(seen.clone());
    let client = TestClient::new(app);
    for expected in ["1", "2", "3"] {
        let res = client.get("/").send().await;
        assert_eq!(
            res.header("x-seen"),
            Some(expected),
            "layer state was rebuilt between requests"
        );
    }
}

#[tokio::test]
async fn a_layer_can_short_circuit_without_reaching_the_handler() {
    let app = Churust::server()
        .install(TowerMiddleware::new(BlockLayer))
        .routing(|r| {
            r.get("/", |_c: Call| async { "handler ran" });
        })
        .build();
    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(res.text(), "blocked");
}

#[tokio::test]
async fn a_failing_layer_still_produces_a_response() {
    // Churust's invariant is that a response is always produced. axum gets this
    // by requiring `Error = Infallible` and taxing every fallible layer with a
    // HandleErrorLayer; here the error is absorbed into a 500 instead.
    let app = Churust::server()
        .install(TowerMiddleware::new(BoomLayer))
        .routing(|r| {
            r.get("/", |_c: Call| async { "handler ran" });
        })
        .build();
    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn an_identity_layer_is_transparent() {
    let app = Churust::server()
        .install(TowerMiddleware::new(tower_layer::Identity::new()))
        .routing(|r| {
            r.get("/", |_c: Call| async { "untouched" });
        })
        .build();
    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "untouched");
}

#[tokio::test]
async fn a_layer_can_be_placed_in_a_phase() {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = Churust::server()
        .install(TowerMiddleware::new(StampLayer(seen)).in_phase(Phase::Setup))
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.header("x-seen"), Some("1"));
}

#[tokio::test]
async fn a_buffered_response_keeps_its_content_length_through_a_layer() {
    // Rewrapping the layered body as a stream unconditionally threw away the
    // exact size of an already-buffered body, so every response through any
    // layer -- even Identity -- lost Content-Length and was chunked instead.
    // Driven over a real socket: TestClient never sees the framing.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .install(TowerMiddleware::new(tower_layer::Identity::new()))
        .routing(|r| {
            r.get("/", |_c: Call| async { "hello" });
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();

    assert!(
        text.contains("content-length: 5"),
        "a buffered body lost its Content-Length through a layer: {text}"
    );
    assert!(
        !text.contains("transfer-encoding: chunked"),
        "a known-length body should not be chunked: {text}"
    );
}

#[tokio::test]
async fn the_inner_service_is_reachable_as_a_type() {
    // `NextService` is public so an application can name it when writing a
    // layer whose bounds mention the inner service.
    let _: NextService = NextService;
}
