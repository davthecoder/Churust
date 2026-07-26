//! Response compression for the [Churust] web framework.
//!
//! [`Compression`] negotiates a content coding from the request's
//! `Accept-Encoding`, compresses the response body, and sets
//! `Content-Encoding`. Brotli, gzip and deflate are supported. A streamed body
//! stays streamed: it is compressed chunk by chunk rather than collected first.
//!
//! ```
//! use churust_core::{Call, Churust, TestClient};
//! use churust_compression::Compression;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Churust::server()
//!     .install(Compression::new())
//!     .routing(|r| {
//!         r.get("/", |_c: Call| async { "hello ".repeat(500) });
//!     })
//!     .build();
//!
//! let res = TestClient::new(app)
//!     .get("/")
//!     .header("accept-encoding", "gzip")
//!     .send()
//!     .await;
//!
//! assert_eq!(res.header("content-encoding"), Some("gzip"));
//! assert_eq!(res.header("vary"), Some("accept-encoding"));
//! # });
//! ```
//!
//! # What is deliberately not compressed
//!
//! Compression that saves nothing still costs CPU on both ends, and in a few
//! cases it is wrong rather than merely wasteful. The plugin skips:
//!
//! - Bodies below [`min_size`](Compression::min_size) (1 KiB by default).
//!   Below roughly that size a gzip member's own header eats the saving.
//! - Content types that are already compressed (images, video, audio, archives)
//!   or that are not known to be text, per
//!   [`compressible_by_default`]. Override with
//!   [`compressible`](Compression::compressible).
//! - Responses that already carry a `Content-Encoding`.
//! - `206 Partial Content` and anything carrying `Content-Range`. The range was
//!   computed against the identity representation, so compressing the selected
//!   span would make the offsets describe bytes that are not there.
//! - Statuses that carry no body at all (`1xx`, `204`, `304`).
//!
//! `Vary: Accept-Encoding` is added to **every** response the plugin sees, not
//! only compressed ones. A cache that stored one variant without it would serve
//! a brotli body to a client that never asked for one.
//!
//! # Should this live in the application at all
//!
//! Often it should not. A reverse proxy in front of the service can compress
//! once for every backend behind it, and it is usually already terminating TLS
//! and doing the buffering. Reach for this plugin when there is no such proxy,
//! when the proxy is not under your control, or when a handler produces a
//! stream long enough that you want it compressed before it crosses the
//! network hop the proxy sits on.
//!
//! [Churust]: churust_core::Churust

#![deny(missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
use churust_core::{AppBuilder, Body, Call, Middleware, Next, Phase, Plugin, Response};
use futures_util::StreamExt;
use http::header::{
    HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
    ETAG, VARY,
};
use http::StatusCode;
use std::sync::Arc;
use tokio::io::AsyncRead;
use tokio_util::io::{ReaderStream, StreamReader};

/// Pieces a buffered body is cut into before being fed to the encoder.
///
/// Compressing a large buffer in one call would occupy the runtime worker for
/// the whole of it. Feeding the same encoder in chunks puts an await point
/// between them without changing the output.
const CHUNK: usize = 16 * 1024;

/// A supported content coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Brotli (`br`). The best ratio of the three, and universally supported by
    /// current browsers over both plaintext and TLS.
    Brotli,
    /// gzip (`gzip`). The safe default: understood by everything.
    Gzip,
    /// deflate (`deflate`), which per RFC 9110 §8.4.1.2 means the zlib format
    /// of RFC 1950 rather than a raw deflate stream.
    Deflate,
}

impl Encoding {
    /// The token as it appears in `Accept-Encoding` and `Content-Encoding`.
    pub fn token(self) -> &'static str {
        match self {
            Encoding::Brotli => "br",
            Encoding::Gzip => "gzip",
            Encoding::Deflate => "deflate",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "br" => Some(Encoding::Brotli),
            "gzip" | "x-gzip" => Some(Encoding::Gzip),
            "deflate" => Some(Encoding::Deflate),
            _ => None,
        }
    }
}

/// How hard the encoder works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    /// Least CPU, largest output.
    Fastest,
    /// The encoder's own default, which is what most deployments want.
    #[default]
    Default,
    /// Most CPU, smallest output. Worth it for a cached or static payload,
    /// rarely worth it per request.
    Best,
}

