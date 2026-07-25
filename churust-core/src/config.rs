//! Layered configuration: defaults < `churust.toml` < env (`CHURUST_*`) < code.

use serde::Deserialize;

/// The fully-resolved application configuration.
///
/// Deserializes from a `churust.toml` file (with a `[server]` table and an
/// optional `[tls]` table) and can be overlaid with `CHURUST_*` environment
/// variables via [`apply_env`](Config::apply_env). Apply it to a builder with
/// [`AppBuilder::with_config`](crate::AppBuilder::with_config), or use
/// [`Churust::from_config`](crate::Churust::from_config) to load and apply the
/// defaults in one step. Unspecified fields fall back to sane defaults.
///
/// ```
/// use churust_core::Config;
///
/// let cfg: Config = toml::from_str(r#"
///     [server]
///     host = "0.0.0.0"
///     port = 9090
/// "#).unwrap();
/// assert_eq!(cfg.server.host, "0.0.0.0");
/// assert_eq!(cfg.server.port, 9090);
/// // unspecified fields keep their defaults
/// assert_eq!(cfg.server.max_body_bytes, 1 << 20);
/// assert!(cfg.tls.is_none());
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Server-level settings (`[server]` table).
    pub server: ServerSection,
    /// Optional TLS settings (`[tls]` table); `None` serves plaintext HTTP.
    pub tls: Option<TlsSection>,
}

/// The `[server]` configuration table.
///
/// Every field has a default (see [`ServerSection::default`]), so a partial
/// `[server]` table is valid.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSection {
    /// Bind address host (default `"127.0.0.1"`).
    pub host: String,
    /// Bind port (default `8080`).
    pub port: u16,
    /// Maximum accepted request body size in bytes (default `1 MiB`). Larger
    /// bodies are rejected with `413 Payload Too Large`.
    pub max_body_bytes: usize,
    /// Per-request timeout in milliseconds (default `30000`). `0` disables the
    /// timeout.
    pub request_timeout_ms: u64,
    /// How long a connection may take to send its complete header block, in
    /// milliseconds (default `10000`). `0` disables it.
    ///
    /// This is the slow-loris defence: without it a client can hold a
    /// connection open indefinitely by dribbling one header byte at a time,
    /// because the per-request timeout does not start until there is a request.
    pub header_read_timeout_ms: u64,
    /// Maximum number of headers accepted on a request (default `100`).
    pub max_headers: usize,
    /// Maximum path segments accepted before a request is rejected with `414`
    /// (default `64`).
    ///
    /// The router walks segments recursively with backtracking, so path depth
    /// is a stack-depth question. hyper bounds the request line, which bounds
    /// this in practice, but the bound should be Churust's own and stated
    /// rather than inherited by accident.
    pub max_path_segments: usize,
    /// Maximum WebSocket frame size in bytes (default `1 MiB`). Requires the
    /// `ws` feature.
    pub ws_max_frame_bytes: usize,
    /// How long an idle connection is kept open for reuse, in milliseconds
    /// (default `75000`). `0` disables keep-alive: answer and close.
    ///
    /// Idle means *no request in flight* — a handler slower than this is busy,
    /// not idle, and its connection is left alone. The timer restarts when a
    /// request finishes.
    pub keep_alive_ms: u64,
    /// Listen backlog: connections the kernel may queue before the accept loop
    /// reaches them (default `1024`).
    pub backlog: u32,
    /// How long graceful shutdown waits for in-flight requests to finish, in
    /// milliseconds (default `30000`). `0` waits indefinitely.
    ///
    /// Without a bound, one slow request delays shutdown forever, which in a
    /// container means being killed rather than exiting cleanly.
    pub shutdown_timeout_ms: u64,
    /// Maximum reassembled WebSocket message size in bytes (default `4 MiB`).
    /// Requires the `ws` feature.
    ///
    /// Separate from the frame cap because a peer can send many small
    /// continuation frames that reassemble into one enormous message.
    pub ws_max_message_bytes: usize,
    /// What to do with a non-canonical path spelling (default `"strict"`).
    ///
    /// One of `"strict"`, `"redirect"` or `"collapse"`. See
    /// [`PathPolicy`](crate::PathPolicy).
    pub path_policy: crate::path::PathPolicy,
    /// Maximum size of a received HTTP/2 header block, in bytes (default
    /// `16384`).
    ///
    /// The HTTP/2 counterpart of [`max_headers`](Self::max_headers), which
    /// configures HTTP/1 only: h2 has no header *count*, it has an encoded
    /// size. Exceeding it is refused at the protocol level.
    pub h2_max_header_list_size: u32,
    /// Maximum concurrent HTTP/2 streams per connection (default `200`). `0`
    /// removes the limit.
    ///
    /// One h2 connection multiplexes many requests, so without this a single
    /// connection is an unbounded amount of concurrent work — the shape behind
    /// the HTTP/2 stream-flood denial-of-service family. hyper's own docs
    /// encourage setting an explicit limit rather than inheriting its default.
    pub h2_max_concurrent_streams: u32,
    /// Maximum simultaneously served connections (default `25000`). `0` means
    /// unlimited.
    ///
    /// The backlog bounds what the kernel queues; this bounds what the process
    /// accepts. Without it, memory and file descriptors are limited only by
    /// what the OS will hand out, and the failure mode is the whole process
    /// dying rather than new connections waiting.
    pub max_connections: usize,
    /// Maximum TLS handshakes in progress at once (default `256`). `0` means
    /// unlimited. Requires the `tls` feature.
    ///
    /// Much smaller than [`max_connections`](Self::max_connections) on purpose:
    /// a handshake is asymmetric work — cheap to ask for, expensive to answer —
    /// so it needs its own, tighter bound.
    pub max_tls_handshakes: usize,
    /// How long a TLS handshake may take before the connection is dropped, in
    /// milliseconds (default `10000`). `0` disables the bound.
    ///
    /// [`header_read_timeout_ms`](Self::header_read_timeout_ms) cannot cover
    /// this: before the handshake completes there is no HTTP layer to time out.
    /// Without it, a client that completes the TCP handshake and then sends one
    /// byte per minute holds a connection open indefinitely.
    pub tls_handshake_timeout_ms: u64,
}

