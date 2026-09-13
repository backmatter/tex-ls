use super::*;

/// Source updates carry shared text; read jobs carry request descriptors.
pub(super) enum WorkerJob {
    InspectAcquisition {
        id: RequestId,
    },
    Colors {
        id: RequestId,
        path: PathBuf,
        presentation: Option<(Range, lsp_types::Color)>,
    },
    SemanticTokens {
        id: RequestId,
        path: PathBuf,
        range: Option<Range>,
    },
    WillRenameFiles {
        id: RequestId,
        files: Vec<(PathBuf, PathBuf)>,
    },
    DidRenameFiles {
        files: Vec<(PathBuf, PathBuf)>,
    },
    WorkspaceRoots {
        roots: Vec<PathBuf>,
        open: Vec<PathBuf>,
    },
    /// A buffer edit (from `didOpen` or `didChange`): write the full text into the
    /// db, then (re)analyze diagnostics.
    Edit {
        stamp: u64,
        uri: Uri,
        path: PathBuf,
        text: Arc<TextBuffer>,
        version: i32,
        opened: bool,
        kind: FileKind,
        declarations: Arc<ResolvedDeclarations>,
        /// The document's resolved lint-rule selection, applied to the analyze.
        rules: RuleSelection,
        build: Box<BuildConfig>,
        texmf: InstalledPackages,
        /// The document's resolved exclude filter, applied to sibling discovery
        /// ([`Worker::seed_dir`]). Built on the main side because the worker holds
        /// no config; exclude-nothing when no `tex-ls.toml` governs.
        exclude: ExcludeFilter,
        /// The exact transform from the text the db currently holds to `text`, for
        /// the incremental reparse to splice (`AGENTS.md` decision #6). [`None`]
        /// when the text arrived by a route carrying no edits — a `didOpen`, a
        /// re-lint sweep — which clears the chain rather than leaving one that no
        /// longer describes how this text was reached.
        ///
        /// Only ever a hint: a stale or missing chain costs a full parse and
        /// nothing else, because [`reparse_edits`](crate::parser::reparse_edits)
        /// rejects any chain that does not land on exactly this text.
        edits: Option<Vec<Edit>>,
    },
    /// A configuration-only update for one source before its next read.
    Declarations {
        path: PathBuf,
        declarations: Arc<ResolvedDeclarations>,
    },
    /// `didClose`: evict the file from the db. Diagnostics are cleared directly by
    /// the main loop.
    Close {
        path: PathBuf,
    },
    /// A `workspace/didChangeWatchedFiles` event for a **non-open** `.tex`/`.bib`
    /// project file: re-read it from disk (or evict it on delete) and re-lint every
    /// open document, since a sibling's labels/cites may have changed. The main loop
    /// has already confirmed the path is not an open editor buffer (whose overlay text
    /// is authoritative), so this path deliberately re-reads disk.
    WatchedChange {
        path: PathBuf,
        deleted: bool,
        exclude: ExcludeFilter,
    },
    /// A formatting request: format on the read pool and reply to `id`.
    Format {
        id: RequestId,
        path: PathBuf,
        style: FormatStyle,
        kind: FileKind,
        /// The `sentence`/`semantic` language and merged no-break abbreviations,
        /// resolved from the document's config (see [`ResolvedSettings`]).
        sentence_lang: SentenceLanguage,
        sentence_no_break: Vec<String>,
    },
    /// A range-formatting request: like [`WorkerJob::Format`] but bounded to the
    /// editor selection (expanded to whole document-level blocks on the read pool).
    RangeFormat {
        id: RequestId,
        path: PathBuf,
        style: FormatStyle,
        kind: FileKind,
        ranges: Vec<Range>,
        sentence_lang: SentenceLanguage,
        sentence_no_break: Vec<String>,
    },
    /// An on-type-formatting request (`textDocument/onTypeFormatting`): the user
    /// typed `}`. Re-indents the containing top-level block, but only when the `}`
    /// structurally closes a multi-line group or an `\end{…}` (see
    /// [`compute_on_type_format`]); otherwise no edits. `position` is the cursor
    /// just after the typed `}`.
    OnTypeFormat {
        id: RequestId,
        path: PathBuf,
        style: FormatStyle,
        kind: FileKind,
        position: Position,
        sentence_lang: SentenceLanguage,
        sentence_no_break: Vec<String>,
    },
    /// A document-symbol request: build the outline on the read pool and reply to
    /// `id`. Cross-file only for the `.aux` enrichment (resolved numbers); the
    /// database snapshot carries project membership, and `build` locates the aux
    /// files.
    Symbols {
        id: RequestId,
        path: PathBuf,
        kind: FileKind,
        build: BuildConfig,
        options: tex_ls_protocol::presentation::OutlineOptions,
    },
    /// A `workspace/symbol` request: aggregate every tracked file's outline on the
    /// read pool and reply to `id` with the matches for `query`. The database
    /// snapshot supplies the whole project's membership.
    InlayHints {
        id: RequestId,
        path: PathBuf,
        range: Range,
        options: tex_ls_protocol::presentation::HintOptions,
        build: BuildConfig,
    },
    WorkspaceDiagnostic {
        id: RequestId,
        previous: serde_json::Value,
        partial: Option<serde_json::Value>,
    },
    WorkspaceDiagnosticReport {
        request: WorkspaceDiagnosticSources,
        settings: HashMap<PathBuf, (ResolvedSettings, Option<i32>)>,
    },
    WorkspaceSymbols {
        id: RequestId,
        query: String,
        options: Vec<(PathBuf, tex_ls_protocol::presentation::OutlineOptions)>,
        active: Option<PathBuf>,
    },
    /// A folding-range request: compute foldable regions on the read pool and reply
    /// to `id`. Single-file like [`Symbols`](Self::Symbols), using its source snapshot.
    FoldingRange {
        id: RequestId,
        path: PathBuf,
        kind: FileKind,
    },
    /// A selection-range request: for each cursor `positions`, compute the nested
    /// "expand selection" chain from the CST ancestor walk on the read pool and reply
    /// to `id`. Single-file and positional like [`FoldingRange`](Self::FoldingRange),
    /// using its source snapshot.
    SelectionRange {
        id: RequestId,
        path: PathBuf,
        kind: FileKind,
        positions: Vec<Position>,
    },
    /// A document-link request: build clickable include/package/bib/graphics links
    /// on the read pool and reply to `id`. Single-file and positional (it bypasses
    /// the range-free project graph); `path` supplies the base directory that
    /// relative targets resolve and existence-check against. `texmf` gates the
    /// installed-tree fallback (a system `\usepackage{amsmath}` → its real source);
    /// the read pool builds/consults the index so the tree walk stays off the main loop.
    DocumentLink {
        id: RequestId,
        path: PathBuf,
        kind: FileKind,
        texmf: InstalledPackages,
    },
    /// A completion request: classify the cursor and build candidates on the read
    /// pool and reply to `id`. Carries the `uri` (the salsa-key path is derived from
    /// it) so file-path completion can read the document's on-disk directory, and the
    /// `[texmf]` settings gating the installed-set completion tier.
    Completion {
        id: RequestId,
        uri: Uri,
        position: Position,
        texmf: InstalledPackages,
    },
    /// A completion-resolve request: attach lazy signature/citation detail to a
    /// highlighted item on the read pool and reply to `id`. The item's `data`
    /// payload is self-contained, so no document buffer is needed; the cross-file
    /// lookup reads membership from the database snapshot.
    ResolveCompletion {
        id: RequestId,
        // Boxed: a `CompletionItem` is large and would bloat every `WorkerJob`.
        item: Box<CompletionItem>,
    },
    /// A hover request: describe the command/environment signature or `\cite` entry
    /// under the cursor on the read pool and reply to `id`. Cross-file (the signature
    /// scope folds in loaded packages, and a `\cite` resolves against the project
    /// bibliography) through the membership in the database snapshot.
    Hover {
        id: RequestId,
        path: PathBuf,
        position: Position,
        /// `[build]` settings for the label-number lookup (a `\label`/`\ref` hover
        /// reads the compile's `.aux`).
        build: BuildConfig,
    },
    /// A `tex-ls.forwardSearch` execute-command request: resolve the cursor file's document
    /// root and that root's compiled PDF, then hand `(%f, %p, %l)` to the
    /// configured viewer. Cross-file — the root scan runs the salsa
    /// `file_is_document_root` query over the label namespace—and none of it can
    /// run on the main loop, which holds no database.
    ForwardSearch {
        id: RequestId,
        path: PathBuf,
        /// 1-based, as SyncTeX and every viewer count.
        line: u32,
        /// `[build]` settings locating the PDF, with `root` already absolutized.
        build: BuildConfig,
        executable: String,
        args: Vec<String>,
    },
    /// A signature-help request: describe the command/environment whose argument
    /// the cursor is typing in on the read pool and reply to `id`. The signature
    /// scope folds in loaded packages from the database snapshot's project.
    SignatureHelp {
        id: RequestId,
        path: PathBuf,
        position: Position,
    },
    /// A go-to-definition request: resolve the `\ref`/`\cite` under the cursor to
    /// its `\label`/bib entry on the read pool and reply to `id`. `texmf` gates the
    /// file-target fallback (an include/package argument jumps to its resolved
    /// source, installed-tree-aware).
    GotoDefinition {
        id: RequestId,
        path: PathBuf,
        position: Position,
        texmf: InstalledPackages,
    },
    /// A find-references request: enumerate every `\ref`/`\cite` use of the
    /// label/key under the cursor on the read pool and reply to `id`. It is
    /// cross-file and invokable from a definition site.
    References {
        id: RequestId,
        path: PathBuf,
        position: Position,
        include_declaration: bool,
    },
    /// A `documentHighlight` request: shade the cross-reference key under the cursor
    /// and every same-key occurrence in the *same* buffer. Single-file (the
    /// lightweight cousin of [`References`](Self::References)); dispatched to the read
    /// pool like the others to keep the threading model uniform.
    DocumentHighlight {
        id: RequestId,
        path: PathBuf,
        position: Position,
    },
    /// A `prepareRename` request: confirm the cursor sits on a renameable label/cite
    /// key and reply with that key's range + placeholder. The cursor target comes
    /// from one parse; command/environment names additionally use the project-wide
    /// user-definition gate.
    PrepareRename {
        id: RequestId,
        path: PathBuf,
        position: Position,
    },
    /// A `rename` request: build the project-wide [`WorkspaceEdit`] renaming the
    /// label/cite key under the cursor and every referencing command. Cross-file
    /// scope comes from the database snapshot.
    Rename {
        id: RequestId,
        path: PathBuf,
        position: Position,
        new_name: String,
    },
    InspectProject {
        id: RequestId,
        path: PathBuf,
    },
    /// Standard synchronous local linked editing.
    LinkedEditing {
        id: RequestId,
        path: PathBuf,
        position: Position,
    },
    /// A pull diagnostic request. Preceding source writes reach the database
    /// before this descriptor captures a snapshot.
    Diagnostic {
        id: RequestId,
        path: PathBuf,
        kind: FileKind,
        previous_result_id: Option<String>,
        /// The document's resolved lint-rule selection, applied to the report.
        rules: RuleSelection,
        build: BuildConfig,
    },
    /// A `textDocument/codeAction` request: re-lint the buffer off a fresh snapshot
    /// and reply with quick-fixes plus any syntax-aware refactoring at `range`.
    /// Cross-file state comes from the database snapshot; `uri` is needed to key
    /// the resulting [`WorkspaceEdit`].
    CodeAction {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        kind: FileKind,
        range: Range,
        /// Action kinds requested by the client. A parent kind such as `refactor`
        /// admits its descendants, including `refactor.rewrite`.
        only: Option<Vec<CodeActionKind>>,
        /// The document's resolved lint-rule selection, applied to the findings.
        rules: RuleSelection,
    },
}

