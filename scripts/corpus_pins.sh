# Shared pinned inputs for fetching and checking release corpora.
# name|owner/repo|commit
PINS="
latex3|latex3/latex3|3d1d347d8937863c0786988b14d307a6091ee397
latex2e|latex3/latex2e|3a9fdd88bdc53f16a0c2158aa70d259607de333a
pgf|pgf-tikz/pgf|1c7fc0fdc3ec8a6bdcfd68785c6bbd43ec110178
latexindent|cmhughes/latexindent.pl|748f0f68397793b4646fa48762b0041b889cfcb4
"


verify_corpus() {
  local name="$1" expected="" corpus_name repo sha
  while IFS='|' read -r corpus_name repo sha; do
    if [ "$corpus_name" = "$name" ]; then expected="$sha"; break; fi
  done <<< "$PINS"
  local dir="${CORPORA_DIR}/${name}"
  if [ -z "$expected" ] || [ ! -d "$dir/.git" ] \
      || [ "$(git -C "$dir" rev-parse HEAD)" != "$expected" ] \
      || [ -n "$(git -C "$dir" status --porcelain --untracked-files=all)" ]; then
    echo "error: $name must be a clean checkout at its corpus pin; run scripts/fetch_gate_corpora.sh" >&2
    return 2
  fi
}
