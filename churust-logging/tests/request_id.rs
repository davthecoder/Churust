//! Request correlation: a per-request id, and W3C trace-context continuation.

use churust_core::{Call, Churust, TestClient};
use churust_logging::{CallLogging, RequestId};

fn app() -> churust_core::App {
    Churust::server()
        .install(CallLogging::new())
        .routing(|r| {
            r.get("/", |c: Call| async move {
                let id = c.get::<RequestId>().expect("RequestId must be seeded");
                format!("{}|{}", id.request_id, id.trace_id)
            });
        })
        .build()
}

fn parts(body: &str) -> (String, String) {
    let mut it = body.split('|');
    (
        it.next().unwrap_or("").to_string(),
        it.next().unwrap_or("").to_string(),
    )
}

#[tokio::test]
async fn every_request_gets_an_id_echoed_in_a_header() {
    let res = TestClient::new(app()).get("/").send().await;
    let (request_id, trace_id) = parts(&res.text());

    assert_eq!(request_id.len(), 16, "request id should be 16 hex chars");
    assert_eq!(trace_id.len(), 32, "trace id should be 32 hex chars");
    assert!(request_id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(res.header("x-request-id"), Some(request_id.as_str()));
}

#[tokio::test]
async fn ids_differ_between_requests() {
    let client = TestClient::new(app());
    let a = parts(&client.get("/").send().await.text()).0;
    let b = parts(&client.get("/").send().await.text()).0;
    assert_ne!(a, b, "each request needs its own id");
}

#[tokio::test]
async fn an_inbound_traceparent_is_continued() {
    let incoming = "4bf92f3577b34da6a3ce929d0e0e4736";
    let res = TestClient::new(app())
        .get("/")
        .header("traceparent", &format!("00-{incoming}-00f067aa0ba902b7-01"))
        .send()
        .await;

    let (_, trace_id) = parts(&res.text());
    assert_eq!(
        trace_id, incoming,
        "a request crossing a service boundary must keep one trace id"
    );
}

#[tokio::test]
async fn a_malformed_traceparent_is_not_trusted() {
    let client = TestClient::new(app());

    for bad in [
        "garbage",
        "00-tooshort-00f067aa0ba902b7-01",
        "00-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-00f067aa0ba902b7-01", // not hex
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01", // all-zero is invalid
    ] {
        let res = client.get("/").header("traceparent", bad).send().await;
        let (_, trace_id) = parts(&res.text());
        assert_eq!(
            trace_id.len(),
            32,
            "a fresh trace id should replace {bad:?}"
        );
        assert_ne!(
            trace_id, "00000000000000000000000000000000",
            "an all-zero trace id must never be adopted"
        );
        assert!(
            trace_id.chars().all(|c| c.is_ascii_hexdigit()),
            "generated trace id must be hex, got {trace_id:?}"
        );
    }
}
