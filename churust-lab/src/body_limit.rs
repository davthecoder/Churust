//! A body limit written in the type rather than configured beside it.

use async_trait::async_trait;
use churust_core::{Call, FromCall, Result, RouteBodyLimit};

/// Wraps a body extractor with a size limit expressed as a const generic.
///
/// `BodyLimit<Json<Avatar>, { 1 << 20 }>` is a one-megabyte JSON body. The
/// limit is part of the handler's signature, so it is visible at the call site
/// and cannot drift away from the route it protects.
///
/// # Why this is worth trying
///
/// Churust's [`RouteBuilder::max_body_bytes`](churust_core::RouteBuilder::max_body_bytes)
/// already attaches a limit to a route, and it is attached to the builder, so
/// it cannot be registered against the wrong thing. The failure mode this
/// avoids belongs to a *different* design: config resolved at runtime by type
/// lookup, which silently falls back to a default when registered on the wrong
/// scope. A limit that quietly is not applied is worse than no limit, and
/// putting it in the type makes "not applied" unrepresentable.
///
/// What is still unsettled — and why this lives in the lab — is whether the
/// ergonomics earn their place next to the builder method, which reads better
/// and does not push a const generic through every signature.
///
/// # Interaction with the other limits
///
/// This only ever *tightens*. The server-wide `max_body_bytes` is enforced by
/// the engine before any extractor runs, and a per-route limit applies
/// underneath that. Whichever is smallest wins.
///
/// ```
/// use churust_core::{Churust, TestClient};
/// use churust_lab::BodyLimit;
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.post("/note", |BodyLimit(text): BodyLimit<String, 16>| async move {
///             format!("{} bytes", text.len())
///         });
///     })
///     .build();
///
/// let client = TestClient::new(app);
/// assert_eq!(client.post("/note").body("short").send().await.text(), "5 bytes");
/// assert_eq!(
///     client.post("/note").body("far too long to fit in sixteen").send().await.status(),
///     http::StatusCode::PAYLOAD_TOO_LARGE
/// );
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct BodyLimit<T, const LIMIT: usize>(
    /// The extracted value.
    pub T,
);

#[async_trait]
impl<T, const LIMIT: usize> FromCall for BodyLimit<T, LIMIT>
where
    T: FromCall,
{
    async fn from_call(mut call: Call) -> Result<Self> {
        // Seed the same per-route limit the builder sets, rather than measuring
        // here. Measuring would mean collecting the body first — the exact
        // allocation the limit exists to prevent — and it would bypass the
        // streaming path, where the cap must trip at the byte that crosses the
        // line rather than after everything has been read.
        //
        // Tighten only: an outer limit already in force stays in force.
        let effective = match call.get::<RouteBodyLimit>() {
            Some(RouteBodyLimit(outer)) => outer.min(LIMIT),
            None => LIMIT,
        };
        call.insert(RouteBodyLimit(effective));
        T::from_call(call).await.map(BodyLimit)
    }
}
