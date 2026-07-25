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
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED};
use http::{HeaderValue, StatusCode};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

/// Serves files from a directory. Build with [`StaticFiles::dir`], then mount
/// its [`handler`](StaticFiles::handler) on a `{path...}` wildcard route.
#[derive(Debug, Clone)]
pub struct StaticFiles {
    root: PathBuf,
    index: Option<String>,
    list_directories: bool,
}

impl StaticFiles {
    /// Serve files rooted at `root`.
    pub fn dir(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            index: None,
            list_directories: false,
        }
    }

    /// List a directory's contents when it has no index file.
    ///
    /// Off by default, and deliberately so: a listing discloses every filename
    /// under the served root, which is how backup files, editor swap files and
    /// forgotten exports get found. Turn it on for a file mirror, not for an
    /// application's assets.
    pub fn list_directories(mut self, yes: bool) -> Self {
        self.list_directories = yes;
        self
    }

    /// Serve `<dir>/<index>` when a request resolves to a directory.
    pub fn index(mut self, file: impl Into<String>) -> Self {
        self.index = Some(file.into());
        self
    }

    /// Turn this into a [`Handler`] for a wildcard route. The handler reads the
    /// route's single captured path parameter as the relative path.
    ///
    /// # Panics
    ///
    /// Panics if the root does not exist or is not a directory. This is a
    /// configuration mistake, and the alternative is worse: the previous
    /// behaviour returned `500` on every request, forever, for a typo that
    /// could have been caught at startup.
    ///
    /// Use [`try_handler`](StaticFiles::try_handler) to handle it yourself.
    pub fn handler(self) -> impl Handler {
        let root = self.root.clone();
        self.try_handler().unwrap_or_else(|| {
            panic!(
                "static root {} does not exist or is not a directory",
                root.display()
            )
        })
    }

    /// Like [`handler`](StaticFiles::handler) but returns `None` instead of
    /// panicking when the root is unusable.
    pub fn try_handler(self) -> Option<impl Handler> {
        if !self.root.is_dir() {
            return None;
        }
        Some(self.into_handler_unchecked())
    }

    fn into_handler_unchecked(self) -> impl Handler {
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

        let canonical_root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|_| Error::internal("static root does not exist"))?;

        // Symlink-escape guard, applied *before* anything decides what to do
        // with the target. It used to sit after the directory dispatch, so a
        // symlink to a directory outside the root reached the listing renderer
        // unchecked and disclosed every filename under whatever it pointed at.
        // `metadata` follows symlinks, so `is_dir()` was true and the listing
        // arm returned before the guard ever ran.
        let resolved = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        if !resolved.starts_with(&canonical_root) {
            return Err(Error::not_found("not found"));
        }
        path = resolved;

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        if meta.is_dir() {
            match &self.index {
                Some(index) if path.join(index).is_file() => path = path.join(index),
                _ if self.list_directories => {
                    // A directory has one URL, and it ends in `/`. The listing's
                    // links are relative and bare (`href="a.txt"`), so a browser
                    // resolves them against that slash; served at `/files` they
                    // would resolve one level too high. Redirect rather than
                    // render, so there is a single canonical URL per directory.
                    if !call.path().ends_with('/') {
                        let target = match call.uri().query() {
                            Some(q) if !q.is_empty() => format!("{}/?{}", call.path(), q),
                            _ => format!("{}/", call.path()),
                        };
                        return Ok(Response::new(http::StatusCode::PERMANENT_REDIRECT)
                            .with_header(
                                http::header::LOCATION,
                                http::HeaderValue::from_str(&target)
                                    .map_err(|_| Error::not_found("not found"))?,
                            ));
                    }
                    return self.render_listing(&path).await;
                }
                _ => return Err(Error::not_found("not found")),
            }
        }

        // Re-check after the index join: the index file may itself be a symlink
        // pointing out of the root, and the guard above resolved the directory.
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(Error::not_found("not found"));
        }

        // Stat the resolved file for the validators. The earlier `metadata`
        // call answered "is this a directory"; this one is about the entity
        // actually being served, which may be the index file instead.
        let file_meta = tokio::fs::metadata(&canonical)
            .await
            .map_err(|_| Error::not_found("not found"))?;
        let len = file_meta.len();
        let modified = file_meta.modified().ok();

        let etag = entity_tag(&file_meta);
        let last_modified = modified.map(httpdate::fmt_http_date);
        let content_type = content_type_for(&canonical);

        // Precondition evaluation, RFC 9110 §13.2.2 order. If-Match and
        // If-Unmodified-Since reject; If-None-Match and If-Modified-Since
        // short-circuit to 304.
        if let Some(if_match) = call.header("if-match") {
            if !etag_matches(if_match, etag.as_deref()) {
                return Ok(Response::new(StatusCode::PRECONDITION_FAILED));
            }
        } else if let Some(raw) = call.header("if-unmodified-since") {
            if let (Some(since), Some(m)) = (parse_http_date(raw), modified) {
                if is_modified_after(m, since) {
                    return Ok(Response::new(StatusCode::PRECONDITION_FAILED));
                }
            }
        }

        // §13.1.3: when If-None-Match is present, If-Modified-Since is ignored
        // entirely — not consulted as a tie-breaker.
        let not_modified = if let Some(if_none_match) = call.header("if-none-match") {
            etag_matches(if_none_match, etag.as_deref())
        } else if let Some(raw) = call.header("if-modified-since") {
            match (parse_http_date(raw), modified) {
                (Some(since), Some(m)) => !is_modified_after(m, since),
                _ => false,
            }
        } else {
            false
        };

        if not_modified {
            let mut res = Response::new(StatusCode::NOT_MODIFIED);
            apply_validators(&mut res, etag.as_deref(), last_modified.as_deref());
            return Ok(res);
        }

        // A Range only applies if If-Range agrees the client's copy is current.
        let mut spec = match call.header("range") {
            Some(raw) => parse_range(raw, len),
            None => RangeSpec::Absent,
        };
        if matches!(spec, RangeSpec::Satisfiable(..)) {
            if let Some(if_range) = call.header("if-range") {
                if !if_range_matches(if_range, etag.as_deref(), last_modified.as_deref()) {
                    spec = RangeSpec::Absent;
                }
            }
        }

        if matches!(spec, RangeSpec::Unsatisfiable) {
            let mut res = Response::new(StatusCode::RANGE_NOT_SATISFIABLE)
                .with_header(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                res = res.with_header(CONTENT_RANGE, v);
            }
            return Ok(res);
        }

        let file = tokio::fs::File::open(&canonical)
            .await
            .map_err(|_| Error::not_found("not found"))?;

        let range = match spec {
            RangeSpec::Satisfiable(s, e) => Some((s, e)),
            _ => None,
        };
        let (status, start, count) = match range {
            Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
            None => (StatusCode::OK, 0, len),
        };

        let mut res = Response::stream(
            content_type,
            Body::from_stream(file_stream(file, start, count)),
        )
        .with_status(status)
        .with_header(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        apply_validators(&mut res, etag.as_deref(), last_modified.as_deref());

        // The length is known before a byte is read, so it is reported rather
        // than left to chunked framing. This is also what lets a synthesized
        // HEAD on a static file answer with the real size.
        if let Ok(v) = HeaderValue::from_str(&count.to_string()) {
            res.headers.insert(CONTENT_LENGTH, v);
        }
        if let Some((s, e)) = range {
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {s}-{e}/{len}")) {
                res = res.with_header(CONTENT_RANGE, v);
            }
        }

        Ok(res)
    }
}

