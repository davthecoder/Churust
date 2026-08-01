#![cfg(feature = "tls")]
//! `run_sharded` must not serve an application's TLS port in plaintext.
//!
//! The sharded engine is a second accept path, and it was written without the
//! TLS branch the shared one has. An application that configured a certificate
//! and then chose `run_sharded` for throughput got a server that answered
//! ordinary HTTP on the port it believed was HTTPS — no error, no warning, and
//! every request and response in clear text on the wire.
//!
//! That is the worst shape a security defect can take: the configuration says
//! the traffic is encrypted, the code silently disagrees, and nothing in the
//! application's own tests would notice, because a plaintext client talking to
//! a plaintext server works perfectly.

use churust_core::{Call, Churust};
use std::io::{Read, Write};
use std::net::TcpStream;

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
    let dir =
        std::env::temp_dir().join(format!("churust-sharded-tls-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Start a TLS-configured app under the sharded engine on an ephemeral port.
fn start_tls_sharded() -> (u16, tokio::sync::oneshot::Sender<()>) {
    let dir = temp_dir("plaintext");
    let (cert, key) = self_signed(&dir);

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let app = Churust::server()
        .host("127.0.0.1")
        .port(port)
        .tls(cert, key)
        .routing(|r| {
            r.get("/", |_c: Call| async { "secret" });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    std::thread::spawn(move || {
        let _ = churust_core::engine::serve_sharded(app, vec![addr], 2, async move {
            let _ = rx.await;
        });
    });

    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return (port, tx);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("the sharded server never came up on {port}");
}

#[test]
fn a_tls_port_never_answers_plaintext_http() {
    let (port, _stop) = start_tls_sharded();

    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    sock.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .expect("a read deadline");
    // An ordinary, unencrypted HTTP request. A TLS server sees this as a
    // malformed ClientHello and closes; a plaintext server answers it.
    sock.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut out = String::new();
    let _ = sock.read_to_string(&mut out);

    assert!(
        !out.starts_with("HTTP/"),
        "the TLS port answered an unencrypted HTTP request with {:?} — the \
         application configured a certificate and its traffic is in clear text",
        out.lines().next().unwrap_or("")
    );
    assert!(
        !out.contains("secret"),
        "the handler's body reached a plaintext client: {out:?}"
    );
}

#[test]
fn a_tls_client_completes_its_handshake() {
    // The other half: refusing plaintext is not enough if the port also refuses
    // the traffic it is for.
    let (port, _stop) = start_tls_sharded();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let mut roots = rustls::RootCertStore::empty();
        // The certificate is self-signed and its own root; the test trusts it
        // explicitly rather than turning verification off, so this still
        // exercises a real chain check.
        let dir = temp_dir("plaintext");
        let pem = std::fs::read(dir.join("cert.pem")).unwrap();
        for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
            roots.add(cert.unwrap()).unwrap();
        }
        let cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg));
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let handshake = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connector.connect(server_name, stream),
        )
        .await;
        assert!(
            matches!(handshake, Ok(Ok(_))),
            "a TLS client could not handshake with the sharded server: {:?}",
            handshake.map(|r| r.map(|_| ()))
        );
    });
}
