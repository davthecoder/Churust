//! Procedural macros for the [Churust](https://docs.rs/churust) web framework.
//!
//! This crate exposes a single attribute macro, [`main`], which turns an
//! `async fn main()` into a synchronous entry point backed by a multi-threaded
//! Tokio runtime.  You should **not** depend on this crate directly; instead
//! use the top-level `churust` crate and write `#[churust::main]`.
//!
//! # Quick start
//!
//! In your application's `main.rs`, after adding `churust` as a dependency:
//!
//! ```text
//! #[churust::main]
//! async fn main() -> std::io::Result<()> {
//!     // Churust::server()…start().await goes here.
//!     Ok(())
//! }
//! ```
//!
//! The examples in this crate are illustrations rather than doctests: the
//! generated code names `::churust`, which cannot resolve from inside
//! `churust-macros` because `churust` depends on this crate and not the
//! reverse. Executable coverage lives in `churust/tests/macro_main.rs`.
//!
//! The attribute strips the `async` modifier and injects a Tokio runtime so
//! that the Rust compiler sees an ordinary synchronous `fn main()`.  This
//! mirrors the ergonomics of `#[tokio::main]` while keeping the framework's
//! entry-point convention explicit and framework-branded.
//!
//! # When to use this crate directly
//!
//! Almost never.  The re-export via `churust::main` (or `churust::prelude::*`)
//! is the intended consumption path.  The only reason to take a direct
//! dependency on `churust-macros` is if you are building a crate that wraps or
//! extends Churust's attribute macros at a low level.

#![deny(missing_docs)]

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// Turns `async fn main()` into a synchronous entry point that runs on a
/// multi-threaded Tokio runtime.
///
/// Apply this attribute to your application's `main` function in place of
/// `#[tokio::main]`.  The attribute:
///
/// 1. Verifies that the annotated function is `async`.  A compile error is
///    emitted if it is not.
/// 2. Removes the `async` keyword so the result is a valid synchronous `fn
///    main` that the Rust runtime can call directly.
/// 3. Wraps the original function body in a call to
///    `tokio::runtime::Builder::new_multi_thread().enable_all().build()` and
///    then blocks on the async body with `block_on`.
///
/// # Constraints
///
/// * The annotated function **must** be `async`.  Applying the attribute to a
///   non-async function is a hard compile error:
///
///   ```text
///   error: #[churust::main] requires an `async fn`
///   ```
///
/// * `tokio` must be a dependency (directly or transitively) of the crate that
///   uses this attribute, because the expanded code references
///   `::tokio::runtime::Builder`.
///
/// # Return type
///
/// The return type is preserved unchanged.  Returning `std::io::Result<()>` is
/// idiomatic because `Churust::server().start().await` returns that type:
///
/// ```text
/// #[churust::main]
/// async fn main() -> std::io::Result<()> {
///     Ok(())
/// }
/// ```
///
/// Returning `()` is also valid when no top-level I/O error needs to be
/// propagated:
///
/// ```text
/// #[churust::main]
/// async fn main() {
///     // fire-and-forget setup, panics on failure
/// }
/// ```
///
/// # Attribute arguments
///
/// No arguments are accepted.  The attribute token stream is intentionally
/// ignored so that the macro does not trap future extensions, but passing any
/// tokens currently has no effect.
///
/// # Generated code
///
/// Given the following input:
///
/// ```text
/// #[churust::main]
/// async fn main() -> std::io::Result<()> {
///     do_something().await
/// }
/// ```
///
/// The macro expands to roughly the following synchronous function.  The
/// generated runtime variable uses a mangled name (`__rt`) to avoid shadowing
/// anything in the caller's scope:
///
/// ```text
/// fn main() -> std::io::Result<()> {
///     let __rt = ::churust::__private::tokio::runtime::Builder::new_multi_thread()
///         .enable_all()
///         .build()
///         .expect("failed to build tokio runtime");
///     __rt.block_on(async move {
///         do_something().await
///     })
/// }
/// ```
///
/// The runtime is reached through `churust`'s re-export rather than `::tokio`,
/// so an application needs only `churust` as a dependency.
///
/// All original attributes (e.g. `#[cfg(…)]`, doc comments) and visibility
/// modifiers on the original function are forwarded to the generated function
/// unchanged.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    if input.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input.sig.fn_token,
            "#[churust::main] requires an `async fn`",
        )
        .to_compile_error()
        .into();
    }

    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;
    let output = &sig.output;
    let ident = &sig.ident;

    // Rebuild a non-async signature with the same name/return type.
    let expanded = quote! {
        #(#attrs)*
        #vis fn #ident() #output {
            let __rt = ::churust::__private::tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            __rt.block_on(async move #body)
        }
    };

    expanded.into()
}
