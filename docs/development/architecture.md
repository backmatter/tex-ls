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

The native binary only initializes logging and parses CLI arguments. Private
`src/commands` modules own dispatch, configuration precedence, input selection,
formatting, linting, CST output, and debug reports. The lint command separates
bibliography acquisition, project analysis over acquired text, and file fixes.
The library's `cli` module defines arguments without owning command execution.

## The parser

The lexer and recursive-descent parser emit events that build a lossless rowan CST.
Every input byte survives, including malformed input; recovery must always advance.
Parse shape depends only on source, file flavor, and explicit project declarations.
Installed packages and completion metadata cannot change it. Fix syntax errors in
the parser. Recursive grammar entry is bounded to 128 levels. Exhaustion emits a
syntax diagnostic and retains the remaining bytes as flat error content. Math
fragment splices also account for their surrounding depth; recovery bases use a
full parse. This bounds later tree walks on native and WASM hosts.

`parser/lexer/context.rs` owns immutable parse context and its edit rebasing.
`parser/lexer/operands.rs` owns pending operand state. Closed reader and declaration
facts live in `parser/reader_facts.rs`, shared by lexing, grammar, and semantic
ownership. The lexer owns token scanning and exposes context through named queries.

LaTeX and BibTeX have separate grammars and trees. Shared signature and syntax facts
live in the parser crate; editor presentation belongs above it. tex-ls does not
execute TeX or infer signatures from dynamic macro expansion. Definition operands
and stored replacement bodies have explicit ownership. Grammar recovery respects
that ownership and retains the bytes it actually consumed.

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
sequential coordinates. The direct single-edit API requires a caller-proved text
transform and coherent parse parts. The chained entry point validates supplied
transforms, including single-edit batches; analysis uses that validating boundary.
The analysis cache requires the previous parse's flavor and declarations to match
before reusing its lexical context, including when a declaration update follows a
staged edit. Performance thresholds never weaken these requirements.

Successful analysis splices share immutable context and declaration snapshots
with the previous artifact, avoiding copies of their full-file maps per edit.
A full parse constructs a fresh context. Declaration snapshots can still be
shared when their exact values match; retained older artifacts remain immutable.
Resolved parse contexts cache lexical restrictions instead of rescanning the
whole file for every edit. Fast paths prove lexical state, operand ownership,
attachment, fragment boundaries and diagnostics against the full source context.
State-bearing edits, uncertain reader transitions and unsupported embedded modes
fall back to a full parse. Python's foreign tokens cannot enter the ordinary leaf
tier, so a listing does not force unrelated prose edits to parse fully.
Source signals in inactive text still participate in these proofs because they can
change later lexical modes. Protected captures must retain their terminators and
cannot extend into appended source. DTX fragments must retain documentary context.

Context positions remain exact after edits. Length-changing splices return a
rebased context that subsequent edits and analysis caching must carry forward.
Fragment lexers use absolute source offsets and borrow immutable declaration maps.
Splices share off-spine green subtrees; rebuilding ancestor child arrays still
costs work proportional to those arrays. Indexed boundary lookup preserves adjacent
candidates and zero-length nodes, including malformed input.

## Analysis inputs and ownership

`IncrementalDatabase` owns writes; `Analysis` exposes read snapshots. Open overlays
and disk/backing contents are separate layers: closing an overlay restores backing
contents. Source/project membership, declarations, settings, and external facts are
explicit inputs. Removing memberships reconstructs Salsa storage so deleted inputs
do not accumulate; surviving syntax may be reused, but queries can become cold.
WASM linear-memory capacity can remain above live allocations.

Batch source renames capture backing layers, overlays, project ownership and
per-source metadata before removing paths, so swaps preserve their inputs.
Same-project moves preserve inherited settings and explicit overrides. An inherited
source clears an overwritten destination's override. Cross-project transfers retain
the captured effective declarations as explicit settings. Explicit project
associations follow their source; otherwise the destination root selects ownership.
Membership removal rebuilds storage before destination memberships are published.
Browser presentation, formatting and lint overrides follow the same move plan.

