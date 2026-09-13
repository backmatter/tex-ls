use super::*;

fn read_result<T>(id: RequestId, compute: impl FnOnce() -> T) -> Result<T, Response> {
    match std::panic::catch_unwind(AssertUnwindSafe(compute)) {
        Ok(result) => Ok(result),
        Err(payload) => {
            let (code, message) = match payload.downcast::<salsa::Cancelled>() {
                Ok(reason) => {
                    let code = match *reason {
                        salsa::Cancelled::PendingWrite | salsa::Cancelled::Local => {
                            ErrorCode::ContentModified
                        }
                        _ => ErrorCode::InternalError,
                    };
                    (code, reason.to_string())
                }
                Err(_) => {
                    log::error!("Language request {id:?} panicked");
                    (ErrorCode::InternalError, "Language request failed".into())
                }
            };
            Err(Response::new_err(id, code as i32, message))
        }
    }
}

/// Send one response and distinguish a cancelled read from an empty result.
pub(super) fn respond<T: serde::Serialize>(
    id: RequestId,
    out_tx: &Sender<Outbound>,
    compute: impl FnOnce() -> T,
) {
    let response = match read_result(id.clone(), compute) {
        Ok(result) => Response::new_ok(id, result),
        Err(response) => response,
    };
    let _ = out_tx.send(Outbound::Response(response));
}

/// Diagnostic pulls have an explicit retry contract for a superseding write.
/// Client cancellation is completed by the request ledger before this boundary.
pub(super) fn respond_diagnostic<T: serde::Serialize>(
    id: RequestId,
    out_tx: &Sender<Outbound>,
    compute: impl FnOnce() -> T,
) {
    let response = match read_result(id.clone(), compute) {
        Ok(result) => Response::new_ok(id, result),
        Err(mut response) => {
            if let Err(error) = &mut response.response_result
                && error.code == ErrorCode::ContentModified as i32
            {
                error.code = ErrorCode::ServerCancelled as i32;
                error.data = Some(serde_json::json!({"retriggerRequest": true}));
            }
            response
        }
    };
    let _ = out_tx.send(Outbound::Response(response));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_symbols(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    options: &tex_ls_protocol::presentation::OutlineOptions,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        let symbols = if kind == FileKind::Bib {
            compute_bib_symbols(snapshot, path, encoding)
        } else {
            compute_symbols(snapshot, path, encoding, options)
        };
        DocumentSymbolResponse::DocumentSymbolList(symbols)
    });
}

pub(super) fn run_workspace_symbols(
    snapshots: &[Analysis],
    id: RequestId,
    query: &str,
    enc: PositionEncoding,
    options: &[(PathBuf, tex_ls_protocol::presentation::OutlineOptions)],
    active: Option<&Path>,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_projects_workspace_symbols(
            snapshots,
            query,
            enc,
            &|path| {
                options
                    .iter()
                    .filter(|(scope, _)| path.starts_with(scope))
                    .max_by_key(|(scope, _)| scope.components().count())
                    .map(|(_, options)| options.clone())
                    .unwrap_or_default()
            },
            active,
        )
    });
}

pub(super) fn run_folding(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_folding(snapshot, path, encoding, kind)
    });
}

pub(super) fn run_selection_range(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    positions: &[Position],
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_selection_range(snapshot, path, encoding, kind, positions)
    });
}

pub(super) fn run_document_link(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_document_link(snapshot, path, encoding, kind)
    });
}

pub(super) fn run_completion(
    snapshot: &Analysis,
    id: RequestId,
    uri: &Uri,
    enc: PositionEncoding,
    position: Position,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        CompletionResponse::CompletionList(compute_completion(
            snapshot,
            uri,
            &uri_to_path(uri),
            enc,
            position,
        ))
    });
}

pub(super) fn run_completion_resolve(
    snapshot: &Analysis,
    id: RequestId,
    item: CompletionItem,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || completion_resolve::resolve(snapshot, item));
}

