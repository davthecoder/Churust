//! The slow-loris defence must cover every phase of a connection.
//!
//! `header_read_timeout_ms` is documented as "the slow-loris defence" but was
//! applied only to `builder.http1()`. The HTTP/2 branch got a timer and no
//! deadline, and h2 keep-alive pings are disabled by default in hyper — so a
//! peer that completed the preface and then dribbled a partial HEADERS frame
//! was bounded by nothing but the idle watchdog at `keep_alive_ms` (75s by
//! default, against a documented 10s defence), while holding a connection
//! permit the whole time.
//!
//! There is a third phase, before either protocol exists. `auto::Builder`
//! sniffs up to the 24 bytes of the HTTP/2 preface to decide which connection
//! to build, and both deadlines above are properties of a connection that has
//! not been built yet. hyper-util's `ReadVersion` future holds no timer of its
//! own, so a peer that sends nothing at all — or 23 of the 24 preface bytes —
//! parks there holding a connection permit and a drain token. The idle watchdog
//! is the only backstop, and `keep_alive_ms(0)` disables even that.

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

/// Read until the peer closes, or fail after `patience`.
async fn time_until_closed(sock: &mut tokio::net::TcpStream, patience: Duration) -> Duration {
    let t0 = Instant::now();
    let mut buf = [0u8; 4096];
    loop {
        match tokio::time::timeout(patience, sock.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return t0.elapsed(),
            // Whatever the server says on the way out — keep reading.
            Ok(Ok(_)) => continue,
            Err(_) => panic!("still connected after {patience:?}"),
        }
    }
}

/// Serve `app` on an ephemeral port and hand back the address.
///
/// The listener is bound before the server task starts and never dropped, so
/// no other test can take the port in between.
async fn serve(app: churust_core::App) -> std::net::SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, std::future::pending::<()>()).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    addr
}

#[tokio::test]
async fn a_peer_that_sends_nothing_is_dropped_before_a_protocol_is_chosen() {
    // `keep_alive_ms(0)` disables the idle watchdog outright, so nothing but
    // the header deadline can close this connection. That is the point: the
    // watchdog was the only thing covering this phase, and at 0 it is not
    // there at all.
    let app = Churust::server()
        .header_read_timeout_ms(400)
        .keep_alive_ms(0)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let addr = serve(app).await;

    // Not one byte. hyper-util is parked in `ReadVersion` waiting to see
    // whether this is an HTTP/2 preface, and neither protocol's deadline
    // exists until it decides.
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let elapsed = time_until_closed(&mut sock, Duration::from_secs(6)).await;
    assert!(
        elapsed < Duration::from_secs(3),
        "a silent connection was held for {elapsed:?}"
    );
}

#[tokio::test]
async fn a_peer_that_stalls_inside_the_preface_is_dropped() {
    // The sneakier shape: 23 of the 24 preface bytes. Every byte matches, so
    // `ReadVersion` keeps waiting for the last one rather than falling through
    // to HTTP/1, and the connection is parked for as long as the peer likes.
    let app = Churust::server()
        .header_read_timeout_ms(400)
        .keep_alive_ms(0)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let addr = serve(app).await;

    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(&PREFACE[..PREFACE.len() - 1]).await.unwrap();
    let elapsed = time_until_closed(&mut sock, Duration::from_secs(6)).await;
    assert!(
        elapsed < Duration::from_secs(3),
        "a connection stalled inside the preface was held for {elapsed:?}"
    );
}
