//! tex-ls-formatter — the deterministic, rule-based formatting engine behind
//! [tex-ls](https://github.com/backmatter/tex-ls), for LaTeX (`.tex`, `.sty`/`.cls`, `.dtx`,
//! `.ins`) and BibTeX (`.bib`).
//!
//! Layout is decided solely by the formatter's rules and the Wadler/Prettier
//! layout engine, on the lossless CST from [`tex_ls_parser`]. The formatter
//! changes only trivia — it never inserts, deletes, or rewrites a non-trivia
//! token — and `fmt(fmt(x)) == fmt(x)`.
//!
//! This crate is embeddable (it builds for `wasm32-unknown-unknown`); the
//! batch path-walking check API and the CLI live in the `tex-ls` crate.
//!
//! With the optional `serde` feature, [`FormatStyle`] and its enums are
//! (de)serializable under `tex-ls.toml`'s kebab-case spellings; the `schema`
//! feature additionally derives `schemars::JsonSchema` for embedders that
//! publish a config schema (see [`formatter::style`]).

pub mod bib;
pub mod formatter;
pub mod settings;

pub use formatter::{
    FormatError, FormatStyle, ItemIndent, LineEnding, MathWrap, SentenceOptions, WrapMode, format,
};

// Re-export rowan so embedders can name the exact tree types this crate is
// built against without pinning a matching rowan version themselves.
pub use rowan;