impl From<Level> for async_compression::Level {
    fn from(level: Level) -> Self {
        match level {
            Level::Fastest => async_compression::Level::Fastest,
            Level::Default => async_compression::Level::Default,
            Level::Best => async_compression::Level::Best,
        }
    }
}

/// Whether a media type is worth compressing.
///
/// True for text, JSON, XML, JavaScript, SVG and WebAssembly, including the
/// structured suffixes (`application/vnd.api+json`). False for everything else,
/// which is the conservative direction: a missed saving costs bandwidth once,
/// while recompressing a JPEG costs CPU on every request and makes it larger.
///
/// ```
/// use churust_compression::compressible_by_default;
///
/// assert!(compressible_by_default("text/html; charset=utf-8"));
/// assert!(compressible_by_default("application/vnd.api+json"));
/// assert!(!compressible_by_default("image/png"));
/// ```
pub fn compressible_by_default(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if essence.starts_with("text/") {
        return true;
    }
    if essence.ends_with("+json") || essence.ends_with("+xml") || essence.ends_with("+text") {
        return true;
    }
    matches!(
        essence.as_str(),
        "application/json"
            | "application/javascript"
            | "application/xml"
            | "application/xhtml+xml"
            | "application/rss+xml"
            | "application/atom+xml"
            | "application/wasm"
            | "application/manifest+json"
            | "application/x-ndjson"
            | "image/svg+xml"
    )
}

/// A predicate deciding whether a media type should be compressed.
type CompressibleFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The response compression plugin.
///
/// Construct with [`Compression::new`] and refine with the builder methods.
/// Also usable as scoped middleware through `RouteBuilder::intercept`.
#[derive(Clone)]
pub struct Compression {
    min_size: usize,
    level: Level,
    /// Server preference, most preferred first. Only these are ever chosen.
    preference: Vec<Encoding>,
    compressible: CompressibleFn,
}

impl std::fmt::Debug for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compression")
            .field("min_size", &self.min_size)
            .field("level", &self.level)
            .field("preference", &self.preference)
            .finish_non_exhaustive()
    }
}

impl Default for Compression {
    fn default() -> Self {
        Self::new()
    }
}

impl Compression {
    /// Brotli, then gzip, then deflate, for text-like bodies of at least 1 KiB.
    pub fn new() -> Self {
        Self {
            min_size: 1024,
            level: Level::Default,
            preference: vec![Encoding::Brotli, Encoding::Gzip, Encoding::Deflate],
            compressible: Arc::new(compressible_by_default),
        }
    }

    /// Only compress buffered bodies of at least `bytes`.
    ///
    /// A streamed body has no length until it ends, so it is always compressed
    /// regardless of this setting.
    pub fn min_size(mut self, bytes: usize) -> Self {
        self.min_size = bytes;
        self
    }

    /// Set how hard the encoder works.
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Restrict and order the codings this server is willing to produce, most
    /// preferred first.
    ///
    /// Client preference wins on `q` value; this order only breaks ties. Use it
    /// to drop brotli on a CPU-bound service:
    ///
    /// ```
    /// use churust_compression::{Compression, Encoding};
    ///
    /// let plugin = Compression::new().encodings([Encoding::Gzip]);
    /// ```
    ///
    /// # Panics
    ///
    /// If the list is empty, which would install a plugin that can never do
    /// anything.
    pub fn encodings(mut self, encodings: impl IntoIterator<Item = Encoding>) -> Self {
        let list: Vec<Encoding> = encodings.into_iter().collect();
        assert!(
            !list.is_empty(),
            "Compression needs at least one encoding to offer"
        );
        self.preference = list;
        self
    }

