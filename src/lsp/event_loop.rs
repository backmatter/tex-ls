use super::*;

struct EditContext {
    stamp: u64,
    discovery: u64,
    versions: HashMap<String, i32>,
}

/// The blocking message loop. Owns [`GlobalState`]; spawns the worker thread and
/// the read pool, then shuttles messages between the client and the workers.
/// `encoding` is the position encoding [`serve`] negotiated (and advertised) at
/// `initialize`.
pub(super) fn main_loop(
    connection: Connection,
    init_params: serde_json::Value,
    encoding: PositionEncoding,
) -> Result<(), DynError> {
    let transport = connection.sender;
    let (response_tx, response_rx) = unbounded();
    let client_connection = Connection {
        sender: response_tx.clone(),
        receiver: crossbeam_channel::never(),
    };
    let mut server_requests: HashMap<RequestId, String> = HashMap::new();
    let connection = Connection {
        sender: response_tx,
        receiver: connection.receiver,
    };
    let policy = tex_ls_protocol::ResponsePolicy::new(&init_params);
    let mut response_contexts = HashMap::new();
    let mut exited_without_shutdown = false;
    let mut lifecycle = tex_ls_protocol::lifecycle::Lifecycle::default();
    lifecycle
        .initialize()
        .expect("completed initialize handshake");
    let requests = Arc::new(std::sync::Mutex::new(request_ids::RequestIds::default()));
    let versioned_edits = init_params
        .pointer("/capabilities/workspace/workspaceEdit/documentChanges")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut edit_contexts: HashMap<RequestId, EditContext> = HashMap::new();
    let editor_settings = init_params
        .get("initializationOptions")
        .map(EditorSettings::from_client_value)
        .unwrap_or_else(|| Ok(EditorSettings::default()))
        .and_then(|settings| settings.with_workspace_roots(&workspace_roots(&init_params)));
    let (editor_settings, config_messages) = match editor_settings {
        Ok(settings) => (settings, Vec::new()),
        Err(error) => (EditorSettings::default(), vec![error]),
    };
    let (supports_pull_diagnostics, supports_diagnostic_refresh) =
        client_diagnostic_support(&init_params);
    let supports_dynamic_watchers = client_watched_files_support(&init_params);
    let mut state = GlobalState {
        refreshes: refresh::RefreshRequests::new(&policy),
        diagnostic_stamp: 0,
        discovery_stamp: 0,
        active_path: None,
        supports_inlay_refresh: policy.supports("/workspace/inlayHint/refreshSupport"),
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
        watcher_acknowledged: false,
        artifact_watches: HashMap::new(),
        scoped_editor_settings: HashMap::new(),
        pending_configuration: HashMap::new(),
        configuration_generation: 0,
        position_encoding: encoding,
        workspace_roots: workspace_roots(&init_params),
    };

    // Register on-disk watchers now: `lsp-server`'s `Connection::initialize` already
    // consumed the client's `initialized` notification, so we never see it as a
    // notification — the post-handshake point here is the LSP-legal place to fire
    // dynamic registrations.
    register_file_watchers(&connection, &mut state);
    if policy.supports("/workspace/configuration") {
        request_configuration(&connection, &mut state);
    }

    // Inverse search, gated on the one client capability it needs. A client that
    // cannot `window/showDocument` never binds a socket, which is also why the
    // default test client is unaffected by any of this.
    let (ipc, ipc_rx) = ipc_channel(
        client_show_document_support(&init_params),
        &state.editor_settings,
        workspace_roots(&init_params),
    );

    let read_pool = TaskPool::new("tex-ls-lsp-read", read_pool_size());
    let (job_tx, job_rx) = unbounded::<WorkerJob>();
    let (out_tx, out_rx) = unbounded::<Outbound>();
    let worker = spawn_worker(
        job_rx,
        out_tx,
        read_pool.spawner(),
        encoding,
        state.workspace_roots.clone(),
        requests.clone(),
    );

    let mut watcher = watching::FallbackWatcher::new();
    let mut loading = loading::LoadingProgress::new(policy.supports("/window/workDoneProgress"));
    let progress_tick = crossbeam_channel::tick(std::time::Duration::from_millis(100));
    loop {
        let roots = if state.workspace_roots.is_empty() {
            state
                .documents
                .keys()
                .filter_map(uri_to_fs_path)
                .filter_map(|path| path.parent().map(Path::to_path_buf))
                .collect()
        } else {
            state.workspace_roots.clone()
        };
        // Client acknowledgement does not guarantee events for editor-excluded
        // build directories. Explicit artifact candidates always remain native-owned.
        let artifacts = state.artifact_watches.values().flatten().cloned().collect();
        watcher.update(
            if state.watcher_acknowledged {
                Vec::new()
            } else {
                roots
            },
            artifacts,
        );
        for message in state.config_messages.drain(..) {
            let _ = connection
                .sender
                .send(Message::Notification(Notification::new(
                    "window/showMessage".to_owned(),
                    serde_json::json!({"type": 1, "message": message}),
                )));
        }
        select! {
            recv(progress_tick) -> _ => {
                loading.poll(&transport);
                if lifecycle.require_running().is_ok() {
                    state.refreshes.flush(&connection, &mut state.next_request_id);
                }
            },
            recv(watcher.events) -> events => {
                if let Ok(changes) = events {
                    // Queued transition events are harmless: backing replacement
                    // compares source revisions and preserves editor overlays.
                    state.diagnostic_stamp += 1;
                    on_watched_files_change(&connection, &mut state, &job_tx, DidChangeWatchedFilesParams { changes });
                }
            }
            recv(connection.receiver) -> msg => {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Request(mut req) => {
                        if let Some(uri) = req.params.pointer("/textDocument/uri").and_then(|value| serde_json::from_value::<Uri>(value.clone()).ok()).and_then(|uri| uri_to_fs_path(&uri)) { state.active_path = Some(uri); }
                        let Some(internal) = requests.lock().expect("request ledger").receive(req.id.clone()) else {
                            // Duplicate active wire IDs cannot identify two responses. Keep
                            // the original request and discard the malformed duplicate.
                            log::warn!("Ignoring duplicate active request ID {}", req.id);
                            continue;
                        };
                        response_contexts.insert(internal.clone(), (req.method.clone(), req.params.pointer("/textDocument/uri").cloned()));
                        req.id = internal;
                        if lifecycle.require_running().is_err() {
                            let _ = connection.sender.send(Message::Response(Response::new_err(req.id, ErrorCode::InvalidRequest as i32, "Session is shutting down".into())));
                            continue;
                        }
                        if req.method == "shutdown" {
                            lifecycle.shutdown().expect("running session");
                            let original = requests.lock().expect("request ledger").complete(&req.id).expect("shutdown request");
                            edit_contexts.clear();
                            response_contexts.clear();
                            for id in requests.lock().expect("request ledger").drain() {
                                edit_contexts.remove(&id);
                                let _ = transport.send(Message::Response(Response::new_err(id, ErrorCode::RequestCanceled as i32, "Session is shutting down".into())));
                            }
                            let _ = transport.send(Message::Response(Response::new_ok(original, serde_json::Value::Null)));
                            continue;
                        }
                        // Ahead of the handler, so the declarations governing the
                        // named document reach the worker before whatever job it
                        // sends on this same channel.
                        publish_declarations_for_request(&mut state, &req, &job_tx);
                        if matches!(req.method.as_str(), "workspace/willRenameFiles" | "textDocument/rename" | "textDocument/codeAction" | "textDocument/colorPresentation") {
                            edit_contexts.insert(req.id.clone(), EditContext { stamp: state.diagnostic_stamp, discovery: state.discovery_stamp, versions: state.documents.iter().map(|(uri, doc)| (uri.as_str().to_owned(), doc.version)).collect() });
                        }
                        match req.method.as_str() {
                            "workspace/willRenameFiles" => {
                                if let Some(files) = tex_ls_protocol::file_operations::decode_files(&req.params) {
                                    let _ = job_tx.send(WorkerJob::WillRenameFiles { id: req.id, files });
                                } else {
                                    let _ = connection.sender.send(Message::Response(Response::new_err(req.id, ErrorCode::InvalidParams as i32, "Invalid rename files".into())));
                                }
                            }
                            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/range" => on_semantic_tokens(&connection, &state, &job_tx, req),
                            "textDocument/linkedEditingRange" => on_linked_editing(&connection, &state, &job_tx, req),
                            method if method == DocumentFormattingRequest::METHOD.as_str() => {
                                on_formatting(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DocumentRangeFormattingRequest::METHOD.as_str() || method == "textDocument/rangesFormatting" => {
                                on_range_formatting(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DocumentOnTypeFormattingRequest::METHOD.as_str() => {
                                on_type_formatting(&connection, &mut state, &job_tx, req)
                            }
                            method if method == DocumentSymbolRequest::METHOD.as_str() => {
                                on_document_symbol(&connection, &mut state, &job_tx, req)
                            }
                            method if method == WorkspaceSymbolRequest::METHOD.as_str() => {
                                on_workspace_symbol(&connection, &mut state, &job_tx, req)
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
                            "textDocument/documentColor" | "textDocument/colorPresentation" => on_colors(&connection, &state, &job_tx, req),
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
                                on_execute_command(&connection, &mut state, &job_tx, req)
                            }
                            "textDocument/inlayHint" => on_inlay_hints(&connection, &mut state, &job_tx, req),
                            "workspace/diagnostic" => {
                                let params = match serde_json::from_value::<lsp_types::WorkspaceDiagnosticParams>(req.params) {
                                    Ok(params) => params,
                                    Err(_) => {
                                        let _ = connection.sender.send(Message::Response(Response::new_err(req.id, ErrorCode::InvalidParams as i32, "Invalid workspace diagnostic params".into())));
                                        continue;
                                    }
                                };
                                let previous = serde_json::to_value(params.previous_result_ids).expect("result IDs serialize");
                                let partial = params.partial_result_params.partial_result_token.map(|token| serde_json::to_value(token).expect("progress token serializes"));
                                let _ = job_tx.send(WorkerJob::WorkspaceDiagnostic { id: req.id, previous, partial });
                            }
                            method if method == DocumentDiagnosticRequest::METHOD.as_str() => {
                                on_document_diagnostic(&connection, &mut state, &job_tx, req)
                            }
                            _ => respond_unhandled(&connection, req),
                        }
                    }
                    Message::Notification(not) => {
                        if let Some(uri) = not.params.pointer("/textDocument/uri").and_then(|value| serde_json::from_value::<Uri>(value.clone()).ok()).and_then(|uri| uri_to_fs_path(&uri)) { state.active_path = Some(uri); }
                        if not.method == "exit" {
                            exited_without_shutdown = lifecycle.require_running().is_ok();
                            lifecycle.exit();
                            break;
                        }
                        if not.method == "$/cancelRequest" {
                            if let Some(id) = not.params.get("id").and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok())
                                && let Some(internal) = requests.lock().expect("request ledger").cancel(&id) {
                                edit_contexts.remove(&internal);
                                response_contexts.remove(&internal);
                                let _ = transport.send(Message::Response(Response::new_err(id, ErrorCode::RequestCanceled as i32, "Request cancelled".into())));
                            }
                            continue;
                        }
                        if lifecycle.require_running().is_err() { continue; }
                        if matches!(not.method.as_str(), "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didClose" | "workspace/didChangeConfiguration" | "workspace/didChangeWatchedFiles" | "workspace/didChangeWorkspaceFolders" | "workspace/didRenameFiles") {
                            state.diagnostic_stamp += 1;
                        }
                        let folders_changed = not.method == "workspace/didChangeWorkspaceFolders";
                        let refresh_configuration = not.method == "workspace/didChangeConfiguration" && policy.supports("/workspace/configuration");
                        if refresh_configuration {
                            request_configuration(&connection, &mut state);
                        } else {
                            on_notification(&client_connection, &mut state, &job_tx, not);
                            if folders_changed && policy.supports("/workspace/configuration") { request_configuration(&connection, &mut state); }
                        }
                    }
                    Message::Response(response) => {
                        if loading.response(&response, &transport) { continue; }
                        let Some(method) = server_requests.remove(&response.id) else { continue; };
                        handle_client_response(&connection, &mut state, &job_tx, &method, response);
                    }
                }
            }
            recv(response_rx) -> message => {
                if let Ok(mut message) = message {
                    if let Message::Response(response) = &mut message {
                        let Some(original) = requests.lock().expect("request ledger").complete(&response.id) else { continue };
                        if let Some(EditContext { stamp, versions, .. }) = edit_contexts.remove(&response.id) {
                            if stamp != state.diagnostic_stamp && response.response_result.is_ok() {
                                *response = Response::new_err(response.id.clone(), ErrorCode::ContentModified as i32, "Edit source context changed".into());
                            } else if versioned_edits && let Ok(result) = &mut response.response_result {
                                tex_ls_protocol::workspace_edits::attach_versions(result, &versions);
                            }
                        }
                        if let Some((method, uri)) = response_contexts.remove(&response.id)
                            && let Ok(result) = &mut response.response_result {
                            policy.response(&method, uri.as_ref(), result);
                        }
                        response.id = original;
                    }
                    match &mut message {
                        Message::Request(request) => { server_requests.insert(request.id.clone(), request.method.clone()); }
                        Message::Notification(notification) => policy.response(&notification.method, None, &mut notification.params),
                        _ => {}
                    }
                    let _ = transport.send(message);
                }
            }
            recv(out_rx) -> outbound => {
                let Ok(outbound) = outbound else { continue };
                if let Outbound::ReadStarted { id } = outbound {
                    if let Some(EditContext { stamp, discovery, versions }) = edit_contexts.get_mut(&id)
                        && only_discovery_advanced(*stamp, *discovery, state.diagnostic_stamp, state.discovery_stamp)
                        && versions.len() == state.documents.len()
                        && state.documents.iter().all(|(uri, doc)| versions.get(uri.as_str()) == Some(&doc.version)) {
                        *stamp = state.diagnostic_stamp;
                        *discovery = state.discovery_stamp;
                    }
                } else if let Outbound::Loading(active) = outbound {
                    loading.update(active, &transport);
                } else if let Outbound::Progress { id, token, mut value } = outbound {
                    if requests.lock().expect("request ledger").contains(&id) {
                        policy.response("workspace/diagnostic", None, &mut value);
                        let _ = transport.send(Message::Notification(Notification::new("$/progress".into(), serde_json::json!({"token":token,"value":value}))));
                    }
                } else {
                    forward_outbound(&connection, &client_connection, &mut state, &job_tx, outbound);
                }
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
    if exited_without_shutdown {
        return Err("exit received before shutdown".into());
    }
    Ok(())
}

/// Forward a worker result to the client. Diagnostics are version-gated: a result
/// is sent only when its document is still open at exactly that version, so a
/// stale (superseded or post-close) analyze never repaints squiggles.
pub(super) fn forward_outbound(
    connection: &Connection,
    client_connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    outbound: Outbound,
) {
    match outbound {
        Outbound::Refresh(feature) => state.refreshes.invalidate(feature),
        Outbound::WatchArtifacts { source, paths } => {
            state.artifact_watches.insert(source, paths);
        }
        Outbound::WorkspaceDiagnosticSettings(request) => {
            let settings = request
                .paths
                .iter()
                .filter_map(|path| {
                    let uri = path_to_uri(path)?;
                    let version = state.documents.get(&uri).map(|document| document.version);
                    Some((path.clone(), (state.resolve_settings(&uri), version)))
                })
                .collect();
            let _ = job_tx.send(WorkerJob::WorkspaceDiagnosticReport { request, settings });
        }
        Outbound::Diagnostics {
            stamp,
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
            if stamp != state.diagnostic_stamp {
                relint_all_open(connection, state, job_tx);
                return;
            }
            if state
                .documents
                .get(&uri)
                .is_some_and(|doc| doc.version == version)
            {
                send_diagnostics(client_connection, uri, diags, Some(version));
            }
        }
        Outbound::ReadStarted { .. } | Outbound::Loading(_) | Outbound::Progress { .. } => {
            unreachable!("progress handled by request boundary")
        }
        Outbound::Response(resp) => {
            let _ = connection.sender.send(Message::Response(resp));
        }
        Outbound::RelintAll => {
            // Discovery may change backing sources without a client notification.
            // Reject diagnostics captured before that externally acquired state.
            state.diagnostic_stamp += 1;
            state.discovery_stamp += 1;
            relint_all_open(connection, state, job_tx);
        }
    }
}

/// Worker discovery notifications precede ReadStarted on the same FIFO channel.
/// Client/configuration/watcher changes use independent stamp advances and cannot
/// be absorbed by a delayed admission notification, even with identical versions.
fn only_discovery_advanced(
    captured: u64,
    captured_discovery: u64,
    current: u64,
    discovery: u64,
) -> bool {
    current - captured == discovery - captured_discovery
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn delayed_read_admission_does_not_accept_new_configuration_or_backing_state() {
        assert!(only_discovery_advanced(5, 2, 7, 4));
        // Same editor versions, but a watched-file/configuration update arrived
        // after capture and before the main loop received ReadStarted.
        assert!(!only_discovery_advanced(5, 2, 8, 4));
        assert!(!only_discovery_advanced(5, 2, 6, 2));
        assert!(only_discovery_advanced(5, 2, 5, 2));
    }
}
