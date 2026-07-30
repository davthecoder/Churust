//! HTTP/3 end to end: a real QUIC client against a real QUIC listener.
//!
//! The unit tests cover the pieces in isolation. This file is the one that
//! would notice if the transport did not actually carry a request.

#![cfg(feature = "http3")]

use bytes::{Buf, Bytes};
use churust_core::{Body, Call, Churust, Response, TestClient};
use http::StatusCode;
use std::net::SocketAddr;
use std::sync::Arc;

/// A self-signed certificate, generated in process so no key material is
/// checked into the repository.
struct Cert {
    chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
}

fn self_signed() -> Cert {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("a self-signed certificate for localhost");
    Cert {
        chain: vec![generated.cert.der().clone()],
        key: rustls::pki_types::PrivateKeyDer::Pkcs8(generated.key_pair.serialize_der().into()),
    }
}

/// Start the library's own h3 listener for `app` and return the address it
/// bound.
///
/// This deliberately goes through `Http3Server` rather than driving quinn and
/// h3 directly: a harness that reimplemented the serving loop would prove that
/// quinn works, not that this crate does.
async fn serve(app: churust_core::App, cert: &Cert) -> SocketAddr {
    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(cert.chain.clone(), cert.key.clone_key())
        .expect("the generated pair should be accepted");
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic config");
    let config = quinn::ServerConfig::with_crypto(Arc::new(quic));

    let server = churust_core::http3::Http3Server::bind("127.0.0.1:0".parse().unwrap(), config)
        .expect("a UDP socket");
    let addr = server.local_addr().expect("a local address");
    tokio::spawn(server.serve(app));
    addr
}

/// Complete a QUIC handshake and hand the connection back still open.
///
/// The h3 layer is deliberately not built on top: what this establishes is the
/// state *just after* the handshake the budget guards, which is what a test of
/// that budget needs to hold open.
async fn connect_only(addr: SocketAddr, cert: &Cert) -> quinn::Connection {
    let mut roots = rustls::RootCertStore::empty();
    for der in &cert.chain {
        roots.add(der.clone()).expect("trust the test certificate");
    }
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("a client socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));

    endpoint
        .connect(addr, "localhost")
        .expect("a connect attempt")
        .await
        .expect("a completed handshake")
}

/// Send one h3 request and return the status and body.
async fn request(
    addr: SocketAddr,
    cert: &Cert,
    method: http::Method,
    path: &str,
    body: Option<Bytes>,
) -> (StatusCode, String) {
    let (head, body) = request_head(addr, cert, method, path, body).await;
    (head.status(), body)
}

/// As [`request`], but hands back the whole response head.
///
/// Most tests here care only about the status and the body; the ones that check
/// what a response is *decorated* with need the header map, and reading it off
/// a discarded head is not possible.
async fn request_head(
    addr: SocketAddr,
    cert: &Cert,
    method: http::Method,
    path: &str,
    body: Option<Bytes>,
) -> (http::Response<()>, String) {
    request_head_with_headers(addr, cert, method, path, &[], body).await
}

/// As [`request_head`], but with request headers of your own.
///
/// Separate because one thing a test needs to be able to say is what the client
/// *claimed* about its body, which is not always what it then sent.
async fn request_head_with_headers(
    addr: SocketAddr,
    cert: &Cert,
    method: http::Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Bytes>,
) -> (http::Response<()>, String) {
    let mut roots = rustls::RootCertStore::empty();
    for der in &cert.chain {
        roots.add(der.clone()).expect("trust the test certificate");
    }
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("a client socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));

    let connection = endpoint
        .connect(addr, "localhost")
        .expect("a connect attempt")
        .await
        .expect("a completed handshake");

    let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("an h3 client");

    // The driver has to be polled for the connection to make progress, and it
    // only finishes when the connection does, so it runs alongside.
    let drive = tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });

    let uri: http::Uri = format!("https://localhost{path}").parse().unwrap();
    let mut builder = http::Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(()).unwrap();

    let mut stream = send.send_request(req).await.expect("a request stream");
    if let Some(body) = body {
        stream.send_data(body).await.expect("send the body");
    }
    stream.finish().await.expect("finish the request");

    let response = stream.recv_response().await.expect("a response");
    let mut out = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.expect("a body chunk") {
        out.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }

    drop(send);
    let _ = drive.await;

    (response, String::from_utf8(out).expect("a utf-8 body"))
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
            r.get("/users/{id}", |churust_core::Path(id): churust_core::Path<u64>| async move {
                format!("user #{id}")
            });
            r.post("/echo", |body: String| async move { body });
            r.get("/missing-on-purpose", |_c: Call| async {
                (StatusCode::NOT_FOUND, "nope")
            });
            r.get("/whoami", |c: Call| async move {
                match c.peer_addr() {
                    Some(a) => format!("peer {}", a.ip()),
                    None => "peer unknown".to_string(),
                }
            });
            r.get("/streamed", |_c: Call| async {
                let chunks = futures_util::stream::iter(
                    (0..4).map(|i| Ok::<_, std::io::Error>(Bytes::from(format!("part{i} ")))),
                );
                Response::stream("text/plain", Body::from_stream(chunks))
            });
            // A body that dies partway, the way a database cursor does: the
            // status line has long since gone out, so the only way to say so
            // is to leave the stream unfinished.
            r.get("/truncated", |_c: Call| async {
                let chunks = futures_util::stream::iter(vec![
                    Ok::<_, std::io::Error>(Bytes::from_static(b"part0 ")),
                    Err(std::io::Error::other("the cursor died")),
                ]);
                Response::stream("text/plain", Body::from_stream(chunks))
            });
        })
        .build()
}

