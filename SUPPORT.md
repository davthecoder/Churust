# Getting help with Churust

## Start here

| You want to | Go to |
| --- | --- |
| Learn the API | [docs.rs/churust](https://docs.rs/churust) |
| See working code | [`examples/`](https://github.com/davthecoder/Churust/tree/main/examples) — hello, api, chat, static |
| Understand a design decision | [`docs/design/`](https://github.com/davthecoder/Churust/tree/main/docs/design) |
| Ask a question | [Discussions → Q&A](https://github.com/davthecoder/Churust/discussions/categories/q-a) |
| Report a bug | [Issues](https://github.com/davthecoder/Churust/issues/new/choose) |
| Suggest a feature | [Discussions → Ideas](https://github.com/davthecoder/Churust/discussions/categories/ideas) |
| Report a vulnerability | [SECURITY.md](SECURITY.md) — **not** a public issue |

## Question or bug?

Use Discussions if you're asking "how do I…" or "is this supposed to…". Use
Issues if you can show something is broken — a reproduction, the wrong output,
or a panic.

If you're not sure, open a discussion. We'll convert it to an issue if it turns
out to be a bug, which is much less friction than the reverse.

## Making your question answerable

Include:

- Churust version and which features you enabled (`full`, `ws`, `fs`, `tls`)
- Rust version — `rustc --version`
- The smallest bit of code that shows the problem
- What you expected, and what happened instead
- The full error, not a summary of it

A compiling reproduction gets an answer far faster than a description of one.

## Response times

Churust is maintained in spare time. Expect a few days, longer around holidays.
Security reports are the exception and are handled on the timeline in
[SECURITY.md](SECURITY.md).

Nudging an unanswered thread after a week is fine and not rude.

## Things we can't help with

General Rust or async questions are better served by the
[Rust users forum](https://users.rust-lang.org/) or the
[Rust Discord](https://discord.gg/rust-lang). Same for tokio, hyper, and rustls
questions that aren't about how Churust uses them.
