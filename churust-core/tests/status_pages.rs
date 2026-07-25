//! `on_error` — v1 design §5.3's "StatusPages-lite" hook.

use churust_core::{Call, Churust, Response, TestClient};
use http::StatusCode;

#[tokio::test]
async fn a_hook_can_replace_the_default_404_body() {
    let app = Churust::server()
        .on_error(|status, call| {
            (status == StatusCode::NOT_FOUND)
                .then(|| Response::text(format!("no {} here", call.path())).with_status(status))
        })
        .routing(|r| {
            r.get("/", |_c: Call| async { "home" });
        })
        .build();
    let client = TestClient::new(app);

    let missing = client.get("/nope").send().await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing.text(), "no /nope here");

    // Success is untouched.
    assert_eq!(client.get("/").send().await.text(), "home");
}

#[tokio::test]
async fn returning_none_keeps_the_default_rendering() {
    let app = Churust::server()
        .on_error(|status, _call| {
            // Only take over 500s; a 404 falls through to the default.
            (status == StatusCode::INTERNAL_SERVER_ERROR)
                .then(|| Response::text("our fault").with_status(status))
        })
        .routing(|r| {
            r.get("/", |_c: Call| async { "home" });
        })
        .build();

    let res = TestClient::new(app).get("/nope").send().await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.text(), "Not Found", "the default body should remain");
}

#[tokio::test]
async fn the_hook_sees_errors_returned_by_handlers() {
    let app = Churust::server()
        .on_error(|status, _call| {
            Some(Response::text(format!("handled {status}")).with_status(status))
        })
        .routing(|r| {
            r.get("/boom", |_c: Call| async {
                Err::<&str, _>(churust_core::Error::bad_request("nope"))
            });
        })
        .build();

    let res = TestClient::new(app).get("/boom").send().await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(res.text(), "handled 400 Bad Request");
}

#[tokio::test]
async fn security_headers_still_apply_to_a_custom_error_page() {
    let app = Churust::server()
        .on_error(|status, _call| Some(Response::text("custom").with_status(status)))
        .routing(|r| {
            r.get("/", |_c: Call| async { "home" });
        })
        .build();

    let res = TestClient::new(app).get("/nope").send().await;
    assert_eq!(res.text(), "custom");
    assert_eq!(
        res.header("x-content-type-options"),
        Some("nosniff"),
        "a replaced error page is still a response an attacker can reach"
    );
}