/// The handshake budget releases its slot once the handshake is done, rather
/// than holding it for as long as the connection lives.
///
/// With a cap of one, an unreleased slot would mean the first connection had to
/// *end* before a second could shake hands. That is the failure this guards:
/// the slot is scoped to the handshake, and `max_connections` is the bound that
/// takes over afterwards.
#[tokio::test]
async fn a_completed_handshake_releases_its_slot_in_the_budget() {
    let cert = self_signed();
    let app = Churust::server()
        .max_tls_handshakes(1)
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
        })
        .build();
    let addr = serve(app, &cert).await;

    // Held open for the rest of the test, so its handshake slot would still be
    // taken if the slot outlived the handshake.
    let held = connect_only(addr, &cert).await;

    let answered = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        request(addr, &cert, http::Method::GET, "/hello", None),
    )
    .await
    .expect("a second handshake should not have to wait for the first connection to close");

    assert_eq!(answered.0, StatusCode::OK);
    assert_eq!(answered.1, "hello over quic");
    drop(held);
}

/// The target authority reaches the handler over h3, so `Call::host` works.
///
/// HTTP/3 carries the authority in `:authority`, which lands in the URI, and
/// there is no `Host` field beside it. Normalising the target to origin form
/// dropped the authority, and with it the only signal `Call::host` had.
#[tokio::test]
async fn the_target_host_reaches_the_handler_over_h3() {
    let cert = self_signed();
    let app = Churust::server()
        .routing(|r| {
            r.get("/whoishost", |c: Call| async move {
                match c.host() {
                    Some(h) => format!("host {h}"),
                    None => "host unknown".to_string(),
                }
            });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/whoishost", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "host localhost",
        "the `:authority` should survive into `Call::host`"
    );
}

/// A host guard selects the right handler over h3.
///
/// The dangerous shape, and the reason this is not merely a missing `404`: a
/// host-scoped route with an unguarded sibling on the same method and path is
/// the ordinary virtual-host arrangement. With no authority to match on, the
/// guard failed and the fallback answered — the wrong tenant's handler, with a
/// `200`.
#[tokio::test]
async fn a_host_guarded_route_wins_over_its_fallback_on_h3() {
    let cert = self_signed();
    let app = Churust::server()
        .routing(|r| {
            r.get("/tenant", |_c: Call| async { "the localhost tenant" });
            r.guard(churust_core::guard::host("localhost"));
            r.get("/tenant", |_c: Call| async { "public fallback" });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/tenant", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "the localhost tenant",
        "the host-guarded route should have matched, not the fallback"
    );
}

/// A guard for a host this request is not for still does not match.
///
/// The companion to the test above: seeding the `Host` field must not make every
/// host guard pass.
#[tokio::test]
async fn a_guard_for_another_host_does_not_match_on_h3() {
    let cert = self_signed();
    let app = Churust::server()
        .routing(|r| {
            r.get("/tenant", |_c: Call| async { "someone else's tenant" });
            r.guard(churust_core::guard::host("elsewhere.example"));
        })
        .build();
    let addr = serve(app, &cert).await;

    let (status, _) = request(addr, &cert, http::Method::GET, "/tenant", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a guard naming another host must not match this request"
    );
}

/// A body the client declares too large is refused before any of it is read.
///
/// The discriminating case is a declaration with nothing behind it: counting
/// chunks as they arrive never sees a byte, so `read_body` alone would hand the
/// handler an empty body and answer `200` for a request the server had already
/// said it would refuse. The TCP path makes the same check for the same reason.
#[tokio::test]
async fn a_declared_oversized_body_is_refused_without_being_read() {
    let cert = self_signed();
    let app = Churust::server()
        .max_body_bytes(16)
        .routing(|r| {
            r.post("/echo", |body: String| async move { body });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (head, _) = request_head_with_headers(
        addr,
        &cert,
        http::Method::POST,
        "/echo",
        &[("content-length", "4096")],
        // Nothing sent: the declaration is the whole of what the server has to
        // go on, which is the point.
        None,
    )
    .await;

    assert_eq!(head.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// A declared body within the cap is served normally.
#[tokio::test]
async fn a_declared_body_within_the_cap_is_served() {
    let cert = self_signed();
    let app = Churust::server()
        .max_body_bytes(4096)
        .routing(|r| {
            r.post("/echo", |body: String| async move { body });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (head, body) = request_head_with_headers(
        addr,
        &cert,
        http::Method::POST,
        "/echo",
        &[("content-length", "5")],
        Some(Bytes::from_static(b"hello")),
    )
    .await;

    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(body, "hello");
}

/// A handshake budget of `0` means unlimited, as it does on TCP.
#[tokio::test]
async fn a_zero_handshake_budget_does_not_refuse_the_handshake() {
    let cert = self_signed();
    let app = Churust::server()
        .max_tls_handshakes(0)
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/hello", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello over quic");
}

/// A handshake deadline that is set does not cut short a handshake that
/// completes well inside it.
#[tokio::test]
async fn a_handshake_inside_the_deadline_is_served() {
    let cert = self_signed();
    let app = Churust::server()
        .tls_handshake_timeout_ms(10_000)
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/hello", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello over quic");
}

/// A disabled handshake deadline leaves the handshake unbounded rather than
/// expiring it immediately.
#[tokio::test]
async fn a_zero_handshake_deadline_does_not_expire_the_handshake() {
    let cert = self_signed();
    let app = Churust::server()
        .tls_handshake_timeout_ms(0)
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
        })
        .build();
    let addr = serve(app, &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/hello", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello over quic");
}

#[tokio::test]
async fn a_get_is_answered_over_quic() {
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/hello", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello over quic");
}

#[tokio::test]
async fn routing_and_extractors_work_the_same_over_h3() {
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/users/42", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "user #42",
        "a handler must not be able to tell which transport it answered"
    );
}

#[tokio::test]
async fn a_request_body_arrives() {
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let (status, body) = request(
        addr,
        &cert,
        http::Method::POST,
        "/echo",
        Some(Bytes::from("sent over quic")),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "sent over quic");
}

#[tokio::test]
async fn a_status_other_than_200_survives_the_transport() {
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/missing-on-purpose", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "nope");
}

#[tokio::test]
async fn a_streamed_response_arrives_whole() {
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let (status, body) = request(addr, &cert, http::Method::GET, "/streamed", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "part0 part1 part2 part3 ");
}

#[tokio::test]
async fn a_missing_certificate_file_is_an_error_not_a_panic() {
    let err = churust_core::http3::server_config_from_pem("/nope/cert.pem", "/nope/key.pem")
        .expect_err("missing files must not build a config");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn alt_svc_points_tcp_clients_at_h3() {
    let app = Churust::server()
        .advertise_http3(8443)
        .routing(|r| {
            r.get("/", |_c: Call| async { "ok" });
            r.get("/missing", |_c: Call| async { StatusCode::NOT_FOUND });
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(
        client.get("/").send().await.header("alt-svc"),
        Some("h3=\":8443\"; ma=86400")
    );
    assert_eq!(
        client.get("/missing").send().await.header("alt-svc"),
        Some("h3=\":8443\"; ma=86400"),
        "a client that only ever sees an error should still learn about h3"
    );
}

#[tokio::test]
async fn the_peer_address_reaches_the_handler_over_h3() {
    // The h3 path passed an empty extension map, so `Call::peer_addr` was
    // always `None` — per-IP rate limiting keyed every h3 request as one
    // client, and audit logs recorded nothing, while the same request over TCP
    // carried the address. `advertise_http3` steers clients here, so the gap
    // widened as h3 adoption grew.
    let cert = self_signed();
    let addr = serve(app(), &cert).await;
    let (status, body) = request(addr, &cert, http::Method::GET, "/whoami", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.starts_with("peer 127.0.0.1"),
        "the peer address did not reach the handler: {body}"
    );
}

/// A stream that opens and then says nothing is dropped at the header deadline.
///
/// The h3 counterpart of `a_stalled_http2_peer_is_dropped_by_the_header_read_deadline`
/// in `h2_slowloris.rs`. `accept` hands back a stream before a frame has been
/// read, so without a deadline a peer could open streams up to
/// `h2_max_concurrent_streams`, send no HEADERS frame on any of them, and hold a
/// task and an h3 resolver each for the life of the process: the request timeout
/// is armed only after the headers arrive, and the QUIC idle timer is reset by
/// any keepalive packet.
///
/// The stall is deliberately *silent* — the stream stays open, unlike the
/// finished-empty stream in the test below, which h3 itself reports.
#[tokio::test]
async fn a_stream_that_never_sends_headers_is_dropped_at_the_header_deadline() {
    let cert = self_signed();
    let app = Churust::server()
        .header_read_timeout_ms(300)
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
        })
        .build();
    let addr = serve(app, &cert).await;

    let mut roots = rustls::RootCertStore::empty();
    for der in &cert.chain {
        roots.add(der.clone()).expect("trust the test certificate");
    }
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("a client socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));

    let connection = endpoint
        .connect(addr, "localhost")
        .expect("a connect attempt")
        .await
        .expect("a completed handshake");
    let raw = connection.clone();

    let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("an h3 client");
    let drive = tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });

    // A *partial* HEADERS frame, then silence — the same shape as the HTTP/2
    // slow-loris the TCP engine bounds. It has to be partial rather than absent:
    // quinn does not surface a bidi stream to the server until bytes arrive on
    // it, so a stream with nothing written is a stream the server never sees and
    // never spends anything on. These two bytes are frame type `0x01` (HEADERS)
    // and a payload length of 16 that never follows, which is enough to make the
    // server accept the stream, spawn its task and wait — the state that had no
    // deadline on it.
    let (mut stalled, mut stalled_recv) = raw.open_bi().await.expect("a raw bidi stream");
    stalled
        .write_all(&[0x01, 0x10])
        .await
        .expect("dribble a partial HEADERS frame");

    // The server should drop its side once the deadline elapses, which the client
    // observes as the receiving half ending.
    let reclaimed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stalled_recv.read_to_end(64),
    )
    .await;
    assert!(
        reclaimed.is_ok(),
        "the stalled stream was never reclaimed: no header deadline fired"
    );

    // And the deadline must be per stream: a sibling on the same connection is
    // still served, and the connection itself is untouched.
    let uri: http::Uri = "https://localhost/hello".parse().unwrap();
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(())
        .unwrap();
    let mut stream = send.send_request(req).await.expect("a request stream");
    stream.finish().await.expect("finish the request");
    let response = stream.recv_response().await.expect("a response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        raw.close_reason().is_none(),
        "the header deadline closed the whole connection: {:?}",
        raw.close_reason()
    );

    drop(send);
    let _ = drive.await;
}

#[tokio::test]
async fn a_stream_that_never_sends_headers_does_not_kill_the_connection() {
    // h3 multiplexes every request of a connection onto that one QUIC
    // connection, so the blast radius of a per-stream failure is the whole
    // client. `resolve_request` reports a *stream* error — a header block over
    // `max_field_section_size`, or a stream that ends before its headers ever
    // arrive — and propagating it out of the accept loop dropped the
    // `h3::server::Connection`, whose `Drop` closes the QUIC connection with
    // H3_NO_ERROR. One malformed stream took every sibling request with it.
    //
    // The trigger here is a raw bidi stream opened and finished without a byte
    // on it, which h3 surfaces as `StreamError::StreamError` with
    // H3_REQUEST_INCOMPLETE.
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let mut roots = rustls::RootCertStore::empty();
    for der in &cert.chain {
        roots.add(der.clone()).expect("trust the test certificate");
    }
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("a client socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));

    let connection = endpoint
        .connect(addr, "localhost")
        .expect("a connect attempt")
        .await
        .expect("a completed handshake");

    // Kept before h3 takes its own clone, so the raw QUIC connection can be
    // used as a side channel and inspected afterwards. Both halves are the
    // same connection, which is the whole point of the test.
    let raw = connection.clone();

    let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("an h3 client");
    let drive = tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });

    // The failing stream goes first, so the server has already handled it by
    // the time the real request arrives. A sibling still in flight would hold
    // the connection open on its own and could mask the defect.
    let (mut doomed, _recv) = raw.open_bi().await.expect("a raw bidi stream");
    doomed.finish().expect("finish it empty");

    // The sibling request: this is what must still be served.
    let uri: http::Uri = "https://localhost/hello".parse().unwrap();
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(())
        .unwrap();
    let mut stream = send
        .send_request(req)
        .await
        .expect("the connection was torn down by the empty stream");
    stream.finish().await.expect("finish the request");
    let response = stream
        .recv_response()
        .await
        .expect("no response: the empty stream killed the connection");
    let mut out = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.expect("a body chunk") {
        out.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        String::from_utf8(out).expect("a utf-8 body"),
        "hello over quic"
    );
    // Names the defect directly: the server must not have closed the QUIC
    // connection over one unusable stream.
    assert!(
        raw.close_reason().is_none(),
        "the connection was closed: {:?}",
        raw.close_reason()
    );

    drop(send);
    let _ = drive.await;
}