/// Describe the command/environment, `\cite` key, or `\label`/`\ref` key under the
/// cursor and reply with a [`Hover`] (or `null` when nothing resolves).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_hover(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    position: Position,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        hover::compute_hover(snapshot, path, encoding, position)
    });
}

/// Locate the cursor file's compiled PDF, launch the configured viewer at
/// `line`, and reply with the resulting status.
///
/// The document root is the first of: `[build] root`, the label namespace's
/// `\documentclass`/`\begin{document}` member ([`root_document_of`]), or the file
/// itself. `%f` stays the *cursor's* file throughout — SyncTeX indexes per input
/// file — while `%p` is the *root's* PDF, which is the whole reason the root
/// matters here.
///
/// The PDF must exist. Without that check a stale or never-built project would
/// launch a viewer onto nothing, and the client would have no way to say so;
/// with it, `missingPdf` is a signal the editor can turn into "build the document
/// first". Reading one directory entry is the same class of environment access
/// the `.aux` freshness check already performs.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_forward_search(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    line: u32,
    build: &BuildConfig,
    executable: &str,
    args: &[String],
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        let resolution = snapshot.resolve_labels();
        let root = if let Some(root) = &build.root {
            root.clone()
        } else {
            match resolution.candidate_roots(path) {
                [] => path.to_path_buf(),
                [root] => root.clone(),
                roots => {
                    return ForwardSearchOutcome::AmbiguousRoot {
                        candidates: roots.to_vec(),
                    };
                }
            }
        };
        match forward_search::pdf_path(&root, build) {
            Some(pdf) if pdf.is_file() => forward_search::spawn_viewer(
                executable,
                args,
                &forward_search::SearchTarget {
                    tex: path.to_path_buf(),
                    pdf,
                    line,
                },
            ),
            Some(path) => ForwardSearchOutcome::MissingPdf { path },
            None => ForwardSearchOutcome::UnsupportedSource,
        }
    });
}

/// Describe the command/environment whose argument the cursor is typing in and
/// reply with a `SignatureHelp` (or `null` when nothing resolves).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_signature_help(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    position: Position,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        signature_help::compute_signature_help(snapshot, path, position, enc)
    });
}

pub(super) fn run_goto_definition(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    position: Position,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_goto_definition(snapshot, path, position, enc)
    });
}

pub(super) fn run_references(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    position: Position,
    include_declaration: bool,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_references(snapshot, path, position, include_declaration, enc)
    });
}

pub(super) fn run_document_highlight(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_document_highlight(snapshot, path, enc, position)
    });
}

pub(super) fn run_prepare_rename(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_prepare_rename(snapshot, path, enc, position).map(|(range, placeholder)| {
            PrepareRenameResult::PrepareRenamePlaceholder(lsp_types::PrepareRenamePlaceholder {
                range,
                placeholder,
            })
        })
    });
}

pub(super) fn run_rename(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    position: Position,
    new_name: &str,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_rename(snapshot, path, position, new_name, enc)
    });
}

pub(super) fn run_linked_editing(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_linked_editing(snapshot, path, enc, position)
    });
}

/// Send a `publishDiagnostics` notification.
pub(super) fn send_diagnostics(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    let not = Notification::new(
        PublishDiagnosticsNotification::METHOD.as_str().to_owned(),
        params,
    );
    let _ = connection.sender.send(Message::Notification(not));
}

