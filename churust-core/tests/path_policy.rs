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