Protocol synchronization applies each change batch atomically. Disjoint partial
ranges in descending order can share the original position table and produce one
source buffer. Overlapping, touching, clamped, mixed whole-document, or uncertain
CRLF-seam changes retain sequential application. Parser hints preserve submitted
order and sequential coordinates.

Range-free name, signature, and dependency projections allow reuse after prose edits.
Ranges come from the current source snapshot. Root views keep separate compilation
roots separate even when they share files. Ambiguous resolution cannot authorize an
edit. File links, completion, and acquisition use the same literal reference roles
and path contexts; `includeonly` records build participation separately from known
sources. Proven local definitions or aliases disable later `includeonly` calls;
earlier filters remain in effect. Loaded package signatures respect load order
and explicit overrides. Package traversal preserves dependency post-order across
cycles and shared dependencies without recursive call-stack growth. The native
formatter's package loader preserves the same ordering and `.dtx` fallback origins.
File-rename previews apply the same ownership and local-override checks before
changing selector spellings, including inert token lists and copied operands.

Diagnostics and code actions share findings. Report identities include relevant
source, declaration, root, acquired format definitions, and compiler state so
unrelated roots can stay cached. Package option checks use the same active file
roles as the dependency graph, including stored token lists and local load-command
overrides. Report identities include loaded package sources rather than every
package file in the project. Acquiring or changing kernel declarations
invalidates reports that use those declarations, including unknown-command findings.
Push and pull consume the same reports; unchanged pulls avoid computing findings.
Compiler numbers describe the last build and cannot create current definitions or
authorize rename edits. Command navigation and unknown-command checks follow resolved
source dependencies, including acquired installed packages, within the selected
root. Navigation can fall back to the installed `latex.ltx` format source. The native
host requests that source when navigating a known built-in command; typing and completion do not load the format. Shared
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

Root views index source membership and candidate roots. Shared children retain all
candidate roots and an independent default view when no unique root is available.
Include and package graph walks are cycle-safe and use explicit stacks, so dependency
depth cannot exhaust the native or WASM call stack.

Acquisition batches stage source layers before publishing each changed project's
ordered member list. Overlays remain authoritative, duplicate paths retain their
last value, and stale acquisitions are rejected before staging. Alias projections
preserve catalog order. Cached graphics-directory facts are shared across child
contexts and invalidate when their effective directories change.

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
Formatter and parser signature inputs remain independent of this editor view. Command copies, expandable document
commands, generated `newif` names, and theorem environments share declaration facts.
The parser uses proved local declaration sites, including literal `c` names, to
disable replaced source readers and declaration factories during its second parse.
Actual command targets retire later declaration interpretations after their
binding. The binding scan rejects inert or already replaced owners. Editor signatures stay
outside the parser's signature input.
Generated names and literal `c` names cannot authorize a control-sequence rename.
Signature propagation through helper calls uses cycle-safe graph traversal.
Rename also rejects source names used to generate variants or stored as literal
alias targets, since control-sequence edits would leave those uses unchanged.
Editor declarations also cover literal etoolbox names, counter commands, expl3
variables, variants, and conditionals. Unsupported xparse argument specifications
and delimited primitive definitions retain their names without partial argument
hints. Known environment names follow the same rule, including when a local declaration replaces a built-in environment.
Ordered per-file declaration and include facts omit source offsets, so prose edits
reuse merged editor symbols. Navigation still reads current declaration ranges.
Read-only navigation scans declaration sites without rename's alias safety checks.
`navigation/names.rs` owns command and environment definitions, references, namespace
selection, and rename permission. The incremental host exposes cached facts and
borrowed scope iterators without exposing Salsa inputs to feature code.
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
Inclusion, package loads, bibliography resources, file references and inherited
graphics paths share cached role facts and ordered lookup per source revision.
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
Shared artifact parsing is deterministic. Native hosts supply exact resolved AUX
child paths when physical aliases change the input directory. Generic and browser
artifacts without those paths retain relative input resolution. Native watch
ownership uses acquired paths independently of opaque artifact identities.
Within an acquisition, physical payloads and AUX children are read once. Parsed
artifacts are cached by physical path, effective directory and logical extension,
preserving relative paths for each logical context. Generation tokens reject
superseded acquisition; publication preserves open overlays.
Native compiler batches publish the complete artifact scope of surviving source
contexts. After a backing refresh proves a source unavailable, its context and
watch candidates retire; shared artifacts remain owned by other live contexts.
Scope shrink reconstructs analysis storage once to release retired artifact cells
and cached payloads. Older snapshots retain their own observations. Generic and
browser artifact batches retain merge semantics.

