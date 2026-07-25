//! A deeper wildcard must win, and a parameter name must mean one thing.
//!
//! Two routing defects this pins:
//!
//! 1. `walk_wildcard` consulted a node's own `{name...}` *before* descending,
//!    so a shallow wildcard shadowed every deeper one. With `/files/{p...}` and
//!    `/files/img/{q...}` registered, nothing under `/files/img/` could reach
//!    the second handler — silently, with no warning at registration.
//! 2. `Router::add` created the parameter node with `get_or_insert_with`, so a
//!    second route using a different `{name}` at the same position inherited
//!    the first route's name and the handler looked up a key that was never
//!    captured.

use churust_core::{Call, Churust, TestClient};
use http::StatusCode;

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.get("/files/{p...}", |c: Call| async move {
                format!("shallow:{}", c.param_raw("p").unwrap_or_default())
            });
            r.get("/files/img/{q...}", |c: Call| async move {
                format!("deep:{}", c.param_raw("q").unwrap_or_default())
            });
        })
        .build()
}

#[tokio::test]
async fn the_deepest_wildcard_wins() {
    let res = TestClient::new(app())
        .get("/files/img/logo.png")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.text(),
        "deep:logo.png",
        "the shallower /files/{{p...}} shadowed the more specific route"
    );
}

#[tokio::test]
async fn the_deeper_wildcard_captures_only_its_own_remainder() {
    let res = TestClient::new(app())
        .get("/files/img/icons/a.svg")
        .send()
        .await;
    assert_eq!(res.text(), "deep:icons/a.svg");
}

#[tokio::test]
async fn the_shallow_wildcard_still_serves_everything_else() {
    let client = TestClient::new(app());
    assert_eq!(
        client.get("/files/a.txt").send().await.text(),
        "shallow:a.txt"
    );
    assert_eq!(
        client.get("/files/docs/a.txt").send().await.text(),
        "shallow:docs/a.txt"
    );
}

#[tokio::test]
async fn a_deeper_method_mismatch_does_not_preempt_a_shallower_handler() {
    // The deep route has no POST; the shallow one does. A 405 found deeper must
    // not win over a real handler found shallower.
    let app = Churust::server()
        .routing(|r| {
            r.post("/files/{p...}", |_c: Call| async { "shallow-post" });
            r.get("/files/img/{q...}", |_c: Call| async { "deep-get" });
        })
        .build();
    let res = TestClient::new(app).post("/files/img/x.png").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "shallow-post");
}

#[tokio::test]
async fn allow_unions_both_depths_when_neither_serves_the_method() {
    // Neither depth has DELETE, so the 405 must describe every method either
    // would have served — the header speaks for the resource, not for one node.
    let app = Churust::server()
        .routing(|r| {
            r.post("/files/{p...}", |_c: Call| async { "p" });
            r.get("/files/img/{q...}", |_c: Call| async { "g" });
        })
        .build();
    let res = TestClient::new(app).delete("/files/img/x.png").send().await;
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    let allow = res.header("allow").unwrap_or_default().to_ascii_uppercase();
    assert!(allow.contains("GET"), "{allow}");
    assert!(allow.contains("POST"), "{allow}");
}

#[tokio::test]
async fn a_wildcard_route_still_works_on_its_own() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/s/{path...}", |c: Call| async move {
                c.param_raw("path").unwrap_or_default().to_string()
            });
        })
        .build();
    assert_eq!(
        TestClient::new(app).get("/s/a/b/c.txt").send().await.text(),
        "a/b/c.txt"
    );
}

#[test]
#[should_panic(expected = "conflicting path parameter names")]
fn conflicting_parameter_names_fail_at_registration() {
    // Silently reusing the first name meant the second handler looked up a key
    // that was never captured, producing a 400 at request time with no clue.
    let _ = Churust::server().routing(|r| {
        r.get("/users/{id}", |_c: Call| async { "a" });
        r.get("/users/{name}/profile", |_c: Call| async { "b" });
    });
}

#[tokio::test]
async fn the_same_parameter_name_at_the_same_position_is_fine() {
    // Sharing a name is the normal case and must keep working.
    let app = Churust::server()
        .routing(|r| {
            r.get("/users/{id}", |c: Call| async move {
                format!("one:{}", c.param_raw("id").unwrap_or_default())
            });
            r.get("/users/{id}/profile", |c: Call| async move {
                format!("two:{}", c.param_raw("id").unwrap_or_default())
            });
        })
        .build();
    let client = TestClient::new(app);
    assert_eq!(client.get("/users/7").send().await.text(), "one:7");
    assert_eq!(client.get("/users/7/profile").send().await.text(), "two:7");
}
