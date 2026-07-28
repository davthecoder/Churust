# Recipe: JSON API

Build a small notes API with logging, CORS, JSON, and bearer auth — the same
shape as [`examples/api`](https://github.com/davthecoder/Churust/tree/main/examples/api).

## Dependencies

```toml
[dependencies]
churust = { version = "0.3", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

## Code

```rust
use churust::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
struct Note {
    id: u64,
    text: String,
}

#[derive(Deserialize)]
struct NewNote {
    text: String,
}

#[derive(Clone, Debug)]
struct AdminUser {
    _name: String,
}

struct Store {
    notes: Mutex<Vec<Note>>,
}

#[churust::main]
async fn main() -> std::io::Result<()> {
    Churust::server()
        .host("127.0.0.1")
        .port(8080)
        .state(Store {
            notes: Mutex::new(Vec::new()),
        })
        .install(CallLogging::new())
        .install(ContentNegotiation::new())
        .install(Cors::new().allow_origin("http://localhost:3000"))
        .install(Auth::bearer(|token: String| async move {
            // Demo only — use secure_compare + a real store in production.
            if token == "admin-token" {
                Some(AdminUser {
                    _name: "admin".into(),
                })
            } else {
                None
            }
        }))
        .routing(|r| {
            r.get("/notes", |s: State<Store>| async move {
                let notes = s.notes.lock().unwrap().clone();
                Json(notes)
            });
            r.post(
                "/notes",
                |Principal(_admin): Principal<AdminUser>,
                 s: State<Store>,
                 Json(input): Json<NewNote>| async move {
                    let mut notes = s.notes.lock().unwrap();
                    let id = notes.len() as u64 + 1;
                    let note = Note {
                        id,
                        text: input.text,
                    };
                    notes.push(note.clone());
                    (StatusCode::CREATED, Json(note))
                },
            );
        })
        .start()
        .await
}
```

## Try it

```bash
cargo run -p api   # from the Churust repo

curl localhost:8080/notes
# []

curl -X POST localhost:8080/notes -d '{"text":"hi"}'
# {"error":"authentication required","status":401}

curl -X POST localhost:8080/notes \
  -H 'authorization: Bearer admin-token' \
  -H 'content-type: application/json' \
  -d '{"text":"hi"}'
# {"id":1,"text":"hi"}
```

## Takeaways

- `Principal<AdminUser>` makes the POST route uncallable without auth.  
- `ContentNegotiation` keeps error bodies JSON-friendly.  
- CORS names real browser origins — not `*` — when credentials matter.
