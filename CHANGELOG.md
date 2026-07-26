# Changelog

All notable changes to Churust are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

All seven crates — `churust`, `churust-core`, `churust-macros`, `churust-json`,
`churust-logging`, `churust-cors`, `churust-auth` — share one version and are
released together, so every entry below applies to the whole set.

## [Unreleased]

### Changed — breaking

- **`SessionStore` is an async trait, and `store` takes the previous cookie
  value.** Both operations may now await, because a store backed by server state
  has to talk to it and a synchronous trait method cannot. `store` also receives
  the cookie the request arrived with, which is what lets a server-side store
  delete the record it is replacing: without it, logging out left the old record
  readable until its own TTL elapsed, which is precisely the revocation such a
  store exists to provide. `CookieStore` ignores the new argument, since it keeps
  no server-side record. Implementors add `#[async_trait]`, mark both methods
  `async`, and take `previous: Option<&str>` on `store`.

- **`SessionStore::store` returns `Result<Option<String>>`, and a revocation
  that did not happen is no longer answered as a logout.** `Option<String>` gave
  a store no way to say "I did not do that", so a logout whose Redis `DEL` never
  landed was indistinguishable from one that worked: `RedisStore` discarded the
  error, returned `None` for "no new cookie", and the middleware let the
  handler's cheerful `200` stand. The record at `churust:session:<id>` then
  survived for its full TTL, and since sliding expiry is on by default, a cookie
  copied before the logout both authenticated *and* pushed the deadline out
  again on every replay — for as long as the holder cared to keep using it.
  Being able to withdraw a session is the entire reason to keep one server-side,
  so failing to do so must be loud. `RedisStore` now reports a delete that did
  not get through, both on logout and on the rotation `Identity::login`
  performs, and the session middleware answers with that error in place of the
  handler's response, setting no cookie: the visitor is told they are still
  signed in and can try again. A failed *write* is still swallowed on purpose —
  that costs the visitor a sign-in, which beats a `500` on every route while
  Redis is unwell. Implementors wrap what they returned before in `Ok`; direct
  callers of `store` add a `?`.

- **`//a` and `/a//b` no longer serve `/a/b`.** Interior empty segments were
  collapsed silently, so one resource had several URLs. Two things follow from
  that: any middleware, guard or proxy rule keyed on a literal prefix
  (`path.starts_with("/admin")`) is bypassable with `//admin`, and a cache keys
  on the URL, so one resource occupies several entries while an intermediary and
  the origin disagree about identity.

  The new `PathPolicy` states the behaviour instead of leaving it emergent:
  `Strict` (**the default**) answers `404`, `Redirect` answers `308` to the
  canonical form — `308` not `301`, so a `POST` stays a `POST` — and `Collapse`
  restores the old behaviour for one release. Set it with
  `AppBuilder::path_policy`, `CHURUST_SERVER_PATH_POLICY`, or `path_policy` in
  `churust.toml`.

  **URLs that stop working under the default:** any request whose path contains
  a repeated slash. A **trailing** slash is *not* affected and is not treated as
  an alias — a directory listing at `/files/` contains relative links that a
  browser resolves against that slash, and stripping it would serve HTML whose
  every link resolves one level too high.

- **`Query<T>` and `Form<T>` reject a repeated key on a scalar field** rather
  than silently picking one. `?q=a&q=b` into a `String` is ambiguous — browsers
  take the last occurrence, some servers take the first — and resolving it
  quietly means a proxy and an origin can disagree about what was asked. Declare
  the field as `Vec<T>` to accept repetition deliberately.

### Fixed

- **`ContentNegotiation` framed its JSON error bodies with the length of the
  plain text they replaced.** `JsonErrors` swapped the body and the
  `Content-Type` but left any `Content-Length` alone, and the JSON envelope is
  always longer than the message it wraps. A synthesized `HEAD` made this
  visible and fatal: the endpoint records the `GET` body's length before
  dropping the bytes, so `HEAD /boom` went out claiming four bytes while the
  middleware then attached a twenty-five byte envelope. hyper's HTTP/1 encoder
  checks a supplied `Content-Length` against the payload it is handed, so in a
  debug build the request panicked the connection task and the client got
  nothing at all. The header is now removed before the body is replaced, and
  hyper frames what is actually sent.
- **A synthesized `HEAD` no longer grows an error body it never had.** The body
  is stripped at the endpoint, inside the plugin phase, so the message was
  already gone by the time `ContentNegotiation` saw the response and the empty
  string was being re-encoded into `{"error":"","status":N}` — a payload on a
  `HEAD` reply, describing an error the matching `GET` does not report. Such a
  reply now keeps its empty body and corrects only its headers, so it describes
  the representation `GET` would return.
- **An HTTP/3 request body cut short is no longer handed to the handler as a
  whole one.** The read loop was `while let Ok(Some(chunk))`, which has two
  outcomes where `recv_data` has three: a clean end of body and a failed stream
  both simply ended the loop, and whatever had been read so far was returned as
  `Ok`. A client that announced 5000 bytes, sent 1200 and then RESET_STREAM —
  reported by h3 as `StreamError::RemoteTerminate` — had its fragment dispatched
  as if the request were complete. The handler ran on it, so an upload was
  stored or a batch of records imported at 1200 bytes of 5000, and a `200` said
  everything had arrived. Nothing downstream could detect it afterwards, because
  a truncated body is indistinguishable from a well-formed shorter one. The
  three outcomes are now matched separately and a failed read returns before
  dispatch, so the handler never sees a partial payload; the stream is reset
  with H3_REQUEST_INCOMPLETE rather than answered with a `400`, since a peer
  that reset its request stream is cancelling and stops reading the response, so
  a status would be a report this server believed it sent and the client never
  saw. This is the request-side mirror of the entry below.
- **A `multipart/form-data` delimiter line admits only spaces and tabs before
  its CRLF, in both parsers.** RFC 2046 §5.1.1 allows nothing but
  `transport-padding` there, and both `Multipart` and `MultipartStream` accepted
  arbitrary bytes instead — so a part whose content held `\r\n--boundaryZ\r\n…`
  opened a part of its own. Go, Python and every other conforming parser read
  those bytes as content, which makes this a parser differential rather than a
  leniency: a proxy or gateway filtering on a field name sees one part that
  merely mentions it and passes the body on, while the origin parses out the
  field the filter exists to reject and acts on it. A boundary match whose tail
  is not a well-formed delimiter line is now content in both parsers and the
  scan continues through it; padding is capped at 64 bytes, since the streaming
  parser must buffer it before it can judge the line. Two consequences of the
  buffered parser scanning rather than splitting: a body with no delimiter at
  all is `400`, as it always was through `MultipartStream`, rather than `200`
  with no parts; and a body ending with a part still open is likewise `400`
  instead of yielding a silently truncated part.
- **A guard on a `{path...}` route no longer makes `OPTIONS` answer `405` with
  an `Allow` naming `OPTIONS`.** `Router::methods_for`, which builds the header
  for the automatic `OPTIONS`, enumerated the exact branch guard-free — `Allow`
  describes the resource, not the request that asked about it — but reached the
  wildcard branch by driving the ordinary matching walk with a synthetic `TRACE`
  call. That call has no headers and no authority, so it fails every guard
  there is: one `guard::host` on `/assets/{p...}` left the method list empty,
  the dispatcher skipped its `204` arm, and the request fell through to the
  `405` path — where the real call *does* pass the guard, so the refusal went
  out advertising `Allow: GET, HEAD, OPTIONS`, naming the very method it was
  refusing. The same probe also assumed no application registers `TRACE`; one
  that does turned it into a match, which the caller discarded, losing the list
  entirely. The wildcard branch is now enumerated directly, the way the exact
  branch always was, unioning the methods of every wildcard the path can reach
  and skipping one whose tail decoded to something containing a separator,
  since routing has already refused that request.
