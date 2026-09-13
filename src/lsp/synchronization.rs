use super::*;

/// Route a notification: edits and lifecycle to the worker, config inline.
pub(super) fn on_notification(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    not: Notification,
) {
    match not.method.as_str() {
        "workspace/didRenameFiles" => {
            if let Some(files) = tex_ls_protocol::file_operations::decode_files(&not.params) {
                let moved: Vec<_> = files
                    .iter()
                    .filter_map(|(old, new)| {
                        let old_uri = path_to_uri(old)?;
                        let new_uri = path_to_uri(new)?;
                        Some((new_uri, state.documents.remove(&old_uri)?))
                    })
                    .collect();
                for (old, _) in &files {
                    state.declarations.remove(old);
                    state.artifact_watches.remove(old);
                    if !state.supports_pull_diagnostics
                        && let Some(uri) = path_to_uri(old)
                    {
                        send_diagnostics(connection, uri, Vec::new(), None);
                    }
                }
                state.documents.extend(moved);
                let _ = job_tx.send(WorkerJob::DidRenameFiles { files });
                state.invalidate_settings();
                relint_all_open(connection, state, job_tx);
            }
        }
        "workspace/didChangeWorkspaceFolders" => {
            let Ok(params) =
                serde_json::from_value::<lsp_types::DidChangeWorkspaceFoldersParams>(not.params)
            else {
                return;
            };
            let removed: Vec<_> = params
                .event
                .removed
                .iter()
                .filter_map(|folder| uri_to_fs_path(&folder.uri))
                .collect();
            state.workspace_roots.retain(|root| !removed.contains(root));
            state.workspace_roots.extend(
                params
                    .event
                    .added
                    .iter()
                    .filter_map(|folder| uri_to_fs_path(&folder.uri)),
            );
            state.workspace_roots.sort();
            state.workspace_roots.dedup();
            state.artifact_watches.retain(|source, _| {
                !removed.iter().any(|root| source.starts_with(root))
                    || state
                        .workspace_roots
                        .iter()
                        .any(|root| source.starts_with(root))
                    || path_to_uri(source).is_some_and(|uri| state.documents.contains_key(&uri))
            });
            state
                .scoped_editor_settings
                .retain(|root, _| state.workspace_roots.contains(root));
            state.invalidate_settings();
            let open = state.documents.keys().filter_map(uri_to_fs_path).collect();
            let _ = job_tx.send(WorkerJob::WorkspaceRoots {
                roots: state.workspace_roots.clone(),
                open,
            });
            relint_all_open(connection, state, job_tx);
        }

        method if method == DidOpenTextDocumentNotification::METHOD.as_str() => {
            let Ok(params) = not.extract::<DidOpenTextDocumentParams>(
                DidOpenTextDocumentNotification::METHOD.as_str(),
            ) else {
                return;
            };
            let doc = params.text_document;
            let uri = doc.uri;
            let text = Arc::new(TextBuffer::new(doc.text, state.position_encoding));
            state.documents.insert(
                uri.clone(),
                Document {
                    text: text.clone(),
                    version: doc.version,
                },
            );
            let path = uri_to_path(&uri);
            let kind = file_kind_for(&path);
            let texmf = state.client_settings(&uri).texmf.clone();
            let resolved = state.analysis_settings(&uri);
            let _ = job_tx.send(WorkerJob::Edit {
                stamp: state.diagnostic_stamp,
                path,
                uri,
                text,
                version: doc.version,
                opened: true,
                kind,
                rules: resolved.rule_selection(),
                build: Box::new(resolved.build.clone()),
                texmf,
                declarations: resolved.declarations.clone(),
                exclude: resolved.exclude,
                // A whole buffer arriving fresh: no transform to describe.
                edits: None,
            });
        }
        method if method == DidChangeTextDocumentNotification::METHOD.as_str() => {
            let Ok(params) = not.extract::<DidChangeTextDocumentParams>(
                DidChangeTextDocumentNotification::METHOD.as_str(),
            ) else {
                return;
            };
            let uri = params.text_document.text_document_identifier.uri;
            let version = params.text_document.version;
            let Some(doc) = state.documents.get_mut(&uri) else {
                return;
            };
            if version <= doc.version {
                log::warn!("Ignoring non-increasing document version for {uri}");
                return;
            }
            let edits = match apply_content_changes(&mut doc.text, params.content_changes) {
                Ok(edits) => edits,
                Err(error) => {
                    log::warn!("Ignoring invalid document update for {uri}: {error}");
                    return;
                }
            };
            doc.version = version;
            let text = doc.text.clone();
            let path = uri_to_path(&uri);
            let kind = file_kind_for(&path);
            let texmf = state.client_settings(&uri).texmf.clone();
            let resolved = state.analysis_settings(&uri);
            let _ = job_tx.send(WorkerJob::Edit {
                stamp: state.diagnostic_stamp,
                path,
                uri,
                text,
                version,
                opened: false,
                kind,
                rules: resolved.rule_selection(),
                build: Box::new(resolved.build.clone()),
                texmf,
                declarations: resolved.declarations.clone(),
                exclude: resolved.exclude,
                edits,
            });
        }
        method if method == DidCloseTextDocumentNotification::METHOD.as_str() => {
            let Ok(params) = not.extract::<DidCloseTextDocumentParams>(
                DidCloseTextDocumentNotification::METHOD.as_str(),
            ) else {
                return;
            };
            let uri = params.text_document.uri;
            state.documents.remove(&uri);
            state.declarations.remove(&uri_to_path(&uri));
            let _ = job_tx.send(WorkerJob::Close {
                path: uri_to_path(&uri),
            });
            // Clear stale squiggles immediately; the worker just evicts the file.
            // In pull mode there is nothing to clear — the client drops a closed
            // file's diagnostics itself by ceasing to pull — and we never push.
            if !state.supports_pull_diagnostics {
                send_diagnostics(connection, uri, Vec::new(), None);
            }
        }
        method if method == DidChangeConfigurationNotification::METHOD.as_str() => {
            if let Ok(params) = not.extract::<DidChangeConfigurationParams>(
                DidChangeConfigurationNotification::METHOD.as_str(),
            ) {
                match EditorSettings::from_client_value(&params.settings) {
                    Ok(settings) => state.editor_settings = settings,
                    Err(error) => {
                        state.config_messages.push(error);
                        return;
                    }
                }
                // Drop cached resolutions so the new fallback is picked up on the
                // next request. A discovered `tex-ls.toml` still wins, so docs in a
                // configured workspace are unaffected.
                state.invalidate_settings();
                relint_all_open(connection, state, job_tx);
            }
        }
        method if method == DidChangeWatchedFilesNotification::METHOD.as_str() => {
            if let Ok(params) = not.extract::<DidChangeWatchedFilesParams>(
                DidChangeWatchedFilesNotification::METHOD.as_str(),
            ) {
                on_watched_files_change(connection, state, job_tx, params);
            }
        }
        _ => {}
    }
}

