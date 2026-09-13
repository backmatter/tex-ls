# Editor setup

Configure your LSP client to run `tex-ls lsp` over stdio for LaTeX and BibTeX.
Untitled buffers support local completion; save them to enable filesystem context.

## Neovim

With Neovim 0.11+:

```lua
vim.lsp.config["tex-ls"] = {
  cmd = { "tex-ls", "lsp" },
  filetypes = { "tex", "latex", "plaintex", "bib" },
  root_markers = { "tex-ls.toml", ".git" },
}
vim.lsp.enable("tex-ls")
```

Use synchronous formatting to avoid applying edits after a buffer changes:

```lua
vim.lsp.buf.format({ name = "tex-ls", async = false, timeout_ms = 5000 })
```

An asynchronous integration must check the buffer's changed tick before applying
edits. Other editors can use their generic LSP integration with the same command.

## Settings

Supply `initializationOptions` or `workspace/didChangeConfiguration` as a bare
object or under a `tex-ls` key. `lineWidth` and `indentWidth` are formatter
fallbacks: discovered `tex-ls.toml` takes precedence, and otherwise the formatting
request's tab size overrides `indentWidth`. See the
[configuration reference](../reference/configuration.md) for project settings.

### TEXMF discovery

Editor settings control installed-package discovery for links, hover, definitions,
and completion. They do not affect CLI formatting or linting.

```json
{ "texmf": { "enabled": true, "roots": ["/opt/texmf"], "useKpsewhich": true } }
```

- `enabled`: scan installed trees; default `true`. Disabled resolution stays local.
- `roots`: extra roots searched before discovered ones; default `[]`.
- `useKpsewhich`: discover roots using `kpsewhich`; default `true`. When false,
  discovery uses default-path heuristics.

## Forward and inverse search

Compile the PDF with SyncTeX enabled, for example `latexmk -pdf -synctex=1`.
tex-ls passes source/PDF paths and a line number to your viewer; the viewer performs
SyncTeX mapping. Configure PDF location in [`[build]`](../reference/configuration.md#build).
Unsaved edits can disagree with the last compiled line numbers.

### Configuring the viewer

Set `forwardSearch` in editor settings:

```json
{
  "forwardSearch": {
    "executable": "zathura",
    "args": ["--synctex-forward", "%l:1:%f", "%p"]
  }
}
```

`executable` is a program name, with flags supplied separately in the required
`args` array. It is spawned directly without a shell.

| Placeholder | Value |
| --- | --- |
| `%f` | Current source file |
| `%p` | Root document's PDF |
| `%l` | One-based line number |
| `%%f` | Literal `%f` |

Arguments wrapped entirely in `"` lose those quotes and bypass substitution.

| Viewer | Executable | Arguments |
| --- | --- | --- |
| zathura | `zathura` | `["--synctex-forward", "%l:1:%f", "%p"]` |
| Okular | `okular` | `["--unique", "file:%p#src:%l%f"]` |
| SumatraPDF | `SumatraPDF` | `["-reuse-instance", "%p", "-forward-search", "%f", "%l"]` |
| Skim | `displayline` | `["%l", "%p", "%f"]` |
| Evince | `evince-synctex` | `["-f", "%l", "%p", "\"code -g %f:%l\""]` |
| qpdfview | `qpdfview` | `["--unique", "%p#src:%f:%l:1"]` |

### Triggering forward search

Send `workspace/executeCommand` with command `tex-ls.forwardSearch` and one argument
`{"uri":"file:///path/main.tex","position":{"line":2,"character":0}}`.
Positions use zero-based LSP lines. The native server returns:

| `outcome` | Additional fields / action |
| --- | --- |
| `launched` | `source`, `pdf`, one-based `line1` |
| `unconfigured` | Configure a viewer |
| `missingPdf` | `path`; build the document |
| `unsupportedSource` | A local source path is required |
| `ambiguousRoot` | `candidates`; configure `[build].root` |
| `launchFailed` | `message` |

### Inverse search

Configure your viewer to run `tex-ls inverse-search --input "%f" --line1 "%l"`,
using its own placeholders. For zathura:

```sh
zathura --synctex-editor-command "tex-ls inverse-search --input %{input} --line1 %{line}"
```

Use `--line0` for zero-based viewers; exactly one line flag is required. The project
must be open in an editor supporting `window/showDocument`. With multiple servers,
the longest matching workspace root wins.

`forwardSearch.ipcDir` overrides `$TEX_LS_IPC_DIR`, the per-user runtime directory,
and the temporary-directory fallback. Use it when the viewer and editor need a
shared IPC location. Keep Unix socket paths short (about 100 bytes maximum).

## Editing features

- Completion, signature help, navigation, references, rename, diagnostics, and
  quick fixes cover LaTeX and BibTeX. Diagnostic links open the rule catalogue.
- Semantic highlighting and folding refresh after declaration/package changes
  when the client supports refresh requests.
- Color pickers support literal `\definecolor`/`\providecolor` in `HTML`, `rgb`,
  `RGB`, and `gray`. Dynamic expressions are skipped. HTML/RGB use 8-bit channels;
  a chromatic selection changes `gray` to `rgb`.
- Linked environment editing updates paired literal names. Clients without it can
  request `refactor.rewrite.environment` code actions.
- **Add column at end** appends a centered column and empty cells to statically
  understood `tabular`, `tabular*`, and `array` environments.
- Workspace symbol search returns up to 256 matches; narrow the query for more
  specific results. Multiple-range formatting merges overlapping expanded blocks.

Custom option keys and values belong in
[`[options]`](../reference/configuration.md#option-completion-schemas).

## Project troubleshooting

Chapters follow literal `% !TeX root = ../main.tex` hints and `[build].root`.
Source dependencies, installed metadata, and compiler artifacts load in the
background. Watched changes refresh diagnostics and AUX-based hints.

Use `workspace/executeCommand` with `tex-ls.inspectProject` and
`[{"uri":"file:///path/main.tex"}]` to inspect roots and dependency edges.
`tex-ls.inspectAcquisition` takes no arguments and reports pending work, read
counts, source-read failures, and installation issues. A TeX lookup times out after
two seconds per process; local editing remains available during discovery.
