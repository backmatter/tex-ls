//! Native inverse search responsibility.
use super::*;

/// A bound inverse-search listener and the thread parked in its accept loop.
pub(super) struct IpcHandle {
    pub(super) listener: Arc<crate::ipc::Listener>,
    pub(super) thread: std::thread::JoinHandle<()>,
}

/// One accepted inverse-search request, on its way to the main loop.
pub(super) struct IpcMessage {
    pub(super) request: crate::ipc::InverseSearchRequest,
    pub(super) responder: crate::ipc::Responder,
}

/// Bind the inverse-search socket and park a thread on its accept loop, forwarding
/// each request to the main loop over `ipc_tx`.
///
/// `None` when the socket cannot be bound. Inverse search is a convenience, so a
/// server that cannot listen still serves everything else — the viewer-side
/// command is what reports the absence, and it says so in terms the user can act
/// on.
pub(super) fn spawn_ipc_listener(
    settings: &EditorSettings,
    roots: Vec<PathBuf>,
    ipc_tx: Sender<IpcMessage>,
) -> Option<IpcHandle> {
    let dir = settings
        .forward_search
        .ipc_dir
        .clone()
        .unwrap_or_else(crate::ipc::ipc_dir);
    let listener = Arc::new(crate::ipc::Listener::bind_in(&dir, roots)?);
    let accept = Arc::clone(&listener);
    let thread = std::thread::Builder::new()
        .name("meaning-lsp-ipc".to_owned())
        .spawn(move || {
            while let Some((request, responder)) = accept.accept_one() {
                if ipc_tx.send(IpcMessage { request, responder }).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(IpcHandle { listener, thread })
}

/// Build the inverse-search channel, or a receiver that can never become ready.
///
/// A disconnected receiver is always ready in `select!`; leaving one installed
/// when the client cannot show documents—or when binding the listener fails—
/// turns the main loop into a busy spin. `never()` preserves the disabled arm's
/// intended behavior without a dummy sender that could obscure listener failure.
pub(super) fn ipc_channel(
    enabled: bool,
    settings: &EditorSettings,
    roots: Vec<PathBuf>,
) -> (Option<IpcHandle>, Receiver<IpcMessage>) {
    if !enabled {
        return (None, never());
    }
    let (ipc_tx, ipc_rx) = unbounded();
    match spawn_ipc_listener(settings, roots, ipc_tx) {
        Some(ipc) => (Some(ipc), ipc_rx),
        None => (None, never()),
    }
}

/// Answer a viewer's inverse search: reveal `msg`'s source position in the editor.
///
/// Acceptance first, because the client is fanning out across every listening
/// server and a wrong "yes" strands the request here. A file counts as ours when
/// it is open, when it sits under one of our workspace roots, or when we have no
/// roots at all (a client that opened a bare file takes anything).
///
/// The responder is answered *before* the editor is told, in that order
/// deliberately: the viewer-side command blocks on the ack, and the integration
/// test drives both ends from one process.
pub(super) fn on_inverse_search(connection: &Connection, state: &mut GlobalState, msg: IpcMessage) {
    let IpcMessage { request, responder } = msg;
    let Some(uri) = path_to_uri(&request.path) else {
        responder.reject("not a representable path");
        return;
    };
    if !state.documents.contains_key(&uri) && !state.owns_path(&request.path) {
        responder.reject("outside this server's workspace");
        return;
    }
    responder.accept();

    // The wire is 1-based (SyncTeX's convention, and every viewer's); LSP is
    // 0-based. A viewer reporting line 0 would be out of contract, so saturate
    // rather than wrap.
    let position = Position::new(request.line.saturating_sub(1), request.character);
    let params = ShowDocumentParams {
        uri,
        external: Some(false),
        take_focus: Some(true),
        selection: Some(Range {
            start: position,
            end: position,
        }),
    };
    let Ok(params) = serde_json::to_value(params) else {
        return;
    };
    // Fire-and-forget, like `workspace/applyEdit`: the client's `{success}` reply
    // is swallowed by the main loop's `Message::Response` arm, since there is
    // nothing we would do differently on a `false`.
    let id = state.next_request_id;
    state.next_request_id += 1;
    let _ = connection.sender.send(Message::Request(Request {
        id: RequestId::from(id),
        method: ShowDocumentRequest::METHOD.as_str().to_owned(),
        params,
    }));
}
