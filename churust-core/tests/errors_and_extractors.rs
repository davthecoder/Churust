//! `IntoError` conversion, plus the `Form<T>` and `Header<T>` extractors.

use churust_core::{Call, Churust, Form, IntoError, TestClient};
use http::StatusCode;
use serde::Deserialize;

#[derive(Deserialize)]
struct Login {
    user: String,
    pass: String,
}

// A user error type, the kind `?` previously could not carry.
#[derive(Debug)]
enum StoreError {
    NotFound,
    Backend(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "no such row"),
            // Exactly the kind of text that must not reach a client.
            StoreError::Backend(d) => write!(f, "connection to db-primary:5432 failed: {d}"),
        }
    }
}

impl IntoError for StoreError {
    fn status(&self) -> StatusCode {
        match self {
            StoreError::NotFound => StatusCode::NOT_FOUND,
            StoreError::Backend(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[tokio::test]
async fn question_mark_works_on_a_foreign_error() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/missing", |_c: Call| async {
                let v: Result<&str, StoreError> = Err(StoreError::NotFound);
                let s = v?; // this is the whole point
                Ok::<_, churust_core::Error>(s)
            });
        })
        .build();

    let res = TestClient::new(app).get("/missing").send().await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_display_text_is_not_leaked_by_default() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/boom", |_c: Call| async {
                let v: Result<&str, StoreError> = Err(StoreError::Backend("timeout".into()));
                let s = v?;
                Ok::<_, churust_core::Error>(s)
            });
        })
        .build();

    let res = TestClient::new(app).get("/boom").send().await;
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = res.text();
    assert!(
        !body.contains("db-primary"),
        "internal detail reached the client: {body}"
    );
    assert!(!body.contains("timeout"), "internal detail leaked: {body}");
}

#[tokio::test]
async fn a_type_can_opt_into_showing_its_message() {
    #[derive(Debug)]
    struct Public(String);
    impl std::fmt::Display for Public {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl IntoError for Public {
        fn status(&self) -> StatusCode {
            StatusCode::BAD_REQUEST
        }
        fn message(&self) -> String {
            self.0.clone()
        }
    }

    let app = Churust::server()
        .routing(|r| {
            r.get("/x", |_c: Call| async {
                let v: Result<&str, Public> = Err(Public("pick a smaller page size".into()));
                let s = v?;
                Ok::<_, churust_core::Error>(s)
            });
        })
        .build();

    let res = TestClient::new(app).get("/x").send().await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(res.text().contains("pick a smaller page size"));
}

#[tokio::test]
async fn form_extractor_reads_urlencoded_bodies() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/login", |Form(l): Form<Login>| async move {
                format!("{}:{}", l.user, l.pass)
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("user=ana&pass=s3cret")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "ana:s3cret");
}

#[tokio::test]
async fn form_decodes_percent_escapes_and_plus() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/login", |Form(l): Form<Login>| async move {
                format!("{}|{}", l.user, l.pass)
            });
        })
        .build();

    // In a form body `+` *is* a space — the opposite of a path segment.
    let res = TestClient::new(app)
        .post("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("user=ana+maria&pass=a%26b")
        .send()
        .await;

    assert_eq!(res.text(), "ana maria|a&b");
}

#[tokio::test]
async fn form_rejects_the_wrong_content_type() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/login", |Form(l): Form<Login>| async move { l.user });
        })
        .build();

    let res = TestClient::new(app)
        .post("/login")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn form_rejects_a_body_that_does_not_deserialize() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/login", |Form(l): Form<Login>| async move { l.user });
        })
        .build();

    let res = TestClient::new(app)
        .post("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("user=ana") // `pass` missing
        .send()
        .await;

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

struct ApiVersion;
impl churust_core::HeaderName for ApiVersion {
    const NAME: &'static str = "x-api-version";
}

#[tokio::test]
async fn header_extractor_parses_a_named_header() {
    use churust_core::Header;

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |Header(v, _): Header<u32, ApiVersion>| async move {
                format!("v{v}")
            });
        })
        .build();

    let res = TestClient::new(app)
        .get("/")
        .header("x-api-version", "3")
        .send()
        .await;
    assert_eq!(res.text(), "v3");
}

#[tokio::test]
async fn a_missing_or_unparseable_header_is_400() {
    use churust_core::Header;

    let app = Churust::server()
        .routing(|r| {
            r.get("/", |Header(v, _): Header<u32, ApiVersion>| async move {
                format!("v{v}")
            });
        })
        .build();
    let client = TestClient::new(app);

    assert_eq!(
        client.get("/").send().await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .get("/")
            .header("x-api-version", "not-a-number")
            .send()
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}
