//! `Path<T>` destructuring — single, tuple, and struct.

use churust_core::{Churust, Path, TestClient};
use http::StatusCode;
use serde::Deserialize;

#[derive(Deserialize)]
struct Route {
    user: u64,
    post: String,
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            // Single, positional — the pre-existing shape, which must not break.
            r.get(
                "/one/{id}",
                |Path(id): Path<u64>| async move { format!("{id}") },
            );

            // Tuple, in capture order.
            r.get(
                "/two/{user}/posts/{post}",
                |Path((user, post)): Path<(u64, String)>| async move { format!("{user}/{post}") },
            );

            // Struct, by name.
            r.get(
                "/named/{user}/posts/{post}",
                |Path(p): Path<Route>| async move { format!("{}:{}", p.user, p.post) },
            );

            // Optional.
            r.get("/opt/{id}", |Path(id): Path<Option<u64>>| async move {
                format!("{id:?}")
            });
        })
        .build()
}

#[tokio::test]
async fn a_single_parameter_still_works() {
    let res = TestClient::new(app()).get("/one/42").send().await;
    assert_eq!(res.text(), "42");
}

#[tokio::test]
async fn a_tuple_destructures_in_capture_order() {
    let res = TestClient::new(app())
        .get("/two/7/posts/hello")
        .send()
        .await;
    assert_eq!(res.text(), "7/hello");
}

#[tokio::test]
async fn a_struct_destructures_by_name() {
    let res = TestClient::new(app())
        .get("/named/9/posts/world")
        .send()
        .await;
    assert_eq!(res.text(), "9:world");
}

#[tokio::test]
async fn an_optional_parameter_is_supported() {
    let res = TestClient::new(app()).get("/opt/5").send().await;
    assert_eq!(res.text(), "Some(5)");
}

#[tokio::test]
async fn a_value_that_does_not_parse_is_400() {
    let res = TestClient::new(app()).get("/one/not-a-number").send().await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_tuple_wider_than_the_route_is_400() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/only/{a}", |Path((a, b)): Path<(u64, u64)>| async move {
                format!("{a}{b}")
            });
        })
        .build();

    let res = TestClient::new(app).get("/only/1").send().await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "asking for two parameters from a one-parameter route must fail loudly"
    );
}

#[tokio::test]
async fn percent_decoded_values_reach_the_extractor() {
    let res = TestClient::new(app())
        .get("/two/3/posts/hello%20world")
        .send()
        .await;
    assert_eq!(res.text(), "3/hello world");
}
