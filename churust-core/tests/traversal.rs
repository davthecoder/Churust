//! Directory-traversal attempts against `StaticFiles`.
//!
//! A canary file is planted immediately outside the served root, so a
//! successful traversal is *detected* rather than merely not-asserted. A test
//! that only checks for a 404 would also pass if the attack path happened to
//! name a file that does not exist.

#![cfg(feature = "fs")]

use churust_core::{fs::StaticFiles, App, Churust, TestClient};
use http::StatusCode;

const CANARY: &str = "CANARY-SHOULD-NEVER-BE-SERVED";

/// `<base>/public` is served; `<base>/secret.txt` holds the canary.
struct Tree {
    root: std::path::PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("churust-traversal-{}-{tag}", std::process::id()));
        let root = base.join("public");
        std::fs::create_dir_all(&root).expect("create served root");
        std::fs::write(root.join("ok.txt"), "public file").expect("write public file");
        std::fs::write(base.join("secret.txt"), CANARY).expect("plant canary");
        Self { root }
    }
}

fn app(root: &std::path::Path) -> App {
    let root = root.to_path_buf();
    Churust::server()
        .routing(move |r| {
            r.get("/s/{path...}", StaticFiles::dir(root.clone()).handler());
        })
        .build()
}

#[tokio::test]
async fn serves_a_normal_file() {
    let t = Tree::new("normal");
    let res = TestClient::new(app(&t.root)).get("/s/ok.txt").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "public file");
}

#[tokio::test]
async fn serves_a_file_whose_name_needs_encoding() {
    let t = Tree::new("encoded-name");
    std::fs::write(t.root.join("my file.txt"), "spaced").expect("write spaced file");

    // Before path decoding existed this 404d: the handler looked for a file
    // literally named "my%20file.txt".
    let res = TestClient::new(app(&t.root))
        .get("/s/my%20file.txt")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "spaced");
}

/// An encoded separator must not silently become a real one.
///
/// `sanitize` already rejects `..`, so this is not a traversal hole — but
/// letting `%2F` act as a path separator means the safety argument depends on
/// reasoning about how decoded segments get rejoined. Refusing it outright
/// makes the rejoined value unambiguous by construction instead.
#[tokio::test]
async fn an_encoded_separator_is_refused_rather_than_normalized() {
    let t = Tree::new("encoded-sep");
    std::fs::create_dir_all(t.root.join("a")).expect("create subdir");
    std::fs::write(t.root.join("a/b.txt"), "nested").expect("write nested file");

    let client = TestClient::new(app(&t.root));

    // The honest path works.
    let ok = client.get("/s/a/b.txt").send().await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.text(), "nested");

    // The encoded one is refused, not quietly treated as the same request.
    let res = client.get("/s/a%2Fb.txt").send().await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "%2F was treated as a separator instead of being refused"
    );
}

#[tokio::test]
async fn every_traversal_shape_is_refused() {
    let t = Tree::new("traversal");
    let client = TestClient::new(app(&t.root));

    let attempts = [
        "/s/../secret.txt",
        "/s/%2e%2e/secret.txt",
        "/s/%2e%2e%2fsecret.txt",
        "/s/..%2fsecret.txt",
        "/s/%252e%252e%252fsecret.txt",
        "/s/%2fsecret.txt",
        "/s/..%5csecret.txt",
        "/s/..%5Csecret.txt",
        "/s/....//secret.txt",
        "/s/a/../../secret.txt",
        "/s/a/%2e%2e/%2e%2e/secret.txt",
        "/s/%2E%2E/secret.txt",
    ];

    for attempt in attempts {
        let res = client.get(attempt).send().await;
        assert!(
            res.status() == StatusCode::NOT_FOUND || res.status() == StatusCode::BAD_REQUEST,
            "{attempt} returned {} — expected 404 or 400",
            res.status()
        );
        assert!(
            !res.text().contains(CANARY),
            "{attempt} SERVED THE CANARY — directory traversal is possible"
        );
    }
}
