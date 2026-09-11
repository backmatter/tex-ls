//! Native worker responsibility.
use super::*;
use meaning_analysis::incremental::ProjectId;
use meaning_protocol::projects::ProjectRegistry;

/// Signal from a finished analyze read-phase back to the worker: the analyze for
/// `uri`@`version` completed (or unwound on cancellation) and dropped its db
/// clone, so the in-flight slot is free.
pub(super) struct AnalyzeDone {
    pub(super) uri: Uri,
    pub(super) version: i32,
}

/// The single in-flight analyze, if any.
pub(super) struct InflightAnalyze {
    pub(super) uri: Uri,
    pub(super) version: i32,
}

/// A queued analyze request: the latest pending edit for a URI.
pub(super) struct AnalyzeRequest {
    pub(super) uri: Uri,
    pub(super) path: PathBuf,
    pub(super) version: i32,
    pub(super) kind: FileKind,
    /// The document's resolved lint-rule selection, applied to the analyze.
    pub(super) rules: RuleSelection,
}

/// What [`Worker::try_dispatch`] should do given the in-flight analyze and the
/// pending queue. Pure decision (see [`decide`]) so it can be unit-tested.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DispatchAction {
    /// Idle with nothing queued, or busy with no newer edit for the in-flight
    /// URI: leave the running analyze and wait for its `done`.
    Wait,
    /// The slot is free; start a fresh analyze for this URI.
    Start(Uri),
    /// A strictly-newer edit for the *in-flight* URI arrived; cancel the running
    /// analyze and start this URI. Only ever the in-flight URI — a different
    /// pending URI must never cancel the in-flight one.
    SupersedeAndStart(Uri),
}

/// Decide the next dispatch action. `inflight` is the running analyze's
/// `(uri, version)`, if any; `pending` maps each queued URI to its latest
/// version. Cancel only on a strictly-newer edit of the *same* URI.
pub(super) fn decide(inflight: Option<(&Uri, i32)>, pending: &HashMap<Uri, i32>) -> DispatchAction {
    match inflight {
        None => match pending.keys().next() {
            Some(uri) => DispatchAction::Start(uri.clone()),
            None => DispatchAction::Wait,
        },
        Some((uri, version)) => {
            if pending.get(uri).is_some_and(|&v| v > version) {
                DispatchAction::SupersedeAndStart(uri.clone())
            } else {
                DispatchAction::Wait
            }
        }
    }
}

/// Spawn the worker thread that owns the [`IncrementalDatabase`] (the sole
/// writer) and drives diagnostics analyzes onto the read pool.
pub(super) fn spawn_worker(
    job_rx: Receiver<WorkerJob>,
    out_tx: Sender<Outbound>,
    read_spawner: Spawner,
    encoding: PositionEncoding,
    roots: Vec<PathBuf>,
) -> JoinHandle<()> {
    let (done_tx, done_rx) = unbounded::<AnalyzeDone>();
    std::thread::Builder::new()
        .name("meaning-lsp-worker".to_owned())
        .spawn(move || {
            let mut db = IncrementalDatabase::default();
            let projects = ProjectRegistry::from_roots(&mut db, &roots);
            let mut worker = Worker {
                db,
                projects,
                aux: Arc::default(),
                out_tx,
                done_tx,
                read_spawner,
                encoding,
                inflight: None,
                pending: HashMap::new(),
                seeded_dirs: HashSet::new(),
                bib_lookups: HashMap::new(),
            };
            worker.run(&job_rx, &done_rx);
        })
        .expect("spawn LSP worker thread")
}

