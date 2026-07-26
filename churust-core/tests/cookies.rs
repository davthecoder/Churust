//! Cookie parsing and `Set-Cookie` building.

use churust_core::{Call, Churust, Cookie, Response, SameSite, TestClient};

#[tokio::test]
async fn reads_a_cookie_from_the_request() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |c: Call| async move {
                c.cookie("session").unwrap_or_else(|| "none".into())
            });
        })
        .build();

    let res = TestClient::new(app)
        .get("/")
        .header("cookie", "theme=dark; session=abc123; other=x")
        .send()
        .await;
    assert_eq!(res.text(), "abc123");
}

#[tokio::test]
async fn a_missing_cookie_is_none() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |c: Call| async move {
                c.cookie("nope").unwrap_or_else(|| "none".into())
            });
        })
        .build();
    assert_eq!(TestClient::new(app).get("/").send().await.text(), "none");
}

#[tokio::test]
async fn values_are_percent_decoded() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |c: Call| async move {
                c.cookie("greeting").unwrap_or_default()
            });
        })
        .build();

    let res = TestClient::new(app)
        .get("/")
        .header("cookie", "greeting=hello%20world")
        .send()
        .await;
    assert_eq!(res.text(), "hello world");
}

#[tokio::test]
async fn builds_a_set_cookie_header_with_safe_defaults() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async {
                Response::text("ok").with_cookie(Cookie::new("session", "abc"))
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    let sc = res.header("set-cookie").expect("Set-Cookie missing");

    assert!(sc.starts_with("session=abc"), "got {sc}");
    // Defaults are the safe ones: a cookie is a credential unless told otherwise.
    assert!(sc.contains("HttpOnly"), "HttpOnly should default on: {sc}");
    assert!(
        sc.contains("SameSite=Lax"),
        "SameSite should default to Lax: {sc}"
    );
    assert!(sc.contains("Path=/"), "Path should default to /: {sc}");
}

#[tokio::test]
async fn attributes_can_be_set() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async {
                Response::text("ok").with_cookie(
                    Cookie::new("id", "v")
                        .path("/admin")
                        .domain("example.com")
                        .max_age(3600)
                        .secure(true)
                        .http_only(false)
                        .same_site(SameSite::Strict),
                )
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    let sc = res.header("set-cookie").unwrap();

    assert!(sc.contains("Path=/admin"), "{sc}");
    assert!(sc.contains("Domain=example.com"), "{sc}");
    assert!(sc.contains("Max-Age=3600"), "{sc}");
    assert!(sc.contains("Secure"), "{sc}");
    assert!(!sc.contains("HttpOnly"), "{sc}");
    assert!(sc.contains("SameSite=Strict"), "{sc}");
}

#[tokio::test]
async fn removal_expires_the_cookie() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async {
                Response::text("bye").with_cookie(Cookie::removal("session"))
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    let sc = res.header("set-cookie").unwrap();
    assert!(sc.contains("Max-Age=0"), "a removal must expire it: {sc}");
}

#[tokio::test]
async fn values_needing_escapes_are_encoded() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async {
                Response::text("ok").with_cookie(Cookie::new("x", "a;b c"))
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    let sc = res.header("set-cookie").unwrap();
    assert!(
        !sc.starts_with("x=a;b c"),
        "a raw ';' would forge a second attribute: {sc}"
    );
    assert!(sc.contains("a%3Bb%20c"), "{sc}");
}

#[tokio::test]
async fn several_cookies_produce_several_headers() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |_c: Call| async {
                Response::text("ok")
                    .with_cookie(Cookie::new("a", "1"))
                    .with_cookie(Cookie::new("b", "2"))
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    let all: Vec<_> = res.headers().get_all("set-cookie").iter().collect();
    assert_eq!(all.len(), 2, "each cookie needs its own Set-Cookie header");
}

