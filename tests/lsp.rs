//! End-to-end smoke test for the minimal LSP server (Phase 4).
//!
//! Drives the full transcript over an in-process `Connection::memory()` pair:
//! `initialize` → `initialized` → `didOpen` (a doc with a parse error) →
//! assert pushed diagnostics → `didChange` (to a valid, messy doc) → assert the
//! diagnostics clear → `textDocument/formatting` → assert the edit equals the
//! formatter's own output → `shutdown` → `exit`.

#[macro_use]
#[path = "support/paths.rs"]
mod test_paths;

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use tex_ls_formatter::formatter::{FormatStyle, format_with_style};

use lsp_types::{
    ClientCapabilities, Code, CodeActionContext, CodeActionKind, CodeActionParams,
    CodeActionProvider, CodeActionResponse, CompletionItem, CompletionItemKind, CompletionParams,
    CompletionResponse, Contents, DefinitionParams, DefinitionResponse,
    DiagnosticClientCapabilities, DiagnosticWorkspaceClientCapabilities,
    DidChangeTextDocumentParams, DidChangeWatchedFilesClientCapabilities,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportProgress,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
    DocumentLink, DocumentLinkOptions, DocumentLinkParams, DocumentOnTypeFormattingOptions,
    DocumentOnTypeFormattingParams, DocumentRangeFormattingParams, DocumentSymbol,
    DocumentSymbolParams, DocumentSymbolResponse, FileChangeType, FileEvent, FoldingRange,
    FoldingRangeKind, FoldingRangeParams, FoldingRangeProvider, FormattingOptions,
    GeneralClientCapabilities, Hover, HoverParams, HoverProvider, InitializeParams,
    InitializeResult, InitializedParams, InsertTextFormat, Location, PartialResultParams, Position,
    PositionEncodingKind, PrepareRenameResult, PublishDiagnosticsParams, Range, ReferenceContext,
    ReferenceParams, RegistrationParams, RenameOptions, RenameParams, SelectionRange,
    SelectionRangeParams, SelectionRangeProvider, SignatureHelp, SignatureHelpParams, SymbolKind,
    TextDocumentClientCapabilities, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TextEdit, Uri, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceEdit, WorkspaceSymbolLocation,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
};

/// Build a valid `file://` URI from a filesystem path, cross-platform. A raw
/// path can't be string-formatted into a URI directly: on Windows it uses
/// backslashes and a drive-letter colon (`C:\dir`), which is not valid URI
/// syntax. Normalize separators to `/` and ensure a leading `/` so a drive
/// path becomes `file:///C:/dir` (matching the server's `uri_to_fs_path`).
fn path_to_file_uri(path: &std::path::Path) -> Uri {
    let mut s = path.display().to_string().replace('\\', "/");
    if !s.starts_with('/') {
        s.insert(0, '/');
    }
    format!("file://{s}")
        .parse()
        .expect("path should form a valid file:// URI")
}

fn recv(client: &Connection) -> Message {
    client
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("timed out waiting for a server message")
}

fn recv_response(client: &Connection) -> Response {
    loop {
        match recv(client) {
            Message::Response(resp) => return resp,
            Message::Request(request) if request.method == "workspace/inlayHint/refresh" => {
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        request.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
            Message::Notification(not) if not.method == "textDocument/publishDiagnostics" => {}
            other => panic!("expected a response, got {other:?}"),
        }
    }
}

fn recv_diagnostics(client: &Connection) -> PublishDiagnosticsParams {
    match recv(client) {
        Message::Notification(not) if not.method == "textDocument/publishDiagnostics" => {
            serde_json::from_value(not.params).expect("valid PublishDiagnosticsParams")
        }
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }
}

fn send_request(client: &Connection, id: i32, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(id),
            method: method.to_owned(),
            params,
        }))
        .unwrap();
}

fn send_notification(client: &Connection, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: method.to_owned(),
            params,
        }))
        .unwrap();
}

/// Spawn an in-process server, perform the `initialize`/`initialized` handshake
/// (passing `init_options` as `initializationOptions`), and return the client end
/// plus the server thread handle.
fn rich_capabilities() -> serde_json::Value {
    serde_json::json!({"workspace":{"applyEdit":true},"textDocument":{
        "completion":{"completionItem":{"snippetSupport":true,"documentationFormat":["markdown"]},"completionItemKind":{"valueSet":(1..=25).collect::<Vec<_>>()}},
        "hover":{"contentFormat":["markdown"]},
        "documentSymbol":{"hierarchicalDocumentSymbolSupport":true,"symbolKind":{"valueSet":(1..=26).collect::<Vec<_>>()}},
        "codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":[]}},"isPreferredSupport":true},
        "publishDiagnostics":{"versionSupport":true,"relatedInformation":true,"codeDescriptionSupport":true,"tagSupport":{"valueSet":[1,2]}}
    }})
}

fn start_server(
    init_options: Option<serde_json::Value>,
) -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());

    // Generic language tests do not depend on the machine's TeX installation.
    // Installation-specific tests supply explicit TEXMF settings.
    let mut init_options = init_options.unwrap_or_else(|| serde_json::json!({}));
    init_options
        .as_object_mut()
        .unwrap()
        .entry("texmf")
        .or_insert_with(|| serde_json::json!({"enabled":false}));
    let params = InitializeParams {
        initialization_options: Some(init_options),
        capabilities: serde_json::from_value(rich_capabilities()).unwrap(),
        ..Default::default()
    };
    send_request(
        &client,
        1,
        "initialize",
        serde_json::to_value(params).unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(1));
    let init: InitializeResult =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert_server_capabilities(&init);
    assert!(matches!(
        init.capabilities.code_action_provider,
        Some(CodeActionProvider::CodeActionOptions(_))
    ));
    assert!(
        init.capabilities.diagnostic_provider.is_none(),
        "diagnosticProvider is gated on client pull support, which this client lacks"
    );
    assert_eq!(
        init.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF16),
        "a client offering no positionEncodings gets the mandatory UTF-16 default"
    );
    send_notification(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );
    (client, server_thread)
}

/// Every capability the server advertises to *every* client, asserted in one
/// place so each handshake helper checks the same list. Client-dependent
/// capabilities (`diagnosticProvider`, `positionEncoding`) stay with their
/// helper.
fn assert_server_capabilities(init: &InitializeResult) {
    assert!(
        init.capabilities.document_formatting_provider.is_some(),
        "server must advertise documentFormattingProvider"
    );
    assert!(
        init.capabilities
            .document_range_formatting_provider
            .is_some(),
        "server must advertise documentRangeFormattingProvider"
    );
    assert!(
        matches!(
            init.capabilities.document_on_type_formatting_provider,
            Some(DocumentOnTypeFormattingOptions { ref first_trigger_character, .. })
                if first_trigger_character == "}"
        ),
        "server must advertise documentOnTypeFormattingProvider triggered by `}}`"
    );
    assert!(
        matches!(
            init.capabilities.document_symbol_provider,
            Some(lsp_types::DocumentSymbolProvider::Bool(true))
        ),
        "server must advertise documentSymbolProvider"
    );
    assert!(
        matches!(
            init.capabilities.workspace_symbol_provider,
            Some(lsp_types::WorkspaceSymbolProvider::Bool(true))
        ),
        "server must advertise workspaceSymbolProvider"
    );
    assert!(
        init.capabilities.completion_provider.is_some(),
        "server must advertise completionProvider"
    );
    assert!(
        matches!(
            init.capabilities.definition_provider,
            Some(lsp_types::DefinitionProvider::Bool(true))
        ),
        "server must advertise definitionProvider"
    );
    assert!(
        matches!(
            init.capabilities.references_provider,
            Some(lsp_types::ReferencesProvider::Bool(true))
        ),
        "server must advertise referencesProvider"
    );
    assert!(
        matches!(
            init.capabilities.rename_provider,
            Some(lsp_types::RenameProvider::RenameOptions(RenameOptions {
                prepare_provider: Some(true),
                ..
            }))
        ),
        "server must advertise renameProvider with prepare support"
    );
    assert!(
        matches!(
            init.capabilities.folding_range_provider,
            Some(FoldingRangeProvider::Bool(true))
        ),
        "server must advertise foldingRangeProvider"
    );
    assert!(
        matches!(
            init.capabilities.selection_range_provider,
            Some(SelectionRangeProvider::Bool(true))
        ),
        "server must advertise selectionRangeProvider"
    );
    assert!(
        matches!(
            init.capabilities.document_link_provider,
            Some(DocumentLinkOptions {
                resolve_provider: Some(false),
                ..
            })
        ),
        "server must advertise documentLinkProvider"
    );
    assert!(
        matches!(
            init.capabilities.hover_provider,
            Some(HoverProvider::Bool(true))
        ),
        "server must advertise hoverProvider"
    );
    assert!(
        init.capabilities
            .signature_help_provider
            .as_ref()
            .and_then(|opts| opts.trigger_characters.as_deref())
            == Some(&["{".to_owned(), "[".to_owned()][..]),
        "server must advertise signatureHelpProvider triggered by `{{` and `[`"
    );
    assert!(init.server_info.is_some(), "server identifies itself");
    assert!(init.capabilities.linked_editing_range_provider.is_some());
    assert!(init.capabilities.text_document_sync.is_some());
    assert!(
        init.capabilities
            .execute_command_provider
            .as_ref()
            .unwrap()
            .commands
            .contains(&"tex-ls.forwardSearch".to_owned())
    );
    assert!(init.capabilities.experimental.is_none());
}

