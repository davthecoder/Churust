//! Directory listing (`fs`) and Unix domain sockets.

use churust_core::{Call, Churust};

#[cfg(feature = "fs")]
mod listing {
    use churust_core::fs::StaticFiles;
    use churust_core::{Churust, TestClient};
    use http::StatusCode;

    fn tree(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("churust-list-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        root
    }

    fn app(root: &std::path::Path, listing: bool) -> churust_core::App {
        let root = root.to_path_buf();
        Churust::server()
            .routing(move |r| {
                r.get(
                    "/s/{path...}",
                    StaticFiles::dir(root.clone())
                        .list_directories(listing)
                        .handler(),
                );
            })
            .build()
    }

    #[tokio::test]
    async fn listing_is_off_by_default() {
        let root = tree("off");
        let res = TestClient::new(app(&root, false)).get("/s/").send().await;
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "disclosing filenames must be opt-in"
        );
    }

    #[tokio::test]
    async fn listing_shows_entries_when_enabled() {
        let root = tree("on");
        let res = TestClient::new(app(&root, true)).get("/s/").send().await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.text();
        assert!(body.contains("a.txt"), "{body}");
        assert!(
            body.contains("sub/"),
            "directories should be marked: {body}"
        );
    }

    #[tokio::test]
    async fn filenames_are_html_escaped() {
        let root = tree("xss");
        std::fs::write(root.join("<script>.txt"), "x").unwrap();

        let res = TestClient::new(app(&root, true)).get("/s/").send().await;
        let body = res.text();
        assert!(
            !body.contains("<script>.txt"),
            "an unescaped filename is stored XSS: {body}"
        );
        assert!(body.contains("&lt;script&gt;"), "{body}");
    }

    #[tokio::test]
    async fn an_index_file_still_wins_over_a_listing() {
        let root = tree("index");
        std::fs::write(root.join("index.html"), "INDEX").unwrap();
        let root2 = root.clone();
        let app = Churust::server()
            .routing(move |r| {
                r.get(
                    "/s/{path...}",
                    StaticFiles::dir(root2.clone())
                        .index("index.html")
                        .list_directories(true)
                        .handler(),
                );
            })
            .build();

        assert_eq!(TestClient::new(app).get("/s/").send().await.text(), "INDEX");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn serves_over_a_unix_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let path = std::env::temp_dir().join(format!("churust-uds-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "over uds" });
        })
        .build();

    let p = path.clone();
    let server = tokio::spawn(async move {
        let _ = churust_core::engine::serve_unix(app, p, std::future::pending::<()>()).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let mut sock = tokio::net::UnixStream::connect(&path).await.unwrap();
    sock.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);

    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    assert!(text.ends_with("over uds"), "got: {text}");

    server.abort();
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test]
async fn a_stale_socket_file_does_not_block_binding() {
    let path = std::env::temp_dir().join(format!("churust-stale-{}.sock", std::process::id()));
    // Simulate what a crashed process leaves behind.
    std::fs::write(&path, b"stale").unwrap();

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    let p = path.clone();
    let server = tokio::spawn(async move {
        let _ = churust_core::engine::serve_unix(app, p, std::future::pending::<()>()).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert!(
        tokio::net::UnixStream::connect(&path).await.is_ok(),
        "a leftover socket file must not make bind fail forever"
    );

    server.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn serves_on_several_addresses_at_once() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Two ephemeral ports, released before binding for real.
    let mut addrs = Vec::new();
    for _ in 0..2 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        addrs.push(l.local_addr().unwrap());
    }
    drop(addrs.clone());

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "multi" });
        })
        .build();

    let bound = addrs.clone();
    let server = tokio::spawn(async move {
        let _ = churust_core::engine::serve_many(app, bound, std::future::pending::<()>()).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    for addr in &addrs {
        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(
            format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.ends_with("multi"), "{addr} did not answer: {text}");
    }

    server.abort();
}

#[tokio::test]
async fn binding_nothing_is_an_error_rather_than_a_silent_no_op() {
    let app = Churust::server().build();
    let err = churust_core::engine::serve_many(app, vec![], std::future::pending::<()>())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
