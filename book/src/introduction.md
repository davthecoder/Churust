<img class="hero-logo" src="https://raw.githubusercontent.com/davthecoder/Churust/main/img/churust_thumb_logo.png" alt="Churust logo — a churro spiral inside a gear" />

# Churust 🌀

**Churro + Rust** — a backend web framework inspired by [Ktor](https://ktor.io).
Simple, secure, robust, and easy to learn.

<div class="badge-row">

[![crates.io](https://img.shields.io/crates/v/churust.svg)](https://crates.io/crates/churust)
[![docs.rs](https://docs.rs/churust/badge.svg)](https://docs.rs/churust)
[![CI](https://github.com/davthecoder/Churust/actions/workflows/ci.yml/badge.svg)](https://github.com/davthecoder/Churust/actions/workflows/ci.yml)
![MSRV 1.96](https://img.shields.io/badge/rust-1.96%2B-blue.svg)

</div>

Churust gives you Ktor’s developer experience in Rust: an application engine, a
routing DSL, an `install(plugin)` system, and a phased interceptor pipeline —
built on a battle-tested async stack (**tokio + hyper + rustls**). Churust owns
the ergonomic layer; it does not reinvent HTTP parsing or TLS.

Coming from Kotlin/Ktor? See **[From Ktor to Churust](getting-started/from-ktor.md)**
for side-by-side examples (routes, `install`, JSON, auth, config, tests).

```rust
use churust::prelude::*;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .port(8080)
        .routing(|r| {
            r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
            r.get("/users/{id}", |Path(id): Path<u64>| async move {
                format!("user #{id}")
            });
        })
        .start()
        .await
}
```

## What you get

| Area | Highlights |
| --- | --- |
| **Handlers** | Call-style *or* typed extractors (`Path`, `Query`, `Json`, `State`, …) |
| **Plugins** | `install(...)` into a fixed phase order: Setup → Monitoring → Plugins → Call → Fallback |
| **Security** | Body limits, timeouts, panic isolation, security headers, opt-in TLS |
| **Protocols** | HTTP/1.1, HTTP/2, optional HTTP/3, WebSockets, static files |
| **Ecosystem** | Opt-in JSON, CORS, auth, rate limit, compression, templates, Redis, OpenAPI, client |

## How this site is organized

| Section | Purpose |
| --- | --- |
| [Getting started](getting-started/quick-start.md) | Install, run your first server |
| [From Ktor to Churust](getting-started/from-ktor.md) | Side-by-side Ktor (Kotlin) ↔ Churust (Rust) |
| [Fundamentals](fundamentals/routing.md) | Routing, extractors, plugins, config, tests |
| [Plugins & features](plugins/overview.md) | One page per optional capability |
| [Recipes](recipes/json-api.md) | End-to-end examples from the repo |
| [Reference](reference/feature-flags.md) | Matrices, config keys, security, non-goals |
| [Community](community/support.md) | Contributing and help channels |

## Docs map

| Resource | Use it for |
| --- | --- |
| **This guide** | How to use Churust day to day |
| [docs.rs/churust](https://docs.rs/churust) | Full API reference for types and traits |
| [examples/](https://github.com/davthecoder/Churust/tree/main/examples) | Runnable apps (`hello`, `api`, `chat`, `static`) |
| [README](https://github.com/davthecoder/Churust#readme) | Short pitch and install blurb |

## Status

Published on [crates.io](https://crates.io/crates/churust). Pre-1.0: the API is
settling rather than settled — expect breaking changes in minor releases until
1.0. Current series documented here: **0.3**.

MSRV: **Rust 1.96+**.

## Next step

→ [Quick start](getting-started/quick-start.md)
