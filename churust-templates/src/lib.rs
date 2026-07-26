//! Server-rendered HTML for the [Churust] web framework, on [minijinja].
//!
//! Install [`Templates`], then take a [`Renderer`] in any handler:
//!
//! ```
//! use churust_core::{Churust, TestClient};
//! use churust_templates::{context, Renderer, Templates};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(
//!         Templates::new()
//!             .add("hello.html", "<h1>Hello, {{ name }}!</h1>")
//!             .expect("template should parse"),
//!     )
//!     .routing(|r| {
//!         r.get("/", |view: Renderer| async move {
//!             view.render("hello.html", context! { name => "world" })
//!         });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app).get("/").send().await;
//! assert_eq!(res.text(), "<h1>Hello, world!</h1>");
//! assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
//! # });
//! ```
//!
//! # Escaping
//!
//! Auto-escaping follows the template's extension: `.html`, `.htm` and `.xml`
//! are escaped, everything else is not. Name an HTML template `.html` and
//! interpolated values cannot inject markup. A template named `page.txt`
//! rendering into a browser is a stored XSS waiting to happen, which is why
//! [`Templates::add`] and [`Templates::from_dir`] keep the extension you give
//! them rather than normalising it away.
//!
//! # Templates are parsed at startup
//!
//! [`Templates::from_dir`] reads and parses every template when the server is
//! built, so a syntax error is a startup failure with a filename in it rather
//! than a `500` on the one route nobody visits until Friday. The cost is that a
//! changed template needs a restart, which is the same trade the rest of the
//! framework makes for routes and static roots.
//!
//! [Churust]: churust_core::Churust
//! [minijinja]: https://docs.rs/minijinja

#![deny(missing_docs)]

use async_trait::async_trait;
use churust_core::{AppBuilder, Call, Error, FromCallParts, Plugin, Response, Result};
use http::header::CONTENT_TYPE;
use http::{HeaderValue, StatusCode};
use minijinja::Environment;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

/// Build a template context inline.
///
/// Re-exported from minijinja so applications do not need their own dependency
/// on it for the common case.
///
/// ```
/// use churust_templates::context;
///
/// let ctx = context! { title => "Home", items => vec![1, 2, 3] };
/// # let _ = ctx;
/// ```
pub use minijinja::context;

/// The template environment, shared by every handler.
///
/// Construct with [`Templates::new`] or [`Templates::from_dir`], then hand it to
/// [`AppBuilder::install`](churust_core::AppBuilder::install).
pub struct Templates {
    env: Environment<'static>,
}

impl std::fmt::Debug for Templates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Templates")
            .field("loaded", &self.env.templates().count())
            .finish_non_exhaustive()
    }
}

impl Default for Templates {
    fn default() -> Self {
        Self::new()
    }
}

impl Templates {
    /// An empty environment. Add templates with [`add`](Templates::add).
    pub fn new() -> Self {
        Self {
            env: Environment::new(),
        }
    }

    /// Add one template from a string, parsing it now.
    ///
    /// The name carries the extension that decides auto-escaping, so call it
    /// `something.html` for HTML.
    ///
    /// # Errors
    ///
    /// If the source does not parse. The error names the line.
    pub fn add(
        mut self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> std::result::Result<Self, minijinja::Error> {
        self.env.add_template_owned(name.into(), source.into())?;
        Ok(self)
    }

    /// Load and parse every template under `dir`, recursively.
    ///
    /// Template names are paths relative to `dir` with forward slashes, so
    /// `templates/mail/welcome.html` is `mail/welcome.html`, which is also what
    /// `{% extends %}` and `{% include %}` inside them refer to.
    ///
    /// # Errors
    ///
    /// If `dir` is not a readable directory, or any template fails to parse.
    /// Both are startup failures on purpose: a template that cannot be parsed
    /// is a route that cannot answer, and finding that out at boot is cheaper
    /// than finding out in production.
    pub fn from_dir(dir: impl AsRef<Path>) -> std::result::Result<Self, TemplateSetupError> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Err(TemplateSetupError::MissingDir(dir.display().to_string()));
        }

        let mut templates = Self::new();
        let mut pending = vec![dir.to_path_buf()];
        let mut found = 0usize;

        while let Some(current) = pending.pop() {
            let entries = std::fs::read_dir(&current).map_err(|e| {
                TemplateSetupError::Io(current.display().to_string(), e.to_string())
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| {
                    TemplateSetupError::Io(current.display().to_string(), e.to_string())
                })?;
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let source = std::fs::read_to_string(&path).map_err(|e| {
                    TemplateSetupError::Io(path.display().to_string(), e.to_string())
                })?;
                let name = relative_name(dir, &path);
                templates = templates
                    .add(name.clone(), source)
                    .map_err(|e| TemplateSetupError::Parse(name, e.to_string()))?;
                found += 1;
            }
        }

        if found == 0 {
            return Err(TemplateSetupError::Empty(dir.display().to_string()));
        }
        Ok(templates)
    }

    /// Reach the underlying environment to register filters, tests, globals or
    /// a custom auto-escape policy.
    ///
    /// ```
    /// use churust_templates::Templates;
    ///
    /// let templates = Templates::new().configure(|env| {
    ///     env.add_filter("shout", |s: String| s.to_uppercase());
    /// });
    /// ```
    pub fn configure(mut self, f: impl FnOnce(&mut Environment<'static>)) -> Self {
        f(&mut self.env);
        self
    }
}

