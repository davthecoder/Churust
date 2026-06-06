//! Application assembly: the [`Churust`] entry point, the [`AppBuilder`] DSL,
//! the immutable [`App`], the [`Plugin`] trait, and the resolved
//! [`ServerConfig`].

use crate::call::Call;
use crate::pipeline::{Endpoint, Middleware, Next, Phase};
use crate::response::Response;
use crate::router::{Match, RouteBuilder, Router};
use crate::state::StateMap;
use bytes::Bytes;
use futures_util::FutureExt;
use http::header::ALLOW;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use std::collections::VecDeque;
use std::sync::Arc;

/// A reusable bundle of behavior that installs itself into an [`AppBuilder`]
/// at build time — Churust's analogue of Ktor's `install(Plugin)`.
///
/// A plugin typically registers one or more [`Middleware`] (and may add state)
/// inside [`install`](Plugin::install). Pass a plugin to
/// [`AppBuilder::install`]; the builder boxes it and calls `install`.
///
/// ```
/// use churust_core::{App, AppBuilder, Churust, Call, Middleware, Next, Plugin, Response, TestClient};
/// use async_trait::async_trait;
/// use std::sync::Arc;
/// use http::{header::HeaderName, HeaderValue};
///
/// struct Mark;
/// #[async_trait]
/// impl Middleware for Mark {
///     async fn handle(&self, call: Call, next: Next) -> Response {
///         let mut res = next.run(call).await;
///         res.headers.insert(HeaderName::from_static("x-plugin"), HeaderValue::from_static("on"));
///         res
///     }
/// }
///
/// struct MarkPlugin;
/// impl Plugin for MarkPlugin {
///     fn install(self: Box<Self>, app: &mut AppBuilder) {
///         app.add_middleware(Arc::new(Mark));
///     }
/// }
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .install(MarkPlugin)
///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
///     .build();
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.header("x-plugin"), Some("on"));
/// # });
/// ```
pub trait Plugin {
    /// Install this plugin into the builder (register middleware, state, etc.).
    /// Consumes the boxed plugin.
    fn install(self: Box<Self>, app: &mut AppBuilder);
}

/// The server configuration resolved at build time and carried by an [`App`].
///
/// Populated from defaults, an optional [`Config`](crate::Config), and the
/// builder's DSL setters. Read it back from a built app with
/// [`App::config`].
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Bind address host.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// Maximum accepted request body size in bytes; larger bodies get `413`.
    pub max_body_bytes: usize,
    /// Per-request timeout in milliseconds; `0` disables the timeout.
    pub request_timeout_ms: u64,
    /// TLS settings, or `None` for plaintext HTTP.
    pub tls: Option<crate::config::TlsSection>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            max_body_bytes: 1 << 20,
            request_timeout_ms: 30_000,
            tls: None,
        }
    }
}

/// The fluent builder for an application, returned by [`Churust::server`].
///
/// Chain DSL methods to configure the server ([`host`](AppBuilder::host),
/// [`port`](AppBuilder::port), [`tls`](AppBuilder::tls), ...), register shared
/// [`state`](AppBuilder::state), [`install`](AppBuilder::install) plugins, and
/// define routes with [`routing`](AppBuilder::routing). Finish with
/// [`build`](AppBuilder::build) to get an [`App`], or
/// [`start`](AppBuilder::start) to build and serve in one step. DSL setters take
/// precedence over a [`with_config`](AppBuilder::with_config) applied earlier.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .port(3000)
///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
///     .build();
/// assert_eq!(app.config().port, 3000);
/// let res = TestClient::new(app).get("/").send().await;
/// assert_eq!(res.text(), "hi");
/// # });
/// ```
pub struct AppBuilder {
    router: Router,
    middleware: Vec<(Phase, Arc<dyn Middleware>)>,
    config: ServerConfig,
    state: StateMap,
}

