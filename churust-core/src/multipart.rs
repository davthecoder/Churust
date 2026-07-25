//! `multipart/form-data` bodies (feature `multipart`) — file uploads.
//!
//! ```
//! use churust_core::{Churust, Multipart, TestClient};
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .routing(|r| {
//!         r.post("/upload", |form: Multipart| async move {
//!             let f = form.file("doc").expect("a file part named doc");
//!             format!("{} ({} bytes)", f.filename.clone().unwrap_or_default(), f.bytes.len())
//!         });
//!     })
//!     .build();
//!
//! let body = "--X\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"a.txt\"\r\n\r\nhello\r\n--X--\r\n";
//! let res = TestClient::new(app)
//!     .post("/upload")
//!     .header("content-type", "multipart/form-data; boundary=X")
//!     .body(body)
//!     .send()
//!     .await;
//! assert_eq!(res.text(), "a.txt (5 bytes)");
//! # });
//! ```
//!
//! # Bounded, not streamed
//!
//! Parsing runs over the already-buffered request body, so an upload is capped
//! by the server-wide [`max_body_bytes`](crate::AppBuilder::max_body_bytes) and
//! by any per-route
//! [`max_body_bytes`](crate::RouteBuilder::max_body_bytes) — raise them
//! deliberately for an upload route. Nothing is spooled to disk.
//!
//! That is a real limitation and it is the safe one: an unbounded streaming
//! parser is a memory-exhaustion surface, and bounding first is why this landed
//! after the limits work rather than before it. A part count cap guards against
//! a body that is small overall but made of a great many parts.

use crate::call::Call;
use crate::error::{Error, Result};
use crate::extract::FromCall;
use async_trait::async_trait;
use bytes::Bytes;

/// The most parts accepted in one body. A small body can still carry tens of
/// thousands of parts, each costing an allocation.
const MAX_PARTS: usize = 256;

/// One part of a `multipart/form-data` body.
#[derive(Debug, Clone)]
pub struct Part {
    /// The `name` from `Content-Disposition`.
    pub name: String,
    /// The `filename`, when the part is a file upload.
    pub filename: Option<String>,
    /// The part's `Content-Type`, if it declared one.
    pub content_type: Option<String>,
    /// The part's raw content.
    pub bytes: Bytes,
}

impl Part {
    /// The content as UTF-8, or `None` if it is not valid UTF-8.
    pub fn text(&self) -> Option<String> {
        String::from_utf8(self.bytes.to_vec()).ok()
    }
}

/// A parsed `multipart/form-data` body.
///
/// Consumes the request body, so it must be the last handler argument.
#[derive(Debug, Clone, Default)]
pub struct Multipart {
    parts: Vec<Part>,
}

impl Multipart {
    /// Every part, in body order.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The first part with this name.
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    /// The first part with this name that carries a filename.
    pub fn file(&self, name: &str) -> Option<&Part> {
        self.parts
            .iter()
            .find(|p| p.name == name && p.filename.is_some())
    }

    /// The text of the first part with this name — the plain form field case.
    pub fn field(&self, name: &str) -> Option<String> {
        self.part(name).and_then(|p| p.text())
    }
}

#[async_trait]
impl FromCall for Multipart {
    async fn from_call(mut call: Call) -> Result<Self> {
        let ct = call
            .header(http::header::CONTENT_TYPE.as_str())
            .unwrap_or("")
            .to_string();
        let boundary = boundary_of(&ct).ok_or_else(|| {
            Error::new(
                http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected multipart/form-data with a boundary",
            )
        })?;

        let body = call.try_receive_bytes().await?;
        crate::extract::check_body_limit(&call, body.len())?;
        parse(&body, &boundary).map(|parts| Multipart { parts })
    }
}

/// Pull `boundary=` out of a `multipart/form-data` content type.
fn boundary_of(content_type: &str) -> Option<String> {
    let mut it = content_type.split(';');
    let media = it.next()?.trim();
    if !media.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in it {
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("boundary") {
            // The value may be quoted.
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn parse(body: &[u8], boundary: &str) -> Result<Vec<Part>> {
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();

    for chunk in split_on(body, delim.as_bytes()).into_iter().skip(1) {
        // The terminating delimiter is followed by "--".
        if chunk.starts_with(b"--") {
            break;
        }
        // Trim the CRLF that follows the delimiter and precedes the next one.
        let chunk = strip_prefix(chunk, b"\r\n").unwrap_or(chunk);
        let chunk = strip_suffix(chunk, b"\r\n").unwrap_or(chunk);
        if chunk.is_empty() {
            continue;
        }

        let Some(split) = find(chunk, b"\r\n\r\n") else {
            return Err(Error::bad_request(
                "malformed multipart part: no header block",
            ));
        };
        let (head, content) = chunk.split_at(split);
        let content = &content[4..];

        let headers = std::str::from_utf8(head)
            .map_err(|_| Error::bad_request("multipart headers are not valid UTF-8"))?;

        let mut name = None;
        let mut filename = None;
        let mut content_type = None;
        for line in headers.split("\r\n") {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            match k.trim().to_ascii_lowercase().as_str() {
                "content-disposition" => {
                    name = param_of(v, "name");
                    filename = param_of(v, "filename");
                }
                "content-type" => content_type = Some(v.trim().to_string()),
                _ => {}
            }
        }

        let Some(name) = name else {
            return Err(Error::bad_request(
                "multipart part is missing a Content-Disposition name",
            ));
        };

        if parts.len() >= MAX_PARTS {
            return Err(Error::new(
                http::StatusCode::PAYLOAD_TOO_LARGE,
                "too many multipart parts",
            ));
        }

        parts.push(Part {
            name,
            filename,
            content_type,
            bytes: Bytes::copy_from_slice(content),
        });
    }

    Ok(parts)
}

/// Read a quoted parameter such as `name="x"` out of a header value.
fn param_of(value: &str, key: &str) -> Option<String> {
    for param in value.split(';') {
        // The first segment is the disposition type ("form-data") and carries
        // no '='. Skipping is required — bailing out here would mean no part
        // ever parsed a name.
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(key) {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn split_on<'a>(data: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while let Some(i) = find(rest, sep) {
        out.push(&rest[..i]);
        rest = &rest[i + sep.len()..];
    }
    out.push(rest);
    out
}

fn strip_prefix<'a>(data: &'a [u8], p: &[u8]) -> Option<&'a [u8]> {
    data.starts_with(p).then(|| &data[p.len()..])
}

fn strip_suffix<'a>(data: &'a [u8], p: &[u8]) -> Option<&'a [u8]> {
    data.ends_with(p).then(|| &data[..data.len() - p.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_boundary() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            boundary_of("multipart/form-data; boundary=\"a b\"").as_deref(),
            Some("a b")
        );
        assert_eq!(boundary_of("application/json"), None);
        assert_eq!(boundary_of("multipart/form-data"), None);
    }
}
