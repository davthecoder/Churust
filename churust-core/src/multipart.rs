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
//! # Two parsers, and which one to reach for
//!
//! [`Multipart`] parses the already-buffered request body, so an upload through
//! it is capped by the server-wide
//! [`max_body_bytes`](crate::AppBuilder::max_body_bytes) and by any per-route
//! [`max_body_bytes`](crate::RouteBuilder::max_body_bytes). Everything lands in
//! memory at once. That is the right answer for form fields and small
//! attachments, and it stays the default because it cannot surprise anyone.
//!
//! [`MultipartStream`] parses incrementally: fields arrive one at a time and
//! each field's content is read in chunks, so an upload can be written to disk
//! or forwarded onward while it is still arriving. Only one chunk plus the
//! boundary is held at any moment.
//!
//! What that changes, precisely: **memory stops scaling with upload size.** The
//! buffered parser holds the whole body, so an upload route sized for 2 GiB
//! costs 2 GiB per concurrent upload; the streaming one costs a chunk. What it
//! does **not** change is the ceiling itself. The engine wraps every request
//! body in a limit of
//! [`max_body_bytes`](crate::AppBuilder::max_body_bytes), so an upload larger
//! than that is still refused with `413` here exactly as it is through
//! [`Payload`](crate::Payload). Raise it deliberately for the upload route; the
//! difference is that raising it is now affordable.
//!
//! Everything else stays bounded: the per-route body cap applies to the stream,
//! part headers are capped, the part count is capped, and [`Field::bytes`]
//! refuses to collect more than
//! [`max_field_bytes`](MultipartStream::max_field_bytes). What the parser will
//! not do is pick a total for you, because a handler that streams to disk has a
//! different ceiling from one that does not.

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
            let value = v.trim().trim_matches('"');
            // An empty boundary would make the delimiter a bare `--`, splitting
            // the body on every occurrence of two hyphens.
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

fn parse(body: &[u8], boundary: &str) -> Result<Vec<Part>> {
    // RFC 2046 §5.1.1: a delimiter is CRLF followed by `--boundary`. Splitting
    // on the bare `--boundary` let a part whose *content* embedded the boundary
    // forge additional parts — so a single uploaded file could inject a second
    // field the client never sent. It is also a parser differential against any
    // proxy or WAF in front, which is the more dangerous half.
    //
    // The opening delimiter has no preceding CRLF, so a synthetic one is
    // prepended rather than special-casing offset 0.
    let delim = format!("\r\n--{boundary}");
    let mut framed = Vec::with_capacity(body.len() + 2);
    framed.extend_from_slice(b"\r\n");
    framed.extend_from_slice(body);
    let body: &[u8] = &framed;
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

/// The most bytes of part headers accepted before a part is refused.
///
/// A part may declare any number of headers, and without a cap a body that is
/// nothing but header lines grows the buffer without ever producing a field.
const MAX_PART_HEADER_BYTES: usize = 8 * 1024;

/// Default ceiling for [`Field::bytes`].
const DEFAULT_MAX_FIELD_BYTES: usize = 8 * 1024 * 1024;

/// Where the incremental parser is in the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Before the first delimiter. Anything here is preamble and is discarded.
    Preamble,
    /// A delimiter was just consumed; the next two bytes say whether another
    /// part follows or the body is over.
    AfterDelimiter,
    /// Inside a part's content.
    InPart,
    /// The closing delimiter was seen.
    Done,
}

/// An incremental `multipart/form-data` parser.
///
/// Consumes the request body, so it must be the last handler argument. Fields
/// arrive one at a time from [`next_field`](Self::next_field), and each field's
/// content is read with [`Field::chunk`] or collected with [`Field::bytes`].
///
/// ```
/// use churust_core::{Churust, MultipartStream, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/upload", |mut form: MultipartStream| async move {
///             let mut total = 0usize;
///             while let Some(mut field) = form.next_field().await? {
///                 // A real handler would write each chunk to disk here.
///                 while let Some(chunk) = field.chunk().await? {
///                     total += chunk.len();
///                 }
///             }
///             Ok::<_, churust_core::Error>(format!("{total} bytes"))
///         });
///     })
///     .build();
///
/// let body = "--X\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"a.txt\"\r\n\r\nhello\r\n--X--\r\n";
/// let res = TestClient::new(app)
///     .post("/upload")
///     .header("content-type", "multipart/form-data; boundary=X")
///     .body(body)
///     .send()
///     .await;
/// assert_eq!(res.text(), "5 bytes");
/// # });
/// ```
pub struct MultipartStream {
    stream: crate::call::BodyStream,
    /// Bytes read from the body but not yet handed out.
    buffer: bytes::BytesMut,
    /// `\r\n--boundary`, the sequence that ends a part's content.
    delimiter: Vec<u8>,
    phase: Phase,
    /// True once the body stream has ended.
    drained: bool,
    parts_seen: usize,
    max_field_bytes: usize,
}