/// Why a template environment could not be built at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSetupError {
    /// The directory does not exist or is not a directory.
    MissingDir(String),
    /// The directory exists but holds no templates, which is almost always a
    /// wrong path rather than an intentionally empty site.
    Empty(String),
    /// A file could not be read.
    Io(String, String),
    /// A template could not be parsed. Carries the template name and the
    /// parser's message, which includes the line.
    Parse(String, String),
}

impl std::fmt::Display for TemplateSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDir(path) => write!(f, "template directory not found: {path}"),
            Self::Empty(path) => write!(f, "template directory holds no templates: {path}"),
            Self::Io(path, why) => write!(f, "could not read {path}: {why}"),
            Self::Parse(name, why) => write!(f, "template {name} failed to parse: {why}"),
        }
    }
}

impl std::error::Error for TemplateSetupError {}

/// A template name relative to the root, with forward slashes on every
/// platform so `{% include %}` reads the same in a repository as it does in a
/// container.
fn relative_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl Plugin for Templates {
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        // Held as application state rather than per-call data: one environment
        // serves every request, and parsing happened at startup.
        app.insert_state(self.env);
    }
}

/// Renders templates from the installed [`Templates`] environment.
///
/// Extract it in a handler. Every method returns a [`Response`] with
/// `Content-Type: text/html; charset=utf-8`.
#[derive(Clone)]
pub struct Renderer(Arc<Environment<'static>>);

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("templates", &self.0.templates().count())
            .finish()
    }
}

impl Renderer {
    /// Render `name` with `ctx` into a `200 OK` HTML response.
    ///
    /// # Errors
    ///
    /// If the template is unknown or rendering fails. The client is told only
    /// that rendering failed: a minijinja error names the template file, the
    /// line and often the offending variable, and none of that belongs in a
    /// response body. The detail is attached as the error's source for the
    /// application's own logging.
    pub fn render(&self, name: &str, ctx: impl Serialize) -> Result<Response> {
        let rendered = self
            .0
            .get_template(name)
            .and_then(|t| t.render(ctx))
            .map_err(|e| {
                Error::internal("template rendering failed")
                    .with_source(RenderFailure(format!("{name}: {e:#}")))
            })?;

        let mut res = Response::text(rendered);
        res.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        Ok(res)
    }

    /// Render `name` with `ctx` and a status other than `200`.
    ///
    /// The obvious use is a `404` or `500` page from an
    /// [`on_error`](churust_core::AppBuilder::on_error) hook.
    pub fn render_with_status(
        &self,
        status: StatusCode,
        name: &str,
        ctx: impl Serialize,
    ) -> Result<Response> {
        Ok(self.render(name, ctx)?.with_status(status))
    }

    /// The names of every loaded template, for a health endpoint or a test.
    pub fn template_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .0
            .templates()
            .map(|(name, _)| name.to_string())
            .collect();
        names.sort();
        names
    }
}

/// The detail behind a rendering failure, kept out of the response body.
#[derive(Debug)]
struct RenderFailure(String);

impl std::fmt::Display for RenderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderFailure {}

#[async_trait]
impl FromCallParts for Renderer {
    async fn from_call_parts(call: &mut Call) -> Result<Self> {
        match call.state::<Environment<'static>>() {
            Some(env) => Ok(Renderer(env)),
            None => Err(Error::internal(
                "the Templates plugin is not installed on this server",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_that_does_not_parse_is_rejected_when_added() {
        let err = Templates::new()
            .add("broken.html", "{% for x in %}")
            .expect_err("an unfinished tag should not be accepted");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn a_missing_directory_is_a_startup_error() {
        let err = Templates::from_dir("does/not/exist").unwrap_err();
        assert!(matches!(err, TemplateSetupError::MissingDir(_)));
        assert!(err.to_string().contains("does/not/exist"));
    }

    #[test]
    fn names_are_relative_and_slash_separated() {
        let root = Path::new("/srv/templates");
        let path = Path::new("/srv/templates/mail/welcome.html");
        assert_eq!(relative_name(root, path), "mail/welcome.html");
    }

    #[test]
    fn html_templates_escape_interpolated_values() {
        let templates = Templates::new()
            .add("x.html", "{{ value }}")
            .expect("parses");
        let env = Arc::new(templates.env);
        let view = Renderer(env);
        let res = view
            .render("x.html", context! { value => "<script>alert(1)</script>" })
            .expect("renders");
        let body = String::from_utf8(res.body.as_slice().unwrap().to_vec()).unwrap();
        assert!(
            !body.contains("<script>"),
            "an .html template must escape markup: {body}"
        );
        assert!(body.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_render_failure_does_not_leak_the_template_detail() {
        let templates = Templates::new().add("x.html", "{{ ok }}").expect("parses");
        let view = Renderer(Arc::new(templates.env));
        let err = view.render("missing.html", context! {}).unwrap_err();
        assert_eq!(err.message(), "template rendering failed");
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