- **`Router::add`'s documentation said a repeated `(method, path)` replaces the
  earlier handler; it panics.** The code has been right since duplicate
  detection was added — silently replacing a route made a typo produce a
  handler that mysteriously never ran — but the prose was never updated, and it
  is prose about a public API, which `cargo doc` cannot check against the
  function beneath it. The doc now states the real rule, that only an
  *unguarded* duplicate is refused and guarded siblings resolve first-match-wins
  in registration order, and its `# Panics` section lists the two panics it
  omitted: the duplicate route, and a `{name}` conflicting with a differently
  spelled parameter already registered at that position.
- **A `HEAD` no longer describes the identity representation of a URL whose
  `GET` is compressed.** The body of a synthesized `HEAD` is dropped at the
  endpoint, which sits inside the plugin phase, so `Compression` saw an empty
  buffer, failed its own size floor and returned the response untouched. For the
  same `Accept-Encoding: gzip`, `HEAD /file` then answered with the identity
  `Content-Length`, `Accept-Ranges: bytes` and a strong `ETag`, while `GET /file`
  answered `Content-Encoding: gzip` with the ranges withdrawn and the tag
  weakened — one URL, one negotiation, two contradictory descriptions. RFC 9111
  §4.3.5 has a shared cache use a `HEAD` to update the stored `GET` and
  invalidate it when the two lengths disagree, so every `HEAD` evicted the
  compressed `GET` the cache was holding, and a downloader that sized a resource
  with `HEAD` was told it could resume a body that arrives without ranges. The
  plugin now remembers the request method and applies the encoded metadata to a
  `HEAD` reply as well: `Content-Encoding` is set, `Accept-Ranges` and the stale
  `Content-Length` are removed, and a strong `ETag` is weakened. No body is
  invented and no length is guessed — the encoded size cannot be known without
  doing the work the client declined to ask for, and RFC 9110 §9.3.2 permits
  omitting a field that is only determined while generating the content. The
  size floor is applied to the `Content-Length` the strip left behind, so a
  `HEAD` and its `GET` also agree about *whether* the resource is compressed.
- **Compressing a large buffered body no longer holds the runtime worker for the
  whole encode.** The comment on `CHUNK` claimed that feeding the encoder in
  pieces "puts an await point between them"; it did not.
  `futures_util::stream::iter` is unconditionally `Ready`, async-compression's
  bufread encoders are pure state machines, and tokio's cooperative budget only
  fires for tokio's own resources — none of which are in this path — so the
  entire encode ran inside a single poll. At the default level, which is brotli
  quality 11, a body of a few megabytes is seconds of CPU during which nothing
  else scheduled on that worker runs, timers and the accept loop included. There
  is now a real `yield_now` on each side of the encoder, and both are needed:
  async-compression turns a `Pending` from its input into `Ready` whenever it
  already has output bytes to hand back, so the input-side yield is the one that
  works while the encoder is eating input without emitting, and the output-side
  yield is the one that works in the ordinary case where it emits on every
  piece. Neither fires before the first piece, so a small body still costs no
  scheduler round trip. This does not make the encode cheaper, only
  interruptible.
- **`Accept-Encoding: gzip;Q=0` is read as the refusal it is.** RFC 9110 §5.6.6
  makes a parameter name case-insensitive, but the quality parameter was matched
  against the literal prefix `q=`. An uppercase `Q` fell through, the coding kept
  the default weight of 1.0, and the server compressed with the very coding the
  client had just said it could not decode; by the same slip `deflate;Q=0.1`
  could not lose a tie it should lose. The parameter name is now folded to lower
  case before the match, which is safe because a qvalue has no letters in it.
- **A stale `If-Range` now retracts the `Range` even when the range was out of
  bounds, instead of answering `416`.** The retraction was gated on the range
  having parsed as satisfiable, which excluded precisely the case `If-Range` is
  written for: a client resuming a download it remembers as larger than the file
  now is sends an offset past the new end together with the validator of the
  copy it remembers. The offset made the range unsatisfiable, the gate therefore
  never consulted the validator, and the reply was `416` with
  `Content-Range: bytes */<len>` — a dead end for the resume, when RFC 9110
  §14.2 says a validator that does not match means the `Range` header field is
  ignored, and a field that has been ignored cannot afterwards be judged
  unsatisfiable. Such a request now gets `200` with the whole current
  representation, which is the fallback offering `If-Range` exists to provide. A
  *matching* `If-Range` leaves the range in force, so an out-of-bounds one is
  still the `416` it always was.
- **`StaticFiles` no longer probes for the index file with a blocking stat on a
  runtime worker.** The guard that decided whether `<dir>/<index>` existed used
  `std::path::Path::is_file`, a synchronous `stat(2)`, inside an `async fn`
  where every other lookup awaits `tokio::fs`. It ran on whichever worker was
  polling the request rather than on the blocking pool, so until the syscall
  returned that worker polled nothing else: on local disk the cost is a warm
  dentry lookup and invisible, but on a stalled NFS or SMB mount the unrelated
  connections scheduled on that worker waited out the mount alongside it. The
  probe now awaits `tokio::fs::metadata` and reads any error as "no index here",
  exactly as `is_file` did, so the behaviour is unchanged and only the blocking
  is gone.
- **`churust-client` follows a relative redirect whose query carries a URL.**
  `resolve` decided whether a `Location` was absolute by searching the whole
  value for `://`, and that substring is perfectly ordinary *inside a query*. So
  `Location: /login?next=https://api.example.com/dashboard` — the return-to
  parameter every login flow uses, and therefore exactly the redirect an HTTP
  client meets most — was read as an absolute target and handed to `http`
  verbatim, which parsed it as a scheme-less origin-form URI; `check_scheme`
  then refused it and `send()` returned `ClientError::Url("no scheme in url")`.
  A redirect the client should simply have followed became a hard failure of the
  whole request. Absoluteness is now decided structurally, as RFC 3986 §4.2
  does: only when the segment before the first `/`, `?` or `#` is a colon
  preceded by a well-formed scheme. Two behaviours follow from doing it
  properly. A `Location` that is nothing but a query (`?page=2`) now replaces
  the query and keeps the path, per §5.3, instead of being joined on as a path
  segment. And a target naming a scheme without a `//`, such as
  `mailto:ops@example.com`, is taken whole and refused by `check_scheme` rather
  than pasted onto the current origin, where it had been producing a real
  request for `http://host/mailto:ops@example.com`.
- **A redirect that turns a request into a `GET` drops the headers describing
  the body it just discarded.** On a `301`, `302` or `303` the loop cleared the
  method and the body but left the caller's headers untouched, and they are
  re-applied on every hop — so a `POST` built with `json()` continued as a `GET`
  still announcing `Content-Type: application/json` for a payload that no longer
  existed. That is a request contradicting itself, and it is not what anything
  else on the wire sends: the Fetch standard deletes the
  request-body-header-names on precisely this transition, as curl, reqwest and
  tower-http all do. `Content-Type`, `Content-Length`, `Content-Encoding` and
  `Transfer-Encoding` are now removed when the method flips, and recorded
  alongside the cross-origin credential strip so a client-wide default header
  cannot put one back on the next hop. `307` and `308` keep the body and so keep
  these headers, which is the whole reason those codes exist.