#[tokio::test]
async fn a_cookie_in_a_second_cookie_header_field_is_found() {
    // HTTP/2 permits cookie crumbs as separate header fields (RFC 9113 §8.2.3),
    // and an HTTP/1.1 client may send more than one `Cookie` line. Reading only
    // the first field meant a session cookie landing in the second was
    // invisible, so every request looked freshly anonymous — a silent logout.
    use bytes::Bytes;
    use http::{header::COOKIE, HeaderMap, HeaderValue, Method};

    let mut headers = HeaderMap::new();
    headers.append(COOKIE, HeaderValue::from_static("theme=dark"));
    headers.append(COOKIE, HeaderValue::from_static("churust_session=abc123"));

    let call = churust_core::Call::new(Method::GET, "/".parse().unwrap(), headers, Bytes::new());

    assert_eq!(call.cookie("theme").as_deref(), Some("dark"));
    assert_eq!(
        call.cookie("churust_session").as_deref(),
        Some("abc123"),
        "a cookie in the second header field was not found"
    );
    assert_eq!(call.cookie("absent"), None);
}

mod attribute_injection {
    use churust_core::cookie::Cookie;

    /// The attribute names a browser would actually parse out of the header.
    ///
    /// Attributes are delimited by `;`, so this is the only question that
    /// matters: a forged name that ends up *inside* another attribute's value
    /// is inert, however alarming the raw string looks.
    fn attribute_names(rendered: &str) -> Vec<String> {
        rendered
            .split(';')
            .skip(1) // the name=value pair
            .map(|a| {
                a.trim()
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
            })
            .collect()
    }

    #[test]
    fn a_semicolon_in_the_path_cannot_forge_attributes() {
        // The realistic shape: an app scoping a cookie to a user-controlled
        // area, `.path(format!("/u/{slug}"))`. A slug carrying `; Path=/;
        // Max-Age=0` would otherwise delete the victim's session cookie, and
        // the same trick reaches `Secure`, `Domain` and a second `Path`.
        let rendered = Cookie::new("sid", "tok")
            .path("/u/x; Path=/; Max-Age=0")
            .to_header_value();

        let names = attribute_names(&rendered);
        assert!(
            !names.contains(&"max-age".to_string()),
            "a forged Max-Age attribute survived: {rendered}"
        );
        assert_eq!(
            names.iter().filter(|n| *n == "path").count(),
            1,
            "more than one Path attribute: {rendered}"
        );
        assert!(rendered.starts_with("sid=tok; Path=/u/x"), "{rendered}");
    }

    #[test]
    fn a_semicolon_in_the_domain_cannot_forge_attributes() {
        let rendered = Cookie::new("sid", "tok")
            .domain("example.com; Secure")
            .to_header_value();
        assert!(
            !attribute_names(&rendered).contains(&"secure".to_string()),
            "a forged Secure attribute survived: {rendered}"
        );
    }

    #[test]
    fn control_characters_cannot_split_the_header() {
        // A CRLF in an attribute is header injection if it reaches the wire.
        let rendered = Cookie::new("sid", "tok")
            .path("/a\r\nX-Injected: yes")
            .to_header_value();
        assert!(!rendered.contains('\r'), "{rendered}");
        assert!(!rendered.contains('\n'), "{rendered}");
        assert!(
            rendered.contains("Path=/a"),
            "the cookie should still be issued: {rendered}"
        );
    }

    #[test]
    fn an_ordinary_path_and_domain_are_untouched() {
        let rendered = Cookie::new("sid", "tok")
            .path("/app/v1")
            .domain("sub.example.com")
            .to_header_value();
        assert!(rendered.contains("; Path=/app/v1"), "{rendered}");
        assert!(rendered.contains("; Domain=sub.example.com"), "{rendered}");
    }

    #[test]
    fn a_sanitised_cookie_still_reaches_the_wire() {
        // The pre-existing behaviour for a CRLF was to drop the whole
        // Set-Cookie silently, so the session was never issued and nothing
        // said why. Sanitising keeps the cookie.
        let rendered = Cookie::new("sid", "tok")
            .path("/a\r\nevil")
            .to_header_value();
        assert!(
            http::HeaderValue::from_str(&rendered).is_ok(),
            "not a valid header value: {rendered:?}"
        );
    }
}
