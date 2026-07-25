//! The trie-based [`Router`], its [`Match`] result, and the [`RouteBuilder`]
//! DSL used inside [`AppBuilder::routing`](crate::AppBuilder::routing).
//!
//! Routes are matched segment by segment against static text, `{param}`
//! captures, and a trailing `{name...}` wildcard. Static segments win over
//! parameters, which win over wildcards.

use crate::handler::{boxed, BoxHandler, IntoHandler};
use http::Method;
use std::collections::HashMap;

/// The outcome of routing a `(method, path)` pair against the [`Router`].
///
/// Returned by [`Router::route`]. The framework translates each variant into a
/// response: [`Found`](Match::Found) runs the handler,
/// [`MethodNotAllowed`](Match::MethodNotAllowed) yields `405` with an `Allow`
/// header, and [`NotFound`](Match::NotFound) yields `404`.
///
/// ```
/// use churust_core::{boxed, Call, IntoHandler, Router, Match};
/// use http::Method;
///
/// let mut router = Router::new();
/// router.add(Method::GET, "/users/{id}", boxed((|_c: Call| async { "ok" }).into_handler()));
///
/// match router.route(&Method::GET, "/users/7") {
///     Match::Found { params, .. } => assert_eq!(params.get("id").unwrap(), "7"),
///     _ => panic!("expected a match"),
/// }
/// assert!(matches!(router.route(&Method::GET, "/nope"), Match::NotFound));
/// assert!(matches!(router.route(&Method::POST, "/users/7"), Match::MethodNotAllowed { .. }));
/// ```
pub enum Match {
    /// A handler matched the path and method. Carries the matched handler and
    /// the captured path parameters (`{name}` -> value).
    Found {
        /// The handler registered for this `(path, method)`.
        handler: BoxHandler,
        /// The captured path parameters, keyed by name.
        params: HashMap<String, String>,
    },
    /// The path matched a route, but not for this method. `allow` lists the
    /// methods that *are* registered (used to build the `Allow` header).
    MethodNotAllowed {
        /// The methods registered for the matched path.
        allow: Vec<Method>,
    },
    /// No route matched the path at all.
    NotFound,
}

#[derive(Default)]
struct Node {
    statics: HashMap<String, Node>,
    param: Option<(String, Box<Node>)>,      // {name}
    wildcard: Option<(String, BoxHandlers)>, // {name...} terminal
    handlers: BoxHandlers,
}

#[derive(Default)]
struct BoxHandlers(HashMap<Method, BoxHandler>);

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node").finish_non_exhaustive()
    }
}

/// A compiled, trie-based router mapping `(method, path)` to a handler.
///
/// Build one with [`Router::new`], register routes with [`Router::add`], and
/// look them up with [`Router::route`]. Inside an application you usually do not
/// touch the `Router` directly — the
/// [`RouteBuilder`] DSL in
/// [`AppBuilder::routing`](crate::AppBuilder::routing) populates it for you.
///
/// Supported pattern syntax (full paths, leading `/`):
/// - static segments: `/users/list`
/// - named parameters: `/users/{id}` (captured as `id`)
/// - trailing wildcard: `/files/{path...}` (captures the remaining path,
///   slashes included; must be the last segment)
///
/// ```
/// use churust_core::{boxed, Call, IntoHandler, Router, Match};
/// use http::Method;
///
/// let mut router = Router::new();
/// router.add(Method::GET, "/files/{path...}", boxed((|_c: Call| async { "" }).into_handler()));
/// match router.route(&Method::GET, "/files/a/b/c.txt") {
///     Match::Found { params, .. } => assert_eq!(params.get("path").unwrap(), "a/b/c.txt"),
///     _ => panic!("expected wildcard match"),
/// }
/// ```
#[derive(Debug, Default)]
pub struct Router {
    root: Node,
}

