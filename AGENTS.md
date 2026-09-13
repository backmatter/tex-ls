# Agent instructions

- For LSP implementation work, follow `docs/development/architecture.md` and
  keep the architecture and user-facing documentation current with changes.
- Fix syntax errors in the parser, not in formatter or linter workarounds.
- Parsing must preserve every input byte and make progress on malformed input.
  Parse shape depends only on source text and explicit project declarations.
- Incremental reparsing must match a full parse in tree and errors. Fall back
  to full parsing when equivalence cannot be proved.
- Formatting changes trivia only and must be idempotent. Preserve protected
  bodies and comments except configured line endings. A consumed space versus
  single newline must not decide layout.
- Safe lint fixes must preserve semantics without relying on later formatting.
- Keep filesystem access, threads, and processes in the native host. Shared
  parser, formatter, analysis, and protocol runtime code must remain wasm-compatible.
- Before handing off substantial code changes, run `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
  `cargo test --workspace --all-features --locked`, and
  `python3 scripts/check_architecture.py`. Build `tex-ls-browser`, `tex-ls-parser`
  and `tex-ls-formatter` for `wasm32-unknown-unknown` with all features.
- Run `bash scripts/check_host_parity.sh` when changing shared language or host code.
  Build the release executable and run `bash scripts/check_typeset_stability.sh`
  when changing keyval signatures or optional-argument lowering.
- Use native Cargo, rustup, Python and Node commands. Do not add a task runner,
  environment manager, bootstrap wrapper, release system or documentation framework.