impl AppBuilder {
    fn new() -> Self {
        Self {
            router: Router::new(),
            middleware: Vec::new(),
            config: ServerConfig::default(),
            state: StateMap::default(),
        }
    }

    /// Set the bind host (default `"127.0.0.1"`). Returns `self` for chaining.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.config.host = host.into();
        self
    }
    /// Set the bind port (default `8080`). Returns `self` for chaining.
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }
    /// Set the maximum accepted request body size in bytes (default `1 MiB`).
    /// Larger bodies are rejected with `413 Payload Too Large`. Returns `self`
    /// for chaining.
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.config.max_body_bytes = n;
        self
    }

    /// Apply a fully-resolved [`Config`](crate::Config), overwriting the current
    /// server settings.
    ///
    /// This is lowest precedence: DSL setters called *after* it (e.g. another
    /// [`port`](AppBuilder::port)) win. Use it to seed the builder from a config
    /// file/env, then override specific fields in code. Returns `self` for
    /// chaining.
    pub fn with_config(mut self, cfg: crate::config::Config) -> Self {
        self.config.host = cfg.server.host;
        self.config.port = cfg.server.port;
        self.config.max_body_bytes = cfg.server.max_body_bytes;
        self.config.request_timeout_ms = cfg.server.request_timeout_ms;
        self.config.tls = cfg.tls;
        self
    }

    /// Set the per-request timeout in milliseconds (default `30000`). A value of
    /// `0` disables the timeout. Requests exceeding it get `408 Request Timeout`.
    /// Returns `self` for chaining.
    pub fn request_timeout_ms(mut self, ms: u64) -> Self {
        self.config.request_timeout_ms = ms;
        self
    }

    /// Enable TLS, reading the certificate chain from `cert_path` and the
    /// private key from `key_path` (PEM). The files are loaded when the server
    /// starts; this only records the paths. Requires the `tls` feature to have
    /// any effect at serve time. Returns `self` for chaining.
    pub fn tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.config.tls = Some(crate::config::TlsSection {
            cert: cert_path.into(),
            key: key_path.into(),
        });
        self
    }

    /// Register a shared application-state value of type `T`, retrieved later
    /// via the [`State`](crate::State) extractor or
    /// [`Call::state`](crate::Call::state). One value is held per type;
    /// registering another `T` replaces it. Returns `self` for chaining.
    ///
    /// ```
    /// use churust_core::{Churust, State, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #[derive(Clone)]
    /// struct Db { name: &'static str }
    /// let app = Churust::server()
    ///     .state(Db { name: "postgres" })
    ///     .routing(|r| { r.get("/", |db: State<Db>| async move { db.name }); })
    ///     .build();
    /// assert_eq!(TestClient::new(app).get("/").send().await.text(), "postgres");
    /// # });
    /// ```
    pub fn state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state.insert(value);
        self
    }

    /// Install a [`Plugin`], letting it register middleware/state. Consumes the
    /// plugin value. Returns `self` for chaining.
    pub fn install<P: Plugin + 'static>(mut self, plugin: P) -> Self {
        Box::new(plugin).install(&mut self);
        self
    }

    /// Register a [`Middleware`] in a specific [`Phase`]. Plugins use this to
    /// place their middleware precisely; the builder sorts all middleware by
    /// phase (stably) at [`build`](AppBuilder::build) time.
    pub fn add_middleware_in(&mut self, phase: Phase, mw: Arc<dyn Middleware>) {
        self.middleware.push((phase, mw));
    }

    /// Register a [`Middleware`] in the default [`Phase::Plugins`] phase — the
    /// common case for application middleware.
    pub fn add_middleware(&mut self, mw: Arc<dyn Middleware>) {
        self.add_middleware_in(Phase::Plugins, mw);
    }

    /// Define routes via the [`RouteBuilder`] DSL. The closure receives a
    /// mutable builder on which to register handlers and nested scopes. Returns
    /// `self` for chaining.
    ///
    /// ```
    /// use churust_core::{Churust, Call, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "home" });
    ///         r.post("/echo", |mut c: Call| async move { c.receive_text().await.unwrap_or_default() });
    ///     })
    ///     .build();
    /// assert_eq!(TestClient::new(app).get("/").send().await.text(), "home");
    /// # });
    /// ```
    pub fn routing(mut self, f: impl FnOnce(&mut RouteBuilder)) -> Self {
        let mut b = RouteBuilder::new(&mut self.router);
        f(&mut b);
        self
    }

    /// Build the app and serve it until Ctrl-C — a shorthand for
    /// `self.build().start().await`. Binds a socket, so it does not return until
    /// shutdown.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn run() -> std::io::Result<()> {
    /// Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .start()
    ///     .await
    /// # }
    /// ```
    pub async fn start(self) -> std::io::Result<()> {
        self.build().start().await
    }

    /// Finish building into an immutable, cheaply-cloneable [`App`].
    ///
    /// Installed middleware is sorted by [`Phase`] (stably, preserving install
    /// order within a phase) and the configuration is frozen.
    pub fn build(self) -> App {
        let mut mw = self.middleware;
        mw.sort_by_key(|(phase, _)| *phase); // stable: install order preserved within a phase
        let middleware: Vec<Arc<dyn Middleware>> = mw.into_iter().map(|(_, m)| m).collect();
        App {
            inner: Arc::new(AppInner {
                router: self.router,
                middleware,
                config: self.config,
                state: Arc::new(self.state),
            }),
        }
    }
}

