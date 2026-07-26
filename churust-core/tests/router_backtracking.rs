//! Matching must not stop at the first structurally-matching leaf.
//!
//! `walk` returned the first node whose *shape* matched and never reconsidered,
//! so a second registered route that could actually serve the request was
//! unreachable. Static beats param at each step, which is right — but only as a
//! preference, not as a commitment.

use churust_core::{Call, Churust, TestClient};
use http::{Method, StatusCode};

#[tokio::test]
async fn a_method_mismatch_on_one_route_falls_through_to_another() {
    // `/a/b` matches both shapes. Only the second has a GET.
    let app = Churust::server()
        .routing(|r| {
            r.post("/a/{x}", |_c: Call| async { "post-ax" });
            r.get("/{y}/b", |_c: Call| async { "get-yb" });
        })
        .build();
    let res = TestClient::new(app).get("/a/b").send().await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "matching committed to /a/{{x}} and answered 405"
    );
    assert_eq!(res.text(), "get-yb");
}

#[tokio::test]
async fn registration_order_does_not_decide_it() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/{y}/b", |_c: Call| async { "get-yb" });
            r.post("/a/{x}", |_c: Call| async { "post-ax" });
        })
        .build();
    let res = TestClient::new(app).get("/a/b").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "get-yb");
}

#[tokio::test]
async fn a_failing_guard_falls_through_to_another_route() {
    // A guard on one route must not disable an unrelated one that matches.
    let app = Churust::server()
        .routing(|r| {
            r.get("/a/{x}", |_c: Call| async { "beta" })
                .guard(churust_core::guard::header("x-beta", "1"));
            r.get("/{y}/b", |_c: Call| async { "stable" });
        })
        .build();
    let client = TestClient::new(app);

    let res = client.get("/a/b").send().await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "a guard on /a/{{x}} hid /{{y}}/b entirely"
    );
    assert_eq!(res.text(), "stable");
}

#[tokio::test]
async fn the_guarded_route_still_wins_when_its_guard_passes() {
    // Backtracking must not cost the preference: static beats param when the
    // more specific route can actually serve.
    let app = Churust::server()
        .routing(|r| {
            r.get("/a/{x}", |_c: Call| async { "beta" })
                .guard(churust_core::guard::header("x-beta", "1"));
            r.get("/{y}/b", |_c: Call| async { "stable" });
        })
        .build();
    let res = TestClient::new(app)
        .get("/a/b")
        .header("x-beta", "1")
        .send()
        .await;
    assert_eq!(res.text(), "beta");
}

#[tokio::test]
async fn static_still_beats_param_when_both_can_serve() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/a/b", |_c: Call| async { "static" });
            r.get("/{y}/b", |_c: Call| async { "param" });
        })
        .build();
    assert_eq!(
        TestClient::new(app).get("/a/b").send().await.text(),
        "static"
    );
}

#[tokio::test]
async fn the_captured_parameters_belong_to_the_route_that_served() {
    // Backtracking abandons a branch; its captures must not leak into the one
    // that actually matched.
    let app = Churust::server()
        .routing(|r| {
            r.post("/a/{x}", |_c: Call| async { "post" });
            r.get("/{y}/b", |c: Call| async move {
                format!(
                    "y={:?} x={:?}",
                    c.param_raw("y").unwrap_or("-"),
                    c.param_raw("x").unwrap_or("-")
                )
            });
        })
        .build();
    let res = TestClient::new(app).get("/a/b").send().await;
    assert_eq!(res.text(), r#"y="a" x="-""#);
}

#[tokio::test]
async fn a_genuine_method_mismatch_is_still_405_listing_every_depth() {
    // Nothing serves DELETE, so the Allow header must describe every route that
    // structurally matched, not just the first one found.
    let app = Churust::server()
        .routing(|r| {
            r.post("/a/{x}", |_c: Call| async { "post" });
            r.get("/{y}/b", |_c: Call| async { "get" });
        })
        .build();
    let res = TestClient::new(app)
        .request(Method::DELETE, "/a/b")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    let allow = res.header("allow").unwrap_or_default().to_ascii_uppercase();
    assert!(allow.contains("POST"), "{allow}");
    assert!(allow.contains("GET"), "{allow}");
}

#[tokio::test]
async fn an_unmatched_path_is_still_404() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/a/{x}", |_c: Call| async { "a" });
        })
        .build();
    let res = TestClient::new(app).get("/nope/deep/er").send().await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
