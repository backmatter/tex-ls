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

The memory workload waits for acquisition to settle after each document open and
before measuring retained allocations. One 30-second deadline covers status
requests and polling; all pending counts must stay zero, installation loading
must stop, and the complete acquisition status including read/probe counters
must remain unchanged for 250 milliseconds. The 10,000-edit query histories keep
their feature barriers without a quiet wait per edit. Only diagnostic cancellations
with `retriggerRequest: true` retry; other protocol errors, malformed acquisition
status, and settle timeouts fail the gate. The client contract tests run with
`cargo test --test lsp_memory_client`.

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

Each run starts a fresh server with TEXMF disabled by default. Pass `--texmf` to
include the local TeX installation and record that choice in the output.
Real-project runs wait for acquisition and document symbols, then warm editor
features so intentional first-use source loads are outside the edit IO counter.
The edit IO baseline requires a quiet acquisition window because completed
source reads can queue a final compiler-artifact batch.
They measure completion, hover, definition, and diagnostics
through five edits. Synthetic runs
assert that a documented command resolves to its declaration, including across
files. Synthetic output retains individual warm samples and Linux process memory.
Real-project definition counts depend on the available dependencies.
Assertions check completion, report invalidation/reuse, and zero warm
source reads/compiler probes. Fallback watcher reads are outside those acquisition
counters. Each workload also observes three idle seconds (`--idle-seconds`): Linux
CPU and read counters cover the whole process, including fallback watching and
background jobs. Other hosts report those counters as unavailable.
OS caches are not flushed. Downloaded sources include provenance, but omit assets
needed only for typesetting.

Each RPC request shares a 30-second deadline across notifications and retries.
Acquisition status polling uses the same deadline as its settle wait. A changed
revision may retry; diagnostic cancellation retries only when the server supplies
`retriggerRequest: true`. Other errors fail the workload. Run
`python3 scripts/test_benchmark_lsp_rpc.py` to check this client contract.

For before/after comparisons, pass the executable as the first positional argument
and alternate binaries under the same machine conditions. Keep raw output with the comparison.

## Highlighting workloads

```sh
python3 scripts/benchmark_highlighting.py target/release/tex-ls
```

This measures full semantic-token requests, ten-line range requests, document
outlines, diagnostic pulls, and full requests after incremental prose edits.
Workloads cover prose, many definitions, literal `csname` uses, math, long Unicode
math lines, Python listings, inert token lists, graphics keyvals, and BibTeX,
including a single Unicode line with many entries. Each workload starts a fresh
server with TEXMF disabled, then repeats warm requests after acquisition settles.
It checks token ordering, range/full equivalence, and stable responses. JSON lines retain every timed sample,
source and token hashes, and Linux process RSS/high-water marks. Use `--sizes`,
`--workloads`, and `--repeats` to select a workload; compare matching source and
purpose hashes when alternating before/after executables. Purpose hashes coalesce
adjacent identical roles, so harmless token segmentation changes remain comparable.
Outlines use hierarchical support by default; `--flat-symbols` measures flat
clients. Symbol and diagnostic hashes normalize the temporary document URI for comparisons.

## Bulk navigation workloads

```sh
python3 scripts/benchmark_navigation.py target/release/tex-ls
```

This measures definitions, references with and without declarations, rename, and cursor
highlights for labels, citations, commands, environments, glossary keys, and
BibTeX strings. Separate workloads measure document links and bibliography
folding. Fixtures put many occurrences on one Unicode line and declarations after
uses. The ASCII label workload checks conversion overhead; `many_definitions`
checks command uses in a file with many unrelated definitions. `redefinitions`
checks many declarations of one command, including definition-link containers. Each workload
starts a fresh server with TEXMF disabled and waits for acquisition to settle.
JSON lines retain warm samples, response counts and hashes, source hashes, and
Linux process RSS/high-water marks. Temporary URIs are normalized for before/after
comparisons. Use `--sizes`, `--workloads`, and `--repeats` to limit the run.
The optional `many_files` workload measures a command search across many files
whose declarations do not match. For example, use `--workloads many_files --sizes
250 1000` to check namespace lookup without large response payloads.

## Bulk editor workloads

`python3 scripts/benchmark_bulk.py target/release/tex-ls` measures multi-cursor
LaTeX and BibTeX selections, safe Fix All edits, BibTeX range formatting, document
colors, color presentations, bulk quick fixes, table-column edits, and file-rename previews. It runs both UTF-8 and
UTF-16 on long Unicode lines, with unsorted and duplicate selection cursors.
Each result includes source and response hashes, repeated request timings, and
process memory. Fix All uses an explicit fixture configuration for the safe
script-brace rule. These requests do not change files on disk.
The optional `--workloads unknown_actions` measures suppression alternatives over
a large selection. Their result count is bounded, and changes to that bound
intentionally change the response hash.

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
`browser_profile` accepts a JSON array of `{path, source}` documents. Set
`TEX_LS_PROFILE_ROUNDS` to limit edits and `TEX_LS_PROFILE_FULL_EDIT=1` to shift all
source ranges with whole-document replacements instead of command-prefix edits.
`scripts/profile_wasm_session.cjs` profiles an allocation-metrics WASM build.
The native session-history benchmark includes definition, references, rename,
cursor-highlight, and semantic-token requests alongside
diagnostics and hover. It disables TEXMF discovery so installation observations
cannot change the retained-history comparison.

`bibliography_profile` checks full/incremental equivalence and compares direct flat
symbols and the response adapter with a reference adapter. It records their
conversion and JSON allocation peaks separately. Allocation counting adds overhead; stage timings are not
RPC latency. `bib_rules_profile -- OUTPUT_PREFIX` writes complete model and finding
records for before/after comparison. Isolated rule times include their own traversal
and suppression setup, so do not sum them; the example checks combined findings.
