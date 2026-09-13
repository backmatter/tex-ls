#!/usr/bin/env bash
#
# Fetch the trivia-invariant-layout gate corpora into `corpora/` at the
# repository root, pinned to exact commits so every gate run
# (`tex-ls debug format --checks all|trivia --report .`) is reproducible.
# The directory is gitignored; re-running is a fast no-op when a corpus is
# already checked out at its pin.
#
# The pins are the SHAs the recorded sets in tests/gate_baselines are measured
# against — never bump one without re-recording, or the two-sided ratchet in
# check_gate_baselines.sh compares against a different corpus.
#
# `latexindent` is the odd one out: not package source but latexindent.pl's own
# test suite, ~5.3k small hand-written files of deliberately adversarial LaTeX
# (blank lines in display math, verbatim-argument commands, unmatched braces,
# alignment torture). Its *outputs* are not a target — latexindent is an
# indenter driven by its own YAML config model, so every committed `*-mod1.tex`
# is that tool's answer to a different question. We mine it purely as oracle
# input: the median file is ~200 bytes, so a failure is a near-minimal repro.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPORA_DIR="${REPO_ROOT}/corpora"

source "${REPO_ROOT}/scripts/corpus_pins.sh"

mkdir -p "${CORPORA_DIR}"

while IFS='|' read -r name repo sha; do
  [ -z "${name}" ] && continue
  dir="${CORPORA_DIR}/${name}"
  if [ -e "${dir}" ]; then
    if [ ! -d "${dir}/.git" ]; then
      echo "error: ${dir} exists but is not a corpus checkout" >&2
      exit 2
    fi
    if [ -n "$(git -C "${dir}" status --porcelain --untracked-files=all)" ]; then
      echo "error: ${dir} has local changes; restore or move it before fetching" >&2
      exit 2
    fi
  fi
  if [ -d "${dir}/.git" ] && [ "$(git -C "${dir}" rev-parse HEAD)" = "${sha}" ]; then
    echo "${name}: already at ${sha}"
    continue
  fi
  echo "${name}: fetching ${repo} @ ${sha}"
  if [ ! -d "${dir}/.git" ]; then
    mkdir -p "${dir}"
    git -C "${dir}" init --quiet
    git -C "${dir}" remote add origin "https://github.com/${repo}.git"
  fi
  # A shallow single-commit fetch: reproducible, no history download.
  git -C "${dir}" fetch --quiet --depth 1 "https://github.com/${repo}.git" "${sha}"
  git -C "${dir}" checkout --quiet --no-overwrite-ignore FETCH_HEAD
  echo "${name}: checked out $(git -C "${dir}" rev-parse HEAD)"
done <<EOF
${PINS}
EOF

echo "Gate corpora ready under ${CORPORA_DIR}"
