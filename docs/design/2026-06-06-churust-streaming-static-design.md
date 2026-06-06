# Churust Streaming Responses + Static Files — Design Spec (v2.1)

**Date:** 2026-06-06
**Status:** Approved (design), pending implementation plan
**Builds on:** Churust v1 + v2.0 (WebSockets), all green.

## 1. Summary

Replace Churust's buffered-only response body with a `Body` type that is either
buffered (`Bytes`) or a lazy stream, then add a `StaticFiles` handler (behind an
`fs` feature) that streams files from a directory. This removes the framework's
biggest remaining limitation — responses had to be fully materialized in memory —
and delivers table-stakes static-asset serving. Streaming also unblocks future
work (SSE, large downloads, streaming compression).

## 2. Goals & non-goals

### Goals
- A `Body` enum (`Bytes` | `Stream`) on `Response`, with `From` conversions so
  every existing constructor/`IntoResponse` impl compiles unchanged.
- The engine emits a boxed body that is `Full` for buffered or `StreamBody` for
  streamed responses.
- `StaticFiles::dir(...)` — a directory-serving handler with path-traversal
  protection, content-type detection, optional index file, and 404 handling.
- Streaming uses **no new dependencies** (`http-body-util`, `futures-util`,
  `bytes`, `tokio` are already present).
- `TestClient` transparently collects streamed bodies so existing test
  ergonomics (`.text()`, `.body_bytes()`) keep working.

### Non-goals (YAGNI for v2.1)
- Server-Sent Events helper (trivial follow-on once `Body::from_stream` exists)
- HTTP `Range` / byte-range requests
- ETag / `Last-Modified` / conditional GET
- Directory listing pages
- A `mime_guess` dependency (a small built-in extension→type map suffices)

## 3. Decisions (locked)

| Decision | Choice |
|----------|--------|
| Body shape | `enum Body { Bytes(Bytes), Stream(BoxStream<'static, Result<Bytes>>) }` on `Response.body` |
| Engine output | `BoxBody<Bytes, std::io::Error>` (Full for buffered, StreamBody for streamed) |
| Static files | `fs` feature in **`churust-core`**; built-in MIME map; no new deps |
| File streaming | hand-rolled async read-loop → `futures::stream` (no `tokio-util` dependency) |
| Streaming availability | `Body` is **always-on** (foundational, no deps); only `StaticFiles` is feature-gated |

## 4. Architecture

### 4.1 `Body` (core, always-on) — `churust-core/src/body.rs`
```rust
pub enum Body {
    Bytes(Bytes),
    Stream(Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + 'static>>),
}
```
- Constructors: `Body::empty()`, `Body::from(Bytes)`, `From<String>`, `From<&'static str>`, `From<Vec<u8>>`, `Body::from_stream(s)` where `s: Stream<Item = Result<Bytes, E>>` with `E: Into<Box<dyn Error>>` (mapped to `crate::Error` or `io::Error`).
- Accessors: `as_bytes(&self) -> Option<&Bytes>` (Some only when buffered),
  `is_stream(&self)`, `async into_bytes(self) -> Result<Bytes>` (returns the
  buffer directly, or collects the stream).
- `Default` = `Body::empty()`. `Debug` prints a placeholder for the stream arm.

### 4.2 `Response` change — `churust-core/src/response.rs`
- `Response.body: Bytes` → `Response.body: Body`.
- `Response::text/bytes/new/with_status/with_header` unchanged in signature; body
  is set via `Body::from(...)`.
- Add `Response::stream(content_type, s)` and `Response::body(Body)` helpers.
- Every `IntoResponse` impl keeps producing `Response` (now with a `Body`); the
  `Bytes`/`String`/`&str` cases use `From` so they read identically.

### 4.3 Engine — `churust-core/src/engine.rs`
- `handle` returns `HyperResponse<BoxBody<Bytes, std::io::Error>>` instead of
  `Full<Bytes>`.
- Convert: `Body::Bytes(b) => Full::new(b).map_err(|never| match never {}).boxed()`;
  `Body::Stream(s) => StreamBody::new(s.map_ok(Frame::data).map_err(to_io)).boxed()`.
- The `PAYLOAD_TOO_LARGE` short-circuit body is also boxed the same way.
- Request body-size limit and request timeout are unchanged (they apply to
  request reading; a long streamed *response* is not subject to the request
  timeout).

### 4.4 Contained ripple
- **JSON `ContentNegotiation`** (`churust-json`): only rewrites error responses
  whose `body.as_bytes()` is `Some` (buffered text). Streamed bodies pass through
  untouched. One-line change to read `res.body.as_bytes()` instead of `&res.body`.
