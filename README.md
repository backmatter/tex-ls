# tex-ls

A LaTeX and BibTeX language server, formatter, and linter written in Rust.
Supports `.tex`, `.bib`, `.sty`, `.cls`, `.dtx`, and `.ins` files.
The `.def` and `.lco` extensions use the `.tex` pipeline; `.bibtex` uses `.bib`.

## Install

Install [rustup](https://rustup.rs/) and a native linker, then build from source.
`rust-toolchain.toml` selects the required Rust toolchain.

```sh
git clone --branch v0.1.0 --depth 1 https://github.com/backmatter/tex-ls.git
cd tex-ls
cargo install --path . --locked
tex-ls --version
```

These commands build version 0.1.0. You can also install from its unpacked source
archive on the [releases page](https://github.com/backmatter/tex-ls/releases).
Ensure Cargo's binary directory is on your `PATH`.
Version 0.1.0 is an early release; library and browser APIs may change before 1.0.
Prebuilt binaries and crates.io packages are not provided.

## Use

```sh
tex-ls format paper.tex
tex-ls format --check bibliography.bib
tex-ls lint paper.tex
tex-ls lint --fix paper.tex
tex-ls lsp
```

The language server provides completion, navigation, rename, diagnostics, code
actions, semantic highlighting, and document structure over stdio. See
[editor setup](docs/guide/editor-setup.md).

Run `tex-ls init` to create `tex-ls.toml`, or see the
[configuration reference](docs/reference/configuration.md).
[Formatting](docs/guide/formatting.md) and [linting](docs/guide/linting.md) describe
checks, fixes, and suppression comments.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for builds and checks,
[architecture](docs/development/architecture.md) for crate boundaries, and
[browser usage](crates/tex-ls-browser/README.md) for WebAssembly bindings.

[MIT license](LICENSE). Based on [Badness](https://github.com/jolars/badness)
by Johan Larsson.
