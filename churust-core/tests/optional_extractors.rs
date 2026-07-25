//! `Option<T>` as the spelling for an optional extractor.
//!
//! The alternative is a parallel type per extractor — `OptionalQuery`,
//! `OptionalPath`, `OptionalHeader` — which doubles the surface, doubles the
//! documentation, and is the design axum-extra deprecated after shipping it.
//! One trait that an extractor opts into keeps `Option<T>` working for
//! everything that can meaningfully be absent.
//!
//! The distinction that matters: **absent is `None`, malformed is still an
//! error.** Collapsing the two would turn a typo'd query string into a silent
//! `None` and a handler that quietly does the wrong thing.

use churust_core::{Churust, Header, HeaderName, Path, Query, TestClient};
use http::StatusCode;
use serde::Deserialize;

#[derive(Deserialize)]
struct Pager {
    page: u32,
}

struct XTrace;
impl HeaderName for XTrace {
    const NAME: &'static str = "x-trace";
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.get("/q", |q: Option<Query<Pager>>| async move {
                match q {
                    Some(Query(p)) => format!("page {}", p.page),
                    None => "no query".to_string(),
                }
            });
            r.get("/h", |h: Option<Header<String, XTrace>>| async move {
                match h {
                    Some(Header(v, _)) => format!("trace {v}"),
                    None => "no trace".to_string(),
                }
            });
            r.get("/p/{id}", |p: Option<Path<u64>>| async move {
                match p {
                    Some(Path(id)) => format!("id {id}"),
                    None => "no id".to_string(),
                }
            });
            // A present-but-optional extractor alongside a body extractor: the
            // Option wrapper must not disturb argument-position rules.
            r.post(
                "/mixed",
                |q: Option<Query<Pager>>, body: String| async move {
                    format!("{:?}/{}", q.map(|Query(p)| p.page), body)
                },
            );
        })
        .build()
}

async fn get(path: &str) -> (StatusCode, String) {
    let res = TestClient::new(app()).get(path).send().await;
    (res.status(), res.text())
}

#[tokio::test]
async fn a_present_value_extracts_as_some() {
    let (status, body) = get("/q?page=7").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "page 7");
}

#[tokio::test]
async fn an_absent_query_is_none() {
    let (status, body) = get("/q").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "no query");
}

#[tokio::test]
async fn a_malformed_query_is_still_an_error() {
    // `page` is a `u32`. Absent means None; present-and-wrong means 400. If
    // this returned "no query" the handler would silently serve page 1 for a
    // request that asked for something impossible.
    let (status, _) = get("/q?page=notanumber").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed value was swallowed as None"
    );
}

#[tokio::test]
async fn a_partial_query_is_still_an_error() {
    // A query string exists but lacks a required field: the caller tried and
    // got it wrong, which is not the same as not trying.
    let (status, _) = get("/q?other=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_absent_header_is_none() {
    let (status, body) = get("/h").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "no trace");
}

#[tokio::test]
async fn a_present_header_is_some() {
    let res = TestClient::new(app())
        .get("/h")
        .header("x-trace", "abc123")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "trace abc123");
}

#[tokio::test]
async fn a_present_path_parameter_is_some() {
    let (status, body) = get("/p/42").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "id 42");
}

#[tokio::test]
async fn an_unparseable_path_parameter_is_still_an_error() {
    // The route matched, so the parameter is present — it just does not fit
    // `u64`. Reporting None would claim the URL had no id at all.
    let (status, _) = get("/p/notanumber").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_optional_extractor_composes_with_a_body_extractor() {
    let res = TestClient::new(app())
        .post("/mixed?page=3")
        .body("hello")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "Some(3)/hello");

    let res = TestClient::new(app())
        .post("/mixed")
        .body("hello")
        .send()
        .await;
    assert_eq!(res.text(), "None/hello");
}