Native IO runs separately from analysis, following literal references with cycle
guards. Acquisition keys cache project-reference projections by analysis revision
and storage epoch, independently of cursor context and installation generation.
Equal projections share immutable data across unrelated writes. Project removal
retires the cache; source renames clear path-dependent keys. Pending scans and
forced path invalidation still trigger acquisition.
Dirty dependencies and watcher events trigger acquisition; prose edits do
not reread dependencies. A configured build root is fetched only when it is not
already tracked, so an open root overlay is not reread on its first edit.
Native viewer arguments, recursive TEXINPUTS roots and Kpathsea path output
preserve native path bytes. Unix IPC uses an explicit file-URI transport address
when a socket path cannot be represented in UTF-8; ordinary addresses retain
their existing representation.
Native forward-search outcomes and inverse-search payloads encode non-UTF-8
absolute paths as explicit file-URI objects. Response computation and serialization
share the cancellation and panic boundary; serialization failures complete the
request with an error.
Physical identity reuse retains live logical source owners and the current read
batch, retiring confirmed absent owners even when a close predates publication.
Workspace removal releases that project's native identity cache, discovery state,
failure reports, and installation ownership. Confirmed source retirement also
releases its acquisition and parent-discovery keys; pending backing restoration
preserves those keys.
Native bibliography lookup and acquisition-failure caches retain the targets of
current references, aliases, compiler contexts, and queued requests. Observation
scope checks run after 256 acquired location rows, rather than on every small
batch. Shared location history is reconstructed only when it exceeds twice the
live scope plus 256 cells. Current missing and failed targets remain cached;
abandoned targets may be acquired again if a later edit references them.
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

`tex-ls-protocol` converts analysis facts into LSP results. Both hosts apply the
session's immutable `ResponsePolicy` at the response boundary to negotiate position
encoding, Markdown, snippets, symbol forms, and supported fields. Completion resolve
preserves eager edits, filtering, and sorting and rejects obsolete source tokens.

The native event loop owns transport and editor versions; one worker orders writes.
Queued reads retain parameters, not snapshots. Read admission captures a snapshot
and edit context only when capacity is available, allowing writes and cancellation
to continue. Internal request identities prevent late responses or partial chunks
from completing a newer request that reused a transport ID.

Shutdown cancels outstanding reads and retires pending client requests. Until
`exit`, the native loop rejects new feature requests with their original wire IDs.
It stops watcher scopes and discards late worker results, progress updates and
client replies. Inverse search receives an explicit rejection during that interval,
so late external traffic cannot restart acquisition or send editor requests.
Already queued internal work may finish before `exit` joins the workers.
The fallback watcher owns a named thread. Exit cancels its directory walk and
artifact hashing between entries or chunks, wakes its polling wait, and joins it
before returning from the session. A previous session cannot retain a polling scan.

Cancellation is checked at query boundaries. Explicit cancellation completes once;
a superseding write cancels diagnostic pulls with a retrigger request. Other reads
invalidated by a pending write return `ContentModified`; clients can request again.
Panics become errors, and completed jobs release snapshots before returning capacity.
The response boundary also covers serialization and propagated query panics. It
reports cancellation only when the retained snapshots prove a pending write or local
cancellation; otherwise a panic is an internal error. Diagnostic pulls report write
cancellation as `ServerCancelled` with `retriggerRequest: true`.
Edit results reject changed context and include document versions where supported.
Plain text edits still require client-side freshness checks.