#[tokio::test]
async fn a_request_body_cut_short_by_a_reset_is_not_served_as_complete() {
    // The mirror image of the response case below, and the more dangerous
    // direction: here the truncation is in what the *handler* is given. A
    // client announced 5000 bytes, sent 1200, and reset the stream. h3 reports
    // that as `StreamError::RemoteTerminate` from `recv_data`, but the read
    // loop was `while let Ok(Some(..))`, which cannot tell an error from the
    // clean `Ok(None)` end of body — both just end the loop. So the partial
    // payload was handed to the route as if it were the whole request, the
    // handler ran on it, and a 200 went back. Anything with a side effect —
    // an upload stored, a batch of records imported — committed 1200 bytes of
    // a 5000-byte body and told the client it had all arrived.
    //
    // A reset request cannot be answered with a 400 either: a client that
    // resets its request stream is cancelling, and the status would be thrown
    // away even if it arrived. The only report the peer can actually observe
    // is the server refusing to complete the stream, so that is what this
    // asserts, and it holds whether or not the 1200 bytes were delivered
    // before the reset overtook them.
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let mut roots = rustls::RootCertStore::empty();
    for der in &cert.chain {
        roots.add(der.clone()).expect("trust the test certificate");
    }
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("a client socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));

    let connection = endpoint
        .connect(addr, "localhost")
        .expect("a connect attempt")
        .await
        .expect("a completed handshake");

    let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("an h3 client");
    let drive = tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });

    let uri: http::Uri = "https://localhost/echo".parse().unwrap();
    let req = http::Request::builder()
        .method(http::Method::POST)
        .uri(uri)
        .header("content-length", "5000")
        .body(())
        .unwrap();
    let mut stream = send.send_request(req).await.expect("a request stream");
    stream
        .send_data(Bytes::from(vec![b'x'; 1200]))
        .await
        .expect("send the first 1200 bytes");

    // Long enough for the 1200 bytes to be read on the far side, so the case
    // under test is the interesting one: a server that has real content in
    // hand and has to decide it is not a request.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Not `finish`: the remaining 3800 bytes are abandoned, which is
    // RESET_STREAM on the wire.
    stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);

    let answered = match stream.recv_response().await {
        // Refused before the head reached us.
        Err(_) => None,
        Ok(response) => {
            let mut out = Vec::new();
            loop {
                match stream.recv_data().await {
                    Ok(Some(mut chunk)) => {
                        out.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()))
                    }
                    // A complete-looking answer to an incomplete request.
                    Ok(None) => break Some((response.status(), out.len())),
                    // Refused mid-response, which is also a refusal.
                    Err(_) => break None,
                }
            }
        }
    };

    assert!(
        answered.is_none(),
        "a body cut short at 1200 of 5000 bytes was answered as a complete request: \
         status and echoed length were {answered:?}"
    );

    drop(send);
    let _ = drive.await;
}

