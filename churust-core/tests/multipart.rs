//! `multipart/form-data` — file uploads.

#![cfg(feature = "multipart")]

use churust_core::{Churust, Multipart, TestClient};
use http::StatusCode;

const B: &str = "----churustBoundary";

fn body(parts: &[(&str, Option<&str>, &str)]) -> String {
    let mut out = String::new();
    for (name, filename, content) in parts {
        out.push_str(&format!("--{B}\r\n"));
        match filename {
            Some(f) => out.push_str(&format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n\r\n"
            )),
            None => out.push_str(&format!(
                "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
            )),
        }
        out.push_str(content);
        out.push_str("\r\n");
    }
    out.push_str(&format!("--{B}--\r\n"));
    out
}

fn app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.post("/u", |form: Multipart| async move {
                let names: Vec<String> = form
                    .parts()
                    .iter()
                    .map(|p| {
                        format!(
                            "{}:{}:{}",
                            p.name,
                            p.filename.clone().unwrap_or_default(),
                            p.bytes.len()
                        )
                    })
                    .collect();
                names.join(",")
            });
        })
        .build()
}

async fn post(b: String) -> churust_core::TestResponse {
    TestClient::new(app())
        .post("/u")
        .header(
            "content-type",
            &format!("multipart/form-data; boundary={B}"),
        )
        .body(b)
        .send()
        .await
}

#[tokio::test]
async fn parses_a_single_file_part() {
    let res = post(body(&[("doc", Some("a.txt"), "hello")])).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "doc:a.txt:5");
}

#[tokio::test]
async fn parses_mixed_fields_and_files() {
    let res = post(body(&[
        ("title", None, "My Notes"),
        ("doc", Some("a.txt"), "hello"),
        ("tag", None, "x"),
    ]))
    .await;
    assert_eq!(res.text(), "title::8,doc:a.txt:5,tag::1");
}

#[tokio::test]
async fn field_and_file_accessors() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/u", |form: Multipart| async move {
                let title = form.field("title").unwrap_or_default();
                let file = form.file("doc").and_then(|p| p.text()).unwrap_or_default();
                // `file` must not match a plain field.
                let not_a_file = form.file("title").is_none();
                format!("{title}|{file}|{not_a_file}")
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/u")
        .header(
            "content-type",
            &format!("multipart/form-data; boundary={B}"),
        )
        .body(body(&[
            ("title", None, "Notes"),
            ("doc", Some("a.txt"), "content"),
        ]))
        .send()
        .await;
    assert_eq!(res.text(), "Notes|content|true");
}

#[tokio::test]
async fn binary_content_survives_intact() {
    // A part whose bytes are not UTF-8 must still round-trip.
    let mut raw =
        format!("--{B}\r\nContent-Disposition: form-data; name=\"b\"; filename=\"x.bin\"\r\n\r\n")
            .into_bytes();
    raw.extend_from_slice(&[0x00, 0xFF, 0xFE, 0x41]);
    raw.extend_from_slice(format!("\r\n--{B}--\r\n").as_bytes());

    let app = Churust::server()
        .routing(|r| {
            r.post("/u", |form: Multipart| async move {
                let p = form.file("b").unwrap();
                format!("{}:{:?}", p.bytes.len(), p.text())
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/u")
        .header(
            "content-type",
            &format!("multipart/form-data; boundary={B}"),
        )
        .body(raw)
        .send()
        .await;
    assert_eq!(
        res.text(),
        "4:None",
        "binary must survive and not be lossily decoded"
    );
}

#[tokio::test]
async fn the_wrong_content_type_is_415() {
    let res = TestClient::new(app())
        .post("/u")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn a_missing_boundary_is_415() {
    let res = TestClient::new(app())
        .post("/u")
        .header("content-type", "multipart/form-data")
        .body("whatever")
        .send()
        .await;
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn a_part_without_a_name_is_400() {
    let raw = format!("--{B}\r\nContent-Disposition: form-data\r\n\r\nx\r\n--{B}--\r\n");
    let res = post(raw).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_empty_body_yields_no_parts() {
    let res = post(format!("--{B}--\r\n")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "");
}

#[tokio::test]
async fn a_per_route_body_cap_still_applies() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/u", |form: Multipart| async move {
                format!("{}", form.parts().len())
            })
            .max_body_bytes(16);
        })
        .build();

    let res = TestClient::new(app)
        .post("/u")
        .header(
            "content-type",
            &format!("multipart/form-data; boundary={B}"),
        )
        .body(body(&[(
            "doc",
            Some("a.txt"),
            "a good deal longer than sixteen bytes",
        )]))
        .send()
        .await;
    assert_eq!(
        res.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "an upload route must still honour its cap"
    );
}

#[tokio::test]
async fn a_part_cannot_forge_another_part_by_embedding_the_boundary() {
    // RFC 2046 §5.1.1: a delimiter is CRLF followed by `--boundary`. Splitting
    // on the bare `--boundary` let a part's *content* forge extra parts — one
    // uploaded file could inject a field the client never sent, and the same
    // body then framed differently for any proxy in front.
    let body = concat!(
        "--XYZ\r\n",
        "Content-Disposition: form-data; name=\"file\"\r\n",
        "\r\n",
        // The inner `--XYZ` is NOT preceded by CRLF, so it is content.
        "head--XYZ\r\nContent-Disposition: form-data; name=\"role\"\r\n\r\nadmin\r\n",
        "--XYZ--\r\n"
    );

    let app = Churust::server()
        .routing(|r| {
            r.post("/u", |m: churust_core::multipart::Multipart| async move {
                let names: Vec<&str> = m.parts().iter().map(|p| p.name.as_str()).collect();
                format!("{}:{names:?}", names.len())
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/u")
        .header("content-type", "multipart/form-data; boundary=XYZ")
        .body(body)
        .send()
        .await;

    let text = res.text();
    assert!(
        text.starts_with("1:"),
        "the embedded boundary forged an extra part: {text}"
    );
    assert!(!text.contains("role"), "a forged field appeared: {text}");
}

#[tokio::test]
async fn an_empty_boundary_is_rejected() {
    // A bare `--` delimiter would split the body on every pair of hyphens.
    let app = Churust::server()
        .routing(|r| {
            r.post("/u", |m: churust_core::multipart::Multipart| async move {
                format!("{}", m.parts().len())
            });
        })
        .build();

    for ct in [
        "multipart/form-data; boundary=",
        "multipart/form-data; boundary=\"\"",
    ] {
        let res = TestClient::new(
            Churust::server()
                .routing(|r| {
                    r.post("/u", |m: churust_core::multipart::Multipart| async move {
                        format!("{}", m.parts().len())
                    });
                })
                .build(),
        )
        .post("/u")
        .header("content-type", ct)
        .body("--\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n----\r\n")
        .send()
        .await;
        assert_ne!(res.status(), http::StatusCode::OK, "accepted {ct}");
    }
    let _ = app;
}
