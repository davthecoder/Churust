# Routing & guards

Routes are registered on a `RouteBuilder` inside `.routing(|r| { ... })`.

## HTTP methods

```rust
r.get("/items", handler);
r.post("/items", handler);
r.put("/items/{id}", handler);
r.patch("/items/{id}", handler);
r.delete("/items/{id}", handler);
// and other methods supported by the builder
```

## Path parameters

Segments in `{braces}` are captured and available via the `Path` extractor:

```rust
r.get("/users/{id}", |Path(id): Path<u64>| async move {
    format!("user #{id}")
});
```

`Path` supports scalars, tuples, and structs that deserialize from path parts.

## Catch-all segments

A trailing `{name...}` matches the rest of the path (used by static file
serving):

```rust
r.get(
    "/{path...}",
    StaticFiles::dir("./public").index("index.html").handler(),
);
```

Requires the `fs` feature for `StaticFiles`.

## Automatic HEAD and OPTIONS

Churust provides correct HTTP behavior out of the box:

- Automatic **HEAD** for GET routes
- Automatic **OPTIONS**, including `OPTIONS *`
- One **`Allow`** header that both `405` and `OPTIONS` agree on

## Route guards

Several routes may share a path when a guard distinguishes them, for example
`guard::header`, `guard::host`, or a custom predicate. The first matching route
with a satisfied guard wins; ambiguity is refused rather than guessed.

## Path policy

`PathPolicy` decides what happens to non-canonical paths such as `//a` or
`/a//b`:

| Policy | Behavior |
| --- | --- |
| `strict` (default) | Refuse non-canonical paths |
| `redirect` | `308` to the canonical form |
| `collapse` | Collapse silently |

Silent collapsing can make prefix-based auth checks bypassable and cache
identity ambiguous — prefer `strict` or explicit redirects.

Configure via `churust.toml` (`path_policy`) or the builder DSL. See
[Configuration keys](../reference/config.md).

## Mounting groups

Compose related routes under a prefix when the API grows; keep handlers small
and push shared middleware with `intercept` where you need route-tree-scoped
behavior (see [Pipeline & plugins](pipeline-plugins.md)).

## Next

→ [Handlers & extractors](handlers-extractors.md)