impl StaticFiles {
    /// Render a directory listing as minimal HTML.
    ///
    /// Names are HTML-escaped: a file called `<script>.txt` is a stored-XSS
    /// vector otherwise, and an attacker who can write to the served directory
    /// is exactly who would try it.
    /// Render a directory listing with **bare** relative links.
    ///
    /// `href="a.txt"` rather than `href="sub/a.txt"`: the caller guarantees the
    /// request URL ends in `/`, so a browser resolves each link against the
    /// directory itself and the same markup is correct at any depth. The
    /// previous form prefixed the request-relative path, which was right at a
    /// subdirectory without a trailing slash and wrong everywhere else.
    async fn render_listing(&self, path: &Path) -> Result<Response> {
        let mut entries = tokio::fs::read_dir(path)
            .await
            .map_err(|_| Error::not_found("not found"))?;

        let mut names: Vec<(String, bool)> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            names.push((name, is_dir));
        }
        // Directories first, then alphabetically — stable output matters for
        // anything scraping this.
        names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut html =
            String::from("<!doctype html><meta charset=\"utf-8\"><title>Index</title><ul>");
        for (name, is_dir) in names {
            let slash = if is_dir { "/" } else { "" };
            html.push_str(&format!(
                "<li><a href=\"{}{}\">{}{}</a></li>",
                // The href is a URL, so it needs URL escaping — HTML escaping
                // alone left `:` intact, and a file named
                // `javascript:alert(document.cookie)` is then a syntactically
                // valid absolute URL that a browser will not resolve
                // relatively. It also fixes ordinary names: `a#b.txt` linked to
                // `a` with a fragment, `q?x=1.txt` to `q` with a query.
                escape_html(&percent_encode_segment(&name)),
                slash,
                // The label is text, so it stays HTML-escaped and undecorated.
                escape_html(&name),
                slash
            ));
        }
        html.push_str("</ul>");

