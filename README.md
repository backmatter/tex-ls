# Meaning

Meaning is a LaTeX language server, formatter, and linter written in Rust. It
supports LaTeX documents, BibTeX bibliographies, and package sources, with a
shared analysis engine for native editors and browser applications.

- `meaning lsp` provides completion, hover, definitions, references, rename,
  diagnostics, code actions, and document structure over the Language Server Protocol.
- `meaning format` formats source and can check formatting without changing files.
- `meaning lint` reports syntax errors and lint findings, with optional fixes.

Supported source extensions include `.tex`, `.bib`, `.sty`, `.cls`, `.dtx`, and `.ins`.
Meaning is at version 0.1.0. Its library and browser interfaces may change before 1.0.

## Install

Build from source. Install the Rust toolchain specified in `rust-toolchain.toml`,
then run:

```sh
git clone https://github.com/backmatter/meaning.git
cd meaning
cargo install --path . --locked
```

## Use

```sh
meaning format paper.tex
meaning format --check bibliography.bib
meaning lint paper.tex
meaning lint --fix paper.tex
meaning lsp
```

Configure the formatter and linter in `meaning.toml`. See the
[configuration reference](docs/src/reference/configuration.md).

Any LSP client can start `meaning lsp` over standard input and output. See the
[editor setup guide](docs/src/guide/editor-setup.md). Editor extension source is
included under `editors/`; separate marketplace releases are not required to
use the server.

## Browser and library use

The workspace contains `meaning-parser`, `meaning-formatter`, `meaning-analysis`,
`meaning-protocol`, and the `meaning-browser` and `meaning-wasm` adapters. Shared
language code supports WebAssembly; native filesystem access and process execution
stay in the native host.

Build the browser adapter with:

```sh
cargo build -p meaning-browser --release --target wasm32-unknown-unknown --locked
```

Generate JavaScript bindings with the `wasm-bindgen` CLI version matching
`Cargo.lock`. Browser applications supply document updates and project inputs;
collaboration, persistence, and transport remain application responsibilities.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md), the [architecture decision](docs/adr/0001-independent-language-service.md),
and [TODO.md](TODO.md) for development guidance and remaining work.

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```

[MIT license](LICENSE).

Meaning is based on [Badness](https://github.com/jolars/badness) by Johan Larsson.
