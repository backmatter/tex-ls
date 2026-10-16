#[cfg(test)]
use crate::{
    config::BuildConfig,
    lsp::task_pool,
    text::{IntoSourceText, TextBuffer},
};
use crate::{
    declarations::ResolvedDeclarations,
    file_discovery::ExcludeFilter,
    incremental::{Analysis, IncrementalDatabase},
    linter::RuleSelection,
    lsp::{
        compiler_acquisition, file_acquisition,
        messages::{Outbound, WorkerJob},
        refresh, request_ids,
        task_pool::Spawner,
    },
    project::BibTarget,
    texmf::InstalledPackages,
    text::PositionEncoding,
};
use crossbeam_channel::{Receiver, Sender, select, unbounded};
use lsp_server::{ErrorCode, RequestId, Response};
#[cfg(test)]
use lsp_types::Position;
use lsp_types::Uri;
use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
};
use tex_ls_analysis::{
    incremental::ProjectId,
    source::{FileKind, file_kind_or_tex},
};
#[cfg(test)]
use tex_ls_protocol::convert::path_to_uri;
use tex_ls_protocol::{
    convert::{file_kind_for, uri_to_fs_path},
    diagnostics::compute_diagnostics,
    projects::ProjectRegistry,
};
mod acquisition;
mod dispatch;
mod observation_scope;

/// Signal from a finished analyze read-phase back to the worker: the analyze for
/// `uri`@`version` completed (or unwound on cancellation) and dropped its db
/// clone, so the in-flight slot is free.
pub(super) struct AnalyzeDone {
    pub(super) stamp: u64,
    pub(super) uri: Uri,
    pub(super) version: i32,
}

/// The single in-flight analyze, if any.
pub(super) struct InflightAnalyze {
    pub(super) stamp: u64,
    pub(super) uri: Uri,
    pub(super) version: i32,
}

/// A queued analyze request: the latest pending edit for a URI.
pub(super) struct AnalyzeRequest {
    pub(super) stamp: u64,
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
    requests: Arc<std::sync::Mutex<request_ids::RequestIds>>,
) -> JoinHandle<()> {
    let (done_tx, done_rx) = unbounded::<AnalyzeDone>();
    std::thread::Builder::new()
        .name("tex-ls-lsp-worker".to_owned())
        .spawn(move || {
            let mut db = IncrementalDatabase::default();
            let projects = ProjectRegistry::from_roots(&mut db, &roots);
            let mut worker = Worker {
                requests,
                db,
                projects,
                out_tx,
                done_tx,
                read_spawner,
                encoding,
                inflight: None,
                pending: HashMap::new(),
                pending_reads: std::collections::VecDeque::new(),
                acquisition_keys: HashMap::new(),
                acquisition_project_prefixes: HashMap::new(),
                parent_keys: HashMap::new(),
                loading_indexes: Vec::new(),
                acquisition_failures: Default::default(),
                file_acquisition: file_acquisition::FileAcquisition::new(),
                compiler_acquisition: compiler_acquisition::CompilerAcquisition::default(),
                pending_scans: Default::default(),
                extra_sources: Default::default(),
                extra_locations: Default::default(),
                prepared_files: Default::default(),
                discovery_anchors: Default::default(),
                files_inflight: HashSet::new(),
                deferred_files: std::collections::VecDeque::new(),
                seeded_dirs: HashSet::new(),
                bib_lookups: HashMap::new(),
                observation_scope_work: HashMap::new(),
            };
            worker.run(&job_rx, &done_rx);
        })
        .expect("spawn LSP worker thread")
}

pub(super) struct Worker {
    pending_reads: std::collections::VecDeque<WorkerJob>,
    acquisition_keys: HashMap<(ProjectId, PathBuf), acquisition::AcquisitionKey>,
    acquisition_project_prefixes:
        HashMap<ProjectId, (tex_ls_analysis::incremental::AnalysisRevision, Arc<str>)>,
    parent_keys: HashMap<(ProjectId, PathBuf), String>,
    loading_indexes: Vec<InstalledPackages>,
    acquisition_failures: HashMap<ProjectId, std::collections::BTreeMap<PathBuf, String>>,
    observation_scope_work: HashMap<ProjectId, usize>,
    file_acquisition: file_acquisition::FileAcquisition,
    compiler_acquisition: compiler_acquisition::CompilerAcquisition,
    pending_scans: HashMap<ProjectId, Vec<(PathBuf, PathBuf, ExcludeFilter)>>,
    extra_sources: HashMap<ProjectId, std::collections::BTreeSet<PathBuf>>,
    extra_locations: HashMap<ProjectId, std::collections::BTreeSet<PathBuf>>,
    prepared_files: HashSet<RequestId>,
    discovery_anchors: HashMap<ProjectId, (PathBuf, InstalledPackages)>,
    files_inflight: HashSet<ProjectId>,
    deferred_files: std::collections::VecDeque<WorkerJob>,
    requests: Arc<std::sync::Mutex<request_ids::RequestIds>>,
    pub(super) db: IncrementalDatabase,
    projects: ProjectRegistry,
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
#[cfg(test)]
pub(super) fn seed_bibliographies_with(
    db: &mut IncrementalDatabase,
    project: ProjectId,
    lookups: &mut HashMap<PathBuf, Option<PathBuf>>,
    mut resolve: impl FnMut(&Path, Option<&Path>) -> Option<PathBuf>,
) -> bool {
    let requests = bibliography_requests(&db.snapshot_for(project).expect("project"));
    let mut grew = false;
    for (requested, base_dir) in requests {
        let actual = lookups
            .entry(requested.clone())
            .or_insert_with(|| resolve(&requested, base_dir.as_deref()))
            .clone();
        let Some(actual) = actual else {
            use tex_ls_analysis::external::{
                ExternalInputKind, ExternalInputs, FileInputs, Observation,
            };
            let token = db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .expect("registered project");
            db.apply_external_inputs(
                token,
                ExternalInputs::Files(FileInputs {
                    file_resolution: vec![(requested, Observation::Absent)],
                    ..Default::default()
                }),
            )
            .expect("current acquisition");
            continue;
        };
        let already_tracked = db
            .snapshot_for(project)
            .expect("registered project")
            .lookup_file(&actual)
            .is_some();
        if !already_tracked {
            let Ok(text) = file_acquisition::read_source(&actual) else {
                lookups.insert(requested, None);
                continue;
            };
            db.set_backing(project, &actual, Some(text.into_source_text()))
                .expect("registered project");
            grew = true;
        }
        if actual != requested {
            grew |= db
                .set_project_file_alias(project, &requested, &actual)
                .expect("registered project");
        }
    }
    grew
}

impl Worker {
    fn refresh(&self, feature: refresh::Feature) {
        let _ = self.out_tx.send(Outbound::Refresh(feature));
    }

    fn set_source_declarations(&mut self, path: &Path, declarations: &ResolvedDeclarations) {
        let previous = self.snapshot_for(path).declarations_for(path).clone();
        self.db
            .set_project_source_declarations(self.project_for(path), path, declarations.clone())
            .expect("registered project");
        let syntax_changed = previous.as_db() != declarations.as_db();
        if syntax_changed {
            self.refresh(refresh::Feature::Folding);
        }
        let aliases = |value: &ResolvedDeclarations| {
            value
                .command_names()
                .map(|name| (name.to_owned(), value.command_like(name).map(str::to_owned)))
                .collect::<Vec<_>>()
        };
        if syntax_changed || aliases(&previous) != aliases(declarations) {
            self.refresh(refresh::Feature::SemanticTokens);
        }
    }

