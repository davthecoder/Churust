# Releasing Churust

All fourteen crates ship together on one version number. There is no partial
release: `churust-core`, `churust-macros`, the eleven plugin and companion
crates, and the `churust` umbrella always carry the same version, even when only
one of them changed.

That number lives in exactly one place — `[workspace.package] version` in the
root `Cargo.toml` — plus the fourteen internal entries under
`[workspace.dependencies]`, which must be bumped in step or the umbrella will
depend on versions that were never published. Never edit a crate's
`Cargo.toml` version by hand.

## Cutting a release

`main` is protected: nothing is committed to it directly, so the version bump is
part of a pull request like any other change, and the only thing that happens on
`main` is the tag. That rules out `cargo release --execute`, which wants to
commit and tag in one step on `main` itself. The bump is therefore made by hand
in the release PR — there are exactly two files to touch and CI checks both.

**1. In the release PR:**

- Bump `[workspace.package] version` and every `version = "X.Y.Z"` under
  `[workspace.dependencies]` in the root `Cargo.toml`. They are all the same
  string, so one search and replace does it.
- Run `cargo update --workspace` so `Cargo.lock` records the new versions. The
  lockfile is committed and CI runs `--locked`, so a stale one fails the build.
- In `CHANGELOG.md`, turn `## [Unreleased]` into `## [Unreleased]` followed by
  `## [X.Y.Z] - YYYY-MM-DD`, and add the compare links at the bottom. The
  release workflow pulls the section matching the tag's version into the GitHub
  release, so an entry still sitting under `Unreleased` produces a release with
  no notes.
- Update the version in `README.md`'s install snippets.

**2. Merge the PR.**

**3. Tag the merge commit on `main` and push the tag:**

```sh
git checkout main && git pull
git tag -a v0.3.0 -m "Churust v0.3.0"
git push origin v0.3.0
```

Pushing the tag is the trigger, and a tag is not a commit, so branch protection
does not stand in the way. `.github/workflows/release.yml` then verifies the tag
matches the workspace version — a mismatch aborts before anything is published,
which is the guard against tagging a branch whose bump was forgotten — re-runs
the full CI gate, packages every crate, mints a short-lived crates.io token over
OIDC, publishes the fourteen crates in dependency order, and opens a GitHub
Release.

`release.toml` is kept for `cargo release`'s dry-run planning
(`cargo release minor` with no `--execute` still prints what would change), but
it is not what performs a release.

## Publishing order

The workflow publishes crate by crate, in dependency order, from an explicit
list. `cargo publish --workspace` is not used: it aborts on the first crate that
is already on the registry, which makes a half-finished release unrecoverable.

```
churust-core, churust-macros
        |
        v
churust-auth, churust-client, churust-compression, churust-cors,
churust-json, churust-lab, churust-logging, churust-openapi,
churust-ratelimit, churust-redis, churust-templates
        |
        v
churust
```

That list lives in `.github/workflows/release.yml` and is checked against the
workspace by `.github/scripts/check-release-list.py`, which CI runs on every
pull request. It has drifted once already — seven crates were added while the
list stayed at the original seven, so the umbrella depended on versions that
were never published and the release landed half-done. A comment asking people
to remember is not a mechanism; the script is.

The four `examples/*` crates are `publish = false` and are skipped.

## One-time setup

### Trusted Publishing

CI authenticates by OIDC, so no registry token is stored in GitHub. crates.io
only allows this to be configured on a crate that **already exists**, which is
why it could not cover the first publish — `0.1.0` and most of `0.1.1` went up
from a logged-in machine.

Configured per crate, for all fourteen published crates:

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
A release that introduces several *new* crate names hits the same wall, which is
why the workflow publishes only what the registry is missing and can simply be
re-run.
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
`Cargo.lock` files working. The fix is to yank the bad version across all
fourteen crates and release a new patch — not to try to reuse the number.
