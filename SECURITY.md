# Security Policy

Churust sits directly on the network edge — it terminates TLS, parses request
paths, serves files from disk, and validates auth tokens. Bugs in those paths
are security bugs, and we treat them that way.

## Supported versions

All seven Churust crates share a single version number and are released
together. Only the latest published version is supported; fixes ship forward as
a new release, not as patches to old ones.

| Version | Supported |
| --- | --- |
| 0.3.x | yes |
| < 0.3 | no |

While Churust is pre-1.0, a security fix may land in a minor release with a
breaking change if that is what correctness requires.

## Reporting a vulnerability

**Do not open a public issue for a security bug.**

Report privately through GitHub:

1. Go to the [Security tab](https://github.com/davthecoder/Churust/security)
2. Report a vulnerability
3. Fill in the advisory form

If you can't use that, email **david.cruz@davthecoder.com** with `[SECURITY]` in
the subject.

Useful things to include: the affected crate and version, which features were
enabled, a reproduction (a failing test is ideal), what an attacker gets out of
it, and any suggested fix.

## What to expect

| | Target |
| --- | --- |
| Acknowledgement | 48 hours |
| Initial assessment | 7 days |
| Fix released | 30 days for high severity; sooner for anything actively exploitable |

You'll be credited in the advisory unless you'd rather not be. If a report turns
out not to be a vulnerability, we'll explain why rather than just closing it.

Please give us a reasonable window to ship a fix before disclosing publicly.

## Areas worth your attention

If you're looking for somewhere to point a fuzzer, these are the parts where a
bug has real consequences:

- **Static files** (`fs` feature) — path traversal, `..` handling, absolute
  paths, symlinks escaping the served root, and encoded separators (`%2F`,
  `%5C`), which are refused outright so the rejoined wildcard value cannot be
  reinterpreted
- **Range requests** (`fs` feature) — `Range` parsing arithmetic, seek offsets
  reading outside the intended span, and `416` responses disclosing more than
  the entity length
- **Conditional requests** (`fs` feature) — `ETag` comparison, `If-Match` and
  `If-None-Match` precedence, and whether a `304` can be induced for content
  the client should not have
- **Auth** (`churust-auth`) — JWT validation, algorithm confusion, Basic
  credential parsing, timing in comparisons. Use
  [`churust::secure_compare`] for any request-supplied value checked against a
  secret; a plain `==` leaks how much of a guess was correct
- **CORS** (`churust-cors`) — origin reflection, permissive preflight
  responses. `Cors::allow_any_origin_insecure` reflects every origin by design;
  reports that it is permissive are not findings, reports that a *restrictive*
  configuration leaks are
- **WebSockets** (`ws` feature) — handshake validation, accept-key computation,
  resource exhaustion on upgrade, and whether the frame and message caps can be
  bypassed by fragmentation
- **TLS** (`tls` feature) — certificate loading, configuration defaults, and
  ALPN negotiation
- **HTTP/2** — frame handling is hyper's, but report anything where Churust's
  configuration exposes it badly: a request that behaves differently over h2
  than over HTTP/1.1, or a way past `h2_max_concurrent_streams` /
  `h2_max_header_list_size`. `max_headers` bounds HTTP/1 only
- **Router** — path normalization, percent-decoding, parameter extraction, and
  the path-segment cap. `PathPolicy` decides what happens to a path with
  repeated slashes; under the default `Strict` it is refused. Any spelling that
  reaches a handler it should not — particularly one that slips past a
  prefix-matching middleware — is in scope, as is any way to make `%2F` act as
  a separator
- **Message framing** — a request bearing both `Transfer-Encoding` and
  `Content-Length` does not keep its connection: below hyper 1.11 Churust
  refuses it with `400`, and from 1.11 hyper removes the `Content-Length`,
  frames by the transfer coding and closes afterwards. A framing disagreement
  that survives that — above all a connection that stays open — or any other way
  to desynchronise a connection shared with an upstream proxy, is a report worth
  making
- **Connection limits** — `max_connections`, `max_tls_handshakes` and
  `tls_handshake_timeout_ms`. A way to hold connections or handshake slots
  without consuming a permit, or to make the accept loop spin, is in scope
- **Graceful shutdown** — whether a connection can outlive the drain, or hold
  the process past `shutdown_timeout_ms`
- **Body handling** — request size limits, streaming back-pressure
- **Request limits** — header count, the header read timeout that bounds
  slow-loris, the idle keep-alive timeout, and any way to hold a connection open
  past either
- **Cookies and sessions** — the cookie session store signs with HMAC-SHA256 and
  compares in constant time. A forged signature for a chosen payload, or a way
  to make the store accept one, is in scope. "The payload is readable" is not:
  the store signs rather than encrypts, and says so
- **Multipart** (`multipart` feature) — boundary parsing, part-count and body
  caps, and whether a crafted body can make the parser allocate out of
  proportion to its size
- **Streaming bodies** — whether `Payload` can be used to bypass the size cap
  (it carries the per-route limit as well as the server-wide one), and whether
  a slow or never-ending body ties up a connection past the request timeout
- **Directory listing** (`fs` feature, opt-in) — filename escaping in the
  generated HTML, and whether a listing reveals anything outside the root
- **Unix sockets** — the socket file's permissions are the access control; a
  stale file is unlinked before binding
- **Error rendering** — whether a user error type implementing `IntoError` can
  be made to disclose its `Display` text, which is deliberately *not* the
  default client-facing message
- **The `tower` adapter** (`tower` feature) — whether a `Layer` can be made to
  run the inner service twice, observe another request's continuation, or leak
  state between requests

## Out of scope

- Vulnerabilities in upstream dependencies — report those upstream (tokio,
  hyper, rustls, jsonwebtoken); tell us too if Churust's usage makes it worse
- Anything requiring the attacker to already control the server process
- Denial of service through unbounded resources that the application is
  expected to configure, unless the default is itself unsafe
- Findings from automated scanners with no demonstrated impact
