#!/bin/sh
# Install a prebuilt tex-ls release without Rust or administrator access.
set -eu

install_tex_ls() {
    tag='@TAG@'
    case "$(uname -s)" in
        Linux) os=unknown-linux-musl ;;
        Darwin) os=apple-darwin ;;
        *) echo 'Use install.ps1 on Windows.' >&2; return 1 ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64) arch=x86_64 ;;
        aarch64|arm64) arch=aarch64 ;;
        *) echo 'Unsupported CPU architecture.' >&2; return 1 ;;
    esac
    archive="tex-ls-$arch-$os.tar.gz"
    base="https://github.com/backmatter/tex-ls/releases/download/$tag"
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT HUP INT TERM
    curl -fLsS "$base/$archive" -o "$work/$archive"
    curl -fLsS "$base/SHA256SUMS" -o "$work/SHA256SUMS"
    expected=$(awk -v name="$archive" '$2 == name { print $1 }' "$work/SHA256SUMS")
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$work/$archive" | awk '{print $1}')
    else
        actual=$(shasum -a 256 "$work/$archive" | awk '{print $1}')
    fi
    if [ -z "$expected" ] || [ "$actual" != "$expected" ]; then
        echo 'Download checksum verification failed.' >&2
        return 1
    fi
    tar -xzf "$work/$archive" -C "$work"
    "$work/tex-ls" --version
    bin=${TEX_LS_INSTALL_DIR:-"$HOME/.local/bin"}
    data=${XDG_DATA_HOME:-"$HOME/.local/share"}/tex-ls
    mkdir -p "$bin" "$data"
    install -m 755 "$work/tex-ls" "$bin/tex-ls"
    cp "$work/LICENSE" "$work/unicode-math.LICENSE" "$work/unicode-math.NOTICE" "$data/"
    printf 'Installed tex-ls in %s\n' "$bin"
    case ":$PATH:" in
        *":$bin:"*) ;;
        *)
            quoted=$(printf '%s' "$bin" | sed "s/'/'\\\\''/g")
            line="export PATH='$quoted':\"\$PATH\""
            case "${SHELL:-}" in
                */zsh) profile="$HOME/.zshrc" ;;
                */bash) profile="$HOME/.bashrc" ;;
                */fish)
                    profile="$HOME/.config/fish/config.fish"
                    line="fish_add_path '$quoted'"
                    mkdir -p "$HOME/.config/fish"
                    ;;
                *) profile="$HOME/.profile" ;;
            esac
            if [ "${TEX_LS_NO_MODIFY_PATH:-0}" != 1 ]; then
                if ! grep -Fqx "$line" "$profile" 2>/dev/null; then
                    printf '\n# tex-ls\n%s\n' "$line" >> "$profile"
                fi
                printf 'Restart your terminal, then run: tex-ls --version\n'
            fi
            ;;
    esac
}
install_tex_ls
