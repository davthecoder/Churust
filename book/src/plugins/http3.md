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

## Limits

WebSockets over HTTP/3 (RFC 9220 Extended CONNECT) are **not** implemented; the
`ws` feature uses the HTTP/1.1 upgrade handshake.

## API reference

- [docs.rs/churust-core](https://docs.rs/churust-core) `http3` module
