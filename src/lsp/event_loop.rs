//! Native event loop responsibility.
use super::*;

/// The blocking message loop. Owns [`GlobalState`]; spawns the worker thread and
/// the read pool, then shuttles messages between the client and the workers.
/// `encoding` is the position encoding [`serve`] negotiated (and advertised) at
/// `initialize`.
pub(super) fn main_loop(
    connection: Connection,
    init_params: serde_json::Value,
    encoding: PositionEncoding,
) -> Result<(), DynError> {
    let editor_settings = init_params
        .get("initializationOptions")
        .map(EditorSettings::from_client_value)
        .unwrap_or_else(|| Ok(EditorSettings::default()));
    let (editor_settings, config_messages) = match editor_settings {
        Ok(settings) => (settings, Vec::new()),
        Err(error) => (EditorSettings::default(), vec![error]),
    };
    let (supports_pull_diagnostics, supports_diagnostic_refresh) =
        client_diagnostic_support(&init_params);
    let supports_dynamic_watchers = client_watched_files_support(&init_params);
    let mut state = GlobalState {
        documents: HashMap::new(),
        editor_settings,
        config_cache: HashMap::new(),
        config_errors: HashMap::new(),
        config_messages,
        // Matches the worker's freshly-built database, which starts out declaring
        // nothing; the mirror is only ever compared, never read for a parse.
        declarations: HashMap::new(),
        supports_pull_diagnostics,
        supports_diagnostic_refresh,
        supports_dynamic_watchers,
        next_request_id: 1,
        position_encoding: encoding,
        workspace_roots: workspace_roots(&init_params),
    };

    // Register on-disk watchers now: `lsp-server`'s `Connection::initialize` already
    // consumed the client's `initialized` notification, so we never see it as a
    // notification — the post-handshake point here is the LSP-legal place to fire
    // dynamic registrations.
    register_file_watchers(&connection, &mut state);

    // Inverse search, gated on the one client capability it needs. A client that
    // cannot `window/showDocument` never binds a socket, which is also why the
    // default test client is unaffected by any of this.
    let (ipc, ipc_rx) = ipc_channel(
        client_show_document_support(&init_params),
        &state.editor_settings,
        workspace_roots(&init_params),
    );

    let read_pool = TaskPool::new("meaning-lsp-read", read_pool_size());
    let (job_tx, job_rx) = unbounded::<WorkerJob>();
    let (out_tx, out_rx) = unbounded::<Outbound>();
    let worker = spawn_worker(
        job_rx,
        out_tx,
        read_pool.spawner(),
        encoding,
        state.workspace_roots.clone(),
    );

    loop {
        for message in state.config_messages.drain(..) {
            let _ = connection
                .sender
                .send(Message::Notification(Notification::new(
                    "window/showMessage".to_owned(),
                    serde_json::json!({"type": 1, "message": message}),
                )));
        }
        select! {
            recv(connection.receiver) -> msg => {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Request(req) => {
                        // `handle_shutdown` answers `shutdown` and waits for the
                        // following `exit`, returning `true` once both are seen.
                        if connection.handle_shutdown(&req)? {
                            break;
                        }
                        // Ahead of the handler, so the declarations governing the
                        // named document reach the worker before whatever job it
                        // sends on this same channel.
                        publish_declarations_for_request(&mut state, &req, &job_tx);
                        match req.method.as_str() {
                            method if method == DocumentFormattingRequest::METHOD.as_str() => {
                                on_formatting(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DocumentRangeFormattingRequest::METHOD.as_str() => {
                                on_range_formatting(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DocumentOnTypeFormattingRequest::METHOD.as_str() => {
                                on_type_formatting(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DocumentSymbolRequest::METHOD.as_str() => {
                                on_document_symbol(&connection, &mut state, &job_tx, req)
                            }
                            method if method == WorkspaceSymbolRequest::METHOD.as_str() => {
                                on_workspace_symbol(&connection, &job_tx, req)
                            }
                            method if method == CompletionRequest::METHOD.as_str() => {
                                on_completion(&connection, &mut state, &job_tx, req)
                            }
                            method if method == CompletionResolveRequest::METHOD.as_str() => {
                                on_completion_resolve(&connection, &job_tx, req)
                            }
                            method if method == HoverRequest::METHOD.as_str() => {
                                on_hover(&connection, &mut state, &job_tx, req)
                            }
                            method if method == ForwardSearchRequest::METHOD.as_str() => {
                                on_forward_search(&connection, &mut state, &job_tx, req)
                            }
                            method if method == SignatureHelpRequest::METHOD.as_str() => {
                                on_signature_help(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DefinitionRequest::METHOD.as_str() => {
                                on_goto_definition(&connection, &mut state, &job_tx, req)
                            }
                            method if method == ReferencesRequest::METHOD.as_str() => on_references(&connection, &state, &job_tx, req),
                            method if method == DocumentHighlightRequest::METHOD.as_str() => {
                                on_document_highlight(&connection, &state, &job_tx, req)
                            }
                            method if method == PrepareRenameRequest::METHOD.as_str() => {
                                on_prepare_rename(&connection, &state, &job_tx, req)
                            }
                            method if method == RenameRequest::METHOD.as_str() => on_rename(&connection, &state, &job_tx, req),
                            method if method == FoldingRangeRequest::METHOD.as_str() => {
                                on_folding_range(&connection, &state, &job_tx, req)
                            }
                            method if method == SelectionRangeRequest::METHOD.as_str() => {
                                on_selection_range(&connection, &state, &job_tx, req)
                            }
                            method if method == DocumentLinkRequest::METHOD.as_str() => {
                                on_document_link(&connection, &mut state, &job_tx, req)
                            }
                            method if method == CodeActionRequest::METHOD.as_str() => {
                                on_code_action(&connection, &mut state, &job_tx, req)
                            }
                            method if method == ExecuteCommandRequest::METHOD.as_str() => {
                                on_execute_command(&connection, &state, &job_tx, req)
                            }
                            method if method == DocumentDiagnosticRequest::METHOD.as_str() => {
                                on_document_diagnostic(&connection, &mut state, &job_tx, req)
                            }
                            _ => respond_unhandled(&connection, req),
                        }
                    }
                    Message::Notification(not) => {
                        on_notification(&connection, &mut state, &job_tx, not);
                    }
                    // Server-initiated requests (watcher registration,
                    // `workspace/applyEdit`) are fire-and-forget, so the client's
                    // response needs no action.
                    Message::Response(_) => {}
                }
            }
            recv(out_rx) -> outbound => {
                let Ok(outbound) = outbound else { continue };
                forward_outbound(&connection, &mut state, &job_tx, outbound);
            }
            recv(ipc_rx) -> msg => {
                let Ok(msg) = msg else { continue };
                on_inverse_search(&connection, &mut state, msg);
            }
        }
    }

    // The accept thread is parked in a blocking `accept`, which dropping a
    // channel cannot wake, and `serve` runs *in-process* in the LSP test binary —
    // so a detached listener would hold a bound socket for the whole run. Dial
    // ourselves once, join, and let `Listener`'s `Drop` unlink both nodes.
    if let Some(ipc) = ipc {
        ipc.listener.wake();
        let _ = ipc.thread.join();
    }

    // Dropping `job_tx` disconnects the worker's receiver so it exits; the read
    // pool's workers exit when `read_pool` drops at the end of this scope.
    drop(job_tx);
    let _ = worker.join();
    Ok(())
}

/// Forward a worker result to the client. Diagnostics are version-gated: a result
/// is sent only when its document is still open at exactly that version, so a
/// stale (superseded or post-close) analyze never repaints squiggles.
pub(super) fn forward_outbound(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    outbound: Outbound,
) {
    match outbound {
        Outbound::Diagnostics {
            uri,
            version,
            diags,
        } => {
            // Pull and push are mutually exclusive: a pull-capable client is served
            // exclusively via `textDocument/diagnostic`, so drop the push (the
            // analyze still ran, warming the salsa memos the pull reads).
            if state.supports_pull_diagnostics {
                return;
            }
            if state
                .documents
                .get(&uri)
                .is_some_and(|doc| doc.version == version)
            {
                send_diagnostics(connection, uri, diags, Some(version));
            }
        }
        Outbound::Response(resp) => {
            let _ = connection.sender.send(Message::Response(resp));
        }
        Outbound::ApplyEdit { id, label, edit } => {
            let params = ApplyWorkspaceEditParams {
                metadata: None,
                label: Some(label),
                edit,
            };
            if let Ok(params) = serde_json::to_value(params) {
                let request_id = state.next_request_id;
                state.next_request_id += 1;
                let _ = connection.sender.send(Message::Request(Request {
                    id: RequestId::from(request_id),
                    method: ApplyWorkspaceEditRequest::METHOD.as_str().to_owned(),
                    params,
                }));
            }
            let _ = connection.sender.send(Message::Response(Response::new_ok(
                id,
                serde_json::Value::Null,
            )));
        }
        Outbound::RelintAll => relint_all_open(connection, state, job_tx),
    }
}