impl std::fmt::Debug for MultipartStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultipartStream")
            .field("phase", &self.phase)
            .field("buffered", &self.buffer.len())
            .field("parts_seen", &self.parts_seen)
            .finish_non_exhaustive()
    }
}

impl MultipartStream {
    /// Build a parser over `stream` for `boundary`.
    fn new(stream: crate::call::BodyStream, boundary: &str) -> Self {
        let mut buffer = bytes::BytesMut::new();
        // The first delimiter in a body has no CRLF before it, every later one
        // does. Seeding the buffer with a CRLF lets one search handle both
        // instead of special-casing the opening delimiter everywhere.
        buffer.extend_from_slice(b"\r\n");
        Self {
            stream,
            buffer,
            delimiter: format!("\r\n--{boundary}").into_bytes(),
            phase: Phase::Preamble,
            drained: false,
            parts_seen: 0,
            max_field_bytes: DEFAULT_MAX_FIELD_BYTES,
        }
    }

    /// Cap what [`Field::bytes`] will collect (default 8 MiB).
    ///
    /// Only affects collecting. [`Field::chunk`] is unbounded on purpose: a
    /// handler streaming to disk is choosing its own ceiling, and the framework
    /// has no way to know what it is.
    pub fn max_field_bytes(mut self, bytes: usize) -> Self {
        self.max_field_bytes = bytes;
        self
    }

    /// Pull one more chunk from the body.
    ///
    /// Returns `false` once the body has ended.
    async fn fill(&mut self) -> Result<bool> {
        if self.drained {
            return Ok(false);
        }
        use futures_util::StreamExt;
        match self.stream.next().await {
            Some(Ok(chunk)) => {
                self.buffer.extend_from_slice(&chunk);
                Ok(true)
            }
            // The body's own error, which for an over-limit body is the 413
            // raised by the per-route cap. Propagating it keeps that status.
            Some(Err(e)) => Err(e),
            None => {
                self.drained = true;
                Ok(false)
            }
        }
    }

    /// The next field, or `None` at the end of the body.
    ///
    /// A field that was not read to the end is discarded first, so a handler
    /// may skip a part it does not want without leaving the parser mid-stream.
    ///
    /// # Errors
    ///
    /// If the body is malformed, ends early, exceeds the part count, or the
    /// underlying body stream fails, which includes the per-route body cap
    /// being crossed.
    pub async fn next_field(&mut self) -> Result<Option<Field<'_>>> {
        if self.phase == Phase::InPart {
            self.skip_rest_of_part().await?;
        }
        if self.phase == Phase::Preamble {
            self.consume_through_delimiter().await?;
        }
        if self.phase == Phase::Done {
            return Ok(None);
        }

        // AfterDelimiter: two bytes decide whether another part follows.
        while self.buffer.len() < 2 {
            if !self.fill().await? {
                return Err(Error::bad_request("multipart body ended after a delimiter"));
            }
        }
        if &self.buffer[..2] == b"--" {
            self.phase = Phase::Done;
            return Ok(None);
        }
        // Any transport padding between the delimiter and the CRLF is legal and
        // ignorable, but the CRLF itself is required.
        let line_end = loop {
            match find(&self.buffer, b"\r\n") {
                Some(at) => break at,
                None => {
                    if self.buffer.len() > MAX_PART_HEADER_BYTES || !self.fill().await? {
                        return Err(Error::bad_request("malformed multipart delimiter"));
                    }
                }
            }
        };
        let _ = self.buffer.split_to(line_end + 2);

        let (name, filename, content_type) = self.read_part_headers().await?;

        self.parts_seen += 1;
        if self.parts_seen > MAX_PARTS {
            return Err(Error::new(
                http::StatusCode::PAYLOAD_TOO_LARGE,
                "too many multipart parts",
            ));
        }

