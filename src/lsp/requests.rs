use super::*;

/// `textDocument/formatting`: build a format job for the worker, or reply `null`
/// when the document is unknown.
pub(super) fn on_formatting(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params =
        match req.extract::<DocumentFormattingParams>(DocumentFormattingRequest::METHOD.as_str()) {
            Ok((_, params)) => params,
            Err(_) => {
                let resp = Response::new_err(
                    id,
                    ErrorCode::InvalidParams as i32,
                    "invalid formatting params".to_owned(),
                );
                let _ = connection.sender.send(Message::Response(resp));
                return;
            }
        };

    let uri = params.text_document.uri;
    if !state.documents.contains_key(&uri) {
        // Unknown document: nothing to format.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    }
    let resolved = state.resolve_settings(&uri);
    let mut style = resolved.style;
    // Only an explicit file indent-width overrides the request tab size.
    if !resolved.indent_width_configured && params.options.tab_size > 0 {
        style.indent_width = params.options.tab_size as usize;
    }
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    // `wrap` is decided per request: a configured `wrap` wins, else the default
    // (`reflow`, the same for every file kind).
    style.wrap = resolved.wrap_override.unwrap_or_default();
    let _ = job_tx.send(WorkerJob::Format {
        id,
        path,
        style,
        kind,
        sentence_lang: resolved.sentence_lang,
        sentence_no_break: resolved.sentence_no_break,
    });
}

/// `textDocument/rangeFormatting`: build a range-format job for the worker, or
/// reply `null` when the document is unknown. Mirrors [`on_formatting`]; the only
/// extra input is the selection `range`, resolved against the buffer on the read
/// pool.
pub(super) fn on_range_formatting(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let ranges: Option<Vec<Range>> = if req.method == "textDocument/rangesFormatting" {
        req.params
            .get("ranges")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
    } else {
        req.params
            .get("range")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .map(|range| vec![range])
    };
    let mut params_value = req.params;
    params_value["range"] = serde_json::json!(Range::default());
    let parsed = serde_json::from_value::<DocumentRangeFormattingParams>(params_value);
    let (Ok(params), Some(ranges)) = (parsed, ranges) else {
        let _ = connection.sender.send(Message::Response(Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            "Invalid formatting ranges".into(),
        )));
        return;
    };

    let uri = params.text_document.uri;
    if !state.documents.contains_key(&uri) {
        // Unknown document: nothing to format.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    }
    let resolved = state.resolve_settings(&uri);
    let mut style = resolved.style;
    if !resolved.indent_width_configured && params.options.tab_size > 0 {
        style.indent_width = params.options.tab_size as usize;
    }
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    style.wrap = resolved.wrap_override.unwrap_or_default();
    let _ = job_tx.send(WorkerJob::RangeFormat {
        id,
        path,
        style,
        kind,
        ranges,
        sentence_lang: resolved.sentence_lang,
        sentence_no_break: resolved.sentence_no_break,
    });
}

/// Handle `textDocument/onTypeFormatting`. The client fires this only on a
/// registered trigger (`}`); we re-check the character and dispatch a read-pool
/// job that re-indents the containing block when the `}` structurally closes a
/// multi-line construct. Mirrors [`on_range_formatting`]'s settings resolution.
pub(super) fn on_type_formatting(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req
        .extract::<DocumentOnTypeFormattingParams>(DocumentOnTypeFormattingRequest::METHOD.as_str())
    {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid on-type formatting params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    // We only re-indent on `}`. Any other trigger is a no-op (reply `null`).
    let uri = params.text_document.uri;
    if params.ch != "}" || !state.documents.contains_key(&uri) {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    }
    let resolved = state.resolve_settings(&uri);
    let mut style = resolved.style;
    if !resolved.indent_width_configured && params.options.tab_size > 0 {
        style.indent_width = params.options.tab_size as usize;
    }
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    style.wrap = resolved.wrap_override.unwrap_or_default();
    let _ = job_tx.send(WorkerJob::OnTypeFormat {
        id,
        path,
        style,
        kind,
        position: params.position,
        sentence_lang: resolved.sentence_lang,
        sentence_no_break: resolved.sentence_no_break,
    });
}

/// `textDocument/documentSymbol`: build an outline job for the worker, or reply
/// `null` when the document is unknown.
pub(super) fn on_document_symbol(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<DocumentSymbolParams>(DocumentSymbolRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid documentSymbol params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document.uri;
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: no symbols.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    // Resolve `[build]` for the `.aux` number enrichment, like hover.
    let build = state.resolve_settings(&uri).build;
    let _ = job_tx.send(WorkerJob::Symbols {
        id,
        path,
        kind,
        build,
        options: state.client_settings(&uri).outline.clone(),
    });
}

/// `workspace/symbol`: forward the query to the worker, which scans every tracked
/// project file. Unlike [`on_document_symbol`], it is not tied to an open buffer.
pub(super) fn on_workspace_symbol(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<WorkspaceSymbolParams>(WorkspaceSymbolRequest::METHOD.as_str())
    {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid workspace/symbol params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };
    let _ = job_tx.send(WorkerJob::WorkspaceSymbols {
        id,
        query: params.query,
        options: std::iter::once((PathBuf::new(), state.editor_settings.outline.clone()))
            .chain(
                state
                    .scoped_editor_settings
                    .iter()
                    .map(|(path, settings)| (path.clone(), settings.outline.clone())),
            )
            .collect(),
        active: state.active_path.clone(),
    });
}

/// `textDocument/foldingRange`: build a folding job for the worker, or reply `null`
/// when the document is unknown.
pub(super) fn on_folding_range(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<FoldingRangeParams>(FoldingRangeRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid foldingRange params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document.uri;
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: no folds.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    let _ = job_tx.send(WorkerJob::FoldingRange { id, path, kind });
}

/// `textDocument/selectionRange`: build a selection-range job for the worker, or reply
/// `null` when the document is unknown. Modeled on [`on_folding_range`], plus the
/// cursor `positions` the expand-selection chains are computed at.
pub(super) fn on_selection_range(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<SelectionRangeParams>(SelectionRangeRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid selectionRange params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document.uri;
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: no ranges.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    let _ = job_tx.send(WorkerJob::SelectionRange {
        id,
        path,
        kind,
        positions: params.positions,
    });
}

/// `textDocument/documentLink`: dispatch a document-link job to the worker. Replies
/// `null` for an unknown document; otherwise the read pool resolves the links.
/// Uses the source snapshot, like [`on_folding_range`].
pub(super) fn on_document_link(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<DocumentLinkParams>(DocumentLinkRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid documentLink params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document.uri;
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: no links.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    // `texmf` is editor (machine) configuration, not resolved per document
    // (session-stable; the index is first-wins).
    let texmf = state.client_settings(&uri).texmf.clone();
    let _ = job_tx.send(WorkerJob::DocumentLink {
        id,
        path,
        kind,
        texmf,
    });
}

/// `textDocument/diagnostic`: build an on-demand diagnostic job for the worker.
///
/// Always replies with a *report* (never `null`): an empty full report when the
/// client is push-only (it should not be pulling) or the document is unknown,
/// otherwise a [`WorkerJob::Diagnostic`] that computes off a fresh snapshot. The
/// snapshot is current because the preceding edit's `Edit` job sits ahead of this
/// one on the FIFO `job_tx` (see [`WorkerJob::Diagnostic`]).
pub(super) fn on_document_diagnostic(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params =
        match req.extract::<DocumentDiagnosticParams>(DocumentDiagnosticRequest::METHOD.as_str()) {
            Ok((_, params)) => params,
            Err(_) => {
                let resp = Response::new_err(
                    id,
                    ErrorCode::InvalidParams as i32,
                    "invalid diagnostic params".to_owned(),
                );
                let _ = connection.sender.send(Message::Response(resp));
                return;
            }
        };

    let uri = params.text_document.uri;
    // A push-only client should not be pulling; an unknown document has no buffer.
    // Either way, answer with an empty full report rather than leaving the request
    // hanging or replying `null`.
    if !state.supports_pull_diagnostics {
        reply_empty_diagnostic_report(connection, id);
        return;
    }
    let Some(_) = state.documents.get(&uri) else {
        reply_empty_diagnostic_report(connection, id);
        return;
    };
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    let resolved = state.resolve_settings(&uri);
    let rules = resolved.rule_selection();
    let _ = job_tx.send(WorkerJob::Diagnostic {
        id,
        path,
        kind,
        previous_result_id: params.previous_result_id,
        rules,
        build: resolved.build,
    });
}

/// Reply to a `textDocument/diagnostic` request with an empty *full* report. Used
/// when there is nothing to compute (push-only client, unknown buffer) — the pull
/// protocol requires a report, so `null` is not an option.
pub(super) fn reply_empty_diagnostic_report(connection: &Connection, id: RequestId) {
    let report = DocumentDiagnosticReportProgress::DocumentDiagnosticReport(
        DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(
            RelatedFullDocumentDiagnosticReport::default(),
        ),
    );
    let value = serde_json::to_value(report).unwrap_or(serde_json::Value::Null);
    let _ = connection
        .sender
        .send(Message::Response(Response::new_ok(id, value)));
}

/// `textDocument/completion`: build a completion job for the worker, or reply
/// `null` when the document is unknown.
pub(super) fn on_completion(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<CompletionParams>(CompletionRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid completion params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: nothing to complete.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    // The editor's `texmf` settings gate the installed-set completion tier
    // (session-stable).
    let texmf = state.client_settings(&uri).texmf.clone();
    let _ = job_tx.send(WorkerJob::Completion {
        id,
        uri,
        position,
        texmf,
    });
}

/// `completionItem/resolve`: dispatch a resolve job for the worker. The item's
/// `data` is self-contained (it carries everything needed to recompute detail),
/// so there is no document to look up — only invalid params short-circuit here.
pub(super) fn on_completion_resolve(
    connection: &Connection,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let item = match req.extract::<CompletionItem>(CompletionResolveRequest::METHOD.as_str()) {
        Ok((_, item)) => item,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid completion item".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };
    let _ = job_tx.send(WorkerJob::ResolveCompletion {
        id,
        item: Box::new(item),
    });
}

/// `textDocument/hover`: build a hover job for the worker, or reply `null` when the
/// document is unknown. A `.bib` cursor is not rejected — `compute_hover` simply finds
/// nothing there today (no bib-field hover yet), so it returns `null` on its own.
pub(super) fn on_hover(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<HoverParams>(HoverRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid hover params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: nothing to describe.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    // Resolve `[build]` for the label-number lookup (a `\label`/`\ref` hover reads
    // the compile's `.aux`), like go-to-def resolves `[texmf]`.
    let build = state.resolve_settings(&uri).build;
    let _ = job_tx.send(WorkerJob::Hover {
        id,
        path,
        position,
        build,
    });
}

/// Launch a native viewer from the standard execute-command request.
pub(super) fn on_forward_search(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    id: RequestId,
    params: ForwardSearchParams,
) {
    let reply = |outcome: ForwardSearchOutcome| {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id.clone(),
            serde_json::to_value(outcome).expect("outcome serializes"),
        )));
    };
    let Some((executable, args)) = state.client_settings(&params.uri).forward_search.viewer()
    else {
        reply(ForwardSearchOutcome::Unconfigured);
        return;
    };
    let (executable, args) = (executable.to_owned(), args.to_vec());
    let Some(path) = uri_to_fs_path(&params.uri) else {
        reply(ForwardSearchOutcome::UnsupportedSource);
        return;
    };
    let Some(line) = params.position.line.checked_add(1) else {
        let _ = connection.sender.send(Message::Response(Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            "Source line exceeds supported range".into(),
        )));
        return;
    };
    let build = state.resolve_settings(&params.uri).build;
    let _ = job_tx.send(WorkerJob::ForwardSearch {
        id,
        path,
        line,
        build,
        executable,
        args,
    });
}

/// `textDocument/signatureHelp`: build a signature-help job for the worker, or
/// reply `null` when the document is unknown. The request's `context` is ignored:
/// help is recomputed statelessly per request, and with exactly one signature per
/// reply `is_retrigger`/`active_signature_help` add nothing.
pub(super) fn on_signature_help(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<SignatureHelpParams>(SignatureHelpRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid signature help params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: nothing to describe.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let _ = job_tx.send(WorkerJob::SignatureHelp { id, path, position });
}

/// `textDocument/codeAction`: build a code-action job for the worker, or reply with
/// an empty action list when the document is unknown. Surfaces linter autofixes as
/// quick-fixes and syntax-aware LaTeX refactorings; a `.bib` cursor is handled too
/// (its bib-lint fixes are surfaced the same way).
pub(super) fn on_code_action(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<CodeActionParams>(CodeActionRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid codeAction params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document.uri;
    let range = params.range;
    let only = params.context.only;
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: no actions.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let path = uri_to_path(&uri);
    let kind = file_kind_for(&path);
    let rules = state.resolve_settings(&uri).rule_selection();
    let _ = job_tx.send(WorkerJob::CodeAction {
        id,
        uri,
        path,
        kind,
        range,
        only,
        rules,
    });
}

/// `textDocument/definition`: build a go-to-definition job for the worker, or reply
/// `null` when the document is unknown or is a `.bib` (cite/ref sites live in
/// `.tex`, so a `.bib` cursor has nothing to jump *from*).
pub(super) fn on_goto_definition(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<DefinitionParams>(DefinitionRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid definition params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: nothing to resolve.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    // The editor's `texmf` settings gate the file-target fallback (an include/package
    // argument jumps to its resolved source, TEXMF-aware like document links).
    let texmf = state.client_settings(&uri).texmf.clone();
    let _ = job_tx.send(WorkerJob::GotoDefinition {
        id,
        path,
        position,
        texmf,
    });
}

/// `textDocument/references`: build a find-references job for the worker, or reply
/// `null` when the document is unknown. Unlike go-to-definition, a `.bib` cursor is
/// *not* rejected — find-references can start on an `@entry` key and report its
/// `\cite` use sites.
pub(super) fn on_references(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<ReferenceParams>(ReferencesRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid references params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let include_declaration = params.context.include_declaration;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: nothing to resolve.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let _ = job_tx.send(WorkerJob::References {
        id,
        path,
        position,
        include_declaration,
    });
}

/// `textDocument/documentHighlight`: build a document-highlight job for the worker,
/// or reply `null` when the document is unknown. Single-file, so no project membership
/// is captured — the worker shades the key under the cursor against the cursor buffer
/// alone.
pub(super) fn on_document_highlight(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params =
        match req.extract::<DocumentHighlightParams>(DocumentHighlightRequest::METHOD.as_str()) {
            Ok((_, params)) => params,
            Err(_) => {
                let resp = Response::new_err(
                    id,
                    ErrorCode::InvalidParams as i32,
                    "invalid document-highlight params".to_owned(),
                );
                let _ = connection.sender.send(Message::Response(resp));
                return;
            }
        };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        // Unknown document: nothing to highlight.
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let _ = job_tx.send(WorkerJob::DocumentHighlight { id, path, position });
}

/// `textDocument/prepareRename`: build a prepare-rename job, or reply `null` when
/// the document is unknown. The worker decides whether the cursor sits on a
/// renameable key (and returns its range + placeholder) or declines with `null`.
pub(super) fn on_prepare_rename(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params =
        match req.extract::<TextDocumentPositionParams>(PrepareRenameRequest::METHOD.as_str()) {
            Ok((_, params)) => params,
            Err(_) => {
                let resp = Response::new_err(
                    id,
                    ErrorCode::InvalidParams as i32,
                    "invalid prepareRename params".to_owned(),
                );
                let _ = connection.sender.send(Message::Response(resp));
                return;
            }
        };

    let uri = params.text_document.uri;
    let position = params.position;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let _ = job_tx.send(WorkerJob::PrepareRename { id, path, position });
}

/// `textDocument/rename`: build a rename job, or reply `null` when the document is
/// unknown. The worker resolves the key under the cursor and answers with a
/// project-wide [`WorkspaceEdit`] (or `null` when the rename is declined).
pub(super) fn on_rename(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let params = match req.extract::<RenameParams>(RenameRequest::METHOD.as_str()) {
        Ok((_, params)) => params,
        Err(_) => {
            let resp = Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "invalid rename params".to_owned(),
            );
            let _ = connection.sender.send(Message::Response(resp));
            return;
        }
    };

    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let new_name = params.new_name;
    let path = uri_to_path(&uri);
    let Some(_) = state.documents.get(&uri) else {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    };
    let _ = job_tx.send(WorkerJob::Rename {
        id,
        path,
        position,
        new_name,
    });
}

pub(super) fn on_execute_command(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let response = match req.extract::<ExecuteCommandParams>(ExecuteCommandRequest::METHOD.as_str())
    {
        Ok((_, params)) if params.command == "tex-ls.forwardSearch" => {
            let argument = params
                .arguments
                .filter(|arguments| arguments.len() == 1)
                .and_then(|mut arguments| arguments.pop())
                .and_then(|argument| serde_json::from_value::<ForwardSearchParams>(argument).ok());
            if let Some(argument) = argument {
                on_forward_search(connection, state, job_tx, id, argument);
                return;
            }
            Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "Expected arguments: [{uri, position}]".into(),
            )
        }
        Ok((_, params)) if params.command == "tex-ls.inspectAcquisition" => {
            let _ = job_tx.send(WorkerJob::InspectAcquisition { id });
            return;
        }
        Ok((_, params)) if params.command == "tex-ls.inspectProject" => {
            let path = params
                .arguments
                .as_ref()
                .and_then(|args| args.first())
                .and_then(|arg| arg.get("uri"))
                .cloned()
                .and_then(|value| serde_json::from_value::<Uri>(value).ok())
                .and_then(|uri| uri_to_fs_path(&uri));
            if let Some(path) = path {
                let _ = job_tx.send(WorkerJob::InspectProject { id, path });
                return;
            }
            Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                "Expected arguments: [{uri}]".into(),
            )
        }
        Ok((_, params)) => Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            format!("unknown workspace command: {}", params.command),
        ),
        Err(_) => Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            "invalid executeCommand params".into(),
        ),
    };
    let _ = connection.sender.send(Message::Response(response));
}

pub(super) fn on_linked_editing(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let id = req.id.clone();
    let Ok((_, params)) =
        req.extract::<TextDocumentPositionParams>("textDocument/linkedEditingRange")
    else {
        let _ = connection.sender.send(Message::Response(Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            "invalid linkedEditingRange params".into(),
        )));
        return;
    };
    let uri = params.text_document.uri;
    if !state.documents.contains_key(&uri) {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    }
    let _ = job_tx.send(WorkerJob::LinkedEditing {
        id,
        path: uri_to_path(&uri),
        position: params.position,
    });
}

/// Re-lint every open document, because cross-file resolution may have changed for all
/// of them (a project member's content changed, membership grew, or the governing
/// config changed). A pull client learns this by re-pulling: nudge it with
/// `workspace/diagnostic/refresh`. A push client gets a fresh analyze re-queued per
/// open document at its current version. Shared by [`Outbound::RelintAll`] and the
/// `tex-ls.toml` watched-change path.
pub(super) fn relint_all_open(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
) {
    // Publish configuration before asking pull clients to request new results.
    // This also reaches clients that do not support diagnostic refresh.
    for uri in state.documents.keys().cloned().collect::<Vec<_>>() {
        state.publish_declarations(&uri, job_tx);
    }
    if state.supports_inlay_refresh {
        let id = state.next_request_id;
        state.next_request_id += 1;
        let _ = connection.sender.send(Message::Request(Request {
            id: RequestId::from(format!("tex-ls-server-{id}")),
            method: "workspace/inlayHint/refresh".into(),
            params: serde_json::Value::Null,
        }));
    }
    if state.supports_pull_diagnostics {
        if state.supports_diagnostic_refresh {
            let id = state.next_request_id;
            state.next_request_id += 1;
            let _ = connection.sender.send(Message::Request(Request {
                id: RequestId::from(format!("tex-ls-server-{id}")),
                method: DiagnosticRefreshRequest::METHOD.as_str().to_owned(),
                params: serde_json::Value::Null,
            }));
        }
        return;
    }
    // Push mode: re-queue a fresh analyze for every open document at its current
    // version. The worker coalesces per-URI, so this is cheap; salsa memos make the
    // actual recompute incremental. A re-lint of a doc in an already-seeded directory
    // discovers no new members, so it can't re-trigger `RelintAll` (no loop). Snapshot
    // the buffers first so the per-document `resolve_settings` (`&mut self`) doesn't
    // alias the `documents` borrow.
    let snapshot: Vec<(Uri, Arc<TextBuffer>, i32)> = state
        .documents
        .iter()
        .map(|(uri, doc)| (uri.clone(), doc.text.clone(), doc.version))
        .collect();
    for (uri, text, version) in snapshot {
        let path = uri_to_path(&uri);
        let kind = file_kind_for(&path);
        let texmf = state.client_settings(&uri).texmf.clone();
        let resolved = state.analysis_settings(&uri);
        let _ = job_tx.send(WorkerJob::Edit {
            stamp: state.diagnostic_stamp,
            uri,
            path,
            text,
            version,
            opened: false,
            kind,
            rules: resolved.rule_selection(),
            build: Box::new(resolved.build.clone()),
            texmf,
            declarations: resolved.declarations.clone(),
            exclude: resolved.exclude,
            // The same text at the same version: nothing moved, so there is no
            // transform to describe. Clearing costs nothing — the base already is
            // this text, so `parsed_document` answers from it without a chain.
            edits: None,
        });
    }
}

pub(super) fn on_inlay_hints(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let Ok(params) = serde_json::from_value::<lsp_types::InlayHintParams>(req.params) else {
        let _ = connection.sender.send(Message::Response(Response::new_err(
            req.id,
            ErrorCode::InvalidParams as i32,
            "Invalid hint parameters".into(),
        )));
        return;
    };
    let uri = params.text_document.uri;
    if !state.documents.contains_key(&uri) {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            req.id,
            serde_json::json!([]),
        )));
        return;
    }
    let build = state.resolve_settings(&uri).build;
    let _ = job_tx.send(WorkerJob::InlayHints {
        id: req.id,
        path: uri_to_path(&uri),
        range: params.range,
        options: state.client_settings(&uri).inlay_hints.clone(),
        build,
    });
}

