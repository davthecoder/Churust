//! `OPTIONS *` asks about the server, not about a resource.
//!
//! RFC 9110 §9.3.7 defines the asterisk-form request target as applying to the
//! server as a whole — it is how a client probes capabilities. Routing it as if
//! it were a path produced `404`, which claims the server does not exist.
//!
//! Driven over a socket rather than through `TestClient`: asterisk-form is a
//! property of the request line, and a client API that takes a path cannot
//! express it.

use churust_core::{Call, Churust};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bind an ephemeral port and keep the listener, so nothing can take the port
/// between discovering it and serving on it. Dropping it first is a race that
/// shows up as one test's client reaching another test's server.
async fn bound() -> (tokio::net::TcpListener, std::net::SocketAddr) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

async fn serve_and_ask(request_line: &str, register_routes: bool) -> String {
    let (l, addr) = bound().await;
    let app = Churust::server()
        .routing(move |r| {
            if register_routes {
                r.get("/a", |_c: Call| async { "a" });
                r.post("/b", |_c: Call| async { "b" });
                r.delete("/c/{id}", |_c: Call| async { "c" });
            }
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(request_line.as_bytes()).await.unwrap();
    let mut buf = [0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read failed");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

#[tokio::test]
async fn options_asterisk_reports_server_wide_capabilities() {
    let res = serve_and_ask("OPTIONS * HTTP/1.1\r\nHost: x\r\n\r\n", true).await;
    assert!(
        res.starts_with("HTTP/1.1 204"),
        "expected 204 for OPTIONS *, got: {res}"
    );

    let allow = res
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("allow:"))
        .unwrap_or_else(|| panic!("no Allow header in: {res}"))
        .to_ascii_uppercase();

    // Every method registered anywhere in the router, plus the implicit ones.
    for m in ["GET", "POST", "DELETE", "HEAD", "OPTIONS"] {
        assert!(allow.contains(m), "Allow missing {m}: {allow}");
    }
}

#[tokio::test]
async fn options_asterisk_still_answers_on_a_server_with_no_routes() {
    // A server with nothing registered still exists, and still speaks OPTIONS.
    let res = serve_and_ask("OPTIONS * HTTP/1.1\r\nHost: x\r\n\r\n", false).await;
    assert!(res.starts_with("HTTP/1.1 204"), "{res}");
    assert!(res.to_ascii_uppercase().contains("ALLOW: OPTIONS"), "{res}");
}

#[tokio::test]
async fn a_literal_asterisk_path_is_still_a_404() {
    // `/*` is an ordinary path that happens to contain an asterisk. Only the
    // asterisk-form target — no leading slash — is the server-wide probe.
    let res = serve_and_ask("OPTIONS /* HTTP/1.1\r\nHost: x\r\n\r\n", true).await;
    assert!(
        res.starts_with("HTTP/1.1 404"),
        "expected 404 for the path /*, got: {res}"
    );
}
