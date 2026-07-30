//! A live WebSocket must not hold shutdown open for the whole grace period.
//!
//! An upgraded connection holds a drain token, so the shutdown *waits* for it.
//! Nothing told it to wind down: the connection loop that watches the drain
//! signal has already exited by then — hyper resolves an upgraded connection the
//! moment the `101` is dispatched, and the socket lives on in a detached task.
//!
//! So every shutdown cost the full `shutdown_timeout_ms` whenever a WebSocket was
//! live, which a rolling restart pays every time. And at `0`, which means "wait
//! as long as it takes", the drain never returned at all.

#![cfg(feature = "ws")]

use churust_core::{Call, Churust, WebSocketUpgrade};
use std::time::{Duration, Instant};

/// An app with one echo WebSocket route and the given drain budget.
fn app(shutdown_timeout_ms: u64) -> churust_core::App {
    Churust::server()
        // High, so the idle reaper cannot be what ends the socket: the drain is
        // what is under test.
        .ws_idle_timeout_ms(600_000)
        .shutdown_timeout_ms(shutdown_timeout_ms)
        .routing(|r| {
            r.get("/ws", |ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(msg)) = socket.recv().await {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                })
            });
            r.get("/plain", |_c: Call| async { "ok" });
        })
        .build()
}

/// Open a WebSocket to `addr` and exchange one message, so it is genuinely live.
async fn live_socket(
    addr: std::net::SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use futures_util::{SinkExt, StreamExt};
    let (mut sock, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("the websocket handshake");
    sock.send(tokio_tungstenite::tungstenite::Message::Text("hi".into()))
        .await
        .expect("send");
    let echoed = sock.next().await.expect("an echo").expect("a frame");
    assert_eq!(echoed.into_text().unwrap(), "hi");
    sock
}

#[tokio::test]
async fn a_live_websocket_does_not_hold_shutdown_for_the_whole_grace_period() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let app = app(4_000);
    let server = tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Held open across the shutdown, and deliberately never closed by the client
    // — the server is what has to let go.
    let _sock = live_socket(addr).await;

    let t0 = Instant::now();
    let _ = tx.send(());
    let done = tokio::time::timeout(Duration::from_secs(10), server).await;
    let elapsed = t0.elapsed();

    assert!(done.is_ok(), "the server never finished draining");
    assert!(
        elapsed < Duration::from_millis(3_000),
        "shutdown took {elapsed:?}: the live WebSocket burned the grace period \
         instead of being told to wind down"
    );
}

#[tokio::test]
async fn a_live_websocket_does_not_hang_shutdown_forever_at_timeout_zero() {
    // `shutdown_timeout_ms = 0` means "wait as long as it takes", so there is no
    // deadline to rescue this one: either the socket winds down or the process
    // never exits. This is the case a grace period cannot paper over.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let app = app(0);
    let server = tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _sock = live_socket(addr).await;

    let _ = tx.send(());
    let done = tokio::time::timeout(Duration::from_secs(10), server).await;
    assert!(
        done.is_ok(),
        "shutdown never returned: at timeout 0 a live WebSocket hangs the process"
    );
}

#[tokio::test]
async fn an_idle_http_connection_still_drains_promptly() {
    // The control. If this regressed too, the measurement above would be telling
    // us something about connections in general rather than about WebSockets.
    use tokio::io::AsyncWriteExt;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let app = app(4_000);
    let server = tokio::spawn(async move {
        churust_core::engine::serve_on(app, l, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET /plain HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let t0 = Instant::now();
    let _ = tx.send(());
    let done = tokio::time::timeout(Duration::from_secs(10), server).await;
    assert!(done.is_ok(), "the server never finished draining");
    assert!(
        t0.elapsed() < Duration::from_millis(3_000),
        "an idle HTTP connection should not burn the grace period either"
    );
}
