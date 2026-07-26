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

/// The most transport padding accepted on a delimiter line.
///
/// RFC 2046 writes `transport-padding := *LWSP-char`, so in principle it is
/// unbounded, but it exists for line-oriented transports that pad and no HTTP
/// client sends any. The streaming parser has to buffer the padding before it
/// can tell a delimiter from content, so an unbounded run would be an
/// unbounded buffer; both parsers use the same ceiling, because two parsers
/// that disagree about where a part ends are exactly the differential this
/// module is trying not to be.
const MAX_TRANSPORT_PADDING: usize = 64;

/// What follows a `\r\n--boundary` match in the body.
///
/// RFC 2046 §5.1.1 is precise about the shape of a delimiter *line*: the
/// delimiter, then `transport-padding` — SP and HTAB only — then CRLF; the
/// closing delimiter puts `--` immediately after the boundary instead. A run of
/// bytes that looks like a delimiter but is followed by anything else was never
/// a delimiter, and belongs to the enclosing part's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// Another part follows. The payload is how many bytes past the match its
    /// headers begin, which is the padding plus the CRLF.
    Part(usize),
    /// The closing delimiter: everything after it is epilogue.
    Close,
    /// Not a delimiter line, so the matched bytes are part content.
    Content,
    /// Undecidable until more of the body arrives. Only the streaming parser
    /// ever sees this; the buffered one has the whole body and says so.
    NeedMore,
}

/// Classify the bytes immediately after a `\r\n--boundary` match.
///
/// `at_end` says that no further bytes can arrive, which turns every otherwise
/// ambiguous truncation into a decision rather than a wait.
fn delimiter_tail(after: &[u8], at_end: bool) -> Tail {
    // The closing delimiter's `--` comes before its own padding, so the first
    // two bytes settle that case on their own.
    match (after.first(), after.get(1)) {
        (Some(b'-'), Some(b'-')) => return Tail::Close,
        (Some(b'-'), Some(_)) => return Tail::Content,
        (Some(b'-'), None) => {
            return if at_end {
                Tail::Content
            } else {
                Tail::NeedMore
            }
        }
        _ => {}
    }

    let mut i = 0;
    loop {
        match after.get(i) {
            Some(b' ' | b'\t') if i < MAX_TRANSPORT_PADDING => i += 1,
            Some(b'\r') => {
                return match after.get(i + 1) {
                    Some(b'\n') => Tail::Part(i + 2),
                    Some(_) => Tail::Content,
                    None if at_end => Tail::Content,
                    None => Tail::NeedMore,
                }
            }
            // Anything else — including padding past the ceiling — means this
            // was not a delimiter line.
            Some(_) => return Tail::Content,
            None if at_end => return Tail::Content,
            None => return Tail::NeedMore,
        }
    }
}

fn parse(body: &[u8], boundary: &str) -> Result<Vec<Part>> {
    // RFC 2046 §5.1.1: a delimiter is CRLF followed by `--boundary`, and the
    // line it opens carries nothing but transport padding before its CRLF. Both
    // halves of that are load-bearing.
    //
    // Splitting on the bare `--boundary` let a part whose *content* embedded the
    // boundary forge additional parts — so a single uploaded file could inject a
    // second field the client never sent. Accepting arbitrary bytes where only
    // padding is allowed left exactly the same hole one character further along:
    // `\r\n--boundaryZ` is content to Go, to Python and to anything else in
    // front, and used to be a delimiter here. That difference of opinion is the
    // dangerous half, because a proxy or WAF that filters on a field name never
    // sees the field it is there to reject while the origin parses it out and
    // acts on it.
    //
    // So a match whose tail is not a well-formed delimiter line is content, and
    // the scan carries on straight through it.
    //
    // The opening delimiter has no preceding CRLF, so a synthetic one is
    // prepended rather than special-casing offset 0.
    let delim = format!("\r\n--{boundary}").into_bytes();
    let mut framed = Vec::with_capacity(body.len() + 2);
    framed.extend_from_slice(b"\r\n");
    framed.extend_from_slice(body);
    let body: &[u8] = &framed;

    let mut parts = Vec::new();
    // Where the current part's headers begin, or `None` while the scan is still
    // in the preamble that precedes the first delimiter.
    let mut part_at: Option<usize> = None;
    let mut closed = false;
    let mut at = 0;

    while let Some(rel) = find(&body[at..], &delim) {
        let hit = at + rel;
        // The whole body is in hand, so the tail is always decidable; the
        // `NeedMore` arm below only keeps the match exhaustive without a panic.
        match delimiter_tail(&body[hit + delim.len()..], true) {
            Tail::Part(skip) => {
                if let Some(from) = part_at {
                    push_part(&mut parts, &body[from..hit])?;
                }
                at = hit + delim.len() + skip;
                part_at = Some(at);
            }
            Tail::Close => {
                // `take` clears the open part because it has just ended
                // properly; the check after the loop is only for a body that
                // ran out with a part still open.
                if let Some(from) = part_at.take() {
                    push_part(&mut parts, &body[from..hit])?;
                }
                closed = true;
                break;
            }
            // Content the scan must keep. Resuming one byte past the start of
            // the false match rather than past the whole of it matters when the
            // boundary can overlap itself: a real delimiter may begin inside the
            // bytes that just failed to be one.
            Tail::Content | Tail::NeedMore => at = hit + 1,
        }
    }

    if part_at.is_some() {
        // A part was opened and the body ran out before its delimiter arrived.
        // Reporting the truncation is what the streaming parser does, and a
        // truncated upload silently becoming a shorter file is the failure this
        // avoids.
        return Err(Error::bad_request("multipart body ended inside a part"));
    }
    if !closed {
        // No delimiter line anywhere, so this was not a multipart body at all.
        // It used to be `200` with no parts here and `400` through
        // `MultipartStream`; one body, one answer.
        return Err(Error::bad_request(
            "multipart body has no boundary delimiter",
        ));
    }

    Ok(parts)
}

