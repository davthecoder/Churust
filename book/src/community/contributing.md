# Contributing

Contributions are welcome. Read the full guide first:

**[CONTRIBUTING.md](https://github.com/davthecoder/Churust/blob/main/CONTRIBUTING.md)**

## Short version

- Churust keeps a **narrow scope**  
- **Tests come first**  
- Anything optional is **feature-gated** so default builds never change  
- Warnings are **errors** in CI  
- MSRV is **1.96**  

## Before you open a PR

Run the full gate listed in CONTRIBUTING (`fmt`, clippy feature matrix, test
matrix, examples, docs). For this documentation site:

```bash
cargo install mdbook --locked --version 0.4.48   # once
cd book && mdbook build
```

## Channels

| | |
| --- | --- |
| Ask a question | [Discussions → Q&A](https://github.com/davthecoder/Churust/discussions/categories/q-a) |
| Propose a feature | [Discussions → Ideas](https://github.com/davthecoder/Churust/discussions/categories/ideas) |
| Report a bug | [Issues](https://github.com/davthecoder/Churust/issues/new/choose) |
| Report a vulnerability | [SECURITY.md](https://github.com/davthecoder/Churust/blob/main/SECURITY.md) — privately |
| Code of conduct | [CODE_OF_CONDUCT.md](https://github.com/davthecoder/Churust/blob/main/CODE_OF_CONDUCT.md) |

Design specs live under
[`docs/design/`](https://github.com/davthecoder/Churust/tree/main/docs/design)
(internal milestones, not product version numbers).
