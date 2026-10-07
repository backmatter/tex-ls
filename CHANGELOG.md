# Changelog

## Unreleased

- Preserve printed `includeonly` arguments during file rename after local overrides or inside inert source. Keep conditional and dynamic listing settings from exposing unproved TeX references, and stop FancyVerb comments at bare CR line endings.
- Keep copied and inspected command names navigable without executing their argument roles. Preserve ambient text or math parsing, nested real commands, and rename targets; apply local reader overrides in source order. Generated listing declarations cannot override readers after their factory has been redefined.
- Cache label descriptions per source file so completion lists reuse section, caption, and ownership facts instead of rescanning the document for every candidate. Empty label queries render long titles only for the selected results.
- Reuse proved operand ownership across source queries, editor declarations, hover, and lint checks. Keep first-use and post-edit requests from rebuilding the same definition and token indexes in each consumer. Table-column action probes reuse source signatures and request them only inside a table.
- Speed up multi-cursor selection, Fix All, BibTeX range formatting, color pickers, and file-rename previews by batching positions and reusing source facts. Run native file-rename previews on the read pool with cancellation. Clamp oversized columns safely on 32-bit hosts.
- Batch bulk quick-fix edits and competing-edit checks. Bound suppression suggestions to eight construct alternatives and one file-wide alternative, avoiding repeated full-file validation for every finding in a large selection.
- Return InvalidParams for malformed range-formatting requests instead of terminating the native server.
- Avoid stack overflow when detecting cycles in deeply nested include or package dependencies.
- Publish acquired source membership and location catalogs once per batch, preserving overlays and stale-read guards without repeatedly cloning and sorting growing projects.
- Index root membership, unresolved includes, and reverse subfile-parent relationships to avoid repeated workspace-wide searches during project construction. Resolve and encode semantic-token ranges only on requested lines while retaining full-document ownership and typography facts.
- Remove repeated document scans from highlighting and graphics-path lookup. Reuse counted Unicode prefixes when encoding semantic tokens, outlines, diagnostics, navigation, document links, folds, and hints. Convert navigation ranges once per file without changing response order. Keep definition byte spans and batch declaration-container lookup for large result sets. Index command declaration ranges once per request and check glossary rename roots once per affected file. Use binary search for dependency aliases. Share inherited graphics directories across child files so prose edits do not rescan the root for every child. Reuse source mode and definition queries, keep cursor ownership checks local, coalesce identical token purposes, and remap semantic tokens without duplicating the payload. Cache lexical reparse guards and retain proven local edits in files with scoped control-word alphabets. Reject unclosed edited verbatim captures and shortverb edits whose boundaries cannot be proved.
- Distinguish ordinary text, literal content, resource paths, and math structure in semantic highlighting. Keep printed numbers and BibTeX text and dates neutral. Report accurate standard roles for parameters, keywords, functions, TikZ operations, and BibTeX directives; editors choose their colors.
- Add declaration and nested prose typography modifiers. Fix VS Code neutral-text and combined-font fallbacks without prescribing theme colors, and keep fallback parameter and operator patterns out of prose.
- Color Python listings in both native and browser hosts. Allow client grammars for explicit C, C++, JavaScript, TypeScript, JSON, CSS, and HTML headers. Stop embedded grammars at LaTeX closers even when foreign strings or comments are unfinished.
- Parse minted shortcuts, listing TeX escapes, Fancyvrb command characters, and included or excluded comment environments. Preserve authored listing layout, separate package defaults, and honor local overrides and explicit project declarations.
- Protect literal URLs, unfinished inline verbatim, `\detokenize` token lists, and the token printed by `\string` from active TeX features. Preserve definition-bound parameters, subsequent active source, and literal layout; keep `alltt` commands active.
- Restore known lexical switches and literal catcodes at group boundaries. Keep ambiguous language selectors, dynamic listing settings, and unknown conditional catcodes protected. Reparse settings edits fully when body ownership can change.
- Share file-command override checks across highlighting, navigation, completion, and dependency collection. Honor local `\includeonly` overrides without discarding earlier filters. Preserve nested keyval boundaries and keep printed option values neutral.

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
