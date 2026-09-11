//! Native capabilities responsibility.
use super::*;

/// Pick the position encoding from the client's `general.positionEncodings`:
/// UTF-8 when offered (columns are then plain byte distances — no per-line
/// re-count), else the protocol-mandatory UTF-16 default (also the fallback for
/// a pre-3.17 client that sends no offer). Advertised back via
/// `ServerCapabilities::position_encoding` and honored by every [`LineIndex`]
/// conversion (each is built with [`LineIndex::with_encoding`]).
pub(super) fn negotiate_position_encoding(init_params: &serde_json::Value) -> PositionEncoding {
    let offers = init_params
        .get("capabilities")
        .and_then(|c| c.get("general"))
        .and_then(|g| g.get("positionEncodings"))
        .and_then(serde_json::Value::as_array);
    match offers {
        Some(list) if list.iter().any(|v| v.as_str() == Some("utf-8")) => PositionEncoding::Utf8,
        _ => PositionEncoding::Utf16,
    }
}

/// Read the client's diagnostic capabilities from the `initialize` params, as
/// `(supports_pull, supports_refresh)`. Pointer-walks the JSON (like
/// [`EditorSettings::from_client_value`]) rather than deserializing the whole
/// `ClientCapabilities`: pull support is the mere presence of
/// `capabilities.textDocument.diagnostic`; refresh support is
/// `capabilities.workspace.diagnostic.refreshSupport == true`.
pub(super) fn client_diagnostic_support(init_params: &serde_json::Value) -> (bool, bool) {
    let caps = init_params.get("capabilities");
    let supports_pull = caps
        .and_then(|c| c.get("textDocument"))
        .and_then(|t| t.get("diagnostic"))
        .is_some();
    let supports_refresh = caps
        .and_then(|c| c.get("workspace"))
        .and_then(|w| w.get("diagnostic"))
        .and_then(|d| d.get("refreshSupport"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    (supports_pull, supports_refresh)
}

/// Whether the client supports dynamically registered file watchers, i.e.
/// `capabilities.workspace.didChangeWatchedFiles.dynamicRegistration == true`.
/// Pointer-walks the JSON like [`client_diagnostic_support`]. When `false` we skip
/// registration and fall back to seed-on-open: on-disk edits to non-open includes go
/// unnoticed until something re-seeds the directory.
pub(super) fn client_watched_files_support(init_params: &serde_json::Value) -> bool {
    init_params
        .get("capabilities")
        .and_then(|c| c.get("workspace"))
        .and_then(|w| w.get("didChangeWatchedFiles"))
        .and_then(|d| d.get("dynamicRegistration"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Whether the client supports `window/showDocument`, i.e.
/// `capabilities.window.showDocument.support == true`. Pointer-walks the JSON
/// like [`client_diagnostic_support`].
///
/// This is the whole of inverse search's client contract, so the IPC listener is
/// bound only when it holds: a server that cannot reveal the position has no
/// business advertising itself to viewers and stealing the request from one that
/// can.
pub(super) fn client_show_document_support(init_params: &serde_json::Value) -> bool {
    init_params
        .get("capabilities")
        .and_then(|c| c.get("window"))
        .and_then(|w| w.get("showDocument"))
        .and_then(|s| s.get("support"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The workspace this server owns, as filesystem paths: the `workspaceFolders`,
/// falling back to the deprecated `rootUri`. Empty when the client opened a bare
/// file, which an inverse-search client reads as "will take anything".
pub(super) use meaning_protocol::projects::workspace_roots;

pub(super) fn native_capabilities(
    encoding: PositionEncoding,
    pull: bool,
) -> lsp_types::ServerCapabilities {
    let mut capabilities = meaning_protocol::server_capabilities(encoding, pull);
    capabilities.execute_command_provider = Some(lsp_types::ExecuteCommandOptions {
        commands: vec![CHANGE_ENVIRONMENT_COMMAND.to_owned()],
        work_done_progress_options: Default::default(),
    });
    capabilities.experimental = Some(serde_json::json!({"textDocumentForwardSearch": true}));
    capabilities
}

pub(super) const CHANGE_ENVIRONMENT_COMMAND: &str = "meaning.changeEnvironment";
