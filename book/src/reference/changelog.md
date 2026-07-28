# Changelog & migration

## Releases

Full history lives in the repository:

**[CHANGELOG.md](https://github.com/davthecoder/Churust/blob/main/CHANGELOG.md)**

Current series documented in this guide: **0.3.x**.

## How Churust versions

- All `churust*` crates share one version (lockstep).  
- Pre-1.0: **breaking changes may appear in minor** bumps (`0.3` → `0.4`).  
- Prefer caret pins on the minor you tested: `churust = "0.3"`.

## Migrating between minors

1. Read the changelog section for the target version.  
2. `cargo update -p churust` (or bump the pin) and rebuild with `-D warnings`
   if you match project CI.  
3. Re-run examples or your `TestClient` suite.  
4. Check renamed APIs (example: CORS `permissive` → `allow_any_origin_insecure`
   in 0.3).

## docs.rs

API docs for every published version:

- [https://docs.rs/churust](https://docs.rs/churust)

## This book

The site tracks **main** of the guide source under `book/`. Older framework
versions may differ slightly; when in doubt, match code samples to the crate
version on crates.io.
