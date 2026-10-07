# tex-ls for VS Code

tex-ls helps you write LaTeX documents and manage BibTeX references in VS Code.
It suggests commands as you type, points out mistakes, and helps you find your
way around a document.

## Get started

1. Open **Extensions** in VS Code.
2. Search for **tex-ls** by **Backmatter** and click **Install**.
3. Open your document's folder, then a `.tex` or `.bib` file.

If VS Code asks whether you trust the folder, allow it if it is your own project
or comes from someone you trust. tex-ls starts automatically.

You can also [open the extension in the Marketplace](https://marketplace.visualstudio.com/items?itemName=backmatter.tex-ls).
Use the desktop version of VS Code. You do not need to install a compiler to use
these editing tools. Creating and viewing PDFs still needs a separate tool.

## While you write

- Get suggestions for LaTeX commands, labels, and citations as you type.
- Hover over an underlined passage to see a problem. Click the light bulb for
  available fixes.
- Right-click a reference and choose **Go to Definition** to jump to its label.
- Use **Rename Symbol** to rename a label and update its references together.
- Use the **Outline** panel to move between sections.

To tidy up indentation and spacing, right-click your document, choose
**Format Document With...**, then **tex-ls**. You can undo the changes as usual.

## Syntax colors

tex-ls distinguishes commands, names and references, resource paths, math
structure, comments, literal content, and ordinary text. Your theme chooses their
colors. A reference color describes its purpose; Go to Definition works when the
target can be resolved.

Numbers in prose, math, and BibTeX dates use ordinary text color. Numeric settings,
such as dimensions or counter values, have a separate role. `\texttt{file.tex}` is
printed text. `\input{file.tex}` is a resource path. Literal commands and protected
bodies keep their contents separate from active TeX syntax. Unfinished inline
verbatim remains literal through the end of its line.

Headings and `\textbf` mark prose as bold; emphasis marks it as italic. Nested
commands and references retain their own colors. The server distinguishes these
contexts more precisely than the fallback grammar. Enable semantic highlighting
if your theme disables it:

```json
"editor.semanticHighlighting.enabled": true
```

Themes can customize `texText`, `texLiteral`, `texPath`, and `texMath` with
`editor.semanticTokenColorCustomizations`. Ordinary and literal text use the
theme's neutral embedded-content color when it has no specific rule for those
purposes. This lets semantic tokens replace a fallback grammar's string or
comment color. Themes can still choose a separate color for literal content.
The extension supplies font-only defaults for `texBold`, `texItalic`, and
`declaration`. Ordinary text clears inherited bold and italic from the fallback
grammar; source typography modifiers then combine without changing its color.
Use language-qualified semantic selectors, such as `texText:latex` or
`*.texBold:latex`, to override these font defaults. The default excluded `comment`
environment uses comment styling.
`alltt` preserves literal spacing while its commands and braces remain active.
`\detokenize` keeps command-looking text literal, with normal comments and macro
parameter substitution inside definitions.
`\string` makes only its next token literal; following group contents remain active.

Python listings and minted bodies distinguish keywords, function names, comments,
and ordinary code. Explicit C, C++, JavaScript, TypeScript, JSON, CSS, and HTML
headers can use VS Code's embedded language grammar. Unfinished foreign strings
and comments stop at the LaTeX closing delimiter. Foreign text before a same-line
closer can use neutral styling. This integration supplies colors; foreign
completion and diagnostics require a separate language tool.

Literal listing escape settings expose real TeX references inside code. Minted
escapes stay inactive in Python strings and ordinary comments. Listings escapes
also execute inside strings and comments. Minted shortcuts, Fancyvrb short verbs,
and custom excluded comments protect command-looking text from navigation and
rename. Literal Fancyvrb command/group characters can activate real TeX commands.
The formatter preserves the listing's authored layout, including its TeX islands.

Macro-built language names, dynamic settings, and unsupported foreign lexical
profiles retain a protected fallback. tex-ls does not execute TeX to guess them.
Settings and listing declarations inside conditionals also keep affected code
protected. Dynamic option keys cannot leave earlier TeX escapes active by accident.
Literal percent, dollar, and at-sign catcodes and known lexical switches respect
groups; unknown conditional assignments do not invent active references.
Copied or inspected command names remain navigable. Their following groups keep
the surrounding text or math mode and receive no reference, path, option, or
typography role from that name. Real commands inside those groups remain active.

## Useful commands

Open the **Command Palette** from the **View** menu and type `tex-ls`.

| Command | When to use it |
| --- | --- |
| Fix All Safe Issues | Apply the fixes tex-ls considers safe to the current file. You can undo them together. |
| Restart Language Server | Restart tex-ls if suggestions or error messages stop updating. |
| Show Language Server Output | Open the logs when you need help with a problem. |
| Inspect Project | See which files tex-ls thinks belong to your document. This technical report can help diagnose missing references. |

## Settings and help

The defaults are enough to get started. For preferences, open **Settings** and
search for `tex-ls`.

If you see duplicate suggestions or error messages, another LaTeX extension may
be providing the same features. Disable its overlapping features, or disable
that extension for this workspace.

For a project-local or pinned toolchain, set `tex-ls.texmf.roots` in workspace
settings and enable `tex-ls.texmf.explicitOnly`. Relative roots use the workspace
folder; automatic refresh picks up installed or removed packages. These TEXMF
settings can be overridden per workspace folder, including in remote workspaces.
If your build extension already reports compiler errors, set
`tex-ls.diagnostics.compiler` to `false`; source linting and label numbers remain
available. A `[build]`-only `tex-ls.toml` does not override editor formatting widths.

If something is not working, [report the problem](https://github.com/backmatter/tex-ls/issues).
Include your operating system, what you expected, and what happened.
A short example that shows the problem helps.

For formatting on save, remote computers, and other setup options, see the
[additional setup guide](https://github.com/backmatter/tex-ls/blob/main/docs/guide/vscode.md).
