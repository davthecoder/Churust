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
//! is the only backstop, and at `keep_alive_ms(0)` it waits for a first response
//! before closing anything — so in this phase it is no backstop at all, which is
//! why the tests below set that value.

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

#[tokio::test]
async fn keep_alive_zero_closes_an_http2_connection_after_its_response() {
    // The h2 counterpart of `zero_disables_connection_reuse_entirely` in
    // keep_alive.rs, which drives a raw HTTP/1 socket and so covered only one of
    // the two protocols served on the port.
    //
    // `keep_alive_ms(0)` means answer and close. hyper implements that for
    // HTTP/1 through `keep_alive(false)` and has no h2 equivalent, and the idle
    // watchdog used to be switched off entirely at 0 — so an HTTP/2 connection
    // got neither, and was held for the life of the process. The strictest
    // setting available was therefore weaker than the 75s default: this exact
    // client, idling while answering keep-alive pings, kept its connection (and
    // its `max_connections` permit) forever.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .keep_alive_ms(0)
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

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut send, conn) = h2::client::handshake(stream).await.expect("h2 handshake");
    // A real h2 connection task, so pings are answered — the case a liveness
    // probe cannot close.
    let driver = tokio::spawn(conn);

    let req = http::Request::builder().uri("/").body(()).unwrap();
    let (res, _) = send.send_request(req, true).expect("send the request");
    let res = tokio::time::timeout(Duration::from_secs(3), res)
        .await
        .expect("no response")
        .expect("response");
    assert_eq!(res.status(), 200);

    // The server must now close of its own accord. The driver future resolves
    // when the connection ends, so awaiting it is the assertion.
    let ended = tokio::time::timeout(Duration::from_secs(5), driver).await;
    assert!(
        ended.is_ok(),
        "keep_alive_ms(0) left the HTTP/2 connection open after its response"
    );
}

#[tokio::test]
async fn keep_alive_zero_does_not_cut_off_a_slow_http2_handler() {
    // The companion to the test above: "answer and close" has to answer first. A
    // handler slower than any interval the loop might have used is busy, not
    // idle, and cutting its connection would be a self-inflicted truncation.
    //
    // This is also what makes the close a notification rather than a poll. A
    // timer would have to pick an interval and re-check on each tick — against a
    // one-second handler a 25ms interval costs about forty wakes, and
    // `request_timeout_ms` permits thirty seconds of that. Waking on the
    // request finishing is one wake, and it cannot fire early.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .keep_alive_ms(0)
        .routing(|r| {
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                "slow but complete"
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

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut send, conn) = h2::client::handshake(stream).await.expect("h2 handshake");
    let driver = tokio::spawn(conn);

    let req = http::Request::builder().uri("/slow").body(()).unwrap();
    let (res, _) = send.send_request(req, true).expect("send the request");
    let res = tokio::time::timeout(Duration::from_secs(5), res)
        .await
        .expect("the slow handler's connection was closed under it")
        .expect("response");
    assert_eq!(res.status(), 200);

    // Whole body, not a truncated one.
    let mut body = res.into_body();
    let mut got = Vec::new();
    while let Some(chunk) = body.data().await {
        got.extend_from_slice(&chunk.expect("a body chunk"));
    }
    assert_eq!(
        String::from_utf8(got).expect("utf-8"),
        "slow but complete",
        "the response body was cut short"
    );

    // And it still closes once the slow request is genuinely done.
    let ended = tokio::time::timeout(Duration::from_secs(5), driver).await;
    assert!(
        ended.is_ok(),
        "keep_alive_ms(0) left the connection open after the slow response"
    );
}

#[tokio::test]
async fn keep_alive_zero_waits_for_a_concurrent_http2_request() {
    // HTTP/2 multiplexes, so at `keep_alive_ms = 0` a fast request can finish —
    // and wake the close — while a slow one on the same connection is still
    // running. What must not happen is the slow one being cut short.
    //
    // Worth being straight about what this does and does not pin down. It does
    // not discriminate the `!busy()` condition in the close branch: removing that
    // condition keeps this test green, because `graceful_shutdown` finishes
    // in-flight HTTP/2 streams and the linger re-checks `busy()` before dropping
    // the connection. Those two are what actually prevent the truncation. This
    // test pins the *outcome* they produce, which is the thing a user depends on
    // and the thing that would break if either changed.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();

    let app = Churust::server()
        .keep_alive_ms(0)
        .routing(|r| {
            r.get("/fast", |_c: Call| async { "fast" });
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(800)).await;
                "slow but complete"
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

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut send, conn) = h2::client::handshake(stream).await.expect("h2 handshake");
    tokio::spawn(conn);

    // The slow one first, so it is already in flight when the fast one finishes.
    let slow_req = http::Request::builder().uri("/slow").body(()).unwrap();
    let (slow, _) = send.send_request(slow_req, true).expect("send the slow one");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let fast_req = http::Request::builder().uri("/fast").body(()).unwrap();
    let (fast, _) = send.send_request(fast_req, true).expect("send the fast one");

    let fast = tokio::time::timeout(Duration::from_secs(3), fast)
        .await
        .expect("no fast response")
        .expect("fast response");
    assert_eq!(fast.status(), 200);

    // The fast request has completed and its notification has fired. The slow
    // one must still be served, whole.
    let slow = tokio::time::timeout(Duration::from_secs(5), slow)
        .await
        .expect("the slow request's connection was closed when the fast one finished")
        .expect("slow response");
    assert_eq!(slow.status(), 200);

    let mut body = slow.into_body();
    let mut got = Vec::new();
    while let Some(chunk) = body.data().await {
        got.extend_from_slice(&chunk.expect("a body chunk"));
    }
    assert_eq!(
        String::from_utf8(got).expect("utf-8"),
        "slow but complete",
        "the concurrent slow response was cut short"
    );
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
    // `keep_alive_ms(0)` keeps the idle watchdog out of this phase: at 0 it
    // closes a connection once a response has gone out, and this peer never gets
    // that far. So nothing but the header deadline can close this connection,
    // which is the point — the watchdog was the only thing covering the
    // pre-protocol phase, and here it is deliberately not covering it.
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
