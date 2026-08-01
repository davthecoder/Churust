//! The trie-based [`Router`], its [`Match`] result, and the [`RouteBuilder`]
//! DSL used inside [`AppBuilder::routing`](crate::AppBuilder::routing).
//!
//! Routes are matched segment by segment against static text, `{param}`
//! captures, and a trailing `{name...}` wildcard. Static segments win over
//! parameters, which win over wildcards.

use crate::handler::{boxed, BoxHandler, IntoHandler};
use http::Method;
use std::collections::HashMap;

/// Render an `Allow` header value from the methods explicitly registered for a
/// path, adding the ones the dispatcher answers implicitly.
///
/// The single source of truth for `Allow`, whichever response carries it. RFC
/// 9110 §15.5.6 (`405`) and §9.3.7 (`OPTIONS`) describe the same fact about the
/// same resource, and generating it in two places is how they came to disagree:
/// a path with one `GET` route told `OPTIONS` it supported `GET, HEAD, OPTIONS`
/// and simultaneously told a `DELETE` that it supported only `GET`.
///
/// Returns `None` when nothing is registered — an empty `Allow` tells a client
/// nothing, and the right answer there is `404` rather than `405`.
///
/// ```
/// use churust_core::router::allow_header_value;
/// use http::Method;
///
/// // A GET route also answers HEAD and OPTIONS.
/// assert_eq!(allow_header_value(vec![Method::GET]).as_deref(), Some("GET, HEAD, OPTIONS"));
/// // Without a GET there is no HEAD to synthesize.
/// assert_eq!(allow_header_value(vec![Method::POST]).as_deref(), Some("OPTIONS, POST"));
/// assert_eq!(allow_header_value(vec![]), None);
/// ```
pub fn allow_header_value(mut methods: Vec<Method>) -> Option<String> {
    if methods.is_empty() {
        return None;
    }
    // RFC 9110 §9.3.2: HEAD is available wherever GET is, and the dispatcher
    // synthesizes it. Advertising it where there is no GET would be a promise
    // the server cannot keep.
    if methods.contains(&Method::GET) && !methods.contains(&Method::HEAD) {
        methods.push(Method::HEAD);
    }
    // The dispatcher answers OPTIONS for any path that has any route.
    if !methods.contains(&Method::OPTIONS) {
        methods.push(Method::OPTIONS);
    }
    // Sorted so the header is deterministic: two responses describing one
    // resource should be byte-identical, and tests can compare them directly.
    methods.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    methods.dedup();
    Some(
        methods
            .iter()
            .map(Method::as_str)
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The outcome of routing a `(method, path)` pair against the [`Router`].
///
/// Returned by [`Router::route`]. The framework translates each variant into a
/// response: [`Found`](Match::Found) runs the handler,
/// [`MethodNotAllowed`](Match::MethodNotAllowed) yields `405` with an `Allow`
/// header, and [`NotFound`](Match::NotFound) yields `404`.
///
/// ```
/// use churust_core::{boxed, Call, IntoHandler, Router, Match};
/// # use bytes::Bytes;
/// # use http::HeaderMap;
/// # fn probe(p: &str) -> Call {
/// #     Call::new(Method::GET, p.parse().unwrap(), HeaderMap::new(), Bytes::new())
/// # }
/// use http::Method;
///
/// let mut router = Router::new();
/// router.add(Method::GET, "/users/{id}", boxed((|_c: Call| async { "ok" }).into_handler()));
///
/// match router.route(&Method::GET, "/users/7", &probe("/users/7")) {
///     Match::Found { params, .. } => assert_eq!(params.get("id").unwrap(), "7"),
///     _ => panic!("expected a match"),
/// }
/// assert!(matches!(router.route(&Method::GET, "/nope", &probe("/nope")), Match::NotFound));
/// assert!(matches!(router.route(&Method::POST, "/users/7", &probe("/users/7")), Match::MethodNotAllowed { .. }));
/// ```
///
/// Marked `#[non_exhaustive]`: a `match` on this enum outside `churust-core`
/// must carry a `_` arm, so that a future routing outcome is not a breaking
/// change.
#[non_exhaustive]
pub enum Match {
    /// A handler matched the path and method. Carries the matched handler and
    /// the captured path parameters (`{name}` -> value).
    Found {
        /// The handler registered for this `(path, method)`.
        handler: BoxHandler,
        /// The captured path parameters, keyed by name.
        params: crate::call::Params,
    },
    /// The path matched a route, but not for this method. `allow` lists the
    /// methods that *are* registered (used to build the `Allow` header).
    MethodNotAllowed {
        /// The methods registered for the matched path.
        allow: Vec<Method>,
    },
    /// No route matched the path at all.
    NotFound,
    /// A path segment could not be percent-decoded — a malformed escape or
    /// bytes that are not valid UTF-8. The dispatcher turns this into
    /// `400 Bad Request`.
    BadPath,
}

/// [`Match`], but with the fast path holding a borrowed handler.
///
/// Internal, so it may carry a lifetime where [`Match`] cannot.
pub(crate) enum BorrowedMatch<'a> {
    /// The common case: an exact match, handler borrowed from the router.
    Found {
        handler: &'a BoxHandler,
        params: crate::call::Params,
    },
    /// A wildcard match, which comes back from [`Router::route`] owned.
    OwnedFound {
        handler: BoxHandler,
        params: crate::call::Params,
    },
    /// Anything that is not a match.
    Other(Match),
}

#[derive(Default, Clone)]
struct Node {
    statics: HashMap<String, Node>,
    param: Option<(String, Box<Node>)>,      // {name}
    wildcard: Option<(String, BoxHandlers)>, // {name...} terminal
    handlers: BoxHandlers,
}

/// One registered route: a handler plus the guards that must pass for it to
/// serve. Several may share a method; the first whose guards pass wins.
#[derive(Clone)]
struct Candidate {
    guards: Vec<crate::guard::BoxGuard>,
    handler: BoxHandler,
}

#[derive(Default, Clone)]
struct BoxHandlers(HashMap<Method, Vec<Candidate>>);

impl BoxHandlers {
    /// The first candidate whose guards all pass.
    fn pick(&self, method: &Method, call: &crate::call::Call) -> Option<&BoxHandler> {
        self.0
            .get(method)?
            .iter()
            .find_map(|c| c.guards.iter().all(|g| g.check(call)).then_some(&c.handler))
    }

    /// Methods with at least one registered route, guards ignored. Used for
    /// `Allow` on an `OPTIONS` request, which describes the resource rather
    /// than any particular request.
    fn methods(&self) -> Vec<Method> {
        self.0.keys().cloned().collect()
    }

    /// Methods that could actually serve *this* request.
    ///
    /// A method whose every candidate fails its guards does not count: the
    /// route does not match, so the answer is `404`, not `405`. Reporting it
    /// as allowed would tell a client to retry a request that can never work.
    fn methods_matching(&self, call: &crate::call::Call) -> Vec<Method> {
        self.0
            .iter()
            .filter(|(_, cands)| cands.iter().any(|c| c.guards.iter().all(|g| g.check(call))))
            .map(|(m, _)| m.clone())
            .collect()
    }

    fn push(&mut self, method: Method, pattern: &str, handler: BoxHandler) {
        let entries = self.0.entry(method.clone()).or_default();
        // Only an unguarded duplicate is a mistake: guarded siblings are the
        // whole point of guards.
        if entries.iter().any(|c| c.guards.is_empty()) {
            panic!("duplicate route: {method} {pattern} is already registered");
        }
        entries.push(Candidate {
            guards: Vec::new(),
            handler,
        });
    }
}

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
/// # use bytes::Bytes;
/// # use http::HeaderMap;
/// # fn probe(p: &str) -> Call {
/// #     Call::new(Method::GET, p.parse().unwrap(), HeaderMap::new(), Bytes::new())
/// # }
/// use http::Method;
///
/// let mut router = Router::new();
/// router.add(Method::GET, "/files/{path...}", boxed((|_c: Call| async { "" }).into_handler()));
/// match router.route(&Method::GET, "/files/a/b/c.txt", &probe("/files/a/b/c.txt")) {
///     Match::Found { params, .. } => assert_eq!(params.get("path").unwrap(), "a/b/c.txt"),
///     _ => panic!("expected wildcard match"),
/// }
/// ```
#[derive(Debug, Default, Clone)]
pub struct Router {
    root: Node,
    /// Every `(method, pattern)` registered, in registration order.
    ///
    /// Kept alongside the trie rather than recovered from it: the trie stores
    /// segments, so rebuilding a pattern would have to guess at how a parameter
    /// was spelled, and anything reading this wants the spelling the
    /// application actually wrote.
    inventory: Vec<(Method, String)>,
}

impl Router {
    /// Create an empty router. Equivalent to [`Router::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `handler` for `method` at `pattern` (the full path, e.g.
    /// `/users/{id}`).
    ///
    /// Registering different methods at the same path is fine. Registering the
    /// same `(method, path)` twice does *not* replace the earlier handler: an
    /// unguarded duplicate is a mistake this router refuses to guess about, so
    /// it panics. Several routes may share a `(method, path)` when guards tell
    /// them apart — [`RouteBuilder::guard`] attaches one to the registration
    /// just made — and those resolve first-match-wins in registration order, so
    /// an unguarded route registered last is the fallback.
    ///
    /// # Panics
    ///
    /// Panics if a `{name...}` wildcard segment is not the final segment of the
    /// pattern; if an unguarded route is already registered for this
    /// `(method, path)`; or if a `{name}` at this position was already
    /// registered under a different name, since one trie node holds one
    /// parameter name and the second route's handler would look up a capture
    /// that is not there.
    ///
    /// ```
    /// use churust_core::{boxed, Call, IntoHandler, Router, Match};
    /// # use bytes::Bytes;
    /// # use http::HeaderMap;
    /// # fn probe(p: &str) -> Call {
    /// #     Call::new(Method::GET, p.parse().unwrap(), HeaderMap::new(), Bytes::new())
    /// # }
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/ping", boxed((|_c: Call| async { "pong" }).into_handler()));
    /// assert!(matches!(router.route(&Method::GET, "/ping", &probe("/ping")), Match::Found { .. }));
    /// ```
    pub fn add(&mut self, method: Method, pattern: &str, handler: BoxHandler) {
        // Recorded before the trie insert, which may panic on a duplicate or a
        // misplaced wildcard. A router that panicked is not going to be read.
        self.inventory.push((method.clone(), pattern.to_string()));

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
                entry.1.push(method, pattern, handler);
                return;
            } else if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                let entry = node
                    .param
                    .get_or_insert_with(|| (name.to_string(), Box::new(Node::default())));
                // One node, one parameter name. Registering `/users/{id}` and
                // then `/users/{name}/profile` used to silently reuse `id` for
                // both, so the second route's handler looked up `name` and found
                // nothing — a 400 at request time with no clue why. Failing here
                // matches how this router already treats a misplaced wildcard
                // and a duplicate route.
                assert!(
                    entry.0 == name,
                    "conflicting path parameter names at the same position: \
                     `{{{}}}` and `{{{name}}}` (registering {method} {pattern})",
                    entry.0
                );
                node = entry.1.as_mut();
            } else {
                node = node.statics.entry(seg.to_string()).or_default();
            }
        }
        node.handlers.push(method, pattern, handler);
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
    /// # use bytes::Bytes;
    /// # use http::HeaderMap;
    /// # fn probe(p: &str) -> Call {
    /// #     Call::new(Method::GET, p.parse().unwrap(), HeaderMap::new(), Bytes::new())
    /// # }
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/", boxed((|_c: Call| async { "home" }).into_handler()));
    /// assert!(matches!(router.route(&Method::GET, "/", &probe("/")), Match::Found { .. }));
    /// assert!(matches!(router.route(&Method::GET, "/missing", &probe("/missing")), Match::NotFound));
    /// ```
    pub fn route(&self, method: &Method, path: &str, call: &crate::call::Call) -> Match {
        // Split first, decode second. Decoding before splitting would let %2F
        // manufacture separators and forge extra path segments.
        //
        // `Cow`, and borrowed from `path` in the ordinary case: the previous
        // shape (`Vec<String>` immediately re-borrowed into a second
        // `Vec<&str>`) allocated one `String` per segment plus two `Vec`s on
        // every single request, to end up pointing at bytes the caller already
        // owned. Only a segment that actually contains a `%` allocates now.
        let decoded = match decode_segments(path) {
            Some(d) => d,
            None => return Match::BadPath,
        };
        let segments: Vec<&str> = decoded.iter().map(std::convert::AsRef::as_ref).collect();
        let mut params = crate::call::Params::new();

        // 1. Exact search. Static beats param, and a leaf that cannot serve
        //    this request is passed over rather than settled for.
        //
        // The accepted branch's captures are read off `params` afterwards
        // rather than cloned into the closure. `walk_matching` returns straight
        // up the stack the moment `accept` says yes — every rollback sits on
        // the `None` path — so on a hit `params` already holds exactly the
        // accepted branch's captures and nothing else. Cloning them was one
        // `Vec` plus two `String`s per captured parameter, discarded a line
        // later.
        let found = Self::walk_matching(&self.root, &segments, 0, &mut params, &mut |node, _| {
            node.handlers.pick(method, call).cloned()
        });
        if let Some(handler) = found {
            return Match::Found { handler, params };
        }

        //    Nothing served it. `Allow` then has to describe every leaf that
        //    matched structurally, not just the first — they are all the same
        //    resource as far as the client is concerned.
        let mut exact_allow: Vec<Method> = Vec::new();
        params.clear();
        Self::walk_matching(&self.root, &segments, 0, &mut params, &mut |node,
                                                                         _|
         -> Option<
            (),
        > {
            for m in node.handlers.methods_matching(call) {
                if !exact_allow.contains(&m) {
                    exact_allow.push(m);
                }
            }
            // Never accept: visiting every matching leaf is the point.
            None
        });

        // 2. Wildcard fallback. Reaching a node that has no handler for this
        //    method is not the end of the search — a trailing `{name...}` at a
        //    shallower depth may still serve the request. Without this, any
        //    static route sharing a wildcard's prefix hides it entirely.
        //
        //    The walk above may have written captures into `params`. They
        //    belong to the branch just abandoned and must not reach the
        //    wildcard handler.
        params.clear();
        match Self::walk_wildcard(&self.root, &segments, 0, method, call, &mut params) {
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

    /// Route `path` for `method`, borrowing the handler rather than cloning it.
    ///
    /// The dispatcher's path. [`route`](Router::route) hands back a
    /// `BoxHandler`, and `BoxHandler` is an `Arc`: cloning one is an atomic
    /// read-modify-write on the handler's refcount, and every core serving that
    /// route hits the same cache line on every request. [`Handler::handle`]
    /// takes `&self` and returns an owned `'static` future, so the clone buys
    /// nothing — the borrow is over before there is anything to await.
    ///
    /// `route` keeps returning an owned handler: it is public API, and giving
    /// [`Match`] a lifetime would be a breaking change for a saving only the
    /// dispatcher is in a position to take.
    pub(crate) fn route_borrowed(
        &self,
        method: &Method,
        path: &str,
        call: &crate::call::Call,
    ) -> BorrowedMatch<'_> {
        let decoded = match decode_segments(path) {
            Some(d) => d,
            None => return BorrowedMatch::Other(Match::BadPath),
        };
        let segments: Vec<&str> = decoded.iter().map(std::convert::AsRef::as_ref).collect();
        let mut params = crate::call::Params::new();

        let found = Self::walk_matching(&self.root, &segments, 0, &mut params, &mut |node, _| {
            node.handlers.pick(method, call)
        });
        if let Some(handler) = found {
            return BorrowedMatch::Found { handler, params };
        }

        // Nothing served it, so this is one of the slow paths — a 404, a 405 or
        // a wildcard. Those are not what this function exists to make fast, so
        // they go back through `route`, which is the one implementation of what
        // each of them means.
        match self.route(method, path, call) {
            Match::Found { .. } => {
                // Only reachable through the wildcard fallback, which
                // `route_borrowed` deliberately does not duplicate.
                match self.route(method, path, call) {
                    Match::Found { handler, params } => {
                        BorrowedMatch::OwnedFound { handler, params }
                    }
                    other => BorrowedMatch::Other(other),
                }
            }
            other => BorrowedMatch::Other(other),
        }
    }

    /// Attach a guard to the most recently registered route at
    /// `(method, pattern)`.
    ///
    /// Used by [`RouteBuilder::guard`]; registration order is what makes "most
    /// recent" unambiguous.
    fn attach_guard(&mut self, method: &Method, pattern: &str, g: crate::guard::BoxGuard) {
        let mut node = &mut self.root;
        let segments: Vec<&str> = split_segments(pattern);
        for (i, seg) in segments.iter().enumerate() {
            if let Some(_name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix("...}")) {
                debug_assert!(i == segments.len() - 1);
                if let Some(entry) = node.wildcard.as_mut() {
                    if let Some(c) = entry.1 .0.get_mut(method).and_then(|v| v.last_mut()) {
                        c.guards.push(g);
                    }
                }
                return;
            } else if seg.starts_with('{') && seg.ends_with('}') {
                match node.param.as_mut() {
                    Some(entry) => node = entry.1.as_mut(),
                    None => return,
                }
            } else {
                match node.statics.get_mut(*seg) {
                    Some(child) => node = child,
                    None => return,
                }
            }
        }
        if let Some(c) = node.handlers.0.get_mut(method).and_then(|v| v.last_mut()) {
            c.guards.push(g);
        }
    }

    /// Wrap the most recently registered route at `(method, pattern)` so it
    /// carries a per-route body limit.
    fn attach_limit(&mut self, method: &Method, pattern: &str, max_body_bytes: usize) {
        let mut node = &mut self.root;
        let segments: Vec<&str> = split_segments(pattern);
        for (i, seg) in segments.iter().enumerate() {
            if seg
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix("...}"))
                .is_some()
            {
                debug_assert!(i == segments.len() - 1);
                if let Some(entry) = node.wildcard.as_mut() {
                    if let Some(c) = entry.1 .0.get_mut(method).and_then(|v| v.last_mut()) {
                        let inner = c.handler.clone();
                        c.handler = std::sync::Arc::new(LimitedHandler {
                            max_body_bytes,
                            inner,
                        });
                    }
                }
                return;
            } else if seg.starts_with('{') && seg.ends_with('}') {
                match node.param.as_mut() {
                    Some(entry) => node = entry.1.as_mut(),
                    None => return,
                }
            } else {
                match node.statics.get_mut(*seg) {
                    Some(child) => node = child,
                    None => return,
                }
            }
        }
        if let Some(c) = node.handlers.0.get_mut(method).and_then(|v| v.last_mut()) {
            let inner = c.handler.clone();
            c.handler = std::sync::Arc::new(LimitedHandler {
                max_body_bytes,
                inner,
            });
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
        let decoded = match decode_segments(path) {
            Some(d) => d,
            None => return Vec::new(),
        };
        let segments: Vec<&str> = decoded.iter().map(std::convert::AsRef::as_ref).collect();
        let mut params = crate::call::Params::new();
        // Every structurally-matching leaf, not just the first: `OPTIONS`
        // describes the resource, and two routes of different shapes matching
        // one path are one resource to the client.
        let mut out: Vec<Method> = Vec::new();
        Self::walk_matching(&self.root, &segments, 0, &mut params, &mut |node,
                                                                         _|
         -> Option<
            (),
        > {
            for m in node.handlers.methods() {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
            None
        });

        // The wildcard branch is enumerated directly, the same guard-free way
        // the exact branch above is. It used to be driven by `walk_wildcard`
        // with a synthetic `TRACE` call standing in for a real one, on the
        // reasoning that nothing registers `TRACE`, so the walk would report
        // `MethodNotAllowed` carrying the wildcard's method list. Both halves of
        // that were wrong. `walk_wildcard` resolves guards against the call it
        // is given, and a synthetic call has no headers and no authority, so it
        // fails every guard there is: one `guard::host` on a `{p...}` route left
        // `methods_for` empty, the dispatcher skipped its `204` arm, and the
        // request fell through to the `405` path — where the real call *does*
        // pass the guard, so the reply advertised `Allow: GET, HEAD, OPTIONS`
        // while refusing the `OPTIONS` it had just named. It also assumed no
        // application registers `TRACE`; one that does turned the probe into a
        // `Found`, which the `if let` discarded, losing the list entirely.
        Self::collect_wildcard_methods(&self.root, &segments, 0, &mut out);
        out
    }

    /// Union the methods of every wildcard a path could reach, guards ignored.
    ///
    /// Mirrors `walk_wildcard`'s structural descent — statics and params first,
    /// this node's own wildcard last — but unions instead of choosing, because
    /// `Allow` describes a resource rather than the fate of one request:
    /// `walk_wildcard` stops at the first wildcard that can serve, while every
    /// wildcard reachable along the path is part of what this URL supports, and
    /// that is exactly what `walk_wildcard` already unions into `Allow` when
    /// none of them can serve.
    fn collect_wildcard_methods(node: &Node, segs: &[&str], i: usize, out: &mut Vec<Method>) {
        if i < segs.len() {
            if let Some(child) = node.statics.get(segs[i]) {
                Self::collect_wildcard_methods(child, segs, i + 1, out);
            }
            if let Some((_, child)) = &node.param {
                Self::collect_wildcard_methods(child, segs, i + 1, out);
            }
        }
        if let Some((_, handlers)) = &node.wildcard {
            // The same refusal `walk_wildcard` applies: a tail whose decoded
            // segments contain a separator is not a request this wildcard will
            // ever serve, so advertising its methods would promise a `200` that
            // routing has already decided against.
            if segs[i..]
                .iter()
                .any(|s| s.contains('/') || s.contains('\\'))
            {
                return;
            }
            for m in handlers.methods() {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
    }

    /// Every method registered anywhere in this router, deduplicated.
    ///
    /// Answers `OPTIONS *`, which RFC 9110 §9.3.7 defines as a question about
    /// the server rather than about any one resource.
    ///
    /// ```
    /// use churust_core::{boxed, Call, IntoHandler, Router};
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/a", boxed((|_c: Call| async { "" }).into_handler()));
    /// router.add(Method::POST, "/b", boxed((|_c: Call| async { "" }).into_handler()));
    /// let mut all = router.all_methods();
    /// all.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    /// assert_eq!(all, vec![Method::GET, Method::POST]);
    /// ```
    pub fn all_methods(&self) -> Vec<Method> {
        let mut out = Vec::new();
        Self::collect_methods(&self.root, &mut out);
        out
    }

    /// Every registered route as `(method, pattern)`, in registration order.
    ///
    /// The patterns are exactly what was registered, `{id}` and `{path...}`
    /// included, which is what makes this usable as the inventory an API
    /// description is generated from. Routes that share a `(method, path)`
    /// behind different guards appear once per registration, because each is a
    /// separate route even though a client sees one URL.
    ///
    /// ```
    /// use churust_core::{boxed, Call, IntoHandler, Router};
    /// use http::Method;
    ///
    /// let mut router = Router::new();
    /// router.add(Method::GET, "/users/{id}", boxed((|_c: Call| async { "" }).into_handler()));
    /// assert_eq!(router.routes(), &[(Method::GET, "/users/{id}".to_string())]);
    /// ```
    pub fn routes(&self) -> &[(Method, String)] {
        &self.inventory
    }

    fn collect_methods(node: &Node, out: &mut Vec<Method>) {
        for m in node.handlers.methods() {
            if !out.contains(&m) {
                out.push(m);
            }
        }
        if let Some((_, handlers)) = &node.wildcard {
            for m in handlers.methods() {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        for child in node.statics.values() {
            Self::collect_methods(child, out);
        }
        if let Some((_, child)) = &node.param {
            Self::collect_methods(child, out);
        }
    }

    /// Visit every leaf whose *shape* matches, in precedence order, and return
    /// the first result `accept` produces.
    ///
    /// The visitor is what makes this a search rather than a lookup. Returning
    /// the first structurally-matching leaf regardless of whether it could
    /// serve meant a second registered route that *did* match was unreachable:
    /// with `POST /a/{x}` and `GET /{y}/b` registered, `GET /a/b` committed to
    /// the first and answered `405`. A failing guard had the same effect, so a
    /// guard on one route silently disabled an unrelated one.
    ///
    /// Static still beats param at each step — that is a preference, and it
    /// survives; it just no longer ends the search. `params` is restored on the
    /// way out of a branch that is abandoned, so captures never leak from a
    /// route that did not serve into one that did.
    fn walk_matching<'a, T>(
        node: &'a Node,
        segs: &[&str],
        i: usize,
        params: &mut crate::call::Params,
        accept: &mut impl FnMut(&'a Node, &crate::call::Params) -> Option<T>,
    ) -> Option<T> {
        if i == segs.len() {
            return accept(node, params);
        }
        let seg = segs[i];
        if let Some(child) = node.statics.get(seg) {
            if let Some(found) = Self::walk_matching(child, segs, i + 1, params, accept) {
                return Some(found);
            }
        }
        if let Some((name, child)) = &node.param {
            // Remember any value this name already held: a repeated name at
            // different depths must not be clobbered by an abandoned branch.
            let previous = params.get(name).map(str::to_string);
            params.insert(name.clone(), seg.to_string());
            if let Some(found) = Self::walk_matching(child, segs, i + 1, params, accept) {
                return Some(found);
            }
            match previous {
                Some(v) => params.insert(name.clone(), v),
                None => params.remove(name),
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_wildcard(
        node: &Node,
        segs: &[&str],
        i: usize,
        method: &Method,
        call: &crate::call::Call,
        params: &mut crate::call::Params,
    ) -> Option<Match> {
        // Descend before considering this node's own wildcard, so the *deepest*
        // wildcard wins. Consulting it first made a shallow `{p...}` shadow every
        // deeper one: with `/files/{p...}` and `/files/img/{q...}` registered,
        // nothing under `/files/img/` could ever reach the second handler, and
        // nothing reported the shadowing at registration or at match time.
        //
        // A `MethodNotAllowed` found deeper is remembered rather than returned:
        // a shallower wildcard may still have a real handler for this method,
        // and a `405` must not pre-empt a `200`.
        let mut method_mismatch: Option<Match> = None;
        if i < segs.len() {
            if let Some(child) = node.statics.get(segs[i]) {
                match Self::walk_wildcard(child, segs, i + 1, method, call, params) {
                    Some(found @ Match::Found { .. }) => return Some(found),
                    Some(other) => method_mismatch = Some(other),
                    None => {}
                }
            }
            if let Some((pname, child)) = &node.param {
                params.insert(pname.clone(), segs[i].to_string());
                match Self::walk_wildcard(child, segs, i + 1, method, call, params) {
                    Some(found @ Match::Found { .. }) => return Some(found),
                    Some(other) => {
                        // The capture belongs to a branch we are abandoning.
                        params.remove(pname);
                        method_mismatch = Some(other);
                    }
                    None => params.remove(pname),
                }
            }
        }

        if let Some((name, handlers)) = &node.wildcard {
            // Refuse a tail whose segments decoded to something containing a
            // separator. Segments are split before decoding precisely so `%2F`
            // cannot manufacture one — rejoining the decoded pieces with `/`
            // handed that back, making `/files/a%2Fb/c` indistinguishable from
            // `/files/a/b/c` for every consumer of the capture. `StaticFiles`
            // has its own guard; nothing else did.
            //
            // This `BadPath` refuses the wildcard, not the request. `route`
            // deliberately does not forward it as the `400` the variant is
            // named for: the tail being unusable *here* says nothing about the
            // rest of the trie, and a sibling `{n}` may match the same path
            // cleanly with the separator inside one capture — which is the
            // whole point of splitting before decoding. Promoting this into a
            // `400` would overrule that sibling and report a malformed request
            // where there is none. The wildcard simply stops matching, so the
            // request lands on whatever else does, or on `404`.
            if segs[i..]
                .iter()
                .any(|s| s.contains('/') || s.contains('\\'))
            {
                return Some(Match::BadPath);
            }
            let rest = segs[i..].join("/");
            params.insert(name.clone(), rest);
            return Some(match handlers.pick(method, call) {
                Some(h) => Match::Found {
                    handler: h.clone(),
                    params: std::mem::take(params),
                },
                None => {
                    params.remove(name);
                    let mut allow = handlers.methods_matching(call);
                    // Both depths reject the method, so `Allow` must describe
                    // every method either of them would have served.
                    if let Some(Match::MethodNotAllowed { allow: deeper }) = method_mismatch {
                        for m in deeper {
                            if !allow.contains(&m) {
                                allow.push(m);
                            }
                        }
                    }
                    Match::MethodNotAllowed { allow }
                }
            });
        }
        method_mismatch
    }
}

/// Seeds a per-route body limit into the call before delegating.
///
/// The engine's global cap still bounds what is read off the socket; this is
/// the per-route tightening that extractors consult, which is the same place
/// per-route extractor configuration belongs.
struct LimitedHandler {
    max_body_bytes: usize,
    inner: BoxHandler,
}

impl crate::handler::Handler for LimitedHandler {
    fn handle(&self, mut call: crate::call::Call) -> crate::handler::HandlerFuture {
        call.insert(crate::extract::RouteBodyLimit(self.max_body_bytes));
        let inner = self.inner.clone();
        Box::pin(async move { inner.handle(call).await })
    }
}

/// A handler with a scope's middleware chain wrapped around it.
///
/// Built once at route registration rather than consulted per request, so a
/// route in no scope is exactly as cheap as before.
struct ScopedHandler {
    /// The scope's layers, and the terminal that runs the route itself. Both
    /// are assembled at registration: per request this handler clones two
    /// `Arc`s, where it used to build a fresh closure, a fresh boxed future and
    /// a fresh `VecDeque` every time it ran.
    chain: std::sync::Arc<[std::sync::Arc<dyn crate::pipeline::Middleware>]>,
    endpoint: crate::pipeline::Endpoint,
}

impl ScopedHandler {
    fn new(chain: Vec<std::sync::Arc<dyn crate::pipeline::Middleware>>, inner: BoxHandler) -> Self {
        let endpoint: crate::pipeline::Endpoint = std::sync::Arc::new(move |call| {
            let inner = inner.clone();
            Box::pin(async move { inner.handle(call).await }) as _
        });
        Self {
            chain: chain.into(),
            endpoint,
        }
    }
}

impl crate::handler::Handler for ScopedHandler {
    fn handle(&self, call: crate::call::Call) -> crate::handler::HandlerFuture {
        let next = crate::pipeline::Next::with_endpoint(self.chain.clone(), self.endpoint.clone());
        Box::pin(next.run(call))
    }
}

fn split_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Split `path` on `/` and percent-decode each segment individually.
///
/// The order is the point. Decoding the whole path first would turn `%2F` into
/// a separator and let a request forge segments it was never routed through.
/// Returns `None` if any segment is undecodable, which becomes `400`.
///
/// Splits lazily rather than through [`split_segments`]: that helper collects
/// into a `Vec<&str>` that this function immediately consumed and dropped, so
/// going straight from the iterator to the decoded vector is one heap
/// allocation fewer on every request.
fn decode_segments(path: &str) -> Option<Vec<std::borrow::Cow<'_, str>>> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(crate::path::decode_path_segment)
        .collect()
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
    /// Middleware applied to routes registered in this scope, outermost first.
    /// Nested scopes inherit a clone, so a child cannot affect its parent.
    chain: Vec<std::sync::Arc<dyn crate::pipeline::Middleware>>,
    /// The most recent `(method, full path)` registered here, so `guard` knows
    /// which route it modifies.
    last: Option<(Method, String)>,
}

impl<'r> RouteBuilder<'r> {
    pub(crate) fn new(router: &'r mut Router) -> Self {
        Self {
            router,
            prefix: String::new(),
            chain: Vec::new(),
            last: None,
        }
    }

    /// Apply `mw` to every route registered in this scope **after** this call,
    /// including those in nested scopes.
    ///
    /// This is v1 design §6's scope-local interception, and the equivalent of
    /// scope-local interception. Ordering is deliberate: a route registered before
    /// the `intercept` call is not covered by it, so the reading order of the
    /// block matches the behaviour.
    ///
    /// # Ordering, and why there is no `Phase` here
    ///
    /// A scope chain runs in registration order, outermost first. It is not
    /// sorted by [`Phase`](crate::pipeline::Phase), and takes a bare
    /// [`Middleware`](crate::pipeline::Middleware) rather than a
    /// [`Plugin`](crate::Plugin) for that reason: `Phase` orders the one chain
    /// every request goes through, and answers a question — does this run
    /// outside logging, inside the router — that a scope has already answered by
    /// being a scope. Registration order is the whole of the ordering here, so
    /// the block reads top to bottom as it runs.
    ///
    /// Install through [`AppBuilder::install`](crate::AppBuilder::install) or
    /// [`add_middleware_in`](crate::AppBuilder::add_middleware_in) when a
    /// specific phase is what you want; that chain is phase-sorted, stably, so
    /// install order still decides within a phase.
    ///
    /// ```
    /// use churust_core::{Call, Churust, Middleware, Next, Response, TestClient};
    /// use async_trait::async_trait;
    /// use http::StatusCode;
    ///
    /// struct RequireKey;
    /// #[async_trait]
    /// impl Middleware for RequireKey {
    ///     async fn handle(&self, call: Call, next: Next) -> Response {
    ///         if call.header("x-key").is_some() {
    ///             next.run(call).await
    ///         } else {
    ///             Response::new(StatusCode::UNAUTHORIZED)
    ///         }
    ///     }
    /// }
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.get("/open", |_c: Call| async { "open" });
    ///         r.route("/admin", |r| {
    ///             r.intercept(RequireKey);
    ///             r.get("/panel", |_c: Call| async { "panel" });
    ///         });
    ///     })
    ///     .build();
    ///
    /// let client = TestClient::new(app);
    /// assert_eq!(client.get("/open").send().await.status(), StatusCode::OK);
    /// assert_eq!(
    ///     client.get("/admin/panel").send().await.status(),
    ///     StatusCode::UNAUTHORIZED
    /// );
    /// # });
    /// ```
    pub fn intercept<M: crate::pipeline::Middleware>(&mut self, mw: M) -> &mut Self {
        self.chain.push(std::sync::Arc::new(mw));
        self
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
        let h = boxed(handler.into_handler());
        // Wrap once, at registration: the scope chain is fixed by then, so the
        // request path pays nothing for scopes it is not in.
        let h = if self.chain.is_empty() {
            h
        } else {
            std::sync::Arc::new(ScopedHandler::new(self.chain.clone(), h)) as BoxHandler
        };
        self.router.add(method.clone(), &full, h);
        self.last = Some((method, full));
        self
    }

    /// Attach a [`Guard`](crate::Guard) to the route just registered.
    ///
    /// Routes may share a `(method, path)` when guards distinguish them; the
    /// first whose guards all pass serves the request, in registration order,
    /// so an unguarded route registered last is the fallback. A request that
    /// matches no candidate is a `404`.
    ///
    /// ```
    /// use churust_core::{guard, Call, Churust, TestClient};
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.get("/", |_c: Call| async { "beta" })
    ///             .guard(guard::header("x-beta", "1"));
    ///         r.get("/", |_c: Call| async { "stable" });
    ///     })
    ///     .build();
    /// let client = TestClient::new(app);
    /// assert_eq!(client.get("/").header("x-beta", "1").send().await.text(), "beta");
    /// assert_eq!(client.get("/").send().await.text(), "stable");
    /// # });
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if no route has been registered on this builder yet — there would
    /// be nothing for the guard to apply to.
    pub fn guard(&mut self, g: crate::guard::BoxGuard) -> &mut Self {
        let (method, path) = self
            .last
            .clone()
            .expect("guard() must follow a route registration");
        self.router.attach_guard(&method, &path, g);
        self
    }

    /// Cap the request body for the route just registered.
    ///
    /// The server-wide [`max_body_bytes`](crate::AppBuilder::max_body_bytes)
    /// still bounds what is read off the socket; this tightens it for one
    /// route, which is what lets an upload endpoint be generous while the rest
    /// of the API stays strict. Body extractors (`Json<T>`, `Form<T>`) enforce
    /// it and answer `413`.
    ///
    /// ```
    /// use churust_core::{Call, Churust, Form, TestClient};
    /// use http::StatusCode;
    /// use serde::Deserialize;
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// #[derive(Deserialize)]
    /// struct Note { text: String }
    ///
    /// let app = Churust::server()
    ///     .routing(|r| {
    ///         r.post("/tiny", |Form(n): Form<Note>| async move { n.text })
    ///             .max_body_bytes(8);
    ///     })
    ///     .build();
    ///
    /// let res = TestClient::new(app)
    ///     .post("/tiny")
    ///     .header("content-type", "application/x-www-form-urlencoded")
    ///     .body("text=this-is-far-too-long")
    ///     .send()
    ///     .await;
    /// assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    /// # });
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if no route has been registered on this builder yet.
    pub fn max_body_bytes(&mut self, n: usize) -> &mut Self {
        let (method, path) = self
            .last
            .clone()
            .expect("max_body_bytes() must follow a route registration");
        self.router.attach_limit(&method, &path, n);
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
            chain: self.chain.clone(),
            last: None,
        };
        f(&mut child);
        // Forget which route was registered *before* the scope. `guard()` and
        // `max_body_bytes()` resolve against `last`, so leaving it set meant a
        // `.guard(...)` chained after `route(...)` silently attached to the
        // previous sibling — arming the wrong route and leaving the scoped one
        // open. With `last` cleared, that chain panics at build instead.
        self.last = None;
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

    /// A bare call for tests that exercise routing rather than guards.
    fn probe(path: &str) -> Call {
        Call::new(
            Method::GET,
            path.parse::<Uri>().unwrap_or_else(|_| "/".parse().unwrap()),
            HeaderMap::new(),
            Bytes::new(),
        )
    }

    fn run(r: &Router, m: Method, path: &str) -> Match {
        r.route(&m, path, &probe(path))
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
    fn path_params_are_percent_decoded() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/u/{name}", |_c: Call| async { "" });
        }
        match r.route(&Method::GET, "/u/John%20Doe", &probe("/u/John%20Doe")) {
            Match::Found { params, .. } => assert_eq!(params.get("name").unwrap(), "John Doe"),
            _ => panic!("expected a match"),
        }
    }

    #[test]
    fn encoded_slash_does_not_create_a_segment() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/u/{name}", |_c: Call| async { "" });
        }
        // Matching at all proves %2F stayed inside one segment. Had it been
        // decoded before splitting, this would be two segments and miss.
        match r.route(&Method::GET, "/u/a%2Fb", &probe("/u/a%2Fb")) {
            Match::Found { params, .. } => assert_eq!(params.get("name").unwrap(), "a/b"),
            _ => panic!("%2F must not manufacture a separator"),
        }
    }

    #[test]
    fn a_route_with_a_literal_space_is_reachable_encoded() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/a b", |_c: Call| async { "" });
        }
        assert!(matches!(
            r.route(&Method::GET, "/a%20b", &probe("/a%20b")),
            Match::Found { .. }
        ));
    }

    #[test]
    fn malformed_encoding_is_a_bad_path() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/u/{name}", |_c: Call| async { "" });
        }
        assert!(matches!(
            r.route(&Method::GET, "/u/%zz", &probe("/u/%zz")),
            Match::BadPath
        ));
        assert!(matches!(
            r.route(&Method::GET, "/u/%FF", &probe("/u/%FF")),
            Match::BadPath
        ));
    }

    #[test]
    fn wildcard_captures_decoded_segments() {
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/f/{rest...}", |_c: Call| async { "" });
        }
        match r.route(&Method::GET, "/f/a%20b/c", &probe("/f/a%20b/c")) {
            Match::Found { params, .. } => assert_eq!(params.get("rest").unwrap(), "a b/c"),
            _ => panic!("expected the wildcard to match"),
        }
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
        match r.route(&Method::GET, "/u/7/edit", &probe("/u/7/edit")) {
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
    fn an_encoded_separator_in_a_wildcard_tail_does_not_override_a_matching_sibling() {
        // The wildcard refuses this tail, but a sibling `{n}` matches the path
        // cleanly with `n = "a/b"` — that is what splitting before decoding is
        // for. The wildcard's opinion about its own tail must not be promoted
        // into a verdict on the whole request: only POST is registered at the
        // route that does match, so `405` is the honest answer and a `400`
        // would describe a request that is in fact perfectly well formed.
        let mut r = Router::new();
        {
            let mut b = RouteBuilder::new(&mut r);
            b.get("/files/{p...}", |_c: Call| async { "wild" });
            b.post("/files/{n}", |_c: Call| async { "one" });
        }
        match run(&r, Method::GET, "/files/a%2Fb") {
            Match::MethodNotAllowed { allow } => assert_eq!(allow, vec![Method::POST]),
            _ => panic!("expected the sibling param route to answer"),
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
