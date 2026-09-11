//! Native responses responsibility.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_document_diagnostic(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    kind: FileKind,
    previous_result_id: Option<String>,
    rules: &RuleSelection,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        let items = compute_diagnostics(snapshot, path, kind, rules, enc);
        DocumentDiagnosticReportProgress::DocumentDiagnosticReport(diagnostic_report(
            items,
            previous_result_id.as_deref(),
        ))
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_code_action(
    snapshot: &Analysis,
    id: RequestId,
    uri: &Uri,
    path: &Path,
    kind: FileKind,
    range: Range,
    only: Option<&[CodeActionKind]>,
    rules: &RuleSelection,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_code_actions(snapshot, uri, path, kind, range, only, rules, enc)
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_format(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    sentence: SentenceOptions<'_>,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_format(snapshot, path, encoding, style, kind, sentence).map(|edit| vec![edit])
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_range_format(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    range: Range,
    sentence: SentenceOptions<'_>,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_range_format(snapshot, path, encoding, style, kind, range, sentence)
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_on_type_format(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    position: Position,
    sentence: SentenceOptions<'_>,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_on_type_format(snapshot, path, encoding, style, kind, position, sentence)
    });
}

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
fn respond<T: serde::Serialize>(
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

#[allow(clippy::too_many_arguments)]
pub(super) fn run_symbols(
    snapshot: &Analysis,
    id: RequestId,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    build: &BuildConfig,
    aux: &crate::aux::AuxCache,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        let symbols = if kind == FileKind::Bib {
            compute_bib_symbols(snapshot, path, encoding)
        } else {
            compute_symbols(
                snapshot,
                path,
                encoding,
                &NativeServices {
                    aux,
                    texmf: &InstalledPackages::default(),
                    build,
                },
            )
        };
        DocumentSymbolResponse::DocumentSymbolList(symbols)
    });
}

pub(super) fn run_workspace_symbols(
    snapshots: &[Analysis],
    id: RequestId,
    query: &str,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_projects_workspace_symbols(snapshots, query, enc)
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
    texmf: &InstalledPackages,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_document_link(
            snapshot,
            path,
            encoding,
            kind,
            &NativeServices {
                aux: &crate::aux::AuxCache::default(),
                texmf,
                build: &BuildConfig::default(),
            },
        )
    });
}

pub(super) fn run_completion(
    snapshot: &Analysis,
    id: RequestId,
    uri: &Uri,
    enc: PositionEncoding,
    position: Position,
    texmf: &InstalledPackages,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        CompletionResponse::CompletionList(CompletionList {
            apply_kind: None,
            item_defaults: None,
            is_incomplete: true,
            items: compute_completion(
                snapshot,
                uri,
                &uri_to_path(uri),
                enc,
                position,
                &NativeServices {
                    aux: &crate::aux::AuxCache::default(),
                    texmf,
                    build: &BuildConfig::default(),
                },
            ),
        })
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
    build: &BuildConfig,
    aux: &crate::aux::AuxCache,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        hover::compute_hover(
            snapshot,
            path,
            encoding,
            position,
            &NativeServices {
                aux,
                texmf: &InstalledPackages::default(),
                build,
            },
        )
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
/// with it, `Failure` is a signal the editor can turn into "build the document
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
    // A cancelled read leaves the root unresolved; fall back to the cursor's own
    // file rather than failing the request, since a single-file project resolves
    // to exactly that anyway.
    let root = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let resolution = snapshot.resolve_labels();
        root_document_of(snapshot, &namespace_of(resolution, path)).map(Path::to_path_buf)
    }))
    .ok()
    .flatten();
    let root = build
        .root
        .clone()
        .or(root)
        .unwrap_or_else(|| path.to_path_buf());

    let status = match forward_search::pdf_path(&root, build) {
        Some(pdf) if pdf.is_file() => {
            let target = forward_search::SearchTarget {
                tex: path.to_path_buf(),
                pdf,
                line,
            };
            forward_search::spawn_viewer(executable, args, &target)
        }
        Some(pdf) => {
            log::info!(
                "forward search: no PDF at {} (has the document been compiled?)",
                pdf.display()
            );
            ForwardSearchStatus::Failure
        }
        None => ForwardSearchStatus::Failure,
    };
    let result = serde_json::to_value(status.result()).unwrap_or(serde_json::Value::Null);
    let _ = out_tx.send(Outbound::Response(Response::new_ok(id, result)));
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
    texmf: &InstalledPackages,
    enc: PositionEncoding,
    out_tx: &Sender<Outbound>,
) {
    respond(id, out_tx, || {
        compute_goto_definition(
            snapshot,
            path,
            position,
            &NativeServices {
                aux: &crate::aux::AuxCache::default(),
                texmf,
                build: &BuildConfig::default(),
            },
            enc,
        )
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

/// Answer a `changeEnvironment` execute-command: push the begin/end name rewrite
/// via [`Outbound::ApplyEdit`], or reply with an error when no environment
/// encloses the cursor (an executed command should say why it did nothing, unlike
/// the `null`-on-miss document requests).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_change_environment(
    snapshot: &Analysis,
    id: RequestId,
    uri: &Uri,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
    new_name: &str,
    out_tx: &Sender<Outbound>,
) {
    match read_result(id.clone(), || {
        compute_change_environment(snapshot, path, enc, position)
    }) {
        Ok(Some((old_name, ranges))) => {
            let file = snapshot
                .lookup_file(path)
                .expect("environment source exists");
            let idx = snapshot.file_line_index(file, enc);
            let mut changes = HashMap::new();
            for range in ranges {
                push_edit(&mut changes, uri, &idx, range, new_name);
            }
            let edit = WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            };
            let label = format!("change environment: {old_name} -> {new_name}");
            let _ = out_tx.send(Outbound::ApplyEdit { id, label, edit });
        }
        Ok(None) => {
            let _ = out_tx.send(Outbound::Response(Response::new_err(
                id,
                ErrorCode::RequestFailed as i32,
                "no environment around the cursor".to_owned(),
            )));
        }
        Err(response) => {
            let _ = out_tx.send(Outbound::Response(response));
        }
    }
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
}
