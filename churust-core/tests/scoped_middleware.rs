//! Route-scoped middleware — v1 design §6.

use async_trait::async_trait;
use churust_core::{Call, Churust, Middleware, Next, Response, TestClient};
use http::header::HeaderName;
use http::{HeaderValue, StatusCode};

/// Stamps a header so a response shows which scopes it passed through.
struct Stamp(&'static str);

#[async_trait]
impl Middleware for Stamp {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let mut res = next.run(call).await;
        let existing = res
            .headers
            .get("x-scopes")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let joined = if existing.is_empty() {
            self.0.to_string()
        } else {
            format!("{existing},{}", self.0)
        };
        if let Ok(v) = HeaderValue::from_str(&joined) {
            res.headers.insert(HeaderName::from_static("x-scopes"), v);
        }
        res
    }
}

/// Refuses the request outright, to prove a scope guard can short-circuit.
struct Deny;

#[async_trait]
impl Middleware for Deny {
    async fn handle(&self, _call: Call, _next: Next) -> Response {
        Response::text("nope").with_status(StatusCode::FORBIDDEN)
    }
}

#[tokio::test]
async fn scope_middleware_applies_only_inside_the_scope() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/open", |_c: Call| async { "open" });
            r.route("/api", |r| {
                r.intercept(Stamp("api"));
                r.get("/thing", |_c: Call| async { "thing" });
            });
        })
        .build();
    let client = TestClient::new(app);

    let inside = client.get("/api/thing").send().await;
    assert_eq!(inside.text(), "thing");
    assert_eq!(inside.header("x-scopes"), Some("api"));

    let outside = client.get("/open").send().await;
    assert_eq!(outside.text(), "open");
    assert!(
        outside.header("x-scopes").is_none(),
        "scope middleware must not leak to sibling routes"
    );
}

#[tokio::test]
async fn nested_scopes_compose_outermost_first() {
    let app = Churust::server()
        .routing(|r| {
            r.route("/api", |r| {
                r.intercept(Stamp("api"));
                r.route("/admin", |r| {
                    r.intercept(Stamp("admin"));
                    r.get("/x", |_c: Call| async { "x" });
                });
            });
        })
        .build();

    let res = TestClient::new(app).get("/api/admin/x").send().await;
    assert_eq!(res.text(), "x");
    // Inner runs first on the way out, so it stamps first.
    assert_eq!(res.header("x-scopes"), Some("admin,api"));
}

#[tokio::test]
async fn a_scope_middleware_can_short_circuit() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/public", |_c: Call| async { "public" });
            r.route("/private", |r| {
                r.intercept(Deny);
                r.get("/secret", |_c: Call| async { "secret" });
            });
        })
        .build();
    let client = TestClient::new(app);

    let blocked = client.get("/private/secret").send().await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert_ne!(blocked.text(), "secret", "the handler must not have run");

    assert_eq!(client.get("/public").send().await.text(), "public");
}

#[tokio::test]
async fn interceptors_registered_after_a_route_do_not_apply_to_it() {
    // Registration order is meaningful and worth pinning: a route registered
    // before `intercept` was written was not covered by it.
    let app = Churust::server()
        .routing(|r| {
            r.route("/s", |r| {
                r.get("/before", |_c: Call| async { "before" });
                r.intercept(Stamp("late"));
                r.get("/after", |_c: Call| async { "after" });
            });
        })
        .build();
    let client = TestClient::new(app);

    assert!(client
        .get("/s/before")
        .send()
        .await
        .header("x-scopes")
        .is_none());
    assert_eq!(
        client.get("/s/after").send().await.header("x-scopes"),
        Some("late")
    );
}

#[tokio::test]
async fn app_middleware_still_wraps_scope_middleware() {
    let app = Churust::server()
        .install_middleware(Stamp("app"))
        .routing(|r| {
            r.route("/api", |r| {
                r.intercept(Stamp("scope"));
                r.get("/x", |_c: Call| async { "x" });
            });
        })
        .build();

    let res = TestClient::new(app).get("/api/x").send().await;
    // Scope is inner, so it stamps before the app-wide layer.
    assert_eq!(res.header("x-scopes"), Some("scope,app"));
}
