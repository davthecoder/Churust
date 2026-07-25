//! Request limits beyond the body cap.

use churust_core::{Call, Churust, TestClient};
use http::StatusCode;

#[tokio::test]
async fn deep_paths_are_rejected() {
    let app = Churust::server()
        .max_path_segments(8)
        .routing(|r| {
            r.get("/{p...}", |_c: Call| async { "ok" });
        })
        .build();
    let client = TestClient::new(app);

    let shallow = format!("/{}", ["a"; 8].join("/"));
    assert_eq!(client.get(shallow).send().await.status(), StatusCode::OK);

    let deep = format!("/{}", ["a"; 9].join("/"));
    assert_eq!(
        client.get(deep).send().await.status(),
        StatusCode::URI_TOO_LONG,
        "a path past the segment cap must be refused before routing walks it"
    );
}

#[tokio::test]
async fn the_segment_cap_is_counted_after_decoding_not_before() {
    // %2F does not create segments, so an encoded slash must not inflate the
    // count and trip the limit on an otherwise-short path.
    let app = Churust::server()
        .max_path_segments(3)
        .routing(|r| {
            r.get("/u/{name}", |c: Call| async move {
                c.param_raw("name").unwrap_or("").to_string()
            });
        })
        .build();

    let res = TestClient::new(app).get("/u/a%2Fb%2Fc%2Fd").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "a/b/c/d");
}

#[tokio::test]
async fn limits_are_visible_on_the_built_config() {
    let app = Churust::server()
        .max_headers(7)
        .header_read_timeout_ms(1234)
        .max_path_segments(9)
        .build();

    assert_eq!(app.config().max_headers, 7);
    assert_eq!(app.config().header_read_timeout_ms, 1234);
    assert_eq!(app.config().max_path_segments, 9);
}

#[tokio::test]
async fn defaults_are_conservative() {
    let app = Churust::server().build();
    let c = app.config();
    assert_eq!(c.max_headers, 100);
    assert_eq!(c.max_path_segments, 64);
    assert_eq!(c.header_read_timeout_ms, 10_000);
    assert_eq!(c.ws_max_frame_bytes, 1 << 20);
    assert_eq!(c.ws_max_message_bytes, 4 << 20);
}

#[test]
fn limits_come_from_the_environment_too() {
    use churust_core::Config;
    use std::collections::HashMap;

    let env: HashMap<&str, &str> = [
        ("CHURUST_SERVER_MAX_HEADERS", "12"),
        ("CHURUST_SERVER_MAX_PATH_SEGMENTS", "5"),
        ("CHURUST_SERVER_HEADER_READ_TIMEOUT_MS", "900"),
        ("CHURUST_SERVER_WS_MAX_FRAME_BYTES", "2048"),
        ("CHURUST_SERVER_WS_MAX_MESSAGE_BYTES", "4096"),
    ]
    .into_iter()
    .collect();

    let mut cfg = Config::default();
    cfg.apply_env(|k| env.get(k).map(|s| s.to_string()));

    assert_eq!(cfg.server.max_headers, 12);
    assert_eq!(cfg.server.max_path_segments, 5);
    assert_eq!(cfg.server.header_read_timeout_ms, 900);
    assert_eq!(cfg.server.ws_max_frame_bytes, 2048);
    assert_eq!(cfg.server.ws_max_message_bytes, 4096);
}

#[tokio::test]
async fn server_tuning_is_configurable() {
    let app = Churust::server()
        .keep_alive_ms(5_000)
        .backlog(64)
        .shutdown_timeout_ms(1_500)
        .build();

    assert_eq!(app.config().keep_alive_ms, 5_000);
    assert_eq!(app.config().backlog, 64);
    assert_eq!(app.config().shutdown_timeout_ms, 1_500);
}

#[tokio::test]
async fn server_tuning_defaults() {
    let c = Churust::server().build();
    let c = c.config();
    assert_eq!(c.keep_alive_ms, 75_000);
    assert_eq!(c.backlog, 1024);
    assert_eq!(c.shutdown_timeout_ms, 30_000);
}

#[test]
fn server_tuning_comes_from_the_environment() {
    use churust_core::Config;
    use std::collections::HashMap;

    let env: HashMap<&str, &str> = [
        ("CHURUST_SERVER_KEEP_ALIVE_MS", "1000"),
        ("CHURUST_SERVER_BACKLOG", "32"),
        ("CHURUST_SERVER_SHUTDOWN_TIMEOUT_MS", "2000"),
    ]
    .into_iter()
    .collect();

    let mut cfg = Config::default();
    cfg.apply_env(|k| env.get(k).map(|s| s.to_string()));

    assert_eq!(cfg.server.keep_alive_ms, 1000);
    assert_eq!(cfg.server.backlog, 32);
    assert_eq!(cfg.server.shutdown_timeout_ms, 2000);
}
