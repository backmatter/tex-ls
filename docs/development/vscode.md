# Developing the VS Code extension

The extension lives in `editors/vscode`. For installation and everyday use, see
the [extension guide](../../editors/vscode/README.md).

From the repository root, build the server with `cargo build --locked --bin tex-ls`.
Then, in `editors/vscode`:

```sh
npm ci
npm test
TEX_LS_TEST_SERVER="$PWD/../../target/debug/tex-ls" npm run test:integration
```

On Windows, set `TEX_LS_TEST_SERVER` to the absolute path of `tex-ls.exe` before
running the test command. The integration test launches an isolated VS Code with
temporary user settings and projects. On headless Linux, use
`xvfb-run -a npm run test:integration`. Linux tests use X11 so focus-sensitive
commands also work on an isolated display. `VSCODE_EXECUTABLE_PATH` selects an existing
VS Code executable; otherwise the test downloads VS Code. `VSCODE_TEST_VERSION`
selects a version, defaulting to stable.

Build a release binary, then package a VSIX using native Node and vsce:

```sh
node scripts/package.mjs --binary ../../target/release/tex-ls --target linux-x64
```

The script verifies the binary against `texLsServerVersion` in `package.json` and includes the
server's license notices. Output goes to the repository's `dist/` directory.
For an Extension Development Host, open this directory in VS Code and launch it
with `--extensionDevelopmentPath` pointing here. Configure `tex-ls.server.path`
in that window or package once to populate `server/`.

The extension version can advance independently of the bundled server version.
See the [distribution guide](https://github.com/backmatter/tex-ls/blob/main/docs/development/distribution.md#vs-code)
for packaging and publishing. The icon uses the Backmatter family mark, rendered with the existing logo renderer:

```sh
node ../logos/src/render.js backmatter --out dist/marketplace --no-png
rsvg-convert -w 256 -h 256 ../logos/dist/marketplace/backmatter/favicon.svg -o editors/vscode/images/icon.png
```

Run these commands from the repository root with the sibling `logos` checkout.
