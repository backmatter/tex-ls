//! Native synchronization responsibility.
use super::*;

/// Route a notification: edits and lifecycle to the worker, config inline.
pub(super) fn on_notification(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    not: Notification,
) {
    match not.method.as_str() {
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
            let resolved = state.analysis_settings(&uri);
            let _ = job_tx.send(WorkerJob::Edit {
                path,
                uri,
                text,
                version: doc.version,
                opened: true,
                kind,
                rules: resolved.rule_selection(),
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
            let resolved = state.analysis_settings(&uri);
            let _ = job_tx.send(WorkerJob::Edit {
                path,
                uri,
                text,
                version,
                opened: false,
                kind,
                rules: resolved.rule_selection(),
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
                // next request. A discovered `meaning.toml` still wins, so docs in a
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
pub(super) const WATCHED_FILES_REGISTRATION_ID: &str = "meaning-watched-files";

/// Dynamically register file watchers for the project's on-disk leaves
/// (`**/*.{tex,bib,sty,cls,dtx,ins}`) and the config file (`meaning.toml`), so out-of-editor edits to
/// non-open includes reanalyze open documents. Called once right after the initialize
/// handshake. A no-op when the client lacks
/// `didChangeWatchedFiles.dynamicRegistration` (we then rely on seed-on-open). The
/// client's response is fire-and-forget — the main loop ignores it.
pub(super) fn register_file_watchers(connection: &Connection, state: &mut GlobalState) {
    if !state.supports_dynamic_watchers {
        return;
    }
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![
            FileSystemWatcher {
                glob_pattern: GlobPattern::Pattern("**/*.{tex,bib,sty,cls,dtx,ins}".to_owned()),
                kind: None,
            },
            FileSystemWatcher {
                glob_pattern: GlobPattern::Pattern("**/meaning.toml".to_owned()),
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
        id: RequestId::from(id),
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
        if path.file_name().is_some_and(|name| name == "meaning.toml") {
            config_changed = true;
        } else {
            let _ = job_tx.send(WorkerJob::WatchedChange {
                path,
                deleted: event.kind == FileChangeType::Deleted,
            });
        }
    }
    if config_changed {
        // A discovered `meaning.toml` changed on disk: drop cached resolutions so the
        // next analyze re-reads it, then re-lint open docs (mirrors
        // `didChangeConfiguration`, plus the relint a fresh config implies).
        state.invalidate_settings();
        relint_all_open(connection, state, job_tx);
    }
}
