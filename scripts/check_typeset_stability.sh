#!/usr/bin/env bash
# Compile original and formatted inputs, then compare text and rasterized pages.
# See CONTRIBUTING.md for tools and TeX package prerequisites.
#
# Usage: check_typeset_stability.sh [file.tex...]   (default: tests/typeset/*.tex)
# Env:   TEX_LS=/path/to/tex-ls   (default: target/release/tex-ls)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEX_LS="${TEX_LS:-${REPO_ROOT}/target/release/tex-ls}"

INPUTS=("$@")
if [ "${#INPUTS[@]}" -eq 0 ]; then
  shopt -s nullglob
  INPUTS=("${REPO_ROOT}"/tests/typeset/*.tex)
  shopt -u nullglob
fi
if [ "${#INPUTS[@]}" -eq 0 ]; then
  echo "error: no inputs given and tests/typeset/ is empty" >&2
  exit 2
fi
for tool in pdflatex pdftotext pdftoppm; do
  command -v "${tool}" >/dev/null || { echo "error: ${tool} not found" >&2; exit 2; }
done
[ -x "${TEX_LS}" ] || { echo "error: ${TEX_LS} not found — cargo build --release" >&2; exit 2; }

mkdir -p "${REPO_ROOT}/target/typeset-stability"
work="$(mktemp -d "${REPO_ROOT}/target/typeset-stability/run.XXXXXX")"
cleanup() {
  local status=$?
  if [ "${status}" -eq 0 ]; then
    rm -rf "${work}"
  else
    echo "Typesetting check artifacts: ${work}" >&2
  fi
}
trap cleanup EXIT
export SOURCE_DATE_EPOCH=0 FORCE_SOURCE_DATE=1

failed=0
for input in "${INPUTS[@]}"; do
  name="$(basename "${input}" .tex)"
  dir="${work}/${name}"
  mkdir -p "${dir}"
  cp "${input}" "${dir}/before.tex"
  cp "${input}" "${dir}/after.tex"
  if ! "${TEX_LS}" --no-config format "${dir}/after.tex"; then
    failed=1
    continue
  fi
  if cmp -s "${dir}/before.tex" "${dir}/after.tex"; then
    echo "check: ${name} (already formatted)"
  fi
  for side in before after; do
    # Two passes settle references. Use .stdout because hyperref owns .out.
    for pass in 1 2; do
      if ! (cd "${dir}" && pdflatex -interaction=nonstopmode -halt-on-error \
          "${side}.tex" >"${side}.${pass}.stdout" 2>&1); then
        echo "error: ${name} (${side}, pass ${pass}) failed to compile; see ${dir}/${side}.${pass}.stdout" >&2
        failed=1
        continue 3
      fi
    done
    if [ ! -f "${dir}/${side}.pdf" ]; then
      echo "error: ${name} (${side}) produced no PDF" >&2
      failed=1
      continue 2
    fi
    if ! pdftotext -layout "${dir}/${side}.pdf" "${dir}/${side}.txt"; then
      echo "error: ${name} (${side}) text extraction failed" >&2
      failed=1
      continue 2
    fi
  done
  # Identical rendering catches keyval changes to colors, strokes and geometry.
  # PDF metadata and object numbering do not affect these deterministic rasters.
  for side in before after; do
    if ! pdftoppm -r 144 "${dir}/${side}.pdf" "${dir}/${side}" >"${dir}/${side}.raster.stdout" 2>&1; then
      echo "error: ${name} (${side}) rasterization failed" >&2
      failed=1
      continue 2
    fi
  done
  shopt -s nullglob
  before_pages=("${dir}"/before-*.ppm)
  after_pages=("${dir}"/after-*.ppm)
  shopt -u nullglob
  if [ "${#before_pages[@]}" -eq 0 ] || [ "${#before_pages[@]}" -ne "${#after_pages[@]}" ]; then
    echo "TYPESET CHANGE: ${name} page count differs or no pages rendered" >&2
    failed=1
  else
    for page in "${before_pages[@]}"; do
      if ! cmp -s "$page" "${dir}/after-${page##*/before-}"; then
        echo "TYPESET CHANGE: ${name} rendered page ${page##*/before-} differs" >&2
        failed=1
      fi
    done
  fi
  if diff -q "${dir}/before.txt" "${dir}/after.txt" >/dev/null; then
    echo "ok: ${name} extracted PDF text is unchanged after formatting"
  else
    echo "TYPESET CHANGE: ${name}"
    diff "${dir}/before.txt" "${dir}/after.txt" | sed 's/^/  /' || true
    failed=1
  fi
done

exit "${failed}"
