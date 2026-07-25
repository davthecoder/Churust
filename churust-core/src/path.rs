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