impl Router {
    /// Create an empty router. Equivalent to [`Router::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `handler` for `method` at `pattern` (the full path, e.g.
    /// `/users/{id}`).
    ///
    /// Registering different methods at the same path is fine; registering the
    /// same `(method, path)` twice replaces the earlier handler.
    ///
    /// # Panics
    ///
    /// Panics if a `{name...}` wildcard segment is not the final segment of the
    /// pattern.
    ///
    /// ```
    /// use churust_core::{boxed, Call, IntoHandler, Router, Match};
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/ping", boxed((|_c: Call| async { "pong" }).into_handler()));
    /// assert!(matches!(router.route(&Method::GET, "/ping"), Match::Found { .. }));
    /// ```
    pub fn add(&mut self, method: Method, pattern: &str, handler: BoxHandler) {
        let mut node = &mut self.root;
        let segments: Vec<&str> = split_segments(pattern);
        for (i, seg) in segments.iter().enumerate() {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix("...}")) {
                // wildcard must be terminal
                assert!(
                    i == segments.len() - 1,
                    "wildcard `{{{name}...}}` must be last segment"
                );
                let entry = node
                    .wildcard
                    .get_or_insert_with(|| (name.to_string(), BoxHandlers::default()));
                entry.1 .0.insert(method, handler);
                return;
            } else if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                let entry = node
                    .param
                    .get_or_insert_with(|| (name.to_string(), Box::new(Node::default())));
                node = entry.1.as_mut();
            } else {
                node = node.statics.entry(seg.to_string()).or_default();
            }
        }
        node.handlers.0.insert(method, handler);
    }

    /// Route `path` for `method`, returning a [`Match`].
    ///
    /// Matching prefers static segments over `{param}` captures, and falls back
    /// to a `{name...}` wildcard at the deepest matchable ancestor. A path that
    /// matches a node with no handler for `method` yields
    /// [`Match::MethodNotAllowed`]; a path that matches no node yields
    /// [`Match::NotFound`].
    ///
    /// ```
    /// use churust_core::{boxed, Call, IntoHandler, Router, Match};
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/", boxed((|_c: Call| async { "home" }).into_handler()));
    /// assert!(matches!(router.route(&Method::GET, "/"), Match::Found { .. }));
    /// assert!(matches!(router.route(&Method::GET, "/missing"), Match::NotFound));
    /// ```
    pub fn route(&self, method: &Method, path: &str) -> Match {
        let segments = split_segments(path);
        let mut params = HashMap::new();

        // 1. Exact walk. Static beats param, unchanged.
        let exact = Self::walk(&self.root, &segments, 0, &mut params);
        if let Some(node) = exact {
            if let Some(h) = node.handlers.0.get(method) {
                return Match::Found {
                    handler: h.clone(),
                    params,
                };
            }
        }
        let exact_allow: Vec<Method> = exact
            .map(|n| n.handlers.0.keys().cloned().collect())
            .unwrap_or_default();

        // 2. Wildcard fallback. Reaching a node that has no handler for this
        //    method is not the end of the search — a trailing `{name...}` at a
        //    shallower depth may still serve the request. Without this, any
        //    static route sharing a wildcard's prefix hides it entirely.
        //
        //    The walk above may have written captures into `params`. They
        //    belong to the branch just abandoned and must not reach the
        //    wildcard handler.
        params.clear();
        match Self::walk_wildcard(&self.root, &segments, 0, method, &mut params) {
            Some(found @ Match::Found { .. }) => found,
            Some(Match::MethodNotAllowed { allow: wild_allow }) => {
                let mut allow = exact_allow;
                for m in wild_allow {
                    if !allow.contains(&m) {
                        allow.push(m);
                    }
                }
                Match::MethodNotAllowed { allow }
            }
            _ if !exact_allow.is_empty() => Match::MethodNotAllowed { allow: exact_allow },
            _ => Match::NotFound,
        }
    }

    /// The methods registered for `path`, including any a trailing wildcard
    /// would serve. Empty when the path matches no route at all.
    ///
    /// Used by the dispatcher to build the `Allow` header for an `OPTIONS`
    /// request that has no handler of its own.
    ///
    /// ```
    /// use churust_core::{boxed, Call, IntoHandler, Router};
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/x", boxed((|_c: Call| async { "" }).into_handler()));
    /// assert_eq!(router.methods_for("/x"), vec![Method::GET]);
    /// assert!(router.methods_for("/nope").is_empty());
    /// ```
    pub fn methods_for(&self, path: &str) -> Vec<Method> {
        let segments = split_segments(path);
        let mut params = HashMap::new();
        let mut out: Vec<Method> = Self::walk(&self.root, &segments, 0, &mut params)
            .map(|n| n.handlers.0.keys().cloned().collect())
            .unwrap_or_default();

        // TRACE is a probe: nothing registers it, so `walk_wildcard` reports
        // MethodNotAllowed carrying the wildcard's full method list.
        params.clear();
        if let Some(Match::MethodNotAllowed { allow }) =
            Self::walk_wildcard(&self.root, &segments, 0, &Method::TRACE, &mut params)
        {
            for m in allow {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }

    fn walk<'a>(
        node: &'a Node,
        segs: &[&str],
        i: usize,
        params: &mut HashMap<String, String>,
    ) -> Option<&'a Node> {
        if i == segs.len() {
            return Some(node);
        }
        let seg = segs[i];
        if let Some(child) = node.statics.get(seg) {
            if let Some(n) = Self::walk(child, segs, i + 1, params) {
                return Some(n);
            }
        }
        if let Some((name, child)) = &node.param {
            params.insert(name.clone(), seg.to_string());
            if let Some(n) = Self::walk(child, segs, i + 1, params) {
                return Some(n);
            }
            params.remove(name);
        }
        None
    }

    fn walk_wildcard(
        node: &Node,
        segs: &[&str],
        i: usize,
        method: &Method,
        params: &mut HashMap<String, String>,
    ) -> Option<Match> {
        if let Some((name, handlers)) = &node.wildcard {
            let rest = segs[i..].join("/");
            params.insert(name.clone(), rest);
            return Some(match handlers.0.get(method) {
                Some(h) => Match::Found {
                    handler: h.clone(),
                    params: std::mem::take(params),
                },
                None => Match::MethodNotAllowed {
                    allow: handlers.0.keys().cloned().collect(),
                },
            });
        }
        if i < segs.len() {
            if let Some(child) = node.statics.get(segs[i]) {
                if let Some(m) = Self::walk_wildcard(child, segs, i + 1, method, params) {
                    return Some(m);
                }
            }
            if let Some((pname, child)) = &node.param {
                params.insert(pname.clone(), segs[i].to_string());
                if let Some(m) = Self::walk_wildcard(child, segs, i + 1, method, params) {
                    return Some(m);
                }
                params.remove(pname);
            }
        }
        None
    }
}

