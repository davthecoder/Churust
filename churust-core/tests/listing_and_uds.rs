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

#[cfg(unix)]
#[tokio::test]
async fn a_stale_socket_node_left_by_a_crash_is_still_unlinked() {
    // The realistic leftover is a *socket* inode with nobody listening on it,
    // not the regular file the test above writes. Binding must still succeed.
    let path = std::env::temp_dir().join(format!("churust-crash-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let orphan = std::os::unix::net::UnixListener::bind(&path).unwrap();
    drop(orphan); // dropping does not unlink: the node outlives the listener

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
        "a socket node whose owner crashed must not make bind fail forever"
    );

    server.abort();
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test]
async fn a_live_socket_is_not_hijacked_by_a_second_bind() {
    let path = std::env::temp_dir().join(format!("churust-hijack-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "first" });
        })
        .build();

    let p = path.clone();
    let first = tokio::spawn(async move {
        let _ = churust_core::engine::serve_unix(app, p, std::future::pending::<()>()).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let second = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "second" });
        })
        .build();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        churust_core::engine::serve_unix(second, &path, std::future::pending::<()>()),
    )
    .await;

    first.abort();
    let _ = std::fs::remove_file(&path);

    let err = match outcome {
        Ok(Ok(())) => panic!("the second bind returned instead of refusing"),
        Ok(Err(e)) => e,
        Err(_) => panic!("the second bind took over a live socket instead of refusing"),
    };
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn shutting_down_does_not_unlink_a_socket_someone_else_bound() {
    let path = std::env::temp_dir().join(format!("churust-succ-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ours" });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let p = path.clone();
    let server = tokio::spawn(async move {
        churust_core::engine::serve_unix(app, p, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Somebody else takes the path over while we are still up: our node is
    // gone and a different inode now answers there.
    std::fs::remove_file(&path).unwrap();
    let successor = std::os::unix::net::UnixListener::bind(&path).unwrap();

    let _ = tx.send(());
    server.await.unwrap().unwrap();

    assert!(
        tokio::net::UnixStream::connect(&path).await.is_ok(),
        "our shutdown deleted the successor's socket, leaving the path dead"
    );

    drop(successor);
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
async fn an_address_added_with_bind_is_still_served_after_build() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // `AppBuilder::start` honoured `bind`, but `build()` dropped the extra
    // addresses on the floor, so the overwhelmingly common shape — build once,
    // then serve with a custom shutdown signal — listened on the configured
    // `host:port` alone and said nothing about the rest.
    let mut addrs = Vec::new();
    for _ in 0..2 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        addrs.push(l.local_addr().unwrap());
    }

    let app = Churust::server()
        .host(addrs[0].ip().to_string())
        .port(addrs[0].port())
        .bind(addrs[1].to_string())
        .routing(|r| {
            r.get("/", |_c: Call| async { "both" });
        })
        .build();

    let server = tokio::spawn(async move {
        let _ = app.start_with_shutdown(std::future::pending::<()>()).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    for addr in &addrs {
        let mut sock = tokio::net::TcpStream::connect(addr)
            .await
            .unwrap_or_else(|e| panic!("{addr} was never bound: {e}"));
        sock.write_all(
            format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.ends_with("both"), "{addr} did not answer: {text}");
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

#[cfg(unix)]
#[tokio::test]
async fn a_tls_configured_app_refuses_to_serve_over_a_unix_socket() {
    // A Unix socket carries no TLS, and this listener never consulted the
    // setting — so the app served plaintext while `apply_security_headers` kept
    // asserting HSTS, because that gate reads `config.tls.is_some()` rather than
    // the transport. A cleartext service telling clients it is HTTPS-only.
    //
    // Refused rather than downgraded: the two settings contradict each other and
    // silently honouring one is how it went unnoticed.
    let dir = std::env::temp_dir().join("churust-uds-tls-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("refused.sock");
    let _ = std::fs::remove_file(&path);

    let app = Churust::server()
        .tls("/nonexistent/cert.pem", "/nonexistent/key.pem")
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    // Bounded, because the failure mode without the refusal is that `serve_unix`
    // happily serves forever against a shutdown that never fires. An assertion
    // that reports is worth more than a test run that hangs.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        churust_core::engine::serve_unix(app, &path, std::future::pending::<()>()),
    )
    .await
    .expect("serve_unix kept serving: a TLS-configured app was allowed onto a cleartext socket");
    let err = outcome.expect_err("a TLS-configured app must not serve cleartext on a Unix socket");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("HSTS"),
        "the error should say why it matters, got: {err}"
    );
    assert!(
        !path.exists(),
        "the refusal must happen before anything is bound"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_unix_listener_still_serves_with_a_custom_backlog() {
    // The backlog is now ours to choose on this listener too — tokio's
    // `UnixListener::bind` used the platform default however `backlog` was set.
    // The value itself is a kernel hint and not observable from here, so what
    // this pins is that binding through a socket did not break serving.
    let dir = std::env::temp_dir().join("churust-uds-backlog-test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("backlog.sock");
    let _ = std::fs::remove_file(&path);

    let app = Churust::server()
        .backlog(64)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let p = path.clone();
    let server = tokio::spawn(async move {
        churust_core::engine::serve_unix(app, &p, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    sock.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut got = String::new();
    sock.read_to_string(&mut got).await.unwrap();
    assert!(got.contains("200"), "{got}");
    assert!(got.contains("ok"), "{got}");

    let _ = tx.send(());
    let _ = server.await;
    let _ = std::fs::remove_file(&path);
}
