//! Per-route body limits.

use churust_core::{Churust, Form, TestClient};
use http::StatusCode;
use serde::Deserialize;

#[derive(Deserialize)]
struct Note {
    text: String,
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.post("/tiny", |Form(n): Form<Note>| async move { n.text })
                .max_body_bytes(16);
            // No per-route cap: only the server-wide one applies.
            r.post("/roomy", |Form(n): Form<Note>| async move { n.text });
        })
        .build()
}

async fn post(client: &TestClient, path: &str, body: &'static str) -> http::StatusCode {
    client
        .post(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .status()
}

#[tokio::test]
async fn a_route_cap_rejects_an_oversized_body() {
    let client = TestClient::new(app());
    assert_eq!(
        post(&client, "/tiny", "text=this-is-comfortably-over-the-cap").await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn a_body_within_the_cap_is_accepted() {
    let client = TestClient::new(app());
    assert_eq!(post(&client, "/tiny", "text=ok").await, StatusCode::OK);
}

#[tokio::test]
async fn the_cap_applies_only_to_the_route_that_set_it() {
    let client = TestClient::new(app());
    assert_eq!(
        post(&client, "/roomy", "text=this-is-comfortably-over-the-cap").await,
        StatusCode::OK,
        "a sibling route must not inherit another route's cap"
    );
}

#[tokio::test]
async fn the_server_wide_cap_still_applies_underneath() {
    // The per-route knob tightens; it cannot loosen past what the engine read.
    let app = Churust::server()
        .max_body_bytes(4)
        .routing(|r| {
            r.post("/x", |Form(n): Form<Note>| async move { n.text })
                .max_body_bytes(1024);
        })
        .build();

    assert_eq!(app.config().max_body_bytes, 4);
}
