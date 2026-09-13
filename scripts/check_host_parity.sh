#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
output=target/host-parity
mkdir -p "$output"
cargo build --locked --release -p tex-ls
cargo build --locked --release -p tex-ls-browser --target wasm32-unknown-unknown
"${WASM_BINDGEN:-wasm-bindgen}" target/wasm32-unknown-unknown/release/tex_ls_browser.wasm \
  --target nodejs --out-dir "$output/bindings"
node scripts/test_browser_bridge.mjs "$output/bindings/tex_ls_browser.js"
for transcript in tests/transcripts/*.json; do
  node scripts/replay_language_hosts.mjs target/release/tex-ls \
    "$output/bindings/tex_ls_browser.js" "$transcript" \
    "$output/$(basename "$transcript")"
done
