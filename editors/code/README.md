# Meaning

A language server, formatter, and linter for LaTeX.

## Quick start

1. Build and install `meaning` from the repository root with `cargo install --path . --locked`.
2. Run `npm ci` and `npm run package` in this directory.
3. Install the generated VSIX using VS Code's **Install from VSIX** command.
4. Open a LaTeX file. The extension starts `meaning lsp` from your `PATH`.

No marketplace or native-binary release is published yet.

## Features

- Starts `meaning lsp` automatically when you open supported documents.
- Formats documents using Meaning's deterministic, rule-based formatter.
- Surfaces Meaning diagnostics in the editor.
- Works for LaTeX (`.tex`) and related TeX/BibTeX files.

## Commands

- `Meaning: Restart Server`: stops and restarts the Meaning language server
  (re-reads settings and re-resolves the binary). Useful if the LSP gets wedged
  or after changing settings such as `meaning.version` or
  `meaning.executablePath`.
- `Meaning: Forward Search`: opens your PDF viewer at the cursor's position.
  Requires `meaning.forwardSearch.executable` and `meaning.forwardSearch.args`,
  plus a PDF built with SyncTeX enabled (`latexmk -pdf -synctex=1`). Meaning
  never builds the document itself.

## Binary selection

`meaning.executableStrategy` controls the server executable:

- `environment` is the default and finds `meaning` on your `PATH`.
- `path` uses `meaning.executablePath`.
- `bundled` supports platform-specific extension builds and GitHub release downloads.
  It is intended for future packaged releases.

For the initial source repository, use `environment` or `path`.

## Common setup examples

Use a local binary at a fixed path:

```json
{
  "meaning.executableStrategy": "path",
  "meaning.executablePath": "/usr/local/bin/meaning"
}
```

Use whatever `meaning` is on your `PATH`:

```json
{
  "meaning.executableStrategy": "environment"
}
```

Pin to a specific release:

```json
{
  "meaning.version": "0.2.0",
  "meaning.githubRepo": "backmatter/meaning"
}
```

Use `meaning.releaseTag` only if you need an exact tag override:

```json
{
  "meaning.releaseTag": "v0.2.0"
}
```

## Requirements and troubleshooting

- **NixOS**: the bundled binary won't run because of the dynamic loader path.
  Set `meaning.executableStrategy` to `path` (with `meaning.executablePath`) or
  `environment` if `meaning` is on your `PATH`.
- **Offline, restricted networks, or proxies**: the bundled-binary default works
  without network access. Only the explicit-version download paths
  (`meaning.version`/`meaning.releaseTag`) require GitHub connectivity.
- If a download fall-through fails, the extension shows a warning and falls back
  to looking up `meaning` on the system `PATH`.

## Choosing which features to use

Meaning bundles a formatter, a linter, and language features (hover, completion,
navigation, and so on) behind one language server. Each can be turned off
independently, so you can use just the parts you want:

- `meaning.formatting.enable` (default `true`): use Meaning as a formatter. Set
  to `false` to let another extension own LaTeX formatting without disabling
  Meaning.
- `meaning.diagnostics.enable` (default `true`): show Meaning diagnostics (the
  linter). Set to `false` to suppress **all** squiggles, including the
  syntax/parse errors that a `meaning.toml` `[lint]` selection cannot silence.
- `meaning.languageFeatures.enable` (default `true`): hover, completion,
  signature help, go-to-definition, references, symbols, rename, code actions,
  folding, selection ranges, and document links.

For a formatter-only setup (the common "I just want the formatter" case):

```json
{
  "meaning.diagnostics.enable": false,
  "meaning.languageFeatures.enable": false
}
```

The language server keeps running either way—these are client-side gates—so
formatting stays available and the toggles take effect without reinstalling
anything.

## Settings

Meaning registers itself as the default formatter for `[latex]` files.

- `meaning.formatting.enable`: use Meaning as a formatter (default `true`).
- `meaning.diagnostics.enable`: show Meaning diagnostics/the linter (default
  `true`).
- `meaning.languageFeatures.enable`: enable hover, completion, navigation, and
  the other language features (default `true`).
- `meaning.executableStrategy`: how to locate the `meaning` binary—`bundled`
  (default), `environment`, or `path`.
- `meaning.executablePath`: path to the binary, used only when
  `executableStrategy` is `path`.
- `meaning.version`: version to install (default: `"latest"`)
- `meaning.releaseTag`: advanced exact tag override (takes precedence if
  explicitly set)
- `meaning.githubRepo`: GitHub repo for downloads (default: `"backmatter/meaning"`)
- `meaning.serverArgs`: extra args after `meaning lsp`
- `meaning.serverEnv`: extra environment variables
- `meaning.extraPath`: extra PATH entries prepended for the language server
  process
- `meaning.logLevel`: log level for the language server, mapped to `RUST_LOG`
  (`off`, `error`, `warn`, `info`, `debug`, `trace`; unset by default).
  `meaning.serverEnv.RUST_LOG` overrides this if both are set.
- `meaning.trace.server`: LSP trace level (`off`, `messages`, `verbose`)
- `meaning.lineWidth`, `meaning.indentWidth`: formatter width fallbacks. A
  discovered `meaning.toml` always wins; absent one, your editor's tab size wins
  over `indentWidth`.
- `meaning.texmf`: how the server finds your installed TeX tree (`enabled`,
  `roots`, `useKpsewhich`), for document links, package hover, go-to-definition,
  and installed-set completion. Never affects formatting or linting.
- `meaning.forwardSearch.executable`: the PDF viewer for **Meaning: Forward
  Search**. A program name, *not* a command line — it is spawned directly, so
  flags belong in `args`.
- `meaning.forwardSearch.args`: the viewer's arguments, where `%f` is the file
  the cursor is in, `%p` the root document's PDF, and `%l` the line number
  counting from 1. For zathura: `["--synctex-forward", "%l:1:%f", "%p"]`.

These four are read once, when the server starts, so changing them restarts it
automatically. Where the PDF *lands* is project data and belongs in
`meaning.toml`'s `[build]` section (`pdf-dir`, `pdf-filename`, `root`).

## Security and trust

When `meaning.executableStrategy` is `bundled` (the default), the extension
prefers the binary that shipped inside the VSIX. If no bundled binary is
available, or `meaning.version`/`meaning.releaseTag` is set explicitly, it
downloads from GitHub releases configured by `meaning.githubRepo` (default
`backmatter/meaning`).
