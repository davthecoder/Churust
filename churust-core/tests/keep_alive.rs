//! `keep_alive_ms` must bound how long an idle connection is held.
//!
//! An idle keep-alive connection costs a file descriptor and a task. Without a
//! bound, opening connections and going quiet is the cheapest way to exhaust a
//! server — `backlog` does not help, because these are accepted, not pending.

use churust_core::{Call, Churust};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn free_addr() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

/// Start a server and return its address plus the shutdown trigger.
async fn start(keep_alive_ms: u64) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let addr = free_addr();
    let app = Churust::server()
        .keep_alive_ms(keep_alive_ms)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                "slow"
            });
        })
        .build();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        churust_core::engine::serve(app, addr, async {
            let _ = rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    (addr, tx)
}

async fn one_request(sock: &mut tokio::net::TcpStream, path: &str) -> String {
    sock.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = [0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read failed");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

#[tokio::test]
async fn an_idle_connection_is_closed_after_the_keep_alive_period() {
    let (addr, _tx) = start(300).await;
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert!(one_request(&mut sock, "/").await.contains("200"));

    // Go quiet for well over the configured period.
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_millis(1_500), sock.read(&mut buf)).await;
    match read {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => panic!("expected close, read {n} unexpected bytes"),
        Ok(Err(e)) => panic!("expected a clean close, got {e}"),
        Err(_) => panic!("connection still open 1.5s into a 300ms keep-alive"),
    }
}

#[tokio::test]
async fn a_reused_connection_is_not_closed_while_it_stays_active() {
    // The fix must bound *idle* time, not connection lifetime: a client that
    // keeps asking must keep being served on the same socket.
    let (addr, _tx) = start(500).await;
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    for i in 0..4 {
        let res = one_request(&mut sock, "/").await;
        assert!(res.contains("200"), "request {i} on a reused socket: {res}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn a_slow_request_is_not_cut_off_by_the_idle_timeout() {
    // Idle means no request in flight. A handler that takes longer than the
    // keep-alive period is busy, not idle.
    let (addr, _tx) = start(200).await;
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let res = one_request(&mut sock, "/slow").await;
    assert!(
        res.contains("slow"),
        "a 600ms handler was cut off by a 200ms idle timeout: {res}"
    );
}

#[tokio::test]
async fn zero_disables_connection_reuse_entirely() {
    let (addr, _tx) = start(0).await;
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let res = one_request(&mut sock, "/").await;
    assert!(res.contains("200"), "{res}");
    // With reuse off, hyper answers and closes.
    assert!(
        res.contains("connection: close") || {
            let mut buf = [0u8; 64];
            matches!(
                tokio::time::timeout(Duration::from_millis(500), sock.read(&mut buf)).await,
                Ok(Ok(0))
            )
        },
        "keep_alive_ms(0) must not reuse the connection: {res}"
    );
}