- **`churust-templates` let a template's filename decide whether its values
  were escaped, while the response was labelled HTML either way.** minijinja
  picks the auto-escape mode from the extension and escapes only `.html`,
  `.htm` and `.xml` — the right default for a library that can render into any
  format, and the wrong one for `Renderer`, which has a single sink and stamps
  `text/html; charset=utf-8` on every reply it makes. A template the author
  called `page.txt`, or `partials/nav` with no extension at all, therefore
  interpolated its values raw and was then served to a browser as HTML: a
  mislabelled response by construction, and a stored-XSS sink as soon as one of
  those values came from a user. Nothing about a request could steer a handler
  onto such a template — it took the author naming the file — which is why this
  is listed as hardening rather than a live vulnerability, but a crate that
  documents its escaping as the reason interpolation is safe should not leave
  that guarantee resting on a file extension. `Templates::new` now pins the
  policy to HTML for every template, so the escaping agrees with the
  `Content-Type` instead of with the name. `.html`, `.htm` and `.xml` are
  unaffected — the default already escaped all three. A template that is
  genuinely not HTML can restore the old behaviour with
  `Templates::configure(|env| env.set_auto_escape_callback(..))`, which must
  run *before* the template is added: minijinja resolves the mode once, as it
  parses.
- **`Templates::from_dir` did not document that every file under the directory
  is read as a template.** Its `# Errors` section listed a missing directory
  and a parse failure but not the third way it fails, which is a file that is
  not valid UTF-8 — a `logo.png` or an editor artefact left beside the
  templates aborts the boot with the path in the message. The behaviour is
  deliberate and unchanged: skipping whatever does not look like a template
  would also skip a real template saved in the wrong encoding, turning a loud
  startup failure into a `500` on one route. Only the documentation was wrong.
- **CI lints and documents the umbrella crate with every feature, not just
  `full`.** `full` is the plugin set and stops short of `openapi`, `redis`,
  `client`, `client-tls`, `multipart` and `http3`, so nothing in the matrix
  compiled the re-exports those gate — `pub use churust_openapi as openapi;` and
  its neighbours, plus the prelude's `RedisStore` and
  `multipart::{Multipart, MultipartStream, Part}`. Renaming an item behind one
  of them stayed green here and broke only for the user who had turned the
  feature on, and for docs.rs, which builds every crate in this workspace with
  `all-features = true`. The two existing steps — the umbrella clippy and the
  doc build — were widened to `--all-features` rather than new ones added, so
  the gap closes for almost no CI time: the third-party half of that feature set
  is already in the job's cache from the steps around them, and the widening
  measured sixteen extra crate checks and a few seconds of rustdoc.
  CONTRIBUTING.md already asked new optional functionality to reach the matrix;
  this makes the matrix able to hold it.
- **`RateLimit`'s default key buckets an IPv6 peer by its /64 prefix rather than
  by the whole address.** The full address read as "one bucket per client", but
  an IPv6 address does not name a client the way an IPv4 one does: RFC 4291
  §2.5.1 spends its low half on an interface identifier that the host mints
  itself, so the smallest thing anyone is delegated is a /64 and every address
  inside it is free. Over IPv6 the limiter was therefore not a limiter at all —
  a caller taking a fresh source address per request missed the table every
  time, had its arrival time default to *now*, conformed, and never saw a `429`,
  no matter how low the configured rate. The same gap quietly leaked budget to
  honest traffic, since a laptop using RFC 8981 temporary addresses earns a new
  allowance each time it rotates. The key is now the routed half of the address,
  which is the half that costs something to change; IPv4 peers are unaffected,
  and IPv4-mapped peers (`::ffff:a.b.c.d`, how a dual-stack listener reports an
  IPv4 client) are unwrapped first, since they all sit in one /64 and masking
  them would have put the whole IPv4 internet in a single bucket. No coarser
  than a /64 on purpose: a /56 or /48 buckets by delegation, and delegation size
  is a matter of ISP taste, so it would fold strangers together to catch a
  rotation the /64 already catches. **Hosts that really do share one /64 now
  share a budget** — the trade IPv4 has always made behind NAT. A deployment
  that cannot afford it keys on the full address itself with
  `by(|call| call.peer_addr().map(|addr| addr.ip().to_string()))`, which is why
  no new knob was added for the prefix length.
//! The default key is the connection's peer address, without the port, so
//! several connections from one client share a bucket. An IPv4 peer is keyed on
//! the whole address; an IPv6 peer is keyed on its /64 prefix, because the low
//! 64 bits of an IPv6 address are the interface identifier and the host picks
//! those itself. Without the mask, a peer that walks its own subnet gets a
//! fresh allowance per address and is never limited at all, and even an honest
//! client rotating privacy addresses drifts out of its bucket. The cost of the
//! mask is that hosts which genuinely share one /64 share a budget; if that is
//! wrong for your deployment, key on the full address yourself:
//!
//! ```
//! use churust_ratelimit::RateLimit;
//!
//! let limiter = RateLimit::per_minute(60)
//!     .by(|call| call.peer_addr().map(|addr| addr.ip().to_string()));
//! ```
//!
//! Behind a reverse proxy the peer address is the proxy, which would put every
//! visitor in one bucket. Use [`RateLimit::by`] there too, and read a
//! forwarding header only after checking
use std::net::{IpAddr, Ipv6Addr};
/// The peer address without its port, so several connections from one client
/// share a bucket.
        Some(addr) => address_bucket(addr.ip()),
/// The bucket an address belongs to: the whole address for IPv4, the /64 prefix
/// for IPv6.
///
/// Keying on the full IPv6 address was the obvious reading of "one bucket per
/// client" and it was the wrong one, because an IPv6 address does not name a
/// client the way an IPv4 address does. The smallest allocation anybody is
/// delegated is a /64 — RFC 4291 §2.5.1 spends the low half of every unicast
/// address on an interface identifier, and SLAAC has the host mint those itself
/// — so the low 64 bits are chosen by the peer, for free, as often as it likes.
/// That broke the limiter in both directions. An attacker took a fresh address
/// per request and never met a 429, because every request missed the table,
/// defaulted its arrival time to `now` and conformed; and an ordinary laptop
/// running RFC 8981 temporary addresses silently earned a new allowance every
/// time it rotated. Masking to the /64 keys on the part of the address that is
/// routed to the peer rather than the part the peer writes itself, which is the
/// only half that costs anything to change.
///
/// A /64 and no coarser. Aggregating to a /56 or /48 would bucket by delegation
/// rather than by subnet, and delegation sizes are a matter of ISP taste, so it
/// would fold strangers together at some providers to catch a rotation that a
/// /64 already catches. The residual is that hosts sharing one provider's /64 —
/// virtual machines handed single addresses out of a rack prefix, say — share a
/// bucket; that is the trade IPv4 has always made behind NAT, and a deployment
/// that cannot afford it keys on something else with [`RateLimit::by`].
///
/// IPv4-mapped addresses are unwrapped before any of that. A dual-stack
/// listener reports IPv4 peers as `::ffff:a.b.c.d`, and every one of those sits
/// in `::ffff:0:0/96` — inside a single /64 — so masking them would have put
/// the entire IPv4 internet in one bucket and turned the fix into a far worse
/// defect than the one it repairs.
fn address_bucket(ip: IpAddr) -> String {
    let v6 = match ip {
        IpAddr::V4(v4) => return v4.to_string(),
        IpAddr::V6(v6) => v6,
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return v4.to_string();
    }
    let mut octets = v6.octets();
    octets[8..].fill(0);
    Ipv6Addr::from(octets).to_string()
}
    #[test]
    fn a_client_rotating_through_one_ipv6_subnet_gets_no_extra_budget() {
        let limiter = RateLimit::per(2, Duration::from_secs(30));
        let rotate = |suffix: &str| {
            let ip: IpAddr = format!("2001:db8:1:2::{suffix}").parse().unwrap();
            limiter.check(&address_bucket(ip))
        };
        assert!(rotate("1").is_ok());
        assert!(rotate("2").is_ok());
        assert!(
            rotate("3").is_err(),
            "a new address out of the same /64 is the same client and must not \
             reset the budget"
        );
    }
    #[test]
    fn separate_ipv6_subnets_keep_separate_budgets() {
        let one: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_ne!(
            address_bucket(one),
            address_bucket(other),
            "different /64s are different networks"
        );
    }
    #[test]
    fn an_ipv4_peer_keeps_every_octet() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(address_bucket(ip), "203.0.113.7");
    }
    #[test]
    fn an_ipv4_mapped_peer_is_bucketed_as_the_address_it_carries() {
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        let neighbour: IpAddr = "::ffff:203.0.113.8".parse().unwrap();
        assert_eq!(address_bucket(mapped), "203.0.113.7");
        assert_ne!(
            address_bucket(mapped),
            address_bucket(neighbour),
            "a dual-stack socket reports IPv4 peers inside one /64, so masking \
             them would put the whole IPv4 internet in a single bucket"
        );
    }
