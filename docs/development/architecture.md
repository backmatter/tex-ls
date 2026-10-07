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

Copied or inspected command heads retain their control-sequence identity without
executing the command's argument roles. Following groups inherit their enclosing
mode; real commands inside those groups remain active. One ordered operand index
supports key extraction, file roles, signature domains, and highlighting. The
cached analysis queries reuse definition sites when constructing this index.
Outline titles, documentation symbols, color pickers, argument completion, and
call-specific lint fixes use the same ownership rule. The lint driver skips only
the owned head node and token, preserving checks on nested active source.
Literal `ifx` comparisons count character tokens and TeX spaces, including comment
line endings. Local reader definitions disable these rules only after their name.
Generated listing declarations must also prove that their factory remains active
at that source offset before they can override a reader. Copied factory overrides
do not disable later declarations.

Incremental reparsing must equal a full parse in both tree and errors. Each fast
path proves its attachment and lexical boundaries; unproved edits, stale bases,
and incompatible declarations fall back to full parsing. Edit chains preserve
sequential coordinates. Performance thresholds never weaken these requirements.
Resolved parse contexts cache lexical restrictions instead of rescanning the
whole file for every edit. Tier proofs reject changes to state-bearing control
words, selectors, and settings. Python's foreign tokens cannot enter the ordinary
leaf tier, so a Python listing does not force unrelated prose edits to parse fully.
Files with scoped control-word alphabets admit alphabetic WORD
splices, proven opaque-capture splices, and alphabetic math-fragment edits.
Those paths retain their existing text-read, ancestry, old-fragment, boundary,
and diagnostic proofs. They decline region splices and newly state-sensitive
punctuation. Listing settings, catcodes, string operands, shortverb declarations,
alltt, detokenize, and bare input retain conservative full-parse fallbacks.
Edits to `ifx` operands or fragments containing comparisons also fall back when
their outgoing operand state cannot be proved. An edit before a local reader
definition falls back so its ordered offset cannot become stale. Fragment parses
rebase reader offsets to their own start; unrelated files retain the existing tiers.

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
sources. Proven local definitions or aliases disable later `includeonly` calls;
earlier filters remain in effect. Loaded package signatures respect load order
and explicit overrides.
File-rename previews apply the same ownership and local-override checks before
changing selector spellings, including inert token lists and copied operands.

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

Root views index claimed files, candidate roots, and representative IDs instead
of searching every scope for every source. Include graphs index unresolved edges
by source while retaining edge order, so root interpretation visits each source's
own unresolved edges. A reverse subfile-parent index avoids searching a root's
include list for every ordinary child and preserves reverse visit order.
Shared children still keep all candidate roots and an
independent default view when no unique root can be selected.
Include and package cycle detection uses an explicit DFS stack, so dependency
depth cannot exhaust the native or WASM call stack. Active-path positions locate
back edges without searching the path.

Acquisition batches stage source layers before publishing each changed project's
ordered member list once. Membership removals still rebuild storage once. New
location cells are added with one catalog update per batch; subsequent observation
and alias writes reuse those cells. Overlays remain authoritative, duplicate paths
retain their last value, and stale acquisitions are rejected before staging.
Alias projections retain the catalog's path order. Resolution and command lookup
use binary search instead of scanning every alias for each dependency.

A tracked root-graphics query shares inherited directory facts across every child
file context. A prose edit recomputes those facts once; equal directories preserve
the child contexts and file-reference queries. Directory edits invalidate them.

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
The parser uses proved local declaration sites, including literal `c` names, to
disable replaced primitive readers and listing factories during its second parse.
Their editor signatures stay outside the parser's signature input.
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
File-role contexts index preceding graphics-path declarations once per traversal.
Links, acquisition, and path completion use the same ordered lookup, including
empty resets and local overrides, instead of scanning the document for each image.
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

The protocol classifies source purposes, not RGB colors. Control sequences use
`macro`, environment names use `type`, identifiers use `variable`, option and
BibTeX field names use `property`, module names use `namespace`, and operational
numeric settings use `number`. Macro argument placeholders use `parameter`.
Foreign language keywords and declared functions use `keyword` and `function`;
TikZ operation words and path punctuation use `keyword` and `operator`.
Custom types separate `texText`, `texLiteral`,
`texPath`, and `texMath`; ignored source uses `comment`. Prose, math operands and
numbers, printed filenames, and BibTeX text and dates stay neutral. Bare BibTeX
abbreviations retain their identifier role. A color does not promise a resolved
definition or link.

