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
execute TeX or infer signatures from dynamic macro expansion. LaTeX2e and
etoolbox command definers retain their optional star in the command node; the
static definition scanner recognizes their braced and unbraced names.
Primitive replacement groups treat orphan math closers as stored tokens even
when their defined name is an unbraced control word. The flag ends at the
replacement group's closing brace.
Definition operands accept control-symbol names and preserve `\let`'s optional
`=`. Copied, inspected, and compared `\def` tokens cannot mark following groups
as replacement bodies. An incomplete definition scan ends at a closing brace.
Edits to the assignment token fall back to full parsing, including when line
endings or docstrip markers separate it from `\let` and its definee. The parser
and reparse guards share the same trivia predicate.

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
source, declaration, root, acquired format definitions, and compiler state so
unrelated roots can stay cached. Acquiring or changing kernel declarations
invalidates reports that use those declarations, including unknown-command findings.
Push and pull consume the same reports; unchanged pulls avoid computing findings.
Compiler numbers describe the last build and cannot create current definitions or
authorize rename edits. Command navigation and unknown-command checks follow resolved
source dependencies, including acquired installed packages, within the selected
root. Navigation can fall back to the installed `latex.ltx` format source. The native
host requests that source when navigating a
known built-in command; typing and completion do not load the format. Shared
analysis observes the acquired source without filesystem access. Format bootstrap
inputs are not followed as document dependencies. The native index includes `.ltx` and bibliography style sources,
and fingerprints the supported extensions so old caches cannot hide new file types.
Navigation distinguishes delimiter commands from environment arguments and accepts
control symbols. Definition-site scanning also records literal aliases, register, text, font and
math declarations, and literal expl3 `N`- and `c`-name definitions.
When an environment has no navigable declaration, a proved local begin/end pair
provides the definition target for its name. Hover identifies the opposite delimiter
and line. Command and environment hover ranges contain only their actual text, not
the adjacent braces; command definition origins include the backslash so Ctrl-hover
can switch between the command and environment name. Both read only the parsed
environment, and mismatched or incomplete pairs do not offer a target.

A tracked editor-symbol query merges declarations from acquired kernel sources and
the selected root's resolved sources. Include edges and declarations are visited in source order;
aliases copy the signature known at their declaration. Completion, completion resolve,
hover, argument hints, and unknown-command checks use this view. Names with unproved
arity remain known without invented argument slots and block rename collisions.
Traversal through a shared source uses the selected root's include context.
Acquisition collects dependency requests from every candidate root of a shared
source; individual feature requests retain their root context.
Read-only definition lookup in an ambiguous shared source returns destinations
from its candidate roots. Rename still requires a unique, complete namespace.
Formatter and parser signature
inputs remain independent of this editor view. Command copies, expandable document
commands, generated `newif` names, and theorem environments share declaration facts.
Generated names and literal `c` names cannot authorize a control-sequence rename.
Rename also rejects source names used to generate variants or stored as literal
alias targets, since control-sequence edits would leave those uses unchanged.
Editor declarations also cover literal etoolbox names, counter commands, expl3
variables, variants, and conditionals. Unsupported xparse argument specifications
and delimited primitive definitions retain their names without partial argument
hints. Known environment names follow
the same rule, including when a local declaration replaces a built-in environment.
Ordered per-file declaration and include facts omit source offsets, so prose edits
reuse merged editor symbols. Navigation still reads current declaration ranges.
Read-only navigation scans declaration sites without rename's alias safety checks.
Command and environment completion filter static names with the same fuzzy
matcher used by final ranking before allocating candidates or calculating project
and package relevance.
Literal `InputIfFileExists` paths use the ordinary input role. Literal biblatex
`style`, `bibstyle`, and `citestyle` values share file references between acquisition,
navigation, completion, and semantic tokens; `style` can resolve both `.bbx` and `.cbx`.
Only references that execute TeX source contribute command declarations. Graphics,
listings, bibliography databases, and BibTeX style files remain navigable and
acquirable without making their contents part of the command namespace.
Literal package names share comment handling between the package graph, links,
hover, and token ranges. Biblatex style options recognize package names and style
values continued across a comment and indentation. A name continued after a comment
retains separate source spans; completion declines a replacement that would edit
only one fragment.
Installed definitions do not extend the rename permission scope. Workspace
reports omit installed sources and native configured exclusions, clearing reports
previously published for those paths. Document reports also return empty for an
installed source opened through navigation: low-level TeX package and kernel code
produces misleading LaTeX diagnostics. Local package files remain checked. The
installed-source check uses the selected installation index's exact path, so a
project file sharing a basename is not suppressed.

