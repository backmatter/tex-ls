#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
output=target/host-parity
mkdir -p "$output"
cargo build --release -p meaning
cargo build --release -p meaning-browser --target wasm32-unknown-unknown
"${WASM_BINDGEN:-wasm-bindgen}" target/wasm32-unknown-unknown/release/meaning_browser.wasm \
  --target nodejs --out-dir "$output/bindings"
for transcript in tests/transcripts/*.json; do
  node scripts/replay_language_hosts.mjs target/release/meaning \
    "$output/bindings/meaning_browser.js" "$transcript" \
    "$output/$(basename "$transcript")"
done
