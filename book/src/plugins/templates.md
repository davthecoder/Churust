# Templates

<span class="feature-tag">feature: templates</span>

## Enable

```toml
churust = { version = "0.3", features = ["templates"] }
```

## Setup

Templates are parsed at **startup** (fail fast). Output is HTML-escaped for
safety regardless of the template file name, because the rendered response is
served as HTML either way.

```rust
// From directory
let templates = Templates::from_dir("templates")?;

// Or build incrementally
let templates = Templates::new()
    .add("hello", "Hello, {{ name }}!")?;

// Optional minijinja environment tweaks
let templates = templates.configure(|env| {
    // env.add_filter(...);
});
```

Install as a plugin so handlers receive a `Renderer`:

```rust
.install(templates)
```

## Render in a handler

```rust
r.get("/", |r: Renderer| async move {
    r.render("hello", context! { name => "Churust" })
});
```

`Renderer::render` / `render_with_status` return a `Response`. The `context!`
macro is provided by the templates crate (re-exported through the umbrella when
the feature is on — check `prelude` / crate docs for the exact import path in
your version).

## API reference

- [docs.rs/churust-templates](https://docs.rs/churust-templates)
