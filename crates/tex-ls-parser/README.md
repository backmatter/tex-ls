# tex-ls-parser

Lossless LaTeX and BibTeX parsing for [tex-ls](../../README.md). Malformed input
produces a tree and errors; reconstructing the tree preserves every input byte.
The crate supports `wasm32-unknown-unknown`.

Entry points: `parser::parse`, `parser::parse_with_flavor`, `bib::parse`,
`semantic::SemanticModel::build`, and typed wrappers in `ast`. The `semantic` module
contains signatures, definitions, labels, expl3 facts, and outlines.

See [architecture](../../docs/development/architecture.md#the-parser) for parsing
and incremental-equivalence requirements.
