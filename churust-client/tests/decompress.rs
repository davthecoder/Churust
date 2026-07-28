//! Transparent gzip/deflate decode on the outbound client.

use churust_client::Client;
use churust_core::{Call, Churust, Response};
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use http::HeaderValue;
use std::io::Write;
use std::time::Duration;

async fn serve_compressed() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let app = Churust::server()
        .port(port)
        .host("127.0.0.1")
        .routing(|r| {
            r.get("/gzip", |_c: Call| async {
                let mut enc = GzEncoder::new(Vec::new(), Compression::default());
                enc.write_all(b"hello gzip").unwrap();
                let body = enc.finish().unwrap();
                Response::bytes("text/plain", body)
                    .with_header(CONTENT_ENCODING, HeaderValue::from_static("gzip"))
            });
            r.get("/deflate", |_c: Call| async {
                // zlib-wrapped DEFLATE — RFC 9110's "deflate" content coding.
                let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
                enc.write_all(b"hello deflate").unwrap();
                let body = enc.finish().unwrap();
                Response::bytes("text/plain", body)
                    .with_header(CONTENT_ENCODING, HeaderValue::from_static("deflate"))
            });
            r.get("/plain", |_c: Call| async {
                Response::bytes("text/plain", "already clear")
            });
            r.get("/bomb", |_c: Call| async {
                // Highly compressible zeros: tiny on the wire, huge inflated.
                let mut enc = GzEncoder::new(Vec::new(), Compression::best());
                let chunk = vec![0u8; 64 * 1024];
                for _ in 0..64 {
                    // 4 MiB of zeros
                    enc.write_all(&chunk).unwrap();
                }
                let body = enc.finish().unwrap();
                Response::bytes("application/octet-stream", body)
                    .with_header(CONTENT_ENCODING, HeaderValue::from_static("gzip"))
            });
            r.get("/accept", |c: Call| async move {
                c.header("accept-encoding").unwrap_or("none").to_string()
            });
        })
        .build();

    tokio::spawn(async move {
        let _ = app.start_on(listener, std::future::pending()).await;
    });
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn gzip_body_is_inflated() {
    let base = serve_compressed().await;
    let res = Client::new()
        .get(format!("{base}/gzip"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(res.text().unwrap(), "hello gzip");
    assert!(
        res.header("content-encoding").is_none(),
        "encoding header must not describe the decoded body"
    );
}

#[tokio::test]
async fn deflate_zlib_body_is_inflated() {
    let base = serve_compressed().await;
    let res = Client::new()
        .get(format!("{base}/deflate"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().unwrap(), "hello deflate");
    assert!(res.header("content-encoding").is_none());
}

#[tokio::test]
async fn plain_body_unchanged() {
    let base = serve_compressed().await;
    let res = Client::new()
        .get(format!("{base}/plain"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().unwrap(), "already clear");
}

#[tokio::test]
async fn auto_decompress_off_leaves_gzip_bytes() {
    let base = serve_compressed().await;
    let res = Client::new()
        .auto_decompress(false)
        .get(format!("{base}/gzip"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.header("content-encoding"), Some("gzip"));
    assert_ne!(res.bytes().as_ref(), b"hello gzip");
    assert!(res.bytes().len() < b"hello gzip".len() + 32); // still compressed
}

#[tokio::test]
async fn advertises_accept_encoding_when_enabled() {
    let base = serve_compressed().await;
    let res = Client::new()
        .get(format!("{base}/accept"))
        .send()
        .await
        .unwrap();
    let ae = res.text().unwrap();
    assert!(
        ae.contains("gzip") && ae.contains("deflate"),
        "advertised encodings: {ae}"
    );
}

#[tokio::test]
async fn does_not_advertise_encoding_when_disabled() {
    let base = serve_compressed().await;
    let res = Client::new()
        .auto_decompress(false)
        .get(format!("{base}/accept"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().unwrap(), "none");
}

#[tokio::test]
async fn decompression_bomb_hits_byte_ceiling() {
    let base = serve_compressed().await;
    // Cap well below the 4 MiB inflated bomb.
    let err = Client::new()
        .max_response_bytes(64 * 1024)
        .get(format!("{base}/bomb"))
        .send()
        .await
        .unwrap_err();
    match err {
        churust_client::ClientError::BodyTooLarge(limit) => {
            assert_eq!(limit, 64 * 1024);
        }
        other => panic!("expected BodyTooLarge, got {other}"),
    }
}

// Silence unused import when Content-Type is not asserted.
#[allow(dead_code)]
fn _ct() {
    let _ = CONTENT_TYPE;
}
