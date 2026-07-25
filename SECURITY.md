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
| 0.1.x | yes |
| < 0.1 | no |

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
  paths, symlinks escaping the served root
- **Auth** (`churust-auth`) — JWT validation, algorithm confusion, Basic
  credential parsing, timing in comparisons
- **CORS** (`churust-cors`) — origin reflection, permissive preflight responses
- **WebSockets** (`ws` feature) — handshake validation, accept-key computation,
  resource exhaustion on upgrade
- **TLS** (`tls` feature) — certificate loading and configuration defaults
- **Router** — path normalization, percent-decoding, parameter extraction
- **Body handling** — request size limits, streaming back-pressure

## Out of scope

- Vulnerabilities in upstream dependencies — report those upstream (tokio,
  hyper, rustls, jsonwebtoken); tell us too if Churust's usage makes it worse
- Anything requiring the attacker to already control the server process
- Denial of service through unbounded resources that the application is
  expected to configure, unless the default is itself unsafe
- Findings from automated scanners with no demonstrated impact
