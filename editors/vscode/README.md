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

If something is not working, [report the problem](https://github.com/backmatter/tex-ls/issues).
Include your operating system, what you expected, and what happened.
A short example that shows the problem helps.

For formatting on save, remote computers, and other setup options, see the
[additional setup guide](https://github.com/backmatter/tex-ls/blob/main/docs/guide/vscode.md).