/// Parse one part out of the bytes between the end of its delimiter line and
/// the start of the next delimiter, and append it.
///
/// `chunk` carries no framing of its own: the CRLF ending the delimiter line
/// was consumed by the caller and the CRLF before the next delimiter is part of
/// that delimiter, so everything after the header block is content exactly as
/// it was sent.
fn push_part(parts: &mut Vec<Part>, chunk: &[u8]) -> Result<()> {
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
    Ok(())
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
    /// A whole delimiter line was consumed, padding and CRLF included, so the
    /// buffer now starts at the next part's headers. Whoever matched the
    /// delimiter has already decided it was one, which is why nothing further
    /// re-examines those bytes.
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

        // AfterDelimiter: the delimiter line was classified and consumed where
        // it was matched, so what is buffered here is the part's header block.
        // This used to skip to the next CRLF from wherever the delimiter ended,
        // which quietly accepted arbitrary bytes in place of the transport
        // padding RFC 2046 allows, and so let `\r\n--boundaryZ` inside a part's
        // content open a part of its own.
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

    /// Decide whether the boundary match at `at` in the buffer really opens a
    /// delimiter line, reading more of the body while the tail is ambiguous.
    ///
    /// Never returns [`Tail::NeedMore`]: once the body has drained there is
    /// nothing left to wait for and the tail is judged on what is there. The
    /// wait is bounded by [`MAX_TRANSPORT_PADDING`] plus the two bytes of the
    /// CRLF, so a hostile run of padding cannot grow the buffer.
    async fn classify_at(&mut self, at: usize) -> Result<Tail> {
        loop {
            let after = at + self.delimiter.len();
            let tail = delimiter_tail(&self.buffer[after..], self.drained);
            if tail != Tail::NeedMore {
                return Ok(tail);
            }
            self.fill().await?;
        }
    }

    /// Discard everything up to and including the next delimiter line.
    async fn consume_through_delimiter(&mut self) -> Result<()> {
        loop {
            if let Some(at) = find(&self.buffer, &self.delimiter) {
                match self.classify_at(at).await? {
                    Tail::Part(skip) => {
                        let _ = self.buffer.split_to(at + self.delimiter.len() + skip);
                        self.phase = Phase::AfterDelimiter;
                        return Ok(());
                    }
                    Tail::Close => {
                        self.phase = Phase::Done;
                        return Ok(());
                    }
                    // A boundary-looking run that is not a delimiter line is
                    // preamble like everything else before the first real one,
                    // so it can simply be dropped. Dropping one byte rather
                    // than the whole match keeps a genuine delimiter that
                    // overlaps it findable.
                    Tail::Content | Tail::NeedMore => {
                        let _ = self.buffer.split_to(at + 1);
                        continue;
                    }
                }
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
        // How far into the buffer the search may start: everything before it has
        // already been judged not to open a delimiter line. Only ever advances
        // within one call, and `fill` appends, so the offset stays valid.
        let mut from = 0;
        loop {
            if let Some(rel) = find(&self.buffer[from..], &self.delimiter) {
                let at = from + rel;
                // A boundary match only ends the part if a well-formed
                // delimiter line follows it. Deciding that here rather than in
                // `next_field` is what makes the decision act on the content:
                // by the time the handler has been handed these bytes it is too
                // late to take them back and call them a delimiter, or to call
                // a delimiter content.
                match self.classify_at(at).await? {
                    Tail::Part(skip) => {
                        let data = self.buffer.split_to(at).freeze();
                        let _ = self.buffer.split_to(self.delimiter.len() + skip);
                        self.phase = Phase::AfterDelimiter;
                        return Ok((!data.is_empty()).then_some(data));
                    }
                    Tail::Close => {
                        let data = self.buffer.split_to(at).freeze();
                        self.phase = Phase::Done;
                        return Ok((!data.is_empty()).then_some(data));
                    }
                    // Not a delimiter, so these bytes are this part's content
                    // and will be handed out with the rest of it. Resuming one
                    // byte in keeps a real delimiter overlapping the false one
                    // findable.
                    Tail::Content | Tail::NeedMore => {
                        from = at + 1;
                        continue;
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delimiter_line_admits_only_spaces_and_tabs_before_its_crlf() {
        assert_eq!(delimiter_tail(b"\r\nnext", true), Tail::Part(2));
        assert_eq!(delimiter_tail(b" \t \r\nnext", true), Tail::Part(5));
        assert_eq!(delimiter_tail(b"--\r\n", true), Tail::Close);
        // The forged-part case: any other byte means the boundary-looking run
        // was content.
        assert_eq!(delimiter_tail(b"z\r\nnext", true), Tail::Content);
        assert_eq!(delimiter_tail(b"-z", true), Tail::Content);
        assert_eq!(delimiter_tail(b"\rx", true), Tail::Content);
        // Padding past the ceiling is refused the same way, since a parser that
        // buffers unbounded padding is a parser with an unbounded buffer.
        let over = vec![b' '; MAX_TRANSPORT_PADDING + 1];
        assert_eq!(delimiter_tail(&over, true), Tail::Content);
        // A truncation is only undecidable while more bytes may still arrive.
        assert_eq!(delimiter_tail(b"", false), Tail::NeedMore);
        assert_eq!(delimiter_tail(b"\r", false), Tail::NeedMore);
        assert_eq!(delimiter_tail(b"-", false), Tail::NeedMore);
        assert_eq!(delimiter_tail(b"", true), Tail::Content);
        assert_eq!(delimiter_tail(b"\r", true), Tail::Content);
    }

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