    /// Only loaded package/class sources can change another document's token
    /// scope. Range-free definitions and edges keep prose edits quiet.
    fn package_token_inputs(
        &self,
        path: &Path,
    ) -> Option<(
        tex_ls_parser::semantic::signature::SignatureDb,
        Vec<tex_ls_analysis::project::package::PackageEdgeKey>,
    )> {
        if !path
            .extension()
            .is_some_and(|ext| matches!(ext.to_str(), Some("sty" | "cls" | "dtx" | "def" | "lco")))
        {
            return None;
        }
        let snapshot = self.snapshot_for(path);
        let file = snapshot.lookup_file(path)?;
        Some((
            snapshot.document_signatures(file).clone(),
            tex_ls_analysis::project::package::collect_package_edge_keys(
                &snapshot.parsed_tree(file),
                path.parent(),
            ),
        ))
    }

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
        let files_done = self.file_acquisition.rx.clone();
        let compiler_done = self.file_acquisition.compiler_rx.clone();
        let admission = crossbeam_channel::tick(std::time::Duration::from_millis(5));
        let installation_poll = crossbeam_channel::tick(std::time::Duration::from_secs(1));
        loop {
            select! {
                recv(installation_poll) -> _ => {
                    let anchors: Vec<_> = self.discovery_anchors.iter()
                        .map(|(project, (path, index))| (*project, path.clone(), index.clone())).collect();
                    for (project, path, installed) in anchors {
                        if let (Ok(snapshot), Some(index)) = (self.db.snapshot_for(project), installed.ready_index())
                            && snapshot.texmf() != index.as_ref() {
                                drop(snapshot);
                                self.acquisition_keys.remove(&(project, path.clone()));
                                self.acquire_files(project, &path, None, &installed);
                                let _ = self.out_tx.send(Outbound::RelintAll);
                        }
                    }
                }

                recv(compiler_done) -> completed => {
                    if let Ok(completed) = completed {
                        self.compiler_acquisition.complete(&mut self.db, completed, &self.out_tx, &self.file_acquisition);
                        if self.compiler_acquisition.pending_count() == 0 && self.files_inflight.is_empty() && self.loading_indexes.is_empty() {
                            let _ = self.out_tx.send(Outbound::Loading(false));
                        }
                        self.resume_deferred();
                    }
                }
                recv(files_done) -> completed => {
                    if let Ok(completed) = completed {
                        self.complete_files(completed);
                    }
                }
                recv(admission) -> _ => {
                    let loading_before = !self.loading_indexes.is_empty();
                    let ready: Vec<_> = self.discovery_anchors.iter()
                        .filter(|(_, (_, index))| self.loading_indexes.contains(index) && index.ready_index().is_some())
                        .map(|(project, (path, index))| (*project, path.clone(), index.clone())).collect();
                    let have_ready = !ready.is_empty();
                    self.loading_indexes.retain(|index| index.ready_index().is_none());
                    for (project, path, index) in ready {
                        if self.db.snapshot_for(project).is_ok() {
                            self.acquisition_keys.remove(&(project, path.clone()));
                            self.acquire_files(project, &path, None, &index);
                            let _ = self.out_tx.send(Outbound::RelintAll);
                        }
                    }
                    if loading_before && self.loading_indexes.is_empty() && self.files_inflight.is_empty() && self.compiler_acquisition.pending_count() == 0 {
                        let _ = self.out_tx.send(Outbound::Loading(false));
                    }
                    if have_ready { self.resume_deferred(); }
                    self.dispatch_reads();
                    self.try_dispatch();
                }
                recv(job_rx) -> job => {
                    let Ok(job) = job else { break };  // main dropped `job_tx`
                    self.handle_job_guarded(job);
                    while let Ok(j) = job_rx.try_recv() {
                        self.handle_job_guarded(j);
                    }
                    self.dispatch_reads();
                    self.try_dispatch();
                }
                recv(done_rx) -> done => {
                    let Ok(done) = done else { continue };
                    // Free the slot only if this `done` is for the *current*
                    // in-flight analyze — a late `done` from a superseded one must
                    // not clear the new analyze.
                    if matches!(&self.inflight, Some(f) if f.uri == done.uri && f.version == done.version && f.stamp == done.stamp)
                    {
                        self.inflight = None;
                    }
                    self.dispatch_reads();
                    self.try_dispatch();
                }
            }
        }
    }

