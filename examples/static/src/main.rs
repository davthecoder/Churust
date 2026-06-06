use churust::prelude::*;
use churust::Body;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .routing(|r| {
            // Serve files from ./public (create it with an index.html to try).
            r.get(
                "/{path...}",
                StaticFiles::dir("./public").index("index.html").handler(),
            );
            // A streamed dynamic response.
            r.get("/numbers", |_c: Call| async {
                let chunks = futures_util::stream::iter(
                    (1..=5).map(|i| Ok::<_, std::io::Error>(bytes::Bytes::from(format!("{i}\n")))),
                );
                Response::stream("text/plain", Body::from_stream(chunks))
            });
        })
        .start()
        .await
}
