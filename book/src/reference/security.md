# Security defaults

Churust aims to be **secure by default**. This page summarizes guarantees you
get without turning on optional features.

## Always on

| Control | Behavior |
| --- | --- |
| Security headers | Applied on every response, including transport-written `413` / `400` / `408` |
| Body size limits | `max_body_bytes` bounds every request |
| Request timeouts | Bound handler work |
| Header-read timeouts | From **accept**, not only after protocol selection (slow-loris) |
| Header / path-depth caps | Limit parser work |
| Panic isolation | Panicking handler → `500`; process keeps serving |
| Version banner | Not advertised |
| Path policy | Default `strict` refuses non-canonical paths |

## Cookies

Cookie helper defaults: **`HttpOnly`**, **`SameSite=Lax`**. Prefer signed
sessions; use Redis when revocation must be real.

## Secrets

Use `secure_compare` for token/password compares. Never use plain `==` on
secrets in production examples you copy forward.

## Auth

- Bearer / Basic / JWT behind `auth`  
- Basic **only over TLS**  
- `Principal<P>` so protected handlers cannot forget the check  

## CORS

Default builder is restrictive (no origins). Wildcard
`allow_any_origin_insecure` is explicit and uncomfortable on purpose.

## TLS / HTTP

- Opt-in rustls TLS  
- Ambiguous `Transfer-Encoding` + `Content-Length` refused  
- WebSocket frame/message caps when `ws` is enabled  

## Static files

Traversal and symlink escape → 404. Missing root fails at startup.

## What security is not

Churust does not replace:

- Application authorization design  
- Secrets management  
- Dependency auditing (`cargo audit`)  
- Network policy / WAF  

See also [What is not supported](non-goals.md).
