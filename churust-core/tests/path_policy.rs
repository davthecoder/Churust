//! Path normalisation is a policy, and it is now stated.
//!
//! Interior empty segments were silently collapsed, so `/admin/secret`,
//! `//admin/secret` and `/admin//secret` were three URLs for one resource.
//! That is not a traversal bug — the traversal suite covers that — it is
//! *aliasing*, and it has two consequences worth refusing:
//!
//! - Any middleware, guard or proxy rule keyed on a literal prefix
//!   (`path.starts_with("/admin")`) is bypassable with `//admin`.
//! - Caches key on the URL, so one resource occupies several cache entries and
//!   a proxy and the origin can disagree about identity.

use churust_core::{Call, Churust, PathPolicy, TestClient};
use http::StatusCode;

fn app(policy: PathPolicy) -> churust_core::App {
    Churust::server()
        .path_policy(policy)
        .routing(|r| {
            r.get("/", |_c: Call| async { "root" });
            r.get("/admin/secret", |_c: Call| async { "SECRET" });
        })
        .build()
}

async fn get(policy: PathPolicy, path: &str) -> (StatusCode, String, Option<String>) {
    let res = TestClient::new(app(policy)).get(path).send().await;
    let location = res.header("location").map(str::to_string);
    (res.status(), res.text(), location)
}

/// Interior-empty-segment spellings that previously reached the handler.
const ALIASES: &[&str] = &["//admin/secret", "/admin//secret", "//admin//secret"];

#[tokio::test]
async fn the_canonical_path_always_works() {
    for policy in [
        PathPolicy::Strict,
        PathPolicy::Redirect,
        PathPolicy::Collapse,
    ] {
        let (status, body, _) = get(policy, "/admin/secret").await;
        assert_eq!(status, StatusCode::OK, "{policy:?}");
        assert_eq!(body, "SECRET", "{policy:?}");
    }
}

#[tokio::test]
async fn a_trailing_slash_is_not_treated_as_an_alias() {
    // Deliberate, and not an oversight. A directory listing served at `/files/`
    // contains relative links like `<a href="a.txt">`, which a browser resolves
    // against the trailing slash. Strip it and every link on the generated page
    // resolves one level too high. The aliasing benefit is not worth serving
    // broken HTML, and interior slashes — the ones that defeat a prefix check —
    // are handled regardless.
    for policy in [
        PathPolicy::Strict,
        PathPolicy::Redirect,
        PathPolicy::Collapse,
    ] {
        let (status, body, _) = get(policy, "/admin/secret/").await;
        assert_eq!(status, StatusCode::OK, "{policy:?}: {body}");
    }
}

#[tokio::test]
async fn an_alias_with_a_trailing_slash_keeps_the_slash_when_redirected() {
    let (status, _, location) = get(PathPolicy::Redirect, "//admin//secret/").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(location.as_deref(), Some("/admin/secret/"));
}

#[tokio::test]
async fn the_root_path_is_canonical_under_every_policy() {
    // `/` is a trailing slash that is not an alias. Getting this wrong would
    // redirect the root to the empty string.
    for policy in [
        PathPolicy::Strict,
        PathPolicy::Redirect,
        PathPolicy::Collapse,
    ] {
        let (status, body, _) = get(policy, "/").await;
        assert_eq!(status, StatusCode::OK, "{policy:?}");
        assert_eq!(body, "root", "{policy:?}");
    }
}

#[tokio::test]
async fn strict_refuses_every_alias() {
    for alias in ALIASES {
        let (status, _, _) = get(PathPolicy::Strict, alias).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{alias} was served under the strict policy"
        );
    }
}

#[tokio::test]
async fn redirect_sends_each_alias_to_the_canonical_form() {
    for alias in ALIASES {
        let (status, _, location) = get(PathPolicy::Redirect, alias).await;
        assert_eq!(
            status,
            StatusCode::PERMANENT_REDIRECT,
            "{alias} was not redirected"
        );
        assert_eq!(
            location.as_deref(),
            Some("/admin/secret"),
            "{alias} redirected somewhere unexpected"
        );
    }
}