/// A result from a worker (the lint thread or a read-pool job) back to the main
/// loop, which forwards it to the client.
pub(super) enum Outbound {
    ReadStarted {
        id: RequestId,
    },
    Loading(bool),
    WatchArtifacts {
        source: PathBuf,
        paths: Vec<PathBuf>,
    },
    /// Resolve settings for every captured source on the settings-owning thread.
    WorkspaceDiagnosticSettings(WorkspaceDiagnosticSources),
    /// Push diagnostics for `uri` at `version` (gated against the live buffer).
    Diagnostics {
        stamp: u64,
        uri: Uri,
        version: i32,
        diags: Vec<Diagnostic>,
    },
    /// A request response (e.g. a formatting edit array).
    Response(Response),
    Progress {
        id: RequestId,
        token: serde_json::Value,
        value: serde_json::Value,
    },
    /// Project membership grew (the worker discovered on-disk siblings), so the
    /// cross-file resolution may have changed for *every* open document. Re-lint
    /// them all.
    RelintAll,
    /// Inputs changed without an edit to every affected visible document.
    Refresh(refresh::Feature),
}

pub(super) struct WorkspaceDiagnosticSources {
    pub id: RequestId,
    pub previous: serde_json::Value,
    pub partial: Option<serde_json::Value>,
    pub paths: Vec<PathBuf>,
}

