//! The sharded engine must enforce the same admission bounds as the shared one.
//!
//! `serve_sharded` is a second accept loop, and the TLS branch was not the only
//! thing that could go missing from it. `max_connections` is a denial-of-service
//! bound: without it an attacker spends one TCP connect per unit of server
//! memory and file descriptors. A bound that silently stops applying on one of
//! two serving modes is worse than one that was never advertised, because the
//! configuration still claims it.
//!
//! Graceful drain is here for the same reason: a rolling restart that drops
//! in-flight requests is a correctness bug that only shows up in production.

use churust_core::{Call, Churust};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Start a sharded server on an ephemeral port. Returns the port and the
/// shutdown handle; dropping the handle stops the server and joins its threads.
fn start(build: impl FnOnce(churust_core::AppBuilder) -> churust_core::AppBuilder) -> Server {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = probe.local_addr().expect("an address").port();
    drop(probe);

    let builder = Churust::server().host("127.0.0.1").port(port);
    let app = build(builder)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                "slow"
            });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let addr = format!("127.0.0.1:{port}").parse().expect("an address");
    let handle = std::thread::spawn(move || {
        let _ = churust_core::engine::serve_sharded(app, vec![addr], 2, async move {
            let _ = rx.await;
        });
    });

    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Server {
                port,
                tx: Some(tx),
                handle: Some(handle),
            };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the sharded server never came up on {port}");
}

struct Server {
    port: u16,
    tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn stop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Send one request and read whatever comes back within `wait`.
fn request(sock: &mut TcpStream, path: &str, wait: Duration) -> String {
    sock.set_read_timeout(Some(wait)).expect("a read deadline");
    write!(sock, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").expect("write");
    let mut buf = [0u8; 1024];
    match sock.read(&mut buf) {
        Ok(n) => String::from_utf8_lossy(&buf[..n]).into_owned(),
        Err(_) => String::new(),
    }
}

#[test]
fn max_connections_is_enforced() {
    let mut server = start(|b| b.max_connections(1).keep_alive_ms(30_000));

    // The first connection takes the only slot and holds it: keep-alive is long,
    // so the slot is not released by answering.
    let mut first = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    let served = request(&mut first, "/", Duration::from_secs(3));
    assert!(
        served.contains("200"),
        "the first connection was not served: {served:?}"
    );

    // The kernel completes the handshake out of the backlog, so connecting still
    // succeeds — but the server must not *serve* this one while the cap is full.
    let mut second = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    let blocked = request(&mut second, "/", Duration::from_millis(700));
    assert!(
        blocked.is_empty(),
        "a second connection was served while max_connections was 1: {blocked:?}"
    );

    // Releasing the slot must let the waiting connection through, or the cap is
    // not a cap but a permanent ceiling.
    drop(first);
    let after = request(&mut second, "/", Duration::from_secs(5));
    assert!(
        after.contains("200"),
        "the queued connection was never served after a slot freed: {after:?}"
    );

    server.stop();
}

#[test]
fn an_unlimited_cap_serves_everything() {
    // `0` means unlimited, and the sharded acceptor must read it the same way.
    let mut server = start(|b| b.max_connections(0).keep_alive_ms(30_000));

    let mut socks: Vec<TcpStream> = (0..6)
        .map(|_| TcpStream::connect(("127.0.0.1", server.port)).expect("connect"))
        .collect();
    for (i, sock) in socks.iter_mut().enumerate() {
        let res = request(sock, "/", Duration::from_secs(3));
        assert!(
            res.contains("200"),
            "connection {i} was not served under an unlimited cap: {res:?}"
        );
    }

    server.stop();
}

#[test]
fn an_in_flight_request_finishes_across_shutdown() {
    // Graceful drain on the sharded path. The handler sleeps well past the
    // moment shutdown is signalled; the response must still arrive.
    let mut server = start(|b| b.shutdown_timeout_ms(10_000).keep_alive_ms(30_000));

    let mut sock = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read deadline");
    write!(sock, "GET /slow HTTP/1.1\r\nHost: x\r\n\r\n").expect("write");

    // Let the request reach the handler, then signal shutdown underneath it.
    std::thread::sleep(Duration::from_millis(150));
    server.stop();

    let mut buf = [0u8; 1024];
    let n = sock.read(&mut buf).expect("the in-flight response");
    let res = String::from_utf8_lossy(&buf[..n]);
    assert!(
        res.contains("200") && res.contains("slow"),
        "an in-flight request was dropped by shutdown instead of drained: {res:?}"
    );
}