#[tokio::test]
async fn a_redirect_is_308_so_a_post_stays_a_post() {
    // 301 lets a client turn a POST into a GET, which silently drops the body.
    let res = TestClient::new(app(PathPolicy::Redirect))
        .post("//admin/secret")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
}

#[tokio::test]
async fn a_redirect_preserves_the_query_string() {
    let res = TestClient::new(app(PathPolicy::Redirect))
        .get("//admin/secret?page=2&tag=x")
        .send()
        .await;
    assert_eq!(
        res.header("location"),
        Some("/admin/secret?page=2&tag=x"),
        "dropping the query would silently change the request"
    );
}

#[tokio::test]
async fn collapse_preserves_the_old_behaviour() {
    // Available for one release so an application with alias-shaped links has
    // somewhere to stand while it fixes them.
    for alias in ALIASES {
        let (status, body, _) = get(PathPolicy::Collapse, alias).await;
        assert_eq!(status, StatusCode::OK, "{alias}");
        assert_eq!(body, "SECRET", "{alias}");
    }
}

#[tokio::test]
async fn strict_is_the_default() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/admin/secret", |_c: Call| async { "SECRET" });
        })
        .build();
    let res = TestClient::new(app).get("//admin/secret").send().await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_refused_alias_is_an_ordinary_response_on_the_way_out() {
    // Why the policy decision sits in the endpoint, inside the middleware
    // chain, rather than ahead of it. Hoisting it out would let the refusal
    // skip everything wrapped around the endpoint, and a `404` is exactly as
    // reachable as handler output: it would go out without the security
    // headers every other response carries, and `on_error` — whose whole
    // premise is that it renders *any* `4xx`, including the `404` for an
    // unmatched path — would never see it.
    //
    // The cost of that placement is that middleware runs first and observes the
    // raw, un-collapsed spelling. It is not a bypass under `Strict` or
    // `Redirect`: whatever a prefix-keyed middleware decides, the endpoint
    // still replaces the response, so nothing is served.
    let app = Churust::server()
        .on_error(|status, call| {
            (status == StatusCode::NOT_FOUND)
                .then(|| churust_core::Response::text(format!("refused {}", call.path())))
        })
        .routing(|r| {
            r.get("/admin/secret", |_c: Call| async { "SECRET" });
        })
        .build();

    let res = TestClient::new(app).get("//admin/secret").send().await;
    assert_eq!(
        res.text(),
        "refused //admin/secret",
        "the alias refusal never reached the on_error renderer"
    );
    assert_eq!(
        res.header("x-content-type-options"),
        Some("nosniff"),
        "the alias refusal went out without the default security headers"
    );
}

#[tokio::test]
async fn collapse_hands_the_raw_spelling_to_middleware_and_handlers() {
    // `Collapse` collapses for *matching* only; it does not rewrite the URI.
    // So `call.path()` is the spelling the client sent, in middleware and in
    // the handler alike, and a prefix check keyed on it is bypassable — which
    // is the documented hazard of opting into aliases, and the reason
    // `Collapse` exists only as a migration step.
    let app = Churust::server()
        .path_policy(PathPolicy::Collapse)
        .routing(|r| {
            r.get(
                "/admin/secret",
                |c: Call| async move { c.path().to_string() },
            );
        })
        .build();

    let res = TestClient::new(app).get("//admin/secret").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.text(),
        "//admin/secret",
        "Collapse rewrote the URI; the docs promise it does not"
    );
}

#[tokio::test]
async fn an_encoded_slash_is_not_a_separator_under_any_policy() {
    // `%2F` is data inside one segment. Decoding it into a separator before
    // matching is how a normalisation change becomes a traversal bug.
    for policy in [
        PathPolicy::Strict,
        PathPolicy::Redirect,
        PathPolicy::Collapse,
    ] {
        let (status, _, _) = get(policy, "/admin%2Fsecret").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{policy:?} treated %2F as a path separator"
        );
    }
}
