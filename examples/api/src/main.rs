//! # API — a JSON service with all four plugins
//!
//! Shows `install(plugin)` composition, JSON request and response bodies, and
//! authentication enforced by the type system: a handler that asks for
//! `Principal<AdminUser>` cannot run unauthenticated, so there is no way to
//! forget the check.
//!
//! ## Run it
//!
//! ```text
//! cargo run -p api
//! ```
//!
//! ## Try it
//!
//! ```text
//! curl localhost:8080/notes
//! # []
//!
//! curl -X POST localhost:8080/notes -d '{"text":"hi"}'
//! # {"error":"authentication required","status":401}
//!
//! curl -X POST localhost:8080/notes \
//!   -H 'authorization: Bearer admin-token' \
//!   -H 'content-type: application/json' \
//!   -d '{"text":"hi"}'
//! # {"id":1,"text":"hi"}
//! ```
//!
//! ## In your own project
//!
//! ```toml
//! [dependencies]
//! churust = { version = "0.2", features = ["full"] }   # json, logging, cors, auth
//! serde   = { version = "1", features = ["derive"] }
//! ```
//!
//! Two things here are demo-only and must not be copied into production: the
//! bearer callback compares against a hard-coded token, and `Cors::permissive()`
//! allows every origin.

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
