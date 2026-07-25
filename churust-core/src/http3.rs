//! HTTP/3 over QUIC (feature `http3`).
//!
//! HTTP/3 is a different transport, not a different framing of the same one:
//! it runs over QUIC on UDP, so it needs its own socket and its own listener
//! rather than an extra branch inside the TCP engine. That is why this is
//! started separately from [`App::start`](crate::App::start), and why an
//! application that wants both runs both.
//!
//! ```no_run
//! use churust_core::{Call, Churust};
//!
//! # async fn run() -> std::io::Result<()> {
//! let app = Churust::server()
//!     // Tell HTTP/1.1 and HTTP/2 clients that h3 is available on the same
//!     // port, so they can upgrade themselves on the next request.
//!     .advertise_http3(8443)
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "served over h3" });
//!     })
//!     .build();
//!
//! churust_core::http3::serve(
//!     app,
//!     "0.0.0.0:8443".parse().unwrap(),
//!     "cert.pem",
//!     "key.pem",
//! )
//! .await
//! # }
//! ```
//!
//! # How clients find it
//!
//! They do not, on their own. A browser reaches a new origin over TCP, and only
//! moves to h3 if the response says h3 exists. That is the `Alt-Svc` header, and
//! [`AppBuilder::advertise_http3`](crate::AppBuilder::advertise_http3) is what
//! sets it. Serving h3 without advertising it means almost nothing will ever
//! use it.
//!
//! # What is supported
//!
//! Request routing, headers, request bodies and response bodies, including
//! streamed ones, through the same pipeline every other transport uses: a
//! handler cannot tell which transport it is answering.
//!
//! WebSockets are not carried over h3. The upgrade mechanism there is Extended
//! CONNECT from RFC 9220, which is a different handshake from the HTTP/1.1 one
//! the `ws` feature implements, and pretending otherwise would produce a
//! connection that fails at the first frame.

#![cfg(feature = "http3")]

use crate::app::App;
use crate::body::Body;
use bytes::{Buf, Bytes};
use http::{HeaderMap, Method, Request, Uri};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

/// Build a QUIC server configuration from a PEM certificate chain and key.
///
/// The ALPN protocol is `h3` and nothing else: a QUIC connection that
/// negotiated anything else would not be HTTP/3, and there is nothing here to
/// hand it to.
///
/// # Errors
///
/// If either file is missing or unreadable, if no private key is found, or if
/// rustls rejects the pair.
pub fn server_config_from_pem(cert_path: &str, key_path: &str) -> io::Result<quinn::ServerConfig> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic)))
}

fn load_certs(path: &str) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

fn load_key(path: &str) -> io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no private key found"))
}

/// Serve `app` over HTTP/3 on `addr` until the process ends.
///
/// # Errors
///
/// If the certificate or key cannot be loaded, or the UDP socket cannot be
/// bound.
pub async fn serve(app: App, addr: SocketAddr, cert_path: &str, key_path: &str) -> io::Result<()> {
    let config = server_config_from_pem(cert_path, key_path)?;
    serve_with_config(app, addr, config).await
}

/// Serve `app` over HTTP/3 with an already-built QUIC configuration.
///
/// # Errors
///
/// If the UDP socket cannot be bound.
pub async fn serve_with_config(
    app: App,
    addr: SocketAddr,
    config: quinn::ServerConfig,
) -> io::Result<()> {
    Http3Server::bind(addr, config)?.serve(app).await;
    Ok(())
}

/// A bound QUIC listener, not yet serving.
///
/// Binding and serving are separate steps so the port can be read back before
/// anything is accepted. That matters for the ordinary case of binding port 0
/// and telling something else where to connect, and it is what lets a test know
/// the address without racing the server.
pub struct Http3Server {
    endpoint: quinn::Endpoint,
}

impl std::fmt::Debug for Http3Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http3Server")
            .field("local_addr", &self.endpoint.local_addr().ok())
            .finish_non_exhaustive()
    }
}

impl Http3Server {
    /// Bind a UDP socket for QUIC without serving yet.
    ///
    /// # Errors
    ///
    /// If the socket cannot be bound.
    pub fn bind(addr: SocketAddr, config: quinn::ServerConfig) -> io::Result<Self> {
        Ok(Self {
            endpoint: quinn::Endpoint::server(config, addr)?,
        })
    }

    /// The address actually bound, which is what resolves a port of 0.
    ///
    /// # Errors
    ///
    /// If the socket cannot report its address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Accept connections and serve them through `app` until the endpoint
    /// closes.
    pub async fn serve(self, app: App) {
        accept_loop(app, self.endpoint).await;
    }
}

/// Accept QUIC connections until the endpoint closes.
async fn accept_loop(app: App, endpoint: quinn::Endpoint) {
    while let Some(incoming) = endpoint.accept().await {
        let app = app.clone();
        // One task per connection, like the TCP engine: a slow peer must not
        // hold up the accept loop.
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    if let Err(e) = serve_connection(app, connection).await {
                        tracing::debug!(error = %e, "http3 connection ended");
                    }
                }
                // A failed handshake on an internet-facing port is constant
                // background noise, so debug rather than warn, matching how the
                // TCP engine treats a failed TLS handshake.
                Err(e) => tracing::debug!(error = %e, "http3 handshake failed"),
            }
        });
    }
}

