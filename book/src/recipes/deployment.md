# Recipe: deployment

## Binary

```bash
cargo build --release -p my-api
# target/release/my-api
```

Pin features you actually need to keep compile times and attack surface small.

## Configuration in production

Prefer env over baking secrets into images:

```bash
export CHURUST_SERVER_HOST=0.0.0.0
export CHURUST_SERVER_PORT=8080
export CHURUST_SERVER_MAX_BODY_BYTES=2097152
./my-api
```

Or ship a `churust.toml` next to the binary and override with env/DSL as needed.
See [Configuration keys](../reference/config.md).

## Process model

| Concern | Suggestion |
| --- | --- |
| TLS | Terminate at reverse proxy **or** enable `tls` in-process |
| Logs | `tracing` subscriber → stdout; ship to your aggregator |
| Health | A cheap `GET /health` or `/ready` route |
| Graceful stop | `shutdown_timeout_ms` bounds drain; send SIGTERM from orchestrator |
| Connections | Tune `max_connections`, keep-alive, timeouts for your traffic |

## Reverse proxy sketch (nginx)

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    # WebSockets:
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
}
```

If you rate-limit by peer address **behind** a proxy, key on a trusted
forwarded client IP header instead of the proxy’s address — use
`RateLimit::by(...)`.

## Containers

```dockerfile
FROM rust:1.96 as build
WORKDIR /app
COPY . .
RUN cargo build --release -p my-api

FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/release/my-api /my-api
ENV CHURUST_SERVER_HOST=0.0.0.0
ENV CHURUST_SERVER_PORT=8080
EXPOSE 8080
CMD ["/my-api"]
```

Adjust the runtime image for your glibc/musl and TLS needs.

## Checklist

- [ ] Features limited to production needs  
- [ ] Body/timeout/connection caps set consciously  
- [ ] CORS origins are explicit  
- [ ] Secrets via env or secret manager  
- [ ] Security headers left on (see [Security defaults](../reference/security.md))  
- [ ] Health/readiness probes  
- [ ] Load test with realistic keep-alive and payload sizes  
