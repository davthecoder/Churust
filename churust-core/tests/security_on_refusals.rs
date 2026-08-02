//! The security header set must survive the responses the pipeline never sees.
//!
//! `SecurityHeaders` is installed at `Phase::Setup`, so anything the pipeline
//! returns already carries the set and the transport does not need to apply it
//! again — applying it twice cost a `HeaderMap` grow and a full rehash on every
//! request, because `apply_to` opens with a `reserve` that the second call
//! could not satisfy in place.
//!
//! The engine therefore applies the set only to responses that did *not* come
//! from the pipeline. That is the whole risk of the optimisation: each such
//! response is a separate `return` in `engine::respond`, and one that is missed
//! ships without the headers while every existing test stays green. There are
//! four of them and this file covers each, plus the ordinary path, plus the
//! absence of duplicates.

use churust_core::{Call, Churust};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Every header the default `SecurityHeaders` set sends over plaintext.
const EXPECTED: &[&str] = &[
    "x-content-type-options",
    "x-frame-options",
    "referrer-policy",
    "permissions-policy",
    "cross-origin-resource-policy",
];

fn start(request_timeout_ms: u64) -> (u16, Stop) {
    // Deliberately *not* `without_security_headers()`: this file is about the
    // default configuration, which is the one every deployment runs and the one
    // the benchmark app turns off.
    let app = Churust::server()
        .request_timeout_ms(request_timeout_ms)
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello" });
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                "never"
            });
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

/// Send a raw request and return the whole response, lowercased for matching.
fn raw(port: u16, request: &str) -> String {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    sock.set_read_timeout(Some(std::time::Duration::from_secs(15)))
        .expect("a read deadline");
    sock.write_all(request.as_bytes()).expect("write");
    let mut out = Vec::new();
    // A refusal closes the connection, so a short read is the normal ending.
    let _ = sock.read_to_end(&mut out);
    String::from_utf8_lossy(&out).to_lowercase()
}

fn assert_secured(res: &str, what: &str) {
    for header in EXPECTED {
        assert!(
            res.contains(&format!("{header}:")),
            "{what}: response is missing {header}. \
             engine::respond gained a `return` that reports the pipeline applied \
             the security set when it did not. Full response:\n{res}"
        );
    }
}

#[test]
fn security_headers_survive_the_engines_own_refusals() {
    let (port, _stop) = start(30_000);

    // 1. Oversized declared body — refused before dispatch on Content-Length.
    let too_large = raw(
        port,
        "POST /hello HTTP/1.1\r\nHost: localhost\r\nContent-Length: 99999999\r\n\r\n",
    );
    assert!(
        too_large.starts_with("http/1.1 413"),
        "expected 413, got: {too_large}"
    );
    assert_secured(&too_large, "413 payload too large");

    // The conflicting-framing refusal (RFC 9112 §6.3 rule 3, `Content-Length`
    // and `Transfer-Encoding` together) is deliberately not exercised here: it
    // cannot be reached through a socket on hyper 1.11, which strips the
    // `Content-Length` before Churust ever sees the pair. `framing_conformance`
    // owns that story and explains why the guard stays anyway — `hyper = "1"`
    // still admits pre-1.11 versions. Its `return` carries the same
    // `pipeline_applied = false` as the two below, by inspection rather than by
    // test, and that is the one gap in this file.

    // 2. No Host at all — RFC 9112 §3.2, refused before dispatch.
    let no_host = raw(port, "GET /hello HTTP/1.1\r\n\r\n");
    assert!(
        no_host.starts_with("http/1.1 400"),
        "expected 400, got: {no_host}"
    );
    assert_secured(&no_host, "400 missing host");
}

#[test]
fn a_timed_out_request_still_gets_the_security_headers() {
    // The timeout discards the pipeline's response *after* the middleware would
    // have run, so the substitute has never been through it. This is the one
    // non-obvious case, and the one most likely to be lost in a refactor.
    let (port, _stop) = start(80);

    let timed_out = raw(port, "GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(
        timed_out.starts_with("http/1.1 408"),
        "expected 408, got: {timed_out}"
    );
    assert_secured(&timed_out, "408 request timeout");
}

#[test]
fn an_ordinary_response_is_secured_exactly_once() {
    let (port, _stop) = start(30_000);

    let ok = raw(port, "GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(ok.starts_with("http/1.1 200"), "expected 200, got: {ok}");
    assert_secured(&ok, "200 ok");

    // The transport no longer re-applies what the pipeline already set. If both
    // ever run again, `apply_to` uses `entry().or_insert_with()` so the headers
    // would not duplicate — which is exactly why the waste was invisible, and
    // why this counts rather than trusting absence of breakage.
    for header in EXPECTED {
        let seen = ok.matches(&format!("{header}:")).count();
        assert_eq!(
            seen, 1,
            "{header} appears {seen} times, expected once:\n{ok}"
        );
    }
}
