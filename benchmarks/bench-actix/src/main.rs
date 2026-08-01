//! One of five halves of the comparison. The three routes here must stay
//! byte-identical to every other bench app's — `run.sh` refuses to measure if
//! they diverge.
//!
//! actix-web brings its own HTTP stack (actix-http) and its own runtime shape
//! (one single-threaded tokio runtime per worker thread, `SO_REUSEPORT`
//! across them) rather than sharing hyper and tokio with Churust and axum. It
//! is in the comparison for exactly that reason: it is the fastest widely-used
//! Rust server, and a Churust number that only beats hyper-based peers has not
//! been tested against the strongest thing available.

use actix_web::{http::header, web, App, HttpResponse, HttpServer};

async fn plaintext() -> HttpResponse {
    // `HttpResponse::Ok().content_type(...)` would parse the type string per
    // request; the static header value is the zero-copy path, matching
    // Churust's `Response::bytes` and axum's `HeaderValue::from_static`.
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain; charset=utf-8"),
        ))
        .body("Hello, World!")
}

async fn json() -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        ))
        .body(r#"{"message":"Hello, World!"}"#)
}

async fn user(id: web::Path<u64>) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain; charset=utf-8"),
        ))
        .body(format!("user {id}"))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT")
        .expect("PORT must be set")
        .parse()
        .expect("PORT must be a number");

    HttpServer::new(|| {
        App::new()
            .route("/plaintext", web::get().to(plaintext))
            .route("/json", web::get().to(json))
            .route("/user/{id}", web::get().to(user))
    })
    .bind(("127.0.0.1", port))?
    .run()
    .await
}
