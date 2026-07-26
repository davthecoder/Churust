//! Template rendering over the full pipeline, including loading from disk.

use churust_core::{Call, Churust, TestClient};
use churust_templates::{context, Renderer, TemplateSetupError, Templates};
use http::StatusCode;
use std::path::PathBuf;

/// A throwaway directory holding the files a test needs, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "churust-templates-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn write(&self, relative: &str, contents: &str) {
        let target = self.0.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(target, contents).expect("write template");
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn a_handler_renders_a_registered_template() {
    let app = Churust::server()
        .install(
            Templates::new()
                .add("hello.html", "<h1>Hello, {{ name }}!</h1>")
                .unwrap(),
        )
        .routing(|r| {
            r.get("/", |view: Renderer| async move {
                view.render("hello.html", context! { name => "world" })
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text(), "<h1>Hello, world!</h1>");
    assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
}

#[tokio::test]
async fn markup_in_a_value_is_escaped() {
    let app = Churust::server()
        .install(Templates::new().add("x.html", "<p>{{ note }}</p>").unwrap())
        .routing(|r| {
            r.get("/", |view: Renderer| async move {
                view.render("x.html", context! { note => "<img onerror=alert(1)>" })
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert!(
        !res.text().contains("<img"),
        "an .html template must escape markup: {}",
        res.text()
    );
    assert!(res.text().contains("&lt;img"));
}

#[tokio::test]
async fn templates_load_from_a_directory_with_inheritance() {
    let dir = TempDir::new("inherit");
    dir.write(
        "base.html",
        "<html><body>{% block content %}{% endblock %}</body></html>",
    );
    dir.write(
        "page.html",
        "{% extends \"base.html\" %}{% block content %}<h1>{{ title }}</h1>{% endblock %}",
    );
    dir.write("mail/welcome.html", "Hi {{ name }}");

    let templates = Templates::from_dir(dir.path()).expect("directory should load");
    let app = Churust::server()
        .install(templates)
        .routing(|r| {
            r.get("/", |view: Renderer| async move {
                view.render("page.html", context! { title => "Home" })
            });
            r.get("/names", |view: Renderer| async move {
                view.template_names().join(",")
            });
        })
        .build();
    let client = TestClient::new(app);

    let page = client.get("/").send().await;
    assert_eq!(page.text(), "<html><body><h1>Home</h1></body></html>");

    let names = client.get("/names").send().await;
    assert_eq!(
        names.text(),
        "base.html,mail/welcome.html,page.html",
        "nested templates keep a slash-separated relative name"
    );
}

#[test]
fn a_template_that_does_not_parse_fails_at_startup_with_its_name() {
    let dir = TempDir::new("broken");
    dir.write("fine.html", "ok");
    dir.write("broken.html", "{% for x in %}");

    let err = Templates::from_dir(dir.path()).expect_err("a bad template must not load");
    match err {
        TemplateSetupError::Parse(name, _) => assert_eq!(name, "broken.html"),
        other => panic!("expected a parse error naming the file, got {other:?}"),
    }
}

#[test]
fn an_empty_directory_is_refused() {
    let dir = TempDir::new("empty");
    let err = Templates::from_dir(dir.path()).expect_err("an empty directory is a wrong path");
    assert!(matches!(err, TemplateSetupError::Empty(_)));
}

#[tokio::test]
async fn rendering_an_unknown_template_is_a_500_that_says_nothing_useful_to_a_client() {
    let app = Churust::server()
        .install(Templates::new().add("known.html", "ok").unwrap())
        .routing(|r| {
            r.get("/", |view: Renderer| async move {
                view.render("unknown.html", context! {})
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.text(), "template rendering failed");
    assert!(
        !res.text().contains("unknown.html"),
        "the template name is a filesystem detail and stays out of the body"
    );
}

#[tokio::test]
async fn the_extractor_says_so_when_the_plugin_is_missing() {
    let app = Churust::server()
        .routing(|r| {
            r.get("/", |view: Renderer| async move {
                view.render("x.html", context! {})
            });
        })
        .build();

    let res = TestClient::new(app).get("/").send().await;
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(res.text().contains("Templates plugin is not installed"));
}

#[tokio::test]
async fn a_custom_filter_is_available_to_templates() {
    let templates = Templates::new()
        .add("shout.html", "{{ word | shout }}")
        .unwrap()
        .configure(|env| {
            env.add_filter("shout", |s: String| s.to_uppercase());
        });

    let app = Churust::server()
        .install(templates)
        .routing(|r| {
            r.get("/", |view: Renderer| async move {
                view.render("shout.html", context! { word => "quiet" })
            });
        })
        .build();

    assert_eq!(TestClient::new(app).get("/").send().await.text(), "QUIET");
}

#[tokio::test]
async fn a_template_can_render_an_error_page() {
    let templates = Templates::new()
        .add("404.html", "<h1>No such page: {{ path }}</h1>")
        .unwrap();

    let app = Churust::server()
        .install(templates)
        .routing(|r| {
            r.get("/", |_c: Call| async { "home" });
            r.get("/missing-page", |view: Renderer, call: Call| async move {
                view.render_with_status(
                    StatusCode::NOT_FOUND,
                    "404.html",
                    context! { path => call.path().to_string() },
                )
            });
        })
        .build();

    let res = TestClient::new(app).get("/missing-page").send().await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // The slash comes back as `&#x2f;`: the escaper does not trust a value
    // interpolated into markup to be free of anything that could close a tag,
    // and a path is caller-controlled.
    assert_eq!(res.text(), "<h1>No such page: &#x2f;missing-page</h1>");
}