#[tokio::test]
async fn a_streamed_body_that_fails_partway_does_not_look_complete() {
    // Once the head is on the wire the status cannot be taken back, so the
    // only honest report of a body that died partway is a stream the peer can
    // see was aborted. Returning early instead left the `RequestStream` to be
    // dropped, and quinn finishes a stream when it drops — so the client
    // received a well-formed 200 with a short body and no error at all, and
    // would store the truncated answer as the whole thing.
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let mut roots = rustls::RootCertStore::empty();
    for der in &cert.chain {
        roots.add(der.clone()).expect("trust the test certificate");
    }
    let mut tls = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("a client socket");
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));

    let connection = endpoint
        .connect(addr, "localhost")
        .expect("a connect attempt")
        .await
        .expect("a completed handshake");

    let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("an h3 client");
    let drive = tokio::spawn(async move { std::future::poll_fn(|cx| driver.poll_close(cx)).await });

    let uri: http::Uri = "https://localhost/truncated".parse().unwrap();
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri)
        .body(())
        .unwrap();
    let mut stream = send.send_request(req).await.expect("a request stream");
    stream.finish().await.expect("finish the request");

    // Where the abort surfaces depends on what has reached the wire when the
    // reset goes out. With a body this small nothing has been flushed, so the
    // reset takes the head with it and `recv_response` is what fails; with a
    // body large enough to have been sent, the head arrives and the failure
    // lands on a later `recv_data`. Both are correct — asserting one of them
    // would be asserting a flush timing. What must never happen is the third
    // outcome: a complete-looking response whose body is short.
    let mut out = Vec::new();
    let ended_cleanly = match stream.recv_response().await {
        // Aborted before the head reached us.
        Err(_) => false,
        Ok(response) => {
            assert_eq!(response.status(), StatusCode::OK);
            loop {
                match stream.recv_data().await {
                    Ok(Some(mut chunk)) => {
                        out.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()))
                    }
                    // A normal end of body: the client has no way to know it
                    // is short.
                    Ok(None) => break true,
                    // Aborted mid-body, which is the signal we want.
                    Err(_) => break false,
                }
            }
        }
    };

    assert!(
        !ended_cleanly,
        "a body that failed after {} byte(s) was reported as a complete response",
        out.len()
    );

    drop(send);
    let _ = drive.await;
}

