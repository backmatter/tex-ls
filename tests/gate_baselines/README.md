# Formatter corpus baselines

These files record known failures over the corpora pinned in
`scripts/fetch_gate_corpora.sh`. Keep the failure sets exact: an added row is a
regression; a removed row means the baseline needs updating after reviewing the fix.

```sh
bash scripts/fetch_gate_corpora.sh
cargo build --release --locked
bash scripts/check_gate_baselines.sh
```

The check accepts corpus names to restrict a run, for example
`bash scripts/check_gate_baselines.sh latex3`. Reports use width 80 and default
formatting settings, with project configuration disabled.

- `<corpus>.all.txt`: sorted `path<TAB>kind` rows from `debug format --checks all`.
- `<corpus>.trivia.txt`: sorted `path<TAB>kind<TAB>class` rows from
  `debug format --checks trivia`. Classes distinguish formatting refusal,
  content changes, non-convergence, parse errors and losslessness failures.

The script prints added and removed rows. Review the underlying reports and fixes
before editing a baseline; do not accept new failures just to make the check pass.
Generate a full report from a corpus directory with
`../../target/release/tex-ls --no-config debug format --checks all --report .`
or replace `all` with `trivia`.

Strict trivia invariance has no accepted-failure baseline. Survey it directly:

```sh
cd corpora/latex3
../../target/release/tex-ls --no-config debug format --checks trivia-strict --report .
```

Use `--line-width` to investigate width-dependent failures.