/// The `[tls]` configuration table: paths to a PEM certificate chain and
/// private key.
///
/// Only meaningful when the `tls` feature is enabled; otherwise the paths are
/// ignored. Set it on a builder with [`AppBuilder::tls`](crate::AppBuilder::tls).
#[derive(Debug, Clone, Deserialize)]
pub struct TlsSection {
    /// Path to the PEM-encoded certificate chain file.
    pub cert: String,
    /// Path to the PEM-encoded private key file.
    pub key: String,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            max_body_bytes: 1 << 20,
            request_timeout_ms: 30_000,
            header_read_timeout_ms: 10_000,
            max_headers: 100,
            max_path_segments: 64,
            ws_max_frame_bytes: 1 << 20,
            ws_max_message_bytes: 4 << 20,
            keep_alive_ms: 75_000,
            backlog: 1024,
            shutdown_timeout_ms: 30_000,
            path_policy: crate::path::PathPolicy::Strict,
            h2_max_header_list_size: 16 << 10,
            h2_max_concurrent_streams: 200,
            max_connections: 25_000,
            max_tls_handshakes: 256,
            tls_handshake_timeout_ms: 10_000,
        }
    }
}

impl Config {
    /// Load configuration by layering, in increasing precedence: built-in
    /// defaults, then the TOML file at `path` (if it exists and parses), then
    /// `CHURUST_*` environment variables.
    ///
    /// A missing or unparseable file is treated as "use defaults" rather than an
    /// error, so calling this without a config file present is safe.
    ///
    /// ```no_run
    /// use churust_core::Config;
    /// // Reads ./my-app.toml if present, then applies CHURUST_* env overrides.
    /// let cfg = Config::load("my-app.toml");
    /// let _ = cfg.server.port;
    /// ```
    pub fn load(path: &str) -> Self {
        let mut cfg = match std::fs::read_to_string(path) {
            // A malformed file still falls back to defaults — but says so.
            // Silently substituting defaults discards the operator's host,
            // port, limits and TLS paths over a single typo, and a server that
            // came up on 127.0.0.1:8080 with no TLS because of an unreported
            // parse error is the hardest kind of misconfiguration to find.
            Ok(text) => toml::from_str::<Config>(&text).unwrap_or_else(|e| {
                tracing::warn!(path, error = %e, "config file could not be parsed; using defaults");
                Config::default()
            }),
            // A missing file is the documented, ordinary case: defaults apply.
            Err(_) => Config::default(),
        };
        cfg.apply_env(|k| std::env::var(k).ok());
        cfg
    }

    /// Load configuration from the conventional `churust.toml` path (plus
    /// `CHURUST_*` env overrides). A shorthand for `Config::load("churust.toml")`;
    /// this is what [`Churust::from_config`](crate::Churust::from_config) uses.
    ///
    /// ```no_run
    /// use churust_core::Config;
    /// let cfg = Config::load_default();
    /// let _ = cfg.server.host;
    /// ```
    pub fn load_default() -> Self {
        Self::load("churust.toml")
    }

