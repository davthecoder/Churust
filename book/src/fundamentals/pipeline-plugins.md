# Pipeline & plugins

Churust borrows Ktor’s mental model: **plugins** install **middleware** into a
named **pipeline**.

## Phases

Phase order is fixed and deterministic, regardless of install order:

1. **Setup**
2. **Monitoring**
3. **Plugins**
4. **Call**
5. **Fallback**

Middleware owns the `Call`, may mutate it, call `next.run(call)`, and
post-process the `Response` (onion model).

## Installing plugins

```rust
Churust::server()
    .install(CallLogging::new())
    .install(ContentNegotiation::new())
    .install(Cors::new().allow_origin("http://localhost:3000"))
    .install(Auth::bearer(|token: String| async move {
        (token == "admin-token").then(|| Admin { name: "admin".into() })
    }))
    .routing(|r| { /* ... */ })
```

Order of `.install(...)` can still matter for relative stacking *within* a
phase, but you never get a surprise reordering across phases.

## Built-in plugins (feature-gated)

| Plugin | Feature | Role |
| --- | --- | --- |
| `CallLogging` | `logging` | Structured request logs |
| `ContentNegotiation` | `json` | JSON errors / negotiation |
| `Cors` | `cors` | CORS + preflight |
| `Auth` (Bearer / Basic / JWT) | `auth` | Authentication |
| `RateLimit` | `ratelimit` | GCRA rate limiting |
| `Compression` | `compression` | Response compression |
| `Templates` | `templates` | Template engine setup |

See [Plugins & features](../plugins/overview.md) for one page per capability.

## Scoped middleware

Use `intercept` when middleware should apply only to part of the route tree
rather than the whole application.

## Tower interoperability

With the `tower` integration path, you can run any `tower::Service` as
middleware so `tower-http` layers work without Churust reimplementing them.

## Writing your own plugin

Implement the `Plugin` trait: register one or more `Middleware` values into the
phase that matches their job (logging → Monitoring, auth → Plugins, etc.). Keep
side effects explicit and prefer failing at **startup** when configuration is
invalid.

## Next

→ [State & configuration](state-config.md)
