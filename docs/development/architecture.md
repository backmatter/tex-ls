# Architecture

| Crate | Responsibility |
| --- | --- |
| `tex-ls-parser` | Lossless syntax trees, recovery, incremental reparsing, syntax-derived facts and signature data |
| `tex-ls-formatter` | Layout over syntax and supplied signatures |
| `tex-ls-analysis` | Source/project inputs, Salsa queries, resolution, diagnostics and lint fixes |
| `tex-ls-protocol` | LSP positions, URIs, feature results and edit presentation |
| `tex-ls-browser` | In-memory sessions and wasm-bindgen exports |
| `tex-ls` | Native CLI, filesystem access, processes and LSP scheduling |

Shared crates remain WASM-compatible. Hosts acquire external inputs; feature queries
perform no filesystem access, process execution, or host callbacks.
`scripts/check_architecture.py` checks dependency direction.

## The parser

The lexer and recursive-descent parser emit events that build a lossless rowan CST.
Every input byte survives, including malformed input; recovery must always advance.
Parse shape depends only on source, file flavor, and explicit project declarations.
Installed packages and completion metadata cannot change it. Fix syntax errors in
the parser. Recursive grammar entry is bounded to 128 levels. Exhaustion emits a
syntax diagnostic and retains the remaining bytes as flat error content. Math
fragment splices also account for their surrounding depth; recovery bases use a
full parse. This bounds later tree walks on native and WASM hosts.

LaTeX and BibTeX have separate grammars and trees. Shared signature and syntax facts
live in the parser crate; editor presentation belongs above it. tex-ls does not
execute TeX or infer signatures from dynamic macro expansion.

Incremental reparsing must equal a full parse in both tree and errors. Each fast
path proves its attachment and lexical boundaries; unproved edits, stale bases,
and incompatible declarations fall back to full parsing. Edit chains preserve
sequential coordinates. Performance thresholds never weaken these requirements.

## Analysis inputs and ownership

`IncrementalDatabase` owns writes; `Analysis` exposes read snapshots. Open overlays
and disk/backing contents are separate layers: closing an overlay restores backing
contents. Source/project membership, declarations, settings, and external facts are
explicit inputs. Removing memberships reconstructs Salsa storage so deleted inputs
do not accumulate; surviving syntax may be reused, but queries can become cold.
WASM linear-memory capacity can remain above live allocations.

Range-free name, signature, and dependency projections allow reuse after prose edits.
Ranges come from the current source snapshot. Root views keep separate compilation
roots separate even when they share files. Ambiguous resolution cannot authorize an
edit. File links, completion, and acquisition use the same literal reference roles
and path contexts; `includeonly` records build participation separately from known
sources. Loaded package signatures respect load order and explicit overrides.

Diagnostics and code actions share findings. Report identities include relevant
source, declaration, root, and compiler state so unrelated roots can stay cached.
Push and pull consume the same reports; unchanged pulls avoid computing findings.
Compiler numbers describe the last build and cannot create current definitions or
authorize rename edits.

## External facts

Unknown, absent, failed, and present observations are distinct. An incomplete
listing cannot prove absence. Hosts publish backing contents, directory listings,
installed-name indexes, and AUX/LOG/FLS contents before capturing feature snapshots.
Shared artifact parsing is deterministic. Generation tokens reject superseded
acquisition; publication preserves open overlays.

Native IO runs separately from analysis, following literal references with cycle
guards. Dirty dependencies and watcher events trigger acquisition; prose edits do
not reread dependencies. Artifact polling compares content fingerprints because
size and mtime can miss rebuilds. TEXMF discovery runs independently with bounded
process deadlines. TEXINPUTS accepts literal native directory paths, including
Windows short names containing `~`; a trailing `//` enables recursive lookup. Browser applications supply inputs through the
[embedding API](embedding.md).

## Protocol and host lifecycle

`tex-ls-protocol` converts analysis facts into LSP results. Both hosts apply the
session's immutable `ResponsePolicy` at the response boundary to negotiate position
encoding, Markdown, snippets, symbol forms, and supported fields. Completion resolve
preserves eager edits, filtering, and sorting and rejects obsolete source tokens.

The native event loop owns transport and editor versions; one worker orders writes.
Queued reads retain parameters, not snapshots. Read admission captures a snapshot
and edit context only when capacity is available, allowing writes and cancellation
to continue. Internal request identities prevent late responses or partial chunks
from completing a newer request that reused a transport ID.

Cancellation is checked at query boundaries. Explicit cancellation completes once;
a superseding write cancels diagnostic pulls with a retrigger request. Panics become
errors, and completed jobs release snapshots before returning capacity. Edit results
reject changed context and include document versions where supported. Plain text
edits still require client-side freshness checks.

The worker keeps scheduling and ownership in `src/lsp/worker.rs`, with private
modules for acquisition and feature dispatch. Fallback watching honors source
ignore rules and checks explicit compiler candidates, including missing paths
and acquired AUX chains; it does not recursively traverse ignored build trees.
Exclude matching resolves canonical and native path spellings against the
configuration root, including deleted paths through an existing ancestor. Paths
outside that root do not inherit its exclusions. Artifact scope updates are coalesced and use the polling cadence; only a root
change triggers an immediate scan.

The native host owns client requests, configuration acquisition, watcher fallback,
and capability-gated refresh. Publish settings before requesting refresh; coalesce
changes received while a refresh is outstanding. Browser dispatch is synchronous:
the application owns scheduling, refresh, cancellation between calls, and atomic
validation/application of result preconditions.

## The formatter and linter

Formatting lowers syntax with effective signatures and changes trivia only. It must
be idempotent; a consumed space versus one newline cannot decide layout. Preserve
comments and protected bodies except configured line endings. Protocol edits must
reproduce canonical output without splitting Unicode or CRLF boundaries.

Safe lint fixes preserve semantics without later formatting. Fix-all applies complete,
non-conflicting safe fixes from one captured source state; it never runs the
formatter. Typesetting checks cover whitespace-sensitive keyval and optional-argument
changes that token preservation alone cannot prove safe.
CLI lint fixes currently affect one file per finding; project resolution still
supplies cross-file diagnostics. The typesetting check rejects compiler errors
and compares extracted PDF text and rendered pages.

Formatter lowering lives in private modules for prose, expl3, environments, lists,
alignment, groups, commands, math and trivia. Expl3 fallback statements end at a
recognized call, comment, guard, blank line or stream boundary. A single newline
has the same role as a space. CLI debug reports support Markdown and versioned
JSON; the corpus gate validates the JSON schema, coverage, counts and exit status.

## Verification

[CONTRIBUTING.md](../../CONTRIBUTING.md) lists the checks. Property tests cover parser
losslessness, progress, full/incremental equivalence, and formatting idempotence.
Host transcripts compare native and actual WASM behavior in UTF-8 and UTF-16.
Optional corpus, typesetting, and [performance checks](../../benches/README.md) cover
larger workloads. Use native Cargo, rustup, Python, and Node commands.
