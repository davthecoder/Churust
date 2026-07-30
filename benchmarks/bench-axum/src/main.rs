//! The other half of the comparison. Same three routes, same bodies, same
//! content types. `run.sh` refuses to measure if they diverge.

use axum::{
    extract::Path,
    http::{header, HeaderValue},
    response::IntoResponse,
    routing::get,
    Router,
};

async fn plaintext() -> &'static str {
    "Hello, World!"
}

async fn json() -> impl IntoResponse {
    // `HeaderValue::from_static`, not the `&str` tuple form: `[(header::CONTENT_TYPE,
    // "application/json")]` resolves through `V: TryInto<HeaderValue>`, which for
    // `&str` goes via `HeaderValue::try_from` -> `Bytes::copy_from_slice` — a
    // per-request heap copy for a compile-time constant. `from_static` is zero-copy,
    // matching Churust's side (`Response::bytes`, which also uses
    // `HeaderValue::from_static`).
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        r#"{"message":"Hello, World!"}"#,
    )
}

async fn user(Path(id): Path<u64>) -> String {
    format!("user {id}")
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .expect("PORT must be set")
        .parse()
        .expect("PORT must be a number");

    let app = Router::new()
        .route("/plaintext", get(plaintext))
        .route("/json", get(json))
        .route("/user/{id}", get(user));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    axum::serve(listener, app).await.expect("serve");
}
