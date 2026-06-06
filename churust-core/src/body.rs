//! The response [`Body`]: either a fully-buffered `Bytes` payload or a lazy
//! stream of byte chunks (for files, large/dynamic responses, SSE, etc.).

use crate::error::{Error, Result};
use bytes::{Bytes, BytesMut};
use futures_util::stream::{Stream, StreamExt};
use std::pin::Pin;

/// A response body.
///
/// Construct buffered bodies with `From` (`Body::from(bytes)`,
/// `"text".into()`), an empty body with [`Body::empty`], or a streaming body
/// with [`Body::from_stream`]. The engine sends buffered bodies in one shot and
/// streamed bodies frame-by-frame.
pub enum Body {
    /// A fully-buffered, in-memory body.
    Bytes(Bytes),
    /// A lazily-produced stream of byte chunks.
    Stream(Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + 'static>>),
}

impl Body {
    /// An empty buffered body.
    pub fn empty() -> Self {
        Body::Bytes(Bytes::new())
    }

    /// Build a streaming body from any `Stream` of byte chunks. Chunk errors are
    /// converted into [`Error`] and surface when the body is read/collected.
    pub fn from_stream<S, E>(stream: S) -> Self
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
        E: std::fmt::Display,
    {
        let mapped =
            stream.map(|chunk| chunk.map_err(|e| Error::internal(format!("body stream: {e}"))));
        Body::Stream(Box::pin(mapped))
    }

    /// Borrow the buffered bytes, or `None` if this body is a stream. Used by
    /// middleware that only post-processes already-materialized bodies.
    pub fn as_bytes(&self) -> Option<&Bytes> {
        match self {
            Body::Bytes(b) => Some(b),
            Body::Stream(_) => None,
        }
    }

    /// Borrow the buffered bytes as a slice, or `None` for a stream.
    pub fn as_slice(&self) -> Option<&[u8]> {
        self.as_bytes().map(|b| b.as_ref())
    }

    /// True if this is a buffered, empty body. A stream is never reported empty
    /// (its length is unknown until read).
    pub fn is_empty(&self) -> bool {
        matches!(self, Body::Bytes(b) if b.is_empty())
    }

    /// Collect the whole body into `Bytes` (returns the buffer directly when
    /// already buffered, or drains the stream).
    pub async fn into_bytes(self) -> Result<Bytes> {
        match self {
            Body::Bytes(b) => Ok(b),
            Body::Stream(mut s) => {
                let mut buf = BytesMut::new();
                while let Some(chunk) = s.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(buf.freeze())
            }
        }
    }
}

impl Default for Body {
    fn default() -> Self {
        Body::empty()
    }
}

impl From<Bytes> for Body {
    fn from(b: Bytes) -> Self {
        Body::Bytes(b)
    }
}
impl From<Vec<u8>> for Body {
    fn from(b: Vec<u8>) -> Self {
        Body::Bytes(Bytes::from(b))
    }
}
impl From<String> for Body {
    fn from(s: String) -> Self {
        Body::Bytes(Bytes::from(s))
    }
}
impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Body::Bytes(Bytes::from_static(s.as_bytes()))
    }
}

/// Compare a body to `Bytes` (true only when buffered and equal). Lets tests and
/// callers assert on buffered bodies ergonomically.
impl PartialEq<Bytes> for Body {
    fn eq(&self, other: &Bytes) -> bool {
        matches!(self, Body::Bytes(b) if b == other)
    }
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Body::Bytes(b) => f.debug_tuple("Body::Bytes").field(b).finish(),
            Body::Stream(_) => f.write_str("Body::Stream(..)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn buffered_round_trips() {
        let body = Body::from(Bytes::from("hello"));
        assert_eq!(body.as_slice(), Some(&b"hello"[..]));
        assert!(!body.is_empty());
        assert_eq!(body.into_bytes().await.unwrap(), Bytes::from("hello"));
    }

    #[tokio::test]
    async fn empty_is_empty() {
        assert!(Body::empty().is_empty());
        assert_eq!(Body::empty().into_bytes().await.unwrap(), Bytes::new());
    }

    #[tokio::test]
    async fn stream_collects_and_has_no_bytes_view() {
        let chunks = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from("ab")),
            Ok(Bytes::from("cd")),
        ]);
        let body = Body::from_stream(chunks);
        assert!(body.as_bytes().is_none()); // streams expose no borrowed view
        assert!(!body.is_empty());
        assert_eq!(body.into_bytes().await.unwrap(), Bytes::from("abcd"));
    }

    #[test]
    #[allow(clippy::cmp_owned)] // exercises From<String>; the owned String is the input under test, not a throwaway
    fn partial_eq_bytes() {
        assert!(Body::from("x".to_string()) == Bytes::from("x"));
        let s = Body::from_stream(futures_util::stream::iter(vec![Ok::<_, std::io::Error>(
            Bytes::new(),
        )]));
        assert!(!(s == Bytes::new())); // a stream never equals bytes
    }
}
