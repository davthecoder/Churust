//! `Allow` must describe the same resource whichever response carries it.
//!
//! RFC 9110 §15.5.6 requires a `405` to generate an `Allow` field listing the
//! methods the target resource supports. §9.3.7 says the same field on an
//! `OPTIONS` response describes that resource's communication options. They are
//! two views of one fact, so they must agree — and they did not: a path with a
//! single `GET` route answered `Allow: GET` to a `DELETE` while telling
//! `OPTIONS` it supported `GET, HEAD, OPTIONS`.

use churust_core::{App, Call, Churust, TestClient};
use http::{Method, StatusCode};

/// Parse an `Allow` header into a sorted, comparable set.
fn allow_set(header: &str) -> Vec<String> {
    let mut v: Vec<String> = header
        .split(',')
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

async fn allow_from_options(app: App, path: &str) -> Vec<String> {
    let res = TestClient::new(app)
        .request(Method::OPTIONS, path)
        .send()
        .await;
    allow_set(res.header("allow").unwrap_or_default())
}

async fn allow_from_405(app: App, path: &str, method: Method) -> (StatusCode, Vec<String>) {
    let res = TestClient::new(app).request(method, path).send().await;
    (
        res.status(),
        allow_set(res.header("allow").unwrap_or_default()),
    )
}

/// A named way to build an app, so one assertion can sweep many route shapes.
type Shape = (&'static str, fn() -> App);

/// Several route shapes, because the defect was precisely that two code paths
/// drift — a single shape would not have caught it.
fn shapes() -> Vec<Shape> {
    vec![
        ("get only", || {
            Churust::server()
                .routing(|r| {
                    r.get("/x", |_c: Call| async { "g" });
                })
                .build()
        }),
        ("get and post", || {
            Churust::server()
                .routing(|r| {
                    r.get("/x", |_c: Call| async { "g" });
                    r.post("/x", |_c: Call| async { "p" });
                })
                .build()
        }),
        ("post only, no get", || {
            Churust::server()
                .routing(|r| {
                    r.post("/x", |_c: Call| async { "p" });
                })
                .build()
        }),
        ("explicit head alongside get", || {
            Churust::server()
                .routing(|r| {
                    r.get("/x", |_c: Call| async { "g" });
                    r.method(Method::HEAD, "/x", |_c: Call| async {
                        StatusCode::NO_CONTENT
                    });
                })
                .build()
        }),
        ("put and delete", || {
            Churust::server()
                .routing(|r| {
                    r.put("/x", |_c: Call| async { "u" });
                    r.delete("/x", |_c: Call| async { "d" });
                })
                .build()
        }),
    ]
}

#[tokio::test]
async fn the_405_allow_matches_the_options_allow() {
    for (name, build) in shapes() {
        let from_options = allow_from_options(build(), "/x").await;
        // PATCH is registered in none of the shapes, so it always yields 405.
        let (status, from_405) = allow_from_405(build(), "/x", Method::PATCH).await;

        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "[{name}]");
        assert_eq!(
            from_405, from_options,
            "[{name}] 405 said {from_405:?} but OPTIONS said {from_options:?}"
        );
        assert!(
            !from_405.is_empty(),
            "[{name}] an empty Allow tells a client nothing"
        );
    }
}

#[tokio::test]
async fn allow_advertises_only_methods_that_are_really_served() {
    // The header is a promise. Every method it names must not answer 405.
    for (name, build) in shapes() {
        for method in allow_from_options(build(), "/x").await {
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            let res = TestClient::new(build())
                .request(m.clone(), "/x")
                .send()
                .await;
            assert_ne!(
                res.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "[{name}] Allow advertised {method}, which then returned 405"
            );
        }
    }
}

#[tokio::test]
async fn head_is_advertised_wherever_get_is() {
    let allow = allow_from_options(shapes()[0].1(), "/x").await;
    assert!(allow.contains(&"HEAD".to_string()), "{allow:?}");
    assert!(allow.contains(&"OPTIONS".to_string()), "{allow:?}");
}

#[tokio::test]
async fn head_is_not_advertised_where_there_is_no_get() {
    // "post only": synthesizing HEAD from a GET that does not exist would be a
    // promise the server cannot keep.
    let allow = allow_from_options(shapes()[2].1(), "/x").await;
    assert!(!allow.contains(&"HEAD".to_string()), "{allow:?}");
}

#[tokio::test]
async fn an_unknown_path_is_404_not_405() {
    let (status, _) = allow_from_405(shapes()[0].1(), "/nowhere", Method::PATCH).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_installed_error_renderer_does_not_drop_the_allow_header() {
    // `on_error` replaced the whole response, discarding headers the pipeline
    // had already attached — so installing a custom error page silently made
    // every 405 non-conforming (RFC 9110 §15.5.6 requires `Allow`).
    let app = Churust::server()
        .on_error(|status, _call| {
            Some(churust_core::Response::text("custom error").with_status(status))
        })
        .routing(|r| {
            r.get("/x", |_c: Call| async { "g" });
        })
        .build();

    let res = TestClient::new(app)
        .request(Method::PATCH, "/x")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(res.text(), "custom error", "the renderer should still win");
    let allow = allow_set(res.header("allow").unwrap_or_default());
    assert!(
        allow.contains(&"GET".to_string()),
        "Allow was dropped by the error renderer: {allow:?}"
    );
}