    /// Overlay `CHURUST_*` environment variables onto this config, using the
    /// provided `get` lookup so the source can be injected in tests.
    ///
    /// Recognized keys: `CHURUST_SERVER_HOST`, `CHURUST_SERVER_PORT`,
    /// `CHURUST_SERVER_MAX_BODY_BYTES`, `CHURUST_SERVER_REQUEST_TIMEOUT_MS`.
    /// Values that fail to parse (e.g. a non-numeric port) are ignored, leaving
    /// the existing value in place.
    ///
    /// ```
    /// use churust_core::Config;
    /// use std::collections::HashMap;
    ///
    /// let env: HashMap<&str, &str> = [("CHURUST_SERVER_PORT", "7000")].into_iter().collect();
    /// let mut cfg = Config::default();
    /// cfg.apply_env(|k| env.get(k).map(|s| s.to_string()));
    /// assert_eq!(cfg.server.port, 7000);
    /// ```
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) {
        if let Some(v) = get("CHURUST_SERVER_HOST") {
            self.server.host = v;
        }
        if let Some(v) = get("CHURUST_SERVER_PORT").and_then(|s| s.parse().ok()) {
            self.server.port = v;
        }
        if let Some(v) = get("CHURUST_SERVER_MAX_BODY_BYTES").and_then(|s| s.parse().ok()) {
            self.server.max_body_bytes = v;
        }
        if let Some(v) = get("CHURUST_SERVER_HEADER_READ_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            self.server.header_read_timeout_ms = v;
        }
        if let Some(v) = get("CHURUST_SERVER_MAX_HEADERS").and_then(|s| s.parse().ok()) {
            self.server.max_headers = v;
        }
        if let Some(v) = get("CHURUST_SERVER_MAX_PATH_SEGMENTS").and_then(|s| s.parse().ok()) {
            self.server.max_path_segments = v;
        }
        if let Some(v) = get("CHURUST_SERVER_WS_MAX_FRAME_BYTES").and_then(|s| s.parse().ok()) {
            self.server.ws_max_frame_bytes = v;
        }
        if let Some(v) = get("CHURUST_SERVER_KEEP_ALIVE_MS").and_then(|s| s.parse().ok()) {
            self.server.keep_alive_ms = v;
        }
        if let Some(v) = get("CHURUST_SERVER_BACKLOG").and_then(|s| s.parse().ok()) {
            self.server.backlog = v;
        }
        if let Some(v) = get("CHURUST_SERVER_SHUTDOWN_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            self.server.shutdown_timeout_ms = v;
        }
        if let Some(v) = get("CHURUST_SERVER_WS_MAX_MESSAGE_BYTES").and_then(|s| s.parse().ok()) {
            self.server.ws_max_message_bytes = v;
        }
        if let Some(v) = get("CHURUST_SERVER_REQUEST_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            self.server.request_timeout_ms = v;
        }
        if let Some(v) = get("CHURUST_SERVER_PATH_POLICY") {
            match v.to_ascii_lowercase().as_str() {
                "strict" => self.server.path_policy = crate::path::PathPolicy::Strict,
                "redirect" => self.server.path_policy = crate::path::PathPolicy::Redirect,
                "collapse" => self.server.path_policy = crate::path::PathPolicy::Collapse,
                // An unrecognised value keeps the safer setting rather than
                // silently loosening it on a typo.
                _ => {}
            }
        }
        if let Some(v) = get("CHURUST_SERVER_H2_MAX_HEADER_LIST_SIZE").and_then(|s| s.parse().ok())
        {
            self.server.h2_max_header_list_size = v;
        }
        if let Some(v) =
            get("CHURUST_SERVER_H2_MAX_CONCURRENT_STREAMS").and_then(|s| s.parse().ok())
        {
            self.server.h2_max_concurrent_streams = v;
        }
        if let Some(v) = get("CHURUST_SERVER_MAX_CONNECTIONS").and_then(|s| s.parse().ok()) {
            self.server.max_connections = v;
        }
        if let Some(v) = get("CHURUST_SERVER_MAX_TLS_HANDSHAKES").and_then(|s| s.parse().ok()) {
            self.server.max_tls_handshakes = v;
        }
        if let Some(v) = get("CHURUST_SERVER_TLS_HANDSHAKE_TIMEOUT_MS").and_then(|s| s.parse().ok())
        {
            self.server.tls_handshake_timeout_ms = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.server.port, 8080);
        assert_eq!(c.server.max_body_bytes, 1 << 20);
        assert!(c.tls.is_none());
    }

    #[test]
    fn parses_toml() {
        let text = r#"
            [server]
            host = "0.0.0.0"
            port = 9090
        "#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.server.port, 9090);
        // unspecified fields fall back to defaults
        assert_eq!(c.server.max_body_bytes, 1 << 20);
    }

    #[test]
    fn env_overrides_file() {
        let mut c = Config::default();
        let env: HashMap<&str, &str> = [("CHURUST_SERVER_PORT", "7000")].into_iter().collect();
        c.apply_env(|k| env.get(k).map(|s| s.to_string()));
        assert_eq!(c.server.port, 7000);
    }
}
