//! Repeated query and form keys.
//!
//! `?tag=a&tag=b` and a checkbox group posting `opt=email&opt=sms` are ordinary
//! HTML, not an edge case: a repeated `<input type="checkbox">` and a
//! `<select multiple>` produce exactly this. Both previously failed with `400
//! invalid type: string "a", expected a sequence`, blaming the user for the
//! framework's parser choice, with no workaround inside the extractor.

use churust_core::{Churust, Form, Query, TestClient};
use http::StatusCode;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct Tags {
    tag: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct Ids {
    id: Vec<u32>,
}

#[derive(Deserialize, Debug)]
struct One {
    q: String,
}

#[derive(Deserialize, Debug)]
struct Mixed {
    page: u32,
    tag: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct Optional {
    tag: Option<Vec<String>>,
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.get("/tags", |Query(t): Query<Tags>| async move {
                format!("{:?}", t.tag)
            });
            r.get("/ids", |Query(i): Query<Ids>| async move {
                format!("{:?}", i.id)
            });
            r.get("/one", |Query(o): Query<One>| async move { o.q });
            r.get("/mixed", |Query(m): Query<Mixed>| async move {
                format!("{}:{:?}", m.page, m.tag)
            });
            r.get("/optional", |Query(o): Query<Optional>| async move {
                format!("{:?}", o.tag)
            });
            r.post("/prefs", |Form(t): Form<Tags>| async move {
                format!("{:?}", t.tag)
            });
            r.post("/one", |Form(o): Form<One>| async move { o.q });
        })
        .build()
}

async fn get(path: &str) -> (StatusCode, String) {
    let res = TestClient::new(app()).get(path).send().await;
    (res.status(), res.text())
}

async fn post_form(path: &str, body: &'static str) -> (StatusCode, String) {
    let res = TestClient::new(app())
        .post(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await;
    (res.status(), res.text())
}

#[tokio::test]
async fn a_repeated_query_key_fills_a_vec() {
    let (status, body) = get("/tags?tag=a&tag=b&tag=c").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, r#"["a", "b", "c"]"#);
}

#[tokio::test]
async fn repeated_keys_deserialize_to_typed_elements() {
    let (status, body) = get("/ids?id=1&id=2&id=42").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "[1, 2, 42]");
}

#[tokio::test]
async fn a_single_value_still_fills_a_vec() {
    // A one-element list is the common case for a checkbox group where the user
    // ticked one box. It must not need a different code path.
    let (status, body) = get("/tags?tag=solo").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, r#"["solo"]"#);
}

#[tokio::test]
async fn scalars_and_lists_coexist_in_one_struct() {
    let (status, body) = get("/mixed?page=2&tag=x&tag=y").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, r#"2:["x", "y"]"#);
}

#[tokio::test]
async fn an_absent_list_is_none_not_an_error() {
    // Nothing ticked means the browser sends the key not at all.
    let (status, body) = get("/optional").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "None");
}

#[tokio::test]
async fn a_repeated_scalar_is_rejected_rather_than_resolved() {
    // Documented policy, and deliberately *not* last-wins.
    //
    // `?q=first&q=last` against a `String` field is ambiguous: browsers take
    // the last occurrence, some servers take the first. Picking a winner
    // silently means a proxy and an origin can disagree about what the request
    // said. Refusing is the same call made for `Transfer-Encoding` +
    // `Content-Length`: where two readings are possible, do not guess.
    //
    // Declare the field as `Vec<String>` to accept repetition on purpose.
    let (status, body) = get("/one?q=first&q=last").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_checkbox_group_posts_into_a_vec() {
    let (status, body) = post_form("/prefs", "opt=email&opt=sms").await;
    // The field is `tag`, so this is the wrong-name case: a clear 400.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = post_form("/prefs", "tag=email&tag=sms").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, r#"["email", "sms"]"#);
}

#[tokio::test]
async fn a_repeated_form_scalar_is_rejected_too() {
    // Same rule on the body side; the two extractors must not disagree.
    let (status, body) = post_form("/one", "q=first&q=last").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn percent_encoding_still_decodes() {
    // The parser swap must not regress ordinary decoding.
    let (status, body) = get("/one?q=%26%3D%20x").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "&= x");
}

#[tokio::test]
async fn plus_still_means_space_in_a_form_body() {
    let (status, body) = post_form("/one", "q=hello+world").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "hello world");
}

#[tokio::test]
async fn a_missing_required_field_is_still_a_400() {
    let (status, _) = get("/one").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_wrongly_typed_value_is_still_a_400() {
    let (status, _) = get("/ids?id=notanumber").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