        Ok(Response::bytes(
            "text/html; charset=utf-8",
            html.into_bytes(),
        ))
    }
}

/// Escape the five characters that can break out of HTML text or an attribute.
/// Percent-encode everything outside the unreserved set, so a filename cannot
/// be read as anything but one path segment.
///
/// Deliberately aggressive: the threat model here is a filename chosen by
/// whoever can write to the served directory, and the cost of over-encoding is
/// a longer URL that resolves identically.
fn percent_encode_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// A weak entity tag built from `(mtime, len)`.
///
/// Weak (`W/`) on purpose: the bytes are never hashed, so this compares
/// metadata rather than content. Labelling it strong would claim a guarantee
/// that is not being made — two different files sharing a size and timestamp
/// would collide.
fn entity_tag(meta: &std::fs::Metadata) -> Option<String> {
    let modified = meta.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("W/\"{:x}-{:x}\"", secs, meta.len()))
}

fn apply_validators(res: &mut Response, etag: Option<&str>, last_modified: Option<&str>) {
    if let Some(t) = etag {
        if let Ok(v) = HeaderValue::from_str(t) {
            res.headers.insert(ETAG, v);
        }
    }
    if let Some(lm) = last_modified {
        if let Ok(v) = HeaderValue::from_str(lm) {
            res.headers.insert(LAST_MODIFIED, v);
        }
    }
}

/// Does a comma-separated `If-Match` / `If-None-Match` list match our tag?
///
/// `*` matches any existing entity. Comparison is weak, per RFC 9110 §13.1.2:
/// the `W/` prefix is stripped from both sides before comparing, which is what
/// If-None-Match requires.
fn etag_matches(header: &str, etag: Option<&str>) -> bool {
    let Some(ours) = etag else { return false };
    let ours = ours.trim_start_matches("W/");
    header.split(',').any(|candidate| {
        let c = candidate.trim();
        c == "*" || c.trim_start_matches("W/") == ours
    })
}

