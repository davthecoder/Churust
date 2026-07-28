# From Ktor to Churust

Churust deliberately reuses **Ktor’s mental model** so you can keep the same
shape of application while writing **Rust** on **tokio + hyper + rustls**.

| You think in Ktor… | In Churust… |
| --- | --- |
| `Application` / `embeddedServer` | `Churust::server()` builder |
| `routing { … }` | `.routing(\|r\| { … })` |
| `get` / `post` / … | `r.get` / `r.post` / … |
| `install(…)` | `.install(…)` |
| Application plugins + interceptors | Plugins into fixed **phases** |
| `call` | `Call` *or* typed extractors |
| `call.parameters["id"]` | `Path(id): Path<u64>` |
| `call.receive<T>()` | `Json(t): Json<T>` (feature `json`) |
| Auth principal | `Principal<P>` (feature `auth`) |
| Application attributes / DI | `.state(T)` + `State<T>` |
| `application.conf` + env | `churust.toml` + `CHURUST_*` + DSL |
| `testApplication { … }` | `TestClient` (in-process) |

The syntax is Rust. The **structure** of a service — build, install, route,
start — should already be familiar.

---

## 1. A minimal route

### Ktor (Kotlin)

```kotlin
fun Application.module() {
    routing {
        get("/") {
            call.respondText("Hello from Ktor")
        }
    }
}
```

### Churust (Rust)

```rust
use churust::prelude::*;

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .routing(|r| {
            r.get("/", |_call: Call| async { "Hello from Churust 🌀" });
        })
        .start()
        .await
}
```

**Same idea:** declare routes inside a routing block.  
**Rust difference:** an explicit async `main`, and `#[churust::main]` (like
`#[tokio::main]`) bootstraps the runtime Churust re-exports.

---

## 2. Path parameters

### Ktor

```kotlin
routing {
    get("/users/{id}") {
        val id = call.parameters["id"]?.toLongOrNull()
            ?: return@get call.respond(HttpStatusCode.BadRequest)

        call.respondText("user #$id")
    }
}
```

### Churust

```rust
r.get("/users/{id}", |Path(id): Path<u64>| async move {
    format!("user #{id}")
});
```

| Ktor | Churust |
| --- | --- |
| Read `call.parameters["id"]` as a string | `Path<u64>` parses and types the segment |
| Manual `toLongOrNull` + 400 | Malformed path → error **before** the handler body |
| Optional/nullable handling by hand | Use `Option<Path<…>>` / optional extractors where supported |

You still *can* take `Call` and dig into parts yourself; extractors are the
short path for the common case.

---

## 3. Install plugins

### Ktor

```kotlin
fun Application.module() {
    install(CallLogging)
    install(ContentNegotiation) {
        json()
    }
    install(CORS) {
        allowHost("app.example.com", schemes = listOf("https"))
    }

    routing {
        // …
    }
}
```

### Churust

```rust
Churust::server()
    .install(CallLogging::new())
    .install(ContentNegotiation::new())
    .install(Cors::new().allow_origin("https://app.example.com"))
    .install(Compression::new())
    .install(RateLimit::per_minute(120))
    .routing(|r| {
        // …
    })
```

| Ktor | Churust |
| --- | --- |
| `install(Feature)` inside `Application.module` | `.install(plugin)` on the server builder |
| Features often pull in Kotlinx serialization, etc. | Cargo **features** gate crates (`json`, `cors`, …) |
| Interceptor phases / plugin order | Fixed pipeline phases; install order is stable within that model |

Enable features in `Cargo.toml` the same way you would add dependencies in
Gradle — then `install` only what you compiled in:

```toml
churust = { version = "0.3", features = ["full"] }
```

See [Installation & features](installation.md) and
[Pipeline & plugins](../fundamentals/pipeline-plugins.md).

---

## 4. JSON body + response

### Ktor

```kotlin
@Serializable
data class NewNote(val text: String)

@Serializable
data class Note(val id: Long, val text: String)

routing {
    post("/notes") {
        val input = call.receive<NewNote>()
        call.respond(Note(id = 1, text = input.text))
    }
}
```

### Churust

```rust
#[derive(Deserialize)]
struct NewNote { text: String }

#[derive(Serialize)]
struct Note { id: u64, text: String }

r.post("/notes", |Json(input): Json<NewNote>| async move {
    Json(Note { id: 1, text: input.text })
});
```

| Ktor | Churust |
| --- | --- |
| `call.receive<T>()` | `Json<T>` extractor (last argument if it consumes the body) |
| `call.respond(T)` with content negotiation | `Json(T)` implements `IntoResponse` |
| kotlinx.serialization (typical) | `serde` derives |

---

## 5. Authentication / principal

### Ktor (sketch)

