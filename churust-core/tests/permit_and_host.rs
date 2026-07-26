//! Three defects introduced while fixing earlier ones. Tests first, so each is
//! reproduced before it is fixed rather than after.
//!
//! 1. `Call::host` strips the port with `split(':')`, which shreds a bracketed
//!    IPv6 literal — so `guard::host` can never match an IPv6 request and falls
//!    through to the unguarded sibling. That is the exact failure the h2
//!    `:authority` fix existed to prevent, moved to a different address family.
//! 2. The idle watchdog calls `graceful_shutdown()` without arming the linger.
//!    HTTP/2 does not resolve on that alone, and with `winding_down` set every
//!    other branch is gated off, so the connection — and its `max_connections`
//!    permit — is held forever.
//! 3. An upgraded WebSocket holds a permit for the socket's whole life with no
//!    idle bound anywhere, so a handshake that then says nothing pins a permit
//!    indefinitely.

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

// ---------------------------------------------------------------- 1. host()

#[test]
fn host_keeps_an_ipv6_literal_intact() {
    use bytes::Bytes;
    use http::{header::HOST, HeaderMap, HeaderValue, Method};

    let with_host = |v: &str| {
        let mut h = HeaderMap::new();
        h.insert(HOST, HeaderValue::from_str(v).unwrap());
        Call::new(Method::GET, "/".parse().unwrap(), h, Bytes::new())
    };

    // Bracketed literal, with and without a port. The brackets are part of the
    // authority syntax, not of the host, so they come off with the port.
    assert_eq!(
        with_host("[2001:db8::1]:8443").host().as_deref(),
        Some("2001:db8::1")
    );
    assert_eq!(
        with_host("[2001:db8::1]").host().as_deref(),
        Some("2001:db8::1")
    );
    // Ordinary names keep working.
    assert_eq!(
        with_host("example.com:8443").host().as_deref(),
        Some("example.com")
    );
    assert_eq!(
        with_host("example.com").host().as_deref(),
        Some("example.com")
    );

    // And the HTTP/2 shape, where the authority lives in the URI.
    let h2 = Call::new(
        Method::GET,
        "https://[2001:db8::1]:8443/".parse().unwrap(),
        HeaderMap::new(),
        Bytes::new(),
    );
    assert_eq!(h2.host().as_deref(), Some("2001:db8::1"));
}

#[tokio::test]
async fn a_host_guard_matches_an_ipv6_request() {
    use churust_core::TestClient;

    let build = || {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "VHOST" })
                    .guard(churust_core::guard::host("2001:db8::1"));
                r.get("/", |_c: Call| async { "FALLBACK" });
            })
            .build()
    };

    let res = TestClient::new(build())
        .get("/")
        .header("host", "[2001:db8::1]:8443")
        .send()
        .await;
    assert_eq!(
        res.text(),
        "VHOST",
        "an IPv6 request fell through to the unguarded route"
    );
}

// ------------------------------------------------- 2. idle h2 permit release

#[tokio::test]
async fn an_idle_http2_connection_is_closed_and_returns_its_permit() {
    let (l, addr) = bound().await;
    let app = Churust::server()
        .max_connections(1)
        .keep_alive_ms(400)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // An h2c peer that completes the preface and then goes silent.
    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    const SETTINGS: &[u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
    let mut idle = tokio::net::TcpStream::connect(addr).await.unwrap();
    idle.write_all(PREFACE).await.unwrap();
    idle.write_all(SETTINGS).await.unwrap();
    let mut head = [0u8; 9];
    tokio::time::timeout(Duration::from_secs(2), idle.read_exact(&mut head))
        .await
        .expect("no SETTINGS reply")
        .expect("closed");

    // Well past the 400ms keep-alive, the permit must be back so an ordinary
    // client can be served. Without it the server is locked out permanently.
    let mut next = tokio::net::TcpStream::connect(addr).await.unwrap();
    next.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(4), next.read(&mut buf))
        .await
        .expect("the idle h2 connection never released its permit")
        .expect("read failed");
    assert!(String::from_utf8_lossy(&buf[..n]).contains("200"));
}

// ------------------------------------------------ 3. silent WebSocket permit

#[cfg(feature = "ws")]
#[tokio::test]
async fn a_silent_websocket_does_not_pin_a_permit_forever() {
    use churust_core::ws::WebSocketUpgrade;

    let (l, addr) = bound().await;
    let app = Churust::server()
        .max_connections(1)
        .ws_idle_timeout_ms(500)
        .routing(|r| {
            r.get("/ws", |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut sock| async move {
                    // A well-behaved echo server that simply never hears
                    // anything: it parks on recv for the socket's lifetime.
                    while let Some(Ok(msg)) = sock.recv().await {
                        if sock.send(msg).await.is_err() {
                            break;
                        }
                    }
                })
            });
            r.get("/plain", |_c: Call| async { "plain" });
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Complete the handshake, then say nothing at all.
    let (_ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("handshake");

    // The idle socket must be reaped and its permit returned.
    let mut next = tokio::net::TcpStream::connect(addr).await.unwrap();
    next.write_all(b"GET /plain HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(4), next.read(&mut buf))
        .await
        .expect("a silent WebSocket pinned the only permit indefinitely")
        .expect("read failed");
    assert!(String::from_utf8_lossy(&buf[..n]).contains("200"));
}

#[cfg(feature = "ws")]
#[tokio::test]
async fn an_active_websocket_is_not_reaped() {
    // The reaper must bound idleness, not lifetime: a socket in use stays up.
    use churust_core::ws::{Message, WebSocketUpgrade};
    use futures_util::{SinkExt, StreamExt};

    let (l, addr) = bound().await;
    let app = Churust::server()
        .ws_idle_timeout_ms(400)
        .routing(|r| {
            r.get("/ws", |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut sock| async move {
                    while let Some(Ok(msg)) = sock.recv().await {
                        if let Message::Close = msg {
                            break;
                        }
                        if sock.send(msg).await.is_err() {
                            break;
                        }
                    }
                })
            });
        })
        .build();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    // Keep talking across more than one idle period.
    for i in 0..4 {
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            format!("m{i}").into(),
        ))
        .await
        .unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap_or_else(|_| panic!("active socket reaped at message {i}"))
            .unwrap()
            .unwrap();
        assert_eq!(echoed.to_text().unwrap(), format!("m{i}"));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