#[tokio::test]
async fn a_response_over_h3_carries_hsts_without_a_tls_section() {
    // `SecurityHeaders` gates HSTS on whether the *builder* was given a
    // certificate, because over TCP that is the only way to know the client is
    // talking TLS to this process rather than plaintext to a proxy in front of
    // it. QUIC removes the doubt: there is no plaintext HTTP/3, and
    // `server_config_from_pem` pins TLS 1.3 outright — yet `http3::serve` takes
    // its cert and key as arguments and never touches `config.tls`, so the gate
    // read `false` and the one transport that *cannot* be plaintext was the one
    // that never announced HSTS. The module's own example builds the app with
    // no `AppBuilder::tls` call at all, so this was the documented way to use
    // it.
    let cert = self_signed();
    let addr = serve(app(), &cert).await;

    let (head, _) = request_head(addr, &cert, http::Method::GET, "/hello", None).await;
    assert_eq!(
        head.headers()
            .get("strict-transport-security")
            .map(|v| v.to_str().unwrap()),
        Some("max-age=31536000"),
        "an h3 response is TLS 1.3 by construction and must be pinned as such"
    );
}

#[tokio::test]
async fn a_server_that_disables_hsts_is_still_obeyed_over_h3() {
    // The other half of the one above: h3 knowing it is TLS must widen the
    // gate, not bypass the setting behind it.
    let app = Churust::server()
        .security_headers(churust_core::SecurityHeaders::new().hsts(None))
        .routing(|r| {
            r.get("/hello", |_c: Call| async { "hello over quic" });
        })
        .build();
    let cert = self_signed();
    let addr = serve(app, &cert).await;

    let (head, _) = request_head(addr, &cert, http::Method::GET, "/hello", None).await;
    assert!(
        head.headers().get("strict-transport-security").is_none(),
        "hsts(None) was overridden by the transport"
    );
}

#[tokio::test]
async fn a_refusal_before_dispatch_over_h3_carries_the_security_headers() {
    // The h3 mirror of the engine's
    // `a_refusal_before_dispatch_still_carries_the_security_headers`: an
    // oversized body is refused in `serve_request` before the pipeline runs, so
    // the middleware that adds these never saw the response.
    let app = Churust::server()
        .max_body_bytes(16)
        .routing(|r| {
            r.post("/echo", |body: String| async move { body });
        })
        .build();
    let cert = self_signed();
    let addr = serve(app, &cert).await;

    let (head, _) = request_head(
        addr,
        &cert,
        http::Method::POST,
        "/echo",
        Some(Bytes::from(vec![b'x'; 4096])),
    )
    .await;

    assert_eq!(head.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let headers = head.headers();
    assert_eq!(
        headers
            .get("x-content-type-options")
            .map(|v| v.to_str().unwrap()),
        Some("nosniff"),
        "the h3 refusal shipped bare"
    );
    assert_eq!(
        headers.get("x-frame-options").map(|v| v.to_str().unwrap()),
        Some("DENY")
    );
    assert_eq!(
        headers
            .get("strict-transport-security")
            .map(|v| v.to_str().unwrap()),
        Some("max-age=31536000")
    );
}
