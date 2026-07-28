# Authentication

<span class="feature-tag">feature: auth</span>

## Enable

```toml
churust = { version = "0.3", features = ["auth"] }
```

## Model

1. Install an `Auth` plugin that verifies credentials and produces a
   **principal** type `P`.
2. Handlers that need authentication take `Principal<P>`.
3. If verification failed or the header was missing, the handler never runs —
   the client gets `401`. Auth is enforced by the **type system**, so you cannot
   “forget” the check on a protected route.

## Bearer

```rust
#[derive(Clone)]
struct Admin { name: String }

.install(Auth::bearer(|token: String| async move {
    (token == "admin-token").then(|| Admin { name: "admin".into() })
}))
.routing(|r| {
    r.get("/me", |Principal(u): Principal<Admin>| async move { u.name });
})
```

```bash
curl -H 'authorization: Bearer admin-token' localhost:8080/me
```

### Production warning

Do **not** compare tokens with `==` against a hard-coded secret in real apps.
Use [`churust::secure_compare`](https://docs.rs/churust) (or constant-time
compare against a store) so timing does not leak how much of a guess was right.

## Basic

```rust
.install(Auth::basic(|user, pass| async move {
    // Only over TLS — Basic is reversibly encoded, not encrypted.
    verify_user(user, pass).await
}))
```

## JWT

`Auth::jwt(...)` validates JWTs and exposes claims as the principal. See
[docs.rs/churust-auth](https://docs.rs/churust-auth) for the builder and claim
types.

## Login / logout layer

Churust also provides an identity layer over sessions with absolute and idle
deadlines. Pair with cookie sessions or [Redis](redis.md) when you need
server-side revocation.

## Example

Full JSON CRUD with bearer auth:
[`examples/api`](https://github.com/davthecoder/Churust/tree/main/examples/api)
and the [JSON API recipe](../recipes/json-api.md).

## API reference

- [docs.rs/churust-auth](https://docs.rs/churust-auth)
