# Formatting

Format LaTeX documents, packages, and classes (`.tex`, `.sty`, `.cls`, `.dtx`,
`.ins`, `.def`, and `.lco`), and BibTeX bibliographies (`.bib` and `.bibtex`).

```sh
tex-ls format paper.tex bibliography.bib  # rewrite files
tex-ls format - < paper.tex              # stdin → stdout
tex-ls format --check paper.tex          # diff; non-zero if unformatted
```

Piped input also works without `-`; interactive use requires paths. `--check`
requires files and prints a diff and summary to stdout. Add `--quiet` for just
filenames and the summary. `--color always|never` overrides terminal detection;
`NO_COLOR` disables automatic color.

## Style options

CLI flags such as `--line-width`, `--indent-width`, `--item-indent`, and `--wrap`
override `[format]` settings for one run. See the
[configuration reference](../reference/configuration.md#format) for defaults.
Run `tex-ls init` for a starter configuration. `--config PATH` selects a file;
`--no-config` ignores discovered configuration.

## Turning the formatter off

```tex
% tex-ls-format skip: hand-aligned table
\begin{tabular}{ll}
  a   &   b \\
  ccc &   d \\
\end{tabular}
```

| Scope | Directive |
| --- | --- |
| Next construct | `% tex-ls-format skip` |
| Region | `% tex-ls-format off` … `% tex-ls-format on` |
| Whole file | `% tex-ls-format skip-file` |

An unmatched `off` runs to the end of the file. Any directive can carry an optional
`: reason`. Bare `% tex-ls skip`, `off`/`on`, and `skip-file` also suppress linting;
`% tex-ls-lint` suppresses only diagnostics (see [linting](linting.md)).

Directives must be `%` comments. A `.dtx` documentation margin is not a comment;
directives work inside its `macrocode` chunks. Exclude files by path with
`exclude`/`extend-exclude` in [configuration](../reference/configuration.md).

## Guarantees

The formatter refuses inputs with parser diagnostics and leaves those files
unchanged. Run `tex-ls lint` to locate the parse errors. Arbitrary catcode
changes and macro-generated delimiters are outside the surface grammar; for
example, changing `%` into an ordinary character can make balanced TeX appear
unclosed to the parser. Extremely deep nesting also produces a diagnostic.

Formatting is idempotent and changes trivia only. Comments and protected bodies
such as `verbatim`, `lstlisting`, and `\verb` are preserved except configured line
endings. Declare custom protected environments or begin/end aliases in
[`[environments]`](../reference/configuration.md#environments).

Content rewrites such as `x^{2}` to `x^2` are lint fixes; use `tex-ls lint --fix`.
