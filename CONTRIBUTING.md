# Contributing

Install rustup, a native linker, Python 3.11+, and Node for host replay.
`rust-toolchain.toml` pins the compiler, components, and WASM target.
The wasm-bindgen CLI must match `Cargo.lock`:

```sh
cargo install --locked wasm-bindgen-cli --version 0.2.128
cargo build --workspace --locked
cargo run -- lsp
```

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
python3 scripts/check_architecture.py
cargo build -p tex-ls-browser -p tex-ls-parser -p tex-ls-formatter --all-features --locked --target wasm32-unknown-unknown
bash scripts/check_host_parity.sh
```

Host replay needs Bash, Node, and wasm-bindgen. Run timing checks without concurrent
builds. See [performance checks](benches/README.md) for benchmarks.

For keyval signatures or optional-argument lowering, install `pdflatex` and
`pdftotext` and `pdftoppm` (Poppler). The fixtures use geometry, graphicx, PGF/TikZ, listings, pgfplots,
siunitx, enumitem, caption, hyperref, tabularray, amsmath, and empheq, plus the
`example-image` asset from mwe. Then run:

```sh
cargo build --release --locked
bash scripts/check_typeset_stability.sh
```

This check compares extracted text and rendered pages, including graphics, color,
and geometry. Compiler errors fail the check; failed runs retain logs, PDFs and
page images in `target/typeset-stability/`.

Optional parser and corpus checks:

```sh
TEX_LS_PROPERTY_CASES=4096 cargo test -p tex-ls-parser --test property_losslessness
bash scripts/fetch_gate_corpora.sh
cargo build --release --locked
bash scripts/check_gate_baselines.sh
bash scripts/check_reparse_baselines.sh
```

## Changes

Follow the [architecture](docs/development/architecture.md). Include a regression
case for bug fixes and describe how the change was checked. Review snapshot diffs
when updating them with `INSTA_UPDATE=always cargo test --workspace`.

Language metadata lives in `crates/tex-ls-parser/data/`. Run `scripts/gen_*.py`
without arguments to check generated data, or with `--write` to regenerate it.
All generators use pinned sources. CWL, math-symbol and package-name generators
also have offline `--selftest` checks. The BibLaTeX generator fetches the 3.21
model; `--def PATH` accepts an offline copy with the same SHA-256. The package
generator likewise validates the decompressed TeX Live 2025 database hash.
Edit curated signatures directly.

New lint rules belong in `crates/tex-ls-analysis/src/linter/rules/` or its BibTeX
counterpart. Register their metadata and add positive and negative cases. Regenerate
rule references with `cargo run --locked --example docgen`. Update the configuration
schema with `UPDATE_EXPECTED=1 cargo test --test config_schema`.

Retain upstream attribution and licenses. Contributions use the project's MIT license.

Run `cargo deny --all-features check` for advisories, sources, dependency versions
and production licenses. The policy enables all features. Advisory and source checks
include development dependencies;
the license policy excludes test-only dependencies, including the pinned GPL texlab
comparison parser. Adding one to production would subject it to the production policy.

## Distribution

See [distribution](docs/development/distribution.md) for prebuilt executables,
installers, Homebrew, and Mason updates.