The worker keeps scheduling and ownership in `src/lsp/worker.rs`, with private
modules for acquisition and feature dispatch. `compiler_acquisition/artifacts.rs`
owns artifact discovery, batch read/parse caches, and AUX traversal. Its parent owns
acquisition generations, scheduling, and publication. `services.rs` handles source
and location observations. All filesystem access remains native. Native host modules and shared protocol
features import their dependencies from the owning module; their entry points do
not provide a private import list for child implementations. Fallback watching honors source
ignore rules and checks explicit compiler candidates, including missing paths
and acquired AUX chains; it does not recursively traverse ignored build trees.
Explicit workspace roots and ordinary inferred document directories are recursive.
An inferred filesystem root watches direct children only, including source files
and configuration. Nested dependencies of a root-level document need an explicit
workspace or client watch scope. Explicit compiler candidates remain watched at
every depth. When scopes repeat a directory, recursive discovery takes precedence.
Distinct nested roots retain their own ignore contexts. Repeated source candidates
share one metadata observation within a polling scan.
Explicit compiler candidates remain native-owned after client watcher acknowledgement
because client exclusions can suppress events. Editor compiler-diagnostic policy
filters reports without disabling compiler acquisition.
Disk events still refresh backing bytes under open overlays for a later close.
Ordinary open-source backing events preserve pending overlay edits. Closed paths,
physical aliases and configuration events invalidate edits before their
asynchronous refresh.
Compiler artifacts remain disk-owned even when their URI has an open overlay.
Their exact acquired watch paths invalidate pending edits immediately, including
physical aliases with other extensions. The native host maintains reference
counts across watch scopes for constant-time membership checks and retires them
with scope replacement, source rename and workspace removal.
File formatting widths override editor widths only when explicitly present.
Native document, range and on-type formatting share that settings resolution.
Exclude matching resolves canonical and native path spellings against the
configuration root, including deleted paths through an existing ancestor. Paths
outside that root do not inherit its exclusions. Artifact scope updates are coalesced and use the polling cadence; only a root
change triggers an immediate scan.

Inverse-search IPC bounds request and response frames to 64 KiB and uses three-second
frame deadlines. Shutdown interrupts accepted clients that have not completed a request.
Connection attempts are bounded too, with a short timeout for the shutdown wake.
The native Unix socket helper stays outside the shared engine. Failed requests retain
live advertisements; only missing or refused endpoints are removed as stale.
Directory listings follow symbolic links when classifying completion candidates;
ordinary entries use their existing directory-entry metadata.

The native host owns client requests, configuration acquisition, watcher fallback,
and capability-gated refresh. Publish settings before requesting refresh; coalesce
changes received while a refresh is outstanding. Browser dispatch is synchronous:
the application owns scheduling, refresh, cancellation between calls, and atomic
validation/application of result preconditions.
Workspace diagnostic streaming filters non-file paths before requesting scoped
file settings. Untitled buffers continue to receive document diagnostics.

Bulk feature conversion shares counted Unicode prefixes through position cursors
while preserving source ranges and response ordering. Unordered and interior-byte
positions retain ordinary conversion semantics. Indexed CST lookup preserves
adjacent-token boundaries, empty nodes, and malformed source. Cursor-target readers
share left-first token candidates and retain their own ownership filters. Out-of-range columns
saturate before clamping on 32-bit hosts. Invalid range-formatting parameter shapes
return `InvalidParams` without terminating the session.

Fix All and bulk quick fixes deduplicate complete edit sets and preserve edit order
and atomic resolution. Suppression suggestions validate at most eight construct
alternatives and one file-wide alternative against the parser, with cancellation
between attempts. They remain explicit choices outside Fix All. Native file-rename
previews run on the read pool; writes and cancellation invalidate their snapshots.

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

## Source readers and protected syntax

The lexer recovers unfinished delimited verbatim at the line boundary or EOF.
Shortverb delimiters and listing settings use immutable scope snapshots; closing a
scope restores its prior state. Literal source bindings retire readers and helpers
at their proved binding offsets. Inert operands cannot establish or retire readers.
Project replacements take precedence over curated and source-discovered profiles.

