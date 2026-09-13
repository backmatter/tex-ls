# Embedding the language session

`tex-ls-browser` exports `LanguageSession` through wasm-bindgen and
`session::Session` to Rust. See [browser setup](../../crates/tex-ls-browser/README.md)
for a build and JavaScript example. The application supplies sources and owns
workers, transport, storage, and result publication.

## Session lifecycle

Call `dispatch("initialize", params)` first. Positions default to UTF-16; offer UTF-8
in `capabilities.general.positionEncodings` to select it. Use virtual `file:` URIs
for in-memory documents. The browser host does not accept arbitrary URI schemes.

Document versions must increase, but may have gaps. Changes within one notification
apply sequentially and atomically. Repeating `didOpen` replaces the attachment and
invalidates its previous preconditions, even when text and version match. `didClose`
removes the overlay and restores supplied backing contents. `shutdown` rejects
further work; `exit` disposes state. To restart, create a session and resupply inputs.

Workspace roots select the most specific project. `tex-ls/associateSource` accepts
`uri` and `projectUri` to assign a source to a registered project.
`tex-ls/replaceProject` accepts a project `uri` and removes its attachments,
settings, observations, and pending acquisitions. Other projects remain live.

## External inputs

Call `tex-ls/discoveryNeeds` with `{"uri":"file:///project/main.tex"}` for unresolved
literal-file needs. A `source` need requests contents; availability alone does not
load or parse a file. Unknown earlier candidates prevent fallback to later files
or an installed package.

Call `tex-ls/beginExternalRefresh` with a project URI and category:

```json
{"uri":"file:///project/main.tex","kind":"files"}
```

Its result contains a numeric `token`. Pass it with the completed batch to
`tex-ls/applyExternalInputs`:

```json
{
  "token": 1,
  "inputs": {
    "kind": "files",
    "backing": [["/project/chapter.tex", "Chapter text"]],
    "locations": [["/project", {
      "kind": {"state":"present","value":"directory"},
      "directory": {"state":"present","value":{
        "entries":{"chapter.tex":"file","images":"directory"},
        "complete":false
      }}
    }]],
    "fileResolution": []
  }
}
```

Paths are logical and normalized lexically. `fileResolution` maps requested paths
to resolved source or bibliography paths. A null backing value removes stored
contents while preserving an open overlay. Registered sources imply availability.
`tex-ls/registerFile` supplies positive availability without contents; publish an
absent observation to remove it.

Observations distinguish `unknown`, `absent`, `error` with a string `value`, and
`present`. An incomplete directory listing cannot establish absence.

The `installed` category replaces the installed-name index:

```json
{"kind":"installed","value":{"state":"present","value":{"toolchain":"name","files":{"package.sty":"/texmf/package.sty"}}}}
```

Unknown or failed discovery is distinct from an empty index. The `compiler` category
supplies AUX, log, or FLS content, selected by logical filename:

```json
{"kind":"compiler","artifacts":[["/project/main.aux",{"state":"present","value":{"identity":"build/artifact identity","text":"artifact contents"}}]]}
```

An optional artifact `workingDirectory` overrides its logical parent. Shared parsers
extract AUX numbers, compiler messages, and FLS working-directory/input/output facts.
AUX includes follow source order; identities prevent cycles and repeated physical
artifacts. Replacement observations replace prior facts; `absent` clears them.
Last-build numbers cannot create source definitions or authorize rename edits.
Browser-supplied artifacts have unknown source-build provenance.

Tokens are single-use and session-local. A newer token for the same project/category
supersedes older work. Project replacement invalidates pending acquisitions. File
batches also reject obsolete source-storage epochs. Query again after publication;
an existing snapshot does not receive new facts.

## Applying results

`dispatch_with_context(method, params)` returns `{result, preconditions}`. The
JavaScript wrapper exposes the same method with object parameters and results.
Before applying edits or publishing delayed results, pass the preconditions to
`tex-ls/checkPreconditions` as `{preconditions}` and require `{current:true}`.
Validate and apply in one application transaction with no intervening engine writes.
Discard old results after
a session restart. Ordinary `dispatch` returns standard LSP-shaped results.

Echo completion data unchanged to `completionItem/resolve`. The engine rejects
obsolete source/storage tokens and preserves the original insertion fields.

A synchronous WASM call cannot receive cancellation. The worker can discard queued
requests, reject stale results, or terminate and reconstruct the session.

## Diagnostics and refresh

After publishing compiler artifacts, query `textDocument/diagnostic` or
`workspace/diagnostic`. Workspace pulls accept `previousResultIds` and return empty
full reports for removed URIs. The session returns complete reports. The application
owns scheduling, push publication, cancellation, and refresh notifications.
`lint.external.sources` and `lint.external.severities` filter compiler findings.

Re-request semantic tokens after declarations or package sources change. Environment
declarations can also change folding without a text edit. Shared dispatch does not
send refresh requests to the editor.

Color requests support the same literal xcolor declarations as the native host.
Apply the primary and additional presentation edits together against the queried
source version. A chromatic choice for `gray` needs a separate model edit, and the
requested range must match a current literal color span.

## Presentation settings

Pass settings to `tex-ls/updateSettings` globally or per source:

```json
{
  "outline": {
    "labelledEquations": true,
    "unlabelledEquations": false,
    "items": false,
    "environmentNames": { "custom": "Custom display" }
  },
  "inlayHints": { "definitions": true, "references": true, "maxLength": 32 }
}
```

The remaining outline toggles are `sections`, `frames`, `floats`, `theorems`,
`labels`, `macros`, and `environments`, all true by default. Display names make
otherwise transparent environments visible. Hiding a container retains its children.

Hints require an unambiguous AUX label number and fall within the requested range.
`maxLength` accepts 1 to 256 Unicode characters. Page/name references are omitted
because AUX numbers do not supply their rendered text. Refresh hints after settings
or artifacts change.

Workspace symbol queries require every Unicode query token. Exact names rank first,
then the active source's root, then URI and position. A text-document request or
synchronization selects the active source.
