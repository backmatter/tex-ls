# More VS Code setup options

For installation and everyday use, start with the
[extension guide](../../editors/vscode/README.md).

## Format when you save

Open a LaTeX document. Run **Format Document With...**, choose
**Configure Default Formatter...**, and select **tex-ls**.

Open Settings and search for `@lang:latex editor.formatOnSave`.
Enable **Editor: Format On Save** to format LaTeX files whenever you save them.
Repeat with a BibTeX file and `@lang:bibtex editor.formatOnSave` for your bibliography.

## Share formatting preferences

If your project has a `tex-ls.toml` file, tex-ls uses its formatting and checking
rules. This lets coauthors use the same preferences. See the
[configuration reference](../reference/configuration.md) for the available options.

## Supported systems

The extension needs VS Code 1.91 or later. Packages are prepared for Linux,
macOS, and Windows, on Intel/AMD and ARM64 computers. VS Code picks the package
for your computer from the versions available in the Marketplace.
The first release, 0.1.0, supports Linux on Intel/AMD only.

For an offline installation, use **Extensions: Install from VSIX** and select
the package for your computer. See [GitHub Releases](https://github.com/backmatter/tex-ls/releases)
for available downloads. Older releases may contain only the command-line program.

## Work on a remote computer

With Remote SSH, WSL, or a development container, install tex-ls in the remote
VS Code window. It runs on the computer that holds your files.
Browser-only and virtual workspaces are not supported.

## Troubleshooting

Save your files so tex-ls can find references across your project. If your
project has several main documents, **tex-ls: Inspect Project** can help explain
which one it has selected. Run the command from a source file to refresh the report.

Leave **tex-ls: Server Path** empty unless you want to use a separately installed
copy of tex-ls. It accepts an absolute executable path or a program name on PATH.

If someone helping you asks for detailed logs, run **Developer: Set Log Level**,
select **tex-ls**, and choose **Trace**. For full message contents, also set
`tex-ls.trace.server` to `verbose`. Logs may include document text and file paths;
review them before sharing.

tex-ls helps edit your source files. Compiler setup and PDF tools are separate;
see [texe](https://github.com/backmatter/texe) for that project.