Curated l3doc name arguments remain raw. Braced multiline documentation preserves
physical margins and printed commands as inert source. Missing braces, unmargined
continuations and docstrip guards end or decline capture rather than swallowing
later code. Operand readers skip physical documentary margins and guards without
crossing structural nodes.

Raw-reader failure caches belong to one lexer pass and include their delimiter,
escape mode and source boundary. Immutable listing values memoize decoding by
lexical profile without affecting source equality. Malformed repeated headers and
raw arguments must not rescan every suffix to EOF.

Protected incremental splices decline unclosed captures, including edits that remove
a closer. Inline captures also prove that appended source cannot extend the capture.
Alltt has active commands and braces while its other special characters are literal. Detokenize preserves
balanced groups and comments without executing raw captures or catcode toggles
inside its token list. The documentation command `\DoNotIndex` uses the same inert
argument ownership. The grammar marks proven token-list bodies in the CST;
definition, navigation, completion, hover, outline, dependency, and lint walks
share that ownership. Environment editing and refactoring exclude printed pairs.
Local source definitions and aliases can decline the primitive
interpretation. Semantic command aliases cannot change parse shape or reclassify
these core commands. The formatter preserves authored whitespace in alltt
bodies and detokenize arguments. Alltt closing delimiters preserve spaces and
tabs between `\end` and the name group while restoring the outer lexical mode.
Primitive `\string` owns exactly the following token. That token is inert: a
printed command cannot create references, load files, capture a literal body, or
change lexical state. Subsequent group contents remain active. Stored macro
parameters retain their parameter role because substitution precedes expansion.
Inside source-qualified definition replacements, known declaration replacements
and optional defaults, and curated builtin macro arguments, braces remain
structural even after `\string`. Unexpanded definitions and macro argument
collection balance their input before those tokens execute. Expanded definitions
retain the same source structure in the parser; it does not evaluate their
expansion or scanner side effects. A single token observer tracks literal declaration
names, fixed header budgets and argument slots, including arguments emitted by
raw readers. Primitive parameter text cannot consume the first replacement-body
brace. A bare or starred `\verb` immediately before a protected body closer
cannot consume that closer as its inline delimiter. Other source-delimited
verbatim captures in those bodies retain their existing ownership.
Supported register, math and text declarations read their control-sequence
names literally, including the starred classic command and math-operator forms.
Supported expl3 control-sequence declarations also protect their first literal
`N` operand and both operands of `cs_*_eq:NN`. A `c` name does not gain a literal
control-sequence slot. Known `Npn`, `Npx`, `Npe`, `Nn`, `Nx` and `Ne` replacements
share the primitive body scanner; other suffixes do not establish body ownership.
Star skipping has a closed profile for sectioning, operatorname,
includegraphics and DeclareMathOperator; other commands do not acquire a star
variant through a general argument heuristic. Nested owners retain their outer scope.
Ordered operand, stored-argument and raw-reader retirement starts at the proved
definition-name offset. Profiles using source-wide suppression retain that
conservative policy. Unknown commands, CWL signatures and editor signature
metadata do not establish argument ownership;
live primitive groups and root groups keep ordinary `\string` behavior.
The closed ltxdoc `\cmd` profile treats its bare or braced control-sequence name
as a literal operand. Its spelling stays navigable without executing raw readers,
catcode changes or reference roles. A source replacement retires this profile
at its proved binding offset.
Embedded TeX lexers carry the original source offset through Fancyvrb command
characters and escape/comment payloads, so ordered source overrides remain exact
without copying the parse context for every island.
Raw builtin readers also respect ordered literal bindings. Source-generated
inline profiles retain their factory's own binding, including uncertain malformed
updates; a later ordinary binding retires the profile's capture.
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
expansion. Incremental reparsing declines edits that could change these modes and edits
inside Python bodies.
The formatter indexes uncertain tokens and their exact adjacent whitespace once.
Affected ancestors use a conservative token stream; documentary frame environments
retain their frame lowerer with protected body tokens. Exact protected boundary
trivia supplies its own line break, so the frame lowerer does not add another.
Independent consumed prose
gaps still normalize while protected tokens and boundaries remain byte-exact,
except configured line endings. Physical documentary framing keeps its existing
layout. Range formatting refuses those documents. Moving source onto an uncertain
percent or dollar line could absorb tokens and change the next parse. Even a new
line before an uncertain token is unsafe: when percent has EOL catcode, replacing
a preceding space with a newline can change the EOL character's input state and
introduce a paragraph. Neutral CST equality alone does not prove that seam safe.
Sorted range queries avoid rescanning each affected subtree. A final uncertain token
and its adjacent trivia are preserved at EOF.
Opaque delegated bodies can still use the protected splice's full fragment proof.
Conditional listing settings and declarations mark the affected family, style,
or generated capture uncertain. Dynamic option keys and unproved setter arguments
cannot retain earlier escape settings as proof of active TeX. Scope restoration
restores these uncertainty facts with the other listing settings. Conditional
comment inclusion remains literal rather than claiming active or excluded source.
The default comment reader terminates on an exact physical `\end{comment}` line.
A single exact verbatim-package `\def\comment` setup establishes a separate
source-qualified reader after its replacement body. That reader recognizes
`\end {comment}` within a physical line and leaves its suffix active. Multiple
bindings or an altered setup decline this profile. Project environment replacements
still take precedence. Installed package metadata does not establish a reader.
Standalone `IncludeInRelease` calls use the closed kernel reader when the source
binds none of its reader-family names. Copied names and stored token lists cannot
establish this default. A source binding to the command or any forwarding helper
requires the complete, exact kernel definition profile instead. Altered or partial
definitions decline the profile. Dates remain ordinary source, the first actual name token is
stringified, and the description is stored token data. Balanced groups retain their
structural ownership. Docstrip, alltt and unproved percent regimes decline this
bounded interpretation. FancyVerb comment characters stop at CR, LF and CRLF.

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

