//! Security response headers, applied to every response by default.
//!
//! Most frameworks leave these to the application, which means most
//! applications ship without them. Churust sends a conservative set unless told
//! otherwise, and always yields to a handler that set the header itself.
//!
//! ```
//! use churust_core::{Churust, SecurityHeaders};
//!
//! # fn build() {
//! // Customise:
//! Churust::server().security_headers(
//!     SecurityHeaders::new().referrer_policy(Some("strict-origin-when-cross-origin")),
//! );
//!
//! // Or opt out entirely:
//! Churust::server().without_security_headers();
//! # }
//! ```

use crate::call::Call;
use crate::pipeline::{Middleware, Next};
use crate::response::Response;
use async_trait::async_trait;
use http::header::{
    HeaderName, CONTENT_SECURITY_POLICY, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use http::HeaderValue;

/// Which security headers to add, and with what values.
///
/// `None` for any field disables that header. Defaults:
///
/// | Header | Default |
/// | --- | --- |
/// | `X-Content-Type-Options` | `nosniff` |
/// | `X-Frame-Options` | `DENY` |
/// | `Referrer-Policy` | `no-referrer` |
/// | `Strict-Transport-Security` | `max-age=31536000`, **only when TLS is configured** |
/// | `Content-Security-Policy` | off |
///
/// There is no default Content-Security-Policy on purpose: a useful one is
/// application-specific, and a generic one either breaks pages or is so
/// permissive that it implies protection it does not give.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    content_type_options: Option<String>,
    frame_options: Option<String>,
    referrer_policy: Option<String>,
    hsts: Option<String>,
    csp: Option<String>,
}

impl Default for SecurityHeaders {
    fn default() -> Self {
        Self {
            content_type_options: Some("nosniff".into()),
            frame_options: Some("DENY".into()),
            referrer_policy: Some("no-referrer".into()),
            hsts: Some("max-age=31536000".into()),
            csp: None,
        }
    }
}

impl SecurityHeaders {
    /// The default set. Equivalent to [`SecurityHeaders::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Set or disable `X-Content-Type-Options`.
    pub fn content_type_options(mut self, v: Option<&str>) -> Self {
        self.content_type_options = v.map(Into::into);
        self
    }

    /// Set or disable `X-Frame-Options`.
    pub fn frame_options(mut self, v: Option<&str>) -> Self {
        self.frame_options = v.map(Into::into);
        self
    }

    /// Set or disable `Referrer-Policy`.
    pub fn referrer_policy(mut self, v: Option<&str>) -> Self {
        self.referrer_policy = v.map(Into::into);
        self
    }

    /// Set or disable `Strict-Transport-Security`.
    ///
    /// Only sent when the server has TLS configured. Announcing HSTS over
    /// plaintext tells a client nothing it can trust, and behind a terminating
    /// proxy it can pin a hostname to HTTPS the origin does not actually serve.
    pub fn hsts(mut self, v: Option<&str>) -> Self {
        self.hsts = v.map(Into::into);
        self
    }

    /// Set a `Content-Security-Policy`. Off by default.
    pub fn content_security_policy(mut self, v: Option<&str>) -> Self {
        self.csp = v.map(Into::into);
        self
    }

    pub(crate) fn into_middleware(self, tls_enabled: bool) -> SecurityHeadersMiddleware {
        SecurityHeadersMiddleware {
            cfg: self,
            tls_enabled,
        }
    }
}

pub(crate) struct SecurityHeadersMiddleware {
    cfg: SecurityHeaders,
    tls_enabled: bool,
}

impl SecurityHeadersMiddleware {
    fn apply(&self, res: &mut Response) {
        let mut set = |name: HeaderName, value: &Option<String>| {
            let Some(v) = value else { return };
            // The application wins. A handler that set this header did so on
            // purpose, and silently overwriting it would be a trap.
            if res.headers.contains_key(&name) {
                return;
            }
            if let Ok(hv) = HeaderValue::from_str(v) {
                res.headers.insert(name, hv);
            }
        };

        set(X_CONTENT_TYPE_OPTIONS, &self.cfg.content_type_options);
        set(X_FRAME_OPTIONS, &self.cfg.frame_options);
        set(REFERRER_POLICY, &self.cfg.referrer_policy);
        set(CONTENT_SECURITY_POLICY, &self.cfg.csp);
        if self.tls_enabled {
            set(STRICT_TRANSPORT_SECURITY, &self.cfg.hsts);
        }
    }
}

#[async_trait]
impl Middleware for SecurityHeadersMiddleware {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let mut res = next.run(call).await;
        self.apply(&mut res);
        res
    }
}
