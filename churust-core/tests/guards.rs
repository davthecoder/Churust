//! Route guards: predicates that select among routes sharing a method and path.

use churust_core::{guard, Call, Churust, TestClient};
use http::StatusCode;

#[tokio::test]
async fn a_header_guard_selects_between_two_handlers_on_one_path() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/x", |_c: Call| async { "beta" })
                .guard(guard::header("x-beta", "1"));
            r.get("/x", |_c: Call| async { "stable" });
        })
        .build();
    let client = TestClient::new(app);

    let beta = client.get("/x").header("x-beta", "1").send().await;
    assert_eq!(beta.text(), "beta");

    let stable = client.get("/x").send().await;
    assert_eq!(
        stable.text(),
        "stable",
        "a request failing the guard must fall through to the unguarded route"
    );
}

#[tokio::test]
async fn a_guarded_route_with_no_fallback_is_404() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/only", |_c: Call| async { "yes" })
                .guard(guard::header("x-key", "secret"));
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(
        client.get("/only").send().await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .get("/only")
            .header("x-key", "secret")
            .send()
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn host_guard() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "admin site" })
                .guard(guard::host("admin.example.com"));
            r.get("/", |_c: Call| async { "public site" });
        })
        .build();
    let client = TestClient::new(app);

    let admin = client
        .get("/")
        .header("host", "admin.example.com")
        .send()
        .await;
    assert_eq!(admin.text(), "admin site");

    let public = client
        .get("/")
        .header("host", "www.example.com")
        .send()
        .await;
    assert_eq!(public.text(), "public site");
}

#[tokio::test]
async fn combinators_compose() {
    let app = Churust::server()
        .routing(|r| {
            // Either header will do.
            r.get("/any", |_c: Call| async { "hit" })
                .guard(guard::any(vec![
                    guard::header("x-a", "1"),
                    guard::header("x-b", "1"),
                ]));

            // Both are required.
            r.get("/all", |_c: Call| async { "hit" })
                .guard(guard::all(vec![
                    guard::header("x-a", "1"),
                    guard::header("x-b", "1"),
                ]));

            // Inverted.
            r.get("/not", |_c: Call| async { "hit" })
                .guard(guard::not(guard::header("x-a", "1")));
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(
        client.get("/any").header("x-b", "1").send().await.status(),
        StatusCode::OK
    );
    assert_eq!(
        client.get("/any").send().await.status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        client.get("/all").header("x-a", "1").send().await.status(),
        StatusCode::NOT_FOUND,
        "all() needs every guard to pass"
    );
    assert_eq!(
        client
            .get("/all")
            .header("x-a", "1")
            .header("x-b", "1")
            .send()
            .await
            .status(),
        StatusCode::OK
    );

    assert_eq!(client.get("/not").send().await.status(), StatusCode::OK);
    assert_eq!(
        client.get("/not").header("x-a", "1").send().await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_custom_predicate_works() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/even", |_c: Call| async { "even" })
                .guard(guard::fn_guard(|c: &Call| {
                    c.query("n")
                        .and_then(|v| v.parse::<u32>().ok())
                        .is_some_and(|n| n % 2 == 0)
                }));
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(
        client.get("/even?n=4").send().await.status(),
        StatusCode::OK
    );
    assert_eq!(
        client.get("/even?n=5").send().await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn guards_do_not_break_the_duplicate_route_check() {
    // Two routes on the same (method, path) are legal *because* they are
    // distinguished by guards. Only a second unguarded one is a duplicate.
    let result = std::panic::catch_unwind(|| {
        Churust::server().routing(|r| {
            r.get("/x", |_c: Call| async { "a" });
            r.get("/x", |_c: Call| async { "b" });
        });
    });
    assert!(
        result.is_err(),
        "two unguarded routes are still a duplicate"
    );
}