- **A saturated connection budget no longer blocks shutdown.** The accept loop
  awaited a `max_connections` permit *outside* the shutdown race, so once every
  slot was held the shutdown signal was never polled — `serve()` did not return
  and the process had to be `SIGKILL`ed, with `shutdown_timeout_ms` unable to
  bound it. Long-lived connections (WebSockets, SSE, idle h2) made that the
  normal case, not the edge one. Slot acquisition and `accept` are now one
  cancellable step raced against shutdown, in both `serve` and `serve_unix`.
- **A directory listing no longer follows a symlink out of the served root.**
  The symlink-escape guard ran *after* the directory dispatch, and
  `metadata` follows symlinks — so a link inside the root pointing at, say,
  `/etc` reported `is_dir()`, took the listing branch, and returned before the
  guard. Every filename under the target was disclosed. The file path was
  already guarded; only the listing was not. Confinement now runs before
  anything decides what to do with the target, and again after an index file is
  joined.
- **Directory listing links resolve.** They were prefixed with the
  request-relative path, which is right at a subdirectory *without* a trailing
  slash and wrong everywhere else — at `/files/sub/` every link resolved to
  `/files/sub/sub/…`. A directory URL now canonicalises to the trailing-slash
  form (`308` otherwise) and links are bare, so the same markup works at any
  depth. This also makes the `PathPolicy` docs' justification for preserving
  trailing slashes true, which it was not before.
- **A shallow `{name...}` no longer shadows a deeper one.** `walk_wildcard`
  consulted a node's own wildcard before descending, so registering
  `/files/{p...}` and `/files/img/{q...}` made the second unreachable for
  everything under `/files/img/` — silently. The deepest wildcard now wins, a
  `405` found deeper does not pre-empt a real handler found shallower, and when
  neither depth serves the method the `Allow` header unions both.
- **Conflicting path parameter names fail at startup.** `/users/{id}` followed
  by `/users/{name}/profile` silently reused `id` for both, so the second
  handler looked up a key that was never captured and returned `400` with no
  clue why. Now a panic at registration, like a misplaced wildcard or a
  duplicate route.
- **`Call::cookie` reads every `Cookie` header field.** HTTP/2 permits cookie
  crumbs as separate fields (RFC 9113 §8.2.3). Reading only the first meant a
  session cookie in the second was invisible, so every h2 request looked freshly
  anonymous — a silent logout on each request.
- **Session cookies carry a signed expiry.** `Max-Age` on `Set-Cookie` is only a
  hint to a well-behaved client, so a captured cookie stayed valid for as long
  as the signing key did. `Sessions::max_age` now signs the deadline into the
  payload and `CookieStore` enforces it on load. Revocation still needs
  server-side state, and the docs now say so.
- **`on_error` no longer drops protocol-required headers.** It replaced the
  whole response, so installing a custom error page silently removed `Allow`
  from every `405` (RFC 9110 §15.5.6 requires it), and likewise
  `WWW-Authenticate` and `Content-Range`. Headers the renderer did not set are
  now carried over.
- **`serve_many` aborts at bind, as documented.** Binding happened inside each
  spawned accept loop, so a failure surfaced only after shutdown: the process
  served the addresses that worked and said nothing. Every address is now bound
  before any starts serving.
- **`receive_json` and `receive_text` report an over-limit body as `413`.** Both
  used `receive_bytes`, which swallows a read error into an empty payload, so an
  oversized body became `400 invalid JSON body: EOF while parsing a value` — and
  the per-route cap was skipped entirely.
- **The tower adapter preserves `Content-Length`.** It rewrapped every layered
  response as a stream, so any response through any layer — even `Identity` —
  lost its exact length and was chunked, and a synthesized `HEAD` could no
  longer report a size. It also now writes the URI back alongside the headers,
  so a path-rewriting layer is not silently a no-op, and a layer that drives the
  inner service from another task gets a named error instead of a panic.
- **`BearerToken` accepts any casing of the scheme.** RFC 7235 §2.1 makes it
  case-insensitive; matching the literals `Bearer ` and `bearer ` rejected
  `BEARER ` with a `401` for a valid credential.
- **`413` is selected by error type, not by message text.** The oversized-body
  path matched on `http-body-util`'s `Display` output, which a patch release
  could change and silently turn every `413` into a `400`.
- **A malformed `churust.toml` is reported.** A parse error silently reverted
  every setting to defaults — host, port, limits and TLS paths — over a single
  typo, with nothing logged. A missing file remains the ordinary, quiet case.

- **Graceful shutdown now drains.** It never had. The engine took a shutdown
  watcher and dropped it immediately, so `GracefulShutdown::shutdown()` had
  nothing to wait on and returned in microseconds — measured at 378µs with a
  request in flight. `shutdown_timeout_ms` had never delayed an exit. In a
  binary this meant `main` returned and the process tore down mid-response.
  Connection tracking is now Churust's own, because hyper-util's sealed
  `GracefulConnection` covers the plain connection but not the upgradeable one
  WebSockets need.
- **`keep_alive_ms` is a duration again.** It was read as `keep_alive_ms > 0` —
  a boolean — so `5000` and `86400000` were the same program and idle
  connections were never closed. Each cost a file descriptor and a task, which
  made "open connections and go quiet" the cheapest way to exhaust a server.
  Idle means *no request in flight*, so a slow handler is never cut off, and an
  upgraded WebSocket is never truncated.

- **`Allow` agreed with itself.** A path with one `GET` route told `OPTIONS` it
  supported `GET, HEAD, OPTIONS` and simultaneously told a `DELETE` that it
  supported only `GET`. RFC 9110 §15.5.6 and §9.3.7 describe the same fact about
  the same resource; it was generated in two places, and the two drifted. There
  is now one generator, and the header is sorted so both responses are
  byte-identical. A `405` whose `Allow` would be empty is now a `404`.
- **`OPTIONS *` answers.** RFC 9110 §9.3.7 defines the asterisk-form target as
  a question about the server, not a resource. It was routed as a path and
  answered `404` — the one reply that is certainly wrong to a capability probe.
  Now `204` with the methods registered anywhere in the router.
- **Requests carrying both `Transfer-Encoding` and `Content-Length` are refused**
  with `400` and `Connection: close`. hyper frames such a message by the
  transfer coding, which RFC 9112 §6.3 permits — but the risk is upstream: a
  proxy that believed the `Content-Length` forwards a different number of body
  bytes than this server consumes, and the leftovers become the next request on
  a reused connection. Since what sits in front cannot be known, the ambiguity
  is refused.
