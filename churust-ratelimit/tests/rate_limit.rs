//! Rate limiting over the full pipeline.

use churust_core::{Call, Churust, TestClient};
use churust_ratelimit::RateLimit;
use http::StatusCode;
use std::time::Duration;

fn app(limiter: RateLimit) -> churust_core::App {
    Churust::server()
        .install(limiter)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build()
}

#[tokio::test]
async fn requests_above_the_limit_are_refused_with_429() {
    let client = TestClient::new(app(RateLimit::per(2, Duration::from_secs(30))));

    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);
    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);

    let limited = client.get("/").send().await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        limited.text().contains("rate limit exceeded"),
        "the body should say why: {}",
        limited.text()
    );
}

#[tokio::test]
async fn a_refusal_carries_retry_after_in_whole_seconds() {
    let client = TestClient::new(app(RateLimit::per(1, Duration::from_secs(30))));
    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);

    let limited = client.get("/").send().await;
    let retry_after = limited
        .header("retry-after")
        .expect("RFC 9110 §15.5.29 wants a Retry-After on a 429");
    let secs: u64 = retry_after.parse().expect("must be delta-seconds");
    assert!(
        (1..=30).contains(&secs),
        "retry-after should be inside the period: {secs}"
    );
}

#[tokio::test]
async fn the_budget_refills_with_time() {
    // 20 per second is one every 50ms; burst(1) removes the initial allowance
    // so the second request is refused and the third, after the interval, is
    // not. Keeping the wait this short is why the period is sub-second.
    let client = TestClient::new(app(RateLimit::per_second(20).burst(1)));

    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);
    assert_eq!(
        client.get("/").send().await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        client.get("/").send().await.status(),
        StatusCode::OK,
        "the emission interval has passed, so this conforms"
    );
}

#[tokio::test]
async fn a_none_key_exempts_the_request() {
    let limiter = RateLimit::per(1, Duration::from_secs(30)).by(|call: &Call| {
        // Health checks are never limited; everything else shares one bucket.
        (call.path() != "/health").then(|| "all".to_string())
    });
    let app = Churust::server()
        .install(limiter)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
            r.get("/health", |_c: Call| async { "up" });
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);
    assert_eq!(
        client.get("/").send().await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    for _ in 0..5 {
        assert_eq!(
            client.get("/health").send().await.status(),
            StatusCode::OK,
            "an exempt route must never be refused"
        );
    }
}

#[tokio::test]
async fn distinct_keys_get_distinct_budgets() {
    let limiter = RateLimit::per(1, Duration::from_secs(30))
        .by(|call: &Call| call.header("x-api-key").map(str::to_owned));
    let client = TestClient::new(app(limiter));

    assert_eq!(
        client
            .get("/")
            .header("x-api-key", "a")
            .send()
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get("/")
            .header("x-api-key", "a")
            .send()
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        client
            .get("/")
            .header("x-api-key", "b")
            .send()
            .await
            .status(),
        StatusCode::OK,
        "a different key must not inherit another key's spend"
    );
}

#[tokio::test]
async fn scoped_middleware_limits_only_its_own_subtree() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/open", |_c: Call| async { "open" });
            r.route("/api", |r| {
                r.intercept(RateLimit::per(1, Duration::from_secs(30)));
                r.get("/thing", |_c: Call| async { "thing" });
            });
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(
        client.get("/api/thing").send().await.status(),
        StatusCode::OK
    );
    assert_eq!(
        client.get("/api/thing").send().await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    for _ in 0..5 {
        assert_eq!(
            client.get("/open").send().await.status(),
            StatusCode::OK,
            "a route outside the scope is not limited"
        );
    }
}
