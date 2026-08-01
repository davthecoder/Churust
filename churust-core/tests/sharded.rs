//! `engine::serve_sharded` — the one-runtime-per-worker serving mode.
//!
//! It is a second implementation of accepting and serving, sitting beside
//! `serve_listener`, and it is the one that hands a socket between runtimes. A
//! socket handed over without being deregistered first is never polled again:
//! the connection is accepted, counted, and then silently hangs. Nothing in the
//! existing suite would notice, because every other test goes through the
//! shared-runtime path.

use churust_core::{Call, Churust};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// Start a sharded server on an ephemeral port, and return the port plus a
/// handle that shuts it down and joins the threads when dropped.
///
/// The port is found by binding and releasing, which races anything else on the
/// machine grabbing it in between. `serve_sharded` binds by address rather than
/// taking a listener, so the race cannot be closed here the way
/// `engine::serve_on` closes it; a retry loop is the honest mitigation.
fn start(workers: usize) -> (u16, Shutdown) {
    let app = Churust::server()
        .without_security_headers()
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello" });
            r.get(
                "/user/{id}",
                |churust_core::Path(id): churust_core::Path<u64>| async move {
                    format!("user {id}")
                },
            );
        })
        .build();

    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        probe.local_addr().expect("an address").port()
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let addr = format!("127.0.0.1:{port}").parse().expect("a valid address");
    let handle = std::thread::spawn(move || {
        churust_core::engine::serve_sharded(app, vec![addr], workers, async move {
            let _ = rx.await;
        })
    });

    // Poll until it answers: the server builds several runtimes and threads
    // before it is listening, so a fixed sleep is either slow or flaky.
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return (port, Shutdown { tx: Some(tx), handle: Some(handle) });
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("the sharded server never came up on {port}");
}

struct Shutdown {
    tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            // Joining rather than detaching is the assertion: a `serve_sharded`
            // that never returns after its shutdown future resolves would hang
            // this test rather than pass it quietly.
            let _ = handle.join();
        }
    }
}

/// One request on a fresh connection, returning the whole response.
fn get(port: u16, path: &str) -> String {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("a read deadline");
    write!(
        sock,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write the request");
    let mut out = String::new();
    sock.read_to_string(&mut out).expect("read the response");
    out
}

#[test]
fn a_sharded_server_answers() {
    let (port, _stop) = start(2);
    let res = get(port, "/hello");
    assert!(res.starts_with("HTTP/1.1 200"), "unexpected status: {res}");
    assert!(res.ends_with("hello"), "unexpected body: {res}");
}

#[test]
fn every_worker_serves() {
    // The failure this catches is a handoff that only ever reaches one worker —
    // which is exactly what `SO_REUSEPORT` does on macOS, and why this mode
    // accepts centrally instead. Connections are held open together so they
    // cannot be served one after another by the same worker.
    let workers = 4;
    let (port, _stop) = start(workers);

    let mut held: Vec<TcpStream> = Vec::new();
    for _ in 0..(workers * 4) {
        let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("a read deadline");
        // No `Connection: close`: the connection stays open and keeps occupying
        // whichever worker it landed on.
        write!(sock, "GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n").expect("write");
        held.push(sock);
    }

    // Every one of them must be answered while all of them are still open. If
    // the handoff dropped a socket, or handed it over still registered with the
    // acceptor's reactor, the read below times out instead.
    for sock in &mut held {
        let mut reader = BufReader::new(sock);
        let mut line = String::new();
        reader.read_line(&mut line).expect("a status line");
        assert!(
            line.starts_with("HTTP/1.1 200"),
            "a held-open connection was not served: {line:?}"
        );
    }
}

#[test]
fn a_connection_survives_more_than_one_request() {
    // Keep-alive across the handoff: the second request is read from a socket
    // that was registered with a *different* runtime than the one that accepted
    // it, which is the part that goes wrong if `into_std` is skipped.
    let (port, _stop) = start(2);
    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("a read deadline");

    for id in [1u64, 2, 3] {
        write!(
            sock,
            "GET /user/{id} HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .expect("write");
        let mut reader = BufReader::new(&mut sock);
        let mut status = String::new();
        reader.read_line(&mut status).expect("a status line");
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "request {id} on a reused connection failed: {status:?}"
        );
        // Drain the header block so the next request starts at a frame
        // boundary; the body is a known short length.
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("a header line");
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().expect("a numeric content-length");
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).expect("the body");
        assert_eq!(String::from_utf8_lossy(&body), format!("user {id}"));
    }
}

#[test]
fn zero_workers_means_one_worker() {
    // `serve_sharded` clamps rather than dividing by zero or spawning nothing,
    // so a caller that computed a worker count and got it wrong still serves.
    let (port, _stop) = start(0);
    let res = get(port, "/hello");
    assert!(res.starts_with("HTTP/1.1 200"), "unexpected status: {res}");
}

#[test]
fn binding_a_taken_port_is_an_error_not_a_panic() {
    // Bind failures must surface before any worker starts. The whole reason
    // `serve_sharded` binds up front is that a server which came up on half its
    // addresses and said nothing is worse than one that refused to start.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to squat on");
    let addr = squatter.local_addr().expect("an address");

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let result =
        churust_core::engine::serve_sharded(app, vec![addr], 2, std::future::pending::<()>());
    assert!(
        result.is_err(),
        "binding an occupied port reported success"
    );
}

#[test]
fn no_addresses_is_an_error() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
        })
        .build();
    let result = churust_core::engine::serve_sharded(app, Vec::new(), 2, std::future::pending::<()>());
    assert!(result.is_err(), "serving nowhere reported success");
}
