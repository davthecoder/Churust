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

/// Header names not yet on our MSRV floor of `http::header` constants.
static PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");
static CROSS_ORIGIN_RESOURCE_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-resource-policy");

/// Parse a header value once at configuration time. Invalid tokens become
/// `None` so a bad override cannot panic on every request.
fn header_value(v: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(v).ok()
}

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
///
/// Values are parsed into [`HeaderValue`]s when the config is built, so the
/// per-response path is only a contains-key check and an insert — not
/// re-parsing the same constant strings on every request.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    content_type_options: Option<HeaderValue>,
    frame_options: Option<HeaderValue>,
    referrer_policy: Option<HeaderValue>,
    hsts: Option<HeaderValue>,
    csp: Option<HeaderValue>,
    permissions_policy: Option<HeaderValue>,
    cross_origin_resource_policy: Option<HeaderValue>,
}

impl Default for SecurityHeaders {
    fn default() -> Self {
        Self {
            content_type_options: header_value("nosniff"),
            frame_options: header_value("DENY"),
            referrer_policy: header_value("no-referrer"),
            hsts: header_value("max-age=31536000"),
            csp: None,
            permissions_policy: header_value(
                "camera=(), microphone=(), geolocation=(), payment=(), usb=(), interest-cohort=()",
            ),
            cross_origin_resource_policy: header_value("same-origin"),
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
        self.content_type_options = v.and_then(header_value);
        self
    }

    /// Set or disable `X-Frame-Options`.
    pub fn frame_options(mut self, v: Option<&str>) -> Self {
        self.frame_options = v.and_then(header_value);
        self
    }

    /// Set or disable `Referrer-Policy`.
    pub fn referrer_policy(mut self, v: Option<&str>) -> Self {
        self.referrer_policy = v.and_then(header_value);
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
        self.hsts = v.and_then(header_value);
        self
    }

    /// Set a `Content-Security-Policy`. Off by default.
    pub fn content_security_policy(mut self, v: Option<&str>) -> Self {
        self.csp = v.and_then(header_value);
        self
    }

    /// Set or disable `Permissions-Policy` (formerly Feature-Policy).
    pub fn permissions_policy(mut self, v: Option<&str>) -> Self {
        self.permissions_policy = v.and_then(header_value);
        self
    }

    /// Set or disable `Cross-Origin-Resource-Policy`.
    ///
    /// Default `same-origin`. Use `cross-origin` only when you deliberately
    /// serve assets for no-cors embedding from other sites.
    pub fn cross_origin_resource_policy(mut self, v: Option<&str>) -> Self {
        self.cross_origin_resource_policy = v.and_then(header_value);
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
        // One growth for the whole set, rather than one per header that happens
        // to cross a load-factor boundary. `HeaderMap` rehashes everything it
        // already holds when it grows, and adding six headers to a map holding
        // one made it do that more than once per response — the largest single
        // source of `memmove` on the response path when this was profiled.
        let mut wanted = self.set_count();
        if over_tls && self.hsts.is_some() {
            wanted += 1;
        }
        headers.reserve(wanted);

        let mut set = |name: &'static HeaderName, value: &Option<HeaderValue>| {
            let Some(v) = value else { return };
            // The application wins. A handler that set this header did so on
            // purpose, and silently overwriting it would be a trap.
            //
            // `entry` hashes the name once and both looks the slot up and fills
            // it; the `contains_key` + `insert` pair this replaces hashed the
            // same name twice, and header names are hashed with SipHash.
            headers.entry(name).or_insert_with(|| v.clone());
        };

        set(&X_CONTENT_TYPE_OPTIONS, &self.content_type_options);
        set(&X_FRAME_OPTIONS, &self.frame_options);
        set(&REFERRER_POLICY, &self.referrer_policy);
        set(&CONTENT_SECURITY_POLICY, &self.csp);
        set(&PERMISSIONS_POLICY, &self.permissions_policy);
        set(
            &CROSS_ORIGIN_RESOURCE_POLICY,
            &self.cross_origin_resource_policy,
        );
        if over_tls {
            set(&STRICT_TRANSPORT_SECURITY, &self.hsts);
        }
    }

    /// How many of the non-HSTS headers this configuration actually sends.
    ///
    /// HSTS is excluded because whether it is sent depends on the transport,
    /// which only [`apply_to`](Self::apply_to) knows.
    fn set_count(&self) -> usize {
        [
            self.content_type_options.is_some(),
            self.frame_options.is_some(),
            self.referrer_policy.is_some(),
            self.csp.is_some(),
            self.permissions_policy.is_some(),
            self.cross_origin_resource_policy.is_some(),
        ]
        .iter()
        .filter(|set| **set)
        .count()
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
