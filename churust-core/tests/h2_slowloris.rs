//! The slow-loris defence must cover both protocols served on the port.
//!
//! `header_read_timeout_ms` is documented as "the slow-loris defence" but was
//! applied only to `builder.http1()`. The HTTP/2 branch got a timer and no
//! deadline, and h2 keep-alive pings are disabled by default in hyper — so a
//! peer that completed the preface and then dribbled a partial HEADERS frame
//! was bounded by nothing but the idle watchdog at `keep_alive_ms` (75s by
//! default, against a documented 10s defence), while holding a connection
//! permit the whole time.

use churust_core::{Call, Churust};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn a_stalled_http2_peer_is_dropped_by_the_header_read_deadline() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .header_read_timeout_ms(400)
        // High, so the idle watchdog cannot be what closes this: the h2 bound
        // is what is under test.
        .keep_alive_ms(60_000)
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

    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    const SETTINGS: &[u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(PREFACE).await.unwrap();
    sock.write_all(SETTINGS).await.unwrap();

    // A HEADERS frame claiming 200 bytes, of which one is sent. The stream is
    // open and the header block never completes.
    let mut headers = vec![0x00, 0x00, 0xC8, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01];
    headers.push(0x82);
    sock.write_all(&headers).await.unwrap();

    let t0 = Instant::now();
    let mut buf = [0u8; 4096];
    loop {
        match tokio::time::timeout(Duration::from_secs(6), sock.read(&mut buf)).await {
            // Closed: the deadline did its job.
            Ok(Ok(0)) | Ok(Err(_)) => break,
            // Frames from the server (SETTINGS, PING, GOAWAY) — keep reading,
            // deliberately never acknowledging anything.
            Ok(Ok(_)) => continue,
            Err(_) => panic!("a stalled h2 peer was still connected after 6s"),
        }
    }
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "the stalled h2 connection took {elapsed:?} to close"
    );
}

#[tokio::test]
async fn a_responsive_http2_client_is_not_dropped() {
    // The bound must reach a dead peer, not a quiet one that is still there.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .header_read_timeout_ms(300)
        .keep_alive_ms(60_000)
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

    // A real h2 client, which answers pings, idling across several intervals.
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut send, conn) = h2::client::handshake(stream).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    tokio::time::sleep(Duration::from_millis(1_200)).await;

    let req = http::Request::builder().uri("/").body(()).unwrap();
    let (res, _) = send
        .send_request(req, true)
        .expect("connection was dropped");
    let res = tokio::time::timeout(Duration::from_secs(3), res)
        .await
        .expect("no response: the live connection was closed")
        .expect("response");
    assert_eq!(res.status(), 200);
}
