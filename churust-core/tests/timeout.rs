use churust_core::{Call, Churust};
use std::time::Duration;

#[tokio::test]
async fn slow_handler_times_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = Churust::server()
        .host(addr.ip().to_string())
        .port(addr.port())
        .request_timeout_ms(100)
        .routing(|r| {
            r.get("/slow", |_c: Call| async {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                "done"
            });
        })
        .build();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        app.start_on(listener, async move {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET /slow HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf);
    assert!(
        text.starts_with("HTTP/1.1 408"),
        "expected 408, got: {text}"
    );

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}
