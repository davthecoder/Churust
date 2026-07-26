//! `Payload` must respect the per-route body cap.
//!
//! Buffering extractors check the route cap before allocating. `Payload` hands
//! the stream straight to the handler, so a route that tightened its limit was
//! not actually tightened — and a handler that collects the stream allocates up
//! to the *server-wide* ceiling, which an operator may have raised for one
//! legitimate upload route. Every `Payload` route then inherits that ceiling.

use churust_core::{Churust, Payload, Result, TestClient};
use futures_util::StreamExt;
use http::StatusCode;

fn app() -> churust_core::App {
    Churust::server()
        // Deliberately generous, standing in for "raised for one upload route".
        .max_body_bytes(1 << 20)
        .routing(|r| {
            r.post("/capped", |Payload(mut body): Payload| async move {
                let mut n = 0usize;
                while let Some(chunk) = body.next().await {
                    // The cap surfaces as a stream error, because the response
                    // may already have begun by the time it trips. `?` turns it
                    // into the right status.
                    n += chunk?.len();
                }
                Result::Ok(format!("{n} bytes"))
            })
            .max_body_bytes(16);

            r.post("/uncapped", |Payload(mut body): Payload| async move {
                let mut n = 0usize;
                while let Some(Ok(c)) = body.next().await {
                    n += c.len();
                }
                format!("{n} bytes")
            });
        })
        .build()
}

async fn post(path: &str, body: &'static str) -> (StatusCode, String) {
    let res = TestClient::new(app()).post(path).body(body).send().await;
    (res.status(), res.text())
}

#[tokio::test]
async fn a_body_within_the_route_cap_streams_through() {
    let (status, body) = post("/capped", "small").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "5 bytes");
}

#[tokio::test]
async fn a_body_over_the_route_cap_is_rejected() {
    let (status, _) = post("/capped", "this is comfortably over sixteen bytes").await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "the route cap did not apply to the stream"
    );
}

#[tokio::test]
async fn a_sibling_route_does_not_inherit_the_cap() {
    let (status, body) = post("/uncapped", "this is comfortably over sixteen bytes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "38 bytes");
}

#[tokio::test]
async fn a_body_exactly_at_the_cap_is_allowed() {
    // Off-by-one matters: the cap is a maximum, not a strict bound.
    let (status, body) = post("/capped", "0123456789abcdef").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "16 bytes");
}
