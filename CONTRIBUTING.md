# Contributing to Meaning

Thanks for your interest in Meaning, a formatter, linter, and language server
for LaTeX. This guide covers everything you need to build the project, run the
tests, and get a change merged. Contributions of all sizes are welcome, from
typo fixes to new lint rules and parser features.

## Getting set up

Meaning is a Rust workspace (edition 2024): the root package is the `meaning`
CLI/LSP/linter crate, and the publishable `meaning-parser` and
`meaning-formatter` library crates live under `crates/`. The toolchain is pinned
by `rust-toolchain.toml`, so a stable `rustup` install picks up the right
version automatically. The published crates support Rust 1.89 and newer; CI
checks that compatibility floor separately from the pinned development
toolchain.

```sh
git clone https://github.com/backmatter/meaning
cd meaning
cargo build
```

If you use [Nix](https://nixos.org/) with [devenv](https://devenv.sh/), the dev
shell provides the full toolchain plus the profiling and benchmarking tools
(`perf`, `cargo-flamegraph`, `hyperfine`, `cargo-show-asm`, `cargo-llvm-cov`)
and the `go-task` runner. It loads automatically with `direnv`.

The task runner is [go-task](https://taskfile.dev/); `task --list` shows every
available task. The most common ones are below, but every task maps to a plain
`cargo` invocation if you'd rather not install it.

## Building and testing

  | Task                     | Equivalent                                                 | What it does                                             |
  | ------------------------ | ---------------------------------------------------------- | -------------------------------------------------------- |
  | `task build`             | `cargo build`                                              | Dev build.                                               |
  | `task test`              | `cargo test`                                               | Run the whole test suite.                                |
  | `task parser-properties` |                                                            | Run parser losslessness properties at nightly depth.     |
  | `task fmt`               | `cargo fmt`                                                | Format the code.                                         |
  | `task lint`              | `cargo clippy --all-targets --all-features -- -D warnings` | Clippy, warnings as errors.                              |
  | `task check`             |                                                            | Everything CI runs: `fmt-check`, `lint`, `test`, `wasm`. |

Run `task check` before opening a pull request; it mirrors CI exactly.

Meaning uses [insta](https://insta.rs/) for snapshot tests. When a change
deliberately alters formatter or parser output, refresh snapshots with
`task snapshots` and review the diff before committing.

The ordinary test suite runs 256 cases for each parser losslessness property.
`task parser-properties` raises that to 4,096 cases, matching the scheduled
nightly job. When proptest finds a counterexample, reduce it and preserve it as
a readable regression test; the suite deliberately does not persist opaque seed
files.

Performance is first-class. Benchmark before optimizing, and never regress
losslessness for speed.

### Checks that don't run in CI

Two oracles need more than a Rust toolchain, so run them by hand when your
change touches what they cover.

`task typeset:check` compiles `tests/typeset/*.tex` before and after formatting
and diffs the typeset output. The CST oracles cannot see the one risk the
key-value argument flag takes, where a space token is trivia to the CST and
content to TeX, so run this when touching keyval signature data or the
optional-argument lowering. It needs a TeX install.

`task parse-compat` runs [texlab](https://github.com/latex-lsp/texlab)'s parser
as a differential oracle over a corpus, skeletonizing both trees and comparing.
It is a reference we measure against, not one we match, so a divergence is
something to explain rather than automatically fix.

## Project layout

Meaning parses LaTeX into a lossless concrete syntax tree (CST) and builds three
tools on top of it: a formatter (`meaning format`), a linter (`meaning lint`),
and a language server (`meaning lsp`). The architecture follows
[rust-analyzer](https://rust-analyzer.github.io/): a hand-written,
error-tolerant lexer and parser turn LaTeX into a flat token stream, then an
event stream that a tree builder feeds into
[rowan](https://github.com/rust-analyzer/rowan); a **semantic layer** assigns
meaning on top of the generic tree; and incremental recomputation is
[salsa](https://github.com/salsa-rs/salsa)-first.

The [Architecture](https://backmatter.github.io/meaning/development/architecture.html) page in
the book is the full tour, and it is worth reading before a non-trivial change.

The workspace manifests define crate membership. The native root crate owns
CLI and host integrations. Shared syntax, formatting, analysis, and protocol
code live in their corresponding crates under `crates/`. The browser and
playground bindings build separately for wasm. See the architecture documentation for the boundaries.

## Invariants

These properties are held by construction and enforced as test oracles. A change
that breaks one is a bug, not a trade-off.

- Losslessness: `reconstruct(text) == text`, byte for byte.
- Idempotence: `format(format(x)) == format(x)`.
- The formatter is whitespace-only: it changes trivia (whitespace, newlines,
  comments, `.dtx` margins and guards) and nothing else. Content rewrites such
  as `x^{2}` → `x^2` are linter autofixes, not layout.
- Protected regions: verbatim-like content (`verbatim`, `lstlisting`, `\verb`,
  comments) is never altered by the formatter, apart from a document-wide
  line-terminator normalization.

A couple of ground rules keep the design coherent:

- Semantic facts reach the parser only through a narrow, curated admission test;
  when in doubt, a fact belongs in the semantic layer. Parsing is the parser's
  job; layout is the formatter's job. Never paper over a parser mistake in the
  formatter.
- New parser features need corpus and snapshot tests **and** a losslessness
  assertion.

## Making a change

- Prefer trunk-based development and atomic commits. Branch first for
  substantial changes; small fixes can go straight to `main`.
- Follow [Conventional Commits](https://www.conventionalcommits.org/), for
  example `feat(linter): add missing-required-argument rule` or
  `fix(parser): recover at unbalanced brace`.
- Keep commit subjects short (imperative mood, ideally under 60 characters) and
  use the body for rationale. Close issues with `Fixes #123` in the body.
- A rustfmt git hook rewrites unformatted files and aborts the commit, so run
  `cargo fmt` first. Clippy warnings are treated as errors.

All workspace components start at version 0.1.0. Package publishing and release
automation are configured separately from the initial source repository.

### Adding a lint rule

Implement `Rule` under `crates/meaning-analysis/src/linter/rules/`, or `BibRule`
under `crates/meaning-analysis/src/bib/linter/rules/`. Use shared dispatch and
provide a stable ID, description, and triggering examples. Keep `emits_fix`
accurate. Register the module exports, `all_rules()` entry, and ID-list entry
in the corresponding `rules.rs`.

Add unit and public integration coverage, including negative cases. For fixes,
verify exact output, meaning preservation, and repeated application. Regenerate
the reference with `task docs:rules` and review its examples and snapshots.

### Formatter fixtures

Write original fixtures under
`crates/meaning-formatter/tests/fixtures/formatter/` and register them in the
appropriate table in `crates/meaning-formatter/tests/format.rs`. Run
`every_formatter_fixture_is_registered_once` and the table's test. A fixture
slug is not a test name; confirm the test actually exercises the fixture.

Use the latexindent corpus to identify missing shapes, not to copy fixtures.
Compare original probes with `latexindent -` when its output informs a layout
choice. Upstream expected files may use custom settings and are not default
output. Explain differences through Meaning's layout rules.

### Generated data files

Several files in `crates/meaning-parser/data/` are generated from pinned
upstream sources by `scripts/gen_*.py` and guarded by paired `task …:check` and
`:sync` targets: `cwl_signatures.json`, the package and class name lists with
`package_metadata.json`, and `bib_fields.json`. Re-sync them through their task
rather than hand-editing the mechanical facts. `signatures.json`, `colors.json`,
and `tikz_libraries.json` are curated by hand and may be edited directly.

The published `meaning.toml` schema is generated from the Rust configuration
types. Regenerate `meaning.schema.json` with
`UPDATE_EXPECTED=1 cargo test --test config_schema`, and review the diff rather
than editing the JSON by hand.

### Windows CI bites twice

Line endings: the formatter emits LF and tests compare bytes against checked-in
fixtures. When you add a fixture in a new extension under
`crates/*/tests/fixtures/**` or `crates/meaning-parser/tests/corpus/**`, add a
matching `… eol=lf` line to `.gitattributes`. Never normalize line endings in
code to pass a test; fix the attribute instead.

URIs: decode LSP URIs to filesystem paths only through `uri_to_fs_path` and
`path_to_uri` in `crates/meaning-protocol/src/lib.rs`. Tests and snapshots must not assume `/` versus `\`.

## Documentation

User-facing docs are an [mdBook](https://rust-lang.github.io/mdBook/) under
`docs/`. Preview them locally with `task docs:serve` (live reload) or build them
with `task docs`. The linter-rules reference and the benchmark page are
generated; regenerate them with `task docs:rules` and `task bench` respectively
rather than editing the rendered pages by hand.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](https://github.com/backmatter/meaning/blob/main/LICENSE).
