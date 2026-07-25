//! Response compression over the full pipeline.

use bytes::Bytes;
use churust_compression::{Compression, Encoding, Level};
use churust_core::{Body, Call, Churust, Response, TestClient};
use http::header::{CONTENT_ENCODING, CONTENT_TYPE, ETAG, VARY};
use http::{HeaderValue, StatusCode};
use tokio::io::AsyncReadExt;

/// The text every test compresses. Repetitive on purpose: the point of the
/// assertions is the plumbing, not the ratio.
fn payload() -> String {
    "the quick brown fox jumps over the lazy dog. ".repeat(200)
}

async fn gunzip(bytes: &Bytes) -> Vec<u8> {
    let mut out = Vec::new();
    async_compression::tokio::bufread::GzipDecoder::new(std::io::Cursor::new(bytes.to_vec()))
        .read_to_end(&mut out)
        .await
        .unwrap();
    out
}

async fn unbrotli(bytes: &Bytes) -> Vec<u8> {
    let mut out = Vec::new();
    async_compression::tokio::bufread::BrotliDecoder::new(std::io::Cursor::new(bytes.to_vec()))
        .read_to_end(&mut out)
        .await
        .unwrap();
    out
}

async fn unzlib(bytes: &Bytes) -> Vec<u8> {
    let mut out = Vec::new();
    async_compression::tokio::bufread::ZlibDecoder::new(std::io::Cursor::new(bytes.to_vec()))
        .read_to_end(&mut out)
        .await
        .unwrap();
    out
}

fn app() -> churust_core::App {
    Churust::server()
        .install(Compression::new())
        .routing(|r| {
            r.get("/text", |_c: Call| async { payload() });
            r.get("/small", |_c: Call| async { "tiny" });
            r.get("/image", |_c: Call| async {
                Response::bytes("image/png", vec![0u8; 8192])
            });
            r.get("/stream", |_c: Call| async {
                let chunks = futures_util::stream::iter(
                    (0..64).map(|_| Ok::<_, std::io::Error>(Bytes::from(payload()))),
                );
                Response::stream("text/plain", Body::from_stream(chunks))
            });
        })
        .build()
}

#[tokio::test]
async fn a_gzip_client_gets_a_gzip_body_that_decodes_back() {
    let res = TestClient::new(app())
        .get("/text")
        .header("accept-encoding", "gzip")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.header("content-encoding"), Some("gzip"));
    assert!(
        res.body_bytes().len() < payload().len(),
        "the encoded body should be smaller than the source"
    );
    assert_eq!(gunzip(res.body_bytes()).await, payload().as_bytes());
}

#[tokio::test]
async fn brotli_is_preferred_when_the_client_takes_both() {
    let res = TestClient::new(app())
        .get("/text")
        .header("accept-encoding", "gzip, br")
        .send()
        .await;

    assert_eq!(res.header("content-encoding"), Some("br"));
    assert_eq!(unbrotli(res.body_bytes()).await, payload().as_bytes());
}

#[tokio::test]
async fn deflate_is_the_zlib_format_that_rfc_9110_names() {
    let app = Churust::server()
        .install(Compression::new().encodings([Encoding::Deflate]))
        .routing(|r| {
            r.get("/text", |_c: Call| async { payload() });
        })
        .build();

    let res = TestClient::new(app)
        .get("/text")
        .header("accept-encoding", "deflate")
        .send()
        .await;

    assert_eq!(res.header("content-encoding"), Some("deflate"));
    assert_eq!(
        unzlib(res.body_bytes()).await,
        payload().as_bytes(),
        "a raw deflate stream here would fail to decode as zlib"
    );
}

#[tokio::test]
async fn a_client_that_asks_for_nothing_gets_the_identity_body() {
    let res = TestClient::new(app()).get("/text").send().await;

    assert!(res.header("content-encoding").is_none());
    assert_eq!(res.text(), payload());
    assert_eq!(
        res.header("vary"),
        Some("accept-encoding"),
        "an uncompressed response still has to be Vary-marked"
    );
}

#[tokio::test]
async fn a_body_below_the_floor_is_left_alone() {
    let res = TestClient::new(app())
        .get("/small")
        .header("accept-encoding", "gzip")
        .send()
        .await;

    assert!(res.header("content-encoding").is_none());
    assert_eq!(res.text(), "tiny");
}

#[tokio::test]
async fn an_already_compressed_media_type_is_left_alone() {
    let res = TestClient::new(app())
        .get("/image")
        .header("accept-encoding", "gzip, br")
        .send()
        .await;

    assert!(res.header("content-encoding").is_none());
    assert_eq!(res.body_bytes().len(), 8192);
}

#[tokio::test]
async fn a_streamed_body_is_compressed_without_being_collected_first() {
    let res = TestClient::new(app())
        .get("/stream")
        .header("accept-encoding", "gzip")
        .send()
        .await;

    assert_eq!(res.header("content-encoding"), Some("gzip"));
    let expected = payload().repeat(64);
    assert_eq!(gunzip(res.body_bytes()).await, expected.as_bytes());
    assert!(
        res.body_bytes().len() < expected.len() / 10,
        "64 copies of the same text should compress hard"
    );
}