- **Shutdown no longer waits out the grace period for idle connections.** An
  idle HTTP/2 connection is wound down by sending GOAWAY and waiting for the
  *peer* to close, which an idle peer never does — so every shutdown cost the
  full `shutdown_timeout_ms` and a rolling restart paid it on every instance.
  A connection with no request in flight now gets a brief window to flush its
  GOAWAY and is then dropped. Connections with work in flight are unaffected.
- **`churust-cors` merges `Vary` instead of overwriting it, and marks every
  response.** The plugin wrote `Vary: Origin` with a plain insert, discarding
  whatever a layer further in had already earned. Install it alongside
  `churust-compression` and CORS unwinds last, so a gzip response left the
  server keyed on `Origin` alone: a shared cache stores those compressed bytes
  and hands them to the next same-origin client that sent no `Accept-Encoding`,
  which cannot decode them. The merge is now the same one the compression plugin
  performs — split on commas, compare case-insensitively, leave `*` and an entry
  already present alone — so the two agree whichever order they are installed
  in. The mark also no longer depends on the answer being yes: a same-origin or
  refused request gets a response with no `Access-Control-Allow-Origin`
  *because* of its `Origin`, and without `Vary` a cache was free to store that
  header-less answer and replay it to an origin the policy allows, whose browser
  then blocks a response the server would have permitted.

### Added

- **HTTP/3 over QUIC** (`http3` feature, implies `tls`). `churust_core::http3`
  binds its own UDP socket and runs h3 requests through the same pipeline every
  other transport uses, so a handler cannot tell which one answered it: routing,
  extractors, plugins, streamed response bodies and the server-wide body cap all
  behave identically. `Http3Server::bind` separates binding from serving so the
  port can be read back before anything is accepted, and
  `AppBuilder::advertise_http3` emits the `Alt-Svc` header without which almost
  no client would ever try QUIC at all. WebSockets are deliberately not carried:
  h3 upgrades through Extended CONNECT (RFC 9220), a different handshake from the
  HTTP/1.1 one the `ws` feature implements.

- **`churust-compression`: response compression** (brotli, gzip, and `deflate`
  as the zlib format RFC 9110 §8.4.1.2 actually names, which is not what a raw
  deflate encoder emits). Negotiated from `Accept-Encoding` with client `q`
  values deciding and the server's order breaking ties. A streamed body stays
  streamed through the encoder rather than being collected to compress it.
  `Vary: Accept-Encoding` goes on every response the plugin sees, not only the
  compressed ones, because a cache that stored one variant without it would
  serve brotli to a client that never asked. `206`, `Content-Range`,
  already-encoded and body-less responses are skipped, and a strong `ETag` is
  weakened, since a compressed body is equivalent to the original rather than
  identical to it.

- **`churust-ratelimit`: rate limiting.** GCRA rather than a fixed window, so
  requests are smoothed instead of admitted in a stampede at the top of each
  window, and `Retry-After` falls out of the arithmetic as an exact figure. Keyed
  on the peer IP by default, on anything else through `RateLimit::by`, which can
  also return `None` to exempt a request. Usable as a plugin or as scoped
  middleware. The key table is bounded and pruned.

- **`churust-templates`: server-rendered HTML** on minijinja, with auto-escaping
  driven by the template's extension. `Templates::from_dir` reads and parses
  every template at startup, so a syntax error is a boot failure naming the file
  rather than a `500` on the one route nobody visits until Friday. A render
  failure tells the client only that rendering failed; the template name, line
  and offending variable go to the error's source, not the response body.

- **`churust-redis`: server-side sessions.** The cookie carries an opaque
  identifier, 256 bits from the OS CSPRNG, and the contents live in Redis, which
  buys the one thing `CookieStore` cannot offer: logging out deletes the record,
  so a cookie copied beforehand stops working. Sliding or absolute expiry,
  key prefixing, and identifiers validated for shape before they are ever
  interpolated into a key.

- **`churust-client`: an HTTP client**, on the same hyper the server runs on, so
  a Churust binary carries one HTTP implementation rather than two. Pooled
  connections, an enforced timeout covering the whole request including
  redirects, a bounded response body, JSON and form helpers, and a redirect
  follower that re-checks the scheme at every hop so a redirect cannot walk an
  `https` request down to `http`. HTTPS behind the `tls` feature.

- **`churust-openapi`: OpenAPI 3.1 descriptions.** Paths, methods and path
  parameters come from the router, so they cannot drift from the application;
  prose, schemas and responses are written explicitly, because handler extractor
  types are erased by the time a router exists and anything claiming to infer
  them would be inferring them from an annotation you wrote anyway. `undescribed`
  and `stale` report drift in both directions so a test can fail the build when
  the document and the router disagree.

- **Streaming `multipart/form-data`.** `MultipartStream` yields fields one at a
  time and each field's content in chunks, so memory stops scaling with upload
  size: the buffered parser holds the whole body, this one holds a chunk. The
  ceiling itself is unchanged — `max_body_bytes` still bounds the request — but
  raising it for an upload route is now affordable. `Multipart` is unchanged and
  remains the right answer for form fields and small attachments.

- **A login and logout layer over sessions** (`Identities`, `Identity`,
  `Authenticated`). Two deadlines, because they answer different questions: an
  absolute `login_deadline` bounds a session stolen and then used continuously,
  which an idle timeout never expires, and an idle `visit_deadline` protects an
  unattended machine. The last-seen timestamp is refreshed at most once per tenth
  of the deadline rather than on every request, so the session plugin's
  "only re-issue when something changed" rule survives. `Identity::login` rotates
  the session identifier while keeping the rest of the session, so a pre-login
  cart survives a privilege change but a planted session id does not.

- **`Session::rotate` and `SESSION_ID_KEY`**, the mechanism the above rests on: a
  server-side store records its record id under a reserved key, and rotating
  removes it so the next write mints a new one.

- **`Router::routes` and `AppBuilder::routes`**, the registered `(method,
  pattern)` inventory, kept alongside the trie rather than reconstructed from it
  so the patterns are spelled exactly as the application wrote them.

- **`AppBuilder::insert_state`**, the `&mut self` counterpart to `state`, so a
  plugin can publish something for its own extractor to find. `install` hands a
  plugin `&mut AppBuilder` and the chainable setter was unreachable from there.

- **`tower` feature: run a `tower::Service` as Churust middleware.** The
  ecosystem's `Layer`s — compression, tracing, request ids, header manipulation,
  validation, metrics — become reachable without reimplementing any of them
  here. The adapter is one-directional; `Middleware`/`Next` stays the native way
  to write one. The layered service is built once at install time, so a stateful
  layer keeps its state across requests. Two documented limits: backpressure is
  not propagated (`poll_ready` has no counterpart in a pipeline handed an
  already-accepted request), and a `Service` error becomes a `500` — which keeps
  the always-produce-a-response invariant without axum's `Infallible` boundary,
  where adding a timeout layer to a route requires wrapping it in a
  `HandleErrorLayer`.
- **`Body` implements `http_body::Body`**, which is what lets it cross the
  `http`/`http-body` boundary the adapter needs.
- **`Call::headers_mut`**, so middleware can rewrite the request head.
- **`churust-lab`**, an incubation crate that will never reach 1.0. Ideas worth
  trying in public but not worth freezing into `churust-core`'s API live there
  first, and graduate by being deprecated in place rather than deleted, so users
  migrate by dropping the prefix. First inhabitant: `BodyLimit<T, LIMIT>`, a
  body cap written in the type. It shares the workspace version for now; the
  crate docs record why, and when that must be revisited.
