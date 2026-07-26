//! Cookies: reading them off a request and building `Set-Cookie`.
//!
//! Defaults are the safe ones. A cookie is a credential until proven otherwise,
//! so [`Cookie::new`] sets `HttpOnly`, `SameSite=Lax` and `Path=/`; opt out
//! deliberately rather than opt in.

use std::fmt::Write as _;

/// The `SameSite` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// Never sent cross-site.
    Strict,
    /// Sent on top-level navigations. The default, and what most sessions want.
    Lax,
    /// Sent on every cross-site request. Requires `Secure`, per the spec.
    None,
}

impl SameSite {
    fn as_str(self) -> &'static str {
        match self {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        }
    }
}

/// A cookie to send with [`Response::with_cookie`](crate::Response::with_cookie).
#[derive(Debug, Clone)]
pub struct Cookie {
    name: String,
    value: String,
    path: Option<String>,
    domain: Option<String>,
    max_age: Option<i64>,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
}

impl Cookie {
    /// A cookie with safe defaults: `HttpOnly`, `SameSite=Lax`, `Path=/`.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            path: Some("/".into()),
            domain: None,
            max_age: None,
            secure: false,
            http_only: true,
            same_site: Some(SameSite::Lax),
        }
    }

    /// A cookie that deletes an existing one of the same name.
    ///
    /// `Path` must match the cookie being removed, so set it if the original
    /// used something other than `/`.
    pub fn removal(name: impl Into<String>) -> Self {
        let mut c = Self::new(name, "");
        c.max_age = Some(0);
        c
    }

    /// Set `Path`. `None` omits it.
    pub fn path(mut self, p: impl Into<String>) -> Self {
        self.path = Some(p.into());
        self
    }
    /// Set `Domain`.
    pub fn domain(mut self, d: impl Into<String>) -> Self {
        self.domain = Some(d.into());
        self
    }
    /// Set `Max-Age` in seconds.
    pub fn max_age(mut self, secs: i64) -> Self {
        self.max_age = Some(secs);
        self
    }
    /// Set `Secure` — the cookie is only sent over HTTPS.
    pub fn secure(mut self, yes: bool) -> Self {
        self.secure = yes;
        self
    }
    /// Set `HttpOnly` — script cannot read the cookie. On by default.
    pub fn http_only(mut self, yes: bool) -> Self {
        self.http_only = yes;
        self
    }
    /// Set `SameSite`. Defaults to `Lax`.
    pub fn same_site(mut self, s: SameSite) -> Self {
        self.same_site = Some(s);
        self
    }

    /// Render the `Set-Cookie` header value.
    pub fn to_header_value(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{}={}", encode(&self.name), encode(&self.value));
        if let Some(p) = &self.path {
            let _ = write!(out, "; Path={}", attribute_value(p));
        }
        if let Some(d) = &self.domain {
            let _ = write!(out, "; Domain={}", attribute_value(d));
        }
        if let Some(m) = self.max_age {
            let _ = write!(out, "; Max-Age={m}");
        }
        if self.secure {
            out.push_str("; Secure");
        }
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        if let Some(s) = self.same_site {
            let _ = write!(out, "; SameSite={}", s.as_str());
        }
        out
    }
}

/// Strip anything from an attribute value that would change the structure of
/// the header.
///
/// RFC 6265's `av-octet` is any CHAR except CTLs and `;`, and both exclusions
/// matter here. A `;` starts a new attribute, so an application scoping a
/// cookie with `.path(format!("/u/{slug}"))` and a hostile slug could append
/// `Max-Age=0` and delete the victim's session — or set `Secure`, a `Domain`,
/// or a second `Path`. A CR or LF is header injection outright, and previously
/// made `HeaderValue::from_str` reject the whole value, so the `Set-Cookie` was
/// dropped silently and the session was simply never issued.
///
/// Stripping rather than refusing: dropping the attribute would *widen* the
/// cookie's scope, which is the more dangerous failure of the two.
fn attribute_value(raw: &str) -> String {
    raw.chars()
        .filter(|c| *c != ';' && !c.is_control())
        .collect()
}

/// Percent-encode anything that would change the meaning of the header.
///
/// A raw `;` in a value would forge a second attribute, and a raw `,` can split
/// a header. Encoding is what keeps a value a value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            // The cookie-value grammar also permits '%', but we decode on read,
            // so letting a literal '%' through would make the round trip lossy:
            // a stored "%3D" would come back as "=". Escape it.
            b'%' => out.push_str("%25"),
            b'!' | b'#'..=b'+' | b'-'..=b':' | b'<'..=b'[' | b']'..=b'~' => out.push(b as char),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Decode a percent-encoded cookie value. Invalid escapes are left as-is: a
/// cookie is not a security boundary the way a path is, and dropping the value
/// entirely would be worse for the caller than returning it unchanged.
pub(crate) fn decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Find `name` in a `Cookie:` header value.
pub(crate) fn find<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then_some(v.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_separators() {
        assert_eq!(encode("a;b"), "a%3Bb");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("plain"), "plain");
    }

    #[test]
    fn round_trips() {
        for v in ["a;b c", "100%", "a%3Db", "plain", "", "ü"] {
            assert_eq!(decode(&encode(v)), v, "round trip failed for {v:?}");
        }
    }

    #[test]
    fn finds_a_named_pair() {
        assert_eq!(find("a=1; b=2", "b"), Some("2"));
        assert_eq!(find("a=1", "missing"), None);
    }
}