## Editor features

Cached definition sites, operand ownership and modes are shared by signatures,
semantic models, outlines, linting and editor features. Source edits invalidate the
relevant queries. Label descriptions retain their innermost classifying ancestor or
preceding section. The first duplicate declaration wins within a file; lookup checks
the origin before other defining files. Completion matches nonempty queries against
all descriptions before applying its result limit.

Range-token requests derive ownership and typography from the whole document, then
restrict encoding to requested lines. Whole boundary lines preserve full-response
token boundaries. Adjacent spans with identical purposes and modifiers coalesce.
The response policy negotiates types and modifiers independently and remaps both indices and bit positions in the existing token
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

Citation namespaces retain authored keys and choose identity per compilation root.
Direct, active source loads of biblatex default to Biber; literal backend settings
select Biber or classic BibTeX. Unknown backend expressions and pending biblatex
package options open the namespace. Duplicate loads retain the first effective policy.
Backend declarations hidden inside loaded class or package code are not evaluated.
Manual bibitems remain case-sensitive. Literal Biber `ids` aliases are indexed
separately from primary keys; classic BibTeX does not acquire those identities.
Literal entry dependencies carry exact ranges for navigation, completion, and edits.
Usage follows a cycle-safe graph and expands each physical entry once per scope.
Opaque dependencies, aliases, inconsistent shared-root policies, and ambiguous roots
prevent key rename. Required-field lint declines inheritance-dependent absence
claims. This does not implement Biber sourcemaps or a general effective-field model.
Loaded bibliography resources share string navigation, lint facts, and bounded
expansion. Duplicate string definitions produce an ambiguous display; rename needs
one definition and complete coverage of every scope loading it.

Declared key wrappers select a braced argument and optional literal prefix.
They change semantic identities, never parse arity. Per-occurrence prefix proofs
translate canonical rename identities back into authored suffixes.

