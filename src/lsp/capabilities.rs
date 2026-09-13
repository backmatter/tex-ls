use super::*;

/// Read the client's diagnostic capabilities from the `initialize` params, as
/// `(supports_pull, supports_refresh)`. Pointer-walks the JSON (like
/// [`EditorSettings::from_client_value`]) rather than deserializing the whole
/// `ClientCapabilities`: pull support is the mere presence of
/// `capabilities.textDocument.diagnostic`; refresh support is
/// `capabilities.workspace.diagnostics.refreshSupport == true`.
pub(super) fn client_diagnostic_support(init_params: &serde_json::Value) -> (bool, bool) {
    let policy = tex_ls_protocol::ResponsePolicy::new(init_params);
    let supports_pull = policy.pull_diagnostics();
    let supports_refresh = policy.supports("/workspace/diagnostics/refreshSupport");
    (supports_pull, supports_refresh)
}

/// Whether the client supports dynamically registered file watchers, i.e.
/// `capabilities.workspace.didChangeWatchedFiles.dynamicRegistration == true`.
/// Pointer-walks the JSON like [`client_diagnostic_support`]. Without an acknowledged
/// client registration, the native polling worker observes backing-file changes.
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
pub(super) use tex_ls_protocol::projects::workspace_roots;

pub(super) fn native_capabilities(
    encoding: PositionEncoding,
    pull: bool,
) -> lsp_types::ServerCapabilities {
    let mut capabilities = tex_ls_protocol::server_capabilities(encoding, pull);
    if let Some(commands) = &mut capabilities.execute_command_provider {
        commands.commands.push("tex-ls.forwardSearch".into());
        commands.commands.push("tex-ls.inspectAcquisition".into());
    }
    capabilities
}
