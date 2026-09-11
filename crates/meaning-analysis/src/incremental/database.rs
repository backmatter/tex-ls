//! Tracked project inputs, shared physical sources, and storage lifetimes.
use super::*;
use std::collections::HashSet;

/// Host-supplied backing contents and editor overlays share one physical source.
/// The sets record which project memberships each layer keeps alive.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct SourceLayers {
    pub backing: Option<Arc<SourceText>>,
    pub backing_projects: HashSet<ProjectId>,
    pub overlay: Option<Arc<SourceText>>,
    pub overlay_projects: HashSet<ProjectId>,
}

impl SourceLayers {
    pub fn effective(&self) -> Option<&Arc<SourceText>> {
        self.overlay.as_ref().or(self.backing.as_ref())
    }

    fn retain_projects(&mut self, mut retain: impl FnMut(&ProjectId) -> bool) {
        self.backing_projects.retain(&mut retain);
        self.overlay_projects.retain(retain);
        if self.backing_projects.is_empty() {
            self.backing = None;
        }
        if self.overlay_projects.is_empty() {
            self.overlay = None;
        }
    }
}

#[salsa::db]
#[derive(Clone)]
pub struct Database {
    pub(super) storage: salsa::Storage<Self>,
    pub(super) epoch: u64,
    pub(super) revision: u64,
    pub(super) default_project: ProjectId,
    pub(super) projects: Arc<HashMap<ProjectId, ProjectInput>>,
    pub(super) identities: Arc<HashMap<SourceId, HashMap<ProjectId, SourceInput>>>,
    pub(super) files: Arc<HashMap<PathBuf, SourceId>>,
    pub(super) acquisitions: HashMap<(ProjectId, crate::external::ExternalInputKind), u64>,
    pub(super) generations: HashMap<ProjectId, crate::external::ExternalGenerations>,
    pub(super) source_layers: Arc<HashMap<SourceId, SourceLayers>>,
    source_declarations: Arc<HashMap<(ProjectId, PathBuf), ResolvedDeclarations>>,
    pub(super) query_log_enabled: Arc<AtomicBool>,
    pub(super) query_log: Arc<Mutex<Vec<QueryLogEntry>>>,
    pub(super) reparse_cache: Arc<Mutex<ReparseCache>>,
}

impl Default for Database {
    fn default() -> Self {
        let project = ProjectId::new();
        let mut db = Self::empty(project);
        db.insert_project(project, ResolvedDeclarations::default());
        db
    }
}
impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}
pub(super) fn recover_poison<T>(err: std::sync::PoisonError<T>) -> T {
    err.into_inner()
}

pub(crate) use crate::source::normalize_path;

