//! Security response headers, applied to every response by default.
//!
//! Most frameworks leave these to the application, which means most
//! applications ship without them. Churust sends a conservative set unless told
//! otherwise, and always yields to a handler that set the header itself.
//!
//! "Every response" is meant literally, and that takes two mechanisms rather
//! than one. Most responses come out of the pipeline, where
//! `SecurityHeadersMiddleware` decorates them. A few do not: a body refused on
//! its declared `Content-Length` before dispatch, the RFC 9112 §6.3 refusal, a
//! request that outran `request_timeout_ms` so the pipeline never produced
//! anything at all. Those are answered by the transport, and each transport
//! applies the same set to whatever it is about to write — see
//! `App::apply_security_headers`. Applying twice is deliberately harmless,
//! because a header already present is left alone.
//!
//! What is genuinely out of reach is a response Churust never composed: hyper
//! answering `431` for a header block over `max_headers`, or a malformed
//! request line that never became a request. Nothing in this crate sees those.
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
/// | `Permissions-Policy` | sensors and high-risk features locked off (see below) |
/// | `Cross-Origin-Resource-Policy` | `same-origin` |
/// | `Content-Security-Policy` | off |
///
/// There is no default Content-Security-Policy on purpose: a useful one is
/// application-specific, and a generic one either breaks pages or is so
/// permissive that it implies protection it does not give.
///
/// The default `Permissions-Policy` disables camera, microphone, geolocation,
/// payment, USB, and interest-cohort (FLoC) access. JSON APIs never need those
/// browser features, and an HTML app that does can override the header.
///
/// `Cross-Origin-Resource-Policy: same-origin` blocks no-cors cross-origin
/// reads (Spectre-class resource loading). Cross-origin clients that use
/// CORS still work; only opaque cross-origin embedding is refused.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    content_type_options: Option<String>,
    frame_options: Option<String>,
    referrer_policy: Option<String>,
    hsts: Option<String>,
    csp: Option<String>,
    permissions_policy: Option<String>,
    cross_origin_resource_policy: Option<String>,
}

impl Default for SecurityHeaders {
    fn default() -> Self {
        Self {
            content_type_options: Some("nosniff".into()),
            frame_options: Some("DENY".into()),
            referrer_policy: Some("no-referrer".into()),
            hsts: Some("max-age=31536000".into()),
            csp: None,
            permissions_policy: Some(
                "camera=(), microphone=(), geolocation=(), payment=(), usb=(), interest-cohort=()"
                    .into(),
            ),
            cross_origin_resource_policy: Some("same-origin".into()),
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
    /// Only sent when the response is known to be going out over TLS.
    /// Announcing HSTS over plaintext tells a client nothing it can trust, and
    /// behind a terminating proxy it can pin a hostname to HTTPS the origin
    /// does not actually serve.
    ///
    /// Two things establish that knowledge. Over TCP it is
    /// [`AppBuilder::tls`](crate::AppBuilder::tls): this process holding the
    /// certificate is what rules out a plaintext hop in front of it. Over
    /// HTTP/3 nothing needs to be configured, because QUIC has no plaintext
    /// mode — `server_config_from_pem` pins TLS 1.3 — so an h3 response is
    /// encrypted whether or not the builder was ever told about a certificate.
    pub fn hsts(mut self, v: Option<&str>) -> Self {
        self.hsts = v.map(Into::into);
        self
    }

    /// Set a `Content-Security-Policy`. Off by default.
    pub fn content_security_policy(mut self, v: Option<&str>) -> Self {
        self.csp = v.map(Into::into);
        self
    }

    /// Set or disable `Permissions-Policy` (formerly Feature-Policy).
    pub fn permissions_policy(mut self, v: Option<&str>) -> Self {
        self.permissions_policy = v.map(Into::into);
        self
    }

    /// Set or disable `Cross-Origin-Resource-Policy`.
    ///
    /// Default `same-origin`. Use `cross-origin` only when you deliberately
    /// serve assets for no-cors embedding from other sites.
    pub fn cross_origin_resource_policy(mut self, v: Option<&str>) -> Self {
        self.cross_origin_resource_policy = v.map(Into::into);
        self
    }

    /// Fill in whichever of these headers a response does not already carry.
    ///
    /// Split out of the middleware so the transports can reuse it verbatim.
    /// Responses that never reach the pipeline — a body refused on its declared
    /// length, a request that outran its deadline — used to arrive bare because
    /// the only implementation of this lived inside a `Middleware::handle` they
    /// never passed through. One function, called from both places, is what
    /// keeps the two sets from drifting apart.
    ///
    /// `over_tls` answers "is this response definitely encrypted on the way
    /// out", which is the only question [`hsts`](Self::hsts) depends on.
    ///
    /// Idempotent by construction: every header is skipped when already
    /// present, which is the same rule that lets a handler override a default,
    /// so calling it twice on one response cannot change the result.
    pub(crate) fn apply_to(&self, headers: &mut http::HeaderMap, over_tls: bool) {
        let mut set = |name: HeaderName, value: &Option<String>| {
            let Some(v) = value else { return };
            // The application wins. A handler that set this header did so on
            // purpose, and silently overwriting it would be a trap.
            if headers.contains_key(&name) {
                return;
            }
            if let Ok(hv) = HeaderValue::from_str(v) {
                headers.insert(name, hv);
            }
        };

        set(X_CONTENT_TYPE_OPTIONS, &self.content_type_options);
        set(X_FRAME_OPTIONS, &self.frame_options);
        set(REFERRER_POLICY, &self.referrer_policy);
        set(CONTENT_SECURITY_POLICY, &self.csp);
        // Not in `http::header` constants yet on our MSRV floor; static names
        // are stable header tokens.
        set(
            HeaderName::from_static("permissions-policy"),
            &self.permissions_policy,
        );
        set(
            HeaderName::from_static("cross-origin-resource-policy"),
            &self.cross_origin_resource_policy,
        );
        if over_tls {
            set(STRICT_TRANSPORT_SECURITY, &self.hsts);
        }
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

#[async_trait]
impl Middleware for SecurityHeadersMiddleware {
    async fn handle(&self, call: Call, next: Next) -> Response {
        let mut res = next.run(call).await;
        // `tls_enabled` is all the pipeline knows: it runs identically on every
        // transport, so it cannot tell an h3 request from a plaintext one. The
        // transport fills that in afterwards, which is why HSTS over h3 is
        // added by `send_response` rather than here.
        self.cfg.apply_to(&mut res.headers, self.tls_enabled);
        res
    }
}