```kotlin
install(Authentication) {
    bearer("auth") {
        authenticate { credential ->
            if (credential.token == expected) UserIdPrincipal("admin") else null
        }
    }
}

routing {
    authenticate("auth") {
        get("/me") {
            val principal = call.principal<UserIdPrincipal>()!!
            call.respondText(principal.name)
        }
    }
}
```

### Churust

```rust
#[derive(Clone)]
struct Admin { name: String }

Churust::server()
    .install(Auth::bearer(|token: String| async move {
        (token == expected).then(|| Admin { name: "admin".into() })
    }))
    .routing(|r| {
        // Asking for Principal enforces auth (401 otherwise).
        r.get("/me", |Principal(u): Principal<Admin>| async move {
            u.name
        });
    })
```

| Ktor | Churust |
| --- | --- |
| Named auth configs + `authenticate("…")` blocks | Install `Auth`, then require `Principal<P>` on the handler |
| `call.principal<T>()` | `Principal<T>` extractor |
| Forgetting `authenticate` is a common footgun | No `Principal` → route is public; with `Principal` → type system enforces it |

Prefer `secure_compare` (or a real store) over `==` on secrets in production.
See [Authentication](../plugins/auth.md).

---

## 6. Shared application state

### Ktor

```kotlin
// Often: attributes, singletons, or a DI framework
val store = NoteStore()
// … put store where handlers can reach it
```

### Churust

```rust
struct Store { notes: Mutex<Vec<Note>> }

Churust::server()
    .state(Store { notes: Mutex::new(Vec::new()) })
    .routing(|r| {
        r.get("/notes", |s: State<Store>| async move {
            // …
        });
    })
```

Typed `.state(T)` + `State<T>` replace “reach into the application container.”
Use interior mutability (`Mutex`, channels, …) when handlers update shared data.

---

## 7. Configuration

### Ktor

```hocon
# application.conf
ktor {
    deployment {
        port = 8080
        port = ${?PORT}
    }
}
```

### Churust

```toml
# churust.toml
[server]
host = "0.0.0.0"
port = 8080
```

```bash
export CHURUST_SERVER_PORT=9090
```

```rust
Churust::from_config()
    .port(8080) // code still wins over env/file when chained
    .routing(|r| { /* … */ })
```

Precedence: defaults &lt; `churust.toml` &lt; `CHURUST_*` &lt; builder DSL.
Details: [State & configuration](../fundamentals/state-config.md).

---

## 8. Testing

### Ktor

```kotlin
testApplication {
    application { module() }
    val response = client.get("/ping")
    assertEquals(HttpStatusCode.OK, response.status)
}
```

### Churust

```rust
let app = Churust::server()
    .routing(|r| {
        r.get("/ping", |_c: Call| async { "pong" });
    })
    .build();

let res = TestClient::new(app).get("/ping").send().await;
assert_eq!(res.status(), StatusCode::OK);
assert_eq!(res.text(), "pong");
```

No socket bind: the full pipeline runs in-process, same idea as
`testApplication`. See [Testing](../fundamentals/testing.md).

---

## What stays “Rust” on purpose

Churust does **not** try to feel like Kotlin. It reuses Ktor’s *application
shape* so you do not relearn “what a server is,” then leans on Rust where it
matters:

| Topic | Expectation |
| --- | --- |
| Ownership & lifetimes | Handlers own or borrow what extractors give them; no hidden GC |
| Errors | Prefer `Result` / `IntoError` / `?` over silent nulls |
| Async | `async`/`.await` on tokio (via `churust::tokio` or your own features) |
| Types | Path/query/JSON failures surface as typed HTTP errors, not `null` |
| Features | Opt-in Cargo features keep unused protocol surface out of the binary |

You still learn Rust. You should not have to learn a fragmented web stack at the
same time.

---

## Concept map (cheat sheet)

```text
Ktor                         Churust
────                         ───────
Application.module()    →    Churust::server()…start()
routing { }             →    .routing(|r| { })
install(Feature)        →    .install(plugin)  + Cargo feature
call                    →    Call  |  extractors
receive / respond       →    Json / IntoResponse
principal               →    Principal<P>
attributes / DI         →    .state(T) + State<T>
application.conf        →    churust.toml + CHURUST_* + DSL
testApplication         →    TestClient
embeddedServer / Netty  →    hyper (HTTP) + rustls (TLS) + tokio
```

---

## Next

- [Quick start](quick-start.md) — run your first binary  
- [Handlers & extractors](../fundamentals/handlers-extractors.md) — depth on types  
- [JSON API recipe](../recipes/json-api.md) — plugins + auth end-to-end  
- [Pipeline & plugins](../fundamentals/pipeline-plugins.md) — phase model  
