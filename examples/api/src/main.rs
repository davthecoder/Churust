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

// Simple in-memory store.
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
        // Order of installation; phases make execution deterministic regardless.
        .install(CallLogging::new())
        .install(ContentNegotiation::new())
        .install(Cors::permissive())
        .install(Auth::bearer(|token: String| async move {
            // Demo only: accept a fixed admin token.
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
            // Creating a note requires authentication (asking for Principal enforces it).
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
