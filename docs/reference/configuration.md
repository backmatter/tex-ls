# Configuration

tex-ls reads `tex-ls.toml`. All keys are optional and use kebab-case. Unknown keys
and sections are errors. Run `tex-ls init` to write a commented configuration with
defaults.

```toml
[format]
line-width = 80
indent-width = 2
wrap = "reflow"

[lint]
ignore = ["missing-nonbreaking-space"]
```

## Discovery

For each input, tex-ls searches upward from the file's directory for the first
`tex-ls.toml`. It stops at a directory containing `.git`.

Without a project configuration, a non-empty `TEX_LS_CONFIG` selects a configuration
file and overrides the global configuration. Otherwise tex-ls uses the first
existing file in this order:

1. `$XDG_CONFIG_HOME/tex-ls/config.toml`
2. `~/.config/tex-ls/config.toml`
3. The platform configuration directory, such as `%APPDATA%\tex-ls\config.toml`
   on Windows or `~/Library/Application Support/tex-ls/config.toml` on macOS.

These files supply a complete configuration; tex-ls does not merge them with a
project file. Their relative exclusion patterns use the CLI working directory or
the LSP document's directory. Restart the server after changing global defaults.
Without a configuration file, tex-ls uses built-in defaults.

`--config PATH` selects a file. `--no-config` disables configuration discovery.
Individual flags such as `--line-width`, `--wrap`, and `--select` override the
corresponding values for one CLI run.

## Editor support

The [JSON Schema](../../tex-ls.schema.json) provides TOML completion and validation.
The schema at `https://raw.githubusercontent.com/backmatter/tex-ls/main/tex-ls.schema.json`
tracks `main`. Use a revision-specific URL when you need to match a particular build.

### VS Code with Even Better TOML

Add this association to `settings.json`:

```jsonc
{
  "evenBetterToml.schema.associations": {
    "^(.*/)?tex-ls\\.toml$": "https://raw.githubusercontent.com/backmatter/tex-ls/main/tex-ls.schema.json"
  }
}
```

### Inline schema directive

TOML tools that support schema directives accept:

```toml
#:schema https://raw.githubusercontent.com/backmatter/tex-ls/main/tex-ls.schema.json
```

This also selects the schema for a global `config.toml`. Other editors can use the
same URL in their TOML schema settings.

## Top level

### `exclude`

An array of gitignore-style patterns for directory discovery in `format` and `lint`.
Project patterns resolve relative to `tex-ls.toml`. Setting this replaces the
default `[".git/"]`; `--exclude` patterns are always added.

```toml
exclude = ["vendor/", "old-drafts/"]
```

Exclusions apply within their configuration root. Matching handles canonical
Windows paths and short names consistently, including watcher events for deleted
files. Files outside that root do not inherit its exclusions.

### `extend-exclude`

Patterns added to `exclude`, or to the built-in defaults when `exclude` is unset.
The default is `[]`.

```toml
extend-exclude = ["build/"]
```

## `[format]`

### `line-width`

The maximum line width before wrapping. Accepts integers from 1 to 1000; defaults
to `80`. The CLI flag is `--line-width`.

### `indent-width`

Spaces per indentation step. Accepts integers from 1 to 1000; defaults to `2`.
The CLI flag is `--indent-width`.

### `item-indent`

Controls continuation lines relative to `\item`. Defaults to `"hang"`.

| Value | Behavior |
| --- | --- |
| `hang` | Align under the body following a bare `\item ` |
| `indent` | Add one indentation step from the `\item` column |
| `none` | Align with `\item` |

Labels and Beamer overlays do not change the hanging indent.

### `wrap`

Controls paragraph line breaks. Defaults to `"reflow"` for every file type.

| Value | Behavior |
| --- | --- |
| `reflow` | Fill lines up to `line-width` |
| `stable` | Prefer authored breaks while fitting the configured width |
| `sentence` | Put each sentence on its own line, ignoring width |
| `semantic` | Break at sentences and retain authored line breaks |
| `preserve` | Retain authored line breaks |

