# Editor setup

## VS Code

In VS Code, open **Extensions**, search for **tex-ls** by **Backmatter**, and
click **Install**. Open your document's folder and a `.tex` or `.bib` file.
Allow workspace trust for your own projects so tex-ls can start.

The [extension guide](../../editors/vscode/README.md) explains suggestions,
fixes, and formatting. You do not need to install tex-ls separately.
Creating and viewing PDFs needs a separate tool.

## Other editors

The sections below cover manual setup and advanced editor integration.
Configure your editor's LSP client to run `tex-ls lsp` over stdio for LaTeX
and BibTeX. Save new files to enable navigation between project files.

## Neovim

Install tex-ls with [Homebrew, an installer, or a direct download](../../README.md#install).
To install through Mason, add the Backmatter registry to your setup:

```lua
require("mason").setup({
  registries = {
    "github:backmatter/mason-registry",
    "github:mason-org/mason-registry",
  },
})
```

Run `:MasonInstall tex-ls`. Mason adds its executables to Neovim's PATH.

With Neovim 0.11+, configure the language server:

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
fallbacks: explicitly configured `[format] line-width` and `indent-width` take
precedence individually. Otherwise the formatting request's tab size overrides
`indentWidth`. A build-only config preserves editor formatting preferences. See the
[configuration reference](../reference/configuration.md) for project settings.

### TEXMF discovery

Editor settings control installed-package discovery for links, hover, definitions,
and completion. They do not affect CLI formatting or linting.

```json
{ "texmf": { "enabled": true, "roots": ["/opt/texmf"], "useKpsewhich": true } }
```

- `enabled`: scan installed trees; default `true`. Disabled resolution stays local.
- `roots`: extra roots searched before discovered ones; default `[]`. Relative paths
  resolve against the workspace folder supplying the configuration. Initialization
  options with relative roots require exactly one workspace folder; multi-folder
  clients should supply scoped `workspace/configuration` responses or absolute roots.
- `useKpsewhich`: discover roots using `kpsewhich`; default `true`. When false,
  discovery falls back to default-path heuristics if no configured roots exist.
- `explicitOnly`: default `false`. When `true`, only `roots` are indexed: no
  `kpsewhich`, heuristic system roots, or inherited `TEXINPUTS`. Missing roots stay
  empty until they are materialized. This setting takes precedence over `useKpsewhich`.

Active installations are checked in the background approximately every five seconds.
Filename database (`ls-R`) changes, root creation/deletion, and nested directory
changes in trees without databases refresh the index without a server restart.
Requests use the last complete index during refresh. Automatic root discovery runs
once per settings value; change settings or restart after relocating a system
installation. Package names from the bundled CTAN catalog can still appear in
completion; they are suggestions, not evidence that a package is installed.

### Build diagnostics

Set `diagnostics.compiler` to `false` in initialization options or editor settings
when another build integration publishes compiler errors. This suppresses compiler
log diagnostics only: native source linting, AUX numbers, and recorder information
remain available. Changes apply during the session, including clearing old reports.
Project `[lint.external]` filters still apply when compiler diagnostics are enabled.

```json
{
  "texmf": { "roots": [".local/texmf"], "explicitOnly": true },
  "diagnostics": { "compiler": false }
}
```

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

Go to Definition follows static command definitions in the document, included files,
and loaded package sources. For known built-in commands it can also load the LaTeX
kernel, `latex.ltx`, from the selected TEXMF tree on demand. This keeps ordinary
typing and completion free of the kernel source-loading cost. The index refreshes automatically
when tex-ls adds support for more source file types. Navigation recognizes literal
command declarations, control symbols, aliases such as `\let`, and expl3 `N`-name
command definitions, including literal `c` names. Clicking `\begin` or `\end` opens the command definition;
clicking the name in braces opens the environment definition. Environments defined
as a pair of `\name` and `\endname` commands are supported too.
Hover and Ctrl-click treat the command and the braced name as separate targets;
the braces themselves have no target.
If an environment has no source declaration, Go to Definition on its name jumps
to the matching `\begin` or `\end` name when the parser proves the pair. Its hover
identifies the opposite delimiter and line. Incomplete and mismatched pairs do not
offer that navigation target.
For a shared source with several candidate roots, Go to Definition returns the
possible destinations from those roots. Rename remains unavailable when the
namespace is ambiguous.
Installed sources must be available through TEXMF discovery. Commands implemented by the engine or created through expansion may have
no source location. Workspace diagnostics skip installed package sources and paths
excluded by `tex-ls.toml`. Opening an installed package or `latex.ltx` through
navigation does not add diagnostics for that source to the Problems panel. Local
package files still receive diagnostics.

Completion, hover, and argument hints use definitions from those same sources.
Files displayed by listing commands or linked as graphics remain navigable, but
their contents do not define commands for the document.
Supported declarations include `\NewCommandCopy` and its variants,
`\NewExpandableDocumentCommand` and its variants, `\newif`, `\newtheorem`,
and `\declaretheorem`. Aliases copy the signature available at their declaration;
tex-ls does not invent argument hints when it only knows a command's name.
Literal etoolbox declarations such as `\csdef` and `\csletcs`, counter commands
such as `\thesample` from `\newcounter{sample}`, and expl3 variable, variant,
and conditional declarations are also recognized. Generated names can be navigated
to, but cannot be renamed automatically. Unsupported argument specifications retain
their command or environment name without partial argument hints.
For example, `\def\sample#1;{#1}` remains navigable but has no brace-argument snippet.
Literal `\InputIfFileExists` paths are followed like `\input` paths.
Biblatex's literal `style`, `bibstyle`, and `citestyle` options support file navigation,
completion, and highlighting. A `style` value can open both its `.bbx` and `.cbx` files.
Style names may continue across a TeX comment and the following indentation.
Navigation and highlighting retain each fragment's range. Completion leaves such
names unchanged because replacing one fragment would damage the full name.

The `unknown-command` rule warns while editing when a command is absent from core
metadata, loaded-package metadata, and scanned definitions. A package's completion
suggestions do not suppress warnings when that package is not loaded. Loads in
included sources and unconditional package dependencies count too.
Compilation is not required. Open `.ltx` and `.tikz` documents receive the same
unknown-command checks as `.tex` documents. See
[lint suppression comments](linting.md#rules) for package-generated commands that the
static checker cannot recognize.

Hover, completion details, and signature help share documentation cards. Common
commands and environments have short descriptions and manual links. Recognized TeX
and e-TeX primitives get reference cards without invented brace arguments. Hover
also works on control symbols, `\begin`/`\end`, and the head of `\verb`.

Source-defined commands and environments show definition links. A unique definition
also shows nearby comment lines and a bounded source excerpt, including comments
above declarations split across lines. Package commands link to package manuals
when their provenance is known. Loaded `.dtx` macro
and environment documentation is shown when its code association is known. Local
redefinitions use their own source information. tex-ls does not download manuals,
execute TeX, or interpret source comments as Markdown commands. Package/class
metadata and resolved file arguments, including themes and bibliography styles,
provide source links. Descriptions are curated and do not cover every package macro;
source information remains available for the other statically resolved names.

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

## Syntax and semantic highlighting

Your theme chooses the colors. tex-ls classifies all LaTeX commands consistently,
including custom commands, unknown commands, and both `\begin` and `\end`.
Literal environment names share one classification. Package loading does not
change command colors; unknown-command warnings use diagnostic underlines.
Comments and verbatim bodies retain their syntax highlighting.
With semantic highlighting enabled, the literal body of `\verb|...|` and a
flat `\texttt{...}` argument use the theme's string color. They are printed
examples: names inside them are not commands or file references for Ctrl-click.
A TeX command nested inside `\texttt{...}` remains a command.

The extension supplies a TextMate grammar for immediate highlighting and for editors
with semantic highlighting disabled. Its command and environment scopes match the
server's semantic classifications. Semantic information adds roles for reference
keys, such as labels and citations. Other LaTeX extensions can also supply a grammar;
VS Code's **Developer: Inspect Editor Tokens and Scopes** command shows the active
syntax scopes, semantic token, and theme rule at the cursor.

### File and module arguments

Literal arguments use the same file roles for highlighting and Ctrl-click/F12:

| Arguments | Examples | Target |
| --- | --- | --- |
| Classes and packages | `\documentclass`, `\usepackage`, `\RequirePackage` | `.cls` and `.sty` files |
| Beamer themes | `\usetheme`, `\usecolortheme`, `\usefonttheme`, `\useinnertheme`, `\useoutertheme` | The matching `beamer…theme<name>.sty` file |
| Graphics libraries | `\usetikzlibrary`, `\usepgflibrary` | `tikzlibrary<name>.code.tex` or `pgflibrary<name>.code.tex` |
| Bibliography styles | `\bibliographystyle`, `\RequireBibliographyStyle`, `\RequireCitationStyle` | `.bst`, `.bbx`, or `.cbx` files |
| Bibliography language mappings | Second argument of `\DeclareLanguageMapping` | `.lbx` file |
| Source files | `\input`, `\include`, `\import`, `\subimport`, `\subfile`, related loaders | The referenced source file |
| Bibliography databases | `\bibliography`, `\addbibresource` | `.bib` files |
| Images and external content | `\includegraphics`, `\includesvg`, `\lstinputlisting`, related loaders | The referenced asset |

Module names use the theme's namespace color; file paths use its string color.
Names keep their coloring even if a file is missing. Navigation requires a resolved
local or installed target. Comma-separated lists link each name separately, and
local files take precedence over installed files. Section titles and other prose
arguments remain ordinary text. Macro-expanded names are not guessed.
Biblatex `.bbx`, `.cbx`, and `.lbx` files open as LaTeX package code.
