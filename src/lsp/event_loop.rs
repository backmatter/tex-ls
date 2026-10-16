use crate::{
    lsp::{
        DynError,
        capabilities::{
            client_diagnostic_support, client_show_document_support, client_watched_files_support,
        },
        client_requests::handle_client_response,
        inverse_search::{ipc_channel, on_inverse_search},
        loading,
        messages::{Outbound, WorkerJob},
        refresh, request_ids,
        requests::{
            on_code_action, on_colors, on_completion, on_completion_resolve,
            on_document_diagnostic, on_document_highlight, on_document_link, on_document_symbol,
            on_execute_command, on_folding_range, on_formatting, on_goto_definition, on_hover,
            on_inlay_hints, on_linked_editing, on_prepare_rename, on_range_formatting,
            on_references, on_rename, on_selection_range, on_semantic_tokens, on_signature_help,
            on_type_formatting, on_workspace_symbol, relint_all_open,
        },
        responses::{respond_unhandled, send_diagnostics},
        settings::{EditorSettings, publish_declarations_for_request},
        state::GlobalState,
        synchronization::{
            on_notification, on_watched_files_change, register_file_watchers, request_configuration,
        },
        task_pool::{TaskPool, read_pool_size},
        watching,
        worker::spawn_worker,
    },
    text::PositionEncoding,
};
use crossbeam_channel::{Sender, select, unbounded};
use lsp_server::{Connection, ErrorCode, Message, Notification, RequestId, Response};
use lsp_types::{
    CodeActionRequest, CompletionRequest, CompletionResolveRequest, DefinitionRequest,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentDiagnosticRequest, DocumentFormattingRequest, DocumentHighlightRequest,
    DocumentLinkRequest, DocumentOnTypeFormattingRequest, DocumentRangeFormattingRequest,
    DocumentSymbolRequest, ExecuteCommandRequest, FoldingRangeRequest, HoverRequest,
    PrepareRenameRequest, ReferencesRequest, RenameRequest, Request as _, SelectionRangeRequest,
    SignatureHelpRequest, Uri, WorkspaceSymbolRequest,
};
use std::{collections::HashMap, path::Path, sync::Arc};
use tex_ls_protocol::{
    convert::{path_to_uri, uri_to_fs_path},
    projects::workspace_roots,
};

