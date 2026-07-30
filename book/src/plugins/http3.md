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
| `h2_max_concurrent_streams` | Request streams per connection (see below for `0`) |
| `h2_max_header_list_size` | h3 `max_field_section_size` |
| `request_timeout_ms` | Body read plus handler, as one budget |
| `max_body_bytes` | Request body cap |

`header_read_timeout_ms` is **not** applied here. Abandoning a request whose
HEADERS frame never arrives leaks its stream id inside the h3 crate, which stops
the connection ever closing — worse than the stall it would bound. What bounds
the exposure instead is `h2_max_concurrent_streams` per connection and
`max_connections` overall.

Two things behave differently here, both deliberately:

- **`keep_alive_ms = 0`** has no QUIC meaning — a connection multiplexes streams
  and cannot close after one response — so the listener keeps the default
  `75000` ms idle bound instead.
- **`h2_max_concurrent_streams = 0`** means "no limit" on HTTP/2 and cannot be
  translated: quinn allocates one slot per *advertised* stream when it builds a
  connection, so advertising a varint maximum is a four-quintillion-iteration
  loop triggered by the first packet from any peer. The listener keeps the
  default `200` instead.
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
