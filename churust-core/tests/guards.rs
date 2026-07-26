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

#[tokio::test]
async fn host_guards_match_the_http2_authority_too() {
    // HTTP/2 removed the `Host` header in favour of the `:authority`
    // pseudo-header, which lands in the request URI. A host guard that reads
    // only the header silently stopped matching over h2 — and since ALPN
    // prefers h2, traffic fell through to whatever unguarded sibling was
    // registered as the fallback, i.e. the wrong vhost's content.
    use churust_core::{Call, Churust, TestClient};

    let build = || {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "ADMIN" })
                    .guard(churust_core::guard::host("admin.example.com"));
                r.get("/", |_c: Call| async { "PUBLIC" });
            })
            .build()
    };

    // HTTP/1.1 shape: host in the header.
    let res = TestClient::new(build())
        .get("/")
        .header("host", "admin.example.com")
        .send()
        .await;
    assert_eq!(res.text(), "ADMIN");

    // HTTP/2 shape: no Host header, authority in the URI.
    let res = TestClient::new(build())
        .get("https://admin.example.com/")
        .send()
        .await;
    assert_eq!(
        res.text(),
        "ADMIN",
        "the guard ignored the :authority and fell through to the public route"
    );

    // A different authority must still miss.
    let res = TestClient::new(build())
        .get("https://www.example.com/")
        .send()
        .await;
    assert_eq!(res.text(), "PUBLIC");
}

#[tokio::test]
async fn a_host_guard_follows_the_authority_when_a_host_header_disagrees() {
    // Both spellings can arrive on one request. Over HTTP/2 the `host` field is
    // not a forbidden one — h2 passes it through and hyper derives the URI
    // authority from `:authority` alone — so a peer can send
    // `:authority: www.example.com` beside `host: admin.example.com`. Over
    // HTTP/1.1 an absolute-form target does the same, since `Host` is still
    // mandatory there. RFC 9113 §8.3.1 and RFC 9112 §3.2.2 both settle it the
    // same way: the authority *is* the target and the header is to be ignored.
    //
    // The guard used to read the header first, so an intermediary that routed
    // or authorized on the authority and the origin behind it could disagree
    // about which vhost was being addressed — the two ends resolving one
    // request to two different sites is the whole hazard, even though sending
    // the admin authority outright already reaches the same route.
    use churust_core::{Call, Churust, TestClient};

    let build = || {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "ADMIN" })
                    .guard(churust_core::guard::host("admin.example.com"));
                r.get("/", |_c: Call| async { "PUBLIC" });
            })
            .build()
    };

    let res = TestClient::new(build())
        .get("https://www.example.com/")
        .header("host", "admin.example.com")
        .send()
        .await;
    assert_eq!(
        res.text(),
        "PUBLIC",
        "the stray Host header outranked the authority the request was routed on"
    );

    // And the converse: an authority naming the guarded vhost matches however
    // the header disagrees, so the header cannot steer the route either way.
    let res = TestClient::new(build())
        .get("https://admin.example.com/")
        .header("host", "www.example.com")
        .send()
        .await;
    assert_eq!(res.text(), "ADMIN");
}
