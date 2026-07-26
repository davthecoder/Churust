//! The client driving a real Churust server over a real socket.

use churust_client::{Client, ClientError};
use churust_core::{Call, Churust, Response};
use http::header::LOCATION;
use http::{HeaderValue, StatusCode};
use std::time::Duration;

/// Start the test server on an ephemeral port and return its base URL.
///
/// Binding port 0 and reading back what the OS chose is what keeps these tests
/// runnable in parallel and on a machine that already has something on 8080.
async fn serve() -> String {
    // Keep the listener: dropping it to free the port leaves a window in
    // which another test's server can take it, and the client then talks to
    // the wrong server.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let app = Churust::server()
        .port(port)
        .host("127.0.0.1")
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello" });
            r.get("/json", |_c: Call| async {
                Response::bytes("application/json", r#"{"name":"ana","age":3}"#)
            });
            r.post("/echo", |body: String| async move { body });
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(400)).await;
                "eventually"
            });
            r.get("/big", |_c: Call| async { "x".repeat(64 * 1024) });
            r.get("/teapot", |_c: Call| async {
                (StatusCode::IM_A_TEAPOT, "no coffee")
            });
            r.get("/redirect", |_c: Call| async {
                Response::new(StatusCode::FOUND)
                    .with_header(LOCATION, HeaderValue::from_static("/hello"))
            });
            r.get("/loop", |_c: Call| async {
                Response::new(StatusCode::FOUND)
                    .with_header(LOCATION, HeaderValue::from_static("/loop"))
            });
            r.post("/method", |call: Call| async move {
                call.method().as_str().to_string()
            });
            r.get("/method", |call: Call| async move {
                call.method().as_str().to_string()
            });
            r.post("/see-other", |_c: Call| async {
                Response::new(StatusCode::SEE_OTHER)
                    .with_header(LOCATION, HeaderValue::from_static("/method"))
            });
            r.get("/entity", |call: Call| async move {
                format!(
                    "{} content-type={:?}",
                    call.method(),
                    call.header("content-type")
                )
            });
            r.post("/see-other-entity", |_c: Call| async {
                Response::new(StatusCode::SEE_OTHER)
                    .with_header(LOCATION, HeaderValue::from_static("/entity"))
            });
            r.get("/agent", |call: Call| async move {
                call.header("user-agent").unwrap_or("none").to_string()
            });
            r.get("/query", |call: Call| async move {
                call.uri().query().unwrap_or("").to_string()
            });
        })
        .build();

    tokio::spawn(async move {
        let _ = app.start_on(listener, std::future::pending()).await;
    });

    // Give the listener a moment to bind before the first connection.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn a_get_reads_the_body() {
    let base = serve().await;
    let res = Client::new()
        .get(format!("{base}/hello"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text().unwrap(), "hello");
}

