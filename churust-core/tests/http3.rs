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

/// Send one h3 request and return the status and body.
async fn request(
    addr: SocketAddr,
    cert: &Cert,
    method: http::Method,
    path: &str,
    body: Option<Bytes>,
) -> (StatusCode, String) {
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
    let req = http::Request::builder()
        .method(method)
        .uri(uri)
        .body(())
        .unwrap();

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

    (
        response.status(),
        String::from_utf8(out).expect("a utf-8 body"),
    )
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