        self.phase = Phase::InPart;
        let max_field_bytes = self.max_field_bytes;
        Ok(Some(Field {
            name,
            filename,
            content_type,
            max_field_bytes,
            parser: self,
        }))
    }

    /// Discard everything up to and including the next delimiter.
    async fn consume_through_delimiter(&mut self) -> Result<()> {
        loop {
            if let Some(at) = find(&self.buffer, &self.delimiter) {
                let _ = self.buffer.split_to(at + self.delimiter.len());
                self.phase = Phase::AfterDelimiter;
                return Ok(());
            }
            // Keep only what could still be the start of a delimiter, so a long
            // preamble cannot grow the buffer without bound.
            let keep = self.delimiter.len().saturating_sub(1);
            if self.buffer.len() > keep {
                let drop_to = self.buffer.len() - keep;
                let _ = self.buffer.split_to(drop_to);
            }
            if !self.fill().await? {
                return Err(Error::bad_request(
                    "multipart body has no boundary delimiter",
                ));
            }
        }
    }

    /// Read one part's header block.
    async fn read_part_headers(&mut self) -> Result<(String, Option<String>, Option<String>)> {
        let split = loop {
            if let Some(at) = find(&self.buffer, b"\r\n\r\n") {
                break at;
            }
            if self.buffer.len() > MAX_PART_HEADER_BYTES {
                return Err(Error::new(
                    http::StatusCode::PAYLOAD_TOO_LARGE,
                    "multipart part headers are too large",
                ));
            }
            if !self.fill().await? {
                return Err(Error::bad_request(
                    "malformed multipart part: no header block",
                ));
            }
        };

        let head = self.buffer.split_to(split);
        let _ = self.buffer.split_to(4);

        let headers = std::str::from_utf8(&head)
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

        let name = name.ok_or_else(|| {
            Error::bad_request("multipart part is missing a Content-Disposition name")
        })?;
        Ok((name, filename, content_type))
    }

    /// Read and discard the rest of the current part.
    async fn skip_rest_of_part(&mut self) -> Result<()> {
        while self.next_content_chunk().await?.is_some() {}
        Ok(())
    }

    /// The next slice of the current part's content, or `None` at its end.
    ///
    /// Consuming the trailing delimiter is what moves the parser on, so this is
    /// the single place that advances out of [`Phase::InPart`].
    async fn next_content_chunk(&mut self) -> Result<Option<Bytes>> {
        if self.phase != Phase::InPart {
            return Ok(None);
        }
        loop {
            if let Some(at) = find(&self.buffer, &self.delimiter) {
                let data = self.buffer.split_to(at).freeze();
                let _ = self.buffer.split_to(self.delimiter.len());
                self.phase = Phase::AfterDelimiter;
                return Ok((!data.is_empty()).then_some(data));
            }

            // Nothing but a possible partial delimiter is safe to emit, so hold
            // back that many bytes and hand out the rest.
            let hold = self.delimiter.len().saturating_sub(1);
            if self.buffer.len() > hold {
                let take = self.buffer.len() - hold;
                let data = self.buffer.split_to(take).freeze();
                if !data.is_empty() {
                    return Ok(Some(data));
                }
            }

            if !self.fill().await? {
                return Err(Error::bad_request("multipart body ended inside a part"));
            }
        }
    }
}

#[async_trait]
impl FromCall for MultipartStream {
    async fn from_call(call: Call) -> Result<Self> {
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

        // Going through `Payload` rather than taking the raw stream is what
        // makes the per-route body cap apply here exactly as it does there.
        let crate::extract::Payload(stream) = crate::extract::Payload::from_call(call).await?;
        Ok(MultipartStream::new(stream, &boundary))
    }
}

/// One part of a streamed `multipart/form-data` body.
///
/// Borrowed from the parser, so only one field exists at a time: the body is a
/// single stream and the parts are in it in order.
pub struct Field<'a> {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    max_field_bytes: usize,
    parser: &'a mut MultipartStream,
}

impl std::fmt::Debug for Field<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("name", &self.name)
            .field("filename", &self.filename)
            .field("content_type", &self.content_type)
            .finish_non_exhaustive()
    }
}

impl Field<'_> {
    /// The `name` from `Content-Disposition`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `filename`, when the part is a file upload.
    pub fn filename(&self) -> Option<&str> {
        self.filename.as_deref()
    }

    /// The part's own `Content-Type`, if it declared one.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The next slice of this field's content, or `None` at its end.
    ///
    /// Chunk boundaries follow whatever arrived from the network and carry no
    /// meaning: a field's content is the concatenation of its chunks.
    ///
    /// # Errors
    ///
    /// If the body is malformed or ends inside the part.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        self.parser.next_content_chunk().await
    }

    /// Collect the whole field into memory.
    ///
    /// # Errors
    ///
    /// `413` past [`MultipartStream::max_field_bytes`], or whatever
    /// [`chunk`](Self::chunk) would have returned. The limit is checked as the
    /// field is read, so an oversized field is refused partway rather than
    /// after it has all been buffered.
    pub async fn bytes(&mut self) -> Result<Bytes> {
        let mut out = bytes::BytesMut::new();
        while let Some(chunk) = self.chunk().await? {
            if out.len() + chunk.len() > self.max_field_bytes {
                return Err(Error::new(
                    http::StatusCode::PAYLOAD_TOO_LARGE,
                    "multipart field too large",
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out.freeze())
    }

    /// Collect the whole field as UTF-8 text.
    ///
    /// # Errors
    ///
    /// As [`bytes`](Self::bytes), plus `400` if the content is not UTF-8.
    pub async fn text(&mut self) -> Result<String> {
        let raw = self.bytes().await?;
        String::from_utf8(raw.to_vec())
            .map_err(|_| Error::bad_request("multipart field is not valid UTF-8"))
    }
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
