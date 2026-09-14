# Changelog

## [0.1.1](https://github.com/backmatter/tex-ls/compare/v0.1.0...v0.1.1) - 2026-09-14

### Added
- Support project-local build integrations ([#2](https://github.com/backmatter/tex-ls/pull/2))
- Add minimal VS Code extension and Marketplace guides
- Release tex-ls 0.1.0

## 0.1.0

First release of tex-ls, a LaTeX and BibTeX language server, formatter, and
linter written in Rust.

- Native language server over stdio, with completion, navigation, rename,
  diagnostics, code actions, semantic highlighting, and document structure.
- Formatting and linting for LaTeX and BibTeX, with configuration, suppression
  comments, and safe lint fixes.
- Lossless parsing and incremental reparsing, with recovery for malformed and
  deeply nested input.
- Browser sessions through WebAssembly, sharing the native language engine.

Prebuilt executables are available for Linux, macOS, and Windows on x86-64 and
ARM64. Install with Homebrew, Mason, or the platform installer. See the
[README](README.md) for installation and editor setup.
