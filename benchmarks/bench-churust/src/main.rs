//! One half of the comparison. The routes here must stay byte-identical to
//! `bench-axum`'s — `run.sh` refuses to measure if they diverge.

use churust_core::{Call, Churust};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("PORT")
        .expect("PORT must be set")
        .parse()
        .expect("PORT must be a number");

    let app = Churust::server()
        .host("127.0.0.1")
        .port(port)
        // This comparison measures dispatch overhead between two frameworks.
        // Churust's default builder sends five security headers
        // (`X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`,
        // `Permissions-Policy`, `Cross-Origin-Resource-Policy`) that axum's
        // bare `Router` does not; left in place, the two apps would be doing
        // different work, and any throughput gap would partly be measuring
        // that difference rather than the frameworks' dispatch paths. Of the
        // two honest ways to equalise — add the headers to axum (pulling in
        // tower-http or hand-rolling middleware) or remove them here (one
        // line, no new dependency) — this is the cheaper one. Nothing is
        // hidden by dropping them: their cost is already measured directly in
        // `churust-core/benches/headers.rs`, as the `security_headers_on` vs.
        // `security_headers_off` pair. That bench measures the headers; this
        // comparison measures dispatch. Do not add this back "to be safe" —
        // doing so reintroduces the exact confound this comment describes.
        .without_security_headers()
        .routing(|r| {
            r.get("/plaintext", |_c: Call| async { "Hello, World!" });
            r.get("/json", |_c: Call| async {
                // Explicit rather than a `json` helper: churust-core has no
                // JSON response constructor (that lives in churust-json, which
                // this app deliberately does not pull in — the comparison is of
                // core dispatch, not of a plugin).
                churust_core::Response::text(r#"{"message":"Hello, World!"}"#).with_header(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/json"),
                )
            });
            r.get("/user/{id}", |churust_core::Path(id): churust_core::Path<u64>| async move {
                format!("user {id}")
            });
        })
        .build();

    app.start().await
}
