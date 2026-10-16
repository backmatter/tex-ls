# Changelog

## [0.1.4](https://github.com/backmatter/tex-ls/compare/v0.1.3...v0.1.4) - 2026-10-16

### Fixed
- Handle closed macOS sockets and normalize citation origins
- Refresh restored packages and correct native path handling
- Harden language features and reorganize implementation
- Improve semantic highlighting and language server performance

- Reduce repeated parsing and project scans during completion, highlighting,
  diagnostics, navigation, formatting, and bulk edits. Bound retained request caches
  and reuse source facts after incremental updates.
- Improve lossless parsing and full/incremental equivalence for malformed input,
  macro definitions, docstrip sources, literal bodies, and embedded code.
- Distinguish prose, literal content, paths, and math in semantic highlighting.
  Add declaration and text-style modifiers, Python listing colors, and embedded
  grammar support for explicit C, C++, JavaScript, TypeScript, JSON, CSS, and HTML.
- Preserve comments and protected layout during formatting. Batch safe fixes and
  range edits without changing their meaning or splitting Unicode characters.
- Improve bibliography key and string navigation, completion, rename, and
  diagnostics across loaded resources, including literal entry dependencies and
  Biber aliases. Decline edits when resolution is ambiguous.
- Offer package-load quick fixes for supported commands in plain `article`,
  `report`, and `book` documents with verified installed package sources. Recognize
  `.clo` files and refresh package assistance after installation changes.
- Add semantic-token delta responses and extend native/browser parity coverage.
  Reject malformed requests safely and cancel stale native rename previews.
- Fix project discovery, source acquisition, watcher refresh, compiler diagnostics,
  source relocation, and cleanup across native and browser hosts.

## [0.1.3](https://github.com/backmatter/tex-ls/compare/v0.1.2...v0.1.3) - 2026-10-01

### Added
- Improve LaTeX editing and fix Salsa memory safety ([#4](https://github.com/backmatter/tex-ls/pull/4))

### Fixed
- Update vulnerable VS Code dependencies ([#7](https://github.com/backmatter/tex-ls/pull/7))

## [0.1.2](https://github.com/backmatter/tex-ls/compare/v0.1.0...v0.1.2) - 2026-09-14

- Support project-local TeX packages without requiring a system TeX installation.
- Refresh completions when packages are installed or removed.
- Track build outputs in editor-excluded directories.
- Allow build tools to provide compiler diagnostics without duplicates.
- Preserve editor formatting settings in build-only projects.

## [0.1.1](https://github.com/backmatter/tex-ls/compare/v0.1.0...v0.1.1) - 2026-09-14

### Added
- Support project-local build integrations ([#2](https://github.com/backmatter/tex-ls/pull/2))
- Add minimal VS Code extension and Marketplace guides
- Release tex-ls 0.1.0

## 0.1.0

- First release for Linux on Intel/AMD computers.
- Suggest LaTeX commands and BibTeX entries, highlight mistakes, and offer fixes.
- Jump to labels, rename references, browse sections, and format documents.
- Add commands to apply safe fixes, inspect a project, restart tex-ls, and view logs.