pub(super) struct Worker {
    pub(super) db: IncrementalDatabase,
    projects: ProjectRegistry,
    aux: Arc<crate::aux::AuxCache>,
    pub(super) out_tx: Sender<Outbound>,
    /// Read-phase workers signal completion here so the worker can free the
    /// in-flight slot and dispatch the next pending analyze.
    pub(super) done_tx: Sender<AnalyzeDone>,
    pub(super) read_spawner: Spawner,
    /// The position encoding negotiated at `initialize`, threaded into every
    /// read job so its `LineIndex` conversions count columns in the negotiated
    /// unit (see [`negotiate_position_encoding`]).
    pub(super) encoding: PositionEncoding,
    /// The single in-flight analyze, if any. At most one runs at a time: the
    /// write-phase needs exclusive `&mut db`, and salsa cancellation is global, so
    /// a second concurrent analyze couldn't be cancelled selectively.
    pub(super) inflight: Option<InflightAnalyze>,
    /// Coalesced analyze queue: the latest pending request per URI.
    pub(super) pending: HashMap<Uri, AnalyzeRequest>,
    /// Directories already walked for on-disk `.tex`/`.bib` siblings, so each is
    /// seeded at most once (the membership-discovery hot-path guard).
    pub(super) seeded_dirs: HashSet<(ProjectId, PathBuf)>,
    /// Cached bibliography search-path lookups, including misses. The server's
    /// inherited environment is fixed for its lifetime, and local file creation
    /// reaches the database through watched-file events, so repeating a failed
    /// `kpsewhich` process on every keystroke would buy nothing.
    pub(super) bib_lookups: HashMap<ProjectId, HashMap<PathBuf, Option<PathBuf>>>,
}

/// Load bibliography resources referenced by tracked LaTeX files but located
/// outside ordinary sibling discovery. `resolve` owns all environment/filesystem
/// search policy; the database receives only explicit files and aliases, keeping
/// salsa queries deterministic.
pub(super) fn seed_bibliographies_with(
    db: &mut IncrementalDatabase,
    project: ProjectId,
    lookups: &mut HashMap<PathBuf, Option<PathBuf>>,
    mut resolve: impl FnMut(&Path, Option<&Path>) -> Option<PathBuf>,
) -> bool {
    let requests = {
        let snapshot = db.snapshot_for(project).expect("registered project");
        let mut requests = Vec::new();
        for (source_path, source_file) in snapshot.tracked_files() {
            if !file_kind_or_tex(&source_path).is_latex() {
                continue;
            }
            for target in &snapshot.file_cite_facts(source_file).bib_targets {
                let BibTarget::Path(requested) = target else {
                    continue;
                };
                if snapshot.lookup_file(requested).is_some()
                    || snapshot
                        .bibliography_alias(requested)
                        .is_some_and(|actual| snapshot.lookup_file(actual).is_some())
                {
                    continue;
                }
                requests.push((
                    requested.clone(),
                    source_path.parent().map(Path::to_path_buf),
                ));
            }
        }
        requests
    };
    let mut grew = false;
    for (requested, base_dir) in requests {
        let actual = lookups
            .entry(requested.clone())
            .or_insert_with(|| resolve(&requested, base_dir.as_deref()))
            .clone();
        let Some(actual) = actual else {
            continue;
        };
        let already_tracked = db
            .snapshot_for(project)
            .expect("registered project")
            .lookup_file(&actual)
            .is_some();
        if !already_tracked {
            let Ok(text) = std::fs::read_to_string(&actual) else {
                lookups.insert(requested, None);
                continue;
            };
            db.set_backing(project, &actual, Some(text.into_source_text()))
                .expect("registered project");
            grew = true;
        }
        if actual != requested {
            grew |= db
                .set_project_bibliography_alias(project, &requested, &actual)
                .expect("registered project");
        }
    }
    grew
}

impl Worker {
    fn project_for(&self, path: &Path) -> ProjectId {
        self.projects
            .select(path, None)
            .expect("unique workspace roots")
    }

    fn snapshot_for(&self, path: &Path) -> Analysis {
        self.db
            .snapshot_for(self.project_for(path))
            .expect("registered project")
    }

    pub(super) fn run(&mut self, job_rx: &Receiver<WorkerJob>, done_rx: &Receiver<AnalyzeDone>) {
        loop {
            select! {
                recv(job_rx) -> job => {
                    let Ok(job) = job else { break };  // main dropped `job_tx`
                    self.handle_job_guarded(job);
                    while let Ok(j) = job_rx.try_recv() {
                        self.handle_job_guarded(j);
                    }
                    self.try_dispatch();
                }
                recv(done_rx) -> done => {
                    let Ok(done) = done else { continue };
                    // Free the slot only if this `done` is for the *current*
                    // in-flight analyze — a late `done` from a superseded one must
                    // not clear the new analyze.
                    if matches!(&self.inflight, Some(f) if f.uri == done.uri && f.version == done.version)
                    {
                        self.inflight = None;
                    }
                    self.try_dispatch();
                }
            }
        }
    }