#[tokio::test]
async fn a_strong_etag_is_weakened_when_the_body_is_encoded() {
    let app = Churust::server()
        .install(Compression::new())
        .routing(|r| {
            r.get("/tagged", |_c: Call| async {
                let mut res = Response::text(payload());
                res.headers.insert(ETAG, HeaderValue::from_static("\"v1\""));
                res
            });
        })
        .build();

    let plain = TestClient::new(app).get("/tagged").send().await;
    assert_eq!(plain.header("etag"), Some("\"v1\""));

    let app = Churust::server()
        .install(Compression::new())
        .routing(|r| {
            r.get("/tagged", |_c: Call| async {
                let mut res = Response::text(payload());
                res.headers.insert(ETAG, HeaderValue::from_static("\"v1\""));
                res
            });
        })
        .build();
    let encoded = TestClient::new(app)
        .get("/tagged")
        .header("accept-encoding", "gzip")
        .send()
        .await;
    assert_eq!(
        encoded.header("etag"),
        Some("W/\"v1\""),
        "the compressed body is equivalent, not byte-identical"
    );
}

#[tokio::test]
async fn an_unsupported_coding_falls_through_uncompressed() {
    let res = TestClient::new(app())
        .get("/text")
        .header("accept-encoding", "exi, sdch")
        .send()
        .await;

    assert!(res.header("content-encoding").is_none());
    assert_eq!(res.text(), payload());
}

#[tokio::test]
async fn quality_zero_on_everything_sends_identity() {
    let res = TestClient::new(app())
        .get("/text")
        .header("accept-encoding", "gzip;q=0, br;q=0, deflate;q=0")
        .send()
        .await;

    assert!(res.header("content-encoding").is_none());
    assert_eq!(res.text(), payload());
}

#[tokio::test]
async fn the_level_setting_is_honoured() {
    fn app_with(level: Level) -> churust_core::App {
        Churust::server()
            .install(
                Compression::new()
                    .level(level)
                    .encodings([Encoding::Brotli]),
            )
            .routing(|r| {
                r.get("/text", |_c: Call| async { payload() });
            })
            .build()
    }

    let fastest = TestClient::new(app_with(Level::Fastest))
        .get("/text")
        .header("accept-encoding", "br")
        .send()
        .await;
    let best = TestClient::new(app_with(Level::Best))
        .get("/text")
        .header("accept-encoding", "br")
        .send()
        .await;

    assert_eq!(unbrotli(fastest.body_bytes()).await, payload().as_bytes());
    assert_eq!(unbrotli(best.body_bytes()).await, payload().as_bytes());
    assert!(
        best.body_bytes().len() <= fastest.body_bytes().len(),
        "the higher setting should not produce a larger body"
    );
}

#[tokio::test]
async fn scoped_compression_applies_only_inside_its_subtree() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/plain", |_c: Call| async { payload() });
            r.route("/api", |r| {
                r.intercept(Compression::new());
                r.get("/text", |_c: Call| async { payload() });
            });
        })
        .build();
    let client = TestClient::new(app);

    let inside = client
        .get("/api/text")
        .header("accept-encoding", "gzip")
        .send()
        .await;
    assert_eq!(inside.header("content-encoding"), Some("gzip"));

    let outside = client
        .get("/plain")
        .header("accept-encoding", "gzip")
        .send()
        .await;
    assert!(outside.header("content-encoding").is_none());
    assert!(
        outside.header("vary").is_none(),
        "a response the plugin never saw should not be marked"
    );
}

#[tokio::test]
async fn a_204_has_nothing_to_encode() {
    let app = Churust::server()
        .install(Compression::new())
        .routing(|r| {
            r.get("/empty", |_c: Call| async { StatusCode::NO_CONTENT });
        })
        .build();

    let res = TestClient::new(app)
        .get("/empty")
        .header("accept-encoding", "gzip")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert!(res.header("content-encoding").is_none());
    assert!(res.body_bytes().is_empty());
}

#[tokio::test]
async fn the_content_type_survives_compression() {
    let app = Churust::server()
        .install(Compression::new())
        .routing(|r| {
            r.get("/json", |_c: Call| async {
                Response::bytes("application/json", format!("{{\"v\":\"{}\"}}", payload()))
            });
        })
        .build();

    let res = TestClient::new(app)
        .get("/json")
        .header("accept-encoding", "gzip")
        .send()
        .await;

    assert_eq!(res.header("content-encoding"), Some("gzip"));
    assert_eq!(res.headers().get(CONTENT_TYPE).unwrap(), "application/json");
}

#[tokio::test]
async fn a_handler_that_already_encoded_is_not_encoded_twice() {
    let app = Churust::server()
        .install(Compression::new())
        .routing(|r| {
            r.get("/pre", |_c: Call| async {
                let mut res = Response::bytes("text/plain", vec![7u8; 4096]);
                res.headers
                    .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                res
            });
        })
        .build();

    let res = TestClient::new(app)
        .get("/pre")
        .header("accept-encoding", "gzip")
        .send()
        .await;

    assert_eq!(res.header("content-encoding"), Some("gzip"));
    assert_eq!(
        res.body_bytes().as_ref(),
        vec![7u8; 4096].as_slice(),
        "the handler's bytes should be untouched"
    );
    assert_eq!(res.headers().get_all(VARY).iter().count(), 1);
}
