#!/usr/bin/env bash
# Machine check for tests/gate_baselines: re-run both gates over the pinned
# corpora (fetched by scripts/fetch_gate_corpora.sh) and compare the distilled
# failure sets against the recorded baselines.
#
# Added and removed failures both fail the check. Review fixes before removing
# stale baseline entries.
#
# Usage: check_gate_baselines.sh [corpus...]   (default: latex3 latex2e pgf latexindent)
# Env:   TEX_LS=/path/to/tex-ls   (default: target/release/tex-ls)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEX_LS="${TEX_LS:-${REPO_ROOT}/target/release/tex-ls}"
CORPORA_DIR="${REPO_ROOT}/corpora"
source "${REPO_ROOT}/scripts/corpus_pins.sh"
BASELINE_DIR="${REPO_ROOT}/tests/gate_baselines"

CORPORA=("$@")
if [ "${#CORPORA[@]}" -eq 0 ]; then
  CORPORA=(latex3 latex2e pgf latexindent)
fi

if [ ! -x "${TEX_LS}" ]; then
  echo "error: ${TEX_LS} not found or not executable — build it first:" >&2
  echo "  cargo build --release" >&2
  exit 2
fi

failed=0

check_one() {
  local corpus="$1" gate="$2" # gate: all | trivia
  local baseline="${BASELINE_DIR}/${corpus}.${gate}.txt"
  if [ ! -f "${baseline}" ]; then
    echo "error: missing baseline ${baseline}" >&2
    failed=1
    return
  fi
  local current status=0
  # Exit 1 records findings; other nonzero statuses mean the check failed to run.
  current="$(cd "${CORPORA_DIR}/${corpus}" \
    && "${TEX_LS}" --no-config debug format --checks "${gate}" --report-json .)" || status=$?
  if [ "${status}" -gt 1 ]; then
    echo "error: ${corpus}.${gate} checker exited ${status}" >&2
    failed=1
    return
  fi
  local got added removed
  if ! got="$(printf '%s\n' "${current}" | python3 "${REPO_ROOT}/scripts/check_gate_report.py" "${gate}" "${status}")"; then
    echo "error: ${corpus}.${gate} report is incomplete or inconsistent" >&2
    failed=1
    return
  fi
  added="$(LC_ALL=C comm -13 <(LC_ALL=C sort -u "${baseline}") <(printf '%s\n' "${got}") || true)"
  removed="$(LC_ALL=C comm -23 <(LC_ALL=C sort -u "${baseline}") <(printf '%s\n' "${got}") || true)"
  if [ -n "${added}" ]; then
    echo "REGRESSION: ${corpus}.${gate} grew (new failures not in the baseline):"
    printf '%s\n' "${added}" | sed 's/^/  + /'
    failed=1
  fi
  if [ -n "${removed}" ]; then
    echo "STALE BASELINE: ${corpus}.${gate} shrank (recorded failures now pass):"
    printf '%s\n' "${removed}" | sed 's/^/  - /'
    echo "  Re-record ${baseline} (remove these lines) so the sets match reality."
    failed=1
  fi
  if [ -z "${added}" ] && [ -z "${removed}" ]; then
    echo "ok: ${corpus}.${gate} matches the baseline ($(printf '%s\n' "${got}" | grep -c . || true) entries)"
  fi
}

for corpus in "${CORPORA[@]}"; do
  verify_corpus "${corpus}"
  if [ ! -d "${CORPORA_DIR}/${corpus}" ]; then
    echo "error: ${CORPORA_DIR}/${corpus} not found — fetch the corpora first:" >&2
    echo "  bash scripts/fetch_gate_corpora.sh" >&2
    exit 2
  fi
  check_one "${corpus}" all
  check_one "${corpus}" trivia
done

exit "${failed}"
