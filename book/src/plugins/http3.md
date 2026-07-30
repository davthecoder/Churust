# HTTP/3

<span class="feature-tag">feature: http3</span>

Implies `tls` / QUIC credentials.

## Enable

```toml
churust = { version = "0.3", features = ["http3"] }
```

## What you get

HTTP/3 over **QUIC** on its own listener. Use `advertise_http3` so responses can
emit the **`Alt-Svc`** header clients need in order to try h3.

## Setup sketch

1. Enable the feature and provide TLS certificates suitable for QUIC.  
2. Configure the h3 listener (bind address + server config from PEM helpers in
   core).  
3. Enable advertisement so browsers/clients discover the endpoint.

Exact builder methods live on docs.rs for your version
(`server_config_from_pem`, h3 bind helpers).

## Which `[server]` knobs apply

Enabling `http3` starts a second listener, and a setting that reached only the
TCP one would be a bound you believed you had. These all apply here:

| Key | On the HTTP/3 listener |
| --- | --- |
| `max_connections` | Concurrent QUIC connections |
| `max_tls_handshakes` | Concurrent QUIC handshakes — TLS 1.3 either way |
| `tls_handshake_timeout_ms` | Handshake deadline, queue wait included |
| `keep_alive_ms` | QUIC idle timeout (see below for `0`) |
| `h2_max_concurrent_streams` | Request streams per connection; `0` unlimited |
| `h2_max_header_list_size` | h3 `max_field_section_size` |
| `header_read_timeout_ms` | Per-stream wait for a HEADERS frame |
| `request_timeout_ms` | Body read plus handler, as one budget |
| `max_body_bytes` | Request body cap |

Two things behave differently here, both deliberately:

- **`keep_alive_ms = 0`** has no QUIC meaning — a connection multiplexes streams
  and cannot close after one response — so the listener keeps the default
  `75000` ms idle bound instead.
- **A request body is collected before dispatch**, where the TCP path streams it.
  That is what lets a truncated body be refused before a handler ever runs on a
  fragment, which is undetectable afterwards because a truncated body is just a
  shorter body. The cost is one `max_body_bytes` per in-flight request.

`serve` reads these from the app. If you build a `quinn::ServerConfig` yourself
and use `serve_with_config`, the transport parameters are yours — the per-stream
deadlines still apply.

## Limits

WebSockets over HTTP/3 (RFC 9220 Extended CONNECT) are **not** implemented; the
`ws` feature uses the HTTP/1.1 upgrade handshake.

## API reference

- [docs.rs/churust-core](https://docs.rs/churust-core) `http3` module
