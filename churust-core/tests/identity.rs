//! Login, logout, and the deadlines that end a login, over the full pipeline.

use churust_core::{Authenticated, Call, Churust, Identities, Identity, Sessions, TestClient};
use http::StatusCode;
use std::time::Duration;

fn app(layer: Identities) -> churust_core::App {
    Churust::server()
        .install(Sessions::cookie("test-key"))
        .install(layer)
        .routing(|r| {
            r.post("/login", |id: Identity| async move {
                id.login("user-42");
                "welcome"
            });
            r.get(
                "/me",
                |Authenticated(who): Authenticated| async move { who },
            );
            r.get("/whoami", |id: Identity| async move {
                id.id().unwrap_or_else(|| "anonymous".into())
            });
            r.post("/logout", |id: Identity| async move {
                id.logout();
                "bye"
            });
        })
        .build()
}

/// Pull the session cookie out of a `Set-Cookie` header.
fn session_cookie(res: &churust_core::TestResponse) -> String {
    let raw = res.header("set-cookie").expect("expected a session cookie");
    raw.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn an_anonymous_visitor_is_refused_by_the_authenticated_extractor() {
    let client = TestClient::new(app(Identities::new()));
    let res = client.get("/me").send().await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(
        res.headers().get("www-authenticate").is_none(),
        "there is no registered auth scheme for a cookie session, so naming one would be a lie"
    );
}

#[tokio::test]
async fn a_login_survives_across_requests() {
    let client = TestClient::new(app(Identities::new()));

    let login = client.post("/login").send().await;
    assert_eq!(login.text(), "welcome");
    let cookie = session_cookie(&login);

    let me = client.get("/me").header("cookie", &cookie).send().await;
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(me.text(), "user-42");
}

#[tokio::test]
async fn logout_ends_the_login() {
    let client = TestClient::new(app(Identities::new()));

    let login = client.post("/login").send().await;
    let cookie = session_cookie(&login);

    let out = client
        .post("/logout")
        .header("cookie", &cookie)
        .send()
        .await;
    let cleared = session_cookie(&out);

    let me = client.get("/me").header("cookie", &cleared).send().await;
    assert_eq!(
        me.status(),
        StatusCode::UNAUTHORIZED,
        "the re-issued cookie must no longer carry an identity"
    );
}

#[tokio::test]
async fn a_configured_login_url_redirects_instead_of_refusing() {
    let client = TestClient::new(app(Identities::new().login_url("/sign-in")));

    let res = client.get("/me").send().await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.header("location"), Some("/sign-in"));
}

#[tokio::test]
async fn the_absolute_deadline_ends_a_login_however_active_the_visitor_is() {
    let client = TestClient::new(app(Identities::new().login_deadline(1)));

    let login = client.post("/login").send().await;
    let mut cookie = session_cookie(&login);

    // Stay active across the deadline: an idle timeout would keep renewing
    // here, and the absolute one must not.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let res = client.get("/whoami").header("cookie", &cookie).send().await;
        if let Some(refreshed) = res.header("set-cookie") {
            cookie = refreshed.split(';').next().unwrap().to_string();
        }
    }

    let me = client.get("/me").header("cookie", &cookie).send().await;
    assert_eq!(
        me.status(),
        StatusCode::UNAUTHORIZED,
        "a login older than login_deadline must end even under constant use"
    );
}

#[tokio::test]
async fn the_idle_deadline_ends_a_login_that_goes_quiet() {
    let client = TestClient::new(app(Identities::new().visit_deadline(1)));

    let login = client.post("/login").send().await;
    let cookie = session_cookie(&login);

    tokio::time::sleep(Duration::from_millis(1200)).await;

    let me = client.get("/me").header("cookie", &cookie).send().await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn activity_keeps_an_idle_deadline_from_expiring() {
    let client = TestClient::new(app(Identities::new().visit_deadline(3)));

    let login = client.post("/login").send().await;
    let mut cookie = session_cookie(&login);

    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let res = client.get("/whoami").header("cookie", &cookie).send().await;
        assert_eq!(res.text(), "user-42");
        if let Some(refreshed) = res.header("set-cookie") {
            cookie = refreshed.split(';').next().unwrap().to_string();
        }
    }

    let me = client.get("/me").header("cookie", &cookie).send().await;
    assert_eq!(
        me.status(),
        StatusCode::OK,
        "two seconds of steady traffic must not trip a three second idle timeout"
    );
}

#[tokio::test]
async fn an_unchanged_session_is_not_re_issued_on_every_request() {
    // No visit_deadline, so there is no timestamp to refresh and nothing should
    // change between reads. Re-issuing anyway would extend the cookie's expiry
    // on traffic that did not touch the session.
    let client = TestClient::new(app(Identities::new()));

    let login = client.post("/login").send().await;
    let cookie = session_cookie(&login);

    let first = client.get("/whoami").header("cookie", &cookie).send().await;
    assert_eq!(first.text(), "user-42");
    assert!(
        first.header("set-cookie").is_none(),
        "a read-only request must not rewrite the session cookie"
    );
}

#[tokio::test]
async fn the_layer_works_without_the_session_plugin_installed() {
    // Nothing persists, but nothing panics either, and every visitor is
    // anonymous. This is the same failure mode `Session` itself has.
    let app = Churust::server()
        .install(Identities::new())
        .routing(|r| {
            r.get("/whoami", |id: Identity| async move {
                id.id().unwrap_or_else(|| "anonymous".into())
            });
            r.post("/login", |id: Identity| async move {
                id.login("user-42");
                "welcome"
            });
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(client.post("/login").send().await.text(), "welcome");
    assert_eq!(client.get("/whoami").send().await.text(), "anonymous");
}

#[tokio::test]
async fn install_order_does_not_matter() {
    // Identities runs in Phase::Call, inside the session plugin's phase, so
    // installing it first must work exactly as well as installing it second.
    let app = Churust::server()
        .install(Identities::new())
        .install(Sessions::cookie("test-key"))
        .routing(|r| {
            r.post("/login", |id: Identity| async move {
                id.login("user-42");
                "welcome"
            });
            r.get(
                "/me",
                |Authenticated(who): Authenticated| async move { who },
            );
        })
        .build();
    let client = TestClient::new(app);

    let login = client.post("/login").send().await;
    let cookie = session_cookie(&login);
    let me = client.get("/me").header("cookie", &cookie).send().await;
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(me.text(), "user-42");
}

#[tokio::test]
async fn a_session_without_timestamps_is_not_expired_on_sight() {
    // A session that predates the identity layer carries an id and no
    // timestamps. Treating the missing timestamp as infinitely old would log
    // out every existing visitor the moment the layer is deployed.
    let app = Churust::server()
        .install(Sessions::cookie("test-key"))
        .install(Identities::new().login_deadline(1).visit_deadline(1))
        .routing(|r| {
            r.post("/legacy", |call: Call| async move {
                let session = call.get::<churust_core::Session>().unwrap();
                session.set("__churust_uid", "user-42");
                "seeded"
            });
            r.get(
                "/me",
                |Authenticated(who): Authenticated| async move { who },
            );
        })
        .build();
    let client = TestClient::new(app);

    let seeded = client.post("/legacy").send().await;
    let cookie = session_cookie(&seeded);

    let me = client.get("/me").header("cookie", &cookie).send().await;
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(me.text(), "user-42");
}
