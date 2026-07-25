//! Coverage for `#[churust::main]`.
//!
//! This lives here rather than in `churust-macros` because the expansion names
//! `::churust`, which cannot resolve from inside the macro crate — `churust`
//! depends on `churust-macros`, not the reverse. Here it resolves, and the
//! macro is exercised the way applications actually invoke it.

#[churust::main]
async fn returns_unit() {
    let doubled = churust::tokio::task::spawn(async { 21 * 2 })
        .await
        .expect("spawned task should not panic");
    assert_eq!(doubled, 42);
}

#[churust::main]
async fn returns_result() -> std::io::Result<()> {
    churust::tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    Ok(())
}

#[test]
fn builds_a_runtime_and_runs_the_body() {
    returns_unit();
    returns_result().expect("result-returning main should succeed");
}