/// Reply to an unhandled request with a method-not-found error.
pub(super) fn respond_unhandled(connection: &Connection, req: Request) {
    let resp = Response::new_err(
        req.id,
        ErrorCode::MethodNotFound as i32,
        format!("unhandled request: {}", req.method),
    );
    let _ = connection.sender.send(Message::Response(resp));
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn a_panicking_request_sends_one_internal_error() {
        let (tx, rx) = unbounded();
        respond::<()>(RequestId::from(18), &tx, || panic!("request failure"));
        let Outbound::Response(response) = rx.try_recv().unwrap() else {
            panic!("expected response");
        };
        assert_eq!(response.id, RequestId::from(18));
        assert_eq!(
            response.response_result.unwrap_err().code,
            ErrorCode::InternalError as i32
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn cancelled_reads_send_one_error_instead_of_an_empty_success() {
        let (tx, rx) = unbounded();
        respond::<Vec<Diagnostic>>(RequestId::from(17), &tx, || {
            std::panic::resume_unwind(Box::new(salsa::Cancelled::PendingWrite))
        });
        let Outbound::Response(response) = rx.try_recv().unwrap() else {
            panic!("expected response");
        };
        assert_eq!(response.id, RequestId::from(17));
        assert_eq!(
            response.response_result.unwrap_err().code,
            ErrorCode::ContentModified as i32
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn diagnostic_write_cancellation_requests_retry_without_empty_success() {
        let (tx, rx) = unbounded();
        respond_diagnostic::<serde_json::Value>(RequestId::from(19), &tx, || {
            std::panic::resume_unwind(Box::new(salsa::Cancelled::PendingWrite))
        });
        let Outbound::Response(response) = rx.try_recv().unwrap() else {
            panic!("response");
        };
        let error = response.response_result.unwrap_err();
        assert_eq!(error.code, ErrorCode::ServerCancelled as i32);
        assert_eq!(
            error.data,
            Some(serde_json::json!({"retriggerRequest": true}))
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn diagnostic_panic_is_not_a_retryable_cancellation() {
        let (tx, rx) = unbounded();
        respond_diagnostic::<serde_json::Value>(RequestId::from(20), &tx, || panic!("failure"));
        let Outbound::Response(response) = rx.try_recv().unwrap() else {
            panic!("response");
        };
        let error = response.response_result.unwrap_err();
        assert_eq!(error.code, ErrorCode::InternalError as i32);
        assert!(error.data.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn workspace_diagnostic_retry_after_partial_batch_keeps_request_identity() {
        let (tx, rx) = unbounded();
        let mut db = IncrementalDatabase::default();
        let directory = tempfile::tempdir().unwrap();
        for n in 0..130 {
            db.apply_change(&directory.path().join(format!("{n:03}.tex")), "text", None);
        }
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let snapshot = db.snapshot().with_cancellation(cancelled.clone());
        let id = RequestId::from(21);
        respond_diagnostic(id.clone(), &tx, || {
            tex_ls_protocol::diagnostic_store::stream_workspace_diagnostics(
                &[snapshot],
                &serde_json::json!([]),
                PositionEncoding::Utf16,
                |_| RuleSelection::all(),
                |_| None,
                |batch| {
                    assert_eq!(batch.len(), 64);
                    tx.send(Outbound::Progress {
                        id: id.clone(),
                        token: serde_json::json!("partial"),
                        value: serde_json::json!({"items":batch}),
                    })
                    .unwrap();
                    cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                    true
                },
            );
            serde_json::json!({"items":[]})
        });
        assert!(
            matches!(rx.try_recv().unwrap(), Outbound::Progress { id: partial_id, .. } if partial_id == id)
        );
        let Outbound::Response(response) = rx.try_recv().unwrap() else {
            panic!("response");
        };
        assert_eq!(response.id, id);
        assert_eq!(
            response.response_result.unwrap_err().code,
            ErrorCode::ServerCancelled as i32
        );
        assert!(rx.try_recv().is_err());
        // A new pull can compute all reports after the interrupted snapshot drops.
        let retried = tex_ls_protocol::diagnostic_store::workspace_diagnostics(
            &[db.snapshot()],
            &serde_json::json!([]),
            PositionEncoding::Utf16,
            |_| RuleSelection::all(),
            |_| None,
        );
        assert_eq!(retried["items"].as_array().unwrap().len(), 130);
    }
}
