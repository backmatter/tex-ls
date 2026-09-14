# tex-ls

A language server, formatter, and linter for LaTeX and BibTeX.

Get completion, go-to-definition, rename, and diagnostics in your editor.
Format documents and check for mistakes from the command line.

[Editor setup](docs/guide/editor-setup.md) ·
[Formatting](docs/guide/formatting.md) ·
[Linting](docs/guide/linting.md) ·
[Configuration](docs/reference/configuration.md)

## Install

### Homebrew

On macOS or Linux:

```sh
brew install backmatter/tap/tex-ls
```

### Installer

On macOS or Linux:

```sh
curl -fsSL https://github.com/backmatter/tex-ls/releases/latest/download/install.sh | sh
```

On Windows, run in PowerShell:

```powershell
irm https://github.com/backmatter/tex-ls/releases/latest/download/install.ps1 | iex
```

The installers download a prebuilt executable, verify its checksum, and add its
installation directory to your shell configuration. Restart your terminal and editor
after installing. No Rust compiler or administrator access is needed.

### Neovim / Mason

Use the [Backmatter Mason registry](docs/guide/editor-setup.md#neovim), then run
`:MasonInstall tex-ls`.

### Direct download

[Download an executable](https://github.com/backmatter/tex-ls/releases/latest)
for Linux, macOS, or Windows. Intel/AMD and ARM64 builds are available for each.
Extract the archive and put `tex-ls` or `tex-ls.exe` on your `PATH`.

## In your editor

For VS Code, install [tex-ls from the Marketplace](https://marketplace.visualstudio.com/items?itemName=backmatter.tex-ls).
It includes the language server. See the [VS Code guide](editors/vscode/README.md)
for platform availability, installation, and settings.

Configure your editor's LSP client to run `tex-ls lsp`. The
[editor setup guide](docs/guide/editor-setup.md) includes Neovim configuration,
PDF forward and inverse search, and troubleshooting.

## On the command line

```sh
tex-ls format paper.tex                 # format a document
tex-ls format --check paper.tex         # check formatting without writing
tex-ls lint paper.tex bibliography.bib  # report problems
tex-ls lint --fix paper.tex             # apply safe fixes
```

Run `tex-ls init` to create a `tex-ls.toml` configuration file. You can configure
formatting, choose lint rules, and exclude files. See the
[configuration reference](docs/reference/configuration.md).

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for builds and checks,
[architecture](docs/development/architecture.md) for crate boundaries, and
[browser usage](crates/tex-ls-browser/README.md) for WebAssembly bindings.

[MIT license](LICENSE). Based on [Badness](https://github.com/jolars/badness)
by Johan Larsson.
