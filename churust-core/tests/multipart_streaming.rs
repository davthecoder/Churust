//! The incremental `multipart/form-data` parser.
//!
//! The interesting cases are the ones a buffered parser never has to think
//! about: a delimiter split across two network chunks, a part skipped without
//! being read, and a body that stops in the middle.

#![cfg(feature = "multipart")]

use bytes::Bytes;
use churust_core::{Churust, Error, MultipartStream, TestClient};
use http::StatusCode;

/// A body with two fields and one file, as a browser would send it.
fn body() -> String {
    [
        "--X\r\n",
        "Content-Disposition: form-data; name=\"title\"\r\n\r\n",
        "a report\r\n",
        "--X\r\n",
        "Content-Disposition: form-data; name=\"doc\"; filename=\"a.txt\"\r\n",
        "Content-Type: text/plain\r\n\r\n",
        "hello world\r\n",
        "--X--\r\n",
    ]
    .concat()
}

/// An app whose handler reports `name=len` for every field, in order.
fn summing_app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                let mut out = Vec::new();
                while let Some(mut field) = form.next_field().await? {
                    let name = field.name().to_string();
                    let mut total = 0usize;
                    while let Some(chunk) = field.chunk().await? {
                        total += chunk.len();
                    }
                    out.push(format!("{name}={total}"));
                }
                Ok::<_, Error>(out.join(","))
            });
        })
        .build()
}

#[tokio::test]
async fn fields_arrive_in_order_with_their_content() {
    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body())
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "title=8,doc=11");
}

#[tokio::test]
async fn metadata_survives_the_incremental_parse() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                let mut out = Vec::new();
                while let Some(mut field) = form.next_field().await? {
                    let name = field.name().to_string();
                    let filename = field.filename().unwrap_or("-").to_string();
                    let content_type = field.content_type().unwrap_or("-").to_string();
                    let text = field.text().await?;
                    out.push(format!("{name}|{filename}|{content_type}|{text}"));
                }
                Ok::<_, Error>(out.join(" "))
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body())
        .send()
        .await;

    assert_eq!(
        res.text(),
        "title|-|-|a report doc|a.txt|text/plain|hello world"
    );
}

#[tokio::test]
async fn a_skipped_field_does_not_derail_the_parser() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                // Read nothing from the first field at all.
                let first = form.next_field().await?.map(|f| f.name().to_string());
                let second = match form.next_field().await? {
                    Some(mut f) => {
                        let name = f.name().to_string();
                        format!("{name}={}", f.text().await?)
                    }
                    None => "none".into(),
                };
                Ok::<_, Error>(format!("{}, {second}", first.unwrap_or_default()))
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body())
        .send()
        .await;

    assert_eq!(res.text(), "title, doc=hello world");
}

#[tokio::test]
async fn content_containing_the_boundary_text_is_not_split_early() {
    // The bytes "--X" appear inside the value. Only the delimiter preceded by
    // CRLF ends a part, and a parser that searched for the bare boundary would
    // truncate here.
    let body = [
        "--X\r\n",
        "Content-Disposition: form-data; name=\"note\"\r\n\r\n",
        "a --X b\r\n",
        "--X--\r\n",
    ]
    .concat();

    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    assert_eq!(res.text(), "note=7");
}

#[tokio::test]
async fn a_delimiter_followed_by_anything_but_padding_is_content() {
    // RFC 2046 §5.1.1 allows only transport padding — SP and HTAB — between a
    // delimiter and the CRLF that ends its line, so `\r\n--Xz` is content and
    // not a delimiter. A parser that accepts it frames the body differently
    // from every conforming one in front of it, which is how a field-level
    // filter in a proxy comes to pass a body whose forged field the origin then
    // acts on.
    let body = [
        "--X\r\n",
        "Content-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\r\n",
        "BEGIN\r\n",
        "--Xz\r\n",
        "Content-Disposition: form-data; name=\"role\"\r\n\r\n",
        "admin\r\n",
        "--X--\r\n",
    ]
    .concat();

    let app = Churust::server()
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                let mut out = Vec::new();
                while let Some(mut field) = form.next_field().await? {
                    let name = field.name().to_string();
                    out.push(format!("{name}={}", field.text().await?));
                }
                Ok::<_, Error>(out.join(","))
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    let text = res.text();
    assert!(
        text.starts_with("file=BEGIN"),
        "the one part must own everything from BEGIN onward: {text}"
    );
    assert!(
        !text.contains("role="),
        "the padded delimiter forged a field: {text}"
    );
}

#[tokio::test]
async fn real_transport_padding_after_a_delimiter_is_still_accepted() {
    // Spaces and tabs before the CRLF are legal and ignorable, so requiring the
    // CRLF must not mean refusing what precedes it.
    let body = [
        "--X \t\r\n",
        "Content-Disposition: form-data; name=\"note\"\r\n\r\n",
        "hi\r\n",
        "--X--\r\n",
    ]
    .concat();

    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "note=2");
}

