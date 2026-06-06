//! TLS support (feature `tls`). Loads a PEM cert chain + private key and builds
//! a `tokio_rustls::TlsAcceptor` with rustls' safe defaults.

#![cfg(feature = "tls")]

use std::io::{self, BufReader};
use std::sync::Arc;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// Build a [`TlsAcceptor`] from a PEM certificate chain and private key on disk.
///
/// Reads the certificate chain from `cert_path` and the private key from
/// `key_path`, then assembles a rustls [`ServerConfig`] with no client auth and
/// rustls' safe defaults. Used by the serving engine when TLS is configured via
/// [`AppBuilder::tls`](crate::AppBuilder::tls); call it directly only for custom
/// serving setups.
///
/// # Errors
///
/// Returns an [`io::Error`] if either file is missing or unreadable, if no
/// private key is found in the key file, or if the certificate/key pair is
/// rejected by rustls.
///
/// Available only when the `tls` feature is enabled.
pub fn acceptor_from_pem(cert_path: &str, key_path: &str) -> io::Result<TlsAcceptor> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

fn load_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no private key found"))
}

#[cfg(all(test, feature = "tls"))]
mod tests {
    use super::*;

    #[test]
    fn missing_files_error_cleanly() {
        // `TlsAcceptor` is not `Debug` in tokio-rustls 0.26, so we cannot use
        // `unwrap_err()`; match on the `Err` arm instead.
        match acceptor_from_pem("/nonexistent/cert.pem", "/nonexistent/key.pem") {
            Ok(_) => panic!("expected an error for nonexistent cert/key files"),
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
        }
    }
}