- **`Option<T>` extracts optionally**, via a new `OptionalFromCallParts` trait
  that `Query`, `Path`, `Header` and `BearerToken` implement. One trait rather
  than a parallel `OptionalQuery`/`OptionalPath`/`OptionalHeader` type per
  extractor — the design axum-extra shipped, deprecated, and replaced with this
  one. **Absent is `None`; malformed is still an error**, so a typo'd query
  string does not silently become a default.
- **Handler type errors say what is wrong.** A closure that does not satisfy the
  handler bounds produced a bare `the trait bound ...: IntoHandler<_> is not
  satisfied`, which names the trait and nothing else. `#[diagnostic::on_unimplemented]`
  now states the actual rules — every argument but the last is `FromCallParts`,
  only the last may consume the body, there can be only one — and
  `#[diagnostic::do_not_recommend]` stops rustc suggesting the blanket impls,
  which pointed at implementing `Handler` by hand instead of at the wrong
  argument.
- **Repeated query and form keys fill a `Vec<T>`.** `?tag=a&tag=b` and a
  checkbox group posting `opt=email&opt=sms` are ordinary HTML — a repeated
  checkbox and `<select multiple>` produce exactly this — and both previously
  failed with `400 invalid type: string "a", expected a sequence`, blaming the
  caller for the framework's parser choice with no workaround inside the
  extractor. `Query` and `Form` now parse with `serde_html_form`.
- **`Payload` respects the per-route body cap.** Buffering extractors checked
  it; the streaming one did not, so a route that tightened its limit was not
  actually tightened, and a handler collecting the stream allocated up to the
  server-wide ceiling — which an operator may have raised for one legitimate
  upload route, leaving every other `Payload` route to inherit it.
- **`h2_max_header_list_size`** (default `16384`) and
  **`h2_max_concurrent_streams`** (default `200`, `0` unlimited). `max_headers`
  configures HTTP/1 only — it counts headers, and HTTP/2 has no count, only an
  encoded size. An h2 connection multiplexes many requests, so without a stream
  cap one connection is an unbounded amount of concurrent work.
- **`max_connections`** (default `25000`, `0` unlimited) — a bound on
  connections *being served*, acquired before `accept` so excess load waits in
  the kernel backlog rather than as memory and descriptors in the process.
- **`max_tls_handshakes`** (default `256`) and **`tls_handshake_timeout_ms`**
  (default `10000`). A handshake is asymmetric work — cheap to request,
  expensive to answer — so it gets its own tighter bound, and
  `header_read_timeout_ms` cannot cover it because until the handshake finishes
  there is no HTTP layer to time out.
- Accept errors now back off (capped exponential, reset on success) instead of
  spinning. A persistent `EMFILE` — the state a connection flood drives toward —
  previously became a busy loop that starved the tasks that would free a
  descriptor.
- Failed and timed-out TLS handshakes are logged at debug. They were silent,
  which made certificate and protocol-version problems invisible to operators.
  Debug rather than warn: an internet-facing port sees constant scanner noise.

- **Streaming request bodies.** The engine hands the body to the handler as a
  stream instead of collecting it. `Payload` exposes the stream;
  `Call::try_receive_bytes` collects on demand for extractors that need a whole
  body. Previously `max_body_bytes` was a hard ceiling on upload size and memory
  scaled as `max_body_bytes × concurrent uploads`.
- **`Call::peer_addr`** — the connection's socket address, which per-IP rate
  limiting and audit logging had no way to obtain. Behind a reverse proxy this
  is the proxy; check it before trusting `X-Forwarded-For`.
- **Deployment tuning** — `keep_alive_ms`, `backlog` and `shutdown_timeout_ms`
  through the layered config. Graceful shutdown was previously unbounded, so one
  slow request delayed exit forever, which under an orchestrator means being
  killed rather than exiting cleanly. Binding now sets `SO_REUSEADDR`, removing
  the most common cause of a failed restart.
- **`Path<T>` destructuring** — `Path<(u64, String)>` and `Path<Struct>`, not
  just a single positional parameter. Captures are now ordered, which also gives
  `params_iter` the deterministic order its docs had disclaimed.
- **`churust_core::block`** for blocking work, so a synchronous call does not
  occupy a runtime worker. A panic inside becomes a `500`.
- **`Either<A, B>`** to accept one of two extractors — JSON or form in one
  handler — plus `String` and `Bytes` raw-body extractors.
- **`StaticFiles::list_directories`**, opt-in. Filenames are HTML-escaped: a
  file named `<script>.txt` is stored XSS otherwise.
- **Unix domain sockets** via `serve_unix` / `start_unix`, unlinking a stale
  socket file that would otherwise make bind fail forever.
- **Several bind addresses** via `AppBuilder::bind` and `engine::serve_many` —
  IPv4 and IPv6, or a public and an admin port.
- **HTTP/2.** The engine now uses hyper-util's `auto` builder, negotiating h2
  over TLS via ALPN (advertised `h2`, then `http/1.1`) and h2c by prior
  knowledge in plaintext. HTTP/1.1 behaviour is unchanged.

  The v1 engine used `http1::Builder` directly with a note that `auto::Builder`
  returned a future the spawn closure could not own. That was accurate:
  `serve_connection_with_upgrades` borrows the builder. Moving the builder into
  the spawned task resolves it, and the upgrades variant is what keeps
  WebSockets working.

  Graceful drain does not cover upgraded connections — hyper-util 0.1.x does
  not implement `GracefulConnection` for them, and a WebSocket has no request
  boundary at which to drain anyway.
- **Cookies.** `Call::cookie` reads a named cookie percent-decoded;
  `Response::with_cookie` appends a `Set-Cookie` header — appends rather than
  replaces, since several cookies each need their own header. Defaults are the
  safe ones: `HttpOnly`, `SameSite=Lax`, `Path=/`, because a cookie is a
  credential until proven otherwise.
- **Sessions** (`Session`, `Sessions`, `SessionStore`, `CookieStore`).
  An unchanged session is not re-issued, which avoids quietly
  extending the expiry of a session the visitor is not using. `CookieStore`
  **signs but does not encrypt** — the visitor can read their own session — and
  the signature is HMAC-SHA256, compared in constant time.
- **`Multipart`** (`multipart` feature) — `multipart/form-data` uploads, with
  `field`, `file` and `parts` accessors. Parsing runs over the buffered body,
  so an upload is bounded by `max_body_bytes` and by any per-route cap; a
  part-count limit guards a body that is small overall but made of very many
  parts. Not a streaming parser, which is the deliberately safe choice and why
  it landed after the limits work.
- **`TestResponse::headers`** for assertions on headers that repeat, such as
  `Set-Cookie`.
- **Route-scoped middleware.** `RouteBuilder::intercept` applies middleware to
  routes registered later in the scope, including nested ones. Nested scopes
  inherit a clone of the parent chain, so a child cannot affect its parent, and
  a route registered *before* the `intercept` call is deliberately not covered —
  the reading order of the block matches the behaviour. Completes v1 design §6.
- **Route guards.** `RouteBuilder::guard` attaches a predicate to the route just
  registered; several routes may share a `(method, path)` when guards
  distinguish them, and the first whose guards pass serves. Ships
  `guard::header`, `guard::host`, `guard::fn_guard`, and the `all`/`any`/`not`
  combinators. A request matching no candidate is `404`, **not** `405` — the
  route does not match, and advertising the method as allowed would tell a
  client to retry something that can never succeed.
- **Per-route body limits.** `RouteBuilder::max_body_bytes` tightens the
  server-wide cap for one route, so an upload endpoint can be generous while
  the rest of the API stays strict. Enforced by `Json<T>` and `Form<T>` with
  `413`.
