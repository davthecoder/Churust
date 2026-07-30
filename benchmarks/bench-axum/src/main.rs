//! The other half of the comparison. Same three routes, same bodies, same
//! content types. `run.sh` refuses to measure if they diverge.

use axum::{extract::Path, http::header, response::IntoResponse, routing::get, Router};

async fn plaintext() -> &'static str {
    "Hello, World!"
}

async fn json() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
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