## External facts

Unknown, absent, failed, and present observations are distinct. An incomplete
listing cannot prove absence. Hosts publish backing contents, directory listings,
installed-name indexes, and AUX/LOG/FLS contents before capturing feature snapshots.
Shared artifact parsing is deterministic. Generation tokens reject superseded
acquisition; publication preserves open overlays.

Native IO runs separately from analysis, following literal references with cycle
guards. Dirty dependencies and watcher events trigger acquisition; prose edits do
not reread dependencies. A configured build root is fetched only when it is not
already tracked, so an open root overlay is not reread on its first edit.
Artifact polling compares content fingerprints because
size and mtime can miss rebuilds. TEXMF discovery runs independently with bounded
process deadlines. Each settings value shares immutable installation indexes.
Background checks refresh database/directory fingerprints approximately every five
seconds while requests retain the previous complete index. Explicit-only discovery
ignores automatic roots and TEXINPUTS; relative roots resolve at the scoped native
configuration boundary. TEXINPUTS accepts literal native directory paths, including
Windows short names containing `~`; a trailing `//` enables recursive lookup. Browser applications supply inputs through the
[embedding API](embedding.md).

## Protocol and host lifecycle

`tex-ls-protocol` converts analysis facts into LSP results. Bibliography outlines
for flat-symbol clients use the negotiated form before JSON conversion, avoiding
an intermediate hierarchical JSON tree. Workspace search omits field children
that it never returns. Bibliography lint rules borrow source slices from one
snapshot instead of rendering each value back to text, and suppression scanning
visits only top-level comment entries. Both hosts apply the
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
Explicit compiler candidates remain native-owned after client watcher acknowledgement
because client exclusions can suppress events. Editor compiler-diagnostic policy
filters reports without disabling compiler acquisition. File formatting widths
override editor widths individually only when explicitly present.
Exclude matching resolves canonical and native path spellings against the
configuration root, including deleted paths through an existing ancestor. Paths
outside that root do not inherit its exclusions. Artifact scope updates are coalesced and use the polling cadence; only a root
change triggers an immediate scan.

The native host owns client requests, configuration acquisition, watcher fallback,
and capability-gated refresh. Publish settings before requesting refresh; coalesce
changes received while a refresh is outstanding. Browser dispatch is synchronous:
the application owns scheduling, refresh, cancellation between calls, and atomic
validation/application of result preconditions.
Workspace diagnostic streaming filters non-file paths before requesting scoped
file settings. Untitled buffers continue to receive document diagnostics.

## Highlighting

LaTeX control words and control symbols have the standard LSP `macro` type,
including environment delimiters, definitions, math commands, and unresolved names.
Literal environment names have type `type`. These classifications come from syntax
and do not depend on package acquisition or signature lookup. Reference keys retain
their semantic roles. Literal file arguments share extraction with acquisition and
navigation. Package, class, and named-module arguments are `namespace` tokens;
source, bibliography, and asset paths are `string` tokens. Named-file roles encode
the loader's filename prefix and suffix, including Beamer themes, TikZ/PGF libraries,
and bibliography styles. Theme completions strip the filename prefix. Dynamic
arguments do not become file links or module-name tokens. Ordinary comments and
opaque bodies do not contribute command tokens. Inline `\verb` contributes a
command head and a separate `string` token for its literal body. Flat
`\texttt{...}` arguments also receive `string` tokens; nested TeX commands
keep their own classification. These presentation tokens create no references
or file dependencies.

