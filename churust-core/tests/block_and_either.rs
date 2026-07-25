//! `block` for offloading blocking work, and the `Either` extractor.

use churust_core::{block, Churust, Either, Form, TestClient};
use serde::Deserialize;

#[derive(Deserialize)]
struct Note {
    text: String,
}

#[tokio::test]
async fn block_runs_work_off_the_async_runtime() {
    let out = block(|| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        21 * 2
    })
    .await
    .unwrap();
    assert_eq!(out, 42);
}

#[tokio::test]
async fn a_panic_in_blocking_work_becomes_an_error_not_a_crash() {
    let err = block(|| panic!("boom")).await.unwrap_err();
    assert_eq!(err.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn block_is_usable_from_a_handler() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/hash", |_c: churust_core::Call| async {
                let v = block(|| (1..=100u64).sum::<u64>()).await?;
                Ok::<_, churust_core::Error>(format!("{v}"))
            });
        })
        .build();

    assert_eq!(
        TestClient::new(app).get("/hash").send().await.text(),
        "5050"
    );
}

fn either_app() -> churust_core::App {
    Churust::server()
        .routing(|r| {
            r.post("/notes", |body: Either<Form<Note>, String>| async move {
                match body {
                    Either::Left(Form(n)) => format!("form:{}", n.text),
                    Either::Right(raw) => format!("raw:{raw}"),
                }
            });
        })
        .build()
}

#[tokio::test]
async fn either_takes_the_left_branch_when_it_matches() {
    let res = TestClient::new(either_app())
        .post("/notes")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("text=hello")
        .send()
        .await;
    assert_eq!(res.text(), "form:hello");
}

#[tokio::test]
async fn either_falls_through_to_the_right_branch() {
    // Wrong content type for Form, so the fallback must still see the body —
    // proving the first attempt did not consume it.
    let res = TestClient::new(either_app())
        .post("/notes")
        .header("content-type", "text/plain")
        .body("just text")
        .send()
        .await;
    assert_eq!(res.text(), "raw:just text");
}

#[tokio::test]
async fn either_reports_when_both_branches_fail() {
    let app = Churust::server()
        .routing(|r| {
            r.post("/strict", |b: Either<Form<Note>, Form<Note>>| async move {
                match b {
                    Either::Left(Form(n)) | Either::Right(Form(n)) => n.text,
                }
            });
        })
        .build();

    let res = TestClient::new(app)
        .post("/strict")
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await;
    assert_eq!(res.status(), http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
