# State & configuration

## Typed app state

Attach shared state with `.state(T)`, then extract it with `State<T>`:

```rust
#[derive(Clone)]
struct Greeter {
    prefix: String,
}

Churust::server()
    .state(Greeter {
        prefix: "Hello".into(),
    })
    .routing(|r| {
        r.get(
            "/greet",
            |Query(s): Query<Search>, g: State<Greeter>| async move {
                format!("{}, {}!", g.prefix, s.q)
            },
        );
    })
```

`T` must be `Send + Sync + 'static`. Use interior mutability (`Mutex`,
`RwLock`, channels, etc.) when handlers need to update shared data.

You can register multiple distinct state types; extractors select by type.

## Layered configuration

Precedence (lowest → highest):

1. Built-in defaults  
2. `churust.toml`  
3. Environment variables (`CHURUST_*`)  
4. Code DSL (`.host()`, `.port()`, …)

### `churust.toml`

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
path_policy = "strict"         # strict | redirect | collapse

[tls]            # requires the `tls` feature
cert = "cert.pem"
key  = "key.pem"
```

### Loading config

```rust
Churust::from_config() // churust.toml + env
    .host("127.0.0.1") // DSL wins
    .port(8080)
    .routing(|r| { /* ... */ })
    .start()
    .await
```

Environment example: `CHURUST_SERVER_PORT=9090`.

### Code-only

```rust
Churust::server()
    .host("127.0.0.1")
    .port(8080)
```

## Deployment knobs

Useful builder / config fields include:

- Idle keep-alive  
- Listen backlog  
- Connection cap  
- TLS handshake cap and timeout (bounds queueing as well as the handshake)  
- HTTP/2 stream and header limits  
- Shutdown grace period (bounded drain)  
- Multiple bind addresses  
- Unix domain sockets that refuse to unlink a live socket  

Full key list: [Configuration keys](../reference/config.md).

## Cookies & sessions

Core includes a cookie primitive with safe defaults (`HttpOnly`,
`SameSite=Lax`) and HMAC-SHA256 signed cookie sessions. For **revocable**
server-side sessions, use [Redis sessions](../plugins/redis.md) (`redis`
feature).

## Next

→ [Testing with TestClient](testing.md)
