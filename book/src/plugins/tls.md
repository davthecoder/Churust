# TLS

<span class="feature-tag">feature: tls</span>

## Enable

```toml
churust = { version = "0.3", features = ["tls"] }
```

## Config file

```toml
# churust.toml
[tls]
cert = "cert.pem"
key  = "key.pem"
```

```rust
Churust::from_config()
    .routing(|r| { /* ... */ })
    .start()
    .await
```

## Programmatic

Use the core helper to build a rustls acceptor from PEM files (see
`acceptor_from_pem` on docs.rs) and the builder’s TLS setters for your version.

## Knobs

| Setting | Meaning |
| --- | --- |
| `max_tls_handshakes` | Concurrent handshakes (asymmetric work) |
| `tls_handshake_timeout_ms` | Bounds queueing **and** the handshake itself |

HTTP/2 over TLS is negotiated via ALPN. Plaintext h2c uses prior knowledge.

## Production tips

- Prefer a reverse proxy (Caddy, nginx, Traefik) for cert automation when that
  fits your deploy model — or terminate TLS in-process with rustls when you
  need end-to-end in one binary.
- Never enable HTTP Basic auth without TLS.

## Related

- [HTTP/3](http3.md) (requires TLS material for QUIC)
- [Deployment](../recipes/deployment.md)
