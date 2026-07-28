# Configuration keys

## Loading

```rust
Churust::from_config() // defaults < churust.toml < env < later DSL
```

Environment variables use the `CHURUST_` prefix with nested keys separated by
`_`. Example: `CHURUST_SERVER_PORT=9090`.

## `[server]`

| Key | Example | Meaning |
| --- | --- | --- |
| `host` | `"0.0.0.0"` | Bind address |
| `port` | `8080` | Port |
| `max_body_bytes` | `1048576` | Max request body size |
| `request_timeout_ms` | `30000` | Request timeout |
| `keep_alive_ms` | `75000` | Idle keep-alive; `0` answers and closes |
| `max_connections` | `25000` | Concurrent connections; `0` unlimited |
| `max_tls_handshakes` | `256` | Concurrent TLS handshakes |
| `tls_handshake_timeout_ms` | `10000` | Handshake (+ queue) deadline |
| `shutdown_timeout_ms` | `30000` | Bounded drain on shutdown |
| `path_policy` | `"strict"` | `strict` \| `redirect` \| `collapse` |

Header-read timeouts cover a connection from accept time, so a peer that
connects and stays silent is still bounded (slow-loris protection).

## `[tls]`

Requires the `tls` feature.

| Key | Meaning |
| --- | --- |
| `cert` | Path to certificate PEM |
| `key` | Path to private key PEM |

## Example file

```toml
[server]
host = "0.0.0.0"
port = 8080
max_body_bytes = 1048576
request_timeout_ms = 30000
keep_alive_ms = 75000
max_connections = 25000
max_tls_handshakes = 256
tls_handshake_timeout_ms = 10000
shutdown_timeout_ms = 30000
path_policy = "strict"

[tls]
cert = "cert.pem"
key  = "key.pem"
```

Additional knobs (HTTP/2 limits, multi-bind, Unix sockets, backlog) may be
available on the builder — see [docs.rs/churust-core](https://docs.rs/churust-core)
and [State & configuration](../fundamentals/state-config.md).
