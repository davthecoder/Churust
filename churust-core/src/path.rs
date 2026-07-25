//! Percent-decoding for URL path segments.
//!
//! Deliberately separate from the query-string decoder in [`crate::call`]. Two
//! differences matter, and both are defects if carried over:
//!
//! - `+` is a literal plus in a path. Only `application/x-www-form-urlencoded`
//!   query strings treat it as a space.
//! - Invalid UTF-8 is an error, not `U+FFFD`. Replacement characters collapse
//!   distinct byte sequences into the same string, which is unsound input to a
//!   path-safety decision.
//!
//! Callers must split on `/` **before** decoding. Decoding first would let
//! `%2F` manufacture separators and forge extra path segments.

/// Percent-decode one path segment.
///
/// Returns `None` when the input contains a malformed escape (`%zz`, a
/// truncated `%4`) or decodes to bytes that are not valid UTF-8. Callers turn
/// `None` into `400 Bad Request`.
pub(crate) fn decode_path_segment(raw: &str) -> Option<String> {
    // Overwhelmingly the common case, and worth not allocating for.
    if !raw.contains('%') {
        return Some(raw.to_string());
    }

    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // A complete escape needs two more bytes.
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    // Reject rather than replace. U+FFFD would map distinct byte sequences onto
    // the same string, and that string then feeds a path-safety decision.
    String::from_utf8(out).ok()
}

/// What to do when a request path is a non-canonical spelling of a route.
///
/// Interior empty segments — `//a`, `/a//b` — were once collapsed silently,
/// which gave every resource several URLs. That is not a traversal problem, but
/// it is an aliasing one, and aliases have teeth:
///
/// - Middleware, guards and proxy rules that key on a literal prefix
///   (`path.starts_with("/admin")`) are bypassable with `//admin`.
/// - Caches key on the URL, so one resource occupies several entries and an
///   intermediary can disagree with the origin about identity.
///
/// A **trailing** slash is not treated as an alias — see
/// [`canonical_path`] for why removing it would break directory listings.
///
/// Whatever the policy, `%2F` is never decoded into a separator: an encoded
/// slash is data inside one segment, and decoding it early is how a
/// normalisation change turns into a traversal bug.
///
/// ```
/// use churust_core::{Churust, Call, PathPolicy, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .path_policy(PathPolicy::Redirect)
///     .routing(|r| { r.get("/a", |_c: Call| async { "a" }); })
///     .build();
/// let res = TestClient::new(app).get("//a").send().await;
/// assert_eq!(res.status(), http::StatusCode::PERMANENT_REDIRECT);
/// assert_eq!(res.header("location"), Some("/a"));
/// # });
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathPolicy {
    /// Refuse an alias: `//a` is not `/a`, and gets `404`.
    ///
    /// The default. Fewest surprises and the strongest cache identity — one
    /// resource, one URL.
    #[default]
    Strict,
    /// Redirect an alias to its canonical form with `308 Permanent Redirect`.
    ///
    /// Kind to hand-typed and hand-written URLs. `308` rather than `301` so a
    /// `POST` stays a `POST`; `301` permits a client to retry as `GET`, which
    /// silently drops the body.
    Redirect,
    /// Collapse aliases silently, serving them as if canonical.
    ///
    /// The pre-`PathPolicy` behaviour, kept so an application with
    /// alias-shaped links has somewhere to stand while it fixes them. It
    /// creates URL aliases by design; prefer `Strict` or `Redirect`.
    Collapse,
}

/// The canonical spelling of `path`, or `None` if it is already canonical.
///
/// Canonical means no *interior* empty segments — no repeated slashes.
///
/// **A trailing slash is deliberately left alone.** It is not merely
/// cosmetic: it distinguishes a directory from a file. A listing served at
/// `/files/` emits bare relative links (`<a href="a.txt">`), which a browser
/// resolves against the trailing slash; the same markup at `/files` would
/// resolve to `/a.txt`. Rather than pick a spelling here, `StaticFiles`
/// redirects a directory URL that lacks the slash (`308`), so there is still
/// exactly one URL per directory — this policy just is not the thing enforcing
/// it.
///
/// Interior slashes carry no such meaning, and they are the ones with teeth:
/// `//admin` is what slips past a `path.starts_with("/admin")` check.
///
/// ```
/// use churust_core::path::canonical_path;
///
/// assert_eq!(canonical_path("/a/b"), None);              // already canonical
/// assert_eq!(canonical_path("/"), None);                 // the root
/// assert_eq!(canonical_path("/a/"), None);               // trailing slash is significant
/// assert_eq!(canonical_path("//a//b").as_deref(), Some("/a/b"));
/// assert_eq!(canonical_path("//a//b/").as_deref(), Some("/a/b/"));
/// ```
pub fn canonical_path(path: &str) -> Option<String> {
    // The overwhelmingly common case, checked first so ordinary requests never
    // allocate.
    if !path.contains("//") {
        return None;
    }
    let trailing = path.len() > 1 && path.ends_with('/');
    let joined: String = path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    Some(match (joined.is_empty(), trailing) {
        // `//` and `///` all mean the root.
        (true, _) => "/".to_string(),
        (false, true) => format!("/{joined}/"),
        (false, false) => format!("/{joined}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(decode_path_segment("John%20Doe").unwrap(), "John Doe");
        assert_eq!(decode_path_segment("a%2Eb").unwrap(), "a.b");
        assert_eq!(decode_path_segment("%E2%9C%93").unwrap(), "✓");
    }

    #[test]
    fn leaves_plus_alone() {
        assert_eq!(
            decode_path_segment("a+b").unwrap(),
            "a+b",
            "`+` is a literal in a path; only query strings map it to a space"
        );
    }

    #[test]
    fn passes_through_plain_text() {
        assert_eq!(decode_path_segment("users").unwrap(), "users");
        assert_eq!(decode_path_segment("").unwrap(), "");
    }

    #[test]
    fn decodes_dot_segments_without_interpreting_them() {
        // Decoding produces "..". Rejecting it is the caller's job.
        assert_eq!(decode_path_segment("%2e%2e").unwrap(), "..");
        assert_eq!(decode_path_segment("%2E%2E").unwrap(), "..");
    }

    #[test]
    fn decodes_once_only() {
        // %252e is "%2e" after one pass, not ".". Double decoding is how
        // traversal filters get bypassed.
        assert_eq!(decode_path_segment("%252e").unwrap(), "%2e");
        assert_eq!(decode_path_segment("%252e%252e").unwrap(), "%2e%2e");
    }

    #[test]
    fn rejects_malformed_escapes() {
        assert!(decode_path_segment("%zz").is_none());
        assert!(decode_path_segment("%4").is_none());
        assert!(decode_path_segment("%").is_none());
        assert!(decode_path_segment("ok%").is_none());
        assert!(decode_path_segment("%g0").is_none());
    }

    #[test]
    fn rejects_invalid_utf8() {
        // 0xFF is not valid UTF-8 in any position.
        assert!(decode_path_segment("%FF").is_none());
        // A lone continuation byte.
        assert!(decode_path_segment("%80").is_none());
        // A truncated multi-byte sequence.
        assert!(decode_path_segment("%E2%9C").is_none());
    }

    #[test]
    fn decodes_an_encoded_separator_to_a_literal() {
        // The decoder decodes. Refusing separators is the caller's decision,
        // because it differs between the router and StaticFiles.
        assert_eq!(decode_path_segment("a%2Fb").unwrap(), "a/b");
        assert_eq!(decode_path_segment("a%5Cb").unwrap(), "a\\b");
    }
}