Source declarations receive the standard `declaration` modifier. Prose typography
uses `texBold` and `texItalic`, which combine inside nested headings and emphasis.
Commands and identifiers retain their own roles there. An interval sweep resolves
nested classifications by priority, so a heading cannot hide a reference and a
literal body cannot expose commands. Comments inside detokenize token lists keep
normal TeX comment tokenization. Macro parameters inside a definition retain their
parameter role there because substitution happens before detokenize reads the
argument. Tokens never overlap or cross CR, LF, or CRLF;
full and range responses share UTF-8/UTF-16 conversion.
Bulk token, outline, diagnostic, navigation, link, fold, and hint conversion uses
a position cursor to reuse counted Unicode prefixes. Navigation groups ranges
by requested path, converts each file in source order, and restores the original
response order, including duplicate locations and alias URIs. Cursor highlights
also retain semantic ordering when declarations follow uses. Range endpoints use a cursor fork so a parent's
full extent does not advance conversion past its children. The cursor allocates
no character tables and leaves incremental line-table maintenance unchanged.
Unordered and interior-byte queries retain the ordinary position conversion semantics.
Command occurrence queries index declaration ranges once per file when excluding
declarations. Cursor highlights build a range set once rather than scanning every
definition for each command use. Glossary rename checks each affected file's macro
roots once. These indexes belong to the request and do not retain source revisions.
Definition links retain the source byte range through wire conversion. They resolve
each target URI and source index once, and batch declaration-container lookup with
a sweep that preserves fact precedence and inclusive endpoints. Single-target
lookup retains the early first-match scan.

Multi-cursor selections convert input positions once per requested Unicode line,
including positions inside a code point. Their nested output ranges order both
endpoints together so shared parent spans reuse the same counted prefixes.
Selection and file-rename lookup descend through indexed child ranges rather than
searching every sibling. The lookup preserves rowan's adjacent-token boundary
results, including empty nodes and malformed source. Out-of-range columns saturate
before clamping on 32-bit hosts.
Fix All deduplicates edit sets with borrowed keys and converts edits once per
target file while retaining their original order and atomic resolution.
Bulk quick fixes share those conversions and batch diagnostic ranges and
competing-edit checks. Single-correction requests keep the first-match policy.
Suppression suggestions validate at most eight construct alternatives and one
file-wide alternative against the parser. They remain explicit choices and do
not enter Fix All. Cancellation is checked between validation attempts.
BibTeX range formatting, table-column edits and document colors also reuse a
position cursor. Color presentations compare byte spans before converting the
matching color range.
File rename reuses each source's maintained line table and file-role facts,
indexes requested destinations and existing edits, and retains ambiguity and
collision checks. Native rename previews run on the read pool; source writes and
explicit cancellation invalidate their snapshots before a response is published.
Malformed range-formatting parameter shapes return InvalidParams without
terminating the native session.

Only proven math domains color scripts and math boundaries as structure.
Alignment separators require an alignment environment and cannot inherit that
role through an unrelated argument group. Local command definitions invalidate
built-in argument presentation assumptions. Numeric styling applies to curated
settings and literal keyval dimensions/counts, not digit-shaped prose or unknown
text-valued keys. Nested keyval groups preserve their own comma boundaries. File and
module roles share exact literal argument extraction with acquisition and
navigation. Local source overrides invalidate those built-in roles after the
declaration across coloring, links, completion, and dependency collection.
Import directories, literal color names, table column specifications,
and TikZ settings use their own source purposes. Nested keyval values cannot hide
a following literal key. Dynamic arguments do not become file links or literal module names.

The lexer recovers unfinished delimited verbatim at the line boundary or EOF.
Protected incremental splices decline unclosed captures, including edits that remove
a closer. Inline captures also prove that appended source cannot extend the capture. Alltt has active commands
and braces while its other special characters are literal. Detokenize preserves
balanced groups and comments without executing raw captures or catcode toggles
inside its token list. The grammar marks proven token-list bodies in the CST;
definition, navigation, completion, hover, outline, dependency, and lint walks
share that ownership. Environment editing and refactoring exclude printed pairs.
Local source definitions and aliases can decline the primitive
interpretation. Semantic command aliases cannot change parse shape or reclassify
these core commands. The formatter preserves authored whitespace in alltt
bodies and detokenize arguments.
Primitive `\string` owns exactly the following token. That token is inert: a
printed command cannot create references, load files, capture a literal body, or
change lexical state. Subsequent group contents remain active. Stored macro
parameters retain their parameter role because substitution precedes expansion.
Incremental parsing falls back to the full parser for these lexical modes until
fragment equivalence is proved.

Listing ownership comes from source, independently of editor grammars. A scoped
settings resolver reads literal listings styles/defaults, minted defaults and
shortcuts, Fancyvrb settings and custom environments, and comment inclusion or
exclusion declarations. Comment terminators match an exact source line. Minted
environment shortcuts include their starred options form; inline shortcuts use
bounded raw delimiters. Their generated names are known to editor declarations,
but generated spellings do not become control-sequence rename targets.