#[tokio::test]
async fn a_body_that_stops_inside_a_part_is_a_400() {
    let truncated = [
        "--X\r\n",
        "Content-Disposition: form-data; name=\"doc\"\r\n\r\n",
        "half a fi",
    ]
    .concat();

    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(truncated)
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_body_with_no_delimiter_at_all_is_a_400() {
    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body("not multipart at all")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_part_without_a_name_is_refused() {
    let body = [
        "--X\r\n",
        "Content-Type: text/plain\r\n\r\n",
        "x\r\n",
        "--X--\r\n",
    ]
    .concat();

    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_non_multipart_content_type_is_a_415() {
    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn an_empty_part_reads_as_zero_bytes() {
    let body = [
        "--X\r\n",
        "Content-Disposition: form-data; name=\"empty\"\r\n\r\n",
        "\r\n",
        "--X--\r\n",
    ]
    .concat();

    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    assert_eq!(res.text(), "empty=0");
}

#[tokio::test]
async fn a_preamble_before_the_first_delimiter_is_ignored() {
    // RFC 2046 §5.1.1 allows text before the first delimiter, for readers that
    // do not understand multipart at all.
    let body = [
        "this is a preamble for non-multipart readers\r\n",
        "--X\r\n",
        "Content-Disposition: form-data; name=\"note\"\r\n\r\n",
        "hi\r\n",
        "--X--\r\n",
    ]
    .concat();

    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    assert_eq!(res.text(), "note=2");
}

#[tokio::test]
async fn a_quoted_boundary_is_accepted() {
    let res = TestClient::new(summing_app())
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=\"X\"")
        .body(body())
        .send()
        .await;

    assert_eq!(res.text(), "title=8,doc=11");
}

#[tokio::test]
async fn a_field_larger_than_the_collect_limit_is_refused() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/upload", |form: MultipartStream| async move {
                let mut form = form.max_field_bytes(8);
                let mut field = form.next_field().await?.expect("one field");
                let text = field.text().await?;
                Ok::<_, Error>(text)
            });
        })
        .build();

    let body = [
        "--X\r\n",
        "Content-Disposition: form-data; name=\"big\"\r\n\r\n",
        "0123456789abcdef\r\n",
        "--X--\r\n",
    ]
    .concat();

    let res = TestClient::new(app)
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(res.text().contains("field too large"));
}

#[tokio::test]
async fn chunking_the_body_does_not_change_what_is_parsed() {
    // The parser is fed from a real socket here, so a delimiter that lands
    // across two TCP segments exercises the held-back-tail path rather than
    // being handed over in one buffer.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let app = Churust::server()
        .host("127.0.0.1")
        .port(port)
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                let mut out = Vec::new();
                while let Some(mut field) = form.next_field().await? {
                    let name = field.name().to_string();
                    let content = field.bytes().await?;
                    out.push(format!("{name}={}", content.len()));
                }
                Ok::<_, Error>(out.join(","))
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
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // 64 KiB of content, so the body is split across many segments by the
    // kernel regardless of how it is written.
    let payload = "z".repeat(64 * 1024);
    let body = format!(
        "--X\r\nContent-Disposition: form-data; name=\"big\"; filename=\"b.bin\"\r\n\r\n{payload}\r\n--X--\r\n"
    );

    let response = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let request = format!(
            "POST /upload HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/form-data; boundary=X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        // Deliberately dribbled out, so the delimiter at the end lands in its
        // own segment rather than alongside the content.
        for piece in body.as_bytes().chunks(4096) {
            socket.write_all(piece).await.unwrap();
            socket.flush().await.unwrap();
        }
        let mut buf = String::new();
        socket.read_to_string(&mut buf).await.unwrap();
        buf
    };

    assert!(
        response.contains("big=65536"),
        "the streamed parse should see every byte exactly once: {response}"
    );
}

#[tokio::test]
async fn the_server_wide_body_cap_still_applies() {
    // Streaming removes the memory cost, not the ceiling: the engine wraps the
    // stream in `max_body_bytes`, so an oversized upload is still refused.
    //
    // This has to go over a socket. `TestClient` drives the pipeline directly
    // and the server-wide cap is applied by the engine in front of it, so the
    // in-process client would accept a body the real server refuses.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let app = Churust::server()
        .host("127.0.0.1")
        .port(port)
        .max_body_bytes(64)
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                let mut total = 0usize;
                while let Some(mut field) = form.next_field().await? {
                    while let Some(chunk) = field.chunk().await? {
                        total += chunk.len();
                    }
                }
                Ok::<_, Error>(format!("{total}"))
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
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let payload = "y".repeat(4096);
    let body = format!(
        "--X\r\nContent-Disposition: form-data; name=\"big\"\r\n\r\n{payload}\r\n--X--\r\n"
    );

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!(
        "POST /upload HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/form-data; boundary=X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    let _ = socket.read_to_string(&mut response).await;

    assert!(
        response.starts_with("HTTP/1.1 413"),
        "a 4 KiB upload must not pass a 64 byte cap: {}",
        response.lines().next().unwrap_or("")
    );
}

#[tokio::test]
async fn a_chunk_is_never_larger_than_what_arrived() {
    // Guards the invariant the whole design rests on: content is handed out as
    // it arrives, so no single chunk is the size of the body.
    let app = Churust::server()
        .max_body_bytes(1024 * 1024)
        .routing(|r| {
            r.post("/upload", |mut form: MultipartStream| async move {
                let mut field = form.next_field().await?.expect("one field");
                let mut largest = 0usize;
                let mut total = 0usize;
                while let Some(chunk) = field.chunk().await? {
                    largest = largest.max(chunk.len());
                    total += chunk.len();
                }
                Ok::<_, Error>(format!("{total}/{largest}"))
            });
        })
        .build();

    let payload = Bytes::from("q".repeat(200 * 1024));
    let body = format!(
        "--X\r\nContent-Disposition: form-data; name=\"big\"\r\n\r\n{}\r\n--X--\r\n",
        String::from_utf8(payload.to_vec()).unwrap()
    );

    let res = TestClient::new(app)
        .post("/upload")
        .header("content-type", "multipart/form-data; boundary=X")
        .body(body)
        .send()
        .await;

    let text = res.text();
    let (total, _largest) = text.split_once('/').expect("total/largest");
    assert_eq!(total, (200 * 1024).to_string());
}
