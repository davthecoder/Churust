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
            Ok(text) => toml::from_str::<Config>(&text).unwrap_or_default(),
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
        if let Some(v) = get("CHURUST_SERVER_REQUEST_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            self.server.request_timeout_ms = v;
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
