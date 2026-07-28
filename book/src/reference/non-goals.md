# What is not supported

Deliberate scope choices and known gaps. Tracked in detail in the
[production-readiness audit](https://github.com/davthecoder/Churust/blob/main/docs/design/2026-07-25-production-readiness-audit.md)
and
[roadmap](https://github.com/davthecoder/Churust/blob/main/docs/design/2026-07-25-roadmap-to-parity.md).

## On purpose

| Item | Why |
| --- | --- |
| **Actor integration** | Out of framework scope |
| **`multipart/byteranges`** | Multi-range responses; RFC 9110 allows omitting |
| **WebSockets over HTTP/3** | Would need Extended CONNECT (RFC 9220); `ws` is HTTP/1.1 upgrade |
| **Revocation from client-only sessions** | No server-side record — use `churust-redis` |
| **Unlimited body via streaming** | Streaming lowers memory cost; `max_body_bytes` still applies |

## Pre-1.0

The public API may break in minor releases until 1.0. Pin versions in production
and read the [changelog](changelog.md).

## Want something sooner?

Make the case in
[Discussions → Ideas](https://github.com/davthecoder/Churust/discussions/categories/ideas).
