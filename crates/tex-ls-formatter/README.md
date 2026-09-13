# tex-ls-formatter

LaTeX and BibTeX formatting over [tex-ls's lossless syntax trees](../tex-ls-parser/README.md).
The crate supports `wasm32-unknown-unknown`; filesystem APIs live in the native CLI.

Entry points: `formatter::format`, `formatter::format_with_style`, `bib::format`,
and `bib::format_with_style`, configured through `FormatStyle`.

Optional `serde` and `schema` features expose configuration serialization and
JSON Schema. Both are off by default; serialized option names use kebab-case.

See [architecture](../../docs/development/architecture.md#the-formatter-and-linter)
for formatting invariants and [the guide](../../docs/guide/formatting.md) for usage.
