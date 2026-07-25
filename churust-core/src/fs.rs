//! Static file serving (feature `fs`).
//!
//! Register [`StaticFiles`] on a wildcard route to stream files from a
//! directory:
//!
//! ```no_run
//! use churust_core::{Churust, fs::StaticFiles};
//!
//! # fn build() {
//! Churust::server().routing(|r| {
//!     r.get("/assets/{path...}", StaticFiles::dir("./public").index("index.html").handler());
//! });
//! # }
//! ```

use crate::body::Body;
use crate::call::Call;
use crate::error::{Error, Result};
use crate::handler::{Handler, IntoHandler};
use crate::response::Response;
use bytes::Bytes;
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

/// Serves files from a directory. Build with [`StaticFiles::dir`], then mount
/// its [`handler`](StaticFiles::handler) on a `{path...}` wildcard route.
#[derive(Debug, Clone)]
pub struct StaticFiles {
    root: PathBuf,
    index: Option<String>,
}

impl StaticFiles {
    /// Serve files rooted at `root`.
    pub fn dir(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            index: None,
        }
    }

    /// Serve `<dir>/<index>` when a request resolves to a directory.
    pub fn index(mut self, file: impl Into<String>) -> Self {
        self.index = Some(file.into());
        self
    }

    /// Turn this into a [`Handler`] for a wildcard route. The handler reads the
    /// route's single captured path parameter as the relative path.
    pub fn handler(self) -> impl Handler {
        let cfg = self;
        // Closures implement `Handler` indirectly through `HandlerFn` +
        // `IntoHandler` (see the implementation note in `handler.rs`); adapt the
        // closure into a concrete `Handler` here so the returned `impl Handler`
        // is satisfied.
        let closure = move |call: Call| {
            let cfg = cfg.clone();
            async move { cfg.serve(&call).await }
        };
        closure.into_handler()
    }

    async fn serve(&self, call: &Call) -> Result<Response> {
        // The relative path is the route's (single) captured param value.
        let rel = call
            .params_iter()
            .map(|(_, v)| v.to_string())
            .next()
            .unwrap_or_default();

        // Refuse an encoded separator before anything interprets the path.
        //
        // Path parameters arrive percent-decoded, so `%2F` would otherwise
        // reappear as a real separator once the wildcard's segments are
        // rejoined, and `%5C` is a separator on Windows. `sanitize` still
        // rejects the `..` that makes traversal work, so this is not the thing
        // standing between a request and the filesystem — it is what makes the
        // rejoined value unambiguous by construction, so the safety argument
        // does not depend on reasoning about how segments were rejoined.
        //
        // Deliberately stricter than necessary: a file whose name contains a
        // literal encoded slash is not servable. That is an accepted
        // limitation, and 404 rather than 400 so the response does not reveal
        // whether the path would have resolved.
        let raw = call.path().to_ascii_lowercase();
        if raw.contains("%2f") || raw.contains("%5c") {
            return Err(Error::not_found("not found"));
        }

        let safe = sanitize(&rel).ok_or_else(|| Error::not_found("not found"))?;
        let mut path = self.root.join(safe);

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        if meta.is_dir() {
            match &self.index {
                Some(index) => path = path.join(index),
                None => return Err(Error::not_found("not found")),
            }
        }

        // Symlink-escape guard: the canonical path must stay within root.
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        let canonical_root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|_| Error::internal("static root does not exist"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(Error::not_found("not found"));
        }

        let file = tokio::fs::File::open(&canonical)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        let content_type = content_type_for(&canonical);
        Ok(Response::stream(
            content_type,
            Body::from_stream(file_stream(file)),
        ))
    }
}

/// Reject path traversal: no `..`, no root/prefix components. Returns a relative
/// `PathBuf` of plain normal components, or `None` if unsafe.
fn sanitize(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            // `..`, `/`, `C:\`, etc. are all rejected.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Stream a file in 64 KiB chunks (no `tokio-util` dependency).
fn file_stream(
    file: tokio::fs::File,
) -> impl futures_util::stream::Stream<Item = std::result::Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold((file, false), |(mut file, done)| async move {
        if done {
            return None;
        }
        let mut buf = vec![0u8; 64 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), (file, false)))
            }
            Err(e) => Some((Err(e), (file, true))),
        }
    })
}

/// Built-in extension → `Content-Type` map (not exhaustive). Falls back to
/// `application/octet-stream`.
fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Churust, TestClient};
    use http::StatusCode;

    #[test]
    fn sanitize_rejects_decoded_parent_segments() {
        // Path parameters now arrive decoded, so `..` reaches sanitize as a
        // literal parent component rather than as `%2e%2e`.
        assert!(sanitize("../secret").is_none());
        assert!(sanitize("a/../../secret").is_none());
        assert!(sanitize("/etc/passwd").is_none(), "absolute paths rejected");
        assert!(sanitize("ok/file.txt").is_some());
        assert!(sanitize("./ok.txt").is_some(), "a bare `.` is harmless");
    }

    fn temp_dir_with_files() -> PathBuf {
        // Unique-enough dir under the OS temp dir (no extra deps).
        let base = std::env::temp_dir().join(format!("churust-fs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(base.join("sub"));
        std::fs::write(base.join("hello.txt"), b"hello world").unwrap();
        std::fs::write(base.join("page.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(base.join("index.html"), b"INDEX").unwrap();
        base
    }

    fn app(root: PathBuf) -> crate::App {
        Churust::server()
            .routing(move |r| {
                r.get(
                    "/files/{path...}",
                    StaticFiles::dir(root.clone()).index("index.html").handler(),
                );
            })
            .build()
    }

    #[tokio::test]
    async fn serves_file_with_content_type() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root))
            .get("/files/hello.txt")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(res.text(), "hello world");
    }

    #[tokio::test]
    async fn html_content_type() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root))
            .get("/files/page.html")
            .send()
            .await;
        assert_eq!(res.header("content-type"), Some("text/html; charset=utf-8"));
    }

    #[tokio::test]
    async fn missing_file_is_404() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root))
            .get("/files/nope.txt")
            .send()
            .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn directory_serves_index() {
        let root = temp_dir_with_files();
        let res = TestClient::new(app(root)).get("/files/sub").send().await;
        // sub/ has no index.html, so 404; root resolves index on "" path:
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize("../etc/passwd").is_none());
        assert!(sanitize("/etc/passwd").is_none());
        assert!(sanitize("a/../../b").is_none());
        assert_eq!(sanitize("css/app.css"), Some(PathBuf::from("css/app.css")));
        assert_eq!(sanitize("./x.txt"), Some(PathBuf::from("x.txt")));
    }
}
