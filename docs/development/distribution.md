# Distribution

The release-binaries workflow builds tagged source with native Cargo on Linux,
macOS, and Windows. Each x86-64 and ARM64 executable must pass version, formatting,
linting, and LSP initialization/shutdown checks before it is archived. Linux uses
musl; Windows links the C runtime statically. macOS builds target macOS 11 or later.
Archives include the MIT license and the unicode-math notices.

Publishing a GitHub release starts the workflow. For an existing release:

```sh
gh workflow run binaries.yml -f tag=v0.1.0
```

The workflow uploads all six archives, `install.sh`, `install.ps1`, and
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
