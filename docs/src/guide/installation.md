# Installation

Meaning is currently installed from source. Native binary and editor marketplace
releases are not published yet.

Install the Rust toolchain specified in `rust-toolchain.toml`, then run:

```sh
git clone https://github.com/backmatter/meaning.git
cd meaning
cargo install --path . --locked
meaning --version
```

`cargo install` places the executable in Cargo's binary directory. Ensure that
directory is on your `PATH`.

For a local development build:

```sh
cargo build --workspace --locked
cargo run -- lsp
```

See [Editor setup](editor-setup.md) to connect an LSP client. Formatting and linting
also work directly from the command line.
