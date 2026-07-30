//! Message-framing conformance (RFC 9112 §6).
//!
//! hyper owns HTTP/1 framing, so none of this is Churust's implementation —
//! which is exactly why it is worth pinning. These are behaviours the framework
//! depends on and would otherwise notice only after a dependency upgrade
//! changed one of them. A failure here is a signal to read hyper's changelog,
//! not necessarily a bug in this repository.

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

/// Serve an echo-length app, send `raw` verbatim, and read what comes back.
async fn exchange(raw: &[u8]) -> String {
    let (l, addr) = bound().await;
    let app = Churust::server()
        .routing(|r| {
            r.post("/echo", |body: String| async move {
                format!("len={}", body.len())
            });
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
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(raw).await.unwrap();
    let mut buf = Vec::new();
    // Read to EOF where the server closes; otherwise take what is available.
    let _ = tokio::time::timeout(Duration::from_millis(800), sock.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn transfer_encoding_wins_over_content_length() {
    // RFC 9112 §6.3 rule 3: if a message has both a Transfer-Encoding and a
    // Content-Length, the Transfer-Encoding overrides. The mismatch is the
    // classic request-smuggling primitive — a proxy that believes one header
    // and an origin that believes the other disagree about where the next
    // request starts.
    //
    // The chunked body is 5 bytes; the Content-Length lies and says 99.
    let res = exchange(
        b"POST /echo HTTP/1.1\r\n\
          Host: x\r\n\
          Content-Length: 99\r\n\
          Transfer-Encoding: chunked\r\n\
          \r\n\
          5\r\nhello\r\n0\r\n\r\n",
    )
    .await;

    // What matters is the desync, not the status line, and two different
    // layers remove it depending on which hyper resolved.
    //
    // Churust's own guard (`engine::handle`) answers `400` when it can see both
    // headers. From hyper 1.11 it cannot: hyper strips the `Content-Length`
    // during parsing, so the guard's condition is never true and the request
    // reaches the handler framed by the chunked coding — but hyper also sets
    // `keep_alive = false` for exactly this message shape, so the connection
    // still closes. Both paths end the same way, and the property below is the
    // one that actually holds off the attack: whatever an intermediary in front
    // believed the body length to be, no leftover byte survives to be read as
    // the start of a second request.
    //
    // `hyper = "1"` still admits pre-1.11 versions, so the guard stays.
    assert!(
        res.to_ascii_lowercase().contains("connection: close"),
        "the connection must be closed, or leftover bytes become the next request: {res}"
    );
    let responses = res.matches("HTTP/1.1 ").count();
    assert_eq!(
        responses, 1,
        "one ambiguous message produced {responses} responses: {res}"
    );
    // If it was served rather than refused, the transfer coding must be what
    // framed it. Honouring the lying Content-Length instead would mean reading
    // 99 bytes for a 5-byte body — the desync itself.
    assert!(
        res.starts_with("HTTP/1.1 400") || res.contains("len=5"),
        "the Content-Length was framed on instead of the transfer coding: {res}"
    );
}

#[tokio::test]
async fn a_smuggled_second_request_is_not_served_from_one_message() {
    // The CL.TE smuggling shape: the Content-Length covers bytes that, read as
    // chunked, terminate early — leaving a second request line in the buffer.
    // Exactly one response must come back.
    let res = exchange(
        b"POST /echo HTTP/1.1\r\n\
          Host: x\r\n\
          Content-Length: 6\r\n\
          Transfer-Encoding: chunked\r\n\
          \r\n\
          0\r\n\r\n\
          GET / HTTP/1.1\r\nHost: x\r\n\r\n",
    )
    .await;

    let responses = res.matches("HTTP/1.1 ").count();
    assert!(
        responses <= 1,
        "one message produced {responses} responses — smuggled request served: {res}"
    );
}

#[tokio::test]
async fn duplicate_conflicting_content_lengths_are_rejected() {
    // RFC 9112 §6.3 rule 5: differing Content-Length values are unrecoverable.
    let res = exchange(
        b"POST /echo HTTP/1.1\r\n\
          Host: x\r\n\
          Content-Length: 5\r\n\
          Content-Length: 6\r\n\
          \r\n\
          hello",
    )
    .await;
    assert!(
        res.starts_with("HTTP/1.1 400") || res.is_empty(),
        "conflicting Content-Length values were not rejected: {res}"
    );
}

#[tokio::test]
async fn an_unrecognised_transfer_encoding_is_refused() {
    // RFC 9112 §6.1: a Transfer-Encoding the server cannot decode must not be
    // guessed at.
    let res = exchange(
        b"POST /echo HTTP/1.1\r\n\
          Host: x\r\n\
          Transfer-Encoding: bogus\r\n\
          \r\n\
          hello",
    )
    .await;
    assert!(
        !res.contains("len="),
        "an undecodable Transfer-Encoding was served as a body: {res}"
    );
}

#[tokio::test]
async fn a_well_formed_chunked_body_still_works() {
    // The tests above pass trivially if chunked is broken outright.
    let res = exchange(
        b"POST /echo HTTP/1.1\r\n\
          Host: x\r\n\
          Transfer-Encoding: chunked\r\n\
          \r\n\
          5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    )
    .await;
    assert!(res.contains("len=11"), "{res}");
}

#[tokio::test]
async fn a_buffered_response_is_framed_with_content_length() {
    // Anything that wraps the response body engine-wide — the in-flight guard,
    // for one — must stay transparent to `size_hint`. Re-streaming a buffered
    // body silently switches every response to chunked encoding, which loses
    // the length clients use to size a download and to satisfy a `HEAD`.
    let res = exchange(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
    let head = res.to_ascii_lowercase();
    assert!(
        head.contains("content-length: 2"),
        "a buffered body lost its Content-Length: {res}"
    );
    assert!(
        !head.contains("transfer-encoding: chunked"),
        "a known-length body must not be chunked: {res}"
    );
}

#[tokio::test]
async fn a_head_reply_still_reports_the_length() {
    // RFC 9110 §9.3.2: HEAD carries the headers GET would, and clients use the
    // length to size a resource before fetching it.
    let res = exchange(b"HEAD / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
    assert!(
        res.to_ascii_lowercase().contains("content-length: 2"),
        "HEAD lost Content-Length: {res}"
    );
}

#[tokio::test]
async fn transfer_encoding_first_is_refused_too() {
    // Header *order* is the attacker's choice, and the guard only ever saw one
    // of the two orderings: below hyper 1.11, hyper deletes the Content-Length
    // when it parses a Transfer-Encoding first, so `contains_key` was false and
    // the message was served — with the smuggled request answered. The
    // Content-Length-first ordering was refused, which made the gap look closed.
    let res = exchange(
        b"POST /echo HTTP/1.1\r\n\
          Host: x\r\n\
          Transfer-Encoding: chunked\r\n\
          Content-Length: 6\r\n\
          \r\n\
          0\r\n\r\n\
          GET / HTTP/1.1\r\nHost: x\r\n\r\n",
    )
    .await;

    let responses = res.matches("HTTP/1.1 ").count();
    assert_eq!(responses, 1, "the smuggled request was answered: {res}");
    assert!(
        res.to_ascii_lowercase().contains("connection: close"),
        "the connection stayed reusable after an ambiguous message: {res}"
    );
}

#[tokio::test]
async fn an_http11_request_without_a_host_is_refused() {
    // RFC 9112 §3.2. A request with no `Host` leaves `Call::host` and every
    // `guard::host` route deciding on nothing.
    let got = exchange(b"GET / HTTP/1.1\r\n\r\n").await;
    assert!(got.contains("400"), "{got}");
    assert!(
        got.to_ascii_lowercase().contains("connection: close"),
        "the connection must not be reused after a message this ambiguous: {got}"
    );
}

#[tokio::test]
async fn an_http11_request_with_two_hosts_is_refused() {
    // The dangerous half: an intermediary routes or authorizes on one of them
    // and this server would have served the other.
    let addr = bound().await;
    let got = exchange(b"GET / HTTP/1.1\r\nHost: a.example\r\nHost: b.example\r\n\r\n").await;
    assert!(got.contains("400"), "{got}");
}

#[tokio::test]
async fn an_http11_request_with_one_host_is_still_served() {
    // The guard against refusing the ordinary request.
    let got = exchange(b"GET / HTTP/1.1\r\nHost: a.example\r\n\r\n").await;
    assert!(got.contains("200"), "{got}");
}

#[tokio::test]
async fn an_http10_request_without_a_host_is_still_served() {
    // HTTP/1.0 predates the requirement, so the check must not reach it.
    let got = exchange(b"GET / HTTP/1.0\r\n\r\n").await;
    assert!(got.contains("200"), "{got}");
}

#[tokio::test]
async fn an_absolute_form_target_is_served_without_a_host_field() {
    // The authority is already in the target, and it is the one that wins.
    let got = exchange(b"GET http://a.example/ HTTP/1.1\r\n\r\n").await;
    assert!(got.contains("200"), "{got}");
}

