# Churust user guide (mdBook)

Source for the public documentation site deployed to GitHub Pages.

- **Live site:** https://davthecoder.github.io/Churust/
- **API reference:** https://docs.rs/churust (separate from this guide)

## Preview locally

```bash
cargo install mdbook --locked --version 0.4.48
cd book
mdbook serve --open
```

Build only:

```bash
mdbook build   # output in book/book/
```

## Layout

```text
book/
├── book.toml          # mdBook config
├── theme/churust.css  # light brand accents
└── src/               # Markdown chapters (see SUMMARY.md)
```

## Deploy

[`.github/workflows/docs.yml`](../.github/workflows/docs.yml) builds on every
change under `book/` and deploys from `main`.

One-time GitHub settings: **Settings → Pages → Source: GitHub Actions**.

If the repository is renamed or you use a custom domain, update `site-url` in
`book.toml` accordingly.