/// `If-Range` accepts either an entity tag or a date, and unlike `If-None-Match`
/// its tag comparison is strong. Ours are weak, so a tag only matches when it is
/// byte-identical to what we minted.
fn if_range_matches(header: &str, etag: Option<&str>, last_modified: Option<&str>) -> bool {
    let h = header.trim();
    if h.starts_with('"') || h.starts_with("W/") {
        return etag.is_some_and(|t| t == h);
    }
    last_modified.is_some_and(|lm| lm == h)
}

fn parse_http_date(raw: &str) -> Option<std::time::SystemTime> {
    httpdate::parse_http_date(raw.trim()).ok()
}

/// Compare with one-second granularity: HTTP dates carry no sub-second part, so
/// a file written moments after the date it reports would otherwise look
/// modified on every request.
fn is_modified_after(modified: std::time::SystemTime, since: std::time::SystemTime) -> bool {
    let secs = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };
    secs(modified) > secs(since)
}

/// What a `Range` header amounts to for this entity.
///
/// Three outcomes need distinguishing and a bare `Option` collapses two of
/// them: a multi-range request and an out-of-bounds one both fail to produce a
/// span, but the first is answered with the whole entity and the second with
/// `416`.
enum RangeSpec {
    /// No usable range: header absent, malformed, or a multi-range request.
    /// All three are answered with the full representation.
    Absent,
    /// Syntactically valid but outside the entity — `416`.
    Unsatisfiable,
    /// An inclusive byte span.
    Satisfiable(u64, u64),
}

/// Parse a single byte range into an inclusive span.
fn parse_range(raw: &str, len: u64) -> RangeSpec {
    let Some(spec) = raw.trim().strip_prefix("bytes=") else {
        return RangeSpec::Absent; // not a byte-range unit: ignore
    };
    if spec.contains(',') {
        return RangeSpec::Absent; // multi-range: fall back to the whole entity
    }
    let Some((from, to)) = spec.split_once('-') else {
        return RangeSpec::Absent;
    };
    let (from, to) = (from.trim(), to.trim());

    let (start, end) = if from.is_empty() {
        // Suffix range: the last N bytes.
        let Ok(n) = to.trim().parse::<u64>() else {
            return RangeSpec::Absent; // malformed: ignore rather than reject
        };
        if len == 0 || n == 0 {
            return RangeSpec::Unsatisfiable;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let Ok(s) = from.trim().parse::<u64>() else {
            return RangeSpec::Absent;
        };
        let e = if to.trim().is_empty() {
            match len.checked_sub(1) {
                Some(e) => e,
                None => return RangeSpec::Unsatisfiable, // empty file
            }
        } else {
            match to.trim().parse::<u64>() {
                // A too-large end is clamped, not rejected — RFC 9110 §14.1.1.
                Ok(e) => e.min(len.saturating_sub(1)),
                Err(_) => return RangeSpec::Absent,
            }
        };
        (s, e)
    };

    if len == 0 || start > end || start >= len {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Satisfiable(start, end)
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

/// Stream `count` bytes starting at `start`, in 64 KiB chunks (no `tokio-util`
/// dependency).
///
/// The seek happens lazily on first poll so this stays a plain synchronous
/// constructor. `remaining` bounds the read, which is what keeps a range
/// response from over-reading past its end.
fn file_stream(
    file: tokio::fs::File,
    start: u64,
    count: u64,
) -> impl futures_util::stream::Stream<Item = std::result::Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold(
        (file, count, false, start > 0),
        move |(mut file, remaining, done, needs_seek)| async move {
            if done || remaining == 0 {
                return None;
            }
            if needs_seek {
                use tokio::io::AsyncSeekExt;
                if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
                    return Some((Err(e), (file, 0, true, false)));
                }
            }

            let want = remaining.min(64 * 1024) as usize;
            let mut buf = vec![0u8; want];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    let left = remaining - n as u64;
                    Some((Ok(Bytes::from(buf)), (file, left, false, false)))
                }
                Err(e) => Some((Err(e), (file, 0, true, false))),
            }
        },
    )
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