    /// Run [`handle_job`](Self::handle_job), catching any panic so one bad job
    /// can't silently kill the single write-phase worker thread — which would
    /// leave the server a zombie (the main loop keeps running, but every
    /// `job_tx.send` then no-ops, so no further diagnostics, formatting, or
    /// edits reach the db). Mirrors the read pool's per-job isolation
    /// (`task_pool.rs`). Salsa `Cancelled` never unwinds this far: the writer
    /// owns the db exclusively, so its writes are never cancelled.
    pub(super) fn handle_job_guarded(&mut self, job: WorkerJob) {
        let guarded = std::panic::AssertUnwindSafe(|| self.handle_job(job));
        if let Err(panic) = std::panic::catch_unwind(guarded) {
            let msg = panic
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            log::error!("LSP worker thread caught panic while handling job: {msg}");
        }
    }

    pub(super) fn handle_job(&mut self, job: WorkerJob) {
        let enc = self.encoding;
        match job {
            WorkerJob::Edit {
                uri,
                path,
                text,
                version,
                opened,
                kind,
                rules,
                declarations,
                exclude,
                edits,
            } => {
                // Write-phase: push the live buffer into the db. Cheap — the parse
                // is a lazy salsa query deferred to the analyze. Acquiring `&mut
                // db` blocks until any outstanding read snapshot drops (single
                // writer), which is how a fresher edit preempts an in-flight read.
                let project = self.project_for(&path);
                self.db
                    .set_project_source_declarations(project, &path, (*declarations).clone())
                    .expect("registered project");
                if opened {
                    self.refresh_backing(project, &path, false);
                    self.db
                        .open_overlay(project, &path, text.text_arc())
                        .expect("registered project");
                } else {
                    self.db
                        .apply_project_change(project, &path, text.text_arc(), edits)
                        .expect("registered project");
                }
                // Lazily pull the rest of the project off disk so cross-file rules
                // can fire. If this grows the member set, every open document's
                // resolution may have changed — re-lint them all.
                let mut membership_grew = self.seed_dir(&path, &exclude);
                membership_grew |= seed_bibliographies_with(
                    &mut self.db,
                    project,
                    self.bib_lookups.entry(project).or_default(),
                    crate::bibliography::resolve_bibliography_file,
                );
                if membership_grew {
                    let _ = self.out_tx.send(Outbound::RelintAll);
                }
                self.enqueue(AnalyzeRequest {
                    uri,
                    path,
                    version,
                    kind,
                    rules,
                });
            }
            WorkerJob::Declarations { path, declarations } => {
                // `set_declarations` no-ops on an unchanged value, so this is safe
                // to send defensively; the main loop's own mirror keeps it rare.
                self.db
                    .set_project_source_declarations(
                        self.project_for(&path),
                        &path,
                        (*declarations).clone(),
                    )
                    .expect("registered project");
            }
            WorkerJob::Close { path } => {
                let project = self.project_for(&path);
                let before = self.source_version_at(project, &path);
                self.refresh_backing(project, &path, false);
                self.db
                    .close_overlay(project, &path)
                    .expect("registered project");
                if before != self.source_version_at(project, &path) {
                    let _ = self.out_tx.send(Outbound::RelintAll);
                }
            }
            WorkerJob::WatchedChange { path, deleted } => {
                if self.apply_watched_change(&path, deleted) {
                    let _ = self.out_tx.send(Outbound::RelintAll);
                }
            }
            WorkerJob::Format {
                id,
                path,
                style,
                kind,
                sentence_lang,
                sentence_no_break,
            } => {
                // Format reads run on the read pool against a snapshot, concurrent
                // with the analyze slot (they are id-bound responses, not coalesced).
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    let sentence =
                        SentenceOptions::from_resolved(sentence_lang, &sentence_no_break);
                    run_format(&snapshot, id, &path, enc, style, kind, sentence, &out_tx)
                });
            }
            WorkerJob::RangeFormat {
                id,
                path,
                style,
                kind,
                range,
                sentence_lang,
                sentence_no_break,
            } => {
                // Range formatting runs on the read pool against a snapshot, exactly
                // like `Format` (an id-bound response, not coalesced).
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    let sentence =
                        SentenceOptions::from_resolved(sentence_lang, &sentence_no_break);
                    run_range_format(
                        &snapshot, id, &path, enc, style, kind, range, sentence, &out_tx,
                    )
                });
            }
            WorkerJob::OnTypeFormat {
                id,
                path,
                style,
                kind,
                position,
                sentence_lang,
                sentence_no_break,
            } => {
                // On-type formatting reads on the read pool against a snapshot,
                // exactly like `RangeFormat` (an id-bound response, not coalesced).
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    let sentence =
                        SentenceOptions::from_resolved(sentence_lang, &sentence_no_break);
                    run_on_type_format(
                        &snapshot, id, &path, enc, style, kind, position, sentence, &out_tx,
                    )
                });
            }
            WorkerJob::Symbols {
                id,
                path,
                kind,
                build,
            } => {
                // Symbol reads, like formatting, run on the read pool against a
                // snapshot (id-bound responses, not coalesced).
                let snapshot = self.snapshot_for(&path);
                let aux = Arc::clone(&self.aux);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_symbols(&snapshot, id, &path, enc, kind, &build, &aux, &out_tx)
                });
            }
            WorkerJob::WorkspaceSymbols { id, query } => {
                // Workspace symbols scan every file in the database snapshot.
                let snapshots: Vec<_> = self
                    .projects
                    .projects()
                    .map(|project| self.db.snapshot_for(project).expect("registered project"))
                    .collect();
                let out_tx = self.out_tx.clone();
                self.read_spawner
                    .spawn(move || run_workspace_symbols(&snapshots, id, &query, enc, &out_tx));
            }
            WorkerJob::FoldingRange { id, path, kind } => {
                // Folding reads run on the read pool against a snapshot, like
                // symbols (id-bound responses, not coalesced).
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner
                    .spawn(move || run_folding(&snapshot, id, &path, enc, kind, &out_tx));
            }
            WorkerJob::SelectionRange {
                id,
                path,
                kind,
                positions,
            } => {
                // Selection ranges run on the read pool against a snapshot, like
                // folding (single-file, id-bound responses).
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_selection_range(&snapshot, id, &path, enc, kind, &positions, &out_tx)
                });
            }
            WorkerJob::DocumentLink {
                id,
                path,
                kind,
                texmf,
            } => {
                // Document links run on the read pool against a snapshot, like
                // folding (single-file, id-bound responses). Resolution is positional
                // and disk-aware, so no project membership snapshot is needed. The
                // TEXMF index is built/consulted here (off the main loop).
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_document_link(&snapshot, id, &path, enc, kind, &texmf, &out_tx)
                });
            }
            WorkerJob::Completion {
                id,
                uri,
                position,
                texmf,
            } => {
                // Completion reads run on the read pool against a snapshot, like
                // formatting/symbols (id-bound responses, not coalesced). The TEXMF
                // index is built/consulted on the read pool, off the main loop.
                let path = uri_to_fs_path(&uri).expect("validated document URI");
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_completion(&snapshot, id, &uri, enc, position, &texmf, &out_tx)
                });
            }
            WorkerJob::ResolveCompletion { id, item } => {
                let snapshot = completion_resolve::source_path(&item)
                    .map(|path| self.snapshot_for(&path))
                    .unwrap_or_else(|| self.db.snapshot());
                let out_tx = self.out_tx.clone();
                self.read_spawner
                    .spawn(move || run_completion_resolve(&snapshot, id, *item, &out_tx));
            }
            WorkerJob::Hover {
                id,
                path,
                position,
                build,
            } => {
                let snapshot = self.snapshot_for(&path);
                let aux = Arc::clone(&self.aux);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_hover(&snapshot, id, &path, enc, position, &build, &aux, &out_tx)
                });
            }
            WorkerJob::ForwardSearch {
                id,
                path,
                line,
                build,
                executable,
                args,
            } => {
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_forward_search(
                        &snapshot,
                        id,
                        &path,
                        line,
                        &build,
                        &executable,
                        &args,
                        &out_tx,
                    )
                });
            }
            WorkerJob::SignatureHelp { id, path, position } => {
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_signature_help(&snapshot, id, &path, position, enc, &out_tx)
                });
            }
            WorkerJob::GotoDefinition {
                id,
                path,
                position,
                texmf,
            } => {
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_goto_definition(&snapshot, id, &path, position, &texmf, enc, &out_tx)
                });
            }
            WorkerJob::References {
                id,
                path,
                position,
                include_declaration,
            } => {
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_references(
                        &snapshot,
                        id,
                        &path,
                        position,
                        include_declaration,
                        enc,
                        &out_tx,
                    )
                });
            }
            WorkerJob::DocumentHighlight { id, path, position } => {
                // Single-file like prepareRename: no project membership, just a db
                // snapshot to reach the cached model when the buffer is current.
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_document_highlight(&snapshot, id, &path, enc, position, &out_tx)
                });
            }
            WorkerJob::PrepareRename { id, path, position } => {
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_prepare_rename(&snapshot, id, &path, enc, position, &out_tx)
                });
            }
            WorkerJob::Rename {
                id,
                path,
                position,
                new_name,
            } => {
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_rename(&snapshot, id, &path, position, &new_name, enc, &out_tx)
                });
            }
            WorkerJob::ChangeEnvironment {
                id,
                uri,
                path,
                position,
                new_name,
            } => {
                // Single-file like prepareRename: only the cursor buffer's tree is
                // read, so no membership snapshot is needed.
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_change_environment(
                        &snapshot, id, &uri, &path, enc, position, &new_name, &out_tx,
                    )
                });
            }
            WorkerJob::Diagnostic {
                id,
                path,
                kind,
                previous_result_id,
                rules,
            } => {
                // On-demand pull is a free, id-bound read—not the coalesced analyze
                // slot—so it never blocks or supersedes the push analyze.
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_document_diagnostic(
                        &snapshot,
                        id,
                        &path,
                        kind,
                        previous_result_id,
                        &rules,
                        enc,
                        &out_tx,
                    )
                });
            }
            WorkerJob::CodeAction {
                id,
                uri,
                path,
                kind,
                range,
                only,
                rules,
            } => {
                // On-demand re-lint, like the pull-diagnostics path, runs against a
                // snapshot on the read pool.
                let snapshot = self.snapshot_for(&path);
                let out_tx = self.out_tx.clone();
                self.read_spawner.spawn(move || {
                    run_code_action(
                        &snapshot,
                        id,
                        &uri,
                        &path,
                        kind,
                        range,
                        only.as_deref(),
                        &rules,
                        enc,
                        &out_tx,
                    )
                });
            }
        }
    }

    /// Walk the active file's directory once for `.tex`/`.bib` siblings, reading
    /// and upserting any not already tracked, so the cross-file resolvers see the
    /// whole project. Returns whether the member set grew.
    ///
    /// Skips unsaved/synthetic buffers (whose path isn't a real file) and the
    /// filesystem root, so we never walk `/`. A sibling that is already tracked —
    /// an open buffer, or one seeded earlier — keeps its live text (we never read
    /// it back from disk). Each directory is walked at most once (`seeded_dirs`).
    ///
    /// `exclude` is the document's resolved [`ExcludeFilter`] (built on the main
    /// side, where the config lives, and threaded through [`WorkerJob::Edit`]), so
    /// a `meaning.toml` `exclude`/`extend-exclude` prunes the same siblings here as
    /// it does for the CLI. It is exclude-nothing when no config governs.
    pub(super) fn seed_dir(&mut self, path: &Path, exclude: &ExcludeFilter) -> bool {
        if !path.is_file() {
            return false;
        }
        let Some(dir) = path.parent() else {
            return false;
        };
        // Never walk the filesystem root (a `/foo.tex` would otherwise walk all of `/`).
        if dir.parent().is_none() {
            return false;
        }
        let dir = dir.to_path_buf();
        let project = self.project_for(path);
        if !self.seeded_dirs.insert((project, dir.clone())) {
            return false; // already walked
        }
        // A discovered `meaning.toml` governs sibling discovery here too: the
        // document's resolved exclude filter is built on the main side (where the
        // config lives) and threaded in via `WorkerJob::Edit`, so the same
        // `exclude`/`extend-exclude` that scope the CLI's walk prune these siblings.
        let Ok(files) = collect_lint_files(&[dir], exclude) else {
            return false;
        };
        let mut grew = false;
        for (sibling, _kind) in files {
            if self.project_for(&sibling) != project
                || self.snapshot_for(&sibling).lookup_file(&sibling).is_some()
            {
                continue; // open buffer or already seeded — keep its live text
            }
            if let Ok(text) = std::fs::read_to_string(&sibling) {
                self.db
                    .set_backing(project, &sibling, Some(text.into_source_text()))
                    .expect("registered project");
                grew = true;
            }
        }
        grew
    }

    fn source_version_at(
        &self,
        project: ProjectId,
        path: &Path,
    ) -> Option<meaning_analysis::incremental::SourceVersion> {
        let snapshot = self.db.snapshot_for(project).expect("registered project");
        snapshot
            .lookup_file(path)
            .and_then(|source| snapshot.source_version(source))
    }

    /// Acquisition changes backing data while the analysis writer preserves any
    /// editor overlay. Report only changes to the effective source or membership.
    fn refresh_backing(&mut self, project: ProjectId, path: &Path, deleted: bool) -> bool {
        let before = self.source_version_at(project, path);
        let text = if deleted {
            None
        } else {
            match std::fs::read_to_string(path) {
                Ok(text) => Some(text.into_source_text()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    log::warn!("Cannot read backing source {}: {error}", path.display());
                    return false;
                }
            }
        };
        self.db
            .set_backing(project, path, text)
            .expect("registered project");
        before != self.source_version_at(project, path)
    }

    pub(super) fn apply_watched_change(&mut self, path: &Path, deleted: bool) -> bool {
        if meaning_analysis::source::lint_file_kind(path).is_none() {
            return false;
        }
        let project = self.project_for(path);
        let tracked = self.source_version_at(project, path).is_some();
        let in_seeded_dir = path
            .parent()
            .is_some_and(|dir| self.seeded_dirs.contains(&(project, dir.to_path_buf())));
        if !tracked && !in_seeded_dir {
            return false;
        }
        self.refresh_backing(project, path, deleted)
    }

    /// Add `req` to the pending queue, keeping the highest version per URI.
    pub(super) fn enqueue(&mut self, req: AnalyzeRequest) {
        match self.pending.get(&req.uri) {
            Some(existing) if existing.version >= req.version => {}
            _ => {
                self.pending.insert(req.uri.clone(), req);
            }
        }
    }

    /// Start the next analyze if the slot is free, superseding the in-flight one
    /// only when a newer edit of the *same* URI is queued (see [`decide`]).
    pub(super) fn try_dispatch(&mut self) {
        let versions: HashMap<Uri, i32> = self
            .pending
            .iter()
            .map(|(uri, req)| (uri.clone(), req.version))
            .collect();
        let inflight = self.inflight.as_ref().map(|f| (&f.uri, f.version));
        let uri = match decide(inflight, &versions) {
            DispatchAction::Wait => return,
            DispatchAction::Start(uri) => uri,
            DispatchAction::SupersedeAndStart(uri) => {
                // The source write already cancelled readers of the old revision.
                // Cancelling again here would also cancel positional requests
                // captured after that write while draining the job queue.
                self.inflight = None;
                uri
            }
        };
        let Some(req) = self.pending.remove(&uri) else {
            return;
        };
        self.start_analyze(req);
    }

    /// Dispatch the diagnostics read-phase for `req` onto the read pool, holding a
    /// db clone. A superseding edit (or any write) trips `salsa::Cancelled`, caught
    /// so a cancelled analyze publishes nothing.
    pub(super) fn start_analyze(&mut self, req: AnalyzeRequest) {
        let enc = self.encoding;
        let snapshot = self.snapshot_for(&req.path);
        let out_tx = self.out_tx.clone();
        let done_tx = self.done_tx.clone();
        let AnalyzeRequest {
            uri,
            path,
            version,
            kind,
            rules,
        } = req;
        self.inflight = Some(InflightAnalyze {
            uri: uri.clone(),
            version,
        });
        self.read_spawner.spawn(move || {
            let result = salsa::Cancelled::catch(AssertUnwindSafe(|| match kind {
                FileKind::Tex
                | FileKind::CodeTex
                | FileKind::Sty
                | FileKind::Cls
                | FileKind::Dtx
                | FileKind::Ins => analyze_tex(&snapshot, &path, &rules, enc),
                FileKind::Bib => analyze_bib(&snapshot, &path, &rules, enc),
            }));
            if let Ok(Some(diags)) = result {
                let _ = out_tx.send(Outbound::Diagnostics {
                    uri: uri.clone(),
                    version,
                    diags,
                });
            }
            // Release storage before signalling completion so the next source
            // write does not wait for an analysis job reported as finished.
            drop(snapshot);
            let _ = done_tx.send(AnalyzeDone { uri, version });
        });
    }
}
