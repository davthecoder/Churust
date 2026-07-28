# Logging & request IDs

<span class="feature-tag">feature: logging</span>

## Enable

```toml
churust = { version = "0.3", features = ["logging"] }
```

## Install

```rust
.install(CallLogging::new())
```

`CallLogging` uses the `tracing` ecosystem for structured request logs.

## Request correlation

Churust attaches a **request id** to every log line, echoes it as
`x-request-id`, and continues an inbound W3C `traceparent` when present. That
behavior is part of the core request path; pair it with `CallLogging` for
readable structured logs in development and production.

## Tips

- Configure a `tracing` subscriber in `main` (or your process bootstrap) so logs
  actually appear.
- Prefer request ids over grepping timestamps when correlating client and
  server logs.

## API reference

- [docs.rs/churust-logging](https://docs.rs/churust-logging)
