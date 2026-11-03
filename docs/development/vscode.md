# Developing the VS Code extension

Use [Vite+](https://viteplus.dev) (`vp`) for extension development and VSIX packaging.
It selects Node 26 from `.node-version` and pnpm from `packageManager`.
The extension lives in `editors/vscode`. For installation and everyday use, see
the [extension guide](../../editors/vscode/README.md).

From the repository root, build the server with `cargo build --locked --bin tex-ls`.
Then, in `editors/vscode`:

```sh
vp install
vp run test
TEX_LS_TEST_SERVER="$PWD/../../target/debug/tex-ls" vp run test:integration
```

On Windows, set `TEX_LS_TEST_SERVER` to the absolute path of `tex-ls.exe` before
running the test command. The integration test launches an isolated VS Code with
temporary user settings and projects. On headless Linux, use
`xvfb-run -a vp run test:integration`. Linux tests use X11 so focus-sensitive
commands also work on an isolated display. `VSCODE_EXECUTABLE_PATH` selects an existing
VS Code Electron executable, such as `/usr/share/code/code`, rather than the
`bin/code` or `/usr/bin/code` launcher. Otherwise the test downloads VS Code. `VSCODE_TEST_VERSION`
selects a version, defaulting to stable.

`vp run build` bundles the client and its runtime dependencies into `dist/extension.js`
with `vp pack`; only `vscode` stays external. The bundle build also writes
`dist/THIRD-PARTY-NOTICES.txt` with the bundled packages' licenses.

Build a release binary, then package a VSIX:

```sh
node scripts/package.mjs --binary ../../target/release/tex-ls --target linux-x64
```

The script verifies the binary against `texLsServerVersion` in `package.json`, includes the
server's license notices, rebuilds the bundle, and runs `vsce package --no-dependencies`, so
no `node_modules` ship. `.vscodeignore` lists the files the VSIX contains. Output goes to the repository's `dist/` directory.
For an Extension Development Host, open this directory in VS Code and launch it
with `--extensionDevelopmentPath` pointing here. Configure `tex-ls.server.path`
in that window or package once to populate `server/`. Run `vp run build` first.

The extension and bundled server share one release version. Release-plz prepares
the version change, and the release workflow synchronizes the extension metadata.
See the [distribution guide](https://github.com/backmatter/tex-ls/blob/main/docs/development/distribution.md#vs-code)
for packaging and publishing.
