#![cfg(not(feature = "tls"))]
//! Configuring TLS without the `tls` feature must refuse to start.
//!
//! It used to start and serve plaintext. The build succeeded, the server came
//! up, requests were answered — in clear text, on the port the operator had
//! configured a certificate for and that every client and runbook treated as
//! HTTPS. The only signal was a doc sentence saying the feature is "required to
//! have any effect at serve time".
//!
//! This file is the negative build's half of that contract; `sharded_tls.rs`
//! covers the same hazard in the build where the feature *is* on and the
//! sharded engine was the thing skipping it.

use churust_core::{Call, Churust};

fn tls_app() -> churust_core::App {
    Churust::server()
        .host("127.0.0.1")
        .port(0)
        // Paths that do not exist: the refusal must happen before anything tries
        // to read them, so that the error names the real problem rather than a
        // missing file.
        .tls("/nonexistent/cert.pem", "/nonexistent/key.pem")
        .routing(|r| {
            r.get("/", |_c: Call| async { "secret" });
        })
        .build()
}

fn assert_refused(result: std::io::Result<()>, entry_point: &str) {
    let err = match result {
        Err(e) => e,
        Ok(()) => panic!("{entry_point} served a TLS-configured app without the tls feature"),
    };
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "{entry_point} refused, but not as invalid input: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("tls") && msg.contains("plaintext"),
        "{entry_point}'s error should say what the hazard is; got: {msg}"
    );
}

#[test]
fn serve_refuses() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let addr = "127.0.0.1:0".parse().unwrap();
    let out = rt.block_on(churust_core::engine::serve(
        tls_app(),
        addr,
        std::future::pending::<()>(),
    ));
    assert_refused(out, "serve");
}

#[test]
fn serve_many_refuses() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let addr = "127.0.0.1:0".parse().unwrap();
    let out = rt.block_on(churust_core::engine::serve_many(
        tls_app(),
        vec![addr],
        std::future::pending::<()>(),
    ));
    assert_refused(out, "serve_many");
}

#[test]
fn serve_sharded_refuses() {
    let addr = "127.0.0.1:0".parse().unwrap();
    let out =
        churust_core::engine::serve_sharded(tls_app(), vec![addr], 2, std::future::pending::<()>());
    assert_refused(out, "serve_sharded");
}

#[test]
fn serve_on_refuses() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        churust_core::engine::serve_on(tls_app(), l, std::future::pending::<()>()).await
    });
    assert_refused(out, "serve_on");
}

#[test]
fn a_plaintext_app_is_unaffected() {
    // The guard must not refuse the ordinary case. Bound and then shut down
    // immediately, which is enough to prove it got past the check.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(async {
        let app = Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        churust_core::engine::serve_on(app, l, async {}).await
    });
    assert!(out.is_ok(), "a plaintext app was refused: {out:?}");
}