The shared Python lexical profile distinguishes code, strings, multiline strings,
comments, keywords, and declared function names. Minted escapeinside is active
only in code; its mathescape applies to comments. Listings escapeinside and
mathescape also apply inside strings and comments. Proven TeX islands have ordinary
syntax and semantic facts, with a hard recovery boundary. Malformed islands,
dynamic settings, unsupported lexical profiles, and nested foreign captures stay
protected. Included comments and locally redefined comment environments use the
ordinary lexer stream. Explicit project verbatim declarations take precedence
over source-discovered non-verbatim declarations. Nested captures in
Fancyvrb command bodies remain opaque, so they cannot recurse ahead of the
grammar's nesting budget. Fancyvrb supports literal distinct ASCII command/group characters and
a literal comment character. All listing layout, including active islands, is
preserved by the formatter.

Listing families keep separate defaults. Active TeX islands inherit the parent
parse context and lexical configuration. Local overrides of source settings or
listing declaration commands cannot invent escapes or generated environments.
Changes to scoped listing settings fall back to full parsing so later body
ownership cannot retain a stale lexical mode.

Known lexical switches, short verbs, and listing settings restore after brace or
explicit groups. Literal backtick or numeric percent, dollar, and at-sign catcode
assignments support their ordinary/comment/math/letter roles. Unknown conditional
assignments cannot prove a comment or math boundary; affected source stays neutral
without invented references. This is a bounded source interpretation, not TeX
expansion. Incremental reparsing declines these stateful modes and Python bodies.
Opaque delegated bodies can still use the protected splice's full fragment proof.
Conditional listing settings and declarations mark the affected family, style,
or generated capture uncertain. Dynamic option keys and unproved setter arguments
cannot retain earlier escape settings as proof of active TeX. Scope restoration
restores these uncertainty facts with the other listing settings. Conditional
comment inclusion remains literal rather than claiming active or excluded source.
FancyVerb comment characters stop at CR, LF, and CRLF independently.

For explicit single-line C, C++, JavaScript, TypeScript, JSON, CSS, or HTML headers,
the internal `texEmbedded` purpose replaces all blanket semantic coverage of the
foreign body. Clients opting into `experimental.texEmbeddedHighlighting` omit
those spans and use an embedded grammar; other clients receive `texLiteral`.
This is an optional client capability, independent of client identity. A client
must provide its own foreign grammar before opting in. Ambiguous, repeated,
nested, or multiline selectors remain protected rather than promising a grammar
selection that cannot be proved from the header.
The VS Code extension contributes these grammars and embedded language mappings.
Its outer listing boundary stops unfinished foreign grammar states at the LaTeX
closer. Foreign text before a same-line closer can use neutral protected styling.
Shared parse shape never depends on that capability. Python colors use the shared
profile in both hosts. Foreign language completion and diagnostics remain separate
from this color integration.

Definition sites retain their separate raw-candidate and ownership-filtered
proof. A lazy operand query derives command-head ownership from those cached
sites. Signatures, semantic models, modes, outlines, label descriptions, and
feature requests reuse that query. Lint contexts borrow the same sites and
operands while keeping signatures and modes lazy. File-reference collection
shares one role context across search bases. The parser's source-context scan
also shares one definition and operand index with signature extraction.

Label descriptions use a lazy per-file query shared by hover and completion. It
reuses definition sites and builds section, caption, and operand indexes once per
source revision. Each label keeps its innermost classifying ancestor or preceding
section. The first declaration of a duplicate name wins within each file; lookup
still checks the origin before other defining files. Completion candidates reuse
these descriptions instead of scanning the source for each candidate.
For empty label queries, completion borrows context kinds before ranking and
renders descriptions only for the selected results. Nonempty queries still match
all candidate descriptions before the result limit.

Highlighting reuses memoized source definition sites and modes, then indexes local
command names and definition coverage once per request. Mode changes invalidate
the shared mode query; unchanged requests reuse it in highlighting and completion.
Range-token requests collect ownership and typography from the whole document,
then restrict priority resolution and encoding to the requested lines. They retain
whole boundary lines so intersecting tokens match full-response boundaries.
Literal `csname` words are collected during the token walk, avoiding repeated
whole-document scans. Adjacent spans with identical purposes and modifiers are
coalesced before encoding. Cursor-only ownership checks inspect CST ancestors;
command-name completion avoids building unused file-role indexes. The response
policy negotiates types and modifiers
independently and remaps both indices and bit positions in the existing token
array, without retaining a second full payload. Unsupported paths can fall back to `string`, and math
structure to `operator`; unsupported neutral/literal purposes are omitted rather
than assigned a misleading standard type. The VS Code extension registers custom
types, typography modifiers, and language-specific TextMate scope mappings.
Themes choose colors. The extension supplies a neutral `meta.embedded` scope
fallback and font-only defaults for prose typography and declarations. These
editor presentation choices do not enter the language server. User semantic
styling can override them. Its fallback grammar protects literal URLs, inline code,
excluded comment bodies, alltt, token lists, and character constants. Parameter
and operator patterns do not run globally over prose. The server supplies context
that a regex grammar cannot reliably infer, including source overrides and
short-verbatim declarations.

Unsupported foreign-language bodies remain literal. Arbitrary TeX expansion,
package code that changes catcodes, and dynamic verbatim escape rules are not
executed by this source classifier.

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
