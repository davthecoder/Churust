# Releasing Churust

All seven crates ship together on one version number. There is no partial
release: `churust-core`, `churust-macros`, `churust-json`, `churust-logging`,
`churust-cors`, `churust-auth`, and `churust` always carry the same version,
even when only one of them changed.

That number lives in exactly one place — `[workspace.package] version` in the
root `Cargo.toml` — plus the seven internal entries under
`[workspace.dependencies]`. `cargo release` rewrites all of them together.
Never edit a crate's `Cargo.toml` version by hand.

## Cutting a release

```sh
cargo release patch          # dry run: prints the plan, changes nothing
cargo release patch --execute
```

Levels: `patch`, `minor`, `major`, or `rc` / `beta` / `alpha` for prereleases.

`--execute` bumps every crate, rewrites the internal dependency requirements,
makes one commit (`chore: release vX.Y.Z`), tags it `vX.Y.Z`, and pushes both.

Pushing the tag is the trigger. `.github/workflows/release.yml` then re-runs the
full CI gate (fmt, clippy, tests, TLS feature tests), does a dry-run publish,
mints a short-lived crates.io token over OIDC, publishes all seven crates, and
opens a GitHub Release.

`cargo release` will refuse to run off a branch other than `main`, or with a
dirty working tree.

## Publishing order

Nothing to configure — `cargo publish --workspace` resolves it:

```
churust-core, churust-macros
        |
        v
churust-json, churust-logging, churust-cors, churust-auth
        |
        v
churust
```

The four `examples/*` crates are `publish = false` and are skipped.

## One-time setup

### Trusted Publishing — still outstanding

CI authenticates by OIDC, so no registry token is stored in GitHub. crates.io
only allows this to be configured on a crate that **already exists**, which is
why it could not cover the first publish. All seven crates exist now, so this
can and should be done — **until it is, the workflow cannot publish** and
releases have to go up from a logged-in machine.

For each of `churust`, `churust-core`, `churust-macros`, `churust-json`,
`churust-logging`, `churust-cors`, `churust-auth`:

1. crates.io → the crate → Settings → Trusted Publishing → Add
2. Repository owner: `davthecoder`, repository: `Churust`
3. Workflow filename: `release.yml`
4. Environment: leave empty

### Publishing from a machine

The fallback while Trusted Publishing is unconfigured, and how `0.1.0` and
`0.1.1` went up:

```sh
cargo login          # paste a crates.io API token
cargo publish --workspace
```

Then push the tag so CI creates the GitHub Release. The workflow checks the
registry first, finds every crate already published, skips the upload, and goes
straight to the release — no credentials needed on that path.

## crates.io rate limits

Creating **new** crate names is limited to a burst of 5, refilling about 1 every
10 minutes. Publishing new **versions** of crates that already exist is a burst
of 30, refilling 1 per minute.

So the first release — seven names at once — stops after five with a `429`.
That is expected. Wait ten minutes and re-run the same command; cargo skips the
crates that already landed and continues with the rest. Every later release
publishes versions, not names, and never hits this.

## If a release goes wrong

A published version cannot be deleted, only yanked:

```sh
cargo yank --version X.Y.Z churust-core
```

Yanking stops new dependents from resolving it while leaving existing
`Cargo.lock` files working. The fix is to yank the bad version across all seven
crates and release a new patch — not to try to reuse the number.
