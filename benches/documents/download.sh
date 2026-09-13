#!/usr/bin/env bash
#
# Fetch the optional larger inputs for in-process benchmarks. The small baseline
# is committed. Downloaded documents are ignored by Git.

set -euo pipefail

DOCS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DOCS_DIR"

# Pinned tex-fmt release tag (https://github.com/wgunderwood/tex-fmt).
TEXFMT_REF="v0.5.7"
RAW="https://raw.githubusercontent.com/wgunderwood/tex-fmt/${TEXFMT_REF}"

echo "Downloading benchmark documents (tex-fmt @ ${TEXFMT_REF})..."
echo

fetch() {
    local out="$1" path="$2"
    echo "$out"
    curl -fsSL --create-dirs -o "$out" "${RAW}/${path}"
}

# small  → committed baseline (small.tex), no download
fetch cv.tex                   tests/cv/source/cv.tex
fetch masters_dissertation.tex tests/masters_dissertation/source/masters_dissertation.tex
fetch phd_dissertation.tex     tests/phd_dissertation/source/phd_dissertation.tex

echo "Run: cargo bench --bench formatting"