fn split_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// The route-definition DSL handed to the closure in
/// [`AppBuilder::routing`](crate::AppBuilder::routing).
///
/// Register handlers with the per-method helpers ([`get`](RouteBuilder::get),
/// [`post`](RouteBuilder::post), [`put`](RouteBuilder::put),
/// [`delete`](RouteBuilder::delete)) or the generic [`method`](RouteBuilder::method).
/// Group related routes under a common prefix with [`route`](RouteBuilder::route),
/// which nests cleanly. Each handler may be an extractor closure or anything
/// implementing [`Handler`](crate::Handler).
///
/// ```
/// use churust_core::{Churust, Call, TestClient};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let app = Churust::server()
///     .routing(|r| {
///         r.get("/", |_c: Call| async { "home" });
///         r.route("/api", |r| {
///             r.get("/health", |_c: Call| async { "ok" });
///         });
///     })
///     .build();
/// let res = TestClient::new(app).get("/api/health").send().await;
/// assert_eq!(res.text(), "ok");
/// # });
/// ```
pub struct RouteBuilder<'r> {
    router: &'r mut Router,
    prefix: String,
}

impl<'r> RouteBuilder<'r> {
    pub(crate) fn new(router: &'r mut Router) -> Self {
        Self {
            router,
            prefix: String::new(),
        }
    }

    fn full(&self, path: &str) -> String {
        let mut p = self.prefix.clone();
        if !path.starts_with('/') {
            p.push('/');
        }
        p.push_str(path);
        p
    }

    /// Register a handler for `method` at `path`. Accepts both extractor
    /// closures (via the `HandlerFn` family) and anything already implementing
    /// `Handler` — including a pre-boxed `BoxHandler` — through `IntoHandler`.
    pub fn method<Marker, H>(&mut self, method: Method, path: &str, handler: H) -> &mut Self
    where
        H: IntoHandler<Marker>,
    {
        let full = self.full(path);
        self.router
            .add(method, &full, boxed(handler.into_handler()));
        self
    }

    /// Register a `GET` handler at `path`. Returns `&mut Self` for chaining.
    pub fn get<Marker, H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: IntoHandler<Marker>,
    {
        self.method(Method::GET, path, handler)
    }
    /// Register a `POST` handler at `path`. Returns `&mut Self` for chaining.
    pub fn post<Marker, H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: IntoHandler<Marker>,
    {
        self.method(Method::POST, path, handler)
    }
    /// Register a `PUT` handler at `path`. Returns `&mut Self` for chaining.
    pub fn put<Marker, H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: IntoHandler<Marker>,
    {
        self.method(Method::PUT, path, handler)
    }
    /// Register a `DELETE` handler at `path`. Returns `&mut Self` for chaining.
    pub fn delete<Marker, H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: IntoHandler<Marker>,
    {
        self.method(Method::DELETE, path, handler)
    }

    /// Open a nested scope: every route registered inside `f` has `path`
    /// prepended to its pattern. Scopes nest arbitrarily, so prefixes compose.
    /// Returns `&mut Self` for chaining.
    pub fn route(&mut self, path: &str, f: impl FnOnce(&mut RouteBuilder)) -> &mut Self {
        let prefix = self.full(path);
        let mut child = RouteBuilder {
            router: self.router,
            prefix,
        };
        f(&mut child);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::Call;
    use bytes::Bytes;
    use http::{HeaderMap, StatusCode, Uri};

    fn build() -> Router {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/", |_c: Call| async { "root" });
            b.route("/users", |b| {
                b.get("/{id}", |c: Call| async move {
                    format!("user {}", c.param_raw("id").unwrap())
                });
                b.post("/", |_c: Call| async { (StatusCode::CREATED, "created") });
            });
            b.get("/files/{path...}", |c: Call| async move {
                format!("file {}", c.param_raw("path").unwrap())
            });
        }
        r
    }

