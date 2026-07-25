<!--
Thanks for the PR. Delete any section that genuinely doesn't apply — but if
you're deleting the tests section, say why in the description.
-->

## What this changes

<!-- One or two sentences. What behaviour is different after this merges? -->

## Why

<!-- Link the issue or discussion: "Closes #123" / "Discussed in #456".
     If there isn't one and this is more than a small fix, say what prompted it. -->

## How

<!-- Only if the approach isn't obvious from the diff. Alternatives you rejected
     and why are the most useful thing you can write here. -->

## Tests

<!-- Which tests cover this, and how you know they'd fail without the change. -->

---

### Checklist

- [ ] Tests added or updated, written before the implementation
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Feature matrix passes if I touched gated code (`full`, `ws`, `fs`, `tls`)
- [ ] Public API changes have doc comments with a working example
- [ ] A default-feature build is unaffected by any new gated code

### Flags for the reviewer

- [ ] **Breaking change** — describe the migration below
- [ ] **New dependency** — justify it below
- [ ] **New feature flag** — added to the umbrella crate and the CI matrix
- [ ] **Security-relevant** — touches auth, CORS, TLS, static file paths, or the router
- [ ] Performance trade-off worth a second opinion

<!-- Do not bump crate versions. All seven crates release in lockstep and the
     release tooling owns those numbers — see RELEASING.md. -->