Sentence boundaries use punctuation and a language-specific abbreviation list.
Configure them with [`lang`](#lang) and
[`no-break-abbreviations`](#no-break-abbreviations).
In `sentence` mode, postpositive citations such as `\parencite`, `\citep`, and
`\autocite` stay with the preceding sentence, even across an authored newline.
Textual citations such as `\textcite` and `\citet` may start the next sentence.
Plain `\cite` follows the source's line break. `semantic` preserves authored clause
breaks but does not invent them.

`stable` compares layouts by overflow, underflow below `line-width - 15`, changed
breaks, break displacement, raggedness, and line count, in that order. The last line
has no underflow penalty. Blank lines and command-only lines bound each run. The
soft target is fixed; code-like statements use greedy wrapping.

All modes preserve comments, protected bodies, `.dtx` margins, and docstrip guards
except configured line endings. Expl3 regions use their own layout rules, and lines
containing only commands retain their line breaks.

```toml
[format]
wrap = "stable"
```

### `math-wrap`

Controls display-math line breaks in `\[…\]`, `$$…$$`, and single-formula
environments such as `equation`. It does not affect inline math or alignment grids
such as `align`, `gather`, and matrices. Defaults to `"auto"`.

| Value | Behavior |
| --- | --- |
| `auto` | Use `preserve` when `wrap` is `preserve`; otherwise use `break` |
| `preserve` | Retain authored breaks and normalize spacing within each line |
| `single-line` | Use one line, even if it exceeds `line-width` |
| `break` | Break long formulas before top-level relations and binary operators |

### `line-ending`

Sets line-ending bytes throughout the document, including protected bodies.
Defaults to `"auto"`.

| Value | Behavior |
| --- | --- |
| `auto` | Use CRLF if the first line break is CRLF; otherwise use LF |
| `lf` | Use `\n` |
| `crlf` | Use `\r\n` |
| `native` | Use CRLF on Windows and LF elsewhere |

### `lang`

A language code such as `en`, `de`, or `pt-BR` selects the abbreviation profile for
`sentence` and `semantic` wrapping. Profiles cover English, Czech, German, Spanish,
and French. Region subtags are ignored; unknown or unset languages use English.
tex-ls does not detect the language from `babel` or `polyglossia`.

```toml
[format]
lang = "de"
```

### `no-break-abbreviations`

A table of string arrays that extends the built-in abbreviation lists. Keys are
language codes or `default`, which applies to every document. Listed abbreviations
do not end a sentence. The default is an empty table.

```toml
[format.no-break-abbreviations]
default = ["ibid."]
de = ["bzw.", "Abb."]
```

## `[lint]`

[LaTeX](linter-rules.md) and [BibTeX](bib-linter-rules.md) share rule selection.
Each rule's reference states whether it is enabled by default. Unknown rule IDs
are reported when linting.

### `select`

An optional array of rule IDs. When set, only these rules run. When unset, all
default-enabled rules run. Explicit selection can enable opt-in rules.

```toml
[lint]
select = ["deprecated-command", "dash-length"]
```

### `ignore`

An array of rule IDs removed from the selected or default set. Defaults to `[]`.

```toml
[lint]
ignore = ["missing-nonbreaking-space"]
```

### `[lint.external]`

Filters LSP compiler-log diagnostics by source and severity. An empty list suppresses
all external findings. These settings do not run a compiler. The defaults are:

```toml
[lint.external]
sources = ["compiler"]
severities = ["error", "warning", "information"]
```

Compiler messages describe the last build. Source freshness is unverified unless
the host knows the build's source revision.

## `[build]`

The language server uses this section to locate compiler artifacts and the PDF for
[forward search](../guide/editor-setup.md#forward-and-inverse-search).
CLI formatting and linting do not read it.

### `aux-dir`

The directory containing `.aux`, `.log`, and `.fls` files. Relative paths use the
root document's directory. When unset, tex-ls expects each document's artifacts
beside its source. When set, only this directory supplies artifacts, including after
deletion; stale sibling files do not override it. The native host watches configured
artifacts inside and outside the workspace even when the editor excludes their
directory from watching. Missing artifacts are observed after the first build.

### `pdf-dir`

The PDF directory, relative to the root document's directory unless absolute.
Defaults to the root document's directory.

### `pdf-filename`

Overrides the PDF's basename. Use `pdf-dir` for its directory. tex-ls appends `.pdf`
when no extension is present. When unset, the name follows `job-name` or the root
source's stem.

### `job-name`

The compiler's job-name stem. For example, `"thesis"` selects `thesis.aux`,
`thesis.log`, `thesis.fls`, and by default `thesis.pdf`. Included child AUX files keep
their names. `pdf-filename` overrides only the PDF. tex-ls reads these files from
your external build.

### `root`

The compiled root source, relative to `tex-ls.toml` unless absolute. When unset,
tex-ls infers roots from source dependencies and document markers. Opening a chapter
also follows literal `% !TeX root = ../main.tex` hints.

An explicit root loads its dependencies and selects its compiler/PDF context before
inferred roots. It does not merge independent source namespaces or hide conflicting
root hints. Use `tex-ls.inspectProject` to inspect ambiguity and
`tex-ls.inspectAcquisition` for loading failures.

```toml
[build]
root = "main.tex"
aux-dir = "out"
pdf-dir = "out"
job-name = "thesis"
```

## `[commands]`

Declares reference or citation wrappers whose first braced argument contains keys.
Keys are command names without a leading backslash.

```toml
[commands.eqrefs]
like = "cref"

[commands.projectcite]
like = "parencite"
```

### Command `like`

A curated reference or citation command whose key behavior the wrapper copies.
`ref` and `eqref` accept one label; `cref` and citation commands split keys on commas;
`nocite` also supports `*`.

Declarations affect linting, navigation, rename, and completion. They do not expand
macros or change argument attachment, arity, or layout. Match the wrapper's accepted
keys, even if it calls a different command internally. Empty entries, invalid names,
unknown targets, and reclassification of built-in commands are errors.

## `[environments]`

Declares custom environments and command aliases for their delimiters. This section
changes parsing in the formatter, linter, and language server. Changes trigger a
project reparse.

```toml
[environments.myenv]
like = "align"

[environments.eqnarray]
begin = ['\bea']
end = ['\eea']

[environments.split]
begin = ['\bsplit']
```

Use TOML literal strings such as `'\bea'` to avoid escaping backslashes. Names
without a leading backslash are also accepted. Delimiter aliases still obey parser
boundaries; an unreachable closing alias cannot force a pairing.

Invalid declarations are configuration errors:

- Empty entries or unknown `like` targets.
- Delimiter aliases for verbatim environments, environments taking arguments, or
  environments without known behavior.
- Duplicate aliases or aliases claimed by more than one environment.
- Written-out delimiters such as `'\end{split}'`, invalid control words such as
  `'\b ea'` or `'\bea2'`, or existing built-in commands such as `'\emph'`.

### `like`

An optional built-in environment name. It supplies the complete behavior, including
math, alignment, and verbatim handling. Individual properties cannot be configured.
For example, `like = "lstlisting"` protects a custom environment's body from reflow
and linting.

```toml
[environments.mycode]
like = "lstlisting"
```

### `begin`

An array of command names that act as opening delimiters. Defaults to `[]`. Any
opening alias can pair with any closing alias or the written-out `\end{…}`;
the lists need not have equal lengths.

Use declarations when tex-ls cannot read a definition. Simple aliases defined by
`\newcommand` or `\def` in the same file are recognized without configuration.

### `end`

An array of command names that act as closing delimiters. Defaults to `[]`. Closing
aliases can pair with a written-out `\begin{…}`, so either side can be declared alone.

```toml
[environments.eqnarray]
begin = ['\bea', '\beqa']
end = ['\eea']
```

Configure [TEXMF discovery](../guide/editor-setup.md#texmf-discovery) in editor settings.

## Option completion schemas

`[options.<command-or-environment>]` defines optional-argument completion keys.
Each key maps to an array of suggested values; an empty array offers only the key.
A project schema replaces the curated schema for that name. Use
`[options."package:<name>"]` for `\usepackage` options. These settings affect only
completion.

```toml
[options.mygraphic]
width = []
mode = ["draft", "final"]
"α key" = ["one", "two"]

[options."package:mytheme"]
color = ["light", "dark"]
```

Browser embeddings use the same maps under `settings.declarations.options`.

## TEXINPUTS

The native host also indexes directories in `TEXINPUTS`, using the platform's
path-list separator. A plain directory searches its immediate files; a trailing
`//` searches recursively. Windows short directory names such as `USER~1` work
as literal paths. Other Kpathsea expansion requires the installed TeX tools.