    fn run(r: &Router, m: Method, path: &str) -> Match {
        r.route(&m, path)
    }

    #[tokio::test]
    async fn matches_static_and_param() {
        let r = build();
        match run(&r, Method::GET, "/users/7") {
            Match::Found { handler, params } => {
                assert_eq!(params.get("id").unwrap(), "7");
                let mut c = Call::new(
                    Method::GET,
                    "/users/7".parse::<Uri>().unwrap(),
                    HeaderMap::new(),
                    Bytes::new(),
                );
                c.set_params(params);
                let res = handler.handle(c).await;
                assert_eq!(res.body, Bytes::from("user 7"));
            }
            _ => panic!("expected Found"),
        }
    }

    fn build_shadowed() -> Router {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/files/{path...}", |c: Call| async move {
                format!("wild:{}", c.param_raw("path").unwrap_or(""))
            });
            b.get("/files/special/x", |_c: Call| async { "static" });
            b.post("/files/only-post", |_c: Call| async { "posted" });
        }
        r
    }

    #[test]
    fn wildcard_is_reachable_through_a_static_sibling() {
        let r = build_shadowed();
        match run(&r, Method::GET, "/files/special") {
            Match::Found { params, .. } => {
                assert_eq!(params.get("path").unwrap(), "special");
            }
            _ => panic!("wildcard should serve /files/special"),
        }
    }

    #[test]
    fn exact_match_still_wins_over_wildcard() {
        let r = build_shadowed();
        match run(&r, Method::GET, "/files/special/x") {
            Match::Found { params, .. } => {
                assert!(
                    !params.contains_key("path"),
                    "the static route captured a wildcard param"
                );
            }
            _ => panic!("expected the static route"),
        }
    }

    #[test]
    fn allow_header_unions_exact_and_wildcard_methods() {
        let r = build_shadowed();
        match run(&r, Method::DELETE, "/files/only-post") {
            Match::MethodNotAllowed { allow } => {
                assert!(allow.contains(&Method::POST), "missing the exact method");
                assert!(allow.contains(&Method::GET), "missing the wildcard method");
            }
            _ => panic!("expected 405"),
        }
    }

    #[test]
    fn abandoned_branch_params_do_not_leak_into_the_wildcard() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            // Matches /u/7/edit structurally, but has no GET handler.
            b.post("/u/{id}/edit", |_c: Call| async { "edit" });
            b.get("/u/{rest...}", |_c: Call| async { "wild" });
        }
        match r.route(&Method::GET, "/u/7/edit") {
            Match::Found { params, .. } => {
                assert_eq!(params.get("rest").unwrap(), "7/edit");
                assert!(
                    !params.contains_key("id"),
                    "stale `id` leaked from the abandoned walk"
                );
            }
            _ => panic!("the wildcard should have matched"),
        }
    }

    #[test]
    fn unknown_path_is_not_found() {
        let r = build();
        assert!(matches!(run(&r, Method::GET, "/nope"), Match::NotFound));
    }

    #[test]
    fn known_path_wrong_method_is_405() {
        let r = build();
        match run(&r, Method::DELETE, "/users/7") {
            Match::MethodNotAllowed { allow } => assert!(allow.contains(&Method::GET)),
            _ => panic!("expected 405"),
        }
    }

    #[test]
    fn wildcard_captures_rest() {
        let r = build();
        match run(&r, Method::GET, "/files/a/b/c.txt") {
            Match::Found { params, .. } => assert_eq!(params.get("path").unwrap(), "a/b/c.txt"),
            _ => panic!("expected wildcard Found"),
        }
    }

    // Regression guard for blocking issue #2: a pre-boxed `BoxHandler` must be
    // acceptable to the route builder methods, not only closures.
    #[tokio::test]
    async fn route_builder_accepts_boxed_handler() {
        let pre: BoxHandler = boxed((|_c: Call| async { "pre-boxed" }).into_handler());
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/pre", pre);
        }
        match run(&r, Method::GET, "/pre") {
            Match::Found { handler, .. } => {
                let c = Call::new(
                    Method::GET,
                    "/pre".parse::<Uri>().unwrap(),
                    HeaderMap::new(),
                    Bytes::new(),
                );
                let res = handler.handle(c).await;
                assert_eq!(res.body, Bytes::from("pre-boxed"));
            }
            _ => panic!("expected Found"),
        }
    }
}