impl WorkerJob {
    pub(super) fn request_id(&self) -> Option<&RequestId> {
        match self {
            Self::WorkspaceDiagnosticReport { request, .. } => Some(&request.id),
            Self::InspectAcquisition { id }
            | Self::Colors { id, .. }
            | Self::SemanticTokens { id, .. }
            | Self::Format { id, .. }
            | Self::RangeFormat { id, .. }
            | Self::OnTypeFormat { id, .. }
            | Self::Symbols { id, .. }
            | Self::FoldingRange { id, .. }
            | Self::SelectionRange { id, .. }
            | Self::DocumentLink { id, .. }
            | Self::Completion { id, .. }
            | Self::ResolveCompletion { id, .. }
            | Self::Hover { id, .. }
            | Self::ForwardSearch { id, .. }
            | Self::SignatureHelp { id, .. }
            | Self::GotoDefinition { id, .. }
            | Self::References { id, .. }
            | Self::DocumentHighlight { id, .. }
            | Self::PrepareRename { id, .. }
            | Self::Rename { id, .. }
            | Self::WillRenameFiles { id, .. }
            | Self::InspectProject { id, .. }
            | Self::LinkedEditing { id, .. }
            | Self::Diagnostic { id, .. }
            | Self::WorkspaceDiagnostic { id, .. }
            | Self::WorkspaceSymbols { id, .. }
            | Self::InlayHints { id, .. }
            | Self::CodeAction { id, .. } => Some(id),
            _ => None,
        }
    }
}