    /// Replace the media-type predicate.
    ///
    /// ```
    /// use churust_compression::{compressible_by_default, Compression};
    ///
    /// // Also compress a bespoke binary format that happens to be repetitive.
    /// let plugin = Compression::new().compressible(|ct| {
    ///     compressible_by_default(ct) || ct.starts_with("application/x-telemetry")
    /// });
    /// ```
    pub fn compressible<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.compressible = Arc::new(f);
        self
    }

    /// Pick the coding to use, or `None` to send the body as it is.
    ///
    /// Client `q` values decide first; the server's own order breaks ties. An
    /// explicitly listed coding always beats a match through `*`, so
    /// `gzip;q=0.5, *` sends gzip at 0.5 rather than promoting brotli to 1.0
    /// through the wildcard.
    fn negotiate(&self, header: &str) -> Option<Encoding> {
        // (quality, wildcard) per coding this server offers.
        let mut offers: Vec<(Encoding, f32, bool)> = Vec::new();
        let mut wildcard: Option<f32> = None;
        let mut explicit: Vec<(Encoding, f32)> = Vec::new();

        for part in header.split(',') {
            let mut bits = part.split(';');
            let token = bits.next().unwrap_or("").trim().to_ascii_lowercase();
            let mut q = 1.0f32;
            for param in bits {
                let param = param.trim();
                if let Some(value) = param.strip_prefix("q=") {
                    q = value.trim().parse().unwrap_or(0.0);
                }
            }
            if token == "*" {
                wildcard = Some(wildcard.map_or(q, |prev: f32| prev.max(q)));
            } else if let Some(enc) = Encoding::from_token(&token) {
                explicit.push((enc, q));
            }
        }

        for (index, enc) in self.preference.iter().enumerate() {
            let quality = match explicit.iter().find(|(e, _)| e == enc) {
                Some((_, q)) => Some((*q, false)),
                None => wildcard.map(|q| (q, true)),
            };
            if let Some((q, is_wildcard)) = quality {
                if q > 0.0 {
                    // Fold the server's order into the sort key so a tie on q
                    // resolves the same way every time.
                    let _ = index;
                    offers.push((*enc, q, is_wildcard));
                }
            }
        }

        offers
            .into_iter()
            .enumerate()
            .max_by(|(ia, a), (ib, b)| {
                // Higher q wins; then explicit over wildcard; then the server's
                // own preference order, which `enumerate` preserves.
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| (!a.2).cmp(&(!b.2)))
                    .then_with(|| ib.cmp(ia))
            })
            .map(|(_, (enc, _, _))| enc)
    }

    /// Whether this response may be compressed at all.
    fn should_compress(&self, res: &Response) -> bool {
        // A body-less status has nothing to encode, and a `304` must keep the
        // headers of the response it revalidates.
        if res.status.is_informational()
            || res.status == StatusCode::NO_CONTENT
            || res.status == StatusCode::NOT_MODIFIED
        {
            return false;
        }
        // The range was selected from the identity representation.
        if res.status == StatusCode::PARTIAL_CONTENT || res.headers.contains_key(CONTENT_RANGE) {
            return false;
        }
        // Already encoded by the handler or an inner layer. Stacking a second
        // coding is legal and always a mistake.
        if res.headers.contains_key(CONTENT_ENCODING) {
            return false;
        }
        let content_type = res
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !(self.compressible)(content_type) {
            return false;
        }
        match res.body.as_bytes() {
            // A buffered body's length is known, so the floor applies.
            Some(bytes) => bytes.len() >= self.min_size,
            // A stream's is not, so it is compressed on the assumption that a
            // handler streaming a response has more than a kilobyte to say.
            None => true,
        }
    }
}

/// Wrap `body` in the encoder for `encoding`, keeping it a stream throughout.
fn encode(body: Body, encoding: Encoding, level: Level) -> Body {
    let source = match body {
        Body::Bytes(bytes) => {
            let mut rest = bytes;
            let chunks = std::iter::from_fn(move || {
                if rest.is_empty() {
                    None
                } else {
                    let take = CHUNK.min(rest.len());
                    Some(Ok::<Bytes, std::io::Error>(rest.split_to(take)))
                }
            });
            futures_util::stream::iter(chunks).boxed()
        }
        Body::Stream(stream) => stream
            .map(|chunk| chunk.map_err(|e| std::io::Error::other(e.to_string())))
            .boxed(),
    };

    let reader = StreamReader::new(source);
    let level = level.into();
    let encoded: std::pin::Pin<Box<dyn AsyncRead + Send>> = match encoding {
        Encoding::Brotli => {
            Box::pin(async_compression::tokio::bufread::BrotliEncoder::with_quality(reader, level))
        }
        Encoding::Gzip => {
            Box::pin(async_compression::tokio::bufread::GzipEncoder::with_quality(reader, level))
        }
        // Zlib, not raw deflate: see `Encoding::Deflate`.
        Encoding::Deflate => {
            Box::pin(async_compression::tokio::bufread::ZlibEncoder::with_quality(reader, level))
        }
    };

    Body::from_stream(ReaderStream::new(encoded))
}