struct AppInner {
    router: Router,
    middleware: Vec<Arc<dyn Middleware>>,
    config: ServerConfig,
    state: Arc<StateMap>,
}

/// An assembled, immutable, cheaply-cloneable application.
///
/// Produced by [`AppBuilder::build`]. Internally reference-counted, so cloning
/// is cheap and clones share the same router, middleware, state, and config.
/// Serve it with [`start`](App::start) (or
/// [`start_with_shutdown`](App::start_with_shutdown)), drive a single request
/// in-process with [`process`](App::process), or hand it to a
/// [`TestClient`](crate::TestClient) for testing.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
///     .build();
/// let clone = app.clone(); // cheap; shares the same inner app
/// let res = TestClient::new(clone).get("/").send().await;
/// assert_eq!(res.status().as_u16(), 200);
/// # });
/// ```
#[derive(Clone)]
pub struct App {
    inner: Arc<AppInner>,
}

impl App {
    /// The resolved [`ServerConfig`] this app was built with.
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// The single request entry point: run one request through the full
    /// pipeline (middleware then routed handler) and return the [`Response`].
    ///
    /// Both the hyper engine and the [`TestClient`](crate::TestClient) call
    /// this. It is panic-isolated — a panicking handler is caught and turned
    /// into `500 Internal Server Error` rather than crashing the task.
    ///
    /// ```
    /// use churust_core::{Churust, Call};
    /// use http::{HeaderMap, Method};
    /// use bytes::Bytes;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "ok" }); })
    ///     .build();
    /// let res = app.process(Method::GET, "/".parse().unwrap(), HeaderMap::new(), Bytes::new()).await;
    /// assert_eq!(res.status.as_u16(), 200);
    /// # });
    /// ```
    /// THE single request entry point used by the engine and `TestClient`.
    /// Panic-isolated: a panicking handler yields 500.
    pub async fn process(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        self.process_with_extensions(method, uri, headers, body, http::Extensions::new())
            .await
    }