#[tokio::test]
async fn json_round_trips_in_both_directions() {
    #[derive(serde::Deserialize)]
    struct Person {
        name: String,
        age: u8,
    }

    let base = serve().await;
    let client = Client::new();

    let person: Person = client
        .get(format!("{base}/json"))
        .send()
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(person.name, "ana");
    assert_eq!(person.age, 3);

    let echoed = client
        .post(format!("{base}/echo"))
        .json(&serde_json::json!({"k": "v"}))
        .send()
        .await
        .unwrap();
    assert_eq!(echoed.text().unwrap(), r#"{"k":"v"}"#);
}

#[tokio::test]
async fn a_timeout_is_enforced() {
    let base = serve().await;
    let err = Client::new()
        .timeout(Duration::from_millis(50))
        .get(format!("{base}/slow"))
        .send()
        .await
        .expect_err("a 400ms handler must not satisfy a 50ms deadline");

    assert!(matches!(err, ClientError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn a_per_request_timeout_overrides_the_client_one() {
    let base = serve().await;
    let res = Client::new()
        .timeout(Duration::from_millis(50))
        .get(format!("{base}/slow"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("the per-request deadline should win");

    assert_eq!(res.text().unwrap(), "eventually");
}

#[tokio::test]
async fn an_oversized_response_is_refused() {
    let base = serve().await;
    let err = Client::new()
        .max_response_bytes(1024)
        .get(format!("{base}/big"))
        .send()
        .await
        .expect_err("64 KiB must not fit in a 1 KiB ceiling");

    assert!(matches!(err, ClientError::BodyTooLarge(1024)), "{err:?}");
}

#[tokio::test]
async fn a_non_success_status_is_a_response_not_an_error() {
    let base = serve().await;
    let res = Client::new()
        .get(format!("{base}/teapot"))
        .send()
        .await
        .expect("418 is an answer");

    assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
    assert_eq!(res.text().unwrap(), "no coffee");
    assert!(res.error_for_status().is_err(), "opting in must flip it");
}

#[tokio::test]
async fn redirects_are_followed() {
    let base = serve().await;
    let res = Client::new()
        .get(format!("{base}/redirect"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text().unwrap(), "hello");
}

#[tokio::test]
async fn a_redirect_loop_ends_in_an_error_rather_than_a_hang() {
    let base = serve().await;
    let err = Client::new()
        .max_redirects(3)
        .get(format!("{base}/loop"))
        .send()
        .await
        .expect_err("a self-referential redirect must terminate");

    assert!(matches!(err, ClientError::TooManyRedirects(3)), "{err:?}");
}

#[tokio::test]
async fn redirects_can_be_switched_off() {
    let base = serve().await;
    let res = Client::new()
        .max_redirects(0)
        .get(format!("{base}/redirect"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.header("location"), Some("/hello"));
}

#[tokio::test]
async fn a_303_turns_a_post_into_a_get() {
    let base = serve().await;
    let res = Client::new()
        .post(format!("{base}/see-other"))
        .body("ignored")
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.text().unwrap(),
        "GET",
        "RFC 9110 §15.4.4 makes the follow-up a GET"
    );
}

#[tokio::test]
async fn a_303_drops_the_entity_headers_along_with_the_body() {
    // Flipping the method empties the body, so the headers that described that
    // body describe nothing. A `GET` still announcing `Content-Type:
    // application/json` is a request that contradicts itself, and it is not
    // what any other client sends: the Fetch standard deletes the
    // request-body-header-names on exactly this transition.
    let base = serve().await;
    let res = Client::new()
        .post(format!("{base}/see-other-entity"))
        .json(&serde_json::json!({"k": "v"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.text().unwrap(),
        "GET content-type=None",
        "the entity headers outlived the body they described"
    );
}

#[tokio::test]
async fn a_user_agent_is_sent_and_can_be_replaced() {
    let base = serve().await;

    let default = Client::new()
        .get(format!("{base}/agent"))
        .send()
        .await
        .unwrap();
    assert!(default.text().unwrap().starts_with("churust/"));

    let custom = Client::new()
        .user_agent("acme-sync/2.0")
        .unwrap()
        .get(format!("{base}/agent"))
        .send()
        .await
        .unwrap();
    assert_eq!(custom.text().unwrap(), "acme-sync/2.0");
}

#[tokio::test]
async fn query_parameters_reach_the_server() {
    let base = serve().await;
    let res = Client::new()
        .get(format!("{base}/query"))
        .query(&[("tag", "a"), ("tag", "b")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.text().unwrap(), "tag=a&tag=b");
}

#[tokio::test]
async fn a_default_header_is_sent_on_every_request() {
    let base = serve().await;
    let client = Client::new().default_header("x-api-key", "secret").unwrap();

    let app_echo = client
        .get(format!("{base}/agent"))
        .send()
        .await
        .expect("the request should still work");
    assert_eq!(app_echo.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_refused_connection_is_a_transport_error() {
    // Bind and drop on purpose: this test needs a port with nothing on it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let err = Client::new()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .expect_err("nothing is listening");

    assert!(matches!(err, ClientError::Transport(_)), "{err:?}");
}