Completion caches file-role and bibliography search facts per source revision;
cursor prefixes remain request-specific. The ranker counts every match and retains
the best capped candidates in exhaustive stable-sort order. Backend identity
deduplication and literal wrapper-prefix proofs precede selection, so a cap cannot
hide a later higher-ranked result. Descriptions and resolve payloads are constructed
for selected candidates; glossary search descriptions are bounded and cards resolve
lazily. Exhaustive and bounded citation completion share the same resource traversal
and first-definer policy; only selected candidates need response materialization.

Native and browser hosts retain semantic-token baselines for negotiated delta
clients only after successful, accepted response adaptation. Delta responses splice the negotiated integer array.
Each host bounds retained data to 32 documents and 4 MiB, keeps one baseline per URI,
and falls back to full data for unknown IDs or an unhelpful delta. Close, reopen,
file relocation, browser project replacement and shutdown retire baselines.
Only open documents retain them. Native open generations reject late responses
after a lifetime changes.
The shared store performs no filesystem access, scheduling, or thread creation.

### Source hints and package fixes

Source-context hints use current label context. Optional page and name hints use
literal AUX tuple fields and identify their unverified last-build provenance.
Compiler artifacts cannot authorize navigation or edits.
The explicit package-load quick fix requires a unique curated provider and a closed,
plain standard-class preamble. Options, dynamic or conditional loads, loader overrides,
and ambiguous providers decline the action. Metadata-only projects retain the small
curated fallback. With installed sources, the action requires exact SHA-256 identities
from the audited source catalog for the class, existing packages, proposed provider,
and their preamble dependencies. The catalog covers pinned 2025 and 2026 versions of
`article`, `report`, `book`, their default 10pt size files, `amsmath` and its supporting
packages, `amsfonts`, `amssymb`, `array`, and `booktabs`. Installed `graphicx` has no
verified profile. The profiles assume an ordinary current LaTeX format, without
`latexrelease` or compatibility mode. They do not execute TeX or verify custom formats
and later font-use files. Fixture provenance and exact pins live in
`crates/tex-ls-analysis/tests/fixtures/installed-sources/README.md` and `catalog.json`.

Source identities are cached by source revision. Identity alone cannot authorize
an edit. The protocol also proves exact installed filename selection, absence of
earlier local candidates, aliases and editor overlays, and the dependency closure.
Navigation's literate fallback cannot prove a generated load file exists. Unknown
versions, changed bytes, pending observations and acquisition errors decline.

Explicit native quick-fix requests acquire missing observations through the host IO
queue, then retry against a fresh snapshot. Shared diagnostics authorize the edit on
the read pool. Unproved preambles and names without a catalog profile do not trigger
speculative source acquisition. Warm proofs reuse source identities and known
negative locations. Observation retirement retains live proof candidates.

Installed-source watches refresh every owning project, including missing or failed
locations. Deletion or changed filename selection forces a background index refresh;
requests wait for that generation. Watch epochs reject superseded publication.
Routine periodic checks retain the last complete index. Discovery exclusions do not
suppress sources owned by another project or an installed tree. `.clo` sources use
class parsing with implicit `makeatletter`. Embedded hosts supply equivalent source
and location observations.

Edits append the new load after existing preamble loads, preserving their order.
The action is not preferred or part of Fix All.

## Hover documentation

Completion owns context classification, edits, citation traversal, ranking, and
resolution under `tex-ls-protocol/src/completion`. Public compatibility exports keep
existing caller paths. Feature modules import their dependencies explicitly.

Protocol documentation cards are shared by hover, completion resolve, and signature
help. Signature presentation lives in `presentation/signatures.rs`; feature handlers
use analysis lookup directly. Location conversion belongs to `convert`, and rename
edit assembly belongs to `workspace_edits`. Curated descriptions and primitive reference links are editor-only metadata;
they never add parser signatures or infer arguments. Source cards use analysis
navigation definitions and cached `.dtx` associations from the same snapshot.
Completion resolution borrows cached signature scopes without copying their maps.
Its opaque source-path payload preserves ordinary strings and encodes other native
paths as file URI objects, so source selection and lazy documentation retain the
original path identity.
Only unique definitions receive source comments and excerpts. Cards cap definition
links and excerpt length, and render source content as literal code. Local
redefinitions do not inherit built-in descriptions. File-argument cards consume the
same resolved references as navigation, including multiple bibliography style files.
Hosts do not fetch documentation or launch `texdoc` during a hover request.