    /// Like [`App::process`], but seeds the `Call` with pre-built extensions
    /// (e.g. a captured WebSocket upgrade handle). Advanced/engine use.
    pub async fn process_with_extensions(
        &self,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
        extensions: http::Extensions,
    ) -> Response {
        let app = self.clone();
        let fut = async move {
            let mut call = Call::new(method, uri, headers, body);
            call.seed_extensions(extensions);
            call.set_state(app.inner.state.clone());
            app.run_pipeline(call).await
        };
        match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
            Ok(res) => res,
            Err(_) => Response::text("Internal Server Error")
                .with_status(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }

    /// Bind the configured address and serve until Ctrl-C (SIGINT), then drain
    /// in-flight connections gracefully. Does not return until shutdown.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the configured `host:port` is invalid or
    /// the socket cannot be bound (e.g. the port is in use).
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn run() -> std::io::Result<()> {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// app.start().await
    /// # }
    /// ```
    pub async fn start(self) -> std::io::Result<()> {
        let addr = format!("{}:{}", self.inner.config.host, self.inner.config.port)
            .parse::<std::net::SocketAddr>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        crate::engine::serve(self, addr, shutdown).await
    }

    /// Bind and serve until the provided `shutdown` future resolves, then drain
    /// gracefully. Use this to wire a custom shutdown signal (e.g. in tests, or
    /// to combine SIGTERM with SIGINT).
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the configured `host:port` is invalid or
    /// the socket cannot be bound.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// # async fn run() -> std::io::Result<()> {
    /// let app = Churust::server()
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    /// // ... later: tx.send(()) to trigger shutdown ...
    /// app.start_with_shutdown(async { let _ = rx.await; }).await
    /// # }
    /// ```
    pub async fn start_with_shutdown<F>(self, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let addr = format!("{}:{}", self.inner.config.host, self.inner.config.port)
            .parse::<std::net::SocketAddr>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        crate::engine::serve(self, addr, shutdown).await
    }

    async fn run_pipeline(&self, call: Call) -> Response {
        let inner = self.inner.clone();
        let endpoint: Endpoint = Arc::new(move |mut call: Call| {
            let inner = inner.clone();
            Box::pin(async move {
                match inner.router.route(call.method(), call.path()) {
                    Match::Found { handler, params } => {
                        call.set_params(params);
                        handler.handle(call).await
                    }
                    Match::MethodNotAllowed { allow } => {
                        let value = allow
                            .iter()
                            .map(|m| m.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        Response::new(StatusCode::METHOD_NOT_ALLOWED).with_header(
                            ALLOW,
                            HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("")),
                        )
                    }
                    Match::NotFound => {
                        Response::text("Not Found").with_status(StatusCode::NOT_FOUND)
                    }
                }
            }) as _
        });

        let chain: VecDeque<Arc<dyn Middleware>> = self.inner.middleware.iter().cloned().collect();
        Next::new(chain, endpoint).run(call).await
    }
}

/// The framework entry point — a zero-sized namespace for starting an
/// [`AppBuilder`].
///
/// Begin with [`Churust::server`] for an empty builder, or
/// [`Churust::from_config`] to seed it from `churust.toml` plus `CHURUST_*`
/// environment variables.
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
///     .build();
/// assert_eq!(TestClient::new(app).get("/").send().await.text(), "hi");
/// # });
/// ```
pub struct Churust;

impl Churust {
    /// Start a fresh [`AppBuilder`] with default configuration.
    pub fn server() -> AppBuilder {
        AppBuilder::new()
    }

    /// Start an [`AppBuilder`] pre-loaded from `churust.toml` and `CHURUST_*`
    /// environment variables (via [`Config::load_default`](crate::Config::load_default)).
    /// Chain DSL setters afterward to override individual fields.
    ///
    /// ```no_run
    /// use churust_core::{Churust, Call};
    /// // Loads churust.toml + env, then overrides the port in code.
    /// let app = Churust::from_config()
    ///     .port(3000)
    ///     .routing(|r| { r.get("/", |_c: Call| async { "hi" }); })
    ///     .build();
    /// let _ = app;
    /// ```
    pub fn from_config() -> AppBuilder {
        AppBuilder::new().with_config(crate::config::Config::load_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Next;
    use async_trait::async_trait;

    #[tokio::test]
    async fn middleware_runs_in_phase_order() {
        use crate::pipeline::{Next, Phase};
        use async_trait::async_trait;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Recorder {
            log: Arc<Mutex<Vec<&'static str>>>,
            tag: &'static str,
        }
        #[async_trait]
        impl Middleware for Recorder {
            async fn handle(&self, call: Call, next: Next) -> Response {
                self.log.lock().unwrap().push(self.tag);
                next.run(call).await
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let mut builder = Churust::server();
        // Install OUT of phase order; expect execution IN phase order.
        builder.add_middleware_in(
            Phase::Fallback,
            Arc::new(Recorder {
                log: log.clone(),
                tag: "fallback",
            }),
        );
        builder.add_middleware_in(
            Phase::Setup,
            Arc::new(Recorder {
                log: log.clone(),
                tag: "setup",
            }),
        );
        builder.add_middleware_in(
            Phase::Monitoring,
            Arc::new(Recorder {
                log: log.clone(),
                tag: "monitoring",
            }),
        );
        let app = builder
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build();
        let _ = get(&app, "/").await;
        assert_eq!(
            *log.lock().unwrap(),
            vec!["setup", "monitoring", "fallback"]
        );
    }

    fn app() -> App {
        Churust::server()
            .routing(|r| {
                r.get("/", |_c: Call| async { "home" });
                r.get("/boom", |_c: Call| async {
                    panic!("handler exploded");
                    #[allow(unreachable_code)]
                    ""
                });
            })
            .build()
    }

    async fn get(app: &App, path: &str) -> Response {
        app.process(
            Method::GET,
            path.parse::<Uri>().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
    }

    #[tokio::test]
    async fn routes_to_handler() {
        let res = get(&app(), "/").await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, Bytes::from("home"));
    }

    #[tokio::test]
    async fn unknown_path_is_404() {
        let res = get(&app(), "/missing").await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn panicking_handler_yields_500_not_crash() {
        let res = get(&app(), "/boom").await;
        assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn process_with_extensions_seeds_call() {
        #[derive(Clone)]
        struct Marker(u32);
        let app = Churust::server()
            .routing(|r| {
                r.get("/", |c: Call| async move {
                    format!("{}", c.get::<Marker>().map(|m| m.0).unwrap_or(0))
                });
            })
            .build();
        let mut ext = http::Extensions::new();
        ext.insert(Marker(7));
        let res = app
            .process_with_extensions(
                Method::GET,
                "/".parse().unwrap(),
                HeaderMap::new(),
                Bytes::new(),
                ext,
            )
            .await;
        assert_eq!(res.body, Bytes::from("7"));
    }

    #[tokio::test]
    async fn state_extractor_end_to_end() {
        use crate::extract::State;
        #[derive(Clone)]
        struct Counter(u32);

        let app = Churust::server()
            .state(Counter(5))
            .routing(|r| {
                r.get(
                    "/n",
                    |s: State<Counter>| async move { format!("n={}", s.0 .0) },
                );
            })
            .build();
        let res = get(&app, "/n").await;
        assert_eq!(res.body, Bytes::from("n=5"));
    }

    #[tokio::test]
    async fn middleware_installed_via_plugin_runs() {
        struct MarkPlugin;
        struct Mark;
        #[async_trait]
        impl Middleware for Mark {
            async fn handle(&self, call: Call, next: Next) -> Response {
                let mut res = next.run(call).await;
                res.headers.insert(
                    http::header::HeaderName::from_static("x-plugin"),
                    HeaderValue::from_static("on"),
                );
                res
            }
        }
        impl Plugin for MarkPlugin {
            fn install(self: Box<Self>, app: &mut AppBuilder) {
                app.add_middleware(Arc::new(Mark));
            }
        }

        let app = Churust::server()
            .install(MarkPlugin)
            .routing(|r| {
                r.get("/", |_c: Call| async { "ok" });
            })
            .build();
        let res = get(&app, "/").await;
        assert_eq!(res.headers.get("x-plugin").unwrap(), "on");
    }
}
