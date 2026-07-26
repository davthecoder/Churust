//! `Json<T>` must require a JSON content type.
//!
//! Without the check, a cross-origin HTML form with
//! `enctype="text/plain"` posting a JSON-shaped body is a CORS *simple*
//! request: no preflight is sent, the browser attaches the victim's cookies,
//! and the handler deserialises it happily. Every state-changing JSON endpoint
//! is then executable from any site the victim visits, with the CORS layer
//! never consulted. `Form<T>` and `Multipart` already return `415` for a
//! mismatched type; only the JSON path did not.

use churust_core::{Churust, TestClient};
use churust_json::Json;
use http::StatusCode;
use serde::Deserialize;

#[derive(Deserialize)]
struct Transfer {
    amount: u64,
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.post("/transfer", |Json(t): Json<Transfer>| async move {
                format!("moved {}", t.amount)
            });
        })
        .build()
}

async fn post_with(content_type: Option<&str>, body: &'static str) -> (StatusCode, String) {
    let client = TestClient::new(app());
    let mut req = client.post("/transfer");
    if let Some(ct) = content_type {
        req = req.header("content-type", ct);
    }
    let res = req.body(body).send().await;
    (res.status(), res.text())
}

#[tokio::test]
async fn a_cross_origin_text_plain_form_post_is_refused() {
    // The attack shape: `enctype="text/plain"` sends no preflight.
    let (status, body) = post_with(Some("text/plain"), r#"{"amount":1000}"#).await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "a text/plain body was deserialised as JSON: {body}"
    );
}

#[tokio::test]
async fn the_other_two_simple_request_types_are_refused_as_well() {
    // The complete set a form can send without a preflight.
    for ct in [
        "application/x-www-form-urlencoded",
        "multipart/form-data; boundary=x",
    ] {
        let (status, _) = post_with(Some(ct), r#"{"amount":1000}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "accepted {ct}");
    }
}

#[tokio::test]
async fn application_json_is_accepted() {
    let (status, body) = post_with(Some("application/json"), r#"{"amount":7}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "moved 7");
}

#[tokio::test]
async fn a_charset_parameter_is_fine() {
    // Only the media type is compared; parameters are legitimate.
    let (status, body) =
        post_with(Some("application/json; charset=utf-8"), r#"{"amount":7}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "moved 7");
}

#[tokio::test]
async fn a_structured_suffix_is_accepted() {
    // `+json` types are JSON by definition — refusing them would break
    // legitimate APIs that use them.
    for ct in [
        "application/problem+json",
        "application/vnd.api+json",
        "application/merge-patch+json",
    ] {
        let (status, body) = post_with(Some(ct), r#"{"amount":7}"#).await;
        assert_eq!(status, StatusCode::OK, "rejected {ct}: {body}");
    }
}

#[tokio::test]
async fn a_missing_content_type_is_refused() {
    // A body with no declared type is not a JSON body.
    let (status, _) = post_with(None, r#"{"amount":1000}"#).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn malformed_json_is_still_a_400_not_a_415() {
    // The type was right; the content was not. The distinction matters to a
    // client trying to work out what it did wrong.
    let (status, _) = post_with(Some("application/json"), "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
