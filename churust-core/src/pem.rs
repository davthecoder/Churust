//! Reading a PEM certificate chain and private key from disk (feature `tls`).
//!
//! Both TLS transports need this and neither owns it: [`crate::tls`] builds a
//! `tokio_rustls::TlsAcceptor` for TCP, [`crate::http3`] builds a
//! `quinn::ServerConfig` for QUIC, and the loading step in front of each was the
//! same twenty lines twice. The type names differ between those modules only in
//! how they are spelled — `tokio_rustls::rustls` re-exports the `rustls` this
//! uses directly, and the workspace resolves one version of it — so one pair of
//! functions serves both.
//!
//! Here rather than in `tls`, so that the QUIC path is not reaching into the TCP
//! path's module for something that belongs to neither.

#![cfg(feature = "tls")]

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{self, BufReader};

/// Read a PEM certificate chain.
///
/// # Errors
///
/// If the file is missing or unreadable, or if it does not parse as PEM.
pub(crate) fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

/// Read a PEM private key.
///
/// # Errors
///
/// If the file is missing or unreadable, or if it contains no private key. A
/// file that parses but holds nothing is an `InvalidInput` rather than a silent
/// `None`, because a server with no key is not a server.
pub(crate) fn load_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no private key found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_certificate_file_is_reported_not_panicked() {
        match load_certs("/nonexistent/cert.pem") {
            Ok(_) => panic!("expected an error for a missing file"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
        }
    }

    #[test]
    fn a_missing_key_file_is_reported_not_panicked() {
        match load_key("/nonexistent/key.pem") {
            Ok(_) => panic!("expected an error for a missing file"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
        }
    }

    #[test]
    fn a_key_file_without_a_key_is_an_error_rather_than_an_empty_success() {
        let dir = std::env::temp_dir().join("churust-pem-tests");
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("no-key.pem");
        std::fs::write(&path, b"not a pem file at all\n").expect("write the file");

        let loaded = load_key(path.to_str().expect("a utf-8 path"));
        let _ = std::fs::remove_file(&path);

        match loaded {
            Ok(_) => panic!("expected an error for a file with no private key"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
        }
    }
}
