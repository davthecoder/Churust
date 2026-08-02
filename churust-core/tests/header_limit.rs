//! The HTTP/1 header-count limit, asserted over the wire.
//!
//! `tests/limits.rs` already checks that `max_headers` reaches
//! `app.config()`, and that is not the same claim: the number being stored is
//! not evidence that anything enforces it. Nothing enforces it in Churust's own
//! code — hyper's HTTP/1 parser does — and `TestClient` never runs that parser,
//! so no in-process test can see the limit at all.
//!
//! So the limit at the default value is enforced entirely by hyper, and
//! Churust's contribution is to pass a number through. If a hyper release
//! changed how `max_headers` is honoured — or if a refactor here stopped
//! calling the setter — every existing test would still pass and the server
//! would quietly accept unbounded headers. These are the tests that would
//! notice.

use churust_core::{Call, Churust};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Serve on an ephemeral port, returning it with a handle that shuts down on
/// drop.
///
/// The listener is bound here and handed over already-bound, so unlike the
/// sharded tests there is no window for another process to take the port.
fn start(max_headers: Option<usize>) -> (u16, Stop) {
    let mut builder = Churust::server().without_security_headers();
    if let Some(n) = max_headers {
        builder = builder.max_headers(n);
    }
    let app = builder
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello" });
        })
        .build();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("an address").port();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt");
            let _ = churust_core::engine::serve_on(app, listener, async move {
                let _ = rx.await;
            })
            .await;
        });
    });

    (
        port,
        Stop {
            tx: Some(tx),
            handle: Some(handle),
        },
    )
}

struct Stop {
    tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Stop {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Send a request carrying `total` headers, `Host` included, and return the
/// status line.
fn status_with_headers(port: u16, total: usize) -> String {
    let mut req = String::from("GET /hello HTTP/1.1\r\nHost: localhost\r\n");
    // `Host` is already one of them; the rest are filler that means nothing to
    // the router.
    for i in 1..total {
        req.push_str(&format!("X-Filler-{i}: v\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");

    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("a read deadline");
    sock.write_all(req.as_bytes()).expect("write the request");
    let mut out = Vec::new();
    // A refusal closes the connection, so a short read is the normal ending
    // here rather than a failure.
    let _ = sock.read_to_end(&mut out);
    String::from_utf8_lossy(&out)
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

#[test]
fn the_default_header_limit_is_a_hundred() {
    let (port, _stop) = start(None);

    // `Connection: close` is the hundred-and-first header on this one, so the
    // request that must be accepted carries 99 of its own plus `Host`.
    let ok = status_with_headers(port, 99);
    assert!(
        ok.starts_with("HTTP/1.1 200"),
        "99 headers is inside the default limit and must be served, got: {ok:?}"
    );

    let refused = status_with_headers(port, 200);
    assert!(
        !refused.starts_with("HTTP/1.1 200"),
        "200 headers is past the default limit and must be refused, got: {refused:?}"
    );
}

#[test]
fn an_explicit_limit_is_still_enforced() {
    // Below hyper's default, so a refusal here cannot be hyper's own limit
    // doing the work — it is evidence that the configured number is the one in
    // force.
    let (port, _stop) = start(Some(5));

    let ok = status_with_headers(port, 3);
    assert!(
        ok.starts_with("HTTP/1.1 200"),
        "3 headers is inside a limit of 5 and must be served, got: {ok:?}"
    );

    let refused = status_with_headers(port, 40);
    assert!(
        !refused.starts_with("HTTP/1.1 200"),
        "40 headers is past a limit of 5 and must be refused, got: {refused:?}"
    );
}

#[test]
fn a_limit_above_hypers_default_is_honoured() {
    // Above hyper's default, so a request the default would refuse has to be
    // served. This is the direction a dropped setter breaks silently: the
    // server keeps working, just with a limit nobody asked for.
    let (port, _stop) = start(Some(300));

    let ok = status_with_headers(port, 200);
    assert!(
        ok.starts_with("HTTP/1.1 200"),
        "200 headers is inside a limit of 300 and must be served, got: {ok:?}"
    );
}
