//! The accept loop must bound what the process takes on.
//!
//! The listen backlog bounds what the kernel queues; these bound what the
//! server serves. Without them the only limit is what the OS will hand out,
//! and the failure mode is the process dying rather than clients waiting.

use churust_core::{Call, Churust};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn free_addr() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

/// Send a request and read the first chunk of the response.
async fn request(sock: &mut tokio::net::TcpStream, path: &str) -> std::io::Result<String> {
    sock.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await?;
    let mut buf = [0u8; 1024];
    let n = sock.read(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

#[tokio::test]
async fn connections_beyond_the_cap_wait_rather_than_being_served() {
    let addr = free_addr();
    let app = Churust::server()
        .max_connections(1)
        // Long enough that the first connection is unambiguously still holding
        // the only slot when the second one asks for it.
        .keep_alive_ms(60_000)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // First connection takes the only slot and keeps it (keep-alive is long).
    let mut first = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert!(request(&mut first, "/").await.unwrap().contains("200"));

    // The kernel completes the TCP handshake from the backlog, so connecting
    // succeeds — but the server must not serve this connection yet.
    let mut second = tokio::net::TcpStream::connect(addr).await.unwrap();
    let served = tokio::time::timeout(Duration::from_millis(600), request(&mut second, "/")).await;
    assert!(
        served.is_err(),
        "a second connection was served while the cap was 1: {served:?}"
    );

    // Releasing the first slot must let the waiting connection through, or the
    // cap would be a deadlock rather than a limit.
    drop(first);
    let served = tokio::time::timeout(Duration::from_secs(3), request(&mut second, "/"))
        .await
        .expect("waiting connection was never served after a slot freed");
    assert!(served.unwrap().contains("200"));
}

#[tokio::test]
async fn zero_means_unlimited() {
    let addr = free_addr();
    let app = Churust::server()
        .max_connections(0)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Several concurrent, simultaneously-open connections all get served.
    let mut socks = Vec::new();
    for _ in 0..8 {
        socks.push(tokio::net::TcpStream::connect(addr).await.unwrap());
    }
    for (i, sock) in socks.iter_mut().enumerate() {
        let res = tokio::time::timeout(Duration::from_secs(2), request(sock, "/"))
            .await
            .unwrap_or_else(|_| panic!("connection {i} was not served under an unlimited cap"))
            .unwrap();
        assert!(res.contains("200"), "connection {i}: {res}");
    }
}

#[tokio::test]
async fn shutdown_works_while_the_connection_budget_is_saturated() {
    // The cap must not outrank the shutdown signal. Waiting for a permit parks
    // indefinitely once every slot is held, so acquiring one outside the
    // shutdown race meant a saturated server never noticed SIGTERM and had to
    // be SIGKILLed — a worse failure than the unbounded drain the budget was
    // added alongside.
    let addr = free_addr();
    let app = Churust::server()
        .max_connections(1)
        .keep_alive_ms(60_000)
        .shutdown_timeout_ms(1_000)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Occupy the only slot with a live keep-alive connection.
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert!(request(&mut sock, "/").await.unwrap().contains("200"));

    let _ = tx.send(());
    let outcome = tokio::time::timeout(Duration::from_secs(5), server).await;
    assert!(
        outcome.is_ok(),
        "serve() never returned: shutdown is blocked while the budget is full"
    );
}

/// The knobs must survive the config layering, or they are decoration.
#[test]
fn the_new_limits_are_configurable_through_the_environment() {
    let mut cfg = churust_core::Config::default();
    cfg.apply_env(|k| match k {
        "CHURUST_SERVER_MAX_CONNECTIONS" => Some("4096".into()),
        "CHURUST_SERVER_MAX_TLS_HANDSHAKES" => Some("32".into()),
        "CHURUST_SERVER_TLS_HANDSHAKE_TIMEOUT_MS" => Some("2500".into()),
        _ => None,
    });
    assert_eq!(cfg.server.max_connections, 4096);
    assert_eq!(cfg.server.max_tls_handshakes, 32);
    assert_eq!(cfg.server.tls_handshake_timeout_ms, 2500);

    let app = Churust::server().with_config(cfg).build();
    assert_eq!(app.config().max_connections, 4096);
    assert_eq!(app.config().max_tls_handshakes, 32);
    assert_eq!(app.config().tls_handshake_timeout_ms, 2500);
}

#[tokio::test]
async fn serve_many_refuses_to_start_when_any_address_fails_to_bind() {
    // Binding used to happen inside each spawned accept loop, so a failure was
    // not observed until after the shutdown signal: the process served the
    // addresses that worked, reported nothing, and surfaced the error only on
    // the way out. A server that silently came up on half its addresses is
    // exactly what this is supposed to prevent.
    let good = free_addr();
    // 192.0.2.0/24 is TEST-NET-1 (RFC 5737) and is not assigned locally.
    let bad: std::net::SocketAddr = "192.0.2.1:9".parse().unwrap();

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        churust_core::engine::serve_many(app, vec![good, bad], async {
            let _ = rx.await;
        }),
    )
    .await;

    let result =
        outcome.expect("serve_many did not return until shutdown; it should abort at bind");
    assert!(
        result.is_err(),
        "serve_many reported success despite an unbindable address"
    );

    // And it must not have left the good address serving.
    assert!(
        tokio::net::TcpStream::connect(good).await.is_err(),
        "the good address is still accepting after an aborted start"
    );
}
