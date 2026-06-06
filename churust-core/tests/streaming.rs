use churust_core::{Body, Call, Churust, Response};
use std::time::Duration;

#[tokio::test]
async fn streamed_response_reaches_client_in_full() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .routing(|r| {
            r.get("/big", |_c: Call| async {
                let chunks =
                    futures_util::stream::iter((0..5).map(|i| {
                        Ok::<_, std::io::Error>(bytes::Bytes::from(format!("chunk{i};")))
                    }));
                Response::stream("text/plain", Body::from_stream(chunks))
            });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_with_shutdown(async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET /big HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
    // The server uses chunked transfer-encoding for streamed bodies; verify all
    // five chunk payloads appear somewhere in the raw response.
    for i in 0..5usize {
        assert!(
            text.contains(&format!("chunk{i};")),
            "chunk{i} missing in streamed body: {text}"
        );
    }

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}
