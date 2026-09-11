# Agent instructions

- Fix syntax errors in the parser, not in formatter or linter workarounds.
- Parsing must preserve every input byte and make progress on malformed input.
  Parse shape depends only on source text and explicit project declarations.
- Incremental reparsing must match a full parse in tree and errors. Fall back
  to full parsing when equivalence cannot be proved.
- Formatting changes trivia only and must be idempotent. Preserve protected
  bodies and comments except configured line endings. A consumed space versus
  single newline must not decide layout.
- Safe lint fixes must preserve meaning without relying on later formatting.
- Keep filesystem access, threads, and processes in the native host. Shared
  parser, formatter, analysis, and protocol runtime code must remain wasm-compatible.
- Run `task check` before handing off substantial code changes. Run
  `task typeset:check` when changing keyval signatures or optional-argument lowering.
