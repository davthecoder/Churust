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
| `max_tls_handshakes` | `256` | Concurrent TLS handshakes (QUIC included) |
| `tls_handshake_timeout_ms` | `10000` | Handshake (+ queue) deadline |
| `shutdown_timeout_ms` | `30000` | Bounded drain on shutdown |
| `path_policy` | `"strict"` | `strict` \| `redirect` \| `collapse` |

Header-read timeouts cover a connection from accept time, so a peer that
connects and stays silent is still bounded (slow-loris protection). Over HTTP/3
the same deadline bounds each request stream's wait for its HEADERS frame.

### Which transports a knob reaches

One setting should mean one thing on every transport this server speaks, so
unless noted below a `[server]` key applies to HTTP/1.1, HTTP/2 and HTTP/3 alike.
Two deliberate differences are worth knowing:

- **`keep_alive_ms = 0`** means "answer and close". HTTP/1.1 closes after the
  response and HTTP/2 closes once nothing is in flight. A QUIC connection
  multiplexes streams and cannot be closed after a single response, so there is
  nothing for the setting to mean there — the HTTP/3 listener keeps the default
  `75000` ms idle bound rather than never expiring, which would leave a
  connection holding a `max_connections` permit for good.
- **`h2_max_concurrent_streams`** is named for HTTP/2 and also caps concurrent
  request streams per HTTP/3 connection; `0` is unlimited on both.

An HTTP/3 request body is collected before the request is dispatched, where the
TCP path streams it to the handler. That is what lets a body truncated mid-flight
be refused *without the handler running* on a fragment, and the cost is one
`max_body_bytes` per in-flight request — bounded by
`h2_max_concurrent_streams × max_connections`.

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
