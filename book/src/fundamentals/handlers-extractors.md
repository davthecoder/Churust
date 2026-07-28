# Handlers & extractors

A handler is an `async` closure or function that returns anything implementing
`IntoResponse`. Arguments are **extractors**.

## Two styles

**Call-style** — take the whole `Call`:

```rust
r.get("/", |_call: Call| async { "ok" });
```

**Extractor-style** — declare only what you need:

```rust
r.get(
    "/greet",
    |Query(s): Query<Search>, g: State<Greeter>| async move {
        format!("{}, {}!", g.prefix, s.q)
    },
);
```

You can mix both styles across the app freely.

## Extractor categories

### `FromCallParts` (request head)

Borrow the request head. Usable in **any** argument position:

| Extractor | Purpose |
| --- | --- |
| `Path<T>` | Path parameters |
| `Query<T>` | Query string |
| `Header<T, N>` | Named header |
| `State<T>` | Typed app state |
| `BearerToken` | Raw bearer token string |
| `Principal<P>` | Authenticated principal (`auth` feature) |

### `FromCall` (request body)

Consume the body. **Last argument only** — the body is a one-shot stream:

| Extractor | Purpose |
| --- | --- |
| `Json<T>` | JSON body (`json` feature) |
| `Form<T>` | Form body |
| `Bytes` / `String` | Raw body |
| `Payload` | Streaming body (not capped by materializing whole upload) |
| `Multipart` / stream variants | Uploads (`multipart` feature) |
| `Either<A, B>` | One of two body shapes |
| `Call` | Full call (also body-capable) |

The split is enforced by the **compiler**, not convention: two body-consuming
arguments do not compile. Every `FromCallParts` type is also usable last.

## Optional extractors

Wrap any extractor that implements `OptionalFromCallParts` in `Option<T>`
(`Query`, `Path`, `Header`, `BearerToken`, …):

- **Absent** → `None`
- **Malformed** → still an **error**

A typo’d query string does not quietly become a default.

## Hybrid example

```rust
r.post(
    "/notes",
    |Principal(_admin): Principal<AdminUser>,
     s: State<Store>,
     Json(input): Json<NewNote>| async move {
        // ...
        (StatusCode::CREATED, Json(note))
    },
);
```

Here auth and state come from parts; the JSON body is last.

## Handler return types

Anything `IntoResponse`:

- `&str` / `String` / `Bytes`
- `StatusCode`
- `(StatusCode, T)`
- `Json<T>` (feature `json`)
- `Response` (fully controlled)
- Streaming via `Response::stream` and `Body::from_stream`

## Errors in handlers

Implement `IntoError` on your error type so `?` works in handlers without
leaking internal `Display` text to clients. See
[Responses & errors](responses-errors.md).

## Next

→ [Responses & errors](responses-errors.md)
