#![cfg(feature = "fs")]
//! Directory listings: confinement, and links that actually resolve.
//!
//! Two defects this pins:
//!
//! 1. The symlink-escape guard sat *after* the directory dispatch, so a symlink
//!    to a directory outside the served root reached the listing renderer
//!    unchecked. `metadata` follows symlinks, so `is_dir()` was true and the
//!    listing arm returned before the guard ever ran — disclosing every
//!    filename under whatever the link pointed at. The file-serving path was
//!    guarded; only the listing path was not.
//! 2. Listing links were prefixed with the request-relative path, which is
//!    correct at a subdirectory *without* a trailing slash and wrong everywhere
//!    else. Directory URLs are now canonical (trailing slash, `308` otherwise)
//!    and links are bare, so the same markup works at any depth.

use churust_core::{Churust, StaticFiles, TestClient};
use http::StatusCode;

/// A served root with a subdirectory, plus a secret alongside it that a symlink
/// inside the root points at.
fn tree(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("churust-listing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("public");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), "AAA").unwrap();
    std::fs::write(root.join("sub").join("b.txt"), "BBB").unwrap();

    // The escape target, outside the served root.
    let secret_dir = base.join("private");
    std::fs::create_dir_all(&secret_dir).unwrap();
    std::fs::write(secret_dir.join("credentials.txt"), "SECRET").unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret_dir, root.join("escape")).unwrap();

    root
}

fn app(root: &std::path::Path) -> churust_core::App {
    let root = root.to_path_buf();
    Churust::server()
        .routing(move |r| {
            r.get(
                "/s/{path...}",
                StaticFiles::dir(root.clone())
                    .list_directories(true)
                    .handler(),
            );
        })
        .build()
}

#[tokio::test]
#[cfg(unix)]
async fn a_symlinked_directory_outside_the_root_is_not_listed() {
    let root = tree("escape");
    let res = TestClient::new(app(&root)).get("/s/escape/").send().await;
    let body = res.text();
    assert!(
        !body.contains("credentials.txt"),
        "the listing disclosed filenames outside the served root: {body}"
    );
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "{body}");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
#[cfg(unix)]
async fn a_file_through_an_escaping_symlink_is_still_refused() {
    // The file path was already guarded; this keeps it that way.
    let root = tree("escapefile");
    let res = TestClient::new(app(&root))
        .get("/s/escape/credentials.txt")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(!res.text().contains("SECRET"));
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
async fn a_directory_without_a_trailing_slash_redirects_to_one() {
    let root = tree("redirect");
    let res = TestClient::new(app(&root)).get("/s/sub").send().await;
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.header("location"), Some("/s/sub/"));
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
async fn the_redirect_preserves_the_query_string() {
    let root = tree("redirectq");
    let res = TestClient::new(app(&root))
        .get("/s/sub?sort=name")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.header("location"), Some("/s/sub/?sort=name"));
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
async fn listing_links_are_bare_so_they_resolve_at_any_depth() {
    let root = tree("links");
    let client = TestClient::new(app(&root));

    // At the mount root.
    let body = client.get("/s/").send().await.text();
    assert!(
        body.contains(r#"href="a.txt""#),
        "root listing should emit a bare link: {body}"
    );
    assert!(
        body.contains(r#"href="sub/""#),
        "a subdirectory link is the bare name plus a slash: {body}"
    );

    // One level down: same shape, no request-path prefix.
    let body = client.get("/s/sub/").send().await.text();
    assert!(
        body.contains(r#"href="b.txt""#),
        "subdirectory listing must not prefix the request path: {body}"
    );
    assert!(
        !body.contains(r#"href="sub/b.txt""#),
        "the request-path prefix would resolve to /s/sub/sub/b.txt: {body}"
    );
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
async fn a_link_from_a_listing_actually_fetches_the_file() {
    // The end-to-end property the href shape exists for: resolve the emitted
    // relative link against the listing URL and fetch it.
    let root = tree("follow");
    let client = TestClient::new(app(&root));
    let body = client.get("/s/sub/").send().await.text();
    assert!(body.contains(r#"href="b.txt""#), "{body}");

    // A browser at /s/sub/ resolves href="b.txt" to /s/sub/b.txt.
    let res = client.get("/s/sub/b.txt").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "BBB");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
async fn files_are_unaffected_by_the_directory_redirect() {
    let root = tree("files");
    let res = TestClient::new(app(&root)).get("/s/a.txt").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "AAA");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[tokio::test]
async fn a_filename_cannot_become_a_url_scheme_or_query() {
    // The listing's threat model is a filename chosen by whoever can write to
    // the served directory. HTML-escaping alone left `:` intact, so
    // `javascript:alert(...)` was emitted as a syntactically valid absolute URL
    // that a browser executes in the serving origin rather than resolving
    // relatively. `#` and `?` truncated the target instead.
    let base = std::env::temp_dir().join(format!("churust-hostile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("public");
    std::fs::create_dir_all(&root).unwrap();

    let hostile = [
        "javascript:alert(document.cookie)",
        "a#b.txt",
        "q?x=1.txt",
        "50%.txt",
        "a b.txt",
    ];
    for name in hostile {
        std::fs::write(root.join(name), "X").unwrap();
    }

    let body = TestClient::new(app(&root)).get("/s/").send().await.text();

    assert!(
        !body.contains(r#"href="javascript:"#),
        "a filename became a live script URL: {body}"
    );
    assert!(
        body.contains("javascript%3A"),
        "the colon must be encoded so the href stays a relative path: {body}"
    );
    for (raw, encoded) in [("a#b.txt", "a%23b.txt"), ("q?x=1.txt", "q%3Fx%3D1.txt")] {
        assert!(
            body.contains(&format!(r#"href="{encoded}""#)),
            "{raw} should link to {encoded}: {body}"
        );
    }

    // The visible label stays readable — only the href is encoded.
    assert!(
        body.contains("a b.txt"),
        "label should not be percent-encoded: {body}"
    );

    // And an encoded link still fetches the file it names.
    let res = TestClient::new(app(&root)).get("/s/a%20b.txt").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "X");

    let _ = std::fs::remove_dir_all(&base);
}
