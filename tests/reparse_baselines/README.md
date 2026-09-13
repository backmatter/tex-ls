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

| Driver | Edits |
| --- | --- |
| `word-typing` | Five keystrokes inside a word at three sites per file |
| `word-deleting` | Five single-character deletions at three sites |
| `protected-typing` | Five keystrokes inside a protected body |
| `math-word-typing` | Five keystrokes preserving an unscripted math word's partition |
| `math-shape-typing` | Five keystrokes after a scripted math-word base |
| `hazard-single` | Sixteen random edits from the hazard alphabet |
| `hazard-chain` | Five chains of two to four hazard edits |

The sweep uses `.tex`, `.sty`, `.cls`, `.dtx`, and `.ins` files with their normal CLI
parse modes. The math tier declines `.dtx` because its fragment lacks docstrip
line/column context. Token and protected tiers require matching docstrip state.
The drivers do not target the region tier's multi-token paragraph edits.
