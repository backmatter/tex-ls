# Performance checks

Run timing checks without concurrent builds. Record the commit, toolchain, hardware,
workload, warm/cold state, and raw output with performance claims. Preserve correctness
assertions. Process RSS, requested live allocations, and WASM memory capacity measure
different things; report them separately.

| Command | Measures |
| --- | --- |
| `cargo bench --bench formatting` | Parse and format cost |
| `cargo bench --bench keystroke` | Edits through incremental analysis |
| `cargo bench --bench reparse` | Incremental versus full parsing |
| `cargo bench --bench lsp_memory` | Live allocations across session histories |

Set `TEX_LS_BENCH_ASSERT=1` for reparse/keystroke regression assertions and
`TEX_LS_MEMORY_ASSERT=1` for retained-history limits. Fetch optional document inputs
with `bash benches/documents/download.sh`.

## Native LSP workloads

```sh
cargo build --release --locked -p tex-ls
python3 scripts/benchmark_lsp_rpc.py
```

The default workloads use 1, 100, and 1,000 files. For real projects, fetch the
sources pinned by URL and SHA-256 in [lsp-projects.json](lsp-projects.json):

```sh
python3 scripts/fetch_lsp_benchmarks.py
python3 scripts/benchmark_lsp_rpc.py \
  --project target/lsp-release-inputs/paper/expl3-intro.tex \
  --project target/lsp-release-inputs/thesis/thesis.tex \
  --project target/lsp-release-inputs/bibliography/rendering-bibtex.bib
```

Each run starts a fresh server with TEXMF disabled. Real-project runs wait for
acquisition and document symbols, then measure completion and diagnostics through
five edits. Assertions check completion, report invalidation/reuse, and zero warm
source reads/compiler probes. Fallback watcher reads are outside those acquisition
counters. Each workload also observes three idle seconds (`--idle-seconds`): Linux
CPU and read counters cover the whole process, including fallback watching and
background jobs. Other hosts report those counters as unavailable.
OS caches are not flushed. Downloaded sources include provenance, but omit assets
needed only for typesetting.

For before/after comparisons, pass the executable as the first positional argument
and alternate binaries under the same machine conditions. Keep raw output with the comparison.

## Focused profiles

Run `cargo run --release --locked --example NAME`:

| Example | Measures |
| --- | --- |
| `completion_benchmark` | Citation completion, ranking and payload size |
| `diagnostics_benchmark` | Unchanged pulls and independent-root invalidation |
| `workspace_symbols_benchmark` | Empty/exact-name searches at 1,000/10,000 symbols |
| `bibliography_profile` | Parse, model, lint, response and allocation stages |
| `bib_rules_profile` | Semantic collection and individual BibTeX rules |
| `edit_batch_profile`, `reparse_profile`, `lint_query_profile` | Edit batches, reparse tiers and query reuse |
| `browser_profile` | Native parse/query stages and actual session completion for a JSON document fixture |
| `rebuild_profile` | Query cost before and after removing half the project files |
| `session_profile` | Live and disposed allocations after repeated session open/close histories |

`session_profile` needs `--features tex-ls-browser/allocation-metrics`.
`scripts/profile_wasm_session.cjs` profiles an allocation-metrics WASM build.

`bibliography_profile` checks full/incremental equivalence and compares flat symbols
with a reference adapter. Allocation counting adds overhead; stage timings are not
RPC latency. `bib_rules_profile -- OUTPUT_PREFIX` writes complete model and finding
records for before/after comparison. Isolated rule times include their own traversal
and suppression setup, so do not sum them; the example checks combined findings.
