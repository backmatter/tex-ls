use super::*;

/// An open document buffer: its current text and the version it is at.
///
/// The text is an [`Arc<TextBuffer>`] rather than a `String` because everything
/// downstream of an edit only reads it: the worker job, the salsa input, and
/// every read job the same keystroke fires. Capturing the buffer for one of
/// those is a refcount bump, and they share the [`LineIndex`] the first of them
/// builds.
pub(super) struct Document {
    pub(super) text: Arc<TextBuffer>,
    pub(super) version: i32,
}

/// The main loop's state: open-document buffers and the client's editor settings.
/// Holds no database — the worker thread owns that.
pub(super) struct GlobalState {
    pub(super) refreshes: refresh::RefreshRequests,
    pub(super) diagnostic_stamp: u64,
    /// Subset of diagnostic stamp advances published by the worker IO queue.
    pub(super) discovery_stamp: u64,
    pub(super) active_path: Option<PathBuf>,
    pub(super) supports_inlay_refresh: bool,
    pub(super) documents: HashMap<Uri, Document>,
    pub(super) editor_settings: EditorSettings,
    /// Per-document config resolutions, keyed by the document's **anchor directory**
    /// (its parent). A discovered `tex-ls.toml` is authoritative; editor settings
    /// are the fallback. Each entry carries a filesystem fingerprint so clients that
    /// cannot register watched files still notice config changes on normal activity.
    /// Entries are invalidated on `didChangeConfiguration` while retaining their
    /// last valid values for error recovery.
    pub(super) config_cache: HashMap<PathBuf, CachedSettings>,
    pub(super) config_errors: HashMap<PathBuf, String>,
    pub(super) config_messages: Vec<String>,
    /// Last declarations published for each source. Equal values avoid redundant
    /// tracked-input writes and preserve the source's cached queries.
    pub(super) declarations: HashMap<PathBuf, Arc<ResolvedDeclarations>>,
    /// The client advertised `textDocument/diagnostic` pull support, so we serve
    /// diagnostics pull-only and **suppress** the `publishDiagnostics` push (the two
    /// are mutually exclusive).
    pub(super) supports_pull_diagnostics: bool,
    /// The client advertised `workspace.diagnostics.refreshSupport`, so a cross-file
    /// change can nudge it to re-pull via `workspace/diagnostic/refresh` (the pull
    /// analog of the push path's `RelintAll`).
    pub(super) supports_diagnostic_refresh: bool,
    /// The client advertised `workspace.didChangeWatchedFiles.dynamicRegistration`, so
    /// on `initialized` we register watchers for `**/*.{tex,bib,sty,cls,dtx,ins}` and `tex-ls.toml`
    /// and reanalyze on on-disk edits to non-open project files.
    pub(super) supports_dynamic_watchers: bool,
    /// Monotonic id for server→client requests (e.g. `workspace/diagnostic/refresh`,
    /// `client/registerCapability`). Namespaced from the client's request ids, so they
    /// never collide.
    pub(super) next_request_id: u64,
    pub(super) watcher_acknowledged: bool,
    pub(super) artifact_watches: HashMap<PathBuf, Vec<PathBuf>>,
    pub(super) scoped_editor_settings: HashMap<PathBuf, EditorSettings>,
    pub(super) pending_configuration: HashMap<RequestId, Vec<PathBuf>>,
    pub(super) configuration_generation: u64,

    /// The position encoding negotiated at `initialize` (see
    /// [`negotiate_position_encoding`]), governing every `Position` ↔ byte-offset
    /// conversion on the main loop (`didChange` splicing).
    pub(super) position_encoding: PositionEncoding,
    /// The workspace folders this server was started on (see [`workspace_roots`]),
    /// used to decide which inverse searches are ours. Empty when the client
    /// opened a bare file, which means "anything".
    pub(super) workspace_roots: Vec<PathBuf>,
}

impl GlobalState {
    /// Whether `path` falls inside this server's workspace. A server with no
    /// roots claims everything — it has no basis to decline, and declining would
    /// leave the request unanswered by anyone.
    pub(super) fn owns_path(&self, path: &Path) -> bool {
        self.workspace_roots.is_empty()
            || self
                .workspace_roots
                .iter()
                .any(|root| path.starts_with(root))
    }
}
