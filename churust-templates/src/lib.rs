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
//! Every template is HTML-escaped, whatever it is called. Interpolated values
//! cannot inject markup, so a `{{ bio }}` holding `<script>` reaches the
//! browser as text.
//!
//! minijinja's own rule is to pick the escaping from the file extension and
//! escape only `.html`, `.htm` and `.xml`. That is the right default for a
//! library that can render into any format, and the wrong one here, because
//! [`Renderer`] has a single sink: every method it offers labels its reply
//! `text/html; charset=utf-8`. Leaving the filename in charge meant a template
//! named `page.txt`, or `partials/nav` with no extension at all, was
//! interpolated raw and then served as HTML anyway — a stored XSS waiting on
//! whoever named the file. Naming a template `.html` is still the clearer
//! thing to do, but it is no longer what keeps you safe.
//!
//! If you need a template that is genuinely not HTML, install your own policy
//! with [`Templates::configure`] **before** adding it: minijinja resolves the
//! escaping once, when a template is parsed, so a callback set afterwards does
//! not reach templates that are already loaded.
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
use minijinja::{AutoEscape, Environment};
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
        let mut env = Environment::new();
        // minijinja decides auto-escaping from the template's file extension,
        // escaping only `.html`, `.htm` and `.xml` and leaving everything else
        // raw. That rule is right for a library that can render into any
        // format, and wrong here: [`Renderer::render`] has exactly one sink and
        // stamps `text/html; charset=utf-8` on every reply it makes. Under the
        // default callback a template the author called `page.txt`, or
        // `partials/nav` with no extension at all, interpolated its values
        // unescaped and was then shipped to a browser as HTML — a mislabelled
        // response by construction, and a stored-XSS sink the moment one of
        // those values came from a user. Pinning the policy to `Html` makes the
        // escaping agree with the Content-Type instead of with the filename.
        // `.html`/`.htm`/`.xml` are unaffected: the default already chose
        // `Html` for all three.
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        Self { env }
    }

    /// Add one template from a string, parsing it now.
    ///
    /// The name is what handlers and `{% extends %}` refer to. It no longer
    /// decides escaping — see [Escaping](crate#escaping) — but `something.html`
    /// still reads better than `something`.
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
    /// Every file under `dir` is a template. There is no extension filter and
    /// no skip list, so a `logo.png` or an editor artefact left beside the
    /// templates is read as one too, and fails the boot with an [`Io`] error
    /// naming the path if it is not valid UTF-8. That is the intended
    /// behaviour rather than an oversight: a rule that quietly passed over
    /// whatever did not look like a template would also pass over a real
    /// template saved in the wrong encoding, and that omission would come back
    /// as a `500` on the one route nobody exercises until Friday. Loud at boot
    /// with the offending path in the message is the cheaper failure. Keep the
    /// directory for templates and serve assets with `StaticFiles` from
    /// `churust-core`'s `fs` feature.
    ///
    /// # Errors
    ///
    /// If `dir` is not a readable directory, if any file under it cannot be
    /// read as UTF-8, or if any template fails to parse. All three are startup
    /// failures on purpose: a template that cannot be parsed is a route that
    /// cannot answer, and finding that out at boot is cheaper than finding out
    /// in production.
    ///
    /// [`Io`]: TemplateSetupError::Io
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
    /// Order matters for escaping and only for escaping: minijinja resolves a
    /// template's auto-escape mode while it parses it, so a
    /// `set_auto_escape_callback` here reaches only the templates added after
    /// it. Filters, tests and globals are looked up at render time and can be
    /// registered whenever.
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
    fn a_template_without_an_html_extension_escapes_all_the_same() {
        // The extension used to decide this and the Content-Type did not, so
        // the two could disagree. Now only the Content-Type has a say.
        let templates = Templates::new()
            .add("x.txt", "{{ value }}")
            .expect("parses");
        let view = Renderer(Arc::new(templates.env));
        let res = view
            .render("x.txt", context! { value => "<script>alert(1)</script>" })
            .expect("renders");
        let body = String::from_utf8(res.body.as_slice().unwrap().to_vec()).unwrap();
        // The closing slash comes back as `&#x2f;` for the same reason it does
        // in an `.html` template: the escaper will not leave a value anything
        // it could use to close a tag with.
        assert_eq!(body, "&lt;script&gt;alert(1)&lt;&#x2f;script&gt;");
    }

    #[test]
    fn configure_can_hand_escaping_back_to_the_extension() {
        // The escape hatch for an author who really is rendering something
        // that is not HTML. It has to run before `add`, because minijinja
        // resolves the mode as it parses.
        let templates = Templates::new()
            .configure(|env| env.set_auto_escape_callback(|_| AutoEscape::None))
            .add("x.txt", "{{ value }}")
            .expect("parses");
        let view = Renderer(Arc::new(templates.env));
        let res = view
            .render("x.txt", context! { value => "a & b" })
            .expect("renders");
        let body = String::from_utf8(res.body.as_slice().unwrap().to_vec()).unwrap();
        assert_eq!(body, "a & b");
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