    /// Publish a completed file batch before resuming dependent requests.
    fn complete_files(&mut self, completed: file_acquisition::Completed) {
        self.files_inflight.remove(&completed.project);
        let before = self
            .db
            .snapshot_for(completed.project)
            .ok()
            .map(|snapshot| {
                snapshot
                    .tracked_files()
                    .into_iter()
                    .map(|(path, file)| (path, snapshot.source_version(file)))
                    .collect::<Vec<_>>()
            });
        let acquired_sources = matches!(&completed.inputs, tex_ls_analysis::external::ExternalInputs::Files(files) if !files.backing.is_empty());
        let (retry_sources, retry_locations, failures) = match &completed.inputs {
            tex_ls_analysis::external::ExternalInputs::Files(files) => (
                files
                    .backing
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>(),
                files
                    .locations
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>(),
                files
                    .locations
                    .iter()
                    .map(|(path, observation)| {
                        (
                            path.clone(),
                            match &observation.kind {
                                tex_ls_analysis::external::Observation::Error(error) => {
                                    Some(error.clone())
                                }
                                _ => None,
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => unreachable!("file queue"),
        };
        let applied = self
            .db
            .apply_external_inputs(completed.token, completed.inputs)
            .is_ok();
        if applied {
            let current = self
                .acquisition_failures
                .entry(completed.project)
                .or_default();
            for (path, failure) in failures {
                if let Some(failure) = failure {
                    current.insert(path, failure);
                } else {
                    current.remove(&path);
                }
            }
            for (requested, actual) in completed.aliases {
                if let Some(actual) = &actual
                    && actual != &requested
                {
                    let _ = self
                        .db
                        .set_project_file_alias(completed.project, &requested, actual);
                }
                self.bib_lookups
                    .entry(completed.project)
                    .or_default()
                    .insert(requested, actual);
            }
            // Publish only source membership belonging to this project.
            if let Ok(snapshot) = self.db.snapshot_for(completed.project) {
                let available = |path: &Path| {
                    snapshot.lookup_file(path).is_some()
                        || snapshot
                            .file_alias(path)
                            .and_then(|actual| snapshot.lookup_file(actual))
                            .is_some()
                };
                self.parent_keys
                    .retain(|(project, path), _| *project != completed.project || available(path));
                self.acquisition_keys
                    .retain(|(project, path), _| *project != completed.project || available(path));
                let active_directories: HashSet<_> = snapshot
                    .project_members()
                    .iter()
                    .filter_map(|member| {
                        self.projects
                            .root_for(&member.path)
                            .or_else(|| member.path.parent())
                            .map(Path::to_path_buf)
                    })
                    .collect();
                self.seeded_dirs.retain(|(project, directory)| {
                    *project != completed.project || active_directories.contains(directory)
                });
            }
            let work = self
                .observation_scope_work
                .entry(completed.project)
                .or_default();
            *work = work.saturating_add(retry_locations.len());
            if *work >= 256 {
                *work = 0;
                self.retain_observation_scope(completed.project, &retry_locations);
            }
        } else {
            self.acquisition_keys.clear();
            if self.db.snapshot_for(completed.project).is_ok() {
                self.extra_sources
                    .entry(completed.project)
                    .or_default()
                    .extend(retry_sources);
                self.extra_locations
                    .entry(completed.project)
                    .or_default()
                    .extend(retry_locations);
            }
        }
        if (acquired_sources || !applied)
            && let Some((path, installed)) = self.discovery_anchors.get(&completed.project).cloned()
            && self.db.snapshot_for(completed.project).is_ok()
        {
            self.acquire_files(completed.project, &path, None, &installed);
        }
        let after = self
            .db
            .snapshot_for(completed.project)
            .ok()
            .map(|snapshot| {
                snapshot
                    .tracked_files()
                    .into_iter()
                    .map(|(path, file)| (path, snapshot.source_version(file)))
                    .collect::<Vec<_>>()
            });
        if before != after {
            self.compiler_acquisition.sources_changed(
                &mut self.db,
                completed.project,
                &self.file_acquisition,
            );
            let _ = self.out_tx.send(Outbound::RelintAll);
            self.refresh(refresh::Feature::SemanticTokens);
        } else if applied {
            // Closing an overlay may already have evicted its last source layer
            // before this refresh starts. Absence-to-absence still confirms that
            // its compiler context no longer has a backing owner.
            self.compiler_acquisition.retire_unavailable_sources(
                &mut self.db,
                completed.project,
                &self.file_acquisition,
            );
        }
        if self.files_inflight.is_empty()
            && self.loading_indexes.is_empty()
            && self.compiler_acquisition.pending_count() == 0
        {
            let _ = self.out_tx.send(Outbound::Loading(false));
        }
        self.resume_deferred();
    }

    fn resume_deferred(&mut self) {
        let pending = std::mem::take(&mut self.deferred_files);
        for job in pending {
            self.handle_job_guarded(job);
        }
        self.dispatch_reads();
        self.try_dispatch();
    }

    fn prepare_compiler_inputs(&mut self, job: &WorkerJob) {
        match job {
            WorkerJob::InlayHints { path, build, .. }
            | WorkerJob::Symbols { path, build, .. }
            | WorkerJob::Hover { path, build, .. }
            | WorkerJob::Diagnostic { path, build, .. } => {
                self.acquire_compiler(self.project_for(path), path, build);
            }
            WorkerJob::WorkspaceDiagnosticReport { settings, .. } => {
                let mut paths: Vec<_> = settings.keys().collect();
                paths.sort();
                let mut acquired = HashSet::new();
                for path in paths {
                    if !file_kind_for(path).is_latex() {
                        continue;
                    }
                    let project = self.project_for(path);
                    let root = {
                        let snapshot = self.snapshot_for(path);
                        match snapshot.resolve_labels().candidate_roots(path) {
                            [root] => root.clone(),
                            _ => path.clone(),
                        }
                    };
                    let build = &settings[path].0.build;
                    if acquired.insert((
                        project,
                        root,
                        build.root.clone(),
                        build.aux_dir.clone(),
                        build.job_name.clone(),
                    )) {
                        self.acquire_compiler(project, path, build);
                    }
                }
            }
            _ => {}
        }
    }

    fn dispatch_reads(&mut self) {
        while self.read_spawner.available() {
            let Some(job) = self.pending_reads.pop_front() else {
                break;
            };
            self.execute_job_guarded(job);
        }
    }

    pub(super) fn handle_job_guarded(&mut self, job: WorkerJob) {
        if let Some(id) = job.request_id()
            && !self.requests.lock().expect("ledger").contains(id)
        {
            self.prepared_files
                .remove(job.request_id().expect("request"));
            return;
        }
        if let WorkerJob::DocumentLink { id, path, .. } | WorkerJob::GotoDefinition { id, path, .. } =
            &job
            && self.prepared_files.insert(id.clone())
        {
            let project = self.project_for(path);
            let snapshot = self.snapshot_for(path);
            if let Some(file) = snapshot.lookup_file(path) {
                self.extra_locations.entry(project).or_default().extend(
                    snapshot
                        .file_references(file)
                        .iter()
                        .flat_map(|reference| reference.candidates.local.iter().cloned()),
                );
            }
            self.acquisition_keys.remove(&(project, path.clone()));
        }
        if let WorkerJob::GotoDefinition { path, position, .. } = &job {
            let project = self.project_for(path);
            let snapshot = self.snapshot_for(path);
            if let Some(file) = snapshot.lookup_file(path)
                && file_kind_for(path).is_latex()
            {
                let offset = snapshot
                    .file_line_index(file, self.encoding)
                    .offset_at(position.line, position.character);
                let target = tex_ls_analysis::hover::signature_target_at(
                    &snapshot.parsed_tree(file),
                    offset,
                );
                if let Some(target) = target
                    && matches!(target.kind, tex_ls_analysis::hover::TargetKind::Command)
                    && tex_ls_parser::semantic::signature::builtin()
                        .command(&target.name)
                        .is_some()
                {
                    for source in snapshot.format_source_paths() {
                        if snapshot.lookup_file(&source).is_none()
                            && snapshot
                                .file_alias(&source)
                                .and_then(|actual| snapshot.lookup_file(actual))
                                .is_none()
                            && !matches!(
                                snapshot.location_observation(&source).kind,
                                tex_ls_analysis::external::Observation::Absent
                                    | tex_ls_analysis::external::Observation::Error(_)
                            )
                        {
                            self.extra_sources
                                .entry(project)
                                .or_default()
                                .insert(source);
                        }
                    }
                }
            }
        }
        if let WorkerJob::CodeAction {
            path,
            kind,
            range,
            only,
            rules,
            ..
        } = &job
            && kind.is_latex()
            && rules.is_active("unknown-command")
            && tex_ls_protocol::code_action_kind_requested(
                &lsp_types::CodeActionKind::QuickFix,
                only.as_deref(),
            )
        {
            use tex_ls_analysis::external::InputNeed;
            let project = self.project_for(path);
            let snapshot = self.snapshot_for(path);
            for need in tex_ls_protocol::code_action::load_package::input_needs(
                &snapshot,
                path,
                *range,
                self.encoding,
            ) {
                match need {
                    InputNeed::Location(path) => {
                        self.extra_locations
                            .entry(project)
                            .or_default()
                            .insert(path);
                    }
                    InputNeed::Source(path) => {
                        self.extra_sources.entry(project).or_default().insert(path);
                    }
                    InputNeed::Installed => {
                        self.acquisition_keys.remove(&(project, path.to_owned()));
                    }
                }
            }
        }
        let waiting = match &job {
            WorkerJob::Completion {
                uri,
                position,
                texmf,
                ..
            } => uri_to_fs_path(uri).is_some_and(|path| {
                let project = self.project_for(&path);
                self.acquire_files(project, &path, Some((*position, self.encoding)), texmf)
            }),
            WorkerJob::DocumentLink { path, texmf, .. }
            | WorkerJob::GotoDefinition { path, texmf, .. } => {
                let project = self.project_for(path);
                self.acquire_files(project, path, None, texmf)
            }
            WorkerJob::CodeAction {
                path,
                kind,
                only,
                rules,
                texmf,
                ..
            } if kind.is_latex()
                && rules.is_active("unknown-command")
                && tex_ls_protocol::code_action_kind_requested(
                    &lsp_types::CodeActionKind::QuickFix,
                    only.as_deref(),
                ) =>
            {
                let project = self.project_for(path);
                self.acquire_files(project, path, None, texmf) || texmf.ready_index().is_none()
            }
            _ => false,
        };
        if waiting || self.waits_for_project_inputs(&job) {
            self.deferred_files.push_back(job);
            return;
        }
        if job.request_id().is_some()
            && !matches!(job, WorkerJob::InspectAcquisition { .. })
            && (!self.pending_reads.is_empty() || !self.read_spawner.available())
        {
            self.pending_reads.push_back(job);
            return;
        }
        self.execute_job_guarded(job);
    }

    fn waits_for_project_inputs(&self, job: &WorkerJob) -> bool {
        if let WorkerJob::Completion { uri, .. } = job {
            return uri_to_fs_path(uri).is_some_and(|path| {
                let project = self.project_for(&path);
                self.files_inflight.contains(&project) || self.compiler_acquisition.pending(project)
            });
        }
        let path = match job {
            WorkerJob::Format { path, .. }
            | WorkerJob::RangeFormat { path, .. }
            | WorkerJob::OnTypeFormat { path, .. }
            | WorkerJob::Colors { path, .. }
            | WorkerJob::SemanticTokens { path, .. }
            | WorkerJob::Symbols { path, .. }
            | WorkerJob::FoldingRange { path, .. }
            | WorkerJob::SelectionRange { path, .. }
            | WorkerJob::Hover { path, .. }
            | WorkerJob::SignatureHelp { path, .. }
            | WorkerJob::References { path, .. }
            | WorkerJob::DocumentHighlight { path, .. }
            | WorkerJob::PrepareRename { path, .. }
            | WorkerJob::Rename { path, .. }
            | WorkerJob::LinkedEditing { path, .. }
            | WorkerJob::Diagnostic { path, .. }
            | WorkerJob::InlayHints { path, .. }
            | WorkerJob::CodeAction { path, .. }
            | WorkerJob::ForwardSearch { path, .. } => path,
            WorkerJob::WorkspaceSymbols { .. }
            | WorkerJob::WorkspaceDiagnostic { .. }
            | WorkerJob::WorkspaceDiagnosticReport { .. }
            | WorkerJob::WillRenameFiles { .. }
            | WorkerJob::ResolveCompletion { .. } => {
                return !self.files_inflight.is_empty()
                    || self.compiler_acquisition.pending_count() != 0;
            }
            _ => return false,
        };
        self.projects.select(path, None).is_ok_and(|project| {
            self.files_inflight.contains(&project) || self.compiler_acquisition.pending(project)
        })
    }

    /// Run [`handle_job`](Self::handle_job), catching any panic so one bad job
    /// can't silently kill the single write-phase worker thread — which would
    /// leave the server a zombie (the main loop keeps running, but every
    /// `job_tx.send` then no-ops, so no further diagnostics, formatting, or
    /// edits reach the db). Mirrors the read pool's per-job isolation
    /// (`task_pool.rs`). Salsa `Cancelled` never unwinds this far: the writer
    /// owns the db exclusively, so its writes are never cancelled.
    fn execute_job_guarded(&mut self, job: WorkerJob) {
        if let Some(id) = job.request_id()
            && !self.requests.lock().expect("request ledger").contains(id)
        {
            self.prepared_files.remove(id);
            return;
        }
        if !self.waits_for_project_inputs(&job) {
            self.prepare_compiler_inputs(&job);
        }
        if self.waits_for_project_inputs(&job) {
            self.deferred_files.push_back(job);
            return;
        }

        let id = job.request_id().cloned();
        if let Some(id) = &id {
            self.prepared_files.remove(id);
            let _ = self.out_tx.send(Outbound::ReadStarted { id: id.clone() });
        }
        let guarded = std::panic::AssertUnwindSafe(|| self.handle_job(job));
        if let Err(panic) = std::panic::catch_unwind(guarded) {
            let msg = panic
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            log::error!("LSP worker thread caught panic while handling job: {msg}");
            if let Some(id) = id {
                let _ = self.out_tx.send(Outbound::Response(Response::new_err(
                    id,
                    ErrorCode::InternalError as i32,
                    "Request preparation failed".into(),
                )));
            }
        }
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
        let project = self.project_for(&req.path);
        if self.files_inflight.contains(&project) || self.compiler_acquisition.pending(project) {
            self.pending.insert(req.uri.clone(), req);
            return;
        }
        self.start_analyze(req);
    }

    /// Dispatch the diagnostics read-phase for `req` onto the read pool, holding a
    /// db clone. A superseding edit (or any write) trips `salsa::Cancelled`, caught
    /// so a cancelled analyze publishes nothing.
    pub(super) fn start_analyze(&mut self, req: AnalyzeRequest) {
        let enc = self.encoding;
        let Some(permit) = self.read_spawner.try_reserve() else {
            self.pending.insert(req.uri.clone(), req);
            return;
        };
        let snapshot = self.snapshot_for(&req.path);
        let out_tx = self.out_tx.clone();
        let done_tx = self.done_tx.clone();
        let AnalyzeRequest {
            stamp,
            uri,
            path,
            version,
            kind,
            rules,
        } = req;
        self.inflight = Some(InflightAnalyze {
            stamp,
            uri: uri.clone(),
            version,
        });
        self.read_spawner.spawn_reserved(permit, move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                Some(compute_diagnostics(&snapshot, &path, kind, &rules, enc))
            }));
            if let Err(payload) = &result
                && !payload.is::<salsa::Cancelled>()
            {
                log::error!("Background diagnostics panicked");
            }
            if let Ok(Some(diags)) = result {
                let _ = out_tx.send(Outbound::Diagnostics {
                    stamp,
                    uri: uri.clone(),
                    version,
                    diags,
                });
            }
            // Release storage before signalling completion so the next source
            // write does not wait for an analysis job reported as finished.
            drop(snapshot);
            let _ = done_tx.send(AnalyzeDone {
                uri,
                version,
                stamp,
            });
        });
    }
}

fn bibliography_requests(snapshot: &Analysis) -> Vec<(PathBuf, Option<PathBuf>)> {
    bibliography_candidates(snapshot)
        .into_iter()
        .filter(|(requested, _)| {
            snapshot.lookup_file(requested).is_none()
                && !snapshot
                    .file_alias(requested)
                    .is_some_and(|actual| snapshot.lookup_file(actual).is_some())
        })
        .collect()
}

fn bibliography_candidates(snapshot: &Analysis) -> Vec<(PathBuf, Option<PathBuf>)> {
    let mut requests = Vec::new();
    for (source_path, source_file) in snapshot.tracked_files() {
        if !file_kind_or_tex(&source_path).is_latex() {
            continue;
        }
        let targets = snapshot.file_bibliography_targets(source_file);
        if targets.is_empty() {
            continue;
        }
        let mut bases = Vec::new();
        for root in snapshot.document_roots(&source_path) {
            bases.extend(
                snapshot
                    .root_contexts(root)
                    .and_then(|contexts| contexts.get(&source_path))
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        if bases.is_empty() {
            bases.extend(source_path.parent().map(Path::to_path_buf));
        }
        bases.sort();
        bases.dedup();
        for target in &tex_ls_analysis::project::citations::contextual_targets(targets, &bases) {
            let BibTarget::Path(requested) = target else {
                continue;
            };
            requests.push((
                requested.clone(),
                source_path.parent().map(Path::to_path_buf),
            ));
        }
    }
    requests
}

#[cfg(test)]
mod audit_scheduler_tests {
    use super::*;
    #[test]
    fn declarations_refresh_only_affected_presentations() {
        let pool = task_pool::TaskPool::new("declaration-refresh", 1);
        let (out_tx, out_rx) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        let path = Path::new("/refresh/main.tex");
        db.apply_change(path, "\\begin{custom}\ntext\n\\end{custom}\n", None);
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let declarations = |text| {
            toml::from_str::<tex_ls_parser::declarations::Declarations>(text)
                .unwrap()
                .resolve()
                .unwrap()
        };
        worker.set_source_declarations(path, &declarations("[options.custom]\nkey = ['value']\n"));
        assert!(
            out_rx.try_recv().is_err(),
            "completion options do not change folds or tokens"
        );
        let aliases = declarations("[commands.myref]\nlike = 'ref'\n");
        worker.set_source_declarations(path, &aliases);
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::Refresh(refresh::Feature::SemanticTokens))
        ));
        assert!(out_rx.try_recv().is_err());
        let syntax = declarations(
            "[commands.myref]\nlike = 'ref'\n[environments.custom]\nlike = 'lstlisting'\n",
        );
        worker.set_source_declarations(path, &syntax);
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::Refresh(refresh::Feature::Folding))
        ));
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::Refresh(refresh::Feature::SemanticTokens))
        ));
        worker.set_source_declarations(path, &syntax);
        assert!(
            out_rx.try_recv().is_err(),
            "identical settings must stay quiet"
        );
    }

    fn test_worker(
        db: IncrementalDatabase,
        projects: ProjectRegistry,
        out_tx: Sender<Outbound>,
        done_tx: Sender<AnalyzeDone>,
        read_spawner: Spawner,
        requests: Arc<std::sync::Mutex<request_ids::RequestIds>>,
    ) -> Worker {
        Worker {
            requests,
            db,
            projects,
            out_tx,
            done_tx,
            read_spawner,
            encoding: PositionEncoding::Utf16,
            inflight: None,
            pending: Default::default(),
            pending_reads: Default::default(),
            seeded_dirs: Default::default(),
            bib_lookups: Default::default(),
            acquisition_keys: Default::default(),
            acquisition_project_prefixes: Default::default(),
            parent_keys: Default::default(),
            loading_indexes: Default::default(),
            acquisition_failures: Default::default(),
            observation_scope_work: Default::default(),
            file_acquisition: file_acquisition::FileAcquisition::new(),
            compiler_acquisition: compiler_acquisition::CompilerAcquisition::default(),
            pending_scans: Default::default(),
            extra_sources: Default::default(),
            extra_locations: Default::default(),
            prepared_files: Default::default(),
            discovery_anchors: Default::default(),
            files_inflight: Default::default(),
            deferred_files: Default::default(),
        }
    }

    #[test]
    fn bibliography_acquisition_reuses_resource_targets_without_label_or_citation_semantics() {
        use tex_ls_analysis::incremental::QueryKind;
        let mut db = IncrementalDatabase::default();
        let main = Path::new("/p/main.tex");
        let child = Path::new("/p/chapter.tex");
        db.apply_change(
            main,
            r"\begin{document}\addbibresource{a.bib}\input{chapter}\ref{item}\end{document}",
            None,
        );
        db.apply_change(child, r"\bibliography{b}\label{item}", None);
        let collect = |db: &IncrementalDatabase| {
            let mut result = bibliography_candidates(&db.snapshot());
            result.sort();
            result
        };
        db.clear_query_log();
        let expected = vec![
            (PathBuf::from("/p/a.bib"), Some(PathBuf::from("/p"))),
            (PathBuf::from("/p/b.bib"), Some(PathBuf::from("/p"))),
        ];
        assert_eq!(collect(&db), expected);
        assert!(db.query_log().iter().all(|entry| !matches!(
            entry.kind,
            QueryKind::SemanticModel | QueryKind::ResolvedLabels | QueryKind::FileCiteFacts
        )));
        db.clear_query_log();
        assert_eq!(collect(&db), expected);
        assert!(db.query_log().is_empty());
        db.apply_change(
            child,
            r"\renewcommand{\bibliography}[1]{}\bibliography{b}\label{item}",
            None,
        );
        assert_eq!(collect(&db), expected[..1]);
    }

    #[test]
    fn completion_acquisition_reuses_roles_and_refreshes_cursor_context_after_edits() {
        use tex_ls_analysis::incremental::QueryKind;
        let pool = task_pool::TaskPool::new("acquisition-roles", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let path = Path::new("/p/main.tex");
        let source = "\\label{marker}\n\\ref{marker}\n\\input{chapter}\n";
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source, None);
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let installed: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        let position = |worker: &Worker, text: &str, key: &str, typed: usize| {
            let snapshot = worker.snapshot_for(path);
            let offset = text.rfind(key).unwrap() + typed;
            let (line, character) = snapshot
                .file_line_index(file, PositionEncoding::Utf16)
                .position(offset);
            Position::new(line, character)
        };
        let label = position(&worker, source, "marker", 3);
        let input = position(&worker, source, "chapter", 2);
        let role_count = |worker: &Worker| {
            worker
                .db
                .query_log()
                .iter()
                .filter(|entry| entry.kind == QueryKind::FileRoles && entry.file == Some(file))
                .count()
        };
        // Discovery and cursor classification share the same role projection.
        worker.db.clear_query_log();
        let no_context = worker.file_acquisition_key(path, None, &installed);
        assert_eq!(
            role_count(&worker),
            1,
            "discovery computes source roles once"
        );
        worker.db.clear_query_log();
        for _ in 0..8 {
            assert_eq!(
                worker.file_acquisition_key(
                    path,
                    Some((label, PositionEncoding::Utf16)),
                    &installed
                ),
                no_context
            );
            let input_key = worker.file_acquisition_key(
                path,
                Some((input, PositionEncoding::Utf16)),
                &installed,
            );
            assert_ne!(input_key, no_context);
            let prefix = worker.project_acquisition_prefix(&worker.snapshot_for(path));
            assert_eq!(
                input_key.to_string(),
                format!(
                    "{prefix}{:?}{:?}{}",
                    Some("ch"),
                    installed.config(),
                    installed.generation()
                )
            );
        }
        let count = worker
            .db
            .query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileRoles && entry.file == Some(file))
            .count();
        assert_eq!(
            count, 0,
            "repeated label/path requests reuse discovery's source roles"
        );

        let changed = format!("\\renewcommand{{\\input}}[1]{{#1}}\n{source}");
        worker.db.apply_change(path, changed.as_str(), None);
        let input = position(&worker, &changed, "chapter", 2);
        worker.db.clear_query_log();
        let no_context = worker.file_acquisition_key(path, None, &installed);
        assert_eq!(
            role_count(&worker),
            1,
            "a loader edit refreshes discovery roles once"
        );
        worker.db.clear_query_log();
        for _ in 0..8 {
            assert_eq!(
                worker.file_acquisition_key(
                    path,
                    Some((input, PositionEncoding::Utf16)),
                    &installed
                ),
                no_context,
                "a redefined loader no longer supplies a filename acquisition prefix"
            );
        }
        let count = worker
            .db
            .query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileRoles && entry.file == Some(file))
            .count();
        assert_eq!(
            count, 0,
            "cursor requests reuse the refreshed discovery roles"
        );
    }

    #[test]
    fn command_name_acquisition_does_not_evaluate_source_roles() {
        use tex_ls_analysis::incremental::QueryKind;
        let pool = task_pool::TaskPool::new("acquisition-command", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let path = Path::new("/p/main.tex");
        let source = "\\label{marker}\n\\ref{marker}\n";
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source, None);
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let installed: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        let no_context = worker.file_acquisition_key(path, None, &installed);
        let snapshot = worker.snapshot_for(path);
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(source.find("\\ref").unwrap() + 3);
        drop(snapshot);
        worker.db.clear_query_log();
        for _ in 0..8 {
            assert_eq!(
                worker.file_acquisition_key(
                    path,
                    Some((Position::new(line, character), PositionEncoding::Utf16)),
                    &installed
                ),
                no_context
            );
        }
        assert!(
            !worker
                .db
                .query_log()
                .iter()
                .any(|entry| entry.kind == QueryKind::FileRoles),
            "command names skip role context construction"
        );
    }

    #[test]
    fn warm_file_acquisition_reuses_exact_project_prefix_without_source_scans() {
        let pool = task_pool::TaskPool::new("acquisition-prefix", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let path = Path::new("/p/main.tex");
        // The first backslash is after a large ordinary prefix, so the old
        // contains probe really scans that prefix on every warm request.
        let source = format!("{}\n\\section{{After}}\n", "ordinary ".repeat(131_072));
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source.as_str(), None);
        let project = db.project_id();
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let installed: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        let snapshot = worker.snapshot_for(path);
        assert!(snapshot.parse_diagnostics(file).is_empty());
        assert_eq!(snapshot.parsed_tree(file).text().to_string(), source);
        let references: Vec<_> = snapshot
            .project_members()
            .iter()
            .filter(|member| member.kind.is_latex())
            .map(|member| {
                (
                    &member.path,
                    snapshot.file_text(member.file).contains('\\'),
                    snapshot
                        .file_discovery_references(member.file)
                        .iter()
                        .map(|reference| &reference.candidates)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let expected = format!(
            "{references:?}{:?}{:?}{}",
            None::<String>,
            installed.config(),
            installed.generation()
        );
        drop(snapshot);
        acquisition::ACQUISITION_PREFIX_BUILDS.with(|count| count.set(0));
        acquisition::ACQUISITION_PREFIX_SOURCE_BYTES.with(|count| count.set(0));
        acquisition::ACQUISITION_PREFIX_OUTPUT_BYTES.with(|count| count.set(0));
        for _ in 0..8 {
            assert!(
                !worker.acquire_files(project, path, None, &installed),
                "no file dependencies require IO"
            );
            assert_eq!(
                worker
                    .acquisition_keys
                    .get(&(project, path.to_owned()))
                    .unwrap()
                    .to_string(),
                expected
            );
        }
        assert_eq!(worker.db.snapshot().project_members().len(), 1);
        assert!(worker.files_inflight.is_empty());
        assert!(worker.file_acquisition.rx.try_recv().is_err());
        let builds = acquisition::ACQUISITION_PREFIX_BUILDS.with(std::cell::Cell::get);
        let bytes = acquisition::ACQUISITION_PREFIX_SOURCE_BYTES.with(std::cell::Cell::get);
        let output_bytes = acquisition::ACQUISITION_PREFIX_OUTPUT_BYTES.with(std::cell::Cell::get);
        assert!(builds > 0 && bytes >= source.len() && output_bytes > 0);
        assert!(
            builds <= 1,
            "{builds} full project key builds for eight unchanged requests"
        );
        assert!(
            bytes <= source.len(),
            "{bytes} source bytes presented to prefix scans"
        );
    }

    #[test]
    fn acquisition_prefix_memo_tracks_snapshot_inputs_and_native_suffix() {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, FileInputs, Observation,
        };
        fn reference(snapshot: &Analysis) -> String {
            let rows: Vec<_> = snapshot
                .project_members()
                .iter()
                .filter(|member| member.kind.is_latex())
                .map(|member| {
                    (
                        &member.path,
                        snapshot.file_text(member.file).contains('\\'),
                        snapshot
                            .file_discovery_references(member.file)
                            .iter()
                            .map(|reference| &reference.candidates)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            format!("{rows:?}")
        }
        fn verify(worker: &mut Worker, path: &Path) -> Arc<str> {
            let snapshot = worker.snapshot_for(path);
            let expected = reference(&snapshot);
            let needs_build = worker
                .acquisition_project_prefixes
                .get(&snapshot.project_id())
                .is_none_or(|(revision, _)| *revision != snapshot.revision());
            let before = acquisition::ACQUISITION_PREFIX_BUILDS.with(std::cell::Cell::get);
            let prefix = worker.project_acquisition_prefix(&snapshot);
            assert_eq!(&*prefix, expected);
            assert_eq!(
                acquisition::ACQUISITION_PREFIX_BUILDS.with(std::cell::Cell::get),
                before + usize::from(needs_build)
            );
            prefix
        }
        let pool = task_pool::TaskPool::new("acquisition-prefix-invalidation", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let path = Path::new("/p/main.tex");
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, "\\input{first}\n", None);
        let project = db.project_id();
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let initial = verify(&mut worker, path);
        assert!(Arc::ptr_eq(&initial, &verify(&mut worker, path)));
        // A prose edit changes the broad writer revision but not the exact
        // reference projection. Its canonical Arc must survive that rebuild.
        worker
            .db
            .apply_change(path, "prose\n\\input{first}\n", None);
        let unchanged = verify(&mut worker, path);
        assert!(Arc::ptr_eq(&initial, &unchanged));
        acquisition::ACQUISITION_PREFIX_BUILDS.with(|count| count.set(0));
        acquisition::ACQUISITION_PREFIX_COMPARE_BYTES.with(|count| count.set(0));
        for _ in 0..8 {
            assert!(Arc::ptr_eq(&initial, &verify(&mut worker, path)));
        }
        assert_eq!(
            acquisition::ACQUISITION_PREFIX_BUILDS.with(std::cell::Cell::get),
            0
        );
        assert_eq!(
            acquisition::ACQUISITION_PREFIX_COMPARE_BYTES.with(std::cell::Cell::get),
            0
        );
        worker.db.apply_change(path, "\\input{second}\n", None);
        let changed = verify(&mut worker, path);
        assert_ne!(initial, changed);
        assert!(!Arc::ptr_eq(&initial, &changed));
        let declared = toml::from_str::<tex_ls_parser::declarations::Declarations>(
            "[environments.custom]\nlike='equation'\n",
        )
        .unwrap()
        .resolve()
        .unwrap();
        worker
            .db
            .set_project_source_declarations(project, path, declared)
            .unwrap();
        verify(&mut worker, path);
        worker
            .db
            .set_project_file_alias(
                project,
                Path::new("/p/second.tex"),
                Path::new("/p/actual.tex"),
            )
            .unwrap();
        verify(&mut worker, path);
        worker
            .db
            .apply_project_change(project, Path::new("/p/actual.tex"), "\\input{third}", None)
            .unwrap();
        verify(&mut worker, path);
        worker
            .db
            .remove_project_files(project, &[PathBuf::from("/p/actual.tex")]);
        verify(&mut worker, path);
        let token = worker
            .db
            .begin_external_refresh(project, ExternalInputKind::Files)
            .unwrap();
        worker
            .db
            .apply_external_inputs(
                token,
                ExternalInputs::Files(FileInputs {
                    file_resolution: vec![(PathBuf::from("/p/third.tex"), Observation::Absent)],
                    ..Default::default()
                }),
            )
            .unwrap();
        verify(&mut worker, path);
        worker.db.apply_change(path, "ordinary prose", None);
        let no_control = verify(&mut worker, path);
        worker.db.apply_change(path, "ordinary prose \\demo", None);
        assert_ne!(
            no_control,
            verify(&mut worker, path),
            "control-byte presence is part of the projection even without file references"
        );
        worker.db.apply_change(path, "\\input{second}\n", None);
        verify(&mut worker, path);
        let installed: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        let key = worker.file_acquisition_key(path, None, &installed);
        let prefix = verify(&mut worker, path);
        assert_eq!(
            key.to_string(),
            format!(
                "{prefix}{:?}{:?}{}",
                None::<String>,
                installed.config(),
                installed.generation()
            )
        );
        acquisition::ACQUISITION_PREFIX_BUILDS.with(|count| count.set(0));
        let completion = worker.file_acquisition_key(
            path,
            Some((Position::new(0, 10), PositionEncoding::Utf16)),
            &installed,
        );
        assert_ne!(
            key, completion,
            "an input-prefix completion is a separate request suffix"
        );
        let other: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            roots: vec![PathBuf::from("/another")],
            ..Default::default()
        }
        .into();
        assert_ne!(key, worker.file_acquisition_key(path, None, &other));
        assert_eq!(
            acquisition::ACQUISITION_PREFIX_BUILDS.with(std::cell::Cell::get),
            0
        );
    }

    #[test]
    fn moving_final_source_between_roots_releases_project_prefix_history() {
        let pool = task_pool::TaskPool::new("acquisition-prefix-rename", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let old = Path::new("/before/main.tex");
        let new = Path::new("/after/main.tex");
        let mut db = IncrementalDatabase::default();
        let projects = ProjectRegistry::from_roots(
            &mut db,
            &[PathBuf::from("/before"), PathBuf::from("/after")],
        );
        let before = projects.select(old, None).unwrap();
        let after = projects.select(new, None).unwrap();
        assert_ne!(before, after);
        let source: String = (0..512)
            .map(|index| format!("\\input{{child{index}}}\n"))
            .collect();
        let file = db
            .apply_project_change(before, old, source.as_str(), None)
            .unwrap();
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let snapshot = worker.snapshot_for(old);
        assert!(snapshot.parse_diagnostics(file).is_empty());
        assert_eq!(snapshot.file_discovery_references(file).len(), 512);
        assert_eq!(snapshot.project_members().len(), 1);
        drop(snapshot);
        let installed: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        let key = worker.file_acquisition_key(old, None, &installed);
        worker
            .acquisition_keys
            .insert((before, old.to_owned()), key);
        let prefix = &worker.acquisition_project_prefixes[&before].1;
        assert!(
            prefix.len() > 10_000,
            "a large reference projection must actually be cached"
        );
        let retained = Arc::downgrade(prefix);
        worker.handle_job_guarded(WorkerJob::DidRenameFiles {
            files: vec![(old.to_owned(), new.to_owned())],
        });
        assert!(
            worker
                .db
                .snapshot_for(before)
                .unwrap()
                .project_members()
                .is_empty()
        );
        let snapshot = worker.db.snapshot_for(after).unwrap();
        let moved = snapshot
            .lookup_file(new)
            .expect("the destination owns the source");
        assert_eq!(snapshot.file_text(moved), source);
        assert_eq!(snapshot.project_members().len(), 1);
        assert!(
            worker.acquisition_keys.is_empty(),
            "path key ownership has retired"
        );
        assert!(
            retained.upgrade().is_none(),
            "an empty live project must not retain its historical reference projection"
        );
        assert!(!worker.acquisition_project_prefixes.contains_key(&before));
    }

    #[test]
    fn changing_installations_does_not_load_unused_packages() {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
        };
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let unused = dir.path().join("unused.sty");
        std::fs::write(&main, "Text.").unwrap();
        std::fs::write(&unused, "% never referenced").unwrap();
        let pool = task_pool::TaskPool::new("installation-switch", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        db.apply_change(&main, "Text.", None);
        let project = db.project_id();
        let token = db
            .begin_external_refresh(project, ExternalInputKind::Installed)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                toolchain: "old".into(),
                index: tex_ls_analysis::project::texmf::TexmfIndex::from_files(HashMap::from([(
                    "unused.sty".into(),
                    unused,
                )])),
            })),
        )
        .unwrap();
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let disabled = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        assert!(
            !worker.acquire_files(project, &main, None, &disabled),
            "switching installations must not enqueue unreferenced package sources"
        );
        assert_eq!(worker.snapshot_for(&main).project_members().len(), 1);
    }

    #[test]
    fn saturated_reads_queue_parameters_allow_writes_and_skip_cancelled_jobs() {
        let pool = task_pool::TaskPool::new("audit-one-reader", 1);
        let spawner = pool.spawner();
        let held = spawner.reserve();
        let requests = Arc::new(std::sync::Mutex::new(request_ids::RequestIds::default()));
        let original = RequestId::from(9);
        let id = requests.lock().unwrap().receive(original.clone()).unwrap();
        let (out_tx, out_rx) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        let old = PathBuf::from("/audit/old.tex");
        let new = PathBuf::from("/audit/new.tex");
        db.apply_change(&old, "source", None);
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(db, projects, out_tx, done_tx, spawner, requests.clone());
        worker.handle_job_guarded(WorkerJob::SignatureHelp {
            id,
            path: old.clone(),
            position: Position::default(),
        });
        let rename_wire = RequestId::from(10);
        let rename_id = requests
            .lock()
            .unwrap()
            .receive(rename_wire.clone())
            .unwrap();
        worker.handle_job_guarded(WorkerJob::WillRenameFiles {
            id: rename_id,
            files: vec![(old.clone(), new.clone())],
        });
        assert_eq!(worker.pending_reads.len(), 2);
        worker.handle_job_guarded(WorkerJob::DidRenameFiles {
            files: vec![(old, new.clone())],
        });
        assert!(
            worker.db.lookup_file(&new).is_some(),
            "write passed the saturated read queue"
        );
        requests.lock().unwrap().cancel(&original);
        requests.lock().unwrap().cancel(&rename_wire);
        drop(held);
        worker.dispatch_reads();
        assert!(worker.pending_reads.is_empty());
        assert!(
            out_rx
                .try_iter()
                .all(|message| matches!(message, Outbound::Refresh(_))),
            "only rename invalidations, never a cancelled read response"
        );
        assert!(worker.read_spawner.available());
    }

    #[test]
    fn slow_watched_source_io_allows_edits_and_preserves_reopened_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tex");
        std::fs::write(&path, "disk").unwrap();
        let pool = task_pool::TaskPool::new("slow-source-reader", 1);
        let (tx, _rx) = unbounded();
        let (done_tx, _done_rx) = unbounded();
        let mut db = IncrementalDatabase::default();
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let project = db.project_id();
        db.set_backing(project, &path, Some("old disk".into_source_text()))
            .unwrap();
        db.replace_overlay(project, &path, "unsaved".into_source_text())
            .unwrap();
        let mut worker = test_worker(
            db,
            projects,
            tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let (started_tx, started_rx) = unbounded();
        let (release_tx, release_rx) = unbounded();
        let mut first = true;
        worker.file_acquisition = file_acquisition::FileAcquisition::with_before_read(move || {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
        });
        let texmf: InstalledPackages = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        worker
            .discovery_anchors
            .insert(project, (path.clone(), texmf.clone()));
        worker.handle_job_guarded(WorkerJob::Close { path: path.clone() });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        worker.handle_job_guarded(WorkerJob::Edit {
            stamp: 2,
            uri: path_to_uri(&path).unwrap(),
            path: path.clone(),
            text: Arc::new(TextBuffer::new(
                "reopened and edited",
                PositionEncoding::Utf16,
            )),
            version: 2,
            opened: true,
            kind: FileKind::Tex,
            rules: RuleSelection::all(),
            build: Default::default(),
            texmf,
            declarations: Default::default(),
            exclude: ExcludeFilter::none(),
            edits: None,
        });
        let file = worker.db.lookup_file(&path).unwrap();
        assert_eq!(worker.db.snapshot().file_text(file), "reopened and edited");
        assert!(
            worker.file_acquisition.rx.try_recv().is_err(),
            "IO is still paused after the source write"
        );
        release_tx.send(()).unwrap();
        while !worker.files_inflight.is_empty() {
            let completed = worker
                .file_acquisition
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            worker.complete_files(completed);
        }
        assert_eq!(worker.db.snapshot().file_text(file), "reopened and edited");
        worker.db.close_overlay(project, &path).unwrap();
        assert_eq!(worker.db.snapshot().file_text(file), "disk");
    }

    #[test]
    fn recreated_installed_source_refreshes_only_projects_with_observed_misses() {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, FileInputs, LocationKind, LocationObservation,
            Observation,
        };
        let dir = tempfile::tempdir().unwrap();
        let installed_root = dir.path().join("installed");
        std::fs::create_dir(&installed_root).unwrap();
        let source = installed_root.join("restored.sty");
        let unrelated = installed_root.join("unrelated.sty");
        let roots: Vec<_> = ["first", "second", "unobserved"]
            .into_iter()
            .map(|name| dir.path().join(name))
            .collect();
        let mut db = IncrementalDatabase::default();
        let projects = ProjectRegistry::from_roots(&mut db, &roots);
        let installed: InstalledPackages = crate::texmf::TexmfConfig {
            roots: vec![installed_root],
            explicit_only: true,
            use_kpsewhich: false,
            ..Default::default()
        }
        .into();
        assert!(installed.index().by_name.is_empty());
        let mut owners = Vec::new();
        let mut anchors = Vec::new();
        for (index, root) in roots.iter().enumerate() {
            let main = root.join("main.tex");
            let project = projects.select(&main, None).unwrap();
            db.apply_project_change(project, &main, "text", None)
                .unwrap();
            if index < 2 {
                let token = db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .unwrap();
                db.apply_external_inputs(
                    token,
                    ExternalInputs::Files(FileInputs {
                        locations: vec![(
                            source.clone(),
                            LocationObservation {
                                kind: Observation::Absent,
                                ..Default::default()
                            },
                        )],
                        file_resolution: vec![(source.clone(), Observation::Absent)],
                        ..Default::default()
                    }),
                )
                .unwrap();
                owners.push(project);
            }
            anchors.push((project, (main, installed.clone())));
        }
        let pool = task_pool::TaskPool::new("restored-installed-source", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        worker.discovery_anchors.extend(anchors);
        std::fs::write(&source, "restored source").unwrap();
        worker.apply_watched_change(&source, false);
        assert_eq!(worker.files_inflight, owners.iter().copied().collect());
        while !worker.files_inflight.is_empty() {
            let completed = worker
                .file_acquisition
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(owners.contains(&completed.project));
            worker.complete_files(completed);
        }
        for project in owners {
            let snapshot = worker.db.snapshot_for(project).unwrap();
            let file = snapshot
                .lookup_file(&source)
                .expect("restored installed source");
            assert_eq!(snapshot.file_text(file), "restored source");
            assert_eq!(
                snapshot.location_kind(&source),
                Observation::Present(LocationKind::File)
            );
            let resolved = snapshot.resolve_file(&tex_ls_analysis::external::FileCandidates::new(
                source.to_str().unwrap(),
                &[],
                false,
                None,
            ));
            assert_eq!(resolved.target, Observation::Present(source.clone()));
        }
        std::fs::write(&unrelated, "unrequested source").unwrap();
        worker.apply_watched_change(&unrelated, false);
        assert!(
            worker.files_inflight.is_empty(),
            "unobserved installation files must not be acquired"
        );
    }

    #[test]
    fn deleted_watched_source_cannot_be_revived_by_an_older_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tex");
        std::fs::write(&path, "old disk").unwrap();
        let pool = task_pool::TaskPool::new("deleted-source-reader", 1);
        let (tx, _rx) = unbounded();
        let (done_tx, _done_rx) = unbounded();
        let mut db = IncrementalDatabase::default();
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let project = db.project_id();
        db.set_backing(project, &path, Some("old disk".into_source_text()))
            .unwrap();
        let mut worker = test_worker(
            db,
            projects,
            tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let texmf = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        worker
            .discovery_anchors
            .insert(project, (path.clone(), texmf));
        worker.refresh_backing(project, &path, false);
        let old = worker
            .file_acquisition
            .rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        worker.apply_watched_change(&path, true);
        worker.complete_files(old);
        assert!(worker.db.lookup_file(&path).is_none());
        while !worker.files_inflight.is_empty() {
            let completed = worker
                .file_acquisition
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            worker.complete_files(completed);
        }
        assert!(worker.db.lookup_file(&path).is_none());
    }

    #[test]
    fn observation_retirement_keeps_missing_configured_build_root() {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, FileInputs, LocationObservation, Observation,
        };
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("chapter.tex");
        let root = dir.path().join("missing-root.tex");
        let pool = task_pool::TaskPool::new("configured-root-observations", 1);
        let (out_tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        db.apply_change(&main, "chapter", None);
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            out_tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        worker.compiler_acquisition.prepare(
            &mut worker.db,
            project,
            &main,
            &BuildConfig {
                root: Some(root.clone()),
                ..Default::default()
            },
            &worker.file_acquisition,
        );
        let locations = std::iter::once(root.clone())
            .chain((0..600).map(|index| dir.path().join(format!("retired-{index}.tex"))))
            .map(|path| {
                (
                    path,
                    LocationObservation {
                        kind: Observation::Absent,
                        ..Default::default()
                    },
                )
            })
            .collect();
        let token = worker
            .db
            .begin_external_refresh(project, ExternalInputKind::Files)
            .unwrap();
        worker
            .db
            .apply_external_inputs(
                token,
                ExternalInputs::Files(FileInputs {
                    locations,
                    ..Default::default()
                }),
            )
            .unwrap();
        worker.retain_observation_scope(project, &[]);
        assert_eq!(
            worker.db.snapshot().location_kind(&root),
            Observation::Absent,
            "a configured compilation root remains a current negative target"
        );
    }

    #[test]
    fn bibliography_target_churn_retires_obsolete_native_search_cache() {
        assert_native_target_churn_is_bounded(true);
    }

    #[test]
    fn bibliography_target_churn_retires_obsolete_native_failure_cache() {
        assert_native_target_churn_is_bounded(false);
    }

    fn assert_native_target_churn_is_bounded(check_searches: bool) {
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, FileInputs, LocationObservation, Observation,
        };
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let pool = task_pool::TaskPool::new("native-target-churn", 1);
        let (tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        db.apply_change(&main, "text", None);
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let mut latest = PathBuf::new();
        for index in 0..512 {
            latest = dir.path().join(format!("missing-{index}.bib"));
            worker
                .db
                .apply_project_change(
                    project,
                    &main,
                    format!("\\bibliography{{missing-{index}}}"),
                    None,
                )
                .unwrap();
            let token = worker
                .db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .unwrap();
            worker.complete_files(file_acquisition::Completed {
                project,
                token,
                inputs: ExternalInputs::Files(FileInputs {
                    locations: vec![(
                        latest.clone(),
                        LocationObservation {
                            kind: Observation::Error("unreadable target".into()),
                            ..Default::default()
                        },
                    )],
                    file_resolution: vec![(latest.clone(), Observation::Absent)],
                    ..Default::default()
                }),
                aliases: vec![(latest.clone(), None)],
            });
        }
        assert_eq!(
            worker.bib_lookups[&project].get(&latest),
            Some(&None),
            "the active search miss remains cached"
        );
        assert_eq!(
            worker.acquisition_failures[&project]
                .get(&latest)
                .map(String::as_str),
            Some("unreadable target"),
            "the active acquisition error remains visible"
        );
        let retained = if check_searches {
            worker.bib_lookups[&project].len()
        } else {
            worker.acquisition_failures[&project].len()
        };
        assert!(
            retained <= 300,
            "obsolete target histories must be bounded: retained {retained} for one active target"
        );
    }

    #[test]
    fn removing_workspace_root_retires_native_acquisition_ownership() {
        use tex_ls_analysis::external::ExternalInputKind;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let retired_path = root.join("root.tex");
        let retained_path = dir.path().join("fallback.tex");
        std::fs::write(&retired_path, "root").unwrap();
        std::fs::write(&retained_path, "fallback").unwrap();
        let pool = task_pool::TaskPool::new("root-cache-retirement", 1);
        let (tx, _) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        let retained = db.project_id();
        let projects = ProjectRegistry::from_roots(&mut db, std::slice::from_ref(&root));
        let retired = projects.select(&retired_path, None).unwrap();
        let mut worker = test_worker(
            db,
            projects,
            tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        for (project, path) in [(retained, &retained_path), (retired, &retired_path)] {
            worker.file_acquisition.submit(file_acquisition::Plan {
                project,
                token: worker
                    .db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .unwrap(),
                locations: Default::default(),
                sources: std::collections::BTreeSet::from([path.clone()]),
                directories: Default::default(),
                scans: Vec::new(),
                known: Default::default(),
                bibliographies: Vec::new(),
                excluded_roots: Vec::new(),
            });
            worker
                .file_acquisition
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let installed: InstalledPackages = crate::texmf::TexmfConfig {
                enabled: false,
                roots: vec![path.parent().unwrap().to_owned()],
                ..Default::default()
            }
            .into();
            worker
                .discovery_anchors
                .insert(project, (path.clone(), installed.clone()));
            worker.loading_indexes.push(installed);
            worker
                .acquisition_failures
                .entry(project)
                .or_default()
                .insert(path.clone(), "old error".into());
            worker
                .extra_sources
                .entry(project)
                .or_default()
                .insert(path.clone());
            worker
                .extra_locations
                .entry(project)
                .or_default()
                .insert(path.clone());
            worker.pending_scans.insert(
                project,
                vec![(path.clone(), root.clone(), ExcludeFilter::none())],
            );
        }
        assert_eq!(worker.file_acquisition.physical_source_count(), 2);
        for project in [retained, retired] {
            let snapshot = worker.db.snapshot_for(project).unwrap();
            worker.project_acquisition_prefix(&snapshot);
        }
        assert_eq!(worker.acquisition_project_prefixes.len(), 2);
        let retired_prefix = Arc::downgrade(&worker.acquisition_project_prefixes[&retired].1);
        worker.handle_job_guarded(WorkerJob::WorkspaceRoots {
            roots: Vec::new(),
            open: Vec::new(),
        });
        assert!(worker.db.snapshot_for(retired).is_err());
        assert_eq!(worker.acquisition_project_prefixes.len(), 1);
        assert!(worker.acquisition_project_prefixes.contains_key(&retained));
        assert!(retired_prefix.upgrade().is_none());
        assert_eq!(
            worker.file_acquisition.physical_source_count(),
            1,
            "removed projects release IO identity history"
        );
        assert!(!worker.discovery_anchors.contains_key(&retired));
        assert!(!worker.acquisition_failures.contains_key(&retired));
        assert!(!worker.extra_sources.contains_key(&retired));
        assert!(!worker.extra_locations.contains_key(&retired));
        assert!(!worker.pending_scans.contains_key(&retired));
        assert!(worker.discovery_anchors.contains_key(&retained));
        assert!(worker.acquisition_failures.contains_key(&retained));
        assert_eq!(worker.loading_indexes.len(), 1);
        assert_eq!(
            worker.loading_indexes[0],
            worker.discovery_anchors[&retained].1
        );
    }

    #[test]
    fn closing_an_absent_source_withdraws_its_compiler_watch_after_backing_refresh() {
        assert_closed_source_compiler_scope(false);
    }

    #[test]
    fn closing_an_unacquired_disk_source_preserves_its_compiler_watch_after_backing_refresh() {
        assert_closed_source_compiler_scope(true);
    }

    fn assert_closed_source_compiler_scope(has_backing: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tex");
        if has_backing {
            std::fs::write(&path, "\\documentclass{article}\\label{intro}disk").unwrap();
        }
        std::fs::write(path.with_extension("aux"), "\\newlabel{intro}{{1}{1}}\n").unwrap();
        let pool = task_pool::TaskPool::new("compiler-retirement-reader", 1);
        let (tx, rx) = unbounded();
        let (done_tx, _) = unbounded();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        db.replace_overlay(project, &path, "\\documentclass{article}\\label{intro}")
            .unwrap();
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let mut worker = test_worker(
            db,
            projects,
            tx,
            done_tx,
            pool.spawner(),
            Default::default(),
        );
        let disabled = crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        }
        .into();
        worker
            .discovery_anchors
            .insert(project, (path.clone(), disabled));
        worker
            .parent_keys
            .insert((project, path.clone()), "source references".into());
        worker.seed_dir(&path, &ExcludeFilter::none());
        worker.acquire_compiler(project, &path, &Default::default());
        while worker.compiler_acquisition.pending_count() != 0 {
            let completed = worker
                .file_acquisition
                .compiler_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            worker.compiler_acquisition.complete(
                &mut worker.db,
                completed,
                &worker.out_tx,
                &worker.file_acquisition,
            );
        }
        assert!(rx.try_iter().any(|message| {
            matches!(message, Outbound::WatchArtifacts { source, paths } if source == path && !paths.is_empty())
        }));
        worker.handle_job_guarded(WorkerJob::Close { path: path.clone() });
        assert!(
            worker.db.lookup_file(&path).is_none(),
            "the close has no acquired backing yet"
        );
        while !worker.files_inflight.is_empty() {
            let completed = worker
                .file_acquisition
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            worker.complete_files(completed);
        }
        while worker.compiler_acquisition.pending_count() != 0 {
            let completed = worker
                .file_acquisition
                .compiler_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            worker.compiler_acquisition.complete(
                &mut worker.db,
                completed,
                &worker.out_tx,
                &worker.file_acquisition,
            );
        }
        assert_eq!(worker.db.lookup_file(&path).is_some(), has_backing);
        assert_eq!(
            worker.parent_keys.contains_key(&(project, path.clone())),
            has_backing,
            "source cache ownership follows confirmed backing, not the pending Close gap"
        );
        assert_eq!(
            worker
                .seeded_dirs
                .contains(&(project, path.parent().unwrap().to_owned())),
            has_backing,
            "a directory scan marker requires a surviving source owner"
        );
        let withdrawn = rx.try_iter().any(|message| {
            matches!(message, Outbound::WatchArtifacts { source, paths } if source == path && paths.is_empty())
        });
        assert_eq!(
            withdrawn, !has_backing,
            "only confirmed absent backing retires the scope"
        );
        assert_eq!(
            worker.db.snapshot().compiler_artifacts().next().is_some(),
            has_backing
        );
    }

    #[test]
    fn cold_label_completion_waits_for_compiler_numbers_before_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tex");
        std::fs::write(path.with_extension("aux"), "\\newlabel{intro}{{12}{1}}\n").unwrap();
        let pool = task_pool::TaskPool::new("cold-label-reader", 1);
        let (tx, rx) = unbounded();
        let (done_tx, _done_rx) = unbounded();
        let mut db = IncrementalDatabase::default();
        db.apply_change(
            &path,
            "\\documentclass{article}\\section{Intro}\\label{intro}\n\\ref{in}",
            None,
        );
        let projects = ProjectRegistry::from_roots(&mut db, &[]);
        let project = db.project_id();
        let requests = Arc::new(std::sync::Mutex::new(request_ids::RequestIds::default()));
        let id = requests
            .lock()
            .unwrap()
            .receive(RequestId::from(15))
            .unwrap();
        let mut worker = test_worker(db, projects, tx, done_tx, pool.spawner(), requests);
        let (started_tx, started_rx) = unbounded();
        let (release_tx, release_rx) = unbounded();
        let mut first = true;
        worker.file_acquisition = file_acquisition::FileAcquisition::with_before_read(move || {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
        });
        worker.acquire_compiler(project, &path, &BuildConfig::default());
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        worker.handle_job_guarded(WorkerJob::Completion {
            id: id.clone(),
            uri: path_to_uri(&path).unwrap(),
            position: Position::new(1, 7),
            texmf: crate::texmf::TexmfConfig {
                enabled: false,
                ..Default::default()
            }
            .into(),
        });
        assert_eq!(worker.deferred_files.len(), 1);
        assert!(rx.try_iter().all(|message| !matches!(
            message,
            Outbound::ReadStarted { .. } | Outbound::Response(_)
        )));
        release_tx.send(()).unwrap();
        let completed = worker
            .file_acquisition
            .compiler_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        worker.compiler_acquisition.complete(
            &mut worker.db,
            completed,
            &worker.out_tx,
            &worker.file_acquisition,
        );
        while !worker.files_inflight.is_empty() {
            let completed = worker
                .file_acquisition
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            worker.complete_files(completed);
        }
        worker.resume_deferred();
        let response = loop {
            if let Outbound::Response(response) =
                rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
            {
                break response;
            }
        };
        assert_eq!(response.id, id);
        let result = response.response_result.unwrap();
        let item = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == "intro")
            .unwrap();
        assert!(item["detail"].as_str().unwrap().contains("Section 12"));
    }
}