- **`TestClient`** (`churust-core/src/test.rs`): `send()` calls
  `response.body.into_bytes().await` and stores the collected `Bytes`, so
  `TestResponse::text()/body_bytes()` are unchanged for callers.
- **WebSockets** (`churust-core/src/ws.rs`): the 101 response uses
  `Body::empty()` (was an empty `Bytes`).

### 4.5 Static files (`fs` feature) — `churust-core/src/fs.rs`
```rust
pub struct StaticFiles { root: PathBuf, index: Option<String> }
impl StaticFiles {
    pub fn dir(root: impl Into<PathBuf>) -> Self;
    pub fn index(self, file: impl Into<String>) -> Self;
    pub fn handler(self) -> impl Handler;   // for r.get("/assets/{path...}", files.handler())
}
```
- The handler reads the matched `{path...}` wildcard (`call.param_raw` / a
  `Tail` accessor), normalizes it, and **rejects traversal**: any component
  equal to `..`, absolute paths, or paths escaping `root` → `404 Not Found`
  (not 403, to avoid revealing structure).
- Resolves to a file under `root`; if the resolved path is a directory and an
  `index` is set, serves `root/<dir>/<index>`.
- Missing file → `404`. Found → `200` with a streamed `Body` and a
  `Content-Type` from the built-in extension map (fallback
  `application/octet-stream`).
- File streaming: open `tokio::fs::File`, wrap in a `futures::stream::unfold`
  read-loop yielding `Result<Bytes>` chunks (e.g. 64 KiB), build `Body::Stream`.
  No `tokio-util` dependency.
- Built-in MIME map: `html, htm, css, js, mjs, json, wasm, txt, csv, xml, svg,
  png, jpg, jpeg, gif, webp, ico, woff, woff2, ttf, pdf, mp4, mp3` →
  reasonable types; default `application/octet-stream`.

### 4.6 Umbrella + examples
- `churust` umbrella: feature `fs = ["churust-core/fs"]`; re-export `StaticFiles`
  and `Body` (Body is always available; re-export at root + prelude). Prelude
  adds `StaticFiles` under `#[cfg(feature = "fs")]`.
- Extend an example (or add `examples/static`) serving a `./public` dir plus a
  streamed dynamic response, to demonstrate both.

## 5. Data flow (static file)
```
GET /assets/css/app.css
  └─ router matches "/assets/{path...}" → param path = "css/app.css"
  └─ StaticFiles handler: sanitize → root/css/app.css ; exists? open File
        └─ Body::Stream(unfold read-loop, 64 KiB chunks)
        └─ Response 200, Content-Type text/css, body = stream
  └─ engine: Body::Stream → StreamBody → BoxBody → hyper streams frames to client
```

## 6. Error handling
- Path traversal / missing file / not-a-file → `404` (rendered as the normal
  error response; JSON if the `json` plugin is installed).
- File open/read errors mid-stream surface as a stream error → the connection
  terminates; logged if logging is installed. Documented.
- `Body::into_bytes()` on a failing stream returns `Err(crate::Error)`.

## 7. Security
- Path traversal is rejected before any filesystem access (component check +
  canonical-prefix check against `root`).
- Symlink escape: after resolving, verify the canonicalized path still starts
  with the canonicalized `root`; otherwise 404.
- Request body limit + timeout unchanged. Streamed responses are intentionally
  not bounded by the request timeout.

## 8. Testing
- **`Body` units:** buffered round-trip; `from_stream` collected via
  `into_bytes`; `as_bytes` returns `None` for a stream, `Some` for buffered;
  `empty` is empty.
- **Engine integration:** a route returning a streamed `Body` (built from an
  in-memory chunk stream) is read in full by a real HTTP client over a socket;
  bytes match.
- **`TestClient`:** a streamed response is collected so `.text()` returns the
  full content.
- **Static files (`fs`):** create a temp dir with files in the test; serve →
  `200` + correct `Content-Type` + exact bytes; missing → `404`; `..` traversal
  → `404`; directory + index → serves index.
- **Default build untouched:** `fs` is feature-gated; `Body` change keeps all
  existing tests/clippy green.

## 9. Build order (detailed in the plan)
1. `Body` type + unit tests.
2. Swap `Response.body` to `Body`; update constructors/`IntoResponse`; add
   `stream`/`body` helpers; fix in-crate readers (none beyond response itself yet).
3. Engine: emit `BoxBody`; convert buffered/streamed.
4. Fix ripple: JSON middleware (`as_bytes`), `TestClient` (collect), ws
   (`Body::empty`).
5. Engine streaming integration test + `TestClient` streamed-collect test.
6. `fs` feature: MIME map + `StaticFiles` + traversal protection + file stream
   (+ tests).
7. Umbrella `fs` feature + prelude + example; full-suite + clippy matrix
   (default, `fs`, `full`, `ws`).
