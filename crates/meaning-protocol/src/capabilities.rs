//! Capabilities operations.
use super::*;

/// Capabilities implemented by the shared operations. Hosts add their own extensions.
pub fn server_capabilities(
    encoding: PositionEncoding,
    supports_pull_diagnostics: bool,
) -> ServerCapabilities {
    ServerCapabilities {
        // Echo the negotiated encoding (see `negotiate_position_encoding`).
        position_encoding: Some(match encoding {
            PositionEncoding::Utf8 => PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => PositionEncodingKind::UTF16,
        }),
        text_document_sync: Some(TextDocumentSync::Options(
            lsp_types::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::Incremental),
                ..Default::default()
            },
        )),
        diagnostic_provider: supports_pull_diagnostics.then(|| {
            DiagnosticProvider::DiagnosticOptions(DiagnosticOptions {
                identifier: Some("meaning".to_owned()),
                // Editing an `\input` target / `.bib` changes this file's
                // `undefined-ref` / `undefined-citation` set, so a pull in one file
                // can depend on another's content.
                inter_file_dependencies: true,
                // Deferred: workspace pull is a streaming/long-poll protocol that
                // fits the one-shot read-job model poorly (see `TODO.md`).
                workspace_diagnostics: false,
                work_done_progress_options: Default::default(),
            })
        }),
        document_formatting_provider: Some(true.into()),
        // Format the editor selection, expanded to whole document-level blocks (see
        // `compute_range_format`).
        document_range_formatting_provider: Some(true.into()),
        // Re-indent on close: typing `}` re-indents the containing block when that
        // `}` structurally closes a multi-line group or an `\end{…}` (see
        // `compute_on_type_format`). Client opt-in (e.g. `editor.formatOnType`).
        document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
            first_trigger_character: "}".to_owned(),
            more_trigger_character: None,
        }),
        document_symbol_provider: Some(true.into()),
        // Aggregate the per-file outline (sections, frames, labels, floats,
        // theorems, macros, environments) across every tracked project file. No lazy
        // `resolve`, so each result carries its full `Location`.
        workspace_symbol_provider: Some(true.into()),
        // Surface linter autofixes and syntax-aware refactorings. `Simple(true)`
        // returns fully-built actions (no `codeAction/resolve` step).
        code_action_provider: Some(CodeActionProvider::Bool(true)),
        // The change-environment refactor (see [`on_execute_command`]). The
        // `texlab.…` alias keeps texlab client integrations working as-is.
        hover_provider: Some(HoverProvider::Bool(true)),
        // Show the active argument while typing a command's/environment's
        // `{…}`/`[…]` arguments. `{`/`[` open an argument; `}`/`]` as *retriggers*
        // make the client re-query when one closes, so the between-arguments
        // `null` dismisses the popup and a still-inside position advances the
        // highlight. No `,`: a `\cite{a,b}` key list is one slot.
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["{".to_owned(), "[".to_owned()]),
            retrigger_characters: Some(vec!["}".to_owned(), "]".to_owned()]),
            work_done_progress_options: Default::default(),
        }),
        definition_provider: Some(true.into()),
        references_provider: Some(true.into()),
        // Shade a cross-reference key and every same-key occurrence in the buffer.
        // Single-file (the lightweight cousin of `references_provider`).
        document_highlight_provider: Some(true.into()),
        // Rename a `\label`/`\cite` key and every referencing command across its
        // namespace. `prepare_provider` lets the client pre-validate the cursor and
        // anchor the prepare range to the key token.
        rename_provider: Some(lsp_types::RenameProvider::RenameOptions(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        folding_range_provider: Some(FoldingRangeProvider::Bool(true)),
        // Expand-selection: nested ranges walking outward through the CST hierarchy
        // (token -> group -> argument -> command -> environment -> ... -> root).
        selection_range_provider: Some(SelectionRangeProvider::Bool(true)),
        // Clickable include edges: `\input`/`\include`/`\import`, `\usepackage`/
        // `\documentclass`, `\bibliography`/`\addbibresource`, `\includegraphics`.
        // Links are built eagerly (target + range together), so no `resolve` step.
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        completion_provider: Some(CompletionOptions {
            // `\` opens command/env names; `{` opens a name/key/path argument;
            // `/` re-triggers path segments. Snippet support is read off the
            // client's capabilities, so no extra server flag is needed.
            trigger_characters: Some(vec![
                "\\".to_owned(),
                "{".to_owned(),
                "/".to_owned(),
                "@".to_owned(),
            ]),
            // A highlighted item is sent back via `completionItem/resolve` to gain
            // its signature/citation detail lazily (see [`completion_resolve`]).
            resolve_provider: Some(true),
            ..Default::default()
        }),
        // texlab's spelling for the custom `textDocument/forwardSearch` method,
        // so a client that already probes for it finds meaning too. Advertised
        // unconditionally: the viewer settings can arrive later (or change) via
        // `didChangeConfiguration`, and it is the `Unconfigured` status — not a
        // missing capability — that tells a client to prompt for a viewer.
        ..Default::default()
    }
}
