# Distribution

The release-binaries workflow builds tagged source with native Cargo on Linux,
macOS, and Windows. Each x86-64 and ARM64 executable must pass version, formatting,
linting, and LSP initialization/shutdown checks before it is archived. Linux uses
musl; Windows links the C runtime statically. macOS builds target macOS 11 or later.
Archives include the MIT license and the unicode-math notices.

Use Conventional Commit PR titles and squash merges. Release-plz opens a release
PR on `main`, computes the server version, and generates the changelog. The
workflow copies that version and those release notes into the bundled VS Code
extension. Do not increment release versions by hand. Use release-plz's default versioning
policy.

The server, VS Code extension, lockfiles, and `vVERSION` tag use one version.
Older extension-only releases used independent versions; the first synchronized
release is 0.1.2. Internal unpublished Rust crates
keep their own implementation versions.

Run `python3 scripts/sync_release_versions.py` to check consistency. The release
workflow runs it with `--write --changelog` on the release PR after release-plz
updates Cargo metadata. Review the final synchronized commit and wait for CI
before squash-merging the release PR.

Merging the release PR creates a tag and draft GitHub release. Release automation starts
the binary workflow. Publish the draft only after all six platform builds pass
and their assets and checksums are attached. For an existing tag and release:

```sh
gh workflow run binaries.yml -f tag=v0.1.0
```

The workflow uploads all six archives, platform-specific VSIX files, `install.sh`, `install.ps1`, and
`SHA256SUMS` after every build passes. It does not overwrite existing assets.
The installer scripts are rendered with the release tag so downloads and checksums
always come from the same release. Both installers support `TEX_LS_INSTALL_DIR`.
Set `TEX_LS_NO_MODIFY_PATH=1` to leave persistent shell configuration unchanged.

Check published installers on all six platforms:

```sh
gh workflow run installers.yml -f tag=v0.1.0
```

Run `python3 scripts/test_installers.py` on Unix for checksum-failure, platform
selection, and shell-path tests. No Rust tests are needed for documentation-only
changes; changes to the language engine still use the checks in CONTRIBUTING.md.

## VS Code

The extension lives in `editors/vscode`. Release automation keeps `package.json`,
its lockfile, and `texLsServerVersion` aligned with the server. Extension-only
changes follow the same release process. After each native executable passes
its smoke tests, the binary workflow packages that
executable in a VSIX for `linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`,
`win32-x64`, or `win32-arm64`. Packaging checks the server version against the
`texLsServerVersion` field and includes the MIT license and unicode-math notices.
Tags that predate the extension skip this packaging step.

VSIX files join the existing release assets and checksum manifest. This workflow
does not publish to the VS Code Marketplace. The Check workflow runs unit tests
and real VS Code integration tests on Linux, macOS, and Windows. See the
[extension development instructions](vscode.md)
for local testing and packaging.

The publisher is `backmatter` and the extension ID is `backmatter.tex-ls`.
Package all six targets at the release-plz version. The manifest icon must
be a PNG; the publisher profile logo is separate.

For a tagged server release, commit the extension changes before creating the
tag. Rerunning an old server tag uses the old tagged source and cannot include
new extension changes. The upload step also refuses to overwrite existing assets.

Install the Linux VSIX into an isolated VS Code profile and run the integration
suite against its bundled server. Native CI checks the other operating systems;
a successful local package check alone does not establish runtime compatibility.

Upload the VSIX files one at a time using **Update** on the existing extension in the
[Marketplace publisher management page](https://marketplace.visualstudio.com/manage/publishers/backmatter).
Use the same version for all platforms and wait for each verification to complete.
Confirm the public listing and install with `code --install-extension backmatter.tex-ls`
in a clean profile. Microsoft documents
[platform-specific extensions](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#platformspecific-extensions).
Marketplace uploads are a separate step from GitHub release assets.

## Homebrew

The formula lives in [backmatter/homebrew-tap](https://github.com/backmatter/homebrew-tap).
After the archives are published, download their checksum file and generate the
formula in a local checkout of that repository:

```sh
gh release download v0.1.0 --pattern SHA256SUMS --dir /tmp/tex-ls-release
python3 scripts/render_homebrew_formula.py v0.1.0 /tmp/tex-ls-release/SHA256SUMS > /path/to/homebrew-tap/Formula/tex-ls.rb
```

Review and commit the formula. The tap tests installation and `brew test` on
Linux and macOS, on x86-64 and ARM64. Users install with
`brew install backmatter/tap/tex-ls`.

## Mason

The package lives in [backmatter/mason-registry](https://github.com/backmatter/mason-registry).
Update the version in `packages/tex-ls/package.yaml` after publishing binaries.
Changes on `main` publish a registry archive through Mason's registry-release
action. Test a registry update with an isolated Neovim/Mason installation before
publishing it. The editor guide documents the registry setup and LSP command.
