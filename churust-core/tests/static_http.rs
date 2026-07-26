//! Conditional GET and byte-range behaviour for `StaticFiles`.
//!
//! RFC 9110 §13 (conditional requests) and §14 (range requests). The served
//! file is exactly ten bytes, `0123456789`, so offsets in the assertions are
//! readable.

#![cfg(feature = "fs")]

use churust_core::{fs::StaticFiles, App, Churust, TestClient};
use http::{Method, StatusCode};

const BODY: &str = "0123456789";

fn tree(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("churust-http-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create root");
    std::fs::write(root.join("f.txt"), BODY).expect("write file");
    root
}

fn app(root: &std::path::Path) -> App {
    let root = root.to_path_buf();
    Churust::server()
        .routing(move |r| {
            r.get("/s/{path...}", StaticFiles::dir(root.clone()).handler());
        })
        .build()
}

/// Fetch the file once to learn the validators the server minted.
async fn validators(client: &TestClient) -> (String, String) {
    let res = client.get("/s/f.txt").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    (
        res.header("etag").expect("ETag must be sent").to_string(),
        res.header("last-modified")
            .expect("Last-Modified must be sent")
            .to_string(),
    )
}

#[tokio::test]
async fn serves_the_file_with_validators_and_accept_ranges() {
    let root = tree("validators");
    let res = TestClient::new(app(&root)).get("/s/f.txt").send().await;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), BODY);
    assert!(res.header("etag").is_some(), "ETag missing");
    assert!(
        res.header("last-modified").is_some(),
        "Last-Modified missing"
    );
    assert_eq!(res.header("accept-ranges"), Some("bytes"));
    // Weak, because the bytes are not hashed — only (mtime, len) is compared.
    assert!(
        res.header("etag").unwrap().starts_with("W/"),
        "ETag should be weak, got {:?}",
        res.header("etag")
    );
}

#[tokio::test]
async fn if_none_match_hit_is_304_with_no_body() {
    let root = tree("inm-hit");
    let client = TestClient::new(app(&root));
    let (etag, _) = validators(&client).await;

    let res = client
        .get("/s/f.txt")
        .header("if-none-match", &etag)
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(res.text(), "", "304 must not carry a body");
    assert!(
        res.header("etag").is_some(),
        "304 should still carry the validator"
    );
}

#[tokio::test]
async fn if_none_match_star_is_304() {
    let root = tree("inm-star");
    let client = TestClient::new(app(&root));
    let res = client
        .get("/s/f.txt")
        .header("if-none-match", "*")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn if_none_match_miss_serves_the_file() {
    let root = tree("inm-miss");
    let client = TestClient::new(app(&root));
    let res = client
        .get("/s/f.txt")
        .header("if-none-match", "W/\"something-else\"")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), BODY);
}

#[tokio::test]
async fn if_modified_since_not_modified_is_304() {
    let root = tree("ims");
    let client = TestClient::new(app(&root));
    let (_, last_modified) = validators(&client).await;

    let res = client
        .get("/s/f.txt")
        .header("if-modified-since", &last_modified)
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn if_none_match_takes_precedence_over_if_modified_since() {
    let root = tree("precedence");
    let client = TestClient::new(app(&root));
    let (_, last_modified) = validators(&client).await;

    // The date says "not modified", but the tag does not match. RFC 9110
    // §13.1.3: when If-None-Match is present, If-Modified-Since is ignored.
    let res = client
        .get("/s/f.txt")
        .header("if-none-match", "W/\"stale\"")
        .header("if-modified-since", &last_modified)
        .send()
        .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "If-None-Match must win over If-Modified-Since"
    );
}

#[tokio::test]
async fn if_match_mismatch_is_412() {
    let root = tree("im");
    let client = TestClient::new(app(&root));
    let res = client
        .get("/s/f.txt")
        .header("if-match", "\"nope\"")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn range_returns_206_with_content_range() {
    let root = tree("range");
    let client = TestClient::new(app(&root));
    let res = client
        .get("/s/f.txt")
        .header("range", "bytes=0-3")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.text(), "0123");
    assert_eq!(res.header("content-range"), Some("bytes 0-3/10"));
}

#[tokio::test]
async fn open_ended_and_suffix_ranges() {
    let root = tree("range-forms");
    let client = TestClient::new(app(&root));

    let open = client
        .get("/s/f.txt")
        .header("range", "bytes=7-")
        .send()
        .await;
    assert_eq!(open.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(open.text(), "789");
    assert_eq!(open.header("content-range"), Some("bytes 7-9/10"));

    let suffix = client
        .get("/s/f.txt")
        .header("range", "bytes=-3")
        .send()
        .await;
    assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(suffix.text(), "789", "a suffix range counts from the end");
    assert_eq!(suffix.header("content-range"), Some("bytes 7-9/10"));
}

#[tokio::test]
async fn unsatisfiable_range_is_416_with_the_entity_length() {
    let root = tree("range-416");
    let client = TestClient::new(app(&root));
    let res = client
        .get("/s/f.txt")
        .header("range", "bytes=100-200")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(res.header("content-range"), Some("bytes */10"));
}

#[tokio::test]
async fn multi_range_falls_back_to_the_whole_entity() {
    let root = tree("range-multi");
    let client = TestClient::new(app(&root));
    // multipart/byteranges is deliberately not implemented; RFC 9110 permits
    // answering with the full representation instead.
    let res = client
        .get("/s/f.txt")
        .header("range", "bytes=0-1,4-5")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), BODY);
}

