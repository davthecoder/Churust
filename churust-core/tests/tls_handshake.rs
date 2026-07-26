#![cfg(feature = "tls")]
//! A TLS handshake must be bounded in time.
//!
//! `header_read_timeout_ms` cannot cover this: until the handshake finishes
//! there is no HTTP layer to time out. Without a separate bound, a client that
//! completes the TCP handshake and then dribbles bytes holds a connection —
//! and a task — open indefinitely. That is slowloris with the defence bypassed.

use churust_core::{Call, Churust};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bind an ephemeral port and keep the listener, so nothing can take the port
/// between discovering it and serving on it. Dropping it first is a race that
/// shows up as one test's client reaching another test's server.
async fn bound() -> (tokio::net::TcpListener, std::net::SocketAddr) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

/// Write a throwaway self-signed cert and key, returning their paths.
///
/// Generated per test rather than checked in: key material in a repository is
/// a habit worth not starting, and these are valid only for this process.
fn self_signed(dir: &std::path::Path) -> (String, String) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    std::fs::write(&key_path, cert.key_pair.serialize_pem()).unwrap();
    (
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("churust-tls-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn a_stalled_handshake_is_dropped_at_the_deadline() {
    let dir = temp_dir("stall");
    let (cert, key) = self_signed(&dir);
    let (l, addr) = bound().await;

    let app = Churust::server()
        .tls(cert, key)
        .tls_handshake_timeout_ms(400)
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

    // Complete the TCP handshake, then send one plausible-looking TLS record
    // header and nothing more. A server without a handshake deadline waits
    // forever for the rest.
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(&[0x16, 0x03, 0x01, 0x00, 0xff])
        .await
        .unwrap();

    let t0 = Instant::now();
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf)).await;
    let elapsed = t0.elapsed();

    match read {
        // Either shape is a drop; which one depends on the platform.
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(n)) => panic!("expected a drop, read {n} bytes"),
        Err(_) => panic!("stalled handshake still open after 3s with a 400ms deadline"),
    }
    assert!(
        elapsed < Duration::from_millis(2_000),
        "handshake was dropped after {elapsed:?}, far past the 400ms deadline"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_real_tls_request_still_works() {
    // The deadline must not be so eager that it breaks legitimate clients —
    // the whole test above passes trivially if TLS never works at all.
    let dir = temp_dir("ok");
    let (cert, key) = self_signed(&dir);
    let (l, addr) = bound().await;

    let app = Churust::server()
        .tls(cert.clone(), key)
        .routing(|r| {
            r.get("/", |_c: Call| async { "secure" });
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

    // Trust exactly the certificate we generated; nothing else.
    let pem = std::fs::read(&cert).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for der in rustls_pemfile::certs(&mut pem.as_slice()) {
        roots.add(der.unwrap()).unwrap();
    }
    let client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(client_cfg));
    let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("handshake");
    tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut body = Vec::new();
    tls.read_to_end(&mut body).await.unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("secure"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_deadline_covers_the_queue_and_not_just_the_handshake() {
    // `tls_handshake_timeout_ms` exists to bound how long a peer that has not
    // proved it can speak TLS may hold a *connection* permit. Starting the
    // clock after `max_tls_handshakes` has been acquired bounds only the
    // cryptography, which is the part that was never the problem: the queue in
    // front of it is unbounded, so N stalled ClientHellos are held for N/limit
    // deadlines rather than for one, and the connection budget drains at the
    // rate handshakes expire.
    //
    // One permit and eight stalled peers makes the difference visible: covered,
    // every peer is gone one deadline after it arrived; uncovered, the last one
    // waits for the seven in front of it to expire first.
    let dir = temp_dir("queue");
    let (cert, key) = self_signed(&dir);
    let (l, addr) = bound().await;

    const DEADLINE_MS: u64 = 300;
    const PEERS: usize = 8;

    let app = Churust::server()
        .tls(cert, key)
        .max_tls_handshakes(1)
        .tls_handshake_timeout_ms(DEADLINE_MS)
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

    // Every peer opens and stalls the same way: a plausible TLS record header
    // and then nothing. Only one of them can be handshaking at a time.
    let mut socks = Vec::with_capacity(PEERS);
    for _ in 0..PEERS {
        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(&[0x16, 0x03, 0x01, 0x00, 0xff])
            .await
            .unwrap();
        socks.push(sock);
    }

    // Measured from the moment they are all queued, so the assertion is about
    // the depth of the queue rather than about how long connecting took.
    let t0 = Instant::now();
    for (i, sock) in socks.iter_mut().enumerate() {
        let mut buf = [0u8; 64];
        match tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => {}
            Ok(Ok(n)) => panic!("peer {i} expected a drop, read {n} bytes"),
            Err(_) => panic!("peer {i} was still queued after 10s"),
        }
    }
    let elapsed = t0.elapsed();

    // Serialised, the eighth peer leaves at roughly PEERS * DEADLINE_MS
    // (~2.4s). Covered, every peer leaves at roughly DEADLINE_MS.
    assert!(
        elapsed < Duration::from_millis(DEADLINE_MS * 4),
        "{PEERS} stalled handshakes took {elapsed:?} to clear against a {DEADLINE_MS}ms \
         deadline — the queue is not covered by it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