The VS Code fallback grammar uses matching standard TextMate scopes, including
`entity.name.function.preprocessor` for macros and `entity.name.type` for environment
names. Themes choose colors. Diagnostics report unresolved names independently of
highlighting. Tests cover real single-backslash source, protected text, full/range
responses, UTF-8/UTF-16, line endings, file flavors, and package acquisition.

## Hover documentation

Protocol documentation cards are shared by hover, completion resolve, and signature
help. Curated descriptions and primitive reference links are editor-only metadata;
they never add parser signatures or infer arguments. Source cards use analysis
navigation definitions and cached `.dtx` associations from the same snapshot.
Only unique definitions receive source comments and excerpts. Cards cap definition
links and excerpt length, and render source content as literal code. Local
redefinitions do not inherit built-in descriptions. File-argument cards consume the
same resolved references as navigation, including multiple bibliography style files.
Hosts do not fetch documentation or launch `texdoc` during a hover request.
Package provenance supplies package manual links for completion metadata and
installed definitions. Comment lookup starts at the declaration command, including
when its name appears on a later line.

## VS Code client

`editors/vscode` is a thin TypeScript client using `vscode-languageclient`. One
native server handles the window's workspace folders and untitled documents.
The client forwards editor settings at initialization, on configuration changes,
and through scoped `workspace/configuration` requests. The standard LSP client
handles document synchronization, dynamic file watchers, diagnostics, and providers.
Formatting middleware rejects results after the document changes or closes, or
the request is cancelled. Restart and shutdown operations are serialized.
Fix All Safe Issues requests `source.fixAll.tex-ls` from VS Code's code-action
providers, checks that the document is still current and active, and applies the
server's edit as one workspace edit. The server owns fix selection.
Inspect Project presents `tex-ls.inspectProject` in a read-only virtual JSON
document. Neither command adds language analysis to the client.

Platform-specific VSIX files bundle the corresponding native release executable.
The extension runs in the workspace host for Remote SSH, WSL, and containers.
The extension requires a trusted workspace. Compiler management, builds, and
PDF preview belong to external build integrations.

## The formatter and linter

Formatting lowers syntax with effective signatures and changes trivia only. It must
be idempotent; a consumed space versus one newline cannot decide layout. Preserve
comments and protected bodies except configured line endings. Protocol edits must
reproduce canonical output without splitting Unicode or CRLF boundaries.

Safe lint fixes preserve semantics without later formatting. Fix-all applies complete,
non-conflicting safe fixes from one captured source state; it never runs the
formatter. Unknown-command suppression actions insert comments only after checking
the resulting suppression range. They use `tex-ls` with a rule selector, which
suppresses only that lint rule and leaves formatting enabled. They are non-preferred
quick fixes and stay outside Fix All. Typesetting checks cover whitespace-sensitive keyval and optional-argument
changes that token preservation alone cannot prove safe.
CLI lint fixes currently affect one file per finding; project resolution still
supplies cross-file diagnostics. CLI linting carries the source kind separately
from its reporting path, so stdin retains the `--stdin-filepath` classification.
Unknown-command checks use core metadata and packages loaded by the selected root,
including unconditional package dependencies recorded in CWL metadata. Completion
can offer names from other packages without suppressing their diagnostics.
The typesetting check rejects compiler errors
and compares extracted PDF text and rendered pages.

Formatter lowering lives in private modules for prose, expl3, environments, lists,
alignment, groups, commands, math and trivia. Expl3 fallback statements end at a
recognized call, comment, guard, blank line or stream boundary. A single newline
has the same role as a space. CLI debug reports support Markdown and versioned
JSON; the corpus gate validates the JSON schema, coverage, counts and exit status.

## Verification

[CONTRIBUTING.md](../../CONTRIBUTING.md) lists the checks. Property tests cover parser
losslessness, progress, full/incremental equivalence, and formatting idempotence.
Host transcripts compare native and actual WASM behavior in UTF-8 and UTF-16,
with ambient TEXMF discovery disabled unless the fixture supplies an installation.
Optional corpus, typesetting, and [performance checks](../../benches/README.md) cover
larger workloads. Use native Cargo, rustup, Python, and Node commands.