#[tokio::test]
async fn malformed_range_is_ignored() {
    let root = tree("range-bad");
    let client = TestClient::new(app(&root));
    // An unparseable Range must be ignored, not turned into an error.
    let res = client
        .get("/s/f.txt")
        .header("range", "furlongs=1-2")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), BODY);
}

#[tokio::test]
async fn if_range_matching_gives_partial_mismatching_gives_full() {
    let root = tree("if-range");
    let client = TestClient::new(app(&root));
    let (etag, _) = validators(&client).await;

    let matching = client
        .get("/s/f.txt")
        .header("range", "bytes=0-3")
        .header("if-range", &etag)
        .send()
        .await;
    assert_eq!(matching.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(matching.text(), "0123");

    let stale = client
        .get("/s/f.txt")
        .header("range", "bytes=0-3")
        .header("if-range", "W/\"stale\"")
        .send()
        .await;
    assert_eq!(
        stale.status(),
        StatusCode::OK,
        "a stale If-Range must yield the whole entity"
    );
    assert_eq!(stale.text(), BODY);
}

#[tokio::test]
async fn a_stale_if_range_with_an_out_of_range_request_serves_the_whole_entity() {
    let root = tree("if-range-416");
    let client = TestClient::new(app(&root));

    // The resume case this exists for: the client remembers a ten-megabyte
    // representation, asks to continue from an offset that only made sense for
    // that one, and offers the validator it remembers. The entity has since
    // shrunk, so the offset is now out of bounds *and* the validator is stale.
    //
    // RFC 9110 §14.2 settles which of those the server answers: a validator
    // that does not match means the Range header field is ignored, full stop.
    // Ignoring it cannot produce a 416, because there is no longer a range to
    // find unsatisfiable — the correct answer is 200 with the whole current
    // representation, which is exactly the fallback If-Range exists to provide.
    let res = client
        .get("/s/f.txt")
        .header("range", "bytes=100-200")
        .header("if-range", "W/\"stale\"")
        .send()
        .await;

    assert_eq!(
        res.status(),
        StatusCode::OK,
        "a stale If-Range must retract the Range before it can be judged unsatisfiable"
    );
    assert_eq!(res.text(), BODY);
    assert_eq!(
        res.header("content-range"),
        None,
        "no range was served, so nothing should describe one"
    );

    // The other half of the same rule: a *matching* If-Range leaves the Range
    // in force, so an out-of-bounds one is still the 416 it always was.
    let (etag, _) = validators(&client).await;
    let fresh = client
        .get("/s/f.txt")
        .header("range", "bytes=100-200")
        .header("if-range", &etag)
        .send()
        .await;
    assert_eq!(fresh.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(fresh.header("content-range"), Some("bytes */10"));
}

/// `TestClient` never reaches hyper, so it cannot show whether an explicit
/// `Content-Length` on a *streamed* body conflicts with chunked framing. If it
/// did, real responses would be malformed while every test above still passed.
#[tokio::test]
async fn range_response_is_well_formed_on_the_wire() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = tree("wire");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let root2 = root.clone();
    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(move |r| {
            r.get("/s/{path...}", StaticFiles::dir(root2.clone()).handler());
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_on(listener, async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!(
            "GET /s/f.txt HTTP/1.1\r\nHost: {addr}\r\nRange: bytes=2-5\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);
    println!("--- wire ---\n{text}\n--- end ---");

    assert!(text.starts_with("HTTP/1.1 206"), "got: {text}");
    let lower = text.to_ascii_lowercase();
    assert!(lower.contains("content-range: bytes 2-5/10"), "got: {text}");
    assert!(lower.contains("content-length: 4"), "got: {text}");
    assert!(
        !lower.contains("transfer-encoding: chunked"),
        "content-length and chunked framing must not both be sent: {text}"
    );
    assert!(
        text.ends_with("2345"),
        "body should be exactly the range: {text}"
    );

    let _ = tx.send(());
    let _ = server.await;
}

#[tokio::test]
async fn head_on_a_static_file_reports_the_real_length() {
    let root = tree("head");
    let res = TestClient::new(app(&root))
        .request(Method::HEAD, "/s/f.txt")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "");
    assert_eq!(
        res.header("content-length"),
        Some("10"),
        "HEAD should report the length a GET would send"
    );
}
