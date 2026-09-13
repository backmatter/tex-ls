# Linting

`tex-ls lint` reports diagnostics with source snippets. It exits non-zero when
any diagnostic is found.

```sh
tex-ls lint paper.tex
cat paper.tex | tex-ls lint   # stdin
```

## Parse diagnostics

The linter reports parser recovery errors with rule ID `parse`. Parsing continues
after malformed input, so a file can have several independent findings. Rule
selection and suppression comments cannot hide parse diagnostics.

## Rules

Rules have stable IDs used in diagnostics, configuration, and suppression comments.
See the [LaTeX](../reference/linter-rules.md) and
[BibTeX](../reference/bib-linter-rules.md) catalogues, or print a rule's explanation:

```sh
tex-ls lint --explain deprecated-command
```

Most rules are on by default. The catalogues identify opt-in rules. Select them
through the `[lint]` table in
`tex-ls.toml` or the matching `--select`/`--ignore` CLI flags; see the
[Configuration reference](../reference/configuration.md#lint).

Suppress a rule at one site with a comment directive:

```tex
% tex-ls-lint skip deprecated-command: legacy code
{\bf here}
```

Choose the suppression scope:

  | Scope              | Directive                                                |
  | ------------------ | -------------------------------------------------------- |
  | The next construct | `% tex-ls-lint skip <rule>: <reason>`                   |
  | A region           | `% tex-ls-lint off <rule>` … `% tex-ls-lint on <rule>` |
  | The whole file     | `% tex-ls-lint skip-file <rule>: <reason>`              |

Omit `<rule>` to suppress all rules over that span. An unmatched `off` runs to the
end of the file. The optional `: reason` documents your choice.

Bare directives also disable formatting:
`% tex-ls skip`, `% tex-ls off` / `% tex-ls on`, and `% tex-ls skip-file`.
For layout only, use the `% tex-ls-format` spellings described in
[Formatting](formatting.md#turning-the-formatter-off).

In `.bib` files, put directives in `@comment` entries:

```bib
@comment{tex-ls-lint skip missing-required-field: publisher long gone}
@book{oldbook, title = {An Orphaned Book}}
```

The `inert-suppression` rule reports directives that cannot act, such as a
dangling `skip`, an unmatched `on`, an unclosed `off`, a directive written as
typeset prose on a `.dtx` documentation line, or a format-only directive in a
`.bib` file.

`tex-ls lint --fix` applies safe fixes. Add `--unsafe-fixes` to permit changes to
typeset output, such as ties that prevent line breaks or commands that adjust
sentence spacing.

## Machine-readable output

`tex-ls lint --output json` writes a JSON array to stdout. The `pretty` and
`concise` modes write to stderr. A clean run
emits `[]`, so consumers always receive valid JSON; the exit code still signals
whether findings exist.

```json
[
  {
    "rule": "ellipsis",
    "severity": "warning",
    "path": "paper.tex",
    "start": 5,
    "end": 8,
    "message": "literal `...` ellipsis; use `\\dots`",
    "fix": {
      "edits": [{ "content": "\\dots", "start": 5, "end": 8 }],
      "applicability": "safe",
      "description": "Replace `...` with `\\dots`"
    },
    "related": []
  }
]
```

Ranges are 0-indexed byte offsets into the named file (no line/column
resolution). `severity` is one of `error`, `warning`, `info`, or `hint`;
`applicability` is `safe` or `unsafe` (the `--fix`/`--unsafe-fixes` split). The
`fix` key is omitted when a finding has no auto-fix. An edit carries a `path`
key only when it targets a *different* file than the diagnostic (a cross-file
fix); `related` lists secondary "see also" locations.