#[derive(Default)]
struct TokenDocumentLifetimes {
    next: u64,
    open: HashMap<String, u64>,
}
impl TokenDocumentLifetimes {
    fn observe(&mut self, method: &str, uri: &str) {
        match method {
            "textDocument/didOpen" => {
                self.next += 1;
                self.open.insert(uri.into(), self.next);
            }
            "textDocument/didClose" => {
                self.open.remove(uri);
            }
            _ => {}
        }
    }
    fn capture(&self, uri: &str) -> Option<u64> {
        self.open.get(uri).copied()
    }
    fn reset_uris(
        &mut self,
        uris: impl IntoIterator<Item = String>,
        is_open: impl Fn(&str) -> bool,
        store: &mut tex_ls_protocol::semantic_token_store::SemanticTokenStore,
    ) {
        let affected: std::collections::BTreeSet<_> = uris.into_iter().collect();
        for uri in &affected {
            self.open.remove(uri);
            store.forget(uri);
        }
        for uri in affected {
            if is_open(&uri) {
                self.observe("textDocument/didOpen", &uri);
            }
        }
    }
    fn response(
        &self,
        store: &mut tex_ls_protocol::semantic_token_store::SemanticTokenStore,
        method: &str,
        uri: &str,
        captured: Option<u64>,
        previous: Option<&str>,
        result: &mut serde_json::Value,
    ) {
        if !matches!(
            method,
            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta"
        ) {
            return;
        }
        if captured.is_some() && self.capture(uri) == captured {
            store.response(method, uri, previous, result);
        } else if self.capture(uri).is_none() {
            store.forget(uri);
        }
    }
}

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
    let mut semantic_tokens = tex_ls_protocol::semantic_token_store::SemanticTokenStore::default();
    let mut token_lifetimes = TokenDocumentLifetimes::default();
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
        artifact_watch_counts: HashMap::new(),
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
        if lifecycle.require_running().is_ok() {
            let inferred_roots = state.workspace_roots.is_empty();
            let roots = if inferred_roots {
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
            if state.watcher_acknowledged {
                watcher.update(Vec::new(), artifacts);
            } else if inferred_roots {
                watcher.update_inferred(roots, artifacts);
            } else {
                watcher.update(roots, artifacts);
            }
            for message in state.config_messages.drain(..) {
                let _ = connection
                    .sender
                    .send(Message::Notification(Notification::new(
                        "window/showMessage".to_owned(),
                        serde_json::json!({"type": 1, "message": message}),
                    )));
            }
        } else {
            watcher.update(Vec::new(), Vec::new());
        }
        select! {
            recv(progress_tick) -> _ => {
                if lifecycle.require_running().is_ok() {
                    loading.poll(&transport);
                    state.refreshes.flush(&connection, &mut state.next_request_id);
                }
            },
            recv(watcher.events) -> events => {
                if lifecycle.require_running().is_ok() && let Ok(changes) = events {
                    on_watched_files_change(&connection, &mut state, &job_tx, DidChangeWatchedFilesParams { changes });
                }
            }
            recv(connection.receiver) -> msg => {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Request(mut req) => {
                        if lifecycle.require_running().is_err() {
                            let _ = transport.send(Message::Response(Response::new_err(req.id, ErrorCode::InvalidRequest as i32, "Session is shutting down".into())));
                            continue;
                        }
                        if let Some(uri) = req.params.pointer("/textDocument/uri").and_then(|value| serde_json::from_value::<Uri>(value.clone()).ok()).and_then(|uri| uri_to_fs_path(&uri)) { state.active_path = Some(uri); }
                        let Some(internal) = requests.lock().expect("request ledger").receive(req.id.clone()) else {
                            // Duplicate active wire IDs cannot identify two responses. Keep
                            // the original request and discard the malformed duplicate.
                            log::warn!("Ignoring duplicate active request ID {}", req.id);
                            continue;
                        };
                        let response_uri = req.params.pointer("/textDocument/uri").cloned();
                        let token_lifetime = response_uri.as_ref().and_then(serde_json::Value::as_str).and_then(|uri| token_lifetimes.capture(uri));
                        response_contexts.insert(internal.clone(), (req.method.clone(), response_uri, req.params.get("previousResultId").and_then(serde_json::Value::as_str).map(str::to_owned), token_lifetime));
                        req.id = internal;
                        if req.method == "shutdown" {
                            lifecycle.shutdown().expect("running session");
                            let original = requests.lock().expect("request ledger").complete(&req.id).expect("shutdown request");
                            edit_contexts.clear();
                            response_contexts.clear();
                            semantic_tokens.clear();
                            server_requests.clear();
                            state.pending_configuration.clear();
                            state.config_messages.clear();
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
                            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta" | "textDocument/semanticTokens/range" => on_semantic_tokens(&connection, &state, &job_tx, req),
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
                                on_document_symbol(&connection, &mut state, &job_tx, req, &policy)
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
                        if matches!(not.method.as_str(), "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didClose" | "workspace/didChangeConfiguration" | "workspace/didChangeWorkspaceFolders" | "workspace/didRenameFiles") {
                            state.diagnostic_stamp += 1;
                        }
                        let folders_changed = not.method == "workspace/didChangeWorkspaceFolders";
                        let token_uris = match not.method.as_str() {
                            "textDocument/didOpen" => serde_json::from_value::<DidOpenTextDocumentParams>(not.params.clone()).ok().map(|params| vec![params.text_document.uri.as_str().to_owned()]),
                            "textDocument/didClose" => serde_json::from_value::<DidCloseTextDocumentParams>(not.params.clone()).ok().map(|params| vec![params.text_document.uri.as_str().to_owned()]),
                            "workspace/didRenameFiles" => tex_ls_protocol::file_operations::decode_files(&not.params).map(|files| files.into_iter().flat_map(|(old,new)| [path_to_uri(&old),path_to_uri(&new)]).flatten().map(|uri| uri.as_str().to_owned()).collect()),
                            _ => None,
                        };
                        let refresh_configuration = not.method == "workspace/didChangeConfiguration" && policy.supports("/workspace/configuration");
                        if refresh_configuration {
                            request_configuration(&connection, &mut state);
                        } else {
                            on_notification(&client_connection, &mut state, &job_tx, not);
                            if let Some(uris) = token_uris {
                                token_lifetimes.reset_uris(uris, |uri| uri.parse::<Uri>().is_ok_and(|uri| state.documents.contains_key(&uri)), &mut semantic_tokens);
                            }
                            if folders_changed && policy.supports("/workspace/configuration") { request_configuration(&connection, &mut state); }
                        }
                    }
                    Message::Response(response) => {
                        if lifecycle.require_running().is_err() { continue; }
                        if loading.response(&response, &transport) { continue; }
                        let Some(method) = server_requests.remove(&response.id) else { continue; };
                        handle_client_response(&connection, &mut state, &job_tx, &method, response);
                    }
                }
            }
            recv(response_rx) -> message => {
                if let Ok(mut message) = message {
                    if lifecycle.require_running().is_err() { continue; }
                    if let Message::Response(response) = &mut message {
                        let Some(original) = requests.lock().expect("request ledger").complete(&response.id) else { continue };
                        if let Some(EditContext { stamp, versions, .. }) = edit_contexts.remove(&response.id) {
                            if stamp != state.diagnostic_stamp && response.response_result.is_ok() {
                                *response = Response::new_err(response.id.clone(), ErrorCode::ContentModified as i32, "Edit source context changed".into());
                            } else if versioned_edits && let Ok(result) = &mut response.response_result {
                                tex_ls_protocol::workspace_edits::attach_versions(result, &versions);
                            }
                        }
                        if let Some((method, uri, previous, token_lifetime)) = response_contexts.remove(&response.id)
                            && let Ok(result) = &mut response.response_result {
                            policy.response(&method, uri.as_ref(), result);
                            if policy.supports("/textDocument/semanticTokens/requests/full/delta") && let Some(uri) = uri.as_ref().and_then(serde_json::Value::as_str) {
                                if uri.parse::<Uri>().is_ok_and(|uri| state.documents.contains_key(&uri)) { token_lifetimes.response(&mut semantic_tokens, &method, uri, token_lifetime, previous.as_deref(), result); }
                                else { semantic_tokens.forget(uri); }
                            }
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
                if lifecycle.require_running().is_err() { continue; }
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
                if lifecycle.require_running().is_err() {
                    msg.responder.reject("Session is shutting down");
                    continue;
                }
                on_inverse_search(&connection, &mut state, msg);
            }
        }
    }

    // Stop filesystem polling before waiting for the other session workers.
    drop(watcher);

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
            state.replace_artifact_watches(source, paths);
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

#[cfg(test)]
mod token_lifetime_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn renamed_swapped_and_overwritten_documents_receive_fresh_token_lifetimes() {
        let a = "file:///a.tex";
        let b = "file:///b.tex";
        for final_open in [vec![b], vec![a, b]] {
            let mut lifetimes = TokenDocumentLifetimes::default();
            let mut store = tex_ls_protocol::semantic_token_store::SemanticTokenStore::default();
            lifetimes.observe("textDocument/didOpen", a);
            lifetimes.observe("textDocument/didOpen", b);
            let old_a = lifetimes.capture(a);
            let old_b = lifetimes.capture(b);
            let mut full = json!({"data":[0,0,1,0,0]});
            lifetimes.response(
                &mut store,
                "textDocument/semanticTokens/full",
                b,
                old_b,
                None,
                &mut full,
            );
            let previous = full["resultId"].as_str().unwrap().to_owned();
            lifetimes.reset_uris(
                [a.into(), b.into(), b.into()],
                |uri| final_open.contains(&uri),
                &mut store,
            );
            assert_ne!(lifetimes.capture(b), old_b);
            assert_eq!(lifetimes.capture(a).is_some(), final_open.contains(&a));
            for (uri, captured) in [(a, old_a), (b, old_b)] {
                let mut late = json!({"data":[0,0,1,0,0]});
                lifetimes.response(
                    &mut store,
                    "textDocument/semanticTokens/full",
                    uri,
                    captured,
                    None,
                    &mut late,
                );
                assert!(late.get("resultId").is_none());
            }
            let mut fresh = json!({"data":[0,0,1,0,0]});
            lifetimes.response(
                &mut store,
                "textDocument/semanticTokens/full/delta",
                b,
                lifetimes.capture(b),
                Some(&previous),
                &mut fresh,
            );
            assert!(fresh.get("data").is_some());
            let id = fresh["resultId"].as_str().unwrap().to_owned();
            let mut late = json!({"data":[0,0,1,0,0]});
            lifetimes.response(
                &mut store,
                "textDocument/semanticTokens/full",
                b,
                old_b,
                None,
                &mut late,
            );
            let mut next = json!({"data":[0,0,1,0,0]});
            lifetimes.response(
                &mut store,
                "textDocument/semanticTokens/full/delta",
                b,
                lifetimes.capture(b),
                Some(&id),
                &mut next,
            );
            assert_eq!(next["edits"], json!([]));
        }
    }

    #[test]
    fn closed_affected_uris_do_not_create_phantom_open_lifetimes() {
        let mut lifetimes = TokenDocumentLifetimes::default();
        let mut store = tex_ls_protocol::semantic_token_store::SemanticTokenStore::default();
        lifetimes.reset_uris(["file:///closed.tex".into()], |_| false, &mut store);
        assert!(lifetimes.capture("file:///closed.tex").is_none());
    }
    #[test]
    fn delayed_tokens_do_not_restore_closed_or_reopened_baselines() {
        let mut lifetimes = TokenDocumentLifetimes::default();
        let mut store = tex_ls_protocol::semantic_token_store::SemanticTokenStore::default();
        let uri = "file:///tokens.tex";
        lifetimes.observe("textDocument/didOpen", uri);
        let old = lifetimes.capture(uri);
        let mut full = json!({"data":[0,0,1,0,0]});
        lifetimes.response(
            &mut store,
            "textDocument/semanticTokens/full",
            uri,
            old,
            None,
            &mut full,
        );
        let old_id = full["resultId"].as_str().unwrap().to_owned();
        lifetimes.observe("textDocument/didClose", uri);
        store.forget(uri);
        let mut late = json!({"data":[0,0,1,0,0]});
        lifetimes.response(
            &mut store,
            "textDocument/semanticTokens/full",
            uri,
            old,
            None,
            &mut late,
        );
        assert!(late.get("resultId").is_none());
        lifetimes.observe("textDocument/didOpen", uri);
        let mut late_reopened = json!({"data":[0,0,1,0,0]});
        lifetimes.response(
            &mut store,
            "textDocument/semanticTokens/full",
            uri,
            old,
            None,
            &mut late_reopened,
        );
        assert!(late_reopened.get("resultId").is_none());
        let mut fresh = json!({"data":[0,0,1,0,0]});
        lifetimes.response(
            &mut store,
            "textDocument/semanticTokens/full/delta",
            uri,
            lifetimes.capture(uri),
            Some(&old_id),
            &mut fresh,
        );
        assert!(fresh.get("data").is_some());
        assert!(fresh.get("resultId").is_some());
        let fresh_id = fresh["resultId"].as_str().unwrap().to_owned();
        let mut retired = json!({"data":[0,0,1,0,0]});
        lifetimes.response(
            &mut store,
            "textDocument/semanticTokens/full",
            uri,
            old,
            None,
            &mut retired,
        );
        let mut next = json!({"data":[0,0,1,0,0]});
        lifetimes.response(
            &mut store,
            "textDocument/semanticTokens/full/delta",
            uri,
            lifetimes.capture(uri),
            Some(&fresh_id),
            &mut next,
        );
        assert_eq!(next["edits"], json!([]));
    }
}
