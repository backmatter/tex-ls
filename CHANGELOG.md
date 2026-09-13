# Changelog

## 0.1.0

First source release of tex-ls, a LaTeX and BibTeX language server, formatter, and
linter written in Rust.

- Native language server over stdio, with completion, navigation, rename,
  diagnostics, code actions, semantic highlighting, and document structure.
- Formatting and linting for LaTeX and BibTeX, with configuration, suppression
  comments, and safe lint fixes.
- Lossless parsing and incremental reparsing, with recovery for malformed and
  deeply nested input.
- Browser sessions through WebAssembly, sharing the native language engine.

Install from the `v0.1.0` tag or the release source archive. See the
[README](README.md) for build commands and editor setup.

This release provides source only. Library and browser APIs may change before
1.0. The parser does not execute TeX or infer signatures from dynamic macro
expansion. Formatting refuses unsupported syntax; see the
[formatting guide](docs/guide/formatting.md) for limitations.
