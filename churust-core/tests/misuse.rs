//! Guards against configurations that were previously accepted silently.

use churust_core::{Call, Churust};

#[test]
#[should_panic(expected = "duplicate route")]
fn registering_the_same_route_twice_panics() {
    // Previously the second registration replaced the first via HashMap::insert
    // and the first handler simply never ran — a typo that produced a route
    // which mysteriously did nothing.
    Churust::server().routing(|r| {
        r.get("/dup", |_c: Call| async { "first" });
        r.get("/dup", |_c: Call| async { "second" });
    });
}

#[test]
fn the_same_path_with_different_methods_is_fine() {
    Churust::server().routing(|r| {
        r.get("/x", |_c: Call| async { "get" });
        r.post("/x", |_c: Call| async { "post" });
        r.put("/x", |_c: Call| async { "put" });
    });
}

#[test]
fn equivalent_paths_across_scopes_still_collide() {
    // /api + /v1/thing and /api/v1 + /thing are the same route.
    let build = || {
        Churust::server().routing(|r| {
            r.route("/api", |r| {
                r.get("/v1/thing", |_c: Call| async { "a" });
            });
            r.route("/api/v1", |r| {
                r.get("/thing", |_c: Call| async { "b" });
            });
        });
    };
    assert!(
        std::panic::catch_unwind(build).is_err(),
        "a collision assembled from different scopes is still a collision"
    );
}

#[cfg(feature = "fs")]
#[test]
#[should_panic(expected = "static root")]
fn static_files_rejects_a_missing_root_at_build_time() {
    use churust_core::fs::StaticFiles;
    // Previously this produced a 500 on every single request, forever — a
    // configuration mistake reported as a runtime fault.
    let _ = StaticFiles::dir("/definitely/not/a/real/directory/churust").handler();
}

#[cfg(feature = "fs")]
#[test]
fn static_files_accepts_a_real_root() {
    use churust_core::fs::StaticFiles;
    let root = std::env::temp_dir().join(format!("churust-misuse-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let _ = StaticFiles::dir(&root).handler();
}

#[test]
fn constant_time_comparison_is_available_and_correct() {
    use churust_core::secure_compare;

    assert!(secure_compare("hunter2", "hunter2"));
    assert!(!secure_compare("hunter2", "hunter3"));
    assert!(!secure_compare("short", "much longer value"));
    assert!(secure_compare("", ""));
}