/// Run one QUIC connection's request streams through the pipeline.
async fn serve_connection(
    app: App,
    connection: quinn::Connection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut h3 = h3::server::Connection::new(h3_quinn::Connection::new(connection)).await?;

    loop {
        match h3.accept().await {
            Ok(Some(resolver)) => {
                let (request, stream) = resolver.resolve_request().await?;
                let app = app.clone();
                // One task per request: h3 multiplexes streams on one
                // connection, so serving them in sequence would make a slow
                // handler block every other request on that connection.
                tokio::spawn(async move {
                    if let Err(e) = serve_request(app, request, stream).await {
                        tracing::debug!(error = %e, "http3 request failed");
                    }
                });
            }
            // The peer closed the connection cleanly.
            Ok(None) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}

/// Answer one HTTP/3 request.
async fn serve_request<S>(
    app: App,
    request: Request<()>,
    mut stream: h3::server::RequestStream<S, Bytes>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let (parts, _) = request.into_parts();
    let max_body = app.config().max_body_bytes;

    let body = match read_body(&mut stream, max_body).await {
        Ok(body) => body,
        Err(TooLarge) => {
            let response = http::Response::builder()
                .status(http::StatusCode::PAYLOAD_TOO_LARGE)
                .body(())?;
            stream.send_response(response).await?;
            stream.finish().await?;
            return Ok(());
        }
    };

    let response = app
        .process_with_extensions(
            parts.method.clone(),
            normalise_uri(&parts.uri, &parts.method),
            parts.headers.clone(),
            body,
            http::Extensions::new(),
        )
        .await;

    send_response(&mut stream, response, parts.method == Method::HEAD).await
}

/// The request body exceeded the server's cap.
struct TooLarge;

/// Read a request body, refusing one past `max_body`.
///
/// Counted as it arrives rather than collected and then measured, so an
/// oversized body is refused at the chunk that crosses the line instead of
/// after all of it has been held in memory.
async fn read_body<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    max_body: usize,
) -> Result<Bytes, TooLarge>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut buf = bytes::BytesMut::new();
    while let Ok(Some(mut chunk)) = stream.recv_data().await {
        let piece = chunk.copy_to_bytes(chunk.remaining());
        if buf.len() + piece.len() > max_body {
            return Err(TooLarge);
        }
        buf.extend_from_slice(&piece);
    }
    Ok(buf.freeze())
}

/// HTTP/3 always carries an absolute-form target, TCP requests usually do not.
///
/// The router matches on the path, and `Call::path` reads it off the URI, so
/// both forms already work. What differs is what a handler sees when it prints
/// the URI, and an absolute form there would be a gratuitous difference between
/// transports. `CONNECT` and `OPTIONS *` are left alone: their target is not a
/// path and rewriting it would destroy the request.
fn normalise_uri(uri: &Uri, method: &Method) -> Uri {
    if method == Method::CONNECT || uri.path() == "*" {
        return uri.clone();
    }
    let Some(path_and_query) = uri.path_and_query() else {
        return uri.clone();
    };
    path_and_query
        .as_str()
        .parse()
        .unwrap_or_else(|_| uri.clone())
}

/// Write a [`Response`](crate::Response) out over an h3 stream.
async fn send_response<S>(
    stream: &mut h3::server::RequestStream<S, Bytes>,
    response: crate::Response,
    head_only: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut builder = http::Response::builder().status(response.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = sanitise(response.headers);
    }
    stream.send_response(builder.body(())?).await?;

    if !head_only {
        match response.body {
            Body::Bytes(bytes) => {
                if !bytes.is_empty() {
                    stream.send_data(bytes).await?;
                }
            }
            Body::Stream(mut chunks) => {
                use futures_util::StreamExt;
                // Forwarded chunk by chunk, so a streamed response stays
                // streamed over h3 instead of being collected to send it.
                while let Some(chunk) = chunks.next().await {
                    match chunk {
                        Ok(bytes) => stream.send_data(bytes).await?,
                        // The body failed partway. There is no status left to
                        // change, so the honest signal is an incomplete
                        // stream: reset it rather than finishing cleanly and
                        // claiming the truncated body was the whole thing.
                        Err(_) => return Ok(()),
                    }
                }
            }
        }
    }

    stream.finish().await?;
    Ok(())
}

/// Drop headers HTTP/3 forbids.
///
/// RFC 9114 §4.2 bans connection-specific fields, and a `Connection:
/// keep-alive` copied from a handler written for HTTP/1.1 is enough for a
/// conforming client to treat the whole response as malformed.
fn sanitise(mut headers: HeaderMap) -> HeaderMap {
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_target_becomes_origin_form() {
        let uri: Uri = "https://example.com/a/b?x=1".parse().unwrap();
        assert_eq!(
            normalise_uri(&uri, &Method::GET).to_string(),
            "/a/b?x=1",
            "a handler should see the same target on every transport"
        );
    }

    #[test]
    fn an_origin_form_target_is_left_alone() {
        let uri: Uri = "/a/b".parse().unwrap();
        assert_eq!(normalise_uri(&uri, &Method::GET).to_string(), "/a/b");
    }

    #[test]
    fn a_connect_target_is_left_alone() {
        let uri: Uri = "example.com:443".parse().unwrap();
        assert_eq!(
            normalise_uri(&uri, &Method::CONNECT).to_string(),
            "example.com:443"
        );
    }

    #[test]
    fn connection_specific_headers_are_dropped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("content-type", "text/plain".parse().unwrap());

        let clean = sanitise(headers);
        assert!(clean.get("connection").is_none());
        assert!(clean.get("transfer-encoding").is_none());
        assert_eq!(clean.get("content-type").unwrap(), "text/plain");
    }

    #[test]
    fn a_missing_certificate_is_reported_rather_than_panicking() {
        match server_config_from_pem("/nonexistent/cert.pem", "/nonexistent/key.pem") {
            Ok(_) => panic!("expected an error for missing files"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
        }
    }
}