- **`on_error` status pages.** Render your own `4xx`/`5xx` responses; returning
  `None` keeps the default, so a hook can take over only the statuses it cares
  about. Covers routing failures such as `404` and `405` that never produced an
  `Error`. Runs inside the security-header layer, so a replaced error page is
  still protected. Completes v1 design §5.3.
- **`Header<T, N>` extractor.** Reads one named header, parsed into `T`, with
  the name carried by a marker type implementing `HeaderName` so it is part of
  the handler signature. Completes v1 design §5.2.
- **`CallJson` trait** (`json` feature) — `call.receive_json()` and
  `call.respond_json()` for call-style handlers, completing the hybrid API.
  Lives in `churust-json` so the core keeps no `serde_json` dependency.
  Completes v1 design §5.1.
- **Request correlation** (`logging` feature). `CallLogging` seeds a
  `RequestId`, emits `request_id` and `trace_id` on every line, and echoes
  `x-request-id`. An inbound W3C `traceparent` is continued so a trace survives
  a service boundary — validated rather than trusted, since an all-zero or
  malformed trace id from one caller would otherwise poison the correlation
  index.
- **`AppBuilder::install_middleware`** — the chainable counterpart to
  `add_middleware`.

- **Conditional GET for static files.** Every `StaticFiles` response now carries
  a weak `ETag` (from mtime and length) and `Last-Modified`. `If-None-Match`
  and `If-Modified-Since` produce `304`; `If-Match` and `If-Unmodified-Since`
  produce `412`. Per RFC 9110 §13.1.3, `If-None-Match` suppresses
  `If-Modified-Since` entirely rather than acting as a tie-breaker. Without
  validators a cache could never revalidate — every repeat request refetched
  the whole file.
- **Byte-range requests for static files.** `Accept-Ranges: bytes` on every file
  response, `206` with `Content-Range` for a single span, `416` with
  `bytes */<len>` when unsatisfiable, and `If-Range` honoured. This fixes media
  playback: Churust maps `video/mp4` and `audio/mpeg`, and Safari refuses to
  play a `<video>` whose range probe is answered `200`. Multi-range requests
  return the whole entity rather than `multipart/byteranges`, which RFC 9110
  permits.
- **Security response headers by default** — `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, and
  `Strict-Transport-Security` **only when TLS is configured**. A handler that
  sets one of these itself wins. Configure with
  `AppBuilder::security_headers`, or disable with `without_security_headers`.
  There is no default `Content-Security-Policy`: a generic one either breaks
  pages or implies protection it does not give.
- **Request limits**, all configurable via `churust.toml` and `CHURUST_*`:
  header read timeout (10s, the slow-loris defence), max headers (100), max
  path segments (64, rejected with `414`), and WebSocket frame (1 MiB) and
  message (4 MiB) caps. Closes v1 design §12, which specified read timeouts
  that were never built.
- **`IntoError`** so `?` works on foreign error types. Its `message` defaults to
  the status' canonical reason and **not** `Display` — error types routinely
  render connection strings and file paths, and forwarding those to clients by
  default would turn every adopter's first `?` into an information disclosure.
- **`Form<T>`** extractor for `application/x-www-form-urlencoded` bodies. `415`
  on the wrong content type, `400` when the body does not deserialize.
- **`secure_compare`** for checking a request-supplied value against a secret
  without leaking its contents through timing.
- **`StaticFiles::try_handler`** for callers who would rather handle an unusable
  root than have it panic.

### Changed

- **`Router::route` now takes the request**, which guards need in order to
  decide. Breaking for code driving `Router` directly; applications on the
  routing DSL are unaffected.
- **`StaticFiles::dir(..).handler()` panics if the root is missing or is not a
  directory.** It previously returned `500` on every request, forever — a
  configuration mistake reported as a runtime fault.
- **Registering the same `(method, path)` twice panics.** It previously replaced
  the first handler silently, so a copy-paste typo produced a route that
  mysteriously did nothing.
- `HEAD` on a static file now reports the real `Content-Length`, since the
  length is known before any bytes are read.

### Deprecated

- **`Cors::permissive`** → **`Cors::allow_any_origin_insecure`**. The new name
  is deliberately uncomfortable: reflecting every origin means any site a user
  visits can read authenticated responses from an API that answers on an
  ambient credential. The old name still works and forwards to the new one.

### Security

- **Session cookies are signed with HMAC-SHA256** (`hmac` + `sha2`) rather than
  a keyed FNV digest, and verified in constant time. The previous digest was
  documented as non-cryptographic, but a weak default that is only *documented*
  as weak is still a footgun. Still signed rather than encrypted: the visitor
  can read their own session, and the docs say so.
- Cookie values are percent-encoded on the way out. The cookie-value grammar
  permits a literal `%`, but since values are decoded on read, letting one
  through made the round trip lossy — a stored `%3D` came back as `=`, which
  corrupted a session payload before its signature was checked.
- **An HTTP/3 response body that fails partway is no longer reported as
  complete.** The `Err` arm returned `Ok(())` on the theory that skipping
  `finish` left the stream unfinished. It does not: the `RequestStream` is owned
  by the per-request task and dropped on the way out, and quinn finishes a
  stream when its `SendStream` drops. So a handler streaming a hundred records
  whose cursor died after three sent a well-formed `200` with three records and
  a clean end of stream — the client had no way to tell it was short, and would
  store it as the whole answer. The same handler over HTTP/1.1 or HTTP/2
  surfaces the error to hyper, which aborts. The stream is now reset with
  H3_INTERNAL_ERROR before returning, which wins the race against the drop:
  after a reset the drop-time finish fails with `ClosedStream`, which quinn
  ignores. Depending on what has reached the wire, the client sees the abort
  either in place of the response head or partway through the body; what it
  never sees is a complete-looking short one.
- **One bad HTTP/3 stream no longer closes the whole QUIC connection.**
  `resolve_request` reports a *stream* error — a header block over
  `max_field_section_size`, a stream that ends before its headers arrive — and
  the accept loop propagated it with `?`. That returned from the connection
  task, dropped the `h3::server::Connection`, and its `Drop` closed the QUIC
  connection with H3_NO_ERROR, taking every other request multiplexed on it.
  Resolving now happens inside the per-request task, so a failed stream is
  logged and abandoned on its own. A genuinely connection-fatal error is not
  lost: h3 records it on the shared connection state and the next `accept`
  returns it, which also lets h3 send the true code (H3_FRAME_UNEXPECTED,
  QPACK_DECOMPRESSION_FAILED) instead of the H3_NO_ERROR that dropping sent for
  what was a protocol violation. The same move fixes head-of-line blocking in
  the accept loop: `accept` returns as soon as a bidi stream exists, so
  awaiting the peer's HEADERS frame there stalled every request queued behind a
  stream whose headers never came.
- **`tls_handshake_timeout_ms` now covers the wait for a handshake permit, not
  just the handshake.** The deadline was armed after `max_tls_handshakes` had
  been acquired, and that acquisition had no deadline of its own — so the budget
  behaved as a rate limiter on failure rather than as a cap. At the defaults,
  stalled ClientHellos expired 256 per 10s while the rest waited untimed, each
  still holding the connection permit taken before the handshake task started;
  filling `max_connections` that way costs one TCP connect per slot and blocks
  the accept loop for as long as the queue takes to drain, which is minutes
  rather than the advertised ten seconds. The clock now starts when the
  connection is accepted, and cancelling on expiry releases the queued
  acquisition too. Under genuine handshake overload this can drop a legitimate
  client while it is still queued; the alternative was holding its connection
  slot instead.
- **A connection that never chooses a protocol is now closed at
  `header_read_timeout_ms`.** The knob documented itself as "the slow-loris
  defence", but both mechanisms it drives — the HTTP/1 header deadline and the
  HTTP/2 keep-alive ping — belong to a connection `auto::Builder` has not built
  yet while it reads up to the 24 bytes of the HTTP/2 preface to decide which
  one to build. hyper-util's sniffing future carries no timer, so a peer that
  connected and sent nothing, or sent 23 of the 24 preface bytes, held a
  connection permit and a drain token bounded only by the idle watchdog at
  `keep_alive_ms` — 75s against an advertised 10s — and by nothing whatsoever
  when `keep_alive_ms` is `0`, which disables the watchdog outright. The
  deadline now covers that phase too and stops applying at negotiation rather
  than at the first request, so an HTTP/2 client that handshakes and then idles
  is still governed by the keep-alive ping and not by this.
- Encoded path separators (`%2F`, `%5C`) are refused by `StaticFiles`. Traversal
  was already blocked by the `..` rejection, but `%2F` silently became a real
  separator once a wildcard's segments were rejoined, which made the safety
  argument depend on reasoning about rejoining.
- **A request carrying both `Transfer-Encoding` and `Content-Length` answers
  `400` below hyper 1.11 and is served-then-closed from 1.11 onwards.** Either
  way the connection does not survive the message, which is the property that
  matters: whatever a proxy in front believed the body length to be, no leftover
  byte is read as the start of a second request. hyper 1.11 removes the
  `Content-Length` while parsing, so Churust's own check can no longer see the
  ambiguity to refuse it — but the same release sets `keep_alive = false` for
  exactly this shape. The check stays for the versions `hyper = "1"` still
  admits below 1.11, where nothing else closes the connection.
- **`RateLimit` stores a digest of the bucket key, not the key.** `max_keys`
  bounds how many entries the table holds and nothing bounded how large one was,
  so with a key read off a header — `by(|call| call.header("x-api-key").map(…))`,
  which the docs themselves suggest — the caller chose what a bucket cost.
  A fresh key gets the full burst, so a few thousand requests each carrying a
  distinct several-hundred-kilobyte header were all admitted, stayed far below
  the entry cap, never tripped the prune, and pinned gigabytes for the life of
  the process; repeating it exhausted memory. Each entry is now a 64-bit digest
  and a timestamp, which makes the documented "a few megabytes at 100,000 keys"
  true by construction for every key function rather than only for short keys.
  The seed is per table and randomly chosen rather than fixed, because two keys
  that collide share one budget and a digest anyone could compute offline would
  let a caller hunt for a value colliding with somebody else's key and spend it
  for them. Truncating the key to a fixed length would have been cheaper and
  would have handed exactly that collision to anyone able to type a prefix.

## [0.2.0] - 2026-07-25

### Added

- **`churust::tokio`.** The runtime is re-exported, so `churust = "0.2"` is a
  complete dependency list. Previously a crate depending only on `churust`
  failed to compile with `E0433: cannot find tokio in the crate root`, because
  `#[churust::main]` expanded to an absolute `::tokio` path.
