//! Security response headers, applied by default.

use churust_core::{Call, Churust, Response, SecurityHeaders, TestClient};
use http::StatusCode;

#[tokio::test]
async fn defaults_are_applied_without_being_asked_for() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(res.header("x-frame-options"), Some("DENY"));
    assert_eq!(res.header("referrer-policy"), Some("no-referrer"));
    assert!(
        res.header("permissions-policy")
            .is_some_and(|v| v.contains("camera=()")),
        "default Permissions-Policy should lock high-risk browser features"
    );
    assert_eq!(
        res.header("cross-origin-resource-policy"),
        Some("same-origin")
    );
}

#[tokio::test]
async fn hsts_is_absent_without_tls() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert!(
        res.header("strict-transport-security").is_none(),
        "HSTS over plaintext is meaningless and harmful behind a terminating proxy"
    );
}

#[tokio::test]
async fn a_handler_that_sets_its_own_header_wins() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async {
                Response::text("framed").with_header(
                    http::header::X_FRAME_OPTIONS,
                    http::HeaderValue::from_static("SAMEORIGIN"),
                )
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(
        res.header("x-frame-options"),
        Some("SAMEORIGIN"),
        "the application must be able to override a default"
    );
    // The others are still filled in.
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn headers_can_be_turned_off_entirely() {
    let app = Churust::server()
        .without_security_headers()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert!(res.header("x-content-type-options").is_none());
    assert!(res.header("x-frame-options").is_none());
    assert!(res.header("referrer-policy").is_none());
    assert!(res.header("permissions-policy").is_none());
    assert!(res.header("cross-origin-resource-policy").is_none());
}

#[tokio::test]
async fn individual_headers_can_be_disabled() {
    let app = Churust::server()
        .security_headers(SecurityHeaders::new().frame_options(None))
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert!(res.header("x-frame-options").is_none());
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn values_can_be_customised() {
    let app = Churust::server()
        .security_headers(
            SecurityHeaders::new()
                .referrer_policy(Some("strict-origin-when-cross-origin"))
                .content_security_policy(Some("default-src 'self'")),
        )
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(
        res.header("referrer-policy"),
        Some("strict-origin-when-cross-origin")
    );
    assert_eq!(
        res.header("content-security-policy"),
        Some("default-src 'self'"),
        "CSP is off by default but must be settable"
    );
}

#[tokio::test]
async fn no_csp_by_default() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert!(
        res.header("content-security-policy").is_none(),
        "a default CSP either breaks apps or is so permissive it misleads"
    );
}

#[tokio::test]
async fn error_responses_are_also_protected() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "hi" });
        })
        .build();

    // A 404 is still a response an attacker can reach.
    let res = TestClient::new(app).get("/missing").send().await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
}