Bibliography string expansion retains cycle, depth, step and output limits and
shares lookup work across repeated references. Oversized literal pieces are rejected
before copying. Manual bibliography cards require unique keys. Package provenance
supplies manual links. Comment lookup starts at the declaration command and reads at
most sixteen preceding physical lines, supporting LF, CRLF and CR. Hover and
completion share bounded excerpt and description queries.

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
document. Inspection keeps UTF-8 paths in native spelling and represents absolute
non-UTF-8 paths as percent-encoded file URIs when the platform permits it.
Unrepresentable paths return an explicit error object. Path completion omits names that cannot be represented
in UTF-8 edits; tracked source paths keep their original bytes. Neither command
adds language analysis to the client.

Platform-specific VSIX files bundle the corresponding native release executable.
The extension runs in the workspace host for Remote SSH, WSL, and containers.
The extension requires a trusted workspace. Compiler management, builds, and
PDF preview belong to external build integrations.

## The formatter and linter

Formatting lowers syntax with effective signatures and changes trivia only. It must
be idempotent; a consumed space versus one newline cannot decide layout. Preserve
comments and protected bodies except configured line endings. Protocol edits must
reproduce canonical output without splitting Unicode or CRLF boundaries.
LaTeX and BibTeX share output-tail normalization and line-ending resolution. Tail
policies distinguish ordinary trivia, comments and protected bodies. The protocol
uses the formatter's document-body bounds when expanding formatting ranges.

Layout regions and suppression spans use indexed lookup. BibTeX suppression checks
preserve containment by one original range rather than unioning overlapping ranges.
Following-entry directives skip whitespace and ordinary comment entries but retain
free-text JUNK as a target. A trailing `skip` without a following construct remains
dangling. Directive attachment respects malformed groups and exhausted parents.
Resolved suppression directives share diagnostic construction; carrier-specific
messages and eligibility checks remain explicit.

Comment placement follows physical source lines under LF, CRLF and CR. Preserve
trailing spaces and tabs inside comments; configured line endings apply only to
separate line-ending trivia. Printer fit checks stop at the first physical break or
overflow. Preferred prose fills minimize a lexicographic layout cost. Overflow
pruning requires a strictly cheaper split or merge, including when the continuation
margin reaches or exceeds the line width.

Safe lint fixes preserve semantics without later formatting. Fix-all applies complete,
non-conflicting safe fixes from one captured source state; it never runs the
formatter. Unknown-command suppression actions insert comments only after checking
the resulting suppression range. They use `tex-ls` with a rule selector, which
suppresses only that lint rule and leaves formatting enabled. They are non-preferred
quick fixes and stay outside Fix All. Typesetting checks cover keyval and
optional-argument changes that token preservation alone cannot prove safe.
Fix application rejects spans that split UTF-8 characters before accepting a
complete fix, including across files. Accepted replacements and untouched source
slices are copied once per file, preserving equal-position insertion order.
CLI lint fixes currently affect one file per finding; project resolution still
supplies cross-file diagnostics. CLI linting carries the source kind separately
from its reporting path, so stdin retains the `--stdin-filepath` classification.
Unknown-command checks use core metadata and packages loaded by the selected root,
including unconditional package dependencies recorded in CWL metadata. Completion
can offer names from other packages without suppressing their diagnostics.
The typesetting check rejects compiler errors and compares extracted PDF text and
rendered pages.

`formatter/core/source.rs` prepares signature scopes, token ownership, expl3 regions,
suppressions, and the documentary reflow cache once per document. Recursive lowering
borrows that prepared source. `formatter/core/ranges.rs` owns range selection and
indexed interval predicates. Formatter lowering lives in private modules for prose,
expl3, environments, lists, alignment, groups, commands, math and trivia. Expl3 fallback statements end at a
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