pub(super) fn on_semantic_tokens(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let parsed = (|| {
        let uri: Uri =
            serde_json::from_value(req.params.pointer("/textDocument/uri")?.clone()).ok()?;
        let range = if req.method.ends_with("/range") {
            Some(serde_json::from_value(req.params.get("range")?.clone()).ok()?)
        } else {
            None
        };
        Some((uri, range))
    })();
    let Some((uri, range)) = parsed else {
        let _ = connection.sender.send(Message::Response(Response::new_err(
            req.id,
            ErrorCode::InvalidParams as i32,
            "Invalid semantic token parameters".into(),
        )));
        return;
    };
    if !state.documents.contains_key(&uri) {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            req.id,
            serde_json::Value::Null,
        )));
        return;
    }
    let _ = job_tx.send(WorkerJob::SemanticTokens {
        id: req.id,
        path: uri_to_path(&uri),
        range,
    });
}

/// Dispatch color requests with typed parameter validation.
pub(super) fn on_colors(
    connection: &Connection,
    state: &GlobalState,
    job_tx: &Sender<WorkerJob>,
    req: Request,
) {
    let parsed = if req.method == "textDocument/colorPresentation" {
        serde_json::from_value::<lsp_types::ColorPresentationParams>(req.params)
            .map(|p| (p.text_document.uri, Some((p.range, p.color))))
    } else {
        serde_json::from_value::<lsp_types::DocumentColorParams>(req.params)
            .map(|p| (p.text_document.uri, None))
    };
    let Ok((uri, presentation)) = parsed else {
        let _ = connection.sender.send(Message::Response(Response::new_err(
            req.id,
            ErrorCode::InvalidParams as i32,
            "Invalid color parameters".into(),
        )));
        return;
    };
    if !state.documents.contains_key(&uri) {
        let _ = connection.sender.send(Message::Response(Response::new_ok(
            req.id,
            serde_json::json!([]),
        )));
        return;
    }
    let _ = job_tx.send(WorkerJob::Colors {
        id: req.id,
        path: uri_to_path(&uri),
        presentation,
    });
}