impl Database {
    fn empty(default_project: ProjectId) -> Self {
        static NEXT_EPOCH: AtomicU64 = AtomicU64::new(0);
        Self {
            storage: salsa::Storage::new(None),
            epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            revision: 0,
            default_project,
            projects: Arc::default(),
            identities: Arc::default(),
            files: Arc::default(),
            source_layers: Arc::default(),
            acquisitions: HashMap::new(),
            generations: HashMap::new(),
            source_declarations: Arc::default(),
            query_log_enabled: Arc::new(AtomicBool::new(false)),
            query_log: Arc::default(),
            reparse_cache: Arc::default(),
        }
    }
    fn insert_project(&mut self, id: ProjectId, declarations: ResolvedDeclarations) {
        let project = ProjectInput::builder(
            id,
            Vec::new(),
            declarations,
            Default::default(),
            Arc::default(),
            Default::default(),
        )
        .identity_durability(salsa::Durability::HIGH)
        .declarations_durability(salsa::Durability::HIGH)
        .files_durability(salsa::Durability::MEDIUM)
        .locations_durability(salsa::Durability::MEDIUM)
        .installed_durability(salsa::Durability::MEDIUM)
        .compiler_durability(salsa::Durability::MEDIUM)
        .new(self);
        Arc::make_mut(&mut self.projects).insert(id, project);
    }
    pub fn create_project(&mut self, declarations: ResolvedDeclarations) -> ProjectId {
        let id = ProjectId::new();
        self.insert_project(id, declarations);
        self.revision += 1;
        id
    }
    pub(super) fn project(&self, id: ProjectId) -> ProjectInput {
        *self
            .projects
            .get(&id)
            .expect("project belongs to this snapshot")
    }
    pub(super) fn find_input(&self, id: SourceId, project: ProjectId) -> Option<SourceInput> {
        self.identities.get(&id)?.get(&project).copied()
    }
    pub(super) fn input(&self, id: SourceId, project: ProjectId) -> SourceInput {
        self.find_input(id, project)
            .expect("source belongs to this project snapshot")
    }
    pub fn add_file(&mut self, text: impl IntoSourceText) -> SourceInput {
        static NEXT_MEMORY_FILE: AtomicU64 = AtomicU64::new(0);
        let n = NEXT_MEMORY_FILE.fetch_add(1, Ordering::Relaxed);
        self.upsert_file(Path::new(&format!("<mem>/{n}.tex")), text)
    }
    pub fn declarations_for_in(&self, project: ProjectId, path: &Path) -> &ResolvedDeclarations {
        self.source_declarations
            .get(&(project, normalize_path(path)))
            .unwrap_or_else(|| self.project(project).declarations(self))
    }
    pub fn set_declarations(&mut self, declared: ResolvedDeclarations) -> bool {
        self.set_project_declarations(self.default_project, declared)
    }
    pub fn set_project_declarations(
        &mut self,
        project: ProjectId,
        declared: ResolvedDeclarations,
    ) -> bool {
        let input = self.project(project);
        if input.declarations(self) == &declared {
            return false;
        }
        input
            .set_declarations(self)
            .with_durability(salsa::Durability::HIGH)
            .to(declared.clone());
        for (path, file) in self.tracked_files_in(project) {
            if !self.source_declarations.contains_key(&(project, path))
                && file.declarations(self) != &declared
            {
                file.set_declarations(self)
                    .with_durability(salsa::Durability::HIGH)
                    .to(declared.clone());
            }
        }
        self.revision += 1;
        true
    }
    pub fn set_source_declarations(&mut self, path: &Path, declared: ResolvedDeclarations) -> bool {
        self.set_source_declarations_in(self.default_project, path, declared)
    }
    pub fn set_source_declarations_in(
        &mut self,
        project: ProjectId,
        path: &Path,
        declared: ResolvedDeclarations,
    ) -> bool {
        let key = normalize_path(path);
        if self.source_declarations.get(&(project, key.clone())) == Some(&declared) {
            return false;
        }
        if let Some(file) = self.lookup_file_in(project, &key)
            && file.declarations(self) != &declared
        {
            file.set_declarations(self)
                .with_durability(salsa::Durability::HIGH)
                .to(declared.clone());
        }
        Arc::make_mut(&mut self.source_declarations).insert((project, key), declared);
        self.revision += 1;
        true
    }
    pub fn clear_source_declarations(&mut self, path: &Path) -> bool {
        self.clear_source_declarations_in(self.default_project, path)
    }
    pub fn clear_source_declarations_in(&mut self, project: ProjectId, path: &Path) -> bool {
        let path = normalize_path(path);
        if Arc::make_mut(&mut self.source_declarations)
            .remove(&(project, path.clone()))
            .is_none()
        {
            return false;
        }
        if let Some(file) = self.lookup_file_in(project, &path) {
            let declared = self.project(project).declarations(self).clone();
            if file.declarations(self) != &declared {
                file.set_declarations(self)
                    .with_durability(salsa::Durability::HIGH)
                    .to(declared);
            }
        }
        self.revision += 1;
        true
    }
    pub fn set_bibliography_alias(&mut self, requested: &Path, actual: &Path) -> bool {
        self.set_bibliography_alias_in(self.default_project, requested, actual)
    }
    pub fn set_bibliography_alias_in(
        &mut self,
        project: ProjectId,
        requested: &Path,
        actual: &Path,
    ) -> bool {
        let requested = normalize_path(requested);
        let actual = normalize_path(actual);
        self.set_bibliography_observation(
            project,
            requested,
            crate::external::Observation::Present(actual),
        )
    }
    pub fn bibliography_alias_in(&self, project: ProjectId, requested: &Path) -> Option<&Path> {
        let location = self
            .project(project)
            .locations(self)
            .get(&normalize_path(requested))?;
        match location.bibliography(self) {
            crate::external::Observation::Present(path) => Some(path.as_path()),
            _ => None,
        }
    }
    pub fn upsert_file(&mut self, path: &Path, text: impl IntoSourceText) -> SourceInput {
        self.upsert_file_in(self.default_project, path, text)
    }
    /// Source bytes are shared across project instances; declarations belong to
    /// each instance. Changing bytes updates every existing instance atomically.
    pub fn upsert_file_in(
        &mut self,
        project: ProjectId,
        path: &Path,
        text: impl IntoSourceText,
    ) -> SourceInput {
        self.project(project);
        let key = normalize_path(path);
        let mut text = text.into_source_text();
        let source = self.files.get(&key).copied().unwrap_or_else(SourceId::new);
        let instances: Vec<_> = self
            .identities
            .get(&source)
            .into_iter()
            .flat_map(|instances| instances.values().copied())
            .collect();
        // A revision stamps this project membership as well as its bytes. Use
        // the writer clock so detach/rejoin cannot resurrect an old token while
        // another project keeps the physical SourceId alive.
        let version = SourceVersion {
            project,
            source,
            revision: self.revision + 1,
        };
        if let Some(&previous) = instances.first() {
            if *previous.text(self) == text {
                text = previous.text(self).clone();
            } else {
                for file in &instances {
                    file.set_text(self).to(text.clone());
                    file.set_revision(self).to(version.revision);
                }
                self.revision += 1;
            }
        }
        if let Some(file) = self.find_input(source, project) {
            return file;
        }
        let file = self.insert_file(project, key, text, version);
        self.sync_project_files(project);
        self.revision += 1;
        file
    }
    fn insert_file(
        &mut self,
        project: ProjectId,
        key: PathBuf,
        text: Arc<SourceText>,
        version: SourceVersion,
    ) -> SourceInput {
        let file = SourceInput::builder(
            key.clone(),
            text,
            self.declarations_for_in(project, &key).clone(),
            version.source,
            version.revision,
            project,
        )
        .path_durability(salsa::Durability::HIGH)
        .project_durability(salsa::Durability::HIGH)
        .new(self);
        Arc::make_mut(&mut self.files).insert(key, version.source);
        Arc::make_mut(&mut self.identities)
            .entry(version.source)
            .or_default()
            .insert(project, file);
        file
    }
    pub fn stage_source_edits(&self, source: SourceId, edits: Option<Vec<Edit>>) {
        for file in self.identities[&source].values() {
            self.reparse_stage_edits(*file, edits.clone());
        }
    }
    pub fn tracked_files_in(&self, project: ProjectId) -> Vec<(PathBuf, SourceInput)> {
        let mut files: Vec<_> = self
            .files
            .iter()
            .filter_map(|(path, &id)| {
                self.find_input(id, project)
                    .map(|file| (path.clone(), file))
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }
    fn sync_project_files(&mut self, project: ProjectId) {
        let files: Vec<_> = self
            .tracked_files_in(project)
            .into_iter()
            .map(|(_, file)| file)
            .collect();
        let input = self.project(project);
        if input.files(self) != &files {
            input
                .set_files(self)
                .with_durability(salsa::Durability::MEDIUM)
                .to(files);
        }
    }
    pub fn lookup_file(&self, path: &Path) -> Option<SourceInput> {
        self.lookup_file_in(self.default_project, path)
    }
    pub fn lookup_file_in(&self, project: ProjectId, path: &Path) -> Option<SourceInput> {
        self.find_input(*self.files.get(&normalize_path(path))?, project)
    }
    pub fn remove_file(&mut self, path: &Path) -> Option<SourceInput> {
        let file = self.lookup_file(path)?;
        self.remove_files(&[path.to_owned()]);
        Some(file)
    }
    pub fn remove_files(&mut self, paths: &[PathBuf]) {
        self.remove_files_in(self.default_project, paths);
    }
    pub fn remove_files_in(&mut self, project: ProjectId, paths: &[PathBuf]) {
        let removed: std::collections::HashSet<_> = paths
            .iter()
            .map(|path| normalize_path(path))
            .filter(|path| self.lookup_file_in(project, path).is_some())
            .map(|path| (project, path))
            .collect();
        if !removed.is_empty() {
            self.rebuild(&removed, None);
        }
    }
    pub fn remove_project(&mut self, project: ProjectId) -> bool {
        if !self.projects.contains_key(&project) {
            return false;
        }
        self.rebuild(&std::collections::HashSet::new(), Some(project));
        true
    }
    pub(super) fn rebuild(
        &mut self,
        removed: &std::collections::HashSet<(ProjectId, PathBuf)>,
        removed_project: Option<ProjectId>,
    ) {
        let default_project = if removed_project == Some(self.default_project) {
            ProjectId::new()
        } else {
            self.default_project
        };
        let mut next = Self::empty(default_project);
        next.acquisitions = self
            .acquisitions
            .iter()
            .filter(|((project, _), _)| Some(*project) != removed_project)
            .map(|(key, value)| (*key, *value))
            .collect();
        next.generations = self
            .generations
            .iter()
            .filter(|(project, _)| Some(**project) != removed_project)
            .map(|(key, value)| (*key, *value))
            .collect();
        for (source, layers) in self.source_layers.iter() {
            let file = self.identities[source]
                .values()
                .next()
                .expect("live source");
            let path = file.path(self);
            let mut layers = layers.clone();
            layers.retain_projects(|project| {
                Some(*project) != removed_project && !removed.contains(&(*project, path.clone()))
            });
            if layers.effective().is_some() {
                Arc::make_mut(&mut next.source_layers).insert(*source, layers);
            }
        }
        next.source_declarations = Arc::new(
            self.source_declarations
                .iter()
                .filter(|((project, path), _)| {
                    Some(*project) != removed_project
                        && !removed.contains(&(*project, path.clone()))
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        let mut projects: Vec<_> = self
            .projects
            .keys()
            .copied()
            .filter(|id| Some(*id) != removed_project)
            .collect();
        projects.sort_unstable();
        for project in projects {
            let input = self.project(project);
            next.insert_project(project, input.declarations(self).clone());
            let locations = input
                .locations(self)
                .iter()
                .map(|(path, location)| {
                    let mapped = LocationInput::new(
                        &next,
                        location.observation(self).clone(),
                        location.bibliography(self).clone(),
                    );
                    (path.clone(), mapped)
                })
                .collect();
            let installed = input.installed(self).clone();
            let compiler = input
                .compiler(self)
                .iter()
                .map(|(path, artifact)| {
                    (
                        path.clone(),
                        CompilerInput::new(&next, artifact.artifact(self).clone()),
                    )
                })
                .collect();
            let mapped = next.project(project);
            mapped.set_locations(&mut next).to(locations);
            mapped.set_installed(&mut next).to(installed);
            mapped.set_compiler(&mut next).to(compiler);
            for (path, file) in self.tracked_files_in(project) {
                if removed.contains(&(project, path.clone())) {
                    continue;
                }
                let source = *file.identity(self);
                let text = next
                    .source_layers
                    .get(&source)
                    .and_then(SourceLayers::effective)
                    .unwrap_or_else(|| file.text(self))
                    .clone();
                let changed = &text != file.text(self);
                let mapped = next.insert_file(
                    project,
                    path,
                    text,
                    SourceVersion {
                        project,
                        source,
                        revision: if changed {
                            self.revision + 1
                        } else {
                            *file.revision(self)
                        },
                    },
                );
                if !changed
                    && let Some(state) = self
                        .reparse_cache
                        .lock()
                        .unwrap_or_else(recover_poison)
                        .files
                        .get(&file)
                        .cloned()
                {
                    next.reparse_cache
                        .lock()
                        .unwrap_or_else(recover_poison)
                        .files
                        .insert(mapped, state);
                }
            }
            next.sync_project_files(project);
        }
        if !next.projects.contains_key(&default_project) {
            next.insert_project(default_project, ResolvedDeclarations::default());
        }
        next.query_log_enabled = self.query_log_enabled.clone();
        next.query_log = self.query_log.clone();
        next.revision = self.revision + 1;
        *self = next;
    }
    pub fn reparse_cache_len(&self) -> usize {
        self.reparse_cache
            .lock()
            .unwrap_or_else(recover_poison)
            .files
            .len()
    }

    /// The text currently tracked for `file`.
    pub fn file_text(&self, file: SourceInput) -> &str {
        file.text(self)
    }

    /// Compare supplied contents with the snapshot source, skipping a byte scan
    /// when both views refer to the same allocation.
    pub fn text_is_current(&self, file: SourceInput, text: &str) -> bool {
        let tracked: &str = file.text(self);
        let same_bytes = std::ptr::eq(tracked.as_ptr(), text.as_ptr());
        (same_bytes && tracked.len() == text.len()) || tracked == text
    }

    /// The path `file` is tracked under.
    pub fn file_path(&self, file: SourceInput) -> &Path {
        file.path(self)
    }

    /// Parse diagnostics for `file` (empty when it parses cleanly).
    pub fn parse_diagnostics(&self, file: SourceInput) -> &[ParseDiagnosticData] {
        parse_diagnostics(self, file)
    }

    /// A fresh `SyntaxNode` over the cached parse tree.
    pub fn parsed_tree(&self, file: SourceInput) -> SyntaxNode {
        parsed_tree_root(self, file)
    }

    /// The file's range-free inclusion edges.
    pub fn include_edges(&self, file: SourceInput) -> &[IncludeEdgeKey] {
        include_edges(self, file)
    }

    /// The file's per-file label/reference model.
    pub fn semantic_model(&self, file: SourceInput) -> &SemanticModel {
        semantic_model(self, file)
    }

    /// The file's scanned user-definition signatures.
    pub fn document_signatures(&self, file: SourceInput) -> &SignatureDb {
        document_signatures(self, file)
    }

    /// The file's `.dtx` documentation↔code associations.
    pub fn doc_associations(&self, file: SourceInput) -> &[DocAssociation] {
        doc_associations(self, file)
    }

    /// The file's distinct, sorted `\label` names (the firewall feeding the
    /// cross-file resolver).
    pub fn file_labels(&self, file: SourceInput) -> &[SmolStr] {
        file_labels(self, file)
    }

    /// The file's distinct, sorted `\ref`-family key names (the reference firewall
    /// feeding the cross-file resolver for `unreferenced-label`).
    pub fn file_refs(&self, file: SourceInput) -> &[SmolStr] {
        file_refs(self, file)
    }

    /// The file's distinct, sorted glossary/acronym keys (the firewall feeding
    /// glossary key completion).
    pub fn file_glossary_keys(&self, file: SourceInput) -> &[SmolStr] {
        file_glossary_keys(self, file)
    }

    /// Whether `file` carries a `\documentclass` / `\begin{document}`.
    pub fn file_is_document_root(&self, file: SourceInput) -> bool {
        *file_is_document_root(self, file)
    }

    /// `.bib` parse diagnostics for `file` (empty when it parses cleanly).
    pub fn bib_parse_diagnostics(&self, file: SourceInput) -> &[ParseDiagnosticData] {
        bib_parse_diagnostics(self, file)
    }

    /// A fresh bib `SyntaxNode` over the cached `.bib` parse tree.
    pub fn parsed_bib_tree(&self, file: SourceInput) -> BibSyntaxNode {
        parsed_bib_tree_root(self, file)
    }

    /// The file's per-file bib model (entries, `@string` defs/uses).
    pub fn bib_semantic_model(&self, file: SourceInput) -> &BibModel {
        bib_semantic_model(self, file)
    }

    pub fn clear_query_log(&self) {
        self.query_log_enabled.store(true, Ordering::Relaxed);
        self.query_log.lock().unwrap_or_else(recover_poison).clear();
    }

    pub fn query_log(&self) -> Vec<QueryLogEntry> {
        self.query_log.lock().unwrap_or_else(recover_poison).clone()
    }
}

#[salsa::db]
impl IncrementalDb for Database {
    fn source_input(&self, id: SourceId, project: ProjectId) -> SourceInput {
        self.input(id, project)
    }
    fn project_input(&self, id: ProjectId) -> ProjectInput {
        self.project(id)
    }
    fn reparse_state(&self, file: SourceInput) -> FileReparseState {
        self.reparse_cache
            .lock()
            .unwrap_or_else(recover_poison)
            .files
            .get(&file)
            .cloned()
            .unwrap_or_default()
    }

    fn record_query(&self, entry: QueryLogEntry) {
        if !self.query_log_enabled.load(Ordering::Relaxed) {
            return;
        }
        self.query_log
            .lock()
            .unwrap_or_else(recover_poison)
            .push(entry);
    }

    fn reparse_bib_store(
        &self,
        file: SourceInput,
        prev: Arc<PrevBibParse>,
        consumed: usize,
        generation: u64,
    ) {
        let mut cache = self.reparse_cache.lock().unwrap_or_else(recover_poison);
        let state = cache.files.entry(file).or_default();
        if state.generation != generation {
            return;
        }
        state.generation += 1;
        state.bib_prev = Some(prev);
        state.pending.drain(..consumed.min(state.pending.len()));
    }

    fn reparse_prev(&self, file: SourceInput) -> Option<Arc<PrevParse>> {
        self.reparse_cache
            .lock()
            .unwrap_or_else(recover_poison)
            .files
            .get(&file)
            .and_then(|state| state.prev.clone())
    }

    fn reparse_stage_edits(&self, file: SourceInput, edits: Option<Vec<Edit>>) {
        let mut cache = self.reparse_cache.lock().unwrap_or_else(recover_poison);

        let Some(edits) = edits else {
            // An unknown transform invalidates the chain. A file with no entry
            // has no chain to clear, and must not gain one: the language server
            // pairs every `upsert_file` with a stage, and most of those writes are
            // project seeding, which would otherwise mint an entry per sibling.
            if let Some(state) = cache.files.get_mut(&file) {
                state.pending.clear();
                state.prev = None;
                state.bib_prev = None;
                state.generation += 1;
            }
            return;
        };

        let Some(state) = cache.files.get_mut(&file) else {
            return;
        };
        state.generation += 1;
        let base = state
            .prev
            .as_ref()
            .map(|base| &*base.text)
            .or_else(|| state.bib_prev.as_ref().map(|base| &*base.text));
        let Some(base) = base else {
            return;
        };
        if edits.is_empty() {
            return;
        }
        state.pending.extend(edits);
        if let Some(merged) = crate::parser::edit::coalesce_touching_edits(base, &state.pending) {
            state.pending = vec![merged];
        } else {
            // A disjoint or invalid chain needs a full parse. Retain the current
            // source input, but release hints whose history would grow unread.
            state.pending.clear();
            state.prev = None;
            state.bib_prev = None;
        }
    }

    fn reparse_pending_edits(&self, file: SourceInput) -> Vec<Edit> {
        self.reparse_cache
            .lock()
            .unwrap_or_else(recover_poison)
            .files
            .get(&file)
            .map(|state| state.pending.clone())
            .unwrap_or_default()
    }

    fn reparse_store(
        &self,
        file: SourceInput,
        prev: Arc<PrevParse>,
        consumed: usize,
        generation: u64,
    ) {
        let mut cache = self.reparse_cache.lock().unwrap_or_else(recover_poison);
        let state = cache.files.entry(file).or_default();

        if state.generation != generation {
            return;
        }
        state.generation += 1;
        state.prev = Some(prev);
        // Drain the prefix the caller consumed, unconditionally — including when the
        // chain went unused. A chain kept back because it did not splice is stale
        // forever after: it describes a transform out of a text the base no longer
        // holds, so it would fail to verify on every later parse and poison them all.
        let consumed = consumed.min(state.pending.len());
        state.pending.drain(..consumed);
    }

    #[cfg(test)]
    fn reparse_evict(&self, file: SourceInput) {
        self.reparse_cache
            .lock()
            .unwrap_or_else(recover_poison)
            .files
            .remove(&file);
    }
}

#[salsa::db]
impl salsa::Database for Database {}
