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

### Trusted Publishing

CI authenticates by OIDC, so no registry token is stored in GitHub. crates.io
only allows this to be configured on a crate that **already exists**, which is
why it could not cover the first publish — `0.1.0` and most of `0.1.1` went up
from a logged-in machine.

Configured per crate, for all of `churust`, `churust-core`, `churust-macros`,
`churust-json`, `churust-logging`, `churust-cors`, `churust-auth`:

1. crates.io → the crate → Settings → Trusted Publishing → Add
2. Repository owner: `davthecoder`, repository: `Churust`
3. Workflow filename: `release.yml`
4. Environment: leave empty
5. Enable **Require trusted publishing for all new versions**

Step 5 is the one that matters, and it matters *least* on `churust`. The
umbrella is a thin re-export; `churust-core` is where the engine, router, TLS,
and static-file handling live, and `churust-auth` parses credentials. A leaked
API token that can still publish `churust-core` reaches every user of
`churust`, because they all depend on it transitively. Locking only the
umbrella protects the least valuable crate in the set.

### Releases go through CI, not a laptop

With "require trusted publishing" enabled, `cargo publish` from a developer
machine is refused:

```
403 Forbidden: New versions of this crate can only be published
using Trusted Publishing
```

That is the intended state. Publishing happens in `.github/workflows/release.yml`
and nowhere else, so the release path is reproducible from a clean checkout and
there is no long-lived credential that can be leaked.

If a release stalls partway, re-run the workflow rather than reaching for a
token. The publish step checks the registry first and skips anything already
uploaded, so re-running finishes the job instead of failing on the crates that
succeeded.

## crates.io rate limits

Creating **new** crate names is limited to a burst of 5, refilling about 1 every
10 minutes. Publishing new **versions** of crates that already exist is a burst
of 30, refilling 1 per minute.

So the first release — seven names at once — stopped after five with a `429`.
Every later release publishes versions rather than names and stays well inside
the burst.

Recovering from a partial release is **not** a matter of re-running
`cargo publish --workspace`. That command aborts on the first crate already on
the registry:

```
error: crate churust-core@0.1.0 already exists on crates.io index
```

The release workflow handles this instead: it queries the registry for each
crate at the target version, publishes only what is missing, and skips
crates.io authentication entirely when there is nothing left to publish. So the
recovery for any half-finished release is to re-run the workflow.

## If a release goes wrong

A published version cannot be deleted, only yanked:

```sh
cargo yank --version X.Y.Z churust-core
```

Yanking stops new dependents from resolving it while leaving existing
`Cargo.lock` files working. The fix is to yank the bad version across all seven
crates and release a new patch — not to try to reuse the number.