- **Automatic `HEAD`.** A `HEAD` request to a route with only a `GET` handler
  now runs that handler and returns its status and headers without a body, per
  RFC 9110 §9.3.2. Previously `405`. An explicitly registered `HEAD` route still
  wins, and `HEAD` on a route with no `GET` is still `405`. `Content-Length` is
  preserved for buffered bodies so a client sizing a resource gets the same
  answer `GET` would give; streamed bodies are dropped rather than drained and
  omit the header.
- **Automatic `OPTIONS`.** An `OPTIONS` request no handler claims returns `204`
  with `Allow`, listing the registered methods plus a synthesized `HEAD` where
  `GET` exists. CORS preflight keeps priority.
- `Router::methods_for`, which reports the methods registered for a path
  including any a trailing wildcard would serve.

### Fixed

- **A `{path...}` wildcard was unreachable whenever a static route shared its
  prefix.** With `/files/{path...}` and `/files/special/x` both registered,
  `GET /files/special` returned `404`. This broke `StaticFiles` in its most
  ordinary deployment — a catch-all asset route beside any other route.
  Resolution is now exact match, then wildcard, then `405` with the union of
  both allow sets, then `404`.
- **Path parameters are percent-decoded**, matching query parameters, which
  already were. `/u/John%20Doe` now yields `John Doe` rather than
  `John%20Doe`. Segments are split before decoding, so `%2F` cannot forge a
  path separator, and `+` stays literal in a path rather than becoming a space.
- Malformed percent-encoding and non-UTF-8 path segments return `400` instead
  of being passed through.
- `StaticFiles` refuses encoded separators (`%2F`, `%5C`). They previously
  became real separators once a wildcard's segments were rejoined. Traversal
  was already blocked by the existing `..` rejection; this removes the need to
  reason about rejoining to establish that.

### Changed

- **Tokio features narrowed** from `full` to the set Churust uses:
  `rt-multi-thread`, `net`, `io-util`, `time`, `sync`, `signal`, `macros`, plus
  `fs` under Churust's own `fs` feature. If you relied on a tokio feature
  arriving transitively, declare `tokio` yourself with the features you need;
  Cargo unifies them.
- `#[churust::main]` expands to `::churust::__private::tokio` rather than
  `::tokio`. Invoking the macro directly from `churust-macros` is no longer
  supported; depend on `churust`.

### Breaking

- `Match` gained a `BadPath` variant and is now `#[non_exhaustive]`, so a
  `match` on `churust_core::Match` needs a `_` arm. This affects code driving
  `Router` directly; applications built on the routing DSL are unaffected.

## [0.1.1] - 2026-07-25

### Fixed

- `churust-core`: removed a redundant explicit link target in the `ws.rs` docs.
  It broke `cargo doc` under `-D warnings`, which meant docs failed to build
  under a denying rustdoc configuration.

No API changes. `0.1.1` is a drop-in replacement for `0.1.0`.

## [0.1.0] - 2026-07-25

First public release.

### Added

- **Engine and routing** — application engine over tokio + hyper (HTTP/1.1),
  a trie router with path parameters, and a routing DSL.
- **Hybrid handlers** — call-style (`|call: Call|`) or typed extractors
  (`Path<T>`, `Query<T>`, `State<T>`, `Json<T>`, `BearerToken`, `Principal<P>`),
  mixable in a single handler. `FromCallParts` borrows in any position;
  `FromCall` consumes the body in the last position.
- **Phased pipeline** — `install(plugin)` composing middleware over a named,
  deterministic order: `Setup < Monitoring < Plugins < Call < Fallback`.
- **Plugins** — JSON content negotiation (`json`), request logging over
  `tracing` (`logging`), CORS (`cors`), and Bearer/Basic/JWT authentication
  (`auth`). `full` enables all four.
- **Typed state** — `.state(T)` and the `State<T>` extractor.
- **Layered config** — defaults < `churust.toml` < `CHURUST_*` env < code DSL.
- **Streaming bodies** — the always-on `Body` type, buffered bytes or a lazy
  stream.
- **Static files** (`fs`) — `StaticFiles` with MIME detection, optional
  directory index, chunked streaming, and rejection of path traversal via `..`,
  absolute paths, and symlink escapes.
- **WebSockets** (`ws`) — `WebSocketUpgrade` extractor and `on_upgrade`.
- **TLS** (`tls`) — rustls-backed HTTPS.
- **`#[churust::main]`** — entrypoint macro.
- **Test harness** — `TestClient` drives the full pipeline in-process without
  binding a socket.
- **Secure defaults** — body-size limits, request timeouts, panic isolation (a
  panicking handler returns 500 rather than killing the server), and no version
  banner.

[Unreleased]: https://github.com/davthecoder/Churust/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/davthecoder/Churust/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/davthecoder/Churust/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/davthecoder/Churust/releases/tag/v0.1.0
