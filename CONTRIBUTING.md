# Contributing to Churust

Thanks for considering it. Churust is a small framework with a deliberately
narrow scope, so the most useful thing you can do before writing code is tell us
what you're planning — a short issue or discussion saves everyone from a PR that
gets closed on scope grounds.

- **Bug?** Open an issue with a reproduction.
- **Idea?** Open a [Discussion](https://github.com/davthecoder/Churust/discussions)
  before a PR. Design first, code second.
- **Question?** See [SUPPORT.md](SUPPORT.md).

Everyone taking part is held to the [Code of Conduct](CODE_OF_CONDUCT.md).

## What Churust is, and is not

Churust owns the *ergonomic* layer: an application engine, a routing DSL, an
`install(plugin)` system, and a phased interceptor pipeline. It does not
reimplement HTTP parsing, TLS, or async scheduling — those are tokio, hyper, and
rustls, and they stay that way.

Proposals that add a dependency to do something the stack already does, or that
widen the API surface without a concrete use case, will usually be declined.
Sessions, response compression, HTTP/3 and route-scoped middleware all landed in
0.3.0, so that list is no longer the answer to "why isn't this here?" — if
something is missing now, make the case in a discussion first.

## Getting set up

You need Rust **1.96 or newer** — that's the MSRV, and CI pins exactly 1.96.0.

```sh
git clone https://github.com/davthecoder/Churust.git
cd Churust
cargo test --workspace
```

Run an example to see it work:

```sh
cargo run -p hello           # http://127.0.0.1:8080
cargo run -p api             # all four plugins
cargo run -p chat            # WebSockets
cargo run -p static-example  # static file serving
```

## The workspace

| Crate | What lives there |
| --- | --- |
| `churust-core` | engine, router, pipeline, extractors, `Call`, `Body`, TLS, WebSockets, static files |
| `churust-macros` | `#[churust::main]` |
| `churust-json` | `Json<T>`, content negotiation |
| `churust-logging` | `CallLogging` over `tracing` |
| `churust-cors` | preflight and CORS headers |
| `churust-auth` | Bearer/Basic/JWT, `Principal<P>` |
| `churust-ratelimit` | `RateLimit`, GCRA |
| `churust-compression` | brotli / gzip / deflate response bodies |
| `churust-templates` | `Templates` + `Renderer` over minijinja |
| `churust-redis` | `RedisStore`, a revocable server-side `SessionStore` |
| `churust-client` | outbound HTTP client on the same hyper the server uses |
| `churust-openapi` | an OpenAPI 3.1 document generated from the router |
| `churust-lab` | incubator; deliberately never reaches 1.0 |
| `churust` | umbrella re-export; the crate users actually depend on |
| `examples/*` | runnable examples, `publish = false` |

Design specs are in [`docs/design/`](docs/design), implementation plans in
[`docs/plans/`](docs/plans). Read the spec for the area you're touching — it
explains why things are shaped the way they are, which is usually the thing a
diff can't tell you.

## The rules that actually matter

**Tests come first.** Every feature in this codebase was built test-first, and
the plans in `docs/plans/` are written that way. Write the failing test, watch
it fail, then make it pass. A PR with new behaviour and no test will be sent
back.

**Nothing goes in `churust-core` that can live in a plugin.** The core is
supposed to stay small. If it can be a middleware or a separate crate, it should
be.

**New optional functionality is feature-gated,** and a default-feature build
must be byte-for-byte unaffected. Add the feature to the plugin crate, then
re-export it through the umbrella `churust` crate's `[features]` table, then add
it to the CI matrix.

**Public API changes need doc comments with a working example.** Doctests run in
CI, so the example in your docs is a test whether you meant it to be or not.

**No new dependencies without justification** in the PR description. "It saves
twenty lines" is not justification; "hand-rolling this would be a correctness
risk" is.

## Benchmarks

Two separate things, because only one of them produces numbers you can trust on
a shared machine.

**Regressions** — `cargo bench -p churust-core`. Criterion, in-process, no
socket. To compare a change against `main`:

```sh
git checkout main && cargo bench -p churust-core -- --save-baseline main
git checkout - && cargo bench -p churust-core -- --baseline main
```

CI runs this on every PR against the merge base and comments the table. It fails
only past 20%, because the runners swing 20–30% on their own.

**Comparison against axum** — `benchmarks/run.sh`. Run by hand on an idle
machine; see `benchmarks/README.md`.

## Before you open a PR

Run the same gate CI runs. All of it:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked -p churust --all-targets --all-features -- -D warnings
cargo clippy --locked -p churust-core --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo test --locked -p churust --features full
cargo test --locked -p churust-core --all-features
cargo build --locked -p hello -p api
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

Warnings are errors here — `RUSTFLAGS: "-D warnings"` is set for the whole CI
run. If it warns locally it fails remotely.

`Cargo.lock` is committed, and CI passes `--locked`, so your run and CI's use
the same dependency versions. Commit the lockfile alongside any manifest change
that moves it, or CI fails before it compiles anything. Don't run `cargo update`
to fix an unrelated failure: `.github/workflows/ci-float.yml` already tests the
newest of everything weekly, and that is where an upstream behaviour change is
supposed to surface.

Touching WebSockets, static files, or TLS? Also run the feature you touched
*and* the default build, because the whole point of the gating is that default
builds don't change.

## Pull requests

Branch off `main`, one logical change per PR. Keep the diff reviewable — if it
does three things, it's three PRs.

Commit subjects: imperative mood, lower case, no trailing period.

```
add Retry-After header to 429 responses
fix path traversal check for symlinked directories
```

The PR description should say what changed and why, and call out anything a
reviewer would otherwise have to guess: a behaviour change, a new dependency, a
new feature flag, a performance trade-off.

CI must be green before review. Maintainers may push small fixups directly to
your branch to avoid a round trip.

## Adding a plugin crate

1. `churust-<name>/` with `Cargo.toml` inheriting the workspace fields
   (`version.workspace = true`, `repository.workspace = true`, and the rest).
2. Add it to `members` **and** `[workspace.dependencies]` in the root
   `Cargo.toml` — the `[workspace.dependencies]` entry is what makes lockstep
   releases work.
3. Add an optional dependency and a matching feature in `churust/Cargo.toml`,
   and re-export from the prelude.
4. Extend the CI feature matrix.
5. Tests, docs with examples, and an entry in the README feature table.

Pick the right pipeline phase for your middleware: `Setup < Monitoring <
Plugins < Call < Fallback`. Ordering is deterministic and the phase is how you
control it.

## Releases

Maintainers only, and there is no partial release: all fourteen crates share one
version and go out together. See [RELEASING.md](RELEASING.md).

Don't bump versions in an ordinary PR. `main` is protected, so the bump happens
in a dedicated release PR and the tag is pushed to `main` afterwards — a tag is
not a commit, which is what lets the release run at all.

## Licence

Contributions are licensed under [MIT](LICENSE), the same as the project. By
opening a PR you agree to that.
