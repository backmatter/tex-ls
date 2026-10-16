# Incremental reparse baselines

These files record splice counts over the corpora pinned by
`scripts/fetch_gate_corpora.sh`.

```sh
bash scripts/check_reparse_baselines.sh
RECORD=1 bash scripts/check_reparse_baselines.sh
```

Review count changes before recording them. The test in
`crates/tex-ls-parser/tests/reparse_corpus_sweep.rs` requires every successful
reparse to match a full parse in tree and errors and to preserve the input.
Baseline updates cannot waive these assertions. Splice-rate floors catch fast paths
that stop accepting edits; exact counts also detect changes between tiers.

## Rows

```text
<corpus>  corpus    files=<n>  bytes=<n>
<corpus>  <driver>  spliced=<s>/<a>  token=<n>  verbatim=<n>  math=<n>  region=<n>  files=<n>
```

Header rows identify corpus size. Driver `files` counts files with candidate edit
sites; files without a site are skipped, not counted as refused edits. Seeds derive
from corpus-relative paths, so checkout location does not affect the counts.

Recognizing standalone `IncludeInRelease` descriptions adds protected edit sites.
Their edits require a full-file reader proof and conservatively use full parsing.
The LaTeX3 row includes 50 such attempts from four additional files, retaining all
415 existing successful splices. When a ratio changes, compare per-file candidates
to distinguish added fallback coverage from existing sites losing a fast path.

| Driver | Edits |
| --- | --- |
| `word-typing` | Five keystrokes inside a word at three sites per file |
| `word-deleting` | Five single-character deletions at three sites |
| `protected-typing` | Five keystrokes inside a protected body |
| `math-word-typing` | Five keystrokes preserving an unscripted math word's partition |
| `math-shape-typing` | Five keystrokes after a scripted math-word base |
| `hazard-single` | Sixteen random edits from the hazard alphabet |
| `hazard-chain` | Five chains of two to four hazard edits |

The sweep uses `.tex`, `.sty`, `.cls`, `.dtx`, `.ins`, `.def`, and `.lco` files with their normal CLI
parse modes. The math tier declines `.dtx` because its fragment lacks docstrip
line/column context. Token and protected tiers require matching docstrip state.
The drivers do not target the region tier's multi-token paragraph edits.

## Recording changes

Corpus repository revisions are pinned in `scripts/corpus_pins.sh`. The checker
requires clean checkouts at those commits. Fetch them before checking or recording:

```sh
bash scripts/fetch_gate_corpora.sh
bash scripts/check_reparse_baselines.sh
```

To review one corpus, pass its name, for example
`bash scripts/check_reparse_baselines.sh latex3`. The checker compares exact counts
in both directions. A lower splice rate reports `REGRESSION`, a higher rate reports
`STALE`, and changed counts with the same rate report `CHANGED`. New or missing rows
also fail. Full-parse fallbacks can be required when ownership or lexical state
cannot be proved, but their effect on ordinary editing still needs review.

Inspect the failing rows and their parser guards before recording a change. Retain
the corpus pins, deterministic sampling and driver floors unless the workload
itself needs a separate, reviewed change. Record accepted counts with
`RECORD=1 bash scripts/check_reparse_baselines.sh`, optionally followed by corpus
names. Recording succeeds only after the sweep's equivalence assertions and
splice-rate floors pass.