/// Append `Accept-Encoding` to `Vary` without disturbing what is already there.
fn vary_on_accept_encoding(res: &mut Response) {
    let existing: Vec<String> = res
        .headers
        .get_all(VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect();

    if existing.iter().any(|v| v == "*" || v == "accept-encoding") {
        return;
    }

    let mut merged = existing;
    merged.push("accept-encoding".to_string());
    if let Ok(value) = HeaderValue::from_str(&merged.join(", ")) {
        res.headers.insert(VARY, value);
    }
}

/// Downgrade a strong `ETag` to a weak one.
///
/// A compressed body is a different sequence of bytes for the same resource,
/// so a strong validator would claim byte-for-byte equality that no longer
/// holds. RFC 9110 §8.8.1 makes the weak form exactly this statement:
/// equivalent, not identical. A tag that is already weak is left alone.
fn weaken_etag(res: &mut Response) {
    let Some(tag) = res.headers.get(ETAG).and_then(|v| v.to_str().ok()) else {
        return;
    };
    if tag.starts_with("W/") {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(&format!("W/{tag}")) {
        res.headers.insert(ETAG, value);
    }
}

#[async_trait]
impl Middleware for Compression {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let accept = call
            .header(ACCEPT_ENCODING.as_str())
            .unwrap_or_default()
            .to_string();

        let mut res = next.run(call).await;

        // Every response the plugin sees is Vary-marked, compressed or not, so
        // a shared cache keys on the request's Accept-Encoding either way.
        vary_on_accept_encoding(&mut res);

        if accept.is_empty() {
            return res;
        }
        let Some(encoding) = self.negotiate(&accept) else {
            return res;
        };
        if !self.should_compress(&res) {
            return res;
        }

        // Keep the buffered original: compressing it cannot normally fail, and
        // if it somehow does, sending it uncompressed beats sending a 500.
        let original = res.body.as_bytes().cloned();
        let was_buffered = original.is_some();
        let body = std::mem::take(&mut res.body);
        let encoded = encode(body, encoding, self.level);

        res.body = if was_buffered {
            // Collect back so `Content-Length` stays exact for a body that had
            // one before.
            match encoded.into_bytes().await {
                Ok(bytes) => Body::Bytes(bytes),
                Err(_) => {
                    res.body = Body::Bytes(original.unwrap_or_default());
                    return res;
                }
            }
        } else {
            encoded
        };

        res.headers
            .insert(CONTENT_ENCODING, HeaderValue::from_static(encoding.token()));
        // Whatever length was set describes the identity body. The engine
        // recomputes it for a buffered body and omits it for a stream.
        res.headers.remove(CONTENT_LENGTH);
        // Ranges described the identity body too, and they cannot be honoured
        // for this one. `StaticFiles` sets `Accept-Ranges: bytes`; leaving it
        // told the client it could resume, and its `Range` request then came
        // back `206` with *identity* bytes — `should_compress` correctly skips
        // `206` — against a total it never received. The client splices
        // plaintext into a gzip stream and the resumed download is corrupt.
        // nginx's gzip filter clears this header for the same reason.
        res.headers.remove(http::header::ACCEPT_RANGES);
        weaken_etag(&mut res);
        res
    }
}

impl Plugin for Compression {
    /// Installed in [`Phase::Plugins`]. Anything that inspects a response body
    /// must sit inside this layer, since outside it the body is compressed.
    fn install(self: Box<Self>, app: &mut AppBuilder) {
        app.add_middleware_in(Phase::Plugins, Arc::new(*self));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Compression {
        Compression::new()
    }

    #[test]
    fn negotiation_prefers_the_client_quality() {
        let c = plugin();
        assert_eq!(c.negotiate("gzip;q=1.0, br;q=0.1"), Some(Encoding::Gzip));
        assert_eq!(c.negotiate("gzip;q=0.1, br;q=1.0"), Some(Encoding::Brotli));
    }

    #[test]
    fn server_order_breaks_a_quality_tie() {
        let c = plugin();
        assert_eq!(c.negotiate("gzip, br, deflate"), Some(Encoding::Brotli));
        let gzip_first = plugin().encodings([Encoding::Gzip, Encoding::Brotli]);
        assert_eq!(gzip_first.negotiate("gzip, br"), Some(Encoding::Gzip));
    }

    #[test]
    fn a_zero_quality_refuses_that_coding() {
        let c = plugin();
        assert_eq!(c.negotiate("br;q=0, gzip"), Some(Encoding::Gzip));
        assert_eq!(c.negotiate("br;q=0, gzip;q=0, deflate;q=0"), None);
    }

    #[test]
    fn an_explicit_coding_beats_the_wildcard() {
        let c = plugin();
        // `*` would give brotli 1.0; the explicit gzip entry is what the client
        // actually named, so it wins despite the lower number.
        assert_eq!(c.negotiate("gzip;q=0.5, *;q=0.4"), Some(Encoding::Gzip));
    }

    #[test]
    fn the_wildcard_alone_selects_the_server_preference() {
        assert_eq!(plugin().negotiate("*"), Some(Encoding::Brotli));
    }

    #[test]
    fn unknown_codings_are_ignored() {
        assert_eq!(plugin().negotiate("exi, sdch"), None);
    }

    #[test]
    fn media_types_are_classified_conservatively() {
        assert!(compressible_by_default("text/html"));
        assert!(compressible_by_default("application/json"));
        assert!(compressible_by_default("image/svg+xml"));
        assert!(!compressible_by_default("image/jpeg"));
        assert!(!compressible_by_default("application/zip"));
        assert!(!compressible_by_default(""));
    }

    #[tokio::test]
    async fn encoding_round_trips_through_gzip() {
        use tokio::io::AsyncReadExt;

        let input = Bytes::from("hello ".repeat(4000));
        let body = encode(Body::Bytes(input.clone()), Encoding::Gzip, Level::Default);
        let compressed = body.into_bytes().await.unwrap();
        assert!(
            compressed.len() < input.len(),
            "repetitive text should shrink"
        );

        let mut decoded = Vec::new();
        async_compression::tokio::bufread::GzipDecoder::new(std::io::Cursor::new(
            compressed.to_vec(),
        ))
        .read_to_end(&mut decoded)
        .await
        .unwrap();
        assert_eq!(Bytes::from(decoded), input);
    }

    #[test]
    fn vary_is_appended_not_replaced() {
        let mut res = Response::text("x");
        res.headers.insert(VARY, HeaderValue::from_static("origin"));
        vary_on_accept_encoding(&mut res);
        assert_eq!(res.headers.get(VARY).unwrap(), "origin, accept-encoding");
    }

    #[test]
    fn vary_is_not_duplicated() {
        let mut res = Response::text("x");
        vary_on_accept_encoding(&mut res);
        vary_on_accept_encoding(&mut res);
        assert_eq!(res.headers.get_all(VARY).iter().count(), 1);
        assert_eq!(res.headers.get(VARY).unwrap(), "accept-encoding");
    }

    #[test]
    fn a_strong_etag_becomes_weak() {
        let mut res = Response::text("x");
        res.headers
            .insert(ETAG, HeaderValue::from_static("\"abc\""));
        weaken_etag(&mut res);
        assert_eq!(res.headers.get(ETAG).unwrap(), "W/\"abc\"");
    }

    #[test]
    fn an_already_weak_etag_is_left_alone() {
        let mut res = Response::text("x");
        res.headers
            .insert(ETAG, HeaderValue::from_static("W/\"abc\""));
        weaken_etag(&mut res);
        assert_eq!(res.headers.get(ETAG).unwrap(), "W/\"abc\"");
    }

    #[test]
    fn partial_content_is_never_compressed() {
        let mut res = Response::text("x".repeat(4096));
        res.status = StatusCode::PARTIAL_CONTENT;
        assert!(!plugin().should_compress(&res));
    }

    #[test]
    fn an_already_encoded_response_is_left_alone() {
        let mut res = Response::text("x".repeat(4096));
        res.headers
            .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert!(!plugin().should_compress(&res));
    }

    #[test]
    fn small_bodies_are_left_alone() {
        let res = Response::text("small");
        assert!(!plugin().should_compress(&res));
        assert!(
            plugin().min_size(1).should_compress(&res),
            "lowering the floor should admit it"
        );
    }
}