/// The id under which we register the watched-files capability, reused to deregister.
pub(super) const WATCHED_FILES_REGISTRATION_ID: &str = "tex-ls-watched-files";

/// Dynamically register file watchers for the project's on-disk leaves
/// (`**/*.{tex,bib,bibtex,sty,cls,dtx,ins,def,lco,aux,log,fls}`) and the config file (`tex-ls.toml`), so out-of-editor edits to
/// non-open includes reanalyze open documents. Called once right after the initialize
/// handshake. A no-op when the client lacks
/// `didChangeWatchedFiles.dynamicRegistration` (native watching owns the scope). The
/// client's response is tracked by the main loop before watching is acknowledged.
pub(super) fn register_file_watchers(connection: &Connection, state: &mut GlobalState) {
    if !state.supports_dynamic_watchers {
        return;
    }
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![
            FileSystemWatcher {
                glob_pattern: GlobPattern::Pattern(
                    "**/*.{tex,bib,bibtex,sty,cls,dtx,ins,def,lco,aux,log,fls}".to_owned(),
                ),
                kind: None,
            },
            FileSystemWatcher {
                glob_pattern: GlobPattern::Pattern("**/tex-ls.toml".to_owned()),
                kind: None,
            },
        ],
    };
    let registration = Registration {
        id: WATCHED_FILES_REGISTRATION_ID.to_owned(),
        method: DidChangeWatchedFilesNotification::METHOD
            .as_str()
            .to_owned(),
        register_options: serde_json::to_value(options).ok(),
    };
    let params = RegistrationParams {
        registrations: vec![registration],
    };
    let Ok(params) = serde_json::to_value(params) else {
        return;
    };
    let id = state.next_request_id;
    state.next_request_id += 1;
    let _ = connection.sender.send(Message::Request(Request {
        id: RequestId::from(format!("tex-ls-server-{id}")),
        method: RegistrationRequest::METHOD.as_str().to_owned(),
        params,
    }));
}

/// Refresh backing files, including those hidden by open editor overlays.
pub(super) fn on_watched_files_change(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    params: DidChangeWatchedFilesParams,
) {
    let mut config_changed = false;
    for event in params.changes {
        let path = uri_to_path(&event.uri);
        if path.file_name().is_some_and(|name| name == "tex-ls.toml") {
            config_changed = true;
        } else {
            let exclude = state
                .resolve_settings(&event.uri)
                .exclude
                .with_force_exclude(true);
            let _ = job_tx.send(WorkerJob::WatchedChange {
                exclude,
                path,
                deleted: event.kind == FileChangeType::Deleted,
            });
        }
    }
    if config_changed {
        // A discovered `tex-ls.toml` changed on disk: drop cached resolutions so the
        // next analyze re-reads it, then re-lint open docs (mirrors
        // `didChangeConfiguration`, plus the relint a fresh config implies).
        state.invalidate_settings();
        relint_all_open(connection, state, job_tx);
    }
}

/// Pull workspace-scoped editor defaults. Replacement requests invalidate older
/// responses, while invalid values preserve the last valid scoped settings.
pub(super) fn request_configuration(connection: &Connection, state: &mut GlobalState) {
    state.configuration_generation += 1;
    state.pending_configuration.clear();
    let scopes = if state.workspace_roots.is_empty() {
        vec![PathBuf::new()]
    } else {
        state.workspace_roots.clone()
    };
    let items: Vec<_> = scopes
        .iter()
        .map(|scope| {
            let mut item = serde_json::json!({"section":"tex-ls"});
            if let Some(uri) = path_to_uri(scope) {
                item["scopeUri"] = serde_json::json!(uri);
            }
            item
        })
        .collect();
    let id = RequestId::from(format!(
        "tex-ls-configuration-{}",
        state.configuration_generation
    ));
    state.pending_configuration.insert(id.clone(), scopes);
    let _ = connection.sender.send(Message::Request(Request {
        id,
        method: "workspace/configuration".into(),
        params: serde_json::json!({"items":items}),
    }));
}