/// Spawn an in-process server and handshake as a **pull-capable** client (advertises
/// `textDocument/diagnostic` and `workspace.diagnostic.refreshSupport`). Such a
/// client is served diagnostics pull-only — the server suppresses `publishDiagnostics`.
fn start_server_pull() -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());

    let params = InitializeParams {
        initialization_options: Some(serde_json::json!({"texmf":{"enabled":false}})),
        capabilities: ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                diagnostic: Some(DiagnosticClientCapabilities::default()),
                ..Default::default()
            }),
            workspace: Some(WorkspaceClientCapabilities {
                diagnostics: Some(DiagnosticWorkspaceClientCapabilities {
                    refresh_support: Some(true),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    send_request(
        &client,
        1,
        "initialize",
        serde_json::to_value(params).unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(1));
    let init: InitializeResult =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert_server_capabilities(&init);
    assert!(
        init.capabilities.diagnostic_provider.is_some(),
        "server must advertise diagnosticProvider (pull diagnostics)"
    );
    send_notification(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );
    (client, server_thread)
}

/// Send a `textDocument/diagnostic` pull request.
fn pull_diagnostic(client: &Connection, id: i32, uri: &Uri, previous_result_id: Option<String>) {
    send_request(
        client,
        id,
        "textDocument/diagnostic",
        serde_json::to_value(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: None,
            previous_result_id,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
}

/// Receive the response to a pull request `id`, parsed as a report. Asserts that **no
/// `publishDiagnostics` push** arrives first (pull and push are mutually exclusive);
/// tolerates and acks any server-initiated request (e.g. `workspace/diagnostic/refresh`).
fn recv_document_diagnostic_report(client: &Connection, id: i32) -> DocumentDiagnosticReport {
    loop {
        match recv(client) {
            Message::Response(resp) => {
                assert_eq!(resp.id, RequestId::from(id));
                let result: DocumentDiagnosticReportProgress =
                    serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
                match result {
                    DocumentDiagnosticReportProgress::DocumentDiagnosticReport(report) => {
                        return report;
                    }
                    DocumentDiagnosticReportProgress::DocumentDiagnosticReportPartialResult(_) => {
                        panic!("server returned a partial report; none was requested")
                    }
                }
            }
            Message::Notification(not) if not.method == "textDocument/publishDiagnostics" => {
                panic!("pull-mode client must not receive a publishDiagnostics push")
            }
            Message::Notification(_) => continue,
            // Ack a server→client request (e.g. workspace/diagnostic/refresh) and keep waiting.
            Message::Request(req) => {
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        req.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
        }
    }
}

/// Extract the items from a full report (or `None` if it is an `unchanged` report).
fn report_items(report: &DocumentDiagnosticReport) -> Option<&[lsp_types::Diagnostic]> {
    match report {
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(full) => {
            Some(&full.full_document_diagnostic_report.items)
        }
        DocumentDiagnosticReport::RelatedUnchangedDocumentDiagnosticReport(_) => None,
    }
}

/// The `result_id` carried by either report kind.
fn report_result_id(report: &DocumentDiagnosticReport) -> Option<String> {
    match report {
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(full) => {
            full.full_document_diagnostic_report.result_id.clone()
        }
        DocumentDiagnosticReport::RelatedUnchangedDocumentDiagnosticReport(unchanged) => Some(
            unchanged
                .unchanged_document_diagnostic_report
                .result_id
                .clone(),
        ),
    }
}

fn did_open(client: &Connection, uri: &Uri, version: i32, text: &str) {
    send_notification(
        client,
        "textDocument/didOpen",
        serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "latex".into(),
                version,
                text: text.to_owned(),
            },
        })
        .unwrap(),
    );
}

fn shutdown(client: &Connection, server_thread: std::thread::JoinHandle<()>) {
    send_request(client, 99, "shutdown", serde_json::Value::Null);
    // Drain any in-flight notifications (e.g. a project re-lint racing the response).
    let resp = loop {
        match recv(client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            Message::Request(request) if request.method == "workspace/inlayHint/refresh" => {
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        request.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
            other => panic!("expected the shutdown response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(99));
    send_notification(client, "exit", serde_json::Value::Null);
    server_thread.join().expect("server thread panicked");
}

#[test]
fn lsp_formatting_and_diagnostics_transcript() {
    let (client, server_thread) = start_server(None);

    let uri: Uri = fixture_uri!("/test.tex").parse().unwrap();

    // didOpen a document with an unclosed environment → diagnostics.
    let broken = "\\begin{itemize}\n\\item a\n";
    did_open(&client, &uri, 1, broken);
    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        !diags.diagnostics.is_empty(),
        "an unclosed environment must produce at least one diagnostic"
    );

    // didChange to a valid but messy document → diagnostics clear.
    let messy = "\\section{Hi}   \n\n\n\ntext.  ";
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: messy.to_owned(),
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(
        diags.diagnostics.is_empty(),
        "a valid document must clear diagnostics, got {:?}",
        diags.diagnostics
    );

    // textDocument/formatting → a single whole-document edit equal to the
    // formatter's own output at the requested tab size.
    send_request(
        &client,
        2,
        "textDocument/formatting",
        serde_json::to_value(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let expected = format_with_style(
        messy,
        FormatStyle {
            line_width: 80,
            indent_width: 2,
            ..FormatStyle::default()
        },
    )
    .unwrap();
    assert_eq!(apply_edits(messy, &edits), expected);

    shutdown(&client, server_thread);
}

#[test]
fn lsp_range_formatting_formats_only_the_selected_block() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/range.tex").parse().unwrap();

    // Two messy top-level paragraphs (extra inter-word spaces). Range formatting
    // the first must collapse only its spaces, leaving the second untouched — the
    // distinguishing property versus whole-document formatting.
    let doc = "first    paragraph.\n\nsecond    paragraph.\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    // A *partial* selection inside the first paragraph: it must clamp out to the
    // whole block.
    let select_first = Range {
        start: Position::new(0, 2),
        end: Position::new(0, 5),
    };
    let range_params = |range: Range| {
        serde_json::to_value(DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range,
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap()
    };

    send_request(
        &client,
        2,
        "textDocument/rangeFormatting",
        range_params(select_first),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let formatted = apply_edits(doc, &edits);
    assert_eq!(
        formatted, "first paragraph.\n\nsecond    paragraph.\n",
        "only the selected block is formatted; the second paragraph is untouched"
    );

    // Seam idempotence: with the buffer now holding the post-edit text, the same
    // selection yields no edits.
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: formatted.clone(),
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty());

    send_request(
        &client,
        3,
        "textDocument/rangeFormatting",
        range_params(select_first),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(3));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert!(
        edits.is_empty(),
        "an already-formatted selection yields no edits, got {edits:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_range_formatting_inside_document_leaves_sibling_environment_unchanged() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/range_document.tex").parse().unwrap();

    let doc = concat!(
        "\\begin{document}\n\n",
        "\\begin{frame}{Title}\n",
        "\\begin{description}\n",
        "\\item[Item]\n",
        "  Text    Text\n",
        "\\end{description}\n",
        "\\end{frame}\n\n",
        "Selected    paragraph.\n\n",
        "\\end{document}\n",
    );
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    send_request(
        &client,
        2,
        "textDocument/rangeFormatting",
        serde_json::to_value(DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position::new(9, 2),
                end: Position::new(9, 10),
            },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let formatted = apply_edits(doc, &edits);
    assert_eq!(
        formatted,
        doc.replace("Selected    paragraph.", "Selected paragraph."),
        "formatting a document-body paragraph must not reformat its sibling frame"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_range_formatting_reindents_multiline_environment() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/range_env.tex").parse().unwrap();

    // A poorly-indented multi-line environment followed by a messy paragraph.
    let doc = "\\begin{itemize}\n\\item one\n\\item two\n\\end{itemize}\n\nsecond    paragraph.\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    // Cursor inside the environment body (line 1). It clamps out to the whole
    // environment block.
    send_request(
        &client,
        2,
        "textDocument/rangeFormatting",
        serde_json::to_value(DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position::new(1, 3),
                end: Position::new(1, 3),
            },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let formatted = apply_edits(doc, &edits);
    assert_eq!(
        formatted,
        "\\begin{itemize}\n  \\item one\n  \\item two\n\\end{itemize}\n\nsecond    paragraph.\n",
        "the environment body is reindented; the trailing paragraph is untouched"
    );

    shutdown(&client, server_thread);
}

/// Build `textDocument/onTypeFormatting` params for a `}` typed at `position`
/// (the cursor sits just past the brace).
fn on_type_params(uri: &Uri, position: Position) -> serde_json::Value {
    serde_json::to_value(DocumentOnTypeFormattingParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position,
        ch: "}".to_owned(),
        options: FormattingOptions {
            tab_size: 2,
            insert_spaces: true,
            ..Default::default()
        },
    })
    .unwrap()
}

#[test]
fn lsp_on_type_formatting_reindents_on_environment_close() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/ontype_env.tex").parse().unwrap();

    // A poorly-indented multi-line environment followed by a messy paragraph.
    let doc = "\\begin{itemize}\n\\item one\n\\item two\n\\end{itemize}\n\nsecond    paragraph.\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    // The user just typed the `}` closing `\end{itemize}` on line 3 (13 columns:
    // `\end{itemize}`), so the cursor is at column 13.
    send_request(
        &client,
        2,
        "textDocument/onTypeFormatting",
        on_type_params(&uri, Position::new(3, 13)),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let formatted = apply_edits(doc, &edits);
    assert_eq!(
        formatted,
        "\\begin{itemize}\n  \\item one\n  \\item two\n\\end{itemize}\n\nsecond    paragraph.\n",
        "closing `\\end{{itemize}}` reindents the environment; the paragraph is untouched"
    );

    // Idempotence at the seam: with the buffer now holding the reindented text,
    // typing the same `}` again yields no edits.
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: formatted.clone(),
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty());

    send_request(
        &client,
        3,
        "textDocument/onTypeFormatting",
        on_type_params(&uri, Position::new(3, 13)),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(3));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert!(
        edits.is_empty(),
        "an already-reindented environment yields no edits, got {edits:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_on_type_formatting_ignores_inline_group_close() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/ontype_inline.tex").parse().unwrap();

    // An inline `\textbf{x}` inside a paragraph with messy spacing. Closing its
    // single-line group must NOT trigger a reformat (no prose reflow on `}`).
    let doc = "a    \\textbf{x} and    more.\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    // `a    \textbf{x}`: `a`(0) + 4 spaces + `\textbf`(5-11) + `{`(12) + `x`(13)
    // + `}`(14), so the cursor just past the `}` is at column 15.
    send_request(
        &client,
        2,
        "textDocument/onTypeFormatting",
        on_type_params(&uri, Position::new(0, 15)),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert!(
        edits.is_empty(),
        "closing an inline single-line group yields no edits, got {edits:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_on_type_formatting_refuses_when_buffer_has_parse_errors() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/ontype_broken.tex").parse().unwrap();

    // A multi-line environment (whose `\end{itemize}` close would normally fire)
    // followed by a stray `\end{extra}` that makes the buffer fail to parse
    // cleanly. On-type formatting must refuse: no edits.
    let doc = "\\begin{itemize}\n\\item one\n\\end{itemize}\n\\end{extra}\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(
        !diags.diagnostics.is_empty(),
        "the stray `\\end` must produce a parse diagnostic"
    );

    // Cursor just past the `}` of the (well-formed) `\end{itemize}` on line 2.
    send_request(
        &client,
        2,
        "textDocument/onTypeFormatting",
        on_type_params(&uri, Position::new(2, 13)),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    // A refusal serializes to JSON `null`; either that or an empty array means
    // "no change".
    let edits: Option<Vec<TextEdit>> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert!(
        edits.unwrap_or_default().is_empty(),
        "a buffer with parse errors yields no on-type edits"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_symbol_outline() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/outline.tex").parse().unwrap();

    // A section containing a figure (with a label), a theorem, and a Beamer frame.
    let doc = "\\section{Intro}\n\
        \\begin{figure}\n\
        \\label{fig:one}\n\
        \\end{figure}\n\
        \\begin{theorem}\n\
        x\n\
        \\end{theorem}\n\
        \\begin{frame}[plain]{Summary}\n\
        \\label{frame:summary}\n\
        \\end{frame}\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    send_request(
        &client,
        2,
        "textDocument/documentSymbol",
        serde_json::to_value(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let response: DocumentSymbolResponse =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a documentSymbol response");
    let DocumentSymbolResponse::DocumentSymbolList(symbols) = response else {
        panic!("expected a nested documentSymbol response");
    };

    // One root section; the figure, theorem, and frame nest under it.
    assert_eq!(symbols.len(), 1);
    let section = &symbols[0];
    assert_eq!(section.name, "Intro");
    assert_eq!(section.kind, SymbolKind::Module);
    let children = section.children.as_deref().unwrap_or_default();
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["figure", "theorem", "Summary"]);

    // The figure carries its label as a leaf.
    let figure = &children[0];
    assert_eq!(figure.kind, SymbolKind::Object);
    let figure_kids: &[DocumentSymbol] = figure.children.as_deref().unwrap_or_default();
    assert_eq!(figure_kids.len(), 1);
    assert_eq!(figure_kids[0].name, "fig:one");
    assert_eq!(figure_kids[0].kind, SymbolKind::Constant);

    assert_eq!(children[1].kind, SymbolKind::Class);
    assert_eq!(children[2].kind, SymbolKind::Class);
    assert_eq!(
        children[2].children.as_deref().unwrap_or_default()[0].name,
        "frame:summary"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_symbol_numbers_from_aux() {
    // A compiled project: the sibling `.aux` prefixes section names with their toc
    // numbers and attaches label/float numbers as `detail`.
    let dir = tempfile::tempdir().expect("temp dir");
    let doc = "\\documentclass{article}\n\
        \\begin{document}\n\
        \\section{Intro}\n\
        \\label{sec:intro}\n\
        \\begin{figure}\n\
        \\label{fig:one}\n\
        \\end{figure}\n\
        \\section{Methods}\n\
        \\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, doc).unwrap();
    std::fs::write(
        dir.path().join("main.aux"),
        "\\@writefile{toc}{\\contentsline {section}{\\numberline {1}Intro}{1}{section.1}}\n\
         \\newlabel{sec:intro}{{1}{1}{Intro}{section.1}{}}\n\
         \\newlabel{fig:one}{{3}{2}{}{figure.3}{}}\n\
         \\@writefile{toc}{\\contentsline {section}{\\numberline {2}Methods}{2}{section.2}}\n",
    )
    .unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    send_request(
        &client,
        2,
        "textDocument/documentSymbol",
        serde_json::to_value(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let response: DocumentSymbolResponse =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a documentSymbol response");
    let DocumentSymbolResponse::DocumentSymbolList(symbols) = response else {
        panic!("expected a nested documentSymbol response");
    };

    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["1 Intro", "2 Methods"]);

    let intro_kids = symbols[0].children.as_deref().unwrap_or_default();
    let label = intro_kids
        .iter()
        .find(|c| c.name == "sec:intro")
        .expect("label leaf");
    assert_eq!(label.detail.as_deref(), Some("1"));

    let figure = intro_kids
        .iter()
        .find(|c| c.name == "figure")
        .expect("figure symbol");
    assert_eq!(figure.detail.as_deref(), Some("3"), "via its child label");
    let figure_kids = figure.children.as_deref().unwrap_or_default();
    assert_eq!(figure_kids[0].detail.as_deref(), Some("3"));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_symbol_dtx_documented_macros() {
    let (client, server_thread) = start_server(None);
    // A `.dtx` is parsed in docstrip mode, so the leading-`%` ltxdoc lines become
    // real `macro`/`\DescribeMacro` constructs surfaced as document symbols.
    let uri: Uri = fixture_uri!("/pkg.dtx").parse().unwrap();

    let doc = "\\section{Implementation}\n\
        % \\DescribeMacro{\\foo}\n\
        % \\begin{macro}{\\bar}\n\
        %    \\begin{macrocode}\n\
        \\def\\bar{b}\n\
        %    \\end{macrocode}\n\
        % \\end{macro}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    send_request(
        &client,
        2,
        "textDocument/documentSymbol",
        serde_json::to_value(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let response: DocumentSymbolResponse =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a documentSymbol response");
    let DocumentSymbolResponse::DocumentSymbolList(symbols) = response else {
        panic!("expected a nested documentSymbol response");
    };

    // One root section; the documented macros nest under it as FUNCTION symbols.
    assert_eq!(symbols.len(), 1);
    let section = &symbols[0];
    assert_eq!(section.name, "Implementation");
    assert_eq!(section.kind, SymbolKind::Module);
    let children = section.children.as_deref().unwrap_or_default();
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["\\foo", "\\bar"]);
    assert!(children.iter().all(|c| c.kind == SymbolKind::Function));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_folding_ranges() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/fold.tex").parse().unwrap();

    // line 0: \section{Intro}
    //      1-3: a comment block
    //      4-6: a multi-line environment
    let doc = "\\section{Intro}\n\
        % a\n\
        % b\n\
        % c\n\
        \\begin{itemize}\n\
        \\item x\n\
        \\end{itemize}\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    send_request(
        &client,
        2,
        "textDocument/foldingRange",
        serde_json::to_value(FoldingRangeParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let ranges: Vec<FoldingRange> = serde_json::from_value(resp.response_result.clone().unwrap())
        .expect("a foldingRange response");

    let triples: Vec<(u32, u32, Option<FoldingRangeKind>)> = ranges
        .iter()
        .map(|r| (r.start_line, r.end_line, r.kind.clone()))
        .collect();
    // The section spans the whole document; the comment block folds 1..3; the
    // itemize folds 4..6.
    assert!(
        triples.contains(&(0, 6, None)),
        "section fold, got {triples:?}"
    );
    assert!(
        triples.contains(&(1, 3, Some(FoldingRangeKind::Comment))),
        "comment fold, got {triples:?}"
    );
    assert!(
        triples.contains(&(4, 6, None)),
        "itemize fold, got {triples:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_selection_ranges() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/sel.tex").parse().unwrap();

    // \section{Intro}\n — cursor on the 'n' of Intro (line 0, char 10).
    let doc = "\\section{Intro}\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc -> no diagnostics");

    send_request(
        &client,
        2,
        "textDocument/selectionRange",
        serde_json::to_value(SelectionRangeParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            positions: vec![Position::new(0, 10)],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let ranges: Vec<SelectionRange> = serde_json::from_value(resp.response_result.clone().unwrap())
        .expect("a selectionRange response");

    // One chain per input position.
    assert_eq!(ranges.len(), 1, "one chain per position, got {ranges:?}");

    // Flatten the chain innermost -> outermost and assert it widens through the
    // expected CST levels: the "Intro" word, the {Intro} group, the \section command,
    // and the document root.
    let mut flat: Vec<Range> = Vec::new();
    let mut cur = Some(Box::new(ranges[0].clone()));
    while let Some(node) = cur {
        flat.push(node.range);
        cur = node.parent;
    }
    let pairs: Vec<((u32, u32), (u32, u32))> = flat
        .iter()
        .map(|r| {
            (
                (r.start.line, r.start.character),
                (r.end.line, r.end.character),
            )
        })
        .collect();
    assert_eq!(
        pairs[0],
        ((0, 9), (0, 14)),
        "innermost is the title word, got {pairs:?}"
    );
    assert!(
        pairs.contains(&((0, 8), (0, 15))),
        "the {{Intro}} group is a level, got {pairs:?}"
    );
    assert!(
        pairs.contains(&((0, 0), (0, 15))),
        "the \\section command is a level, got {pairs:?}"
    );
    assert_eq!(
        *pairs.last().unwrap(),
        ((0, 0), (1, 0)),
        "outermost is the whole document, got {pairs:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_links() {
    // Document links are disk-aware: only targets that exist on disk are linked.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("part.tex"), "").unwrap();
    std::fs::write(dir.path().join("mypkg.sty"), "").unwrap();
    std::fs::write(dir.path().join("refs.bib"), "").unwrap();
    std::fs::write(dir.path().join("fig.png"), "").unwrap();
    // Disable the installed-tree (TEXMF) fallback so this test stays hermetic: it
    // asserts the *local-only* contract, and a machine with TeX installed would
    // otherwise resolve the system `amsmath` and add a fifth link.
    let main_path = dir.path().join("main.tex");
    // `\usepackage{amsmath}` has no local file, so it must NOT be linked.
    let main = "\\input{part}\n\
        \\usepackage{mypkg}\n\
        \\usepackage{amsmath}\n\
        \\addbibresource{refs.bib}\n\
        \\includegraphics{fig}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) =
        start_server(Some(serde_json::json!({ "texmf": { "enabled": false } })));
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    send_request(
        &client,
        2,
        "textDocument/documentLink",
        serde_json::to_value(DocumentLinkParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    // A freshly-seeded project re-lints, so drain any stray diagnostics first.
    let resp = loop {
        match recv(&client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(2));
    let links: Vec<DocumentLink> = serde_json::from_value(resp.response_result.clone().unwrap())
        .expect("a documentLink response");

    // Four resolvable targets; the system `amsmath` package is absent, so no link.
    let targets: Vec<Uri> = links.iter().filter_map(|l| l.target.clone()).collect();
    assert_eq!(links.len(), 4, "got {targets:?}");
    assert!(targets.contains(&path_to_file_uri(&dir.path().join("part.tex"))));
    assert!(targets.contains(&path_to_file_uri(&dir.path().join("mypkg.sty"))));
    assert!(targets.contains(&path_to_file_uri(&dir.path().join("refs.bib"))));
    assert!(targets.contains(&path_to_file_uri(&dir.path().join("fig.png"))));

    // The `\input{part}` link underlines just the `part` argument (line 0).
    let part_link = links
        .iter()
        .find(|l| l.target == Some(path_to_file_uri(&dir.path().join("part.tex"))))
        .unwrap();
    assert_eq!(part_link.range.start, Position::new(0, 7));
    assert_eq!(part_link.range.end, Position::new(0, 11));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_bib_document_links() {
    // A `.bib` file has no include structure, but its `doi`/`url` fields become
    // clickable external links (a bare DOI resolves under `https://doi.org/`).
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/refs.bib").parse().unwrap();
    let doc = "@article{k,\n\
        \x20 doi = {10.1000/xyz},\n\
        \x20 url = {https://example.com/a},\n\
        \x20 title = {Not a link},\n\
        }\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    send_request(
        &client,
        2,
        "textDocument/documentLink",
        serde_json::to_value(DocumentLinkParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = loop {
        match recv(&client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(2));
    let links: Vec<DocumentLink> = serde_json::from_value(resp.response_result.clone().unwrap())
        .expect("a documentLink response");

    let targets: Vec<String> = links
        .iter()
        .filter_map(|l| l.target.as_ref().map(|u| u.to_string()))
        .collect();
    assert_eq!(links.len(), 2, "got {targets:?}");
    assert!(targets.contains(&"https://doi.org/10.1000/xyz".to_owned()));
    assert!(targets.contains(&"https://example.com/a".to_owned()));

    // The DOI link underlines just the value (line 1, after `doi = {`).
    let doi_link = links
        .iter()
        .find(|l| {
            l.target.as_ref().map(|u| u.to_string())
                == Some("https://doi.org/10.1000/xyz".to_owned())
        })
        .unwrap();
    assert_eq!(doi_link.range.start, Position::new(1, 9));
    assert_eq!(doi_link.range.end, Position::new(1, 20));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_code_action_quickfix() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/ca.tex").parse().unwrap();

    // `\bf` is a deprecated font switch; the `deprecated-command` rule flags it and
    // carries a `\bf` → `\bfseries` explicit unsafe fix.
    let doc = "\\bf hi\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(
        diags.diagnostics.iter().any(|d| matches!(&d.message, lsp_types::Message::String(message) if message.contains("\\bf"))),
        "deprecated-command should flag \\bf, got {:?}",
        diags.diagnostics
    );

    // A range over the `\bf` control word (line 0, chars 0..3).
    let on_bf = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 3),
    };
    send_request(
        &client,
        2,
        "textDocument/codeAction",
        serde_json::to_value(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: on_bf,
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let actions: Vec<CodeActionResponse> =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a codeAction response");
    let CodeActionResponse::CodeAction(action) = actions
        .iter()
        .find(|a| matches!(a, CodeActionResponse::CodeAction(a) if a.title.contains("bfseries")))
        .expect("a `\\bf` → `\\bfseries` quick-fix")
    else {
        unreachable!()
    };
    let edits = action
        .edit
        .as_ref()
        .and_then(|e| e.changes.as_ref())
        .and_then(|c| c.get(&uri))
        .expect("a single-file edit on the document");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "\\bfseries");
    assert_eq!(edits[0].range.start, Position::new(0, 0));
    assert_eq!(edits[0].range.end, Position::new(0, 3));

    // A range that misses the command (the trailing prose) yields no actions.
    let off_bf = Range {
        start: Position::new(0, 5),
        end: Position::new(0, 5),
    };
    send_request(
        &client,
        3,
        "textDocument/codeAction",
        serde_json::to_value(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: off_bf,
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(3));
    let actions: Vec<CodeActionResponse> =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a codeAction response");
    assert!(
        actions.is_empty(),
        "a range off the command yields no quick-fix, got {actions:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_code_action_adds_a_table_column() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/table.tex").parse().unwrap();
    let doc = "\\begin{tabular}{cc}\n  a & b \\\\\n\\end{tabular}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    let params = |only| CodeActionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range: Range::new(Position::new(1, 3), Position::new(1, 3)),
        context: CodeActionContext {
            only,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    send_request(
        &client,
        2,
        "textDocument/codeAction",
        serde_json::to_value(params(Some(vec![CodeActionKind::Refactor]))).unwrap(),
    );
    let resp = recv_response(&client);
    let actions: Vec<CodeActionResponse> =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a codeAction response");
    let CodeActionResponse::CodeAction(action) = actions
        .iter()
        .find(|action| {
            matches!(action, CodeActionResponse::CodeAction(action) if action.title == "Add column at end")
        })
        .expect("the table refactoring")
    else {
        unreachable!()
    };
    assert_eq!(action.kind, Some(CodeActionKind::RefactorRewrite));
    let edits = action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .and_then(|changes| changes.get(&uri))
        .expect("single-file table edits");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].new_text, "c");
    assert_eq!(edits[1].new_text, " &");

    send_request(
        &client,
        3,
        "textDocument/codeAction",
        serde_json::to_value(params(Some(vec![CodeActionKind::QuickFix]))).unwrap(),
    );
    let resp = recv_response(&client);
    let actions: Vec<CodeActionResponse> =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a codeAction response");
    assert!(
        actions.iter().all(|action| !matches!(
            action,
            CodeActionResponse::CodeAction(action) if action.title == "Add column at end"
        )),
        "a quick-fix-only request must exclude refactorings"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_bib_diagnostics_formatting_and_symbols() {
    let (client, server_thread) = start_server(None);
    // A `.bib` URI: the server routes by extension to the BibTeX pipeline.
    let uri: Uri = fixture_uri!("/refs.bib").parse().unwrap();

    // Two entries sharing a cite key (a lint warning, not a parse error) plus
    // unformatted spacing. The duplicate is reported; formatting still works.
    let doc = "@article{k, title={A}}\n@misc{k, title={B}}\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        diags
            .diagnostics
            .iter()
            .any(|d| d.code == Some(lsp_types::Code::String("duplicate-key".to_owned()))),
        "a duplicate cite key must produce a duplicate-key diagnostic, got {:?}",
        diags.diagnostics
    );

    // textDocument/formatting → a whole-document edit equal to the bib formatter's
    // own output at the requested tab size.
    send_request(
        &client,
        2,
        "textDocument/formatting",
        serde_json::to_value(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let expected = tex_ls_analysis::bib::format_with_style(
        doc,
        FormatStyle {
            line_width: 80,
            indent_width: 2,
            ..FormatStyle::default()
        },
    )
    .unwrap();
    assert_eq!(apply_edits(doc, &edits), expected);

    // textDocument/documentSymbol → a flat list of entries (cite key + type).
    send_request(
        &client,
        3,
        "textDocument/documentSymbol",
        serde_json::to_value(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(3));
    let DocumentSymbolResponse::DocumentSymbolList(symbols) =
        serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a documentSymbol response")
    else {
        panic!("expected a nested documentSymbol response");
    };
    assert_eq!(symbols.len(), 2, "two entries with field children");
    assert!(symbols.iter().all(|s| s.name == "k"));
    assert_eq!(symbols[0].kind, SymbolKind::Object);
    assert_eq!(symbols[1].kind, SymbolKind::Constant);
    assert!(
        symbols.iter().all(
            |symbol| symbol
                .children
                .as_ref()
                .is_some_and(|children| children.len() == 1
                    && children[0].name == "title"
                    && children[0].kind == SymbolKind::Field)
        )
    );
    let details: Vec<&str> = symbols
        .iter()
        .map(|s| s.detail.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(details, vec!["article", "misc"]);

    shutdown(&client, server_thread);
}

#[test]
fn incremental_did_change_splices_buffer() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/inc.tex").parse().unwrap();

    // Open a clean doc → diagnostics clear.
    did_open(&client, &uri, 1, "\\section{Hi}\nworld\n");
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty());

    // Ranged change: replace "world" (line 1, cols 0..5) with an unclosed
    // environment. It must surface as a diagnostic — proving the splice landed in
    // the buffer the parser sees, not the original clean text.
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangePartial {
                    range: Range {
                        start: Position::new(1, 0),
                        end: Position::new(1, 5),
                    },
                    text: "\\begin{itemize}".to_owned(),
                    ..Default::default()
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(
        !diags.diagnostics.is_empty(),
        "the spliced unclosed environment must produce a diagnostic"
    );

    // Format the spliced buffer: the edit must equal formatting "\\section{Hi}\n"
    // + the spliced line. We assert the server formats the *spliced* text, not the
    // original — i.e. the new text contains the inserted command.
    send_request(
        &client,
        2,
        "textDocument/formatting",
        serde_json::to_value(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    // The buffer now has a parse error (unclosed group), so the formatter refuses:
    // a `null` result. This still proves the splice took effect (the original
    // clean buffer would have formatted).
    assert!(
        resp.response_result.clone().ok().is_none()
            || resp.response_result.clone().ok() == Some(serde_json::Value::Null),
        "formatter must refuse the now-broken spliced buffer, got {:?}",
        resp.response_result.clone().ok()
    );

    shutdown(&client, server_thread);
}

/// A `didChange` may carry several changes, each expressed against the text its
/// predecessors produced. The server has to fold them in order — and, since the
/// incremental reparse replays exactly that fold, a batch that lands wrong here is
/// the shape that would have it splice against the wrong text.
#[test]
fn incremental_did_change_folds_a_multi_change_batch() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/batch.tex").parse().unwrap();

    // Deliberately messy (a doubled inter-word space), so the formatter always has
    // an edit to return and the assertion below can never pass vacuously.
    did_open(&client, &uri, 1, "\\section{Hi}\nab  c\n");
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty());

    // Insert "XYZ" after "a", then replace "b" — the second range counts columns in
    // "aXYZb", so a server folding both against the original text would land on the
    // "Z" instead.
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangePartial {
                    range: Range {
                        start: Position::new(1, 1),
                        end: Position::new(1, 1),
                    },
                    text: "XYZ".to_owned(),
                    ..Default::default()
                }
                .into(),
                lsp_types::TextDocumentContentChangePartial {
                    range: Range {
                        start: Position::new(1, 4),
                        end: Position::new(1, 5),
                    },
                    text: "Q".to_owned(),
                    ..Default::default()
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty());

    // Formatting reads the buffer back, so its output says which text the server
    // holds — a mis-resolved second range would leave the `b` or eat the `Z`.
    send_request(
        &client,
        2,
        "textDocument/formatting",
        serde_json::to_value(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(2));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let expected = format_with_style(
        "\\section{Hi}\naXYZQ  c\n",
        FormatStyle {
            line_width: 80,
            indent_width: 2,
            ..FormatStyle::default()
        },
    )
    .unwrap();
    assert_eq!(apply_edits("\\section{Hi}\naXYZQ  c\n", &edits), expected);

    shutdown(&client, server_thread);
}

/// One ranged content change: `(start line, start column, end line, end column,
/// replacement)`, with columns in the negotiated encoding's units.
type RangedChange<'a> = (u32, u32, u32, u32, &'a str);

/// Send a `didChange` carrying `changes`, each expressed against the text its
/// predecessors produced.
fn did_change_ranged(client: &Connection, uri: &Uri, v: i32, changes: &[RangedChange<'_>]) {
    send_notification(
        client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: v,
            },
            content_changes: changes
                .iter()
                .map(|&(sl, sc, el, ec, text)| {
                    lsp_types::TextDocumentContentChangePartial {
                        range: Range {
                            start: Position::new(sl, sc),
                            end: Position::new(el, ec),
                        },
                        text: text.to_owned(),
                        ..Default::default()
                    }
                    .into()
                })
                .collect(),
        })
        .unwrap(),
    );
}

/// Ask for whole-document formatting and return its edits.
fn formatting_edits(client: &Connection, uri: &Uri, id: i32) -> Vec<TextEdit> {
    send_request(
        client,
        id,
        "textDocument/formatting",
        serde_json::to_value(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(client);
    assert_eq!(resp.id, RequestId::from(id));
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    edits
}

fn formatted(text: &str) -> String {
    format_with_style(
        text,
        FormatStyle {
            line_width: 80,
            indent_width: 2,
            ..FormatStyle::default()
        },
    )
    .unwrap()
}

/// A CRLF document, edited incrementally. The line table is patched across an
/// edit rather than rescanned, and a `\r\n` is the one terminator whose verdict an
/// edit can flip without touching either of its bytes — so a Windows-authored
/// file can expose a patch bug that an LF file does not.
///
/// The batch's second change targets a line the first one created, so it can only
/// resolve if the table shifted; and the reply's *range* is computed through the
/// patched table too, which is what makes this more than a text assertion.
#[test]
fn incremental_did_change_patches_a_crlf_document() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/crlf.tex").parse().unwrap();

    // Deliberately messy (a doubled inter-word space) so the formatter always has
    // an edit to return and the assertion can never pass vacuously.
    did_open(&client, &uri, 1, "\\section{Hi}\r\nab  c\r\n");
    assert!(recv_diagnostics(&client).diagnostics.is_empty());

    did_change_ranged(
        &client,
        &uri,
        2,
        &[
            // Split "ab  c" after "ab", adding a CRLF-terminated line.
            (1, 2, 1, 2, "\r\nX"),
            // Line 2 is "X  c", which exists only because of the change above.
            (2, 0, 2, 1, "Y"),
        ],
    );
    assert!(recv_diagnostics(&client).diagnostics.is_empty());

    let edit = formatting_edits(&client, &uri, 2);
    let expected = "\\section{Hi}\r\nab\r\nY  c\r\n";
    assert_eq!(apply_edits(expected, &edit), formatted(expected));

    shutdown(&client, server_thread);
}

/// The same, over non-ASCII content. An edit beside an astral character makes the
/// wide-line flags splice *and* the lines the edit joined be re-derived, together
/// — and the columns below are in UTF-16 units, so they only land if both did.
#[test]
fn incremental_did_change_patches_a_document_with_wide_chars() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/wide.tex").parse().unwrap();

    did_open(&client, &uri, 1, "\\section{Hi}\na𝕏b  c\n");
    assert!(recv_diagnostics(&client).diagnostics.is_empty());

    did_change_ranged(
        &client,
        &uri,
        2,
        &[
            // After `a𝕏`: one UTF-16 unit for the `a`, two for the astral char.
            (1, 3, 1, 3, "\nnew"),
            (2, 0, 2, 3, "NEW"),
        ],
    );
    assert!(recv_diagnostics(&client).diagnostics.is_empty());

    let edit = formatting_edits(&client, &uri, 2);
    assert_eq!(
        apply_edits("\\section{Hi}\na𝕏\nNEWb  c\n", &edit),
        formatted("\\section{Hi}\na𝕏\nNEWb  c\n")
    );

    shutdown(&client, server_thread);
}

#[test]
fn line_width_from_initialization_options() {
    // A narrow line width must reflow a long paragraph the default-80 width would
    // leave on one line.
    let (client, server_thread) = start_server(Some(serde_json::json!({ "lineWidth": 20 })));
    let uri: Uri = fixture_uri!("/wrap.tex").parse().unwrap();

    let para = "alpha beta gamma delta epsilon zeta eta theta\n";
    did_open(&client, &uri, 1, para);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty());

    send_request(
        &client,
        2,
        "textDocument/formatting",
        serde_json::to_value(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            // No tab_size override, so the editor settings drive the style.
            options: FormattingOptions {
                tab_size: 0,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(&client);
    let edits: Vec<TextEdit> =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    let expected = format_with_style(
        para,
        FormatStyle {
            line_width: 20,
            ..FormatStyle::default()
        },
    )
    .unwrap();
    assert_eq!(apply_edits(para, &edits), expected);
    // Sanity: the configured width actually changed the output vs. the default.
    let default_out = format_with_style(para, FormatStyle::default()).unwrap();
    assert_ne!(
        expected, default_out,
        "the test paragraph must format differently at width 20 vs 80"
    );

    shutdown(&client, server_thread);
}

#[test]
fn formatting_reloads_changed_config_without_watcher_support() {
    // Neovim advertises no dynamic watched-file registration. A config edit must
    // therefore become visible on the next request without relying on a
    // `workspace/didChangeWatchedFiles` notification.
    let dir = tempfile::tempdir().expect("temp dir");
    let config_path = dir.path().join("tex-ls.toml");
    std::fs::write(&config_path, "[format]\nline-width = 20\n").expect("write config");
    let uri = path_to_file_uri(&dir.path().join("main.tex"));
    let para = "alpha beta gamma delta epsilon zeta eta theta\n";

    let (client, server_thread) = start_server(None);
    did_open(&client, &uri, 1, para);
    assert!(recv_diagnostics(&client).diagnostics.is_empty());

    let narrow = formatting_edits(&client, &uri, 2);
    let expected_narrow = format_with_style(
        para,
        FormatStyle {
            line_width: 20,
            ..FormatStyle::default()
        },
    )
    .unwrap();
    assert_eq!(apply_edits(para, &narrow), expected_narrow);

    // Keep the byte length unchanged, matching `20` -> `40` in the reported bug,
    // and move the mtime explicitly so the test is reliable on coarse filesystems.
    std::fs::write(&config_path, "[format]\nline-width = 40\n").expect("rewrite config");
    let modified = std::fs::metadata(&config_path).unwrap().modified().unwrap();
    std::fs::File::options()
        .append(true)
        .open(&config_path)
        .unwrap()
        .set_modified(modified + Duration::from_secs(2))
        .unwrap();

    let wide = formatting_edits(&client, &uri, 3);
    let expected_wide = format_with_style(
        para,
        FormatStyle {
            line_width: 40,
            ..FormatStyle::default()
        },
    )
    .unwrap();
    assert_eq!(apply_edits(para, &wide), expected_wide);
    assert_ne!(apply_edits(para, &wide), apply_edits(para, &narrow));

    shutdown(&client, server_thread);
}

#[test]
fn did_close_clears_and_allows_reopen() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/close.tex").parse().unwrap();

    did_open(&client, &uri, 1, "\\begin{itemize}\n");
    let diags = recv_diagnostics(&client);
    assert!(!diags.diagnostics.is_empty(), "unclosed env → diagnostic");

    // Close → diagnostics cleared.
    send_notification(
        &client,
        "textDocument/didClose",
        serde_json::to_value(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "close must clear diagnostics");

    // Reopen the same URI with a clean doc → a fresh input, clean diagnostics.
    did_open(&client, &uri, 1, "\\section{Hi}\n");
    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        diags.diagnostics.is_empty(),
        "reopened clean doc must parse cleanly, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

/// Send a `textDocument/completion` at `position` and return the items.
fn complete(client: &Connection, id: i32, uri: &Uri, position: Position) -> Vec<CompletionItem> {
    complete_draining(client, id, uri, position)
}

fn labels(items: &[CompletionItem]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

/// Send `textDocument/hover` and return the rendered markdown, or `None` when the
/// server replies with `null` (nothing to describe at the position).
fn hover_markdown(client: &Connection, id: i32, uri: &Uri, position: Position) -> Option<String> {
    send_request(
        client,
        id,
        "textDocument/hover",
        serde_json::to_value(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .unwrap(),
    );
    let resp = recv_response(client);
    assert_eq!(resp.id, RequestId::from(id));
    let result = resp.response_result.clone().unwrap();
    if result.is_null() {
        return None;
    }
    let hover: Hover = serde_json::from_value(result).unwrap();
    match hover.contents {
        Contents::MarkupContent(m) => Some(m.value),
        other => panic!("expected markup hover, got {other:?}"),
    }
}

#[test]
fn lsp_hover_command_signature_and_null() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/hover.tex").parse().unwrap();

    let doc = "\\section{Intro}\n\nPlain words here.\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "{:?}", diags.diagnostics);

    // Hover on `\section` (line 0, on the command name).
    let md = hover_markdown(&client, 2, &uri, Position::new(0, 3)).expect("hover for \\section");
    assert!(md.contains("\\section"), "prototype: {md}");
    assert!(md.contains("sectioning level"), "facts: {md}");

    // Hover on plain prose resolves to nothing → `null`.
    assert!(
        hover_markdown(&client, 3, &uri, Position::new(2, 2)).is_none(),
        "prose hover should be null"
    );

    shutdown(&client, server_thread);
}

/// Send a `textDocument/signatureHelp` request and decode the reply (`None` for
/// `null`).
fn signature_help(
    client: &Connection,
    id: i32,
    uri: &Uri,
    position: Position,
) -> Option<SignatureHelp> {
    send_request(
        client,
        id,
        "textDocument/signatureHelp",
        serde_json::to_value(SignatureHelpParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            context: None,
        })
        .unwrap(),
    );
    let resp = recv_response(client);
    assert_eq!(resp.id, RequestId::from(id));
    match resp.response_result.clone().unwrap() {
        serde_json::Value::Null => None,
        value => Some(serde_json::from_value(value).expect("valid SignatureHelp")),
    }
}

#[test]
fn lsp_signature_help_active_argument_and_null() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/sighelp.tex").parse().unwrap();

    let doc = "\\frac{a}{b}\n\nPlain words here.\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Inside the second `{…}` of `\frac` (line 0, right after `b`).
    let help = signature_help(&client, 2, &uri, Position::new(0, 10)).expect("help for \\frac");
    assert_eq!(help.signatures.len(), 1);
    assert_eq!(help.signatures[0].label, "\\frac{#1}{#2}");
    assert_eq!(help.active_parameter, Some(1.into()));

    // A prose position resolves to nothing → `null`.
    assert!(
        signature_help(&client, 3, &uri, Position::new(2, 2)).is_none(),
        "prose position should be null"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_completion_in_untitled_buffers_preserves_edits_and_local_signatures() {
    let (client, server_thread) = start_server(Some(serde_json::json!({
        "texmf": {"enabled": false}
    })));
    let uri: Uri = "untitled:tex-ls-completion".parse().unwrap();
    let source = "\\newcommand{\\auditmacro}[2]{#1#2}\n𝕏 \\auditmTrailing";
    did_open(&client, &uri, 1, source);
    let _ = recv_diagnostics(&client);

    let items = complete(&client, 2, &uri, Position::new(1, 10));
    let item = items
        .into_iter()
        .find(|item| item.label == "auditmacro")
        .expect("untitled buffers complete their local declarations");
    let eager = serde_json::to_value(&item).unwrap();
    let edit: TextEdit = serde_json::from_value(eager["textEdit"].clone()).unwrap();
    assert_eq!(edit.range.start, Position::new(1, 4));
    assert_eq!(edit.range.end, Position::new(1, 18));
    assert_eq!(
        apply_edits(source, &[edit]),
        "\\newcommand{\\auditmacro}[2]{#1#2}\n𝕏 \\auditmacro"
    );

    send_request(&client, 3, "completionItem/resolve", eager.clone());
    let response = recv_response(&client);
    assert_eq!(response.id, RequestId::from(3));
    let resolved = response.response_result.unwrap();
    assert_eq!(resolved["detail"], "\\auditmacro{}{}");
    assert!(
        resolved["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("user-defined command")
    );
    assert_eq!(resolved["textEdit"], eager["textEdit"]);

    did_change_ranged(&client, &uri, 2, &[(1, 4, 1, 18, "alp")]);
    let items = complete(&client, 4, &uri, Position::new(1, 7));
    assert!(labels(&items).contains(&"alpha"));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_completion_commands_environments_and_refs() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/complete.tex").parse().unwrap();

    // A clean document so diagnostics stay empty (the env is matched).
    let doc = "\\section{Intro}\n\
        \\label{sec:intro}\n\
        \\ref{sec:i}\n\
        \\begin{itemize}\n\
        \\item x\n\
        \\end{itemize}\n\
        \\sub\n";
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(
        diags.diagnostics.is_empty(),
        "clean doc → no diagnostics, got {:?}",
        diags.diagnostics
    );

    // Command names: cursor at the end of `\sub` (line 6).
    let cmds = complete(&client, 2, &uri, Position::new(6, 4));
    let names = labels(&cmds);
    assert!(names.contains(&"subsection"), "{names:?}");
    assert!(names.contains(&"subsubsection"), "{names:?}");
    assert!(
        cmds.iter()
            .all(|i| i.kind == Some(CompletionItemKind::Function)),
        "command items are FUNCTION"
    );

    // Completing an existing environment preserves its body and end delimiter.
    let envs = complete(&client, 3, &uri, Position::new(3, 9));
    let itemize = envs
        .iter()
        .find(|i| i.label == "itemize")
        .expect("itemize env candidate");
    assert_ne!(itemize.insert_text_format, Some(InsertTextFormat::Snippet));
    assert_eq!(
        serde_json::to_value(itemize).unwrap()["textEdit"]["newText"],
        "itemize"
    );

    // `\ref{sec:i|}` (line 2) completes the defined label.
    let refs = complete(&client, 4, &uri, Position::new(2, 10));
    assert_eq!(labels(&refs), vec!["sec:intro"]);
    assert_eq!(refs[0].kind, Some(CompletionItemKind::Module));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_completion_file_paths() {
    // A real on-disk directory the document lives in, holding a `.tex`, an image,
    // and a subdirectory. The buffer text itself is in-memory.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("intro.tex"), "x").unwrap();
    std::fs::write(dir.path().join("logo.png"), "x").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    std::fs::create_dir(dir.path().join("chapters")).unwrap();

    let uri = path_to_file_uri(&dir.path().join("main.tex"));
    let (client, server_thread) = start_server(None);

    // `\input{|}` → `.tex` files and directories (not the image or the `.txt`).
    did_open(&client, &uri, 1, "\\input{}\n");
    let _ = recv_diagnostics(&client);
    let inputs = complete(&client, 2, &uri, Position::new(0, 7));
    let names = labels(&inputs);
    assert!(names.contains(&"intro.tex"), "{names:?}");
    assert!(names.contains(&"chapters"), "{names:?}");
    assert!(!names.contains(&"logo.png"), "{names:?}");
    assert!(!names.contains(&"notes.txt"), "{names:?}");
    let intro = inputs.iter().find(|i| i.label == "intro.tex").unwrap();
    assert_eq!(intro.kind, Some(CompletionItemKind::File));
    let chapters = inputs.iter().find(|i| i.label == "chapters").unwrap();
    assert_eq!(chapters.kind, Some(CompletionItemKind::Folder));

    // `\includegraphics{|}` → the image and directories (not the `.tex`).
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: "\\includegraphics{}\n".to_owned(),
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let _ = recv_diagnostics(&client);
    let graphics = complete(&client, 3, &uri, Position::new(0, 17));
    let names = labels(&graphics);
    assert!(names.contains(&"logo.png"), "{names:?}");
    assert!(names.contains(&"chapters"), "{names:?}");
    assert!(!names.contains(&"intro.tex"), "{names:?}");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_hover_label_number_from_aux_dir() {
    // A compiled project with an out-of-tree aux dir: `[build] aux-dir` routes the
    // label-number lookup, so hovering a `\ref` key shows the resolved number.
    let dir = tempfile::tempdir().unwrap();
    let main = "\\documentclass{article}\n\\begin{document}\n\\section{Intro}\n\\label{sec:a}\nSee \\ref{sec:a}.\n\\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, main).unwrap();
    std::fs::write(
        dir.path().join("tex-ls.toml"),
        "[build]\naux-dir = \"out\"\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("out")).unwrap();
    std::fs::write(
        dir.path().join("out/main.aux"),
        "\\newlabel{sec:a}{{2}{1}{Intro}{section.2}{}}\n",
    )
    .unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    // Cursor inside the `sec:a` key of `\ref` on line 4 (`See \ref{sec:a}.`).
    let md = hover_markdown(&client, 2, &uri, Position::new(4, 10)).expect("label hover");
    assert_eq!(md, "Section 2 (Intro)");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_completion_package_names() {
    // A local `.sty` sibling plus the baked name list: `\usepackage{|}` merges
    // both, deduping the local file against its baked namesake.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mylocal.sty"), "x").unwrap();
    std::fs::write(dir.path().join("amsmath.sty"), "x").unwrap();
    // Disable the installed-tree tier so the test is hermetic (a machine with TeX
    // would otherwise fold thousands of installed stems into the results). The
    // local + baked contract under test is unaffected.

    let uri = path_to_file_uri(&dir.path().join("main.tex"));
    let (client, server_thread) =
        start_server(Some(serde_json::json!({ "texmf": { "enabled": false } })));

    // `\usepackage{|}` at column 12: local `.sty` files (as MODULE names, extension
    // stripped) and baked package names, all deduped.
    did_open(&client, &uri, 1, "\\usepackage{}\n");
    let _ = recv_diagnostics(&client);
    let items = complete(&client, 2, &uri, Position::new(0, 12));
    let names = labels(&items);
    assert!(names.contains(&"mylocal"), "local .sty offered: {names:?}");
    assert!(names.contains(&"amsmath"), "baked name offered: {names:?}");
    // `amsmath` is present once (local file deduped against baked name).
    assert_eq!(
        names.iter().filter(|n| **n == "amsmath").count(),
        1,
        "amsmath deduped: {names:?}"
    );
    let amsmath = items.iter().find(|i| i.label == "amsmath").unwrap();
    assert_eq!(amsmath.kind, Some(CompletionItemKind::Module));
    // Ranking is carried by `sortText`, not the alphabetical label order.
    assert!(amsmath.sort_text.is_some(), "sortText set for ranking");

    // `\documentclass{art|}` → the baked class list (`article`), not packages.
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: "\\documentclass{art}\n".to_owned(),
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let _ = recv_diagnostics(&client);
    let classes = complete(&client, 3, &uri, Position::new(0, 18));
    let names = labels(&classes);
    assert!(names.contains(&"article"), "{names:?}");
    assert_eq!(
        names[0], "article",
        "best prefix match precedes fuzzy matches"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_completion_colors_and_tikz_libraries() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/paint.tex").parse().unwrap();

    // `\textcolor{re|}` offers the built-in color list (kind COLOR); a document
    // `\definecolor` name is merged in; `\usetikzlibrary{cal|}` offers libraries.
    let doc = "\\definecolor{brandblue}{HTML}{0055AA}\n\\textcolor{re}{x}\n\\color{bra}\n\\usetikzlibrary{cal}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // `\textcolor{re|}` at line 1, column 13.
    let colors = complete(&client, 2, &uri, Position::new(1, 13));
    let names = labels(&colors);
    assert!(names.contains(&"red"), "built-in color offered: {names:?}");
    let red = colors.iter().find(|i| i.label == "red").unwrap();
    assert_eq!(red.kind, Some(CompletionItemKind::Color));

    // A `\definecolor` name is offered at `\color{bra|}` (line 2, column 10).
    let doc_colors = complete(&client, 3, &uri, Position::new(2, 10));
    assert!(
        labels(&doc_colors).contains(&"brandblue"),
        "document color offered: {:?}",
        labels(&doc_colors)
    );

    // `\usetikzlibrary{cal|}` at line 3, column 19.
    let libs = complete(&client, 4, &uri, Position::new(3, 19));
    let names = labels(&libs);
    assert!(names.contains(&"calc"), "tikz library offered: {names:?}");
    let calc = libs.iter().find(|i| i.label == "calc").unwrap();
    assert_eq!(calc.kind, Some(CompletionItemKind::Module));

    shutdown(&client, server_thread);
}

/// The lint-rule codes carried by a diagnostics batch (parse diagnostics have no
/// code and are dropped), for asserting which cross-file rules fired.
fn rule_codes(diags: &PublishDiagnosticsParams) -> Vec<String> {
    diags
        .diagnostics
        .iter()
        .filter_map(|d| match &d.code {
            Some(Code::String(code)) => Some(code.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn lsp_cross_file_resolution_clears_diagnostics() {
    // A real on-disk project: a root that `\input`s a chapter (defining the
    // referenced label) and an `\addbibresource` bibliography (defining the cited
    // key). Only the root is opened; the server discovers the siblings on disk,
    // assembles a project, and the cross-file rules resolve — no diagnostics.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("part.tex"), "\\label{sec:intro}\n").unwrap();
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{knuth1984, title={The TeXbook}}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\addbibresource{refs.bib}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\ref{sec:intro}\n\
        \\cite{knuth1984}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);

    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        diags.diagnostics.is_empty(),
        "cross-file label + citation must resolve, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_cross_file_undefined_ref_and_citation_fire() {
    // The same shape, but the root references a label and a cite key that nothing
    // in the (now closed, rooted) project defines — so `undefined-ref` and
    // `undefined-citation` fire live in the editor.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("part.tex"), "\\label{sec:intro}\n").unwrap();
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{knuth1984, title={The TeXbook}}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\addbibresource{refs.bib}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\ref{sec:missing}\n\
        \\cite{lamport1986}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);

    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    let codes = rule_codes(&diags);
    assert!(
        codes.iter().any(|c| c == "undefined-ref"),
        "expected undefined-ref, got {codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "undefined-citation"),
        "expected undefined-citation, got {codes:?}"
    );

    shutdown(&client, server_thread);
}

/// Send a `textDocument/definition` at `position` and return the locations,
/// draining any stray diagnostics (a freshly-seeded project re-lints) first.
fn definition(client: &Connection, id: i32, uri: &Uri, position: Position) -> Vec<Location> {
    send_request(
        client,
        id,
        "textDocument/definition",
        serde_json::to_value(DefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .unwrap(),
    );
    let resp = loop {
        match recv(client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(id));
    match serde_json::from_value::<DefinitionResponse>(resp.response_result.clone().unwrap())
        .unwrap()
    {
        DefinitionResponse::Definition(lsp_types::Definition::LocationList(locs)) => locs,
        DefinitionResponse::Definition(lsp_types::Definition::Location(loc)) => vec![loc],
        DefinitionResponse::DefinitionLinkList(_) => panic!("unexpected LocationLink response"),
    }
}

#[test]
fn lsp_definition_same_file_ref_to_label() {
    let (client, server_thread) = start_server(None);
    // Build the URI from a platform-absolute path: go-to-definition re-derives the
    // reply `Location`'s URI from the db's normalized (absolutized) path, so a
    // bare `file:///def.tex` round-trips on Unix but not on Windows, where
    // `/def.tex` lacks a drive and gets the cwd drive prepended. No file is
    // created; the buffer stays in-memory.
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    // A label and a reference to it in the same buffer.
    let doc = "\\label{sec:intro}\n\\ref{sec:intro}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor inside `\ref{sec:intro}` on line 1 → jumps to the `\label` on line 0.
    let locs = definition(&client, 2, &uri, Position::new(1, 6));
    assert_eq!(locs.len(), 1, "one definition, got {locs:?}");
    assert_eq!(locs[0].uri, uri);
    assert_eq!(locs[0].range.start, Position::new(0, 7));

    // Cursor in plain prose / on nothing → no definition (empty array).
    let none = definition(&client, 3, &uri, Position::new(0, 0));
    assert!(none.is_empty(), "the `\\label` site is not a reference");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_definition_cross_file_ref_and_cite() {
    // A real on-disk project: the root `\input`s a chapter that defines the label
    // and `\addbibresource`s a `.bib` that defines the cite key. Go-to-definition
    // crosses files to both.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("part.tex"), "\\label{sec:intro}\n").unwrap();
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{knuth1984, title={The TeXbook}}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\addbibresource{refs.bib}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\ref{sec:intro}\n\
        \\cite{knuth1984}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    // `\ref{sec:intro}` (line 4) → the `\label` in part.tex (line 0).
    let ref_locs = definition(&client, 2, &uri, Position::new(4, 6));
    assert_eq!(ref_locs.len(), 1, "one label definition, got {ref_locs:?}");
    assert_eq!(
        ref_locs[0].uri,
        path_to_file_uri(&dir.path().join("part.tex"))
    );
    assert_eq!(ref_locs[0].range.start, Position::new(0, 7));

    // `\cite{knuth1984}` (line 5) → the `@article` key in refs.bib (line 0).
    let cite_locs = definition(&client, 3, &uri, Position::new(5, 8));
    assert_eq!(cite_locs.len(), 1, "one bib entry, got {cite_locs:?}");
    assert_eq!(
        cite_locs[0].uri,
        path_to_file_uri(&dir.path().join("refs.bib"))
    );
    assert_eq!(cite_locs[0].range.start.line, 0);

    shutdown(&client, server_thread);
}

#[test]
fn lsp_definition_jumps_to_include_and_package_files() {
    // Go-to-definition on a file-referencing argument (include/package) jumps to the
    // resolved file, reusing the document-link resolution. TEXMF is disabled so the
    // test stays hermetic (it asserts the local-file contract).
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("part.tex"), "\\label{a}\n").unwrap();
    std::fs::write(dir.path().join("mypkg.sty"), "% pkg\n").unwrap();

    let main_path = dir.path().join("main.tex");
    let main = "\\usepackage{mypkg}\n\\input{part}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) =
        start_server(Some(serde_json::json!({ "texmf": { "enabled": false } })));
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    // Cursor in `\input{part}` (line 1) → part.tex, whole-file target.
    let input_locs = definition(&client, 2, &uri, Position::new(1, 9));
    assert_eq!(
        input_locs.len(),
        1,
        "one include target, got {input_locs:?}"
    );
    assert_eq!(
        input_locs[0].uri,
        path_to_file_uri(&dir.path().join("part.tex"))
    );
    assert_eq!(input_locs[0].range.start, Position::new(0, 0));

    // Cursor in `\usepackage{mypkg}` (line 0) → the local mypkg.sty.
    let pkg_locs = definition(&client, 3, &uri, Position::new(0, 13));
    assert_eq!(pkg_locs.len(), 1, "one package target, got {pkg_locs:?}");
    assert_eq!(
        pkg_locs[0].uri,
        path_to_file_uri(&dir.path().join("mypkg.sty"))
    );

    shutdown(&client, server_thread);
}

/// Send a `workspace/symbol` request and return the matched symbols as
/// `(name, kind, uri)`, draining any stray diagnostics (a freshly-seeded project
/// re-lints) before the response. The server replies with the modern
/// [`WorkspaceSymbol`] shape, but its `location` is a bare `Location` on the wire,
/// so the untagged [`WorkspaceSymbolResponse`] can deserialize as either variant;
/// normalize both to the fields the assertions care about.
fn workspace_symbols(client: &Connection, id: i32, query: &str) -> Vec<(String, SymbolKind, Uri)> {
    send_request(
        client,
        id,
        "workspace/symbol",
        serde_json::to_value(WorkspaceSymbolParams {
            query: query.to_owned(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .unwrap(),
    );
    let resp = loop {
        match recv(client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(id));
    match serde_json::from_value::<WorkspaceSymbolResponse>(resp.response_result.clone().unwrap())
        .expect("a workspace/symbol response")
    {
        WorkspaceSymbolResponse::WorkspaceSymbolList(symbols) => symbols
            .into_iter()
            .map(|s| {
                let uri = match s.location {
                    WorkspaceSymbolLocation::Location(loc) => loc.uri,
                    WorkspaceSymbolLocation::LocationUriOnly(loc) => loc.uri,
                };
                (
                    s.base_symbol_information.name,
                    s.base_symbol_information.kind,
                    uri,
                )
            })
            .collect(),
        WorkspaceSymbolResponse::SymbolInformationList(symbols) => symbols
            .into_iter()
            .map(|s| {
                (
                    s.base_symbol_information.name,
                    s.base_symbol_information.kind,
                    s.location.uri,
                )
            })
            .collect(),
    }
}

#[test]
fn lsp_workspace_symbol_cross_file() {
    // A real on-disk project: the root `\input`s a chapter that defines a section
    // and a label. `workspace/symbol` aggregates the chapter's outline even though
    // only the root buffer is open (the sibling is seeded off disk).
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("part.tex"),
        "\\section{Chapter One}\n\\label{sec:intro}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    let part_uri = path_to_file_uri(&dir.path().join("part.tex"));

    // A query matching the section title finds it in the seeded sibling.
    let secs = workspace_symbols(&client, 2, "Chapter");
    assert_eq!(
        secs.len(),
        2,
        "section and its label context match, got {secs:?}"
    );
    assert_eq!(secs[0].0, "Chapter One");
    assert_eq!(secs[0].1, SymbolKind::Module);
    assert_eq!(
        secs[0].2, part_uri,
        "the section lives in the included file"
    );

    // A query matching the label finds it (also in the sibling).
    let labels = workspace_symbols(&client, 3, "sec:intro");
    assert_eq!(labels.len(), 1, "one label match, got {labels:?}");
    assert_eq!(labels[0].0, "sec:intro");
    assert_eq!(labels[0].1, SymbolKind::Constant);

    // Matching is case-insensitive.
    let lower = workspace_symbols(&client, 4, "chapter one");
    assert_eq!(
        lower.len(),
        2,
        "all-token case-insensitive context matches, got {lower:?}"
    );

    // A query matching nothing yields an empty result.
    let none = workspace_symbols(&client, 5, "zzz-no-such-symbol");
    assert!(none.is_empty(), "no matches → empty, got {none:?}");

    shutdown(&client, server_thread);
}

/// Send a `textDocument/references` at `position` and return the locations,
/// draining any stray diagnostics first (mirrors [`definition`]).
fn references(
    client: &Connection,
    id: i32,
    uri: &Uri,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    send_request(
        client,
        id,
        "textDocument/references",
        serde_json::to_value(ReferenceParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration,
            },
        })
        .unwrap(),
    );
    let resp = loop {
        match recv(client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(id));
    serde_json::from_value::<Vec<Location>>(resp.response_result.clone().unwrap()).unwrap()
}

/// Sort locations by (line, character) of their start so assertions don't depend
/// on the namespace-member iteration order.
fn sorted_starts(mut locs: Vec<Location>) -> Vec<Position> {
    locs.sort_by_key(|l| (l.range.start.line, l.range.start.character));
    locs.into_iter().map(|l| l.range.start).collect()
}

#[test]
fn lsp_references_same_file_label_uses() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    // A label and two references to it in the same buffer.
    let doc = "\\label{sec:intro}\n\\ref{sec:intro}\n\\ref{sec:intro}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor inside the first `\ref` → both `\ref` use sites, declaration excluded.
    let uses = references(&client, 2, &uri, Position::new(1, 6), false);
    assert_eq!(
        sorted_starts(uses),
        vec![Position::new(1, 5), Position::new(2, 5)],
        "both \\ref uses, no \\label"
    );

    // includeDeclaration → the `\label` site joins the two uses.
    let with_decl = references(&client, 3, &uri, Position::new(1, 6), true);
    assert_eq!(
        sorted_starts(with_decl),
        vec![
            Position::new(0, 7),
            Position::new(1, 5),
            Position::new(2, 5),
        ],
        "the \\label declaration is included"
    );

    // Invoking on the `\label` definition itself resolves the same use set.
    let from_def = references(&client, 4, &uri, Position::new(0, 8), false);
    assert_eq!(
        sorted_starts(from_def),
        vec![Position::new(1, 5), Position::new(2, 5)],
        "find-references works from the definition site"
    );

    // Cursor past the document content (the trailing newline) is on no label/ref.
    let none = references(&client, 5, &uri, Position::new(3, 0), true);
    assert!(none.is_empty(), "no reference under an empty position");

    shutdown(&client, server_thread);
}

fn document_highlight(
    client: &Connection,
    id: i32,
    uri: &Uri,
    position: Position,
) -> Vec<DocumentHighlight> {
    send_request(
        client,
        id,
        "textDocument/documentHighlight",
        serde_json::to_value(DocumentHighlightParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .unwrap(),
    );
    let resp = loop {
        match recv(client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(id));
    // An unknown document replies `null`; a resolved-but-empty highlight is `[]`.
    match resp.response_result.clone().ok() {
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(v) => serde_json::from_value(v).unwrap(),
    }
}

/// Sort highlights by their start and pair each with its kind, so assertions don't
/// depend on the label-then-ref emission order.
fn highlight_starts(
    mut hl: Vec<DocumentHighlight>,
) -> Vec<(Position, Option<DocumentHighlightKind>)> {
    hl.sort_by_key(|h| (h.range.start.line, h.range.start.character));
    hl.into_iter().map(|h| (h.range.start, h.kind)).collect()
}

#[test]
fn lsp_document_highlight_label_and_refs() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    // A label and two references to it in the same buffer.
    let doc = "\\label{sec:intro}\n\\ref{sec:intro}\n\\ref{sec:intro}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor on the first `\ref` key → the key spans of the `\label` definition
    // (WRITE) and both `\ref` uses (READ).
    let expected = vec![
        (Position::new(0, 7), Some(DocumentHighlightKind::Write)),
        (Position::new(1, 5), Some(DocumentHighlightKind::Read)),
        (Position::new(2, 5), Some(DocumentHighlightKind::Read)),
    ];
    let from_ref = document_highlight(&client, 2, &uri, Position::new(1, 6));
    assert_eq!(
        highlight_starts(from_ref),
        expected,
        "the \\label definition and both \\ref uses, by key span"
    );

    // The same set when invoked from the `\label` definition key.
    let from_def = document_highlight(&client, 3, &uri, Position::new(0, 8));
    assert_eq!(
        highlight_starts(from_def),
        expected,
        "highlight resolves identically from the definition site"
    );

    // Strict key gating: the cursor on the command word `\ref` (not its key)
    // highlights nothing.
    let on_word = document_highlight(&client, 4, &uri, Position::new(1, 1));
    assert!(on_word.is_empty(), "cursor on the command word, not a key");

    // A position past the content is on no key.
    let none = document_highlight(&client, 5, &uri, Position::new(3, 0));
    assert!(none.is_empty(), "no key under an empty position");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_highlight_cref_isolates_each_key() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    // `\cref{a,b}` is a multi-key list command; `a` and `b` are independent keys.
    let doc = "\\cref{a,b}\n\\ref{a}\n\\label{a}\n\\label{b}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor on `a` in `\cref{a,b}` → only the `a` family; the sibling `b` and
    // `\label{b}` are untouched.
    let on_a = document_highlight(&client, 2, &uri, Position::new(0, 6));
    assert_eq!(
        highlight_starts(on_a),
        vec![
            (Position::new(0, 6), Some(DocumentHighlightKind::Read)), // `\cref` key `a`
            (Position::new(1, 5), Some(DocumentHighlightKind::Read)), // `\ref{a}`
            (Position::new(2, 7), Some(DocumentHighlightKind::Write)), // `\label{a}`
        ],
        "only the `a` key family, the sibling `b` excluded"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_highlight_citation_keys() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\cite{foo}\n\\cite{foo}\n\\cite{bar}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor on a `\cite{foo}` key → both `foo` citations (READ); `bar` excluded.
    let on_foo = document_highlight(&client, 2, &uri, Position::new(0, 7));
    assert_eq!(
        highlight_starts(on_foo),
        vec![
            (Position::new(0, 6), Some(DocumentHighlightKind::Read)),
            (Position::new(1, 6), Some(DocumentHighlightKind::Read)),
        ],
        "both \\cite{{foo}} keys, the \\cite{{bar}} excluded"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_document_highlight_environment_pair() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("env.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\begin{equation}\nx\n\\end{equation}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor on the `\begin` name → both the begin and end name spans (TEXT).
    let expected = vec![
        (Position::new(0, 7), Some(DocumentHighlightKind::Text)),
        (Position::new(2, 5), Some(DocumentHighlightKind::Text)),
    ];
    let on_begin = document_highlight(&client, 2, &uri, Position::new(0, 8));
    assert_eq!(
        highlight_starts(on_begin),
        expected,
        "the paired \\begin/\\end names, from the \\begin side"
    );

    // The same pair when invoked from the `\end` name.
    let on_end = document_highlight(&client, 3, &uri, Position::new(2, 6));
    assert_eq!(
        highlight_starts(on_end),
        expected,
        "the pair resolves identically from the \\end side"
    );

    // A cursor in the body (not on a delimiter) highlights nothing.
    let in_body = document_highlight(&client, 4, &uri, Position::new(1, 0));
    assert!(in_body.is_empty(), "cursor in the body, not on a delimiter");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_references_cross_file_cite_from_tex_and_bib() {
    // A real on-disk project: the root `\input`s a chapter and `\addbibresource`s a
    // `.bib`. Both the chapter and the root cite the same key.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{knuth1984, title={The TeXbook}}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("part.tex"), "\\cite{knuth1984}\n").unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\addbibresource{refs.bib}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\cite{knuth1984}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let part_uri = path_to_file_uri(&dir.path().join("part.tex"));
    let bib_uri = path_to_file_uri(&dir.path().join("refs.bib"));

    // From the `\cite` in main (line 4) → both cite sites across the namespace.
    let uses = references(&client, 2, &main_uri, Position::new(4, 8), false);
    let mut found: Vec<(Uri, u32)> = uses
        .iter()
        .map(|l| (l.uri.clone(), l.range.start.line))
        .collect();
    found.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.cmp(&b.1)));
    assert_eq!(
        found,
        vec![(main_uri.clone(), 4), (part_uri.clone(), 0)],
        "both \\cite sites, declaration excluded, got {uses:?}"
    );

    // includeDeclaration adds the `.bib` entry.
    let with_decl = references(&client, 3, &main_uri, Position::new(4, 8), true);
    assert!(
        with_decl.iter().any(|l| l.uri == bib_uri),
        "the bib entry is included, got {with_decl:?}"
    );
    assert_eq!(
        with_decl.len(),
        3,
        "two cites + one entry, got {with_decl:?}"
    );

    // Invoking on the `@article` key in the `.bib` finds the same `\cite` uses.
    did_open(
        &client,
        &bib_uri,
        1,
        "@article{knuth1984, title={The TeXbook}}\n",
    );
    let _ = recv_diagnostics(&client);
    let from_bib = references(&client, 4, &bib_uri, Position::new(0, 12), false);
    let mut bib_found: Vec<(Uri, u32)> = from_bib
        .iter()
        .map(|l| (l.uri.clone(), l.range.start.line))
        .collect();
    bib_found.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.cmp(&b.1)));
    assert_eq!(
        bib_found,
        vec![(main_uri.clone(), 4), (part_uri.clone(), 0)],
        "cite uses resolved from the bib entry, got {from_bib:?}"
    );

    shutdown(&client, server_thread);
}

/// Send a `textDocument/prepareRename` at `position` and return the raw response
/// value (`null` when declined), draining stray diagnostics first.
fn prepare_rename(
    client: &Connection,
    id: i32,
    uri: &Uri,
    position: Position,
) -> serde_json::Value {
    send_request(
        client,
        id,
        "textDocument/prepareRename",
        serde_json::to_value(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        })
        .unwrap(),
    );
    let resp = loop {
        match recv(client) {
            Message::Response(resp) => break resp,
            Message::Notification(_) => continue,
            other => panic!("expected a response, got {other:?}"),
        }
    };
    assert_eq!(resp.id, RequestId::from(id));
    resp.response_result.clone().unwrap()
}

/// Send a `textDocument/rename` at `position` and return the resulting per-URI
/// edit map (empty when the server declined with `null`), draining stray
/// diagnostics first.
fn rename(
    client: &Connection,
    id: i32,
    uri: &Uri,
    position: Position,
    new_name: &str,
) -> std::collections::HashMap<Uri, Vec<TextEdit>> {
    let params = serde_json::to_value(RenameParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        new_name: new_name.to_owned(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    })
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let resp = loop {
        send_request(client, id, "textDocument/rename", params.clone());
        let resp = recv_response(client);
        assert_eq!(resp.id, RequestId::from(id));
        // These feature tests do not edit source during the request. Acquisition
        // and initial watcher events can still invalidate an admitted snapshot.
        if resp
            .response_result
            .as_ref()
            .is_err_and(|error| error.code == lsp_server::ErrorCode::ContentModified as i32)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "rename inputs never settled"
            );
            continue;
        }
        break resp;
    };
    match resp.response_result.clone().unwrap() {
        serde_json::Value::Null => std::collections::HashMap::new(),
        value => serde_json::from_value::<WorkspaceEdit>(value)
            .unwrap()
            .changes
            .unwrap_or_default(),
    }
}

/// Apply a single file's `TextEdit`s to `text` and return the result. Edits are
/// applied last-to-first so earlier byte offsets stay valid.
fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
    let index = tex_ls_analysis::text::LineIndex::new(text);
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            let start = index.offset_at(edit.range.start.line, edit.range.start.character);
            let end = index.offset_at(edit.range.end.line, edit.range.end.character);
            (start, end, edit.new_text.as_str())
        })
        .collect();
    spans.sort_by_key(|(start, _, _)| *start);
    let mut out = text.to_owned();
    for (start, end, new_text) in spans.into_iter().rev() {
        out.replace_range(start..end, new_text);
    }
    out
}

#[test]
fn lsp_prepare_rename_anchors_to_key_token() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\label{sec:intro}\n\\ref{sec:intro}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // Cursor inside the `\ref` key → the prepare range covers exactly `sec:intro`
    // (line 1, characters 5..14), with the key as the placeholder.
    let prepared = prepare_rename(&client, 2, &uri, Position::new(1, 6));
    let response: PrepareRenameResult = serde_json::from_value(prepared).unwrap();
    match response {
        PrepareRenameResult::PrepareRenamePlaceholder(lsp_types::PrepareRenamePlaceholder {
            range,
            placeholder,
        }) => {
            assert_eq!(range, Range::new(Position::new(1, 5), Position::new(1, 14)));
            assert_eq!(placeholder, "sec:intro");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    // Cursor on the `\ref` command word (not the key) → declined (`null`).
    let on_command = prepare_rename(&client, 3, &uri, Position::new(1, 2));
    assert!(on_command.is_null(), "the command word is not renameable");

    // Cursor on an empty line → declined.
    let on_nothing = prepare_rename(&client, 4, &uri, Position::new(2, 0));
    assert!(on_nothing.is_null(), "no key under an empty position");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_environment_option_label_supports_definition_and_rename() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("option-label.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\begin{lstlisting}[label={lst:intro}]\n\
               code\n\
               \\end{lstlisting}\n\
               \\ref{lst:intro}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    let locs = definition(&client, 2, &uri, Position::new(3, 6));
    assert_eq!(locs.len(), 1, "one option definition, got {locs:?}");
    assert_eq!(locs[0].uri, uri);
    assert_eq!(locs[0].range.start, Position::new(0, 26));
    assert_eq!(locs[0].range.end, Position::new(0, 35));

    let prepared = prepare_rename(&client, 3, &uri, Position::new(0, 28));
    let response: PrepareRenameResult = serde_json::from_value(prepared).unwrap();
    match response {
        PrepareRenameResult::PrepareRenamePlaceholder(lsp_types::PrepareRenamePlaceholder {
            range,
            placeholder,
        }) => {
            assert_eq!(
                range,
                Range::new(Position::new(0, 26), Position::new(0, 35))
            );
            assert_eq!(placeholder, "lst:intro");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    let changes = rename(&client, 4, &uri, Position::new(0, 28), "lst:overview");
    let rewritten = apply_edits(doc, &changes[&uri]);
    assert!(rewritten.contains("label={lst:overview}"), "{rewritten}");
    assert!(rewritten.contains("\\ref{lst:overview}"), "{rewritten}");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_label_rewrites_def_and_uses_cross_file() {
    // A real on-disk project: the root `\input`s a chapter that defines the label,
    // and references it from both files (one via a `\cref` list alongside a sibling
    // key that must stay untouched).
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("part.tex"),
        "\\label{sec:intro}\n\\cref{sec:intro,other}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\ref{sec:intro}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let part_uri = path_to_file_uri(&dir.path().join("part.tex"));

    // Rename from the `\ref` in main (line 3) → edits in both files.
    let changes = rename(&client, 2, &main_uri, Position::new(3, 6), "sec:overview");
    assert_eq!(changes.len(), 2, "edits span both files, got {changes:?}");

    // The chapter: the `\label` def and the matching `\cref` key are rewritten; the
    // sibling key `other` is left alone.
    let part_src = "\\label{sec:intro}\n\\cref{sec:intro,other}\n";
    let part_out = apply_edits(part_src, &changes[&part_uri]);
    assert_eq!(
        part_out, "\\label{sec:overview}\n\\cref{sec:overview,other}\n",
        "definition + list key renamed, sibling key untouched"
    );

    // The root: the `\ref` use is rewritten.
    let main_out = apply_edits(main, &changes[&main_uri]);
    assert!(
        main_out.contains("\\ref{sec:overview}"),
        "the \\ref use is renamed, got {main_out:?}"
    );

    // An invalid new name (contains a brace) is declined.
    let declined = rename(&client, 3, &main_uri, Position::new(3, 6), "bad}name");
    assert!(
        declined.is_empty(),
        "a syntactically unsafe key is declined"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_cite_key_rewrites_entry_and_uses() {
    // The root `\addbibresource`s a `.bib` and cites its key; rename rewrites the
    // `@entry` key and every `\cite` use, case-insensitively.
    let dir = tempfile::tempdir().expect("temp dir");
    let bib_src = "@article{Knuth1984, title={The TeXbook}}\n";
    std::fs::write(dir.path().join("refs.bib"), bib_src).unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\addbibresource{refs.bib}\n\
        \\begin{document}\n\
        \\cite{knuth1984}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let bib_uri = path_to_file_uri(&dir.path().join("refs.bib"));

    // Rename from the `\cite` use (line 3) → the bib entry and the cite use.
    let changes = rename(&client, 2, &main_uri, Position::new(3, 8), "knuth-texbook");
    assert_eq!(changes.len(), 2, "edits span the .tex and the .bib");

    let bib_out = apply_edits(bib_src, &changes[&bib_uri]);
    assert_eq!(
        bib_out, "@article{knuth-texbook, title={The TeXbook}}\n",
        "the @entry key is rewritten despite the case mismatch"
    );
    let main_out = apply_edits(main, &changes[&main_uri]);
    assert!(
        main_out.contains("\\cite{knuth-texbook}"),
        "the \\cite use is rewritten, got {main_out:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_lone_fragment_has_no_cross_file_diagnostics() {
    // A bare chapter opened standalone (no `\documentclass`): its namespace is
    // rootless, so `undefined-ref` stays inert even though `\ref{x}` resolves to
    // nothing — exactly the gate that keeps fragments quiet.
    let dir = tempfile::tempdir().expect("temp dir");
    let frag_path = dir.path().join("frag.tex");
    let frag = "\\section{Loose}\n\\ref{nowhere}\n";
    std::fs::write(&frag_path, frag).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&frag_path);
    did_open(&client, &uri, 1, frag);

    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        rule_codes(&diags).iter().all(|c| c != "undefined-ref"),
        "a rootless fragment must not flag undefined-ref, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

/// Completion requests tolerate interleaved notifications and retry only the
/// explicit temporary response while background project inputs settle.
fn complete_draining(
    client: &Connection,
    id: i32,
    uri: &Uri,
    position: Position,
) -> Vec<CompletionItem> {
    let params = serde_json::to_value(CompletionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: None,
    })
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let resp = loop {
        send_request(client, id, "textDocument/completion", params.clone());
        let resp = recv_response(client);
        assert_eq!(resp.id, RequestId::from(id));
        // Background acquisition can invalidate a captured read. Model the
        // client's retry of that temporary response, retaining all result checks.
        if resp
            .response_result
            .as_ref()
            .is_err_and(|error| error.code == lsp_server::ErrorCode::ContentModified as i32)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "completion never settled"
            );
            continue;
        }
        break resp;
    };
    match serde_json::from_value::<CompletionResponse>(resp.response_result.clone().unwrap())
        .unwrap()
    {
        CompletionResponse::CompletionItemList(items) => items,
        CompletionResponse::CompletionList(list) => list.items,
    }
}

#[test]
fn lsp_bib_completion_entry_types() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/refs.bib").parse().unwrap();

    // Cursor at the end of `@art` → entry-type candidates.
    did_open(&client, &uri, 1, "@art\n");
    let items = complete_draining(&client, 2, &uri, Position::new(0, 4));
    let names = labels(&items);
    assert!(names.contains(&"article"), "{names:?}");
    let article = items.iter().find(|i| i.label == "article").unwrap();
    assert_eq!(article.kind, Some(CompletionItemKind::Struct));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_bib_completion_field_names() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/fields.bib").parse().unwrap();

    // Cursor at the end of `au` (a field-name position inside an `@article`).
    did_open(&client, &uri, 1, "@article{k,\n  au\n}\n");
    let items = complete_draining(&client, 2, &uri, Position::new(1, 4));
    let names = labels(&items);
    assert!(names.contains(&"author"), "{names:?}");
    let author = items.iter().find(|i| i.label == "author").unwrap();
    assert_eq!(author.kind, Some(CompletionItemKind::Field));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_bib_completion_string_macros() {
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/strings.bib").parse().unwrap();

    // A `@string` def then a value position referencing it → macro candidates.
    let doc = "@string{els = {Elsevier}}\n@article{k, publisher = e}\n";
    did_open(&client, &uri, 1, doc);
    let items = complete_draining(&client, 2, &uri, Position::new(1, 25));
    let names = labels(&items);
    assert!(names.contains(&"els"), "{names:?}");
    let els = items.iter().find(|i| i.label == "els").unwrap();
    assert_eq!(els.kind, Some(CompletionItemKind::Constant));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_cite_completion_cross_file() {
    // A real on-disk project: the root `\addbibresource`s a `.bib` defining the key.
    // Only the root is opened; the server seeds the sibling and offers its keys.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{knuth1984, title={The TeXbook}}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\addbibresource{refs.bib}\n\\cite{kn}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);

    // Cursor inside `\cite{kn|}` → the project's entry key. The list is not
    // prefix-filtered server-side; each item carries a `filterText` of key + title +
    // authors for the client to filter on.
    let items = complete_draining(&client, 2, &uri, Position::new(1, 8));
    let names = labels(&items);
    assert!(names.contains(&"knuth1984"), "{names:?}");
    let key = items.iter().find(|i| i.label == "knuth1984").unwrap();
    assert_eq!(key.kind, Some(CompletionItemKind::Reference));
    let filter = key.filter_text.as_deref().unwrap_or_default();
    assert!(
        filter.contains("knuth1984"),
        "filterText has key: {filter:?}"
    );
    assert!(
        filter.contains("TeXbook"),
        "filterText has title: {filter:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_gls_completion_cross_file() {
    // A real on-disk project: the root defines an acronym in its preamble and
    // `\input`s a chapter that uses `\gls`. Only the chapter is opened; the
    // server seeds the sibling root and offers its keys across the namespace.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("main.tex"),
        "\\documentclass{article}\n\\newacronym{fps}{FPS}{frames per second}\n\\input{chap}\n",
    )
    .unwrap();
    let chap_path = dir.path().join("chap.tex");
    let chap = "\\gls{fp}\n";
    std::fs::write(&chap_path, chap).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&chap_path);
    did_open(&client, &uri, 1, chap);

    // Cursor inside `\gls{fp|}` → the preamble-defined key, prefix-filtered.
    let items = complete_draining(&client, 2, &uri, Position::new(0, 7));
    let names = labels(&items);
    assert!(names.contains(&"fps"), "{names:?}");
    let key = items.iter().find(|i| i.label == "fps").unwrap();
    assert_eq!(key.kind, Some(CompletionItemKind::Reference));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_gls_completion_via_loadglsentries() {
    // Entries live in a dedicated file pulled in by `\loadglsentries`; the edge
    // joins it to the document namespace, so its keys complete in the root.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("entries.tex"),
        "\\newglossaryentry{ex}{name={example},description={an example}}\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\loadglsentries{entries}\n\\gls{e}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);

    // Cursor inside `\gls{e|}`.
    let items = complete_draining(&client, 2, &uri, Position::new(1, 6));
    let names = labels(&items);
    assert!(names.contains(&"ex"), "{names:?}");

    shutdown(&client, server_thread);
}

/// Helper: send a whole-buffer `didChange` (version `v`) replacing the document text.
fn did_change_full(client: &Connection, uri: &Uri, v: i32, text: &str) {
    send_notification(
        client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: v,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangeWholeDocument {
                    text: text.to_owned(),
                }
                .into(),
            ],
        })
        .unwrap(),
    );
}

#[test]
fn lsp_pull_diagnostics_suppress_push_and_report_full() {
    let (client, server_thread) = start_server_pull();
    let uri: Uri = fixture_uri!("/pull.tex").parse().unwrap();

    // didOpen a broken document. A pull-capable client must NOT receive a push.
    did_open(&client, &uri, 1, "\\begin{itemize}\n\\item a\n");

    // Pull on demand: the report carries the parse error.
    pull_diagnostic(&client, 10, &uri, None);
    let report = recv_document_diagnostic_report(&client, 10);
    let items = report_items(&report).expect("a broken document yields a full report");
    assert!(
        !items.is_empty(),
        "an unclosed environment must produce at least one pulled diagnostic"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_pull_diagnostics_current_right_after_change() {
    // A diagnostic pull issued immediately after
    // an edit (without waiting) must reflect the CURRENT buffer, never one behind.
    let (client, server_thread) = start_server_pull();
    let uri: Uri = fixture_uri!("/pull_change.tex").parse().unwrap();

    // Open broken, then change to a valid document and pull at once.
    did_open(&client, &uri, 1, "\\begin{itemize}\n\\item a\n");
    did_change_full(&client, &uri, 2, "\\section{Hi}\n\ntext.\n");
    pull_diagnostic(&client, 11, &uri, None);

    let report = recv_document_diagnostic_report(&client, 11);
    let items = report_items(&report).expect("expected a full report");
    assert!(
        items.is_empty(),
        "the pull must reflect the fixed buffer, got {items:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_pull_diagnostics_unchanged_result_id() {
    let (client, server_thread) = start_server_pull();
    let uri: Uri = fixture_uri!("/pull_unchanged.tex").parse().unwrap();

    did_open(&client, &uri, 1, "\\section{Hi}\n\ntext.\n");

    // First pull: a full report with a result_id.
    pull_diagnostic(&client, 12, &uri, None);
    let first = recv_document_diagnostic_report(&client, 12);
    assert!(matches!(
        first,
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(_)
    ));
    let result_id = report_result_id(&first).expect("full report carries a result_id");

    // Second pull with that result_id and no edit between: an unchanged report.
    pull_diagnostic(&client, 13, &uri, Some(result_id.clone()));
    let second = recv_document_diagnostic_report(&client, 13);
    assert!(
        matches!(
            second,
            DocumentDiagnosticReport::RelatedUnchangedDocumentDiagnosticReport(_)
        ),
        "an unchanged document must report `unchanged`, got {second:?}"
    );
    assert_eq!(report_result_id(&second), Some(result_id));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_push_client_still_receives_pushes() {
    // A client that does NOT advertise pull support keeps the push model.
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/push.tex").parse().unwrap();

    did_open(&client, &uri, 1, "\\begin{itemize}\n\\item a\n");
    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        !diags.diagnostics.is_empty(),
        "push-mode client must still receive pushed diagnostics"
    );

    shutdown(&client, server_thread);
}

/// Handshake as a client that advertises dynamic file-watcher registration. After
/// `initialized` the server registers watchers via a `client/registerCapability`
/// request; capture and ack it, returning its [`RegistrationParams`].
fn start_server_watching() -> (Connection, std::thread::JoinHandle<()>, RegistrationParams) {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());

    let params = InitializeParams {
        capabilities: ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                    dynamic_registration: Some(true),
                    relative_pattern_support: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    send_request(
        &client,
        1,
        "initialize",
        serde_json::to_value(params).unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(1));
    send_notification(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );
    let reg = match recv(&client) {
        Message::Request(req) if req.method == "client/registerCapability" => {
            client
                .sender
                .send(Message::Response(Response::new_ok(
                    req.id,
                    serde_json::Value::Null,
                )))
                .unwrap();
            serde_json::from_value::<RegistrationParams>(req.params)
                .expect("valid RegistrationParams")
        }
        other => panic!("expected client/registerCapability, got {other:?}"),
    };
    (client, server_thread, reg)
}

/// Send a `workspace/didChangeWatchedFiles` notification for the given `(uri, type)`
/// events.
fn did_change_watched_files(client: &Connection, events: &[(Uri, FileChangeType)]) {
    let changes = events
        .iter()
        .map(|(uri, typ)| FileEvent::new(uri.clone(), *typ))
        .collect();
    send_notification(
        client,
        "workspace/didChangeWatchedFiles",
        serde_json::to_value(DidChangeWatchedFilesParams { changes }).unwrap(),
    );
}

/// Drain server messages until a `publishDiagnostics` for `uri` whose rule codes
/// satisfy `want`, acking any server→client request along the way. Panics on timeout.
fn recv_diagnostics_matching(
    client: &Connection,
    uri: &Uri,
    want: impl Fn(&[String]) -> bool,
) -> PublishDiagnosticsParams {
    loop {
        match recv(client) {
            Message::Notification(not) if not.method == "textDocument/publishDiagnostics" => {
                let diags: PublishDiagnosticsParams =
                    serde_json::from_value(not.params).expect("valid PublishDiagnosticsParams");
                if &diags.uri == uri && want(&rule_codes(&diags)) {
                    return diags;
                }
            }
            Message::Notification(_) => {}
            Message::Request(req) => {
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        req.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
            Message::Response(_) => {}
        }
    }
}

#[test]
fn lsp_registers_file_watchers_on_initialized() {
    // A watcher-capable client receives a `client/registerCapability` for
    // `workspace/didChangeWatchedFiles` covering both the project leaves and the config.
    let (client, server_thread, reg) = start_server_watching();
    assert_eq!(reg.registrations.len(), 1, "one registration: {reg:?}");
    let r = &reg.registrations[0];
    assert_eq!(r.method, "workspace/didChangeWatchedFiles");
    let opts = r.register_options.as_ref().expect("watcher options");
    let globs: Vec<String> = opts["watchers"]
        .as_array()
        .expect("watchers array")
        .iter()
        .map(|w| w["globPattern"].as_str().expect("string glob").to_owned())
        .collect();
    assert!(
        globs.contains(&"**/*.{tex,bib,bibtex,sty,cls,dtx,ins,def,lco,aux,log,fls}".to_owned()),
        "expected the supported-source glob, got {globs:?}"
    );
    assert!(
        globs.contains(&"**/tex-ls.toml".to_owned()),
        "expected the config glob, got {globs:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_no_watcher_registration_without_capability() {
    // The default client advertises no dynamic registration, so the server must not
    // send a `client/registerCapability`. `initialized` is processed before `didOpen`,
    // so a stray registration would arrive ahead of the diagnostics push — asserting
    // the first message is `publishDiagnostics` is sufficient.
    let (client, server_thread) = start_server(None);
    let uri: Uri = fixture_uri!("/nowatch.tex").parse().unwrap();
    did_open(&client, &uri, 1, "\\bf hi\n");
    let diags = recv_diagnostics(&client); // panics if a register request comes first
    assert_eq!(diags.uri, uri);

    shutdown(&client, server_thread);
}

#[test]
fn lsp_watched_tex_change_reanalyzes_open_doc() {
    // A root references a label defined in a non-open `\input` sibling. Editing that
    // sibling on disk (out of editor) and signalling the watcher makes the now-dangling
    // `\ref` fire `undefined-ref` in the still-open root.
    let dir = tempfile::tempdir().expect("temp dir");
    let part_path = dir.path().join("part.tex");
    std::fs::write(&part_path, "\\label{sec:intro}\n").unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\ref{sec:intro}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    // Initially the label resolves cross-file: no `undefined-ref`.
    recv_diagnostics_matching(&client, &uri, |codes| {
        !codes.iter().any(|c| c == "undefined-ref")
    });

    // Drop the label on disk, then notify the watcher.
    std::fs::write(&part_path, "% the label is gone now\n").unwrap();
    did_change_watched_files(
        &client,
        &[(path_to_file_uri(&part_path), FileChangeType::Changed)],
    );

    let diags = recv_diagnostics_matching(&client, &uri, |codes| {
        codes.iter().any(|c| c == "undefined-ref")
    });
    assert!(
        rule_codes(&diags).iter().any(|c| c == "undefined-ref"),
        "expected undefined-ref after the label was removed on disk, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_watched_bib_change_reanalyzes_open_doc() {
    // The same shape over a `.bib` resource: rewriting the bibliography on disk to drop
    // the cited key makes `undefined-citation` fire in the open root.
    let dir = tempfile::tempdir().expect("temp dir");
    let bib_path = dir.path().join("refs.bib");
    std::fs::write(&bib_path, "@article{knuth1984, title={The TeXbook}}\n").unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\addbibresource{refs.bib}\n\
        \\begin{document}\n\
        \\cite{knuth1984}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    recv_diagnostics_matching(&client, &uri, |codes| {
        !codes.iter().any(|c| c == "undefined-citation")
    });

    std::fs::write(&bib_path, "@article{lamport1986, title={LaTeX}}\n").unwrap();
    did_change_watched_files(
        &client,
        &[(path_to_file_uri(&bib_path), FileChangeType::Changed)],
    );

    let diags = recv_diagnostics_matching(&client, &uri, |codes| {
        codes.iter().any(|c| c == "undefined-citation")
    });
    assert!(
        rule_codes(&diags).iter().any(|c| c == "undefined-citation"),
        "expected undefined-citation after the key was removed on disk, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_watched_config_change_reanalyzes_open_doc() {
    // Dropping a `tex-ls.toml` beside an open doc (out of editor) and signalling the
    // watcher re-resolves settings: a rule disabled in the new config stops firing.
    let dir = tempfile::tempdir().expect("temp dir");
    let main_path = dir.path().join("main.tex");
    let main = "\\bf hi\n"; // `\bf` trips the `deprecated-command` rule
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let diags = recv_diagnostics_matching(&client, &uri, |codes| {
        codes.iter().any(|c| c == "deprecated-command")
    });
    assert!(
        rule_codes(&diags).iter().any(|c| c == "deprecated-command"),
        "expected deprecated-command before the config ignores it, got {:?}",
        diags.diagnostics
    );

    // Write a config that ignores the rule, then notify the watcher.
    let config_path = dir.path().join("tex-ls.toml");
    std::fs::write(&config_path, "[lint]\nignore = [\"deprecated-command\"]\n").unwrap();
    did_change_watched_files(
        &client,
        &[(path_to_file_uri(&config_path), FileChangeType::Created)],
    );

    let diags = recv_diagnostics_matching(&client, &uri, |codes| {
        !codes.iter().any(|c| c == "deprecated-command")
    });
    assert!(
        !rule_codes(&diags).iter().any(|c| c == "deprecated-command"),
        "deprecated-command must stop firing once the config ignores it, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_watched_config_change_reseeds_declarations() {
    // The sharper half of the test above: a config change that alters the
    // *parse* rather than the rule filter. Declaring `mycode` verbatim
    // (`AGENTS.md` decision #12) protects its body, so the finding inside it
    // disappears — but only if the config path actually re-seeds the
    // declarations input and invalidates the cached parse. A server that
    // re-resolved settings and reused its tree would still report it.
    let dir = tempfile::tempdir().expect("temp dir");
    let main_path = dir.path().join("main.tex");
    let main = "\\begin{mycode}\nWait ... what\n\\end{mycode}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let diags =
        recv_diagnostics_matching(&client, &uri, |codes| codes.iter().any(|c| c == "ellipsis"));
    assert!(
        rule_codes(&diags).iter().any(|c| c == "ellipsis"),
        "undeclared, the body is ordinary prose, got {:?}",
        diags.diagnostics
    );

    let config_path = dir.path().join("tex-ls.toml");
    std::fs::write(&config_path, "[environments.mycode]\nlike = 'lstlisting'\n").unwrap();
    did_change_watched_files(
        &client,
        &[(path_to_file_uri(&config_path), FileChangeType::Created)],
    );

    let diags = recv_diagnostics_matching(&client, &uri, |codes| {
        !codes.iter().any(|c| c == "ellipsis")
    });
    assert!(
        !rule_codes(&diags).iter().any(|c| c == "ellipsis"),
        "a declared verbatim body must protect its contents, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_declared_reference_command_drives_diagnostics_completion_and_definition() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("main.tex");
    let doc = "\\documentclass{article}\n\\begin{document}\n\\label{first}\n\\eqrefs{first}\n\\end{document}\n";
    std::fs::write(&path, doc).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&path);
    did_open(&client, &uri, 1, doc);
    let diags = recv_diagnostics(&client);
    assert!(
        rule_codes(&diags)
            .iter()
            .any(|code| code == "unreferenced-label"),
        "the undeclared command should not count as a reference, got {:?}",
        diags.diagnostics
    );

    let config_path = dir.path().join("tex-ls.toml");
    std::fs::write(&config_path, "[commands.eqrefs]\nlike = 'cref'\n").unwrap();
    did_change_watched_files(
        &client,
        &[(path_to_file_uri(&config_path), FileChangeType::Created)],
    );

    let diags = recv_diagnostics_matching(&client, &uri, |codes| {
        !codes
            .iter()
            .any(|code| matches!(code.as_str(), "unreferenced-label" | "undefined-ref"))
    });
    assert!(
        !rule_codes(&diags)
            .iter()
            .any(|code| matches!(code.as_str(), "unreferenced-label" | "undefined-ref")),
        "declared references should resolve, got {:?}",
        diags.diagnostics
    );

    let items = complete(&client, 2, &uri, Position::new(3, 11));
    assert!(labels(&items).contains(&"first"), "{items:?}");

    let locations = definition(&client, 3, &uri, Position::new(3, 10));
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert_eq!(locations[0].range.start, Position::new(2, 7));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_a_request_is_answered_under_its_own_workspace_declarations() {
    // The declarations ride a project-wide *singleton* salsa input, so a session
    // holding two workspaces overwrites it as attention crosses between them. A
    // request must therefore republish before its job runs — which is why the
    // dispatcher does it for every request rather than each handler doing it for
    // itself. `documentSymbol` is the case that pins it: it reads a tree but
    // resolves settings only for `[build]`, so nothing in its own handler would
    // have.
    let declaring = tempfile::tempdir().expect("temp dir");
    let plain = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        declaring.path().join("tex-ls.toml"),
        "[environments.mycode]\nlike = 'lstlisting'\n",
    )
    .unwrap();

    // The same bytes in both workspaces: a `\section` inside `mycode`. Declared
    // verbatim, the body is protected and the outline sees nothing in it.
    let doc = "\\begin{mycode}\n\\section{Inside}\n\\end{mycode}\n";
    let declared_path = declaring.path().join("main.tex");
    let plain_path = plain.path().join("main.tex");
    std::fs::write(&declared_path, doc).unwrap();
    std::fs::write(&plain_path, doc).unwrap();

    let (client, server_thread) = start_server(None);
    let declared_uri = path_to_file_uri(&declared_path);
    let plain_uri = path_to_file_uri(&plain_path);
    did_open(&client, &declared_uri, 1, doc);
    let _ = recv_diagnostics(&client);
    // The undeclaring workspace is opened *second*, so its (empty) block is what
    // the worker's input holds when the request below arrives.
    did_open(&client, &plain_uri, 1, doc);
    let _ = recv_diagnostics(&client);

    let sections = |id: i32, uri: &Uri| -> Vec<String> {
        send_request(
            &client,
            id,
            "textDocument/documentSymbol",
            serde_json::to_value(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        );
        let resp = recv_response(&client);
        // An empty outline comes back as a bare `[]`, which deserializes as the
        // flat variant — so both are accepted here rather than only the nested
        // one the populated case produces.
        match serde_json::from_value::<DocumentSymbolResponse>(
            resp.response_result.clone().unwrap(),
        )
        .expect("a documentSymbol response")
        {
            DocumentSymbolResponse::DocumentSymbolList(symbols) => {
                symbols.into_iter().map(|s| s.name).collect()
            }
            DocumentSymbolResponse::SymbolInformationList(symbols) => symbols
                .into_iter()
                .map(|s| s.base_symbol_information.name)
                .collect(),
        }
    };

    assert!(
        !sections(2, &declared_uri).contains(&"Inside".to_owned()),
        "the declaring workspace's own block must govern its request"
    );
    // The control, and what keeps the assertion above from passing vacuously:
    // the identical bytes under no declaration *do* surface the section.
    assert!(
        sections(3, &plain_uri).contains(&"Inside".to_owned()),
        "an undeclaring workspace must still read the body as document content"
    );
    // Back again, so the crossing is exercised in both directions rather than
    // once on the way out.
    assert!(
        !sections(4, &declared_uri).contains(&"Inside".to_owned()),
        "crossing back must republish too"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_watched_change_does_not_replace_the_open_overlay() {
    // A watcher event for a file open in the editor must not clobber the live buffer
    // with disk text: the editor overlay is authoritative. Open a clean buffer, change
    // the file on disk to something dirty, and assert no new diagnostics fire for it.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("open.tex");
    std::fs::write(&path, "\\bf on disk\n").unwrap(); // dirty on disk
    let uri = path_to_file_uri(&path);

    let (client, server_thread) = start_server(None);
    // The editor buffer is clean (no deprecated command), unlike the disk.
    did_open(&client, &uri, 1, "clean buffer\n");
    let diags = recv_diagnostics(&client);
    assert_eq!(diags.uri, uri);
    assert!(
        !rule_codes(&diags).iter().any(|c| c == "deprecated-command"),
        "the clean buffer must not report the disk's deprecated command, got {:?}",
        diags.diagnostics
    );

    // A watcher event for the open file must be ignored (no re-read of the dirty disk).
    did_change_watched_files(&client, &[(uri.clone(), FileChangeType::Changed)]);

    // The buffer is still clean. Round-trip a real edit to flush the pipeline: if the
    // watcher event had wrongly re-read disk, a deprecated-command push would be queued
    // ahead of this edit's push. We assert the next push for the buffer stays clean.
    did_change_full(&client, &uri, 2, "still clean\n");
    let diags = recv_diagnostics_matching(&client, &uri, |_| true);
    assert!(
        !rule_codes(&diags).iter().any(|c| c == "deprecated-command"),
        "an open buffer's diagnostics must track the overlay, not disk, got {:?}",
        diags.diagnostics
    );

    shutdown(&client, server_thread);
}

/// A client offering `utf-8` in `general.positionEncodings` is answered with a
/// `positionEncoding: "utf-8"` capability, and every position on the wire then
/// counts columns in bytes: symbol ranges come back byte-counted, and a ranged
/// `didChange` splice is interpreted byte-wise. ("→" is 3 UTF-8 bytes but 1
/// UTF-16 unit, so the two encodings are cleanly told apart.)
#[test]
fn lsp_negotiates_utf8_position_encoding() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());

    let params = InitializeParams {
        capabilities: ClientCapabilities {
            text_document: serde_json::from_value::<ClientCapabilities>(rich_capabilities())
                .unwrap()
                .text_document,
            general: Some(GeneralClientCapabilities {
                position_encodings: Some(vec![
                    PositionEncodingKind::UTF8,
                    PositionEncodingKind::UTF16,
                ]),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    send_request(
        &client,
        1,
        "initialize",
        serde_json::to_value(params).unwrap(),
    );
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(1));
    let init: InitializeResult =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert_eq!(
        init.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF8),
        "a client offering utf-8 must be served utf-8 positions"
    );
    send_notification(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );

    let uri: Uri = fixture_uri!("/utf8.tex").parse().unwrap();
    // Byte layout: "→" 0..3, "\section" 3..11, "{" 11, "Intro" 12..17, "}" 17.
    did_open(&client, &uri, 1, "→\\section{Intro}\n");
    let diags = recv_diagnostics(&client);
    assert!(diags.diagnostics.is_empty(), "clean doc → no diagnostics");

    let symbols = |id: i32, client: &Connection| -> Vec<DocumentSymbol> {
        send_request(
            client,
            id,
            "textDocument/documentSymbol",
            serde_json::to_value(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        );
        let resp = recv_response(client);
        assert_eq!(resp.id, RequestId::from(id));
        match serde_json::from_value(resp.response_result.clone().unwrap())
            .expect("a documentSymbol response")
        {
            DocumentSymbolResponse::DocumentSymbolList(symbols) => symbols,
            other => panic!("expected a nested documentSymbol response, got {other:?}"),
        }
    };

    // Output positions count bytes: the section starts after the 3-byte arrow
    // (a UTF-16 server would say character 1).
    let syms = symbols(2, &client);
    assert_eq!(syms.len(), 1);
    assert_eq!(syms[0].name, "Intro");
    assert_eq!(syms[0].range.start, Position::new(0, 3));

    // Input positions count bytes too: splice "Intro" (bytes 12..17) by its
    // byte-counted range. A UTF-16 server would splice inside "\section".
    send_notification(
        &client,
        "textDocument/didChange",
        serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                text_document_identifier: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                version: 2,
            },
            content_changes: vec![
                lsp_types::TextDocumentContentChangePartial {
                    range: Range {
                        start: Position::new(0, 12),
                        end: Position::new(0, 17),
                    },
                    text: "Body".to_owned(),
                    ..Default::default()
                }
                .into(),
            ],
        })
        .unwrap(),
    );
    let diags = recv_diagnostics(&client);
    assert!(
        diags.diagnostics.is_empty(),
        "the byte-spliced doc still parses cleanly, got {:?}",
        diags.diagnostics
    );

    let syms = symbols(3, &client);
    assert_eq!(syms.len(), 1);
    assert_eq!(
        syms[0].name, "Body",
        "the didChange range must be interpreted in utf-8"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_references_command_same_file() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    // A definition and two uses; `\foobar` must not match `\foo` by prefix.
    let doc = "\\newcommand{\\foo}{x}\n\\foo bar\n\\foo and \\foobar\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // From a use: both uses, the definition-site name excluded without the flag.
    let uses = references(&client, 2, &uri, Position::new(1, 2), false);
    assert_eq!(
        sorted_starts(uses),
        vec![Position::new(1, 0), Position::new(2, 0)],
        "both uses, no declaration, no prefix match"
    );
    let with_decl = references(&client, 3, &uri, Position::new(1, 2), true);
    assert_eq!(
        sorted_starts(with_decl),
        vec![
            Position::new(0, 12),
            Position::new(1, 0),
            Position::new(2, 0)
        ],
        "the `\\foo` inside `\\newcommand` is the declaration"
    );

    // Find-references resolves identically from the definition-site name.
    let from_def = references(&client, 4, &uri, Position::new(0, 14), false);
    assert_eq!(
        sorted_starts(from_def),
        vec![Position::new(1, 0), Position::new(2, 0)]
    );

    // prepareRename anchors to the name behind the backslash.
    let prepared = prepare_rename(&client, 5, &uri, Position::new(1, 2));
    let response: PrepareRenameResult = serde_json::from_value(prepared).unwrap();
    match response {
        PrepareRenameResult::PrepareRenamePlaceholder(lsp_types::PrepareRenamePlaceholder {
            range,
            placeholder,
        }) => {
            assert_eq!(range, Range::new(Position::new(1, 1), Position::new(1, 4)));
            assert_eq!(placeholder, "foo");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    shutdown(&client, server_thread);
}

#[test]
fn lsp_references_command_cross_file() {
    // The root defines `\foo` and `\input`s a chapter using it.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("part.tex"), "\\foo\n").unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\newcommand{\\foo}{x}\n\
        \\begin{document}\n\
        \\input{part}\n\
        \\foo\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let part_uri = path_to_file_uri(&dir.path().join("part.tex"));
    let uses = references(&client, 2, &main_uri, Position::new(4, 2), false);
    let mut by_file: Vec<(String, Position)> = uses
        .into_iter()
        .map(|l| (l.uri.as_str().to_owned(), l.range.start))
        .collect();
    by_file.sort();
    assert_eq!(
        by_file,
        vec![
            (main_uri.as_str().to_owned(), Position::new(4, 0)),
            (part_uri.as_str().to_owned(), Position::new(0, 0)),
        ],
        "uses in both files, declaration excluded"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_command_rewrites_def_in_local_sty() {
    // The definition lives in a local package: rename from a `.tex` use must reach
    // the `.sty` (package-load edge), and rename from the `.sty` definition must
    // reach the documents loading it (the reverse direction).
    let dir = tempfile::tempdir().expect("temp dir");
    let sty_src = "\\newcommand{\\foo}{x}\n";
    std::fs::write(dir.path().join("mystyle.sty"), sty_src).unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\usepackage{mystyle}\n\
        \\begin{document}\n\
        \\foo\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let sty_uri = path_to_file_uri(&dir.path().join("mystyle.sty"));

    // From the use in main: both files rewritten.
    let changes = rename(&client, 2, &main_uri, Position::new(3, 2), "qux");
    assert_eq!(changes.len(), 2, "edits span the .tex and the .sty");
    assert_eq!(
        apply_edits(sty_src, &changes[&sty_uri]),
        "\\newcommand{\\qux}{x}\n",
        "the definition in the package is rewritten"
    );
    assert!(apply_edits(main, &changes[&main_uri]).contains("\\qux\n"));

    // A leading backslash in the typed name is accepted too.
    let with_backslash = rename(&client, 3, &main_uri, Position::new(3, 2), "\\qux");
    assert_eq!(
        with_backslash[&sty_uri], changes[&sty_uri],
        "`\\qux` and `qux` produce identical edits"
    );

    // From the definition site inside the .sty: the reverse direction.
    did_open(&client, &sty_uri, 1, sty_src);
    let _ = recv_diagnostics(&client);
    let from_sty = rename(&client, 4, &sty_uri, Position::new(0, 14), "qux");
    assert_eq!(
        from_sty.len(),
        2,
        "rename from the package reaches the loading document"
    );
    assert!(apply_edits(main, &from_sty[&main_uri]).contains("\\qux\n"));

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_command_gated_to_user_defined() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\textbf{x}\n\\textbf{y}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    // A built-in has no project definition site: prepareRename and rename decline.
    let prepared = prepare_rename(&client, 2, &uri, Position::new(0, 3));
    assert!(prepared.is_null(), "built-in commands are not renameable");
    let changes = rename(&client, 3, &uri, Position::new(0, 3), "strong");
    assert!(changes.is_empty(), "rename declines on a built-in");

    // References stay ungated: a pure occurrence search still works.
    let uses = references(&client, 4, &uri, Position::new(0, 3), false);
    assert_eq!(
        sorted_starts(uses),
        vec![Position::new(0, 0), Position::new(1, 0)],
        "find-references works for built-ins"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_command_edit_shapes_and_validation() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    // Adjacent word, braced argument, adjacent control word, star variant.
    let doc = "\\newcommand{\\foo}{x}\n\\foo bar \\foo{y} \\foo\\bar\n\\foo*\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    let changes = rename(&client, 2, &uri, Position::new(1, 2), "qux");
    assert_eq!(
        apply_edits(doc, &changes[&uri]),
        "\\newcommand{\\qux}{x}\n\\qux bar \\qux{y} \\qux\\bar\n\\qux*\n",
        "every token boundary survives: word, brace, control word, star"
    );

    // A digit would end the control word early at every use site; `@` outside a
    // letter-mode name would mis-lex. Both decline.
    assert!(
        rename(&client, 3, &uri, Position::new(1, 2), "my2cmd").is_empty(),
        "digits are not control-word letters"
    );
    assert!(
        rename(&client, 4, &uri, Position::new(1, 2), "a@b").is_empty(),
        "a plain name must not gain `@`"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_command_at_names_in_sty() {
    // In a `.sty`, `@` is a letter, so `\my@helper` is one token; the new name may
    // keep `@` because the old one proves every site is in `\makeatletter` scope.
    let (client, server_thread) = start_server(None);
    let dir = tempfile::tempdir().expect("temp dir");
    let sty_path = dir.path().join("mystyle.sty");
    let sty_src = "\\newcommand{\\my@helper}{x}\n\\my@helper\n";
    std::fs::write(&sty_path, sty_src).unwrap();
    let sty_uri = path_to_file_uri(&sty_path);
    did_open(&client, &sty_uri, 1, sty_src);
    let _ = recv_diagnostics(&client);

    let changes = rename(&client, 2, &sty_uri, Position::new(1, 3), "my@other");
    assert_eq!(
        apply_edits(sty_src, &changes[&sty_uri]),
        "\\newcommand{\\my@other}{x}\n\\my@other\n",
        "`@` is allowed when the old name already used it"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_rename_command_skips_comments_and_verbatim() {
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\newcommand{\\foo}{x}\n\
        % \\foo in a comment\n\
        \\begin{verbatim}\n\
        \\foo\n\
        \\end{verbatim}\n\
        \\foo\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    let changes = rename(&client, 2, &uri, Position::new(5, 2), "qux");
    let out = apply_edits(doc, &changes[&uri]);
    assert!(
        out.contains("% \\foo in a comment") && out.contains("\\begin{verbatim}\n\\foo\n"),
        "comment and verbatim occurrences are untouched, got {out:?}"
    );
    assert!(
        out.starts_with("\\newcommand{\\qux}{x}") && out.ends_with("\\qux\n"),
        "the real definition and use are rewritten, got {out:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_references_and_rename_environment_cross_file() {
    // `\newenvironment` in the root; balanced pairs in both files plus an
    // unbalanced `\begin` in the chapter (name-based, not pair-based).
    let dir = tempfile::tempdir().expect("temp dir");
    let part_src = "\\begin{myenv}\ny\n\\end{myenv}\n\\begin{myenv}\n";
    std::fs::write(dir.path().join("part.tex"), part_src).unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\newenvironment{myenv}{a}{b}\n\
        \\begin{document}\n\
        \\begin{myenv}\n\
        x\n\
        \\end{myenv}\n\
        \\input{part}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let part_uri = path_to_file_uri(&dir.path().join("part.tex"));

    // References from the name in main's `\begin{myenv}`: every delimiter name in
    // both files; the `\newenvironment{myenv}` name only with the declaration flag.
    let uses = references(&client, 2, &main_uri, Position::new(3, 8), false);
    assert_eq!(uses.len(), 5, "2 in main + 3 in part, got {uses:?}");
    let with_decl = references(&client, 3, &main_uri, Position::new(3, 8), true);
    assert_eq!(with_decl.len(), 6, "plus the definition name");
    assert!(
        with_decl
            .iter()
            .any(|l| l.uri == main_uri && l.range.start == Position::new(1, 16)),
        "the declaration is the `\\newenvironment` name argument"
    );

    // Rename rewrites every delimiter and the definition name.
    let changes = rename(&client, 4, &main_uri, Position::new(3, 8), "blockx");
    let main_out = apply_edits(main, &changes[&main_uri]);
    assert!(
        main_out.contains("\\newenvironment{blockx}")
            && main_out.contains("\\begin{blockx}\nx\n\\end{blockx}"),
        "definition + main pair renamed, got {main_out:?}"
    );
    assert_eq!(
        apply_edits(part_src, &changes[&part_uri]),
        "\\begin{blockx}\ny\n\\end{blockx}\n\\begin{blockx}\n",
        "the unbalanced trailing \\begin is renamed too"
    );

    // A cursor in the body (not on a delimiter or name) declines.
    let in_body = prepare_rename(&client, 5, &main_uri, Position::new(4, 0));
    assert!(in_body.is_null(), "the body is not a name");

    // A built-in environment declines rename (no project definition).
    let builtin = rename(&client, 6, &main_uri, Position::new(2, 9), "doc");
    assert!(builtin.is_empty(), "`document` is not user-defined");

    shutdown(&client, server_thread);
}

#[test]
fn lsp_references_cref_command_word_targets_the_command() {
    // A control-word cursor names the command, never its argument keys.
    let (client, server_thread) = start_server(None);
    let abs = std::path::absolute("def.tex").expect("absolute path");
    let uri = path_to_file_uri(&abs);
    let doc = "\\label{a}\n\\cref{a}\n\\cref{a}\n";
    did_open(&client, &uri, 1, doc);
    let _ = recv_diagnostics(&client);

    let with_decl = references(&client, 2, &uri, Position::new(1, 2), true);
    assert_eq!(
        sorted_starts(with_decl),
        vec![Position::new(1, 0), Position::new(2, 0)],
        "only command occurrences are included"
    );

    shutdown(&client, server_thread);
}

#[test]
fn lsp_definition_user_command_and_environment() {
    // Goto-definition jumps from a use to the definition-site name, across the
    // package-load edge into a local `.sty`.
    let dir = tempfile::tempdir().expect("temp dir");
    let sty_src = "\\newcommand{\\foo}{x}\n";
    std::fs::write(dir.path().join("mystyle.sty"), sty_src).unwrap();
    let main_path = dir.path().join("main.tex");
    let main = "\\documentclass{article}\n\
        \\usepackage{mystyle}\n\
        \\newenvironment{myenv}{a}{b}\n\
        \\begin{document}\n\
        \\foo\n\
        \\begin{myenv}\n\
        \\textbf{x}\n\
        \\end{myenv}\n\
        \\end{document}\n";
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let main_uri = path_to_file_uri(&main_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);

    let sty_uri = path_to_file_uri(&dir.path().join("mystyle.sty"));

    // Command use → the `\foo` name inside the `.sty` definition.
    let defs = definition(&client, 2, &main_uri, Position::new(4, 2));
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].uri, sty_uri);
    assert_eq!(
        defs[0].range,
        Range::new(Position::new(0, 12), Position::new(0, 16))
    );

    // Environment name → the `\newenvironment{myenv}` name argument.
    let env_defs = definition(&client, 3, &main_uri, Position::new(5, 9));
    assert_eq!(env_defs.len(), 1);
    assert_eq!(env_defs[0].uri, main_uri);
    assert_eq!(
        env_defs[0].range,
        Range::new(Position::new(2, 16), Position::new(2, 21))
    );

    // A built-in with no project definition yields nothing.
    let builtin = definition(&client, 4, &main_uri, Position::new(6, 3));
    assert!(builtin.is_empty(), "`\\textbf` has no definition site");

    shutdown(&client, server_thread);
}

// ---------------------------------------------------------------------------
// Forward search (`tex-ls.forwardSearch`).
//
// The "viewer" throughout is `tex-ls debug echo-args`, which records its argv
// to a file: the one program guaranteed to exist wherever CI runs, and it shows
// exactly what `%f`/`%p`/`%l` expanded to.
// ---------------------------------------------------------------------------

/// `initializationOptions` configuring `tex-ls debug echo-args --out <record>`
/// as the PDF viewer, with `args` appended after the recorder's own flags.
fn viewer_options(record: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let mut argv = vec![
        "debug".to_owned(),
        "echo-args".to_owned(),
        "--out".to_owned(),
        record.display().to_string(),
    ];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    serde_json::json!({
        "forwardSearch": {
            "executable": env!("CARGO_BIN_EXE_tex-ls"),
            "args": argv,
        }
    })
}

fn forward_search(client: &Connection, id: i32, uri: &Uri, position: Position) -> String {
    send_request(
        client,
        id,
        "workspace/executeCommand",
        serde_json::json!({"command":"tex-ls.forwardSearch","arguments":[{"uri":uri,"position":position}]}),
    );
    let resp = loop {
        match recv(client) {
            Message::Response(resp) if resp.id == RequestId::from(id) => break resp,
            // Diagnostics for the `didOpen` may still be in flight.
            Message::Notification(_) => continue,
            other => panic!("expected the forwardSearch response, got {other:?}"),
        }
    };
    let result = resp
        .response_result
        .clone()
        .expect("forwardSearch never fails the request");
    result["outcome"]
        .as_str()
        .expect("a structured outcome")
        .to_owned()
}

/// The viewer runs in its own process, so the recording lands asynchronously.
/// Poll on the same ~5 s bound [`recv`] uses.
fn recorded_args(record: &std::path::Path) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(record) {
            return text.lines().map(str::to_owned).collect();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the viewer never wrote {}",
            record.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn forward_search_without_settings_is_unconfigured() {
    // No viewer configured: status 3, and nothing is spawned.
    let dir = tempfile::tempdir().unwrap();
    let main = "\\documentclass{article}\n\\begin{document}\nhi\n\\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server(None);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    assert_eq!(
        forward_search(&client, 2, &uri, Position::new(2, 0)),
        "unconfigured"
    );

    shutdown(&client, server_thread);
}

#[test]
fn forward_search_without_a_pdf_reports_failure() {
    // A configured viewer but an uncompiled project: status 2, so the client can
    // say "build it first" instead of the viewer opening onto nothing.
    let dir = tempfile::tempdir().unwrap();
    let main = "\\documentclass{article}\n\\begin{document}\nhi\n\\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, main).unwrap();
    let record = dir.path().join("viewer-args.txt");

    let (client, server_thread) = start_server(Some(viewer_options(&record, &["%p"])));
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    assert_eq!(
        forward_search(&client, 2, &uri, Position::new(2, 0)),
        "missingPdf"
    );
    assert!(
        !record.exists(),
        "no viewer may be launched when the PDF is missing"
    );

    shutdown(&client, server_thread);
}

#[test]
fn forward_search_invokes_the_viewer_with_substituted_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let main = "\\documentclass{article}\n\\begin{document}\nhi\n\\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, main).unwrap();
    std::fs::write(dir.path().join("main.pdf"), "%PDF-1.7\n").unwrap();
    let record = dir.path().join("viewer-args.txt");

    let (client, server_thread) =
        start_server(Some(viewer_options(&record, &["%p", "%f", "%l", "%%f"])));
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    // Line 2 (0-based) is `hi`; SyncTeX counts from 1, so `%l` must be `3`.
    assert_eq!(
        forward_search(&client, 2, &uri, Position::new(2, 0)),
        "launched"
    );
    assert_eq!(
        recorded_args(&record),
        vec![
            dir.path().join("main.pdf").display().to_string(),
            main_path.display().to_string(),
            "3".to_owned(),
            "%f".to_owned(),
        ]
    );

    shutdown(&client, server_thread);
}

#[test]
fn forward_search_targets_the_root_documents_pdf_from_a_child() {
    // `%f` is the *cursor's* file (SyncTeX indexes per input file) while `%p` is
    // the *root's* PDF — a `\input`ed child has no PDF of its own.
    let dir = tempfile::tempdir().unwrap();
    let main = "\\documentclass{article}\n\\begin{document}\n\\input{chapter}\n\\end{document}\n";
    let chapter = "Chapter text.\n";
    let main_path = dir.path().join("main.tex");
    let chapter_path = dir.path().join("chapter.tex");
    std::fs::write(&main_path, main).unwrap();
    std::fs::write(&chapter_path, chapter).unwrap();
    std::fs::write(dir.path().join("main.pdf"), "%PDF-1.7\n").unwrap();
    let record = dir.path().join("viewer-args.txt");

    let (client, server_thread) = start_server(Some(viewer_options(&record, &["%p", "%f"])));
    let main_uri = path_to_file_uri(&main_path);
    let chapter_uri = path_to_file_uri(&chapter_path);
    did_open(&client, &main_uri, 1, main);
    let _ = recv_diagnostics(&client);
    did_open(&client, &chapter_uri, 1, chapter);
    let _ = recv_diagnostics(&client);

    assert_eq!(
        forward_search(&client, 2, &chapter_uri, Position::new(0, 0)),
        "launched"
    );
    assert_eq!(
        recorded_args(&record),
        vec![
            dir.path().join("main.pdf").display().to_string(),
            chapter_path.display().to_string(),
        ],
        "%p must be the root's PDF while %f stays the cursor's file"
    );

    shutdown(&client, server_thread);
}

#[test]
fn forward_search_honors_build_pdf_dir_and_pdf_filename() {
    let dir = tempfile::tempdir().unwrap();
    let main = "\\documentclass{article}\n\\begin{document}\nhi\n\\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, main).unwrap();
    std::fs::write(
        dir.path().join("tex-ls.toml"),
        "[build]\npdf-dir = \"out\"\npdf-filename = \"thesis\"\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("out")).unwrap();
    std::fs::write(dir.path().join("out/thesis.pdf"), "%PDF-1.7\n").unwrap();
    let record = dir.path().join("viewer-args.txt");

    let (client, server_thread) = start_server(Some(viewer_options(&record, &["%p"])));
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    let _ = recv_diagnostics(&client);

    assert_eq!(
        forward_search(&client, 2, &uri, Position::new(2, 0)),
        "launched"
    );
    assert_eq!(
        recorded_args(&record),
        vec![
            dir.path()
                .join("out")
                .join("thesis.pdf")
                .display()
                .to_string()
        ],
        "`pdf-filename` without an extension must still resolve to a .pdf"
    );

    shutdown(&client, server_thread);
}

#[test]
fn forward_search_honors_build_root_for_an_unseeded_parent() {
    // The classic layout: the root lives one directory above the chapter being
    // edited. The server seeds directories lazily, so opening only
    // `chapters/ch1.tex` never loads `../main.tex` and the include-graph scan
    // finds no root at all. `[build] root` is what makes this work.
    let dir = tempfile::tempdir().unwrap();
    let main =
        "\\documentclass{article}\n\\begin{document}\n\\input{chapters/ch1}\n\\end{document}\n";
    std::fs::write(dir.path().join("main.tex"), main).unwrap();
    std::fs::write(dir.path().join("main.pdf"), "%PDF-1.7\n").unwrap();
    std::fs::write(
        dir.path().join("tex-ls.toml"),
        "[build]\nroot = \"main.tex\"\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("chapters")).unwrap();
    let chapter = "Chapter text.\n";
    let chapter_path = dir.path().join("chapters").join("ch1.tex");
    std::fs::write(&chapter_path, chapter).unwrap();
    let record = dir.path().join("viewer-args.txt");

    let (client, server_thread) = start_server(Some(viewer_options(&record, &["%p", "%f"])));
    let uri = path_to_file_uri(&chapter_path);
    did_open(&client, &uri, 1, chapter);
    let _ = recv_diagnostics(&client);

    assert_eq!(
        forward_search(&client, 2, &uri, Position::new(0, 0)),
        "launched"
    );
    // Compare *files*, not path spellings. This is the one branch where the root
    // comes from `[build] root`, which is absolutized against the config's own
    // directory — and `tex_ls::config::ConfigLoader::discover` reaches that directory by canonicalizing.
    // So on macOS `%p` comes back under `/private/var` while `%f`, taken straight
    // off the LSP URI, stays under `/var`. Which of the two spellings a viewer
    // receives is not what this test pins.
    let recorded: Vec<std::path::PathBuf> = recorded_args(&record)
        .iter()
        .map(|arg| {
            std::path::Path::new(arg)
                .canonicalize()
                .unwrap_or_else(|err| panic!("recorded viewer path `{arg}`: {err}"))
        })
        .collect();
    assert_eq!(
        recorded,
        vec![
            dir.path().join("main.pdf").canonicalize().unwrap(),
            chapter_path.canonicalize().unwrap(),
        ]
    );

    shutdown(&client, server_thread);
}

// ---------------------------------------------------------------------------
// Inverse search (viewer -> IPC -> `window/showDocument`).
// ---------------------------------------------------------------------------

/// Spawn an in-process server that advertises `window/showDocument` support and
/// points its inverse-search IPC at `ipc_dir`, so the test can play the viewer.
fn start_server_show_document(
    ipc_dir: &std::path::Path,
    roots: &[&std::path::Path],
) -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());

    // `window.showDocument` and `workspaceFolders` are both read straight off the
    // raw `initialize` JSON by the server, so build the params as JSON rather
    // than round-tripping a partly-defaulted `InitializeParams`.
    let params = serde_json::json!({
        "capabilities": { "window": { "showDocument": { "support": true } } },
        "workspaceFolders": roots
            .iter()
            .map(|root| serde_json::json!({
                "uri": path_to_file_uri(root).as_str(),
                "name": "test",
            }))
            .collect::<Vec<_>>(),
        "initializationOptions": {
            "forwardSearch": { "ipcDir": ipc_dir.display().to_string() }
        },
    });
    send_request(&client, 1, "initialize", params);
    let resp = recv_response(&client);
    assert_eq!(resp.id, RequestId::from(1));
    let init: InitializeResult =
        serde_json::from_value(resp.response_result.clone().unwrap()).unwrap();
    assert_server_capabilities(&init);
    send_notification(
        &client,
        "initialized",
        serde_json::to_value(InitializedParams {}).unwrap(),
    );
    (client, server_thread)
}

/// Block until the server has published its inverse-search advertisement.
///
/// The `initialize` response is sent before `main_loop` runs, so a test that
/// dials immediately can beat the server to its own socket. A real viewer runs
/// minutes later; this just restores that ordering.
fn wait_for_ipc_advertisement(ipc_dir: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let published = std::fs::read_dir(ipc_dir).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        });
        if published {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server never published an inverse-search advertisement in {}",
            ipc_dir.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait for the server's `window/showDocument`, acking anything else on the way.
fn recv_show_document(client: &Connection) -> lsp_types::ShowDocumentParams {
    loop {
        match recv(client) {
            Message::Request(req) if req.method == "window/showDocument" => {
                let params = serde_json::from_value(req.params).expect("ShowDocumentParams");
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        req.id,
                        serde_json::json!({ "success": true }),
                    )))
                    .unwrap();
                return params;
            }
            Message::Notification(_) => continue,
            // Ack any other server-initiated request (e.g. watcher registration).
            Message::Request(req) => {
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        req.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
            other => panic!("expected window/showDocument, got {other:?}"),
        }
    }
}

#[test]
fn inverse_search_reveals_the_position_via_show_document() {
    let dir = tempfile::tempdir().unwrap();
    let ipc_dir = dir.path().join("ipc");
    let main = "\\documentclass{article}\n\\begin{document}\nhi\n\\end{document}\n";
    let main_path = dir.path().join("main.tex");
    std::fs::write(&main_path, main).unwrap();

    let (client, server_thread) = start_server_show_document(&ipc_dir, &[dir.path()]);
    let uri = path_to_file_uri(&main_path);
    did_open(&client, &uri, 1, main);
    wait_for_ipc_advertisement(&ipc_dir);

    // Play the viewer. The client blocks on the server's ack, so it has to run
    // off-thread: this test is both ends of the IPC at once.
    let viewer = {
        let (ipc_dir, main_path) = (ipc_dir.clone(), main_path.clone());
        std::thread::spawn(move || tex_ls::ipc::send_inverse_search_in(&ipc_dir, &main_path, 3, 0))
    };

    let params = recv_show_document(&client);
    assert_eq!(params.uri, uri);
    assert_eq!(params.take_focus, Some(true));
    assert_eq!(params.external, Some(false));
    // The wire is 1-based, LSP is 0-based, and the selection is collapsed.
    assert_eq!(
        params.selection,
        Some(Range {
            start: Position::new(2, 0),
            end: Position::new(2, 0),
        })
    );
    viewer.join().unwrap().expect("the server accepted");

    shutdown(&client, server_thread);
}

#[test]
fn inverse_search_declines_a_file_outside_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let ipc_dir = dir.path().join("ipc");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    let (client, server_thread) = start_server_show_document(&ipc_dir, &[&workspace]);
    wait_for_ipc_advertisement(&ipc_dir);

    // A file under neither the workspace root nor any open buffer: the server must
    // decline, so the viewer-side command can report that nobody owns it rather
    // than have this server answer for a project it knows nothing about.
    let stranger = dir.path().join("elsewhere.tex");
    let err = tex_ls::ipc::send_inverse_search_in(&ipc_dir, &stranger, 1, 0)
        .expect_err("no server owns this file");
    assert!(
        matches!(err, tex_ls::ipc::IpcError::NoServerForFile(_)),
        "{err:?}"
    );

    shutdown(&client, server_thread);
}

#[test]
fn a_client_without_show_document_support_never_advertises() {
    // Inverse search ends in `window/showDocument`; a server that cannot finish
    // the job must not claim the request from one that can.
    let dir = tempfile::tempdir().unwrap();
    let ipc_dir = dir.path().join("ipc");
    let (client, server_thread) = start_server(Some(serde_json::json!({
        "forwardSearch": { "ipcDir": ipc_dir.display().to_string() }
    })));

    let err = tex_ls::ipc::send_inverse_search_in(&ipc_dir, &dir.path().join("main.tex"), 1, 0)
        .expect_err("nothing should be listening");
    assert!(matches!(err, tex_ls::ipc::IpcError::NoServer), "{err:?}");

    shutdown(&client, server_thread);
}

fn start_with_capabilities(
    caps: serde_json::Value,
) -> (Connection, std::thread::JoinHandle<()>, serde_json::Value) {
    let (server, client) = Connection::memory();
    let thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
    send_request(
        &client,
        1,
        "initialize",
        serde_json::json!({"capabilities":caps}),
    );
    let result = recv_response(&client).response_result.unwrap();
    send_notification(&client, "initialized", serde_json::json!({}));
    (client, thread, result)
}

#[test]
fn minimal_capabilities_get_plain_hover_flat_symbols_and_no_actions() {
    let (client, thread, initialized) = start_with_capabilities(serde_json::json!({}));
    assert!(
        initialized["capabilities"]
            .get("codeActionProvider")
            .is_none()
    );
    assert_eq!(initialized["serverInfo"]["name"], "tex-ls");
    let uri: Uri = fixture_uri!("/policy-minimal.tex").parse().unwrap();
    did_open(&client, &uri, 1, "\\section{Intro}\n");
    let _ = recv_diagnostics(&client);
    send_request(
        &client,
        2,
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument":{"uri":uri}}),
    );
    let symbols = recv_response(&client).response_result.unwrap();
    assert_eq!(symbols[0]["location"]["uri"], uri.as_str());
    assert!(symbols[0].get("selectionRange").is_none());
    send_request(
        &client,
        3,
        "textDocument/hover",
        serde_json::json!({"textDocument":{"uri":uri},"position":{"line":0,"character":3}}),
    );
    let hover = recv_response(&client).response_result.unwrap();
    assert_eq!(hover["contents"]["kind"], "plaintext");
    assert!(!hover["contents"]["value"].as_str().unwrap().contains("```"));
    shutdown(&client, thread);
}

#[test]
fn configuration_pull_is_negotiated_and_rejects_invalid_replacements() {
    let (client, thread, _) =
        start_with_capabilities(serde_json::json!({"workspace":{"configuration":true}}));
    let Message::Request(initial) = recv(&client) else {
        panic!("configuration request");
    };
    assert_eq!(initial.method, "workspace/configuration");
    assert_eq!(
        initial.params,
        serde_json::json!({"items":[{"section":"tex-ls"}]})
    );
    client
        .sender
        .send(Message::Response(Response::new_ok(
            initial.id,
            serde_json::json!([{"lineWidth":20}]),
        )))
        .unwrap();
    // An ordered request is a barrier after the configuration response.
    send_request(
        &client,
        2,
        "workspace/symbol",
        serde_json::json!({"query":""}),
    );
    assert!(recv_response(&client).response_result.is_ok());
    send_notification(
        &client,
        "workspace/didChangeConfiguration",
        serde_json::json!({"settings":null}),
    );
    let Message::Request(replacement) = recv(&client) else {
        panic!("replacement configuration");
    };
    assert_ne!(replacement.id, RequestId::from(2));
    client
        .sender
        .send(Message::Response(Response::new_ok(
            replacement.id,
            serde_json::json!([{"lineWidth":0}]),
        )))
        .unwrap();
    let Message::Notification(error) = recv(&client) else {
        panic!("configuration error");
    };
    assert_eq!(error.method, "window/showMessage");
    shutdown(&client, thread);
}

#[test]
fn linked_editing_requires_an_identical_literal_pair() {
    let (client, thread) = start_server(None);
    let uri: Uri = fixture_uri!("/linked.tex").parse().unwrap();
    for (version, source, expected) in [
        (1, "\\begin{center}x\\end{center}", true),
        (2, "\\begin{center}x\\end{flushleft}", false),
        (3, "\\begin{center}x", false),
    ] {
        if version == 1 {
            did_open(&client, &uri, version, source);
        } else {
            send_notification(
                &client,
                "textDocument/didChange",
                serde_json::json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":source}]}),
            );
        }
        let _ = recv_diagnostics(&client);
        send_request(
            &client,
            20 + version,
            "textDocument/linkedEditingRange",
            serde_json::json!({"textDocument":{"uri":uri},"position":{"line":0,"character":9}}),
        );
        let result = recv_response(&client).response_result.unwrap();
        assert_eq!(!result.is_null(), expected);
        if expected {
            assert_eq!(result["ranges"].as_array().unwrap().len(), 2);
        }
    }
    shutdown(&client, thread);
}

#[test]
fn external_compiler_artifacts_are_authoritative_and_watched_with_either_owner() {
    for client_watching in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let build = dir.path().join("build");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(
            source.join("tex-ls.toml"),
            "[build]\naux-dir=\"../build\"\n",
        )
        .unwrap();
        std::fs::write(source.join("main.log"), "! Stale sibling.\nl.1 Text\n").unwrap();
        let uri = path_to_file_uri(&source.join("main.tex"));
        let log = build.join("main.log");
        let (client, server) = if client_watching {
            let (client, server, _) = start_server_watching();
            (client, server)
        } else {
            start_server(Some(serde_json::json!({"texmf":{"enabled":false}})))
        };
        did_open(&client, &uri, 1, "Text\n");
        let wait = |expected: Option<&str>| loop {
            let report = recv_diagnostics(&client);
            if report.uri != uri {
                continue;
            }
            let messages: Vec<_> = report
                .diagnostics
                .iter()
                .filter(|item| item.source.as_deref() == Some("compiler"))
                .map(|item| serde_json::to_value(&item.message).unwrap())
                .collect();
            assert!(
                !messages
                    .iter()
                    .any(|message| message.as_str().unwrap().contains("Stale sibling"))
            );
            if match expected {
                Some(expected) => messages
                    .iter()
                    .any(|message| message.as_str().unwrap().starts_with(expected)),
                None => messages.is_empty(),
            } {
                break;
            }
        };
        // A missing configured artifact must not fall back to an old sibling.
        wait(None);
        std::fs::write(&log, "! First build.\nl.1 Text\n").unwrap();
        wait(Some("First build."));
        std::fs::write(&log, "! Replacement build.\nl.1 Text\n").unwrap();
        wait(Some("Replacement build."));
        std::fs::remove_file(&log).unwrap();
        wait(None);
        shutdown(&client, server);
    }
}

#[test]
fn ignored_compiler_log_refreshes_push_diagnostics_without_client_events() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
    std::fs::write(
        dir.path().join("tex-ls.toml"),
        "extend-exclude = [\"build\"]\n[build]\naux-dir = \"build\"\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("build")).unwrap();
    let path = dir.path().join("main.tex");
    let uri = path_to_file_uri(&path);
    let log = dir.path().join("build/main.log");
    std::fs::write(&log, "! Initial failure.\nl.1 Text\n").unwrap();
    let (client, server) = start_server(Some(serde_json::json!({
        "texmf":{"enabled":false}
    })));
    did_open(&client, &uri, 1, "Text\n");
    let wait_for_compiler = |expected: Option<&str>| {
        loop {
            let report = recv_diagnostics(&client);
            let messages: Vec<_> = report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.source.as_deref() == Some("compiler"))
                .collect();
            if report.uri == uri
                && match expected {
                    Some(expected) => messages
                        .iter()
                        .any(|diagnostic| matches!(&diagnostic.message, lsp_types::Message::String(message) if message.starts_with(expected))),
                    None => messages.is_empty(),
                }
            {
                break;
            }
        }
    };
    wait_for_compiler(Some("Initial failure."));
    std::fs::write(&log, "! Replacement failure.\nl.1 Text\n").unwrap();
    wait_for_compiler(Some("Replacement failure."));
    std::fs::remove_file(&log).unwrap();
    wait_for_compiler(None);
    shutdown(&client, server);
}

#[test]
fn workspace_diagnostics_acquire_compiler_artifacts_for_closed_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.tex");
    let other = dir.path().join("other.tex");
    for path in [&root, &other] {
        std::fs::write(path, "\\documentclass{article}\n").unwrap();
    }
    std::fs::write(
        other.with_extension("log"),
        "./other.tex:1: Closed root failure\n",
    )
    .unwrap();
    let (client, server) = start_server_pull();
    did_open(
        &client,
        &path_to_file_uri(&root),
        1,
        "\\documentclass{article}\n",
    );
    send_request(
        &client,
        60,
        "workspace/diagnostic",
        serde_json::json!({"previousResultIds":[]}),
    );
    let report = loop {
        match recv(&client) {
            Message::Response(response) if response.id == RequestId::from(60) => {
                break response.response_result.unwrap();
            }
            Message::Notification(_) => {}
            Message::Request(request) => client
                .sender
                .send(Message::Response(Response::new_ok(
                    request.id,
                    serde_json::Value::Null,
                )))
                .unwrap(),
            other => panic!("unexpected response {other:?}"),
        }
    };
    let item = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["uri"] == path_to_file_uri(&other).as_str())
        .unwrap();
    assert!(
        item["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["source"] == "compiler"),
        "{item}"
    );
    shutdown(&client, server);
}

#[test]
fn workspace_diagnostics_keep_closed_file_settings_when_config_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("tex-ls.toml");
    let root = dir.path().join("main.tex");
    let child = dir.path().join("child.tex");
    std::fs::write(&root, "\\documentclass{article}\n\\input{child}\n").unwrap();
    std::fs::write(&config, "[lint]\nselect=[]\n").unwrap();
    std::fs::write(&child, "\\cite{missing}\n").unwrap();
    let (client, server) = start_server_pull();
    did_open(
        &client,
        &path_to_file_uri(&root),
        1,
        "\\documentclass{article}\n\\input{child}\n",
    );
    for (id, contents, expect_finding) in [
        (50, "[lint]\nselect=[]\n", false),
        (51, "[lint\ninvalid", false),
        (52, "[lint]\nselect=[\"undefined-citation\"]\n", true),
    ] {
        std::fs::write(&config, contents).unwrap();
        send_request(
            &client,
            id,
            "workspace/diagnostic",
            serde_json::json!({"previousResultIds":[]}),
        );
        let result = loop {
            match recv(&client) {
                Message::Response(response) if response.id == RequestId::from(id) => {
                    break response.response_result.unwrap();
                }
                Message::Notification(_) => {}
                Message::Request(request) => client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        request.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap(),
                other => panic!("unexpected message: {other:?}"),
            }
        };
        let report = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["uri"] == path_to_file_uri(&child).as_str())
            .unwrap_or_else(|| panic!("child missing: {result}"));
        assert_eq!(
            report["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["code"] == "undefined-citation"),
            expect_finding
        );
    }
    shutdown(&client, server);
}

#[test]
fn workspace_diagnostics_reject_malformed_parameters_without_progress() {
    let (client, server) = start_server_pull();
    for (index, params) in [
        serde_json::json!(null),
        serde_json::json!({}),
        serde_json::json!({"previousResultIds":"wrong"}),
        serde_json::json!({"previousResultIds":[{"uri":fixture_uri!("/main.tex")}]}),
        serde_json::json!({"previousResultIds":[],"partialResultToken":{}}),
        serde_json::json!({"previousResultIds":[],"workDoneToken":[]}),
        serde_json::json!({"previousResultIds":[],"identifier":42}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = 20 + index as i32;
        send_request(&client, id, "workspace/diagnostic", params);
        let Message::Response(response) = recv(&client) else {
            panic!("invalid requests must return an error without emitting progress");
        };
        assert_eq!(response.id, RequestId::from(id));
        assert_eq!(response.response_result.unwrap_err().code, -32602);
    }
    send_request(
        &client,
        40,
        "workspace/diagnostic",
        serde_json::json!({"previousResultIds":[],"partialResultToken":7}),
    );
    assert_eq!(
        recv_response(&client).response_result.unwrap(),
        serde_json::json!({"items":[]})
    );
    shutdown(&client, server);
}

#[test]
fn lsp_workspace_compiler_pull_partial_and_deleted_log() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.tex");
    let uri = path_to_file_uri(&path);
    let log = path.with_extension("log");
    std::fs::write(&log, "./main.tex:2: Bad command\n").unwrap();
    let (client, server_thread) = start_server_pull();
    did_open(&client, &uri, 1, "Text.\nSecond line.\n");
    pull_diagnostic(&client, 21, &uri, None);
    let first = recv_document_diagnostic_report(&client, 21);
    assert!(
        report_items(&first)
            .unwrap()
            .iter()
            .any(|item| item.source.as_deref() == Some("compiler") && item.range.start.line == 1)
    );
    let previous = report_result_id(&first).unwrap();
    send_request(
        &client,
        22,
        "workspace/diagnostic",
        serde_json::json!({"previousResultIds":[{"uri":uri,"value":previous}],"partialResultToken":"batch"}),
    );
    let mut partial = Vec::new();
    loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Notification(notification) if notification.method == "$/progress" => {
                assert_eq!(notification.params["token"], "batch");
                partial.extend(
                    notification.params["value"]["items"]
                        .as_array()
                        .unwrap()
                        .clone(),
                );
            }
            Message::Response(response) if response.id == RequestId::from(22) => {
                assert_eq!(
                    response.response_result.unwrap()["items"],
                    serde_json::json!([])
                );
                break;
            }
            Message::Request(request) => {
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        request.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
            other => panic!("unexpected diagnostic delivery: {other:?}"),
        }
    }
    assert!(
        partial
            .iter()
            .any(|report| report["uri"] == serde_json::json!(uri) && report["kind"] == "unchanged")
    );
    std::fs::remove_file(&log).unwrap();
    send_notification(
        &client,
        "workspace/didChangeWatchedFiles",
        serde_json::json!({"changes":[{"uri":path_to_file_uri(&log),"type":3}]}),
    );
    pull_diagnostic(&client, 23, &uri, Some(previous));
    let cleared = recv_document_diagnostic_report(&client, 23);
    assert!(
        report_items(&cleared)
            .unwrap()
            .iter()
            .all(|item| item.source.as_deref() != Some("compiler"))
    );
    shutdown(&client, server_thread);
}

#[test]
fn lsp_edit_to_diagnostic_measurement() {
    let (client, server_thread) = start_server_pull();
    let uri: Uri = fixture_uri!("/diagnostic-latency.tex").parse().unwrap();
    let source = "A sentence with ordinary words.\n\n".repeat(200);
    did_open(&client, &uri, 1, &source);
    pull_diagnostic(&client, 100, &uri, None);
    recv_document_diagnostic_report(&client, 100);
    let mut times = Vec::new();
    for version in 2..=11 {
        let start = std::time::Instant::now();
        did_change_full(
            &client,
            &uri,
            version,
            &format!("{source}Version {version}.\n"),
        );
        pull_diagnostic(&client, 100 + version, &uri, None);
        recv_document_diagnostic_report(&client, 100 + version);
        times.push(start.elapsed());
    }
    times.sort();
    eprintln!(
        "edit-to-diagnostic (200 paragraphs, 10 edits): median {:?}, max empirical p95 {:?}",
        times[5], times[9]
    );
    shutdown(&client, server_thread);
}

#[test]
fn lsp_compiler_push_clears_after_log_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.tex");
    let uri = path_to_file_uri(&path);
    let log = path.with_extension("log");
    std::fs::write(&log, "./main.tex:1: Bad command\n").unwrap();
    let (client, server_thread) = start_server(None);
    did_open(&client, &uri, 1, "Text.\n");
    loop {
        let report = recv_diagnostics(&client);
        if report.uri == uri
            && report
                .diagnostics
                .iter()
                .any(|item| item.source.as_deref() == Some("compiler"))
        {
            break;
        }
    }
    std::fs::remove_file(&log).unwrap();
    send_notification(
        &client,
        "workspace/didChangeWatchedFiles",
        serde_json::json!({"changes":[{"uri":path_to_file_uri(&log),"type":3}]}),
    );
    loop {
        let report = recv_diagnostics(&client);
        if report.uri == uri
            && report
                .diagnostics
                .iter()
                .all(|item| item.source.as_deref() != Some("compiler"))
        {
            break;
        }
    }
    shutdown(&client, server_thread);
}

#[test]
fn label_hints_refresh_and_clear_with_compiler_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.tex");
    let uri = path_to_file_uri(&path);
    let aux = path.with_extension("aux");
    std::fs::write(&aux, "\\newlabel{a}{{12345}{1}}\n").unwrap();
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
    send_request(
        &client,
        1,
        "initialize",
        serde_json::json!({"capabilities":{"textDocument":{"diagnostic":{},"inlayHint":{}},"workspace":{"inlayHint":{"refreshSupport":true}}},"initializationOptions":{"texmf":{"enabled":false},"inlayHints":{"maxLength":3}}}),
    );
    let initialized = recv_response(&client).response_result.unwrap();
    assert_eq!(
        initialized["capabilities"]["inlayHintProvider"]["resolveProvider"],
        false
    );
    send_notification(&client, "initialized", serde_json::json!({}));
    did_open(&client, &uri, 1, "😀 \\label{a} \\ref{a}\n");
    let params = serde_json::json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":{"line":2,"character":0}}});
    send_request(&client, 2, "textDocument/inlayHint", params.clone());
    let hints = recv_response(&client).response_result.unwrap();
    assert_eq!(hints.as_array().unwrap().len(), 2);
    assert_eq!(hints[0]["label"], "12…");
    std::fs::remove_file(&aux).unwrap();
    send_notification(
        &client,
        "workspace/didChangeWatchedFiles",
        serde_json::json!({"changes":[{"uri":path_to_file_uri(&aux),"type":3}]}),
    );
    send_request(&client, 3, "textDocument/inlayHint", params);
    let mut refreshed = false;
    let mut cleared = false;
    while !refreshed || !cleared {
        match recv(&client) {
            Message::Request(request) => {
                refreshed |= request.method == "workspace/inlayHint/refresh";
                client
                    .sender
                    .send(Message::Response(Response::new_ok(
                        request.id,
                        serde_json::Value::Null,
                    )))
                    .unwrap();
            }
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(3));
                assert_eq!(response.response_result.unwrap(), serde_json::json!([]));
                cleared = true;
            }
            Message::Notification(notification) => {
                assert_ne!(notification.method, "textDocument/publishDiagnostics")
            }
        }
    }
    shutdown(&client, server_thread);
}

#[test]
fn forward_search_rejects_old_method_and_malformed_arguments() {
    let (client, server) = start_server(None);
    send_request(
        &client,
        2,
        "textDocument/forwardSearch",
        serde_json::json!({}),
    );
    assert_eq!(
        recv_response(&client).response_result.unwrap_err().code,
        -32601
    );
    for (id, arguments) in [
        (3, serde_json::json!([])),
        (4, serde_json::json!([{"uri":fixture_uri!("/main.tex")}])),
        (5, serde_json::json!([{}, {}])),
    ] {
        send_request(
            &client,
            id,
            "workspace/executeCommand",
            serde_json::json!({"command":"tex-ls.forwardSearch","arguments":arguments}),
        );
        assert_eq!(
            recv_response(&client).response_result.unwrap_err().code,
            -32602
        );
    }
    shutdown(&client, server);
}

#[test]
fn forward_search_reports_viewer_launch_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.tex");
    std::fs::write(&path, "hello\n").unwrap();
    std::fs::write(path.with_extension("pdf"), "%PDF-1.7\n").unwrap();
    let (client, server) = start_server(Some(
        serde_json::json!({"texmf":{"enabled":false},"forwardSearch":{"executable":dir.path().join("missing-viewer"),"args":["%p"]}}),
    ));
    let uri = path_to_file_uri(&path);
    did_open(&client, &uri, 1, "hello\n");
    let _ = recv_diagnostics(&client);
    assert_eq!(
        forward_search(&client, 2, &uri, Position::new(0, 0)),
        "launchFailed"
    );
    shutdown(&client, server);
}

#[test]
fn forward_search_refuses_ambiguous_roots() {
    let dir = tempfile::tempdir().unwrap();
    let source = "\\documentclass{article}\n\\input{child}\n";
    for name in ["one", "two"] {
        std::fs::write(dir.path().join(format!("{name}.tex")), source).unwrap();
        std::fs::write(dir.path().join(format!("{name}.pdf")), "%PDF-1.7\n").unwrap();
    }
    let path = dir.path().join("child.tex");
    std::fs::write(&path, "child\n").unwrap();
    let record = dir.path().join("viewer.txt");
    let (client, server) = start_server(Some(viewer_options(&record, &["%p"])));
    for name in ["one.tex", "two.tex", "child.tex"] {
        let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
        did_open(&client, &path_to_file_uri(&dir.path().join(name)), 1, &text);
        let _ = recv_diagnostics(&client);
    }
    assert_eq!(
        forward_search(&client, 2, &path_to_file_uri(&path), Position::new(0, 0)),
        "ambiguousRoot"
    );
    assert!(!record.exists());
    shutdown(&client, server);
}

#[test]
fn lsp_chapter_acquisition_loads_root_hints_and_configured_roots() {
    for mode in ["hint", "configured", "conflicting", "missing", "cycle"] {
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("chapters/nested/ch.tex");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        let main = directory.path().join("main.tex");
        std::fs::write(
            &main,
            "\\documentclass{article}\n\\label{sec:parent}\n\\input{chapters/nested/ch}\n",
        )
        .unwrap();
        let hint = match mode {
            "configured" => {
                std::fs::write(
                    directory.path().join("tex-ls.toml"),
                    "[build]\nroot = 'main.tex'\n",
                )
                .unwrap();
                ""
            }
            "conflicting" => {
                std::fs::write(
                    directory.path().join("other.tex"),
                    "\\documentclass{book}\n\\input{chapters/nested/ch}\n",
                )
                .unwrap();
                "% !TeX root = ../../main.tex\n% !TeX root = ../../other.tex\n"
            }
            "missing" => "% !TeX root = ../../absent.tex\n",
            "cycle" => {
                std::fs::write(&main, "% !TeX root = chapters/nested/ch.tex\n\\documentclass{article}\n\\label{sec:parent}\n\\input{chapters/nested/ch}\n").unwrap();
                "% !TeX root = ../../main.tex\n"
            }
            _ => "% !TeX root = ../../main.tex\n",
        };
        let source = format!("{hint}\\ref{{sec:par}}\n");
        std::fs::write(&child, &source).unwrap();
        let (server, client) = Connection::memory();
        let worker = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
        send_request(
            &client,
            1,
            "initialize",
            serde_json::json!({"capabilities":{"textDocument":{"diagnostic":{}}},"initializationOptions":{"texmf":{"enabled":false}}}),
        );
        recv_response(&client).response_result.unwrap();
        send_notification(&client, "initialized", serde_json::json!({}));
        let uri = path_to_file_uri(&child);
        did_open(&client, &uri, 1, &source);
        let items = complete(
            &client,
            2,
            &uri,
            Position::new(hint.lines().count() as u32, 12),
        );
        if matches!(mode, "hint" | "configured" | "cycle") {
            assert!(
                items.iter().any(|item| item.label == "sec:parent"),
                "{mode}: {items:?}"
            );
        }
        send_request(
            &client,
            3,
            "workspace/executeCommand",
            serde_json::json!({"command":"tex-ls.inspectProject","arguments":[{"uri":uri}]}),
        );
        let inspection = recv_response(&client).response_result.unwrap();
        assert_eq!(
            inspection["ambiguous"],
            mode == "conflicting",
            "{mode}: {inspection}"
        );
        let expected = if mode == "conflicting" { 2 } else { 1 };
        assert_eq!(
            inspection["candidateRoots"].as_array().unwrap().len(),
            expected,
            "{mode}: {inspection}"
        );
        if mode == "missing" {
            assert_eq!(inspection["rootHints"].as_array().unwrap().len(), 1);
        }
        shutdown(&client, worker);
    }
}

#[test]
fn configuration_and_package_changes_refresh_unedited_documents_for_pull_clients() {
    use serde_json::{Value, json};
    use std::collections::HashSet;
    fn reply(client: &Connection, request: Request, seen: &mut HashSet<String>) {
        seen.insert(request.method);
        client
            .sender
            .send(Message::Response(Response::new_ok(request.id, Value::Null)))
            .unwrap();
    }
    fn query(
        client: &Connection,
        id: i32,
        method: &str,
        params: Value,
        seen: &mut HashSet<String>,
    ) -> Value {
        send_request(client, id, method, params);
        loop {
            match recv(client) {
                Message::Request(request) => reply(client, request, seen),
                Message::Notification(_) => {}
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.response_result.unwrap();
                }
            }
        }
    }
    fn wait_for(client: &Connection, wanted: &[&str], seen: &mut HashSet<String>) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !wanted.iter().all(|method| seen.contains(*method)) {
            assert!(
                std::time::Instant::now() < deadline,
                "missing refreshes: {seen:?}"
            );
            match recv(client) {
                Message::Request(request) => reply(client, request, seen),
                Message::Notification(_) => {}
                other => panic!("unexpected message {other:?}"),
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.tex");
    let package = dir.path().join("custom.sty");
    let uri = path_to_file_uri(&path);
    let package_uri = path_to_file_uri(&package);
    let source = "\\usepackage{custom}\n\\brand{x}\n\\begin{mycode}\n\\section{Hidden}\nbody\n\\end{mycode}\n";
    std::fs::write(&path, source).unwrap();
    std::fs::write(&package, "% no definitions\n").unwrap();
    let (server, client) = Connection::memory();
    let thread = std::thread::spawn(move || tex_ls::lsp::serve(server).unwrap());
    let mut seen = HashSet::new();
    query(
        &client,
        1,
        "initialize",
        json!({
            "rootUri":path_to_file_uri(dir.path()),
            "initializationOptions":{"texmf":{"enabled":false}},
            "capabilities":{
                "workspace":{"foldingRange":{"refreshSupport":true},"semanticTokens":{"refreshSupport":true}},
                "textDocument":{"diagnostic":{},"semanticTokens":{"formats":["relative"],"tokenTypes":["macro","type"],"requests":{"full":true}}}
            }
        }),
        &mut seen,
    );
    send_notification(&client, "initialized", json!({}));
    did_open(&client, &uri, 1, source);
    let params = json!({"textDocument":{"uri":uri}});
    let before = query(
        &client,
        2,
        "textDocument/foldingRange",
        params.clone(),
        &mut seen,
    );
    // Acquire the local package before testing an edit to its existing overlay.
    query(
        &client,
        3,
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":1,"character":4}}),
        &mut seen,
    );
    let tokens_before = query(
        &client,
        4,
        "textDocument/semanticTokens/full",
        params.clone(),
        &mut seen,
    );
    seen.clear();
    std::fs::write(
        dir.path().join("tex-ls.toml"),
        "[environments.mycode]\nlike = 'lstlisting'\n",
    )
    .unwrap();
    did_change_watched_files(
        &client,
        &[(
            path_to_file_uri(&dir.path().join("tex-ls.toml")),
            FileChangeType::Created,
        )],
    );
    // No diagnostic refresh capability and no edit to main.tex: declarations
    // must still be published before the folding/token refresh is sent.
    wait_for(
        &client,
        &[
            "workspace/foldingRange/refresh",
            "workspace/semanticTokens/refresh",
        ],
        &mut seen,
    );
    let after = query(
        &client,
        5,
        "textDocument/foldingRange",
        params.clone(),
        &mut seen,
    );
    assert!(after.as_array().unwrap().len() < before.as_array().unwrap().len());
    let tokens_after = query(
        &client,
        6,
        "textDocument/semanticTokens/full",
        params.clone(),
        &mut seen,
    );
    assert_ne!(tokens_before, tokens_after);
    seen.clear();
    did_open(&client, &package_uri, 1, "\\newcommand{\\brand}[1]{#1}\n");
    wait_for(&client, &["workspace/semanticTokens/refresh"], &mut seen);
    let tokens = query(
        &client,
        7,
        "textDocument/semanticTokens/full",
        params,
        &mut seen,
    );
    let mut line = 0;
    assert!(
        tokens["data"]
            .as_array()
            .unwrap()
            .as_chunks::<5>()
            .0
            .iter()
            .any(|token| {
                line += token[0].as_u64().unwrap();
                line == 1 && token[1] == 0
            }),
        "new package command should be highlighted: {tokens}"
    );
    query(&client, 8, "shutdown", Value::Null, &mut seen);
    send_notification(&client, "exit", Value::Null);
    thread.join().unwrap();
}

#[test]
fn watched_bibliography_creation_deletion_and_recreation_update_completion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.tex");
    let bib = dir.path().join("refs.bib");
    let uri = path_to_file_uri(&path);
    let bib_uri = path_to_file_uri(&bib);
    let text = "\\documentclass{article}\n\\bibliography{refs}\n\\cite{}\n";
    std::fs::write(&path, text).unwrap();
    let (client, thread, _) =
        start_with_capabilities(serde_json::json!({"textDocument":{"diagnostic":{}}}));
    did_open(&client, &uri, 1, text);
    let keys = |id| {
        send_request(
            &client,
            id,
            "textDocument/completion",
            serde_json::json!({
                "textDocument":{"uri":uri},"position":{"line":2,"character":6}
            }),
        );
        let result = recv_response(&client).response_result.unwrap();
        result["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert!(keys(2).is_empty());
    std::fs::write(&bib, "@misc{first, title={First}}\n").unwrap();
    did_change_watched_files(&client, &[(bib_uri.clone(), FileChangeType::Created)]);
    assert_eq!(keys(3), ["first"]);
    std::fs::remove_file(&bib).unwrap();
    did_change_watched_files(&client, &[(bib_uri.clone(), FileChangeType::Deleted)]);
    assert!(keys(4).is_empty());
    std::fs::write(&bib, "@misc{second, title={Second}}\n").unwrap();
    did_change_watched_files(&client, &[(bib_uri, FileChangeType::Created)]);
    assert_eq!(keys(5), ["second"]);
    shutdown(&client, thread);
}
