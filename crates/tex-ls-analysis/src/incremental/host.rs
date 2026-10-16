//! Analysis writer and immutable read contexts. Salsa handles stay private.
use super::*;
use std::ops::Deref;

#[path = "external_inputs.rs"]
pub(super) mod external_inputs;
#[path = "source_layers.rs"]
mod source_layers;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(u64);
impl SourceId {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectId(u64);
impl ProjectId {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// An optimistic-write precondition for a source's project membership and bytes.
/// Revisions are monotonic stamps, not edit counts. Unrelated writes preserve
/// this token; detaching and rejoining the project replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceVersion {
    pub project: ProjectId,
    pub source: SourceId,
    pub revision: u64,
}

/// A captured analysis state, including the storage lifetime it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisRevision {
    pub project: ProjectId,
    pub epoch: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeError {
    ProjectRemoved,
    SourceRemoved,
    StaleRevision,
    InvalidEdit { index: usize },
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProjectRemoved => f.write_str("The project incarnation is no longer present"),
            Self::SourceRemoved => f.write_str("The source incarnation is no longer present"),
            Self::StaleRevision => f.write_str("The source changed since the update was prepared"),
            Self::InvalidEdit { index } => write!(
                f,
                "Edit {index} is outside the source or splits a UTF-8 character"
            ),
        }
    }
}
impl std::error::Error for ChangeError {}

#[derive(Clone)]
pub struct Analysis {
    cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
    inner: Database,
    project: ProjectId,
}
impl Default for Analysis {
    fn default() -> Self {
        let inner = Database::default();
        Self {
            project: inner.default_project,
            cancellation: None,
            inner,
        }
    }
}

#[derive(Default)]
pub struct IncrementalDatabase {
    read: Analysis,
}

impl Deref for IncrementalDatabase {
    type Target = Analysis;
    fn deref(&self) -> &Analysis {
        &self.read
    }
}

impl IncrementalDatabase {
    pub fn create_project(&mut self, declarations: ResolvedDeclarations) -> ProjectId {
        self.read.inner.create_project(declarations)
    }

    pub fn snapshot_for(&self, project: ProjectId) -> Result<Analysis, ChangeError> {
        if !self.read.inner.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        Ok(Analysis {
            cancellation: None,
            inner: self.read.inner.clone(),
            project,
        })
    }

    pub fn apply_project_change(
        &mut self,
        project: ProjectId,
        path: &Path,
        text: impl IntoSourceText,
        edits: Option<Vec<Edit>>,
    ) -> Result<SourceId, ChangeError> {
        if !self.read.inner.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        let text = text.into_source_text();
        let db = &self.read.inner;
        let attaching = db.lookup_file_in(project, path).is_none();
        let unchanged_attachment = attaching
            && db
                .files
                .get(&database::normalize_path(path))
                .is_some_and(|source| {
                    db.identities[source]
                        .values()
                        .next()
                        .is_some_and(|file| file.text(db) == &text)
                });
        let file = self.read.inner.upsert_file_in(project, path, text);
        let source = *file.identity(&self.read.inner);
        if self.read.inner.source_layers.contains_key(&source) {
            let text = file.text(&self.read.inner).clone();
            let layers = Arc::make_mut(&mut self.read.inner.source_layers)
                .get_mut(&source)
                .expect("source layers exist");
            if attaching {
                layers.backing_projects.insert(project);
                layers.backing.get_or_insert_with(|| text.clone());
            }
            if layers.overlay.is_some() {
                layers.overlay = Some(text);
            } else {
                layers.backing = Some(text);
            }
        }
        // Adding an unchanged source to another project does not transform the
        // bytes underlying existing instances' reparse hints.
        if !unchanged_attachment {
            self.read.inner.stage_source_edits(source, edits);
        }
        Ok(source)
    }

    pub fn set_project_declarations(
        &mut self,
        project: ProjectId,
        declarations: ResolvedDeclarations,
    ) -> Result<bool, ChangeError> {
        if !self.read.inner.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        Ok(self
            .read
            .inner
            .set_project_declarations(project, declarations))
    }

    pub fn set_project_source_declarations(
        &mut self,
        project: ProjectId,
        path: &Path,
        declarations: ResolvedDeclarations,
    ) -> Result<bool, ChangeError> {
        if !self.read.inner.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        Ok(self
            .read
            .inner
            .set_source_declarations_in(project, path, declarations))
    }

    pub fn set_project_file_alias(
        &mut self,
        project: ProjectId,
        requested: &Path,
        actual: &Path,
    ) -> Result<bool, ChangeError> {
        if !self.read.inner.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        Ok(self
            .read
            .inner
            .set_file_alias_in(project, requested, actual))
    }

    pub fn clear_project_source_declarations(
        &mut self,
        project: ProjectId,
        path: &Path,
    ) -> Result<bool, ChangeError> {
        if !self.read.inner.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        Ok(self.read.inner.clear_source_declarations_in(project, path))
    }

    pub fn remove_project(&mut self, project: ProjectId) -> bool {
        let removed = self.read.inner.remove_project(project);
        self.read.project = self.read.inner.default_project;
        removed
    }

    pub fn remove_project_files(&mut self, project: ProjectId, paths: &[PathBuf]) {
        self.read.inner.remove_files_in(project, paths);
    }

    /// Apply sequential byte edits only if the source version still matches.
    /// Validation and text construction finish before any tracked input changes.
    pub fn edit_source(
        &mut self,
        expected: SourceVersion,
        edits: Vec<Edit>,
    ) -> Result<SourceVersion, ChangeError> {
        let file = self.check_version(expected)?;
        let mut text = file.text(&self.read.inner).clone();
        for (index, edit) in edits.iter().enumerate() {
            if !edit.fits(&text) {
                return Err(ChangeError::InvalidEdit { index });
            }
            text = Arc::new(text.with_replacement(edit.range.clone(), &edit.insert));
        }
        let path = file.path(&self.read.inner).clone();
        self.apply_project_change(expected.project, &path, text, Some(edits))?;
        let current = self.read.inner.input(expected.source, expected.project);
        Ok(SourceVersion {
            revision: *current.revision(&self.read.inner),
            ..expected
        })
    }

    pub fn replace_source(
        &mut self,
        expected: SourceVersion,
        text: impl IntoSourceText,
    ) -> Result<SourceVersion, ChangeError> {
        let file = self.check_version(expected)?;
        let path = file.path(&self.read.inner).clone();
        self.apply_project_change(expected.project, &path, text, None)?;
        let current = self.read.inner.input(expected.source, expected.project);
        Ok(SourceVersion {
            revision: *current.revision(&self.read.inner),
            ..expected
        })
    }

    fn check_version(&self, expected: SourceVersion) -> Result<SourceInput, ChangeError> {
        let file = self
            .read
            .inner
            .find_input(expected.source, expected.project)
            .ok_or(ChangeError::SourceRemoved)?;
        if *file.revision(&self.read.inner) != expected.revision {
            return Err(ChangeError::StaleRevision);
        }
        Ok(file)
    }

    pub fn snapshot(&self) -> Analysis {
        self.read.clone()
    }

    pub fn add_file(&mut self, text: impl IntoSourceText) -> SourceId {
        let file = self.read.inner.add_file(text);
        *file.identity(&self.read.inner)
    }

    pub fn upsert_file(&mut self, path: &Path, text: impl IntoSourceText) -> SourceId {
        self.apply_change(path, text, None)
    }

    pub fn apply_change(
        &mut self,
        path: &Path,
        text: impl IntoSourceText,
        edits: Option<Vec<Edit>>,
    ) -> SourceId {
        self.apply_project_change(self.read.project, path, text, edits)
            .expect("default project exists")
    }

    pub fn set_file_text(&mut self, id: SourceId, text: impl IntoSourceText) {
        let path = self.file_path(id).to_path_buf();
        self.apply_change(&path, text, None);
    }

    pub fn set_declarations(&mut self, declarations: ResolvedDeclarations) -> bool {
        self.read.inner.set_declarations(declarations)
    }

    pub fn set_source_declarations(
        &mut self,
        path: &Path,
        declarations: ResolvedDeclarations,
    ) -> bool {
        self.read.inner.set_source_declarations(path, declarations)
    }

    pub fn clear_source_declarations(&mut self, path: &Path) -> bool {
        self.read.inner.clear_source_declarations(path)
    }

    pub fn set_file_alias(&mut self, requested: &Path, actual: &Path) -> bool {
        self.read.inner.set_file_alias(requested, actual)
    }

    pub fn remove_file(&mut self, path: &Path) -> Option<SourceId> {
        let id = self.lookup_file(path)?;
        self.read.inner.remove_file(path)?;
        Some(id)
    }

    /// Remove project members with one storage reconstruction. Surviving source
    /// identities and source allocations stay unchanged.
    pub fn remove_files(&mut self, paths: &[PathBuf]) {
        self.read.inner.remove_files(paths);
    }

    pub fn clear_query_log(&self) {
        self.read.inner.clear_query_log();
    }
    pub fn reparse_cache_len(&self) -> usize {
        self.read.inner.reparse_cache_len()
    }
    pub fn reparse_prev(&self, id: SourceId) -> Option<Arc<PrevParse>> {
        self.read
            .inner
            .find_input(id, self.read.project)
            .and_then(|file| self.read.inner.reparse_prev(file))
    }
    pub fn reparse_pending_edits(&self, id: SourceId) -> Vec<Edit> {
        self.read
            .inner
            .reparse_pending_edits(self.read.inner.input(id, self.read.project))
    }
}

impl Analysis {
    /// Host-owned request cancellation; no host callbacks or IO in computations.
    pub fn with_cancellation(mut self, cancelled: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.cancellation = Some(cancelled);
        self
    }
    pub fn check_cancelled(&self) {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            std::panic::resume_unwind(Box::new(salsa::Cancelled::Local));
        }
    }
    /// Check request cancellation and whether a writer supersedes this snapshot.
    pub fn check_revision_cancelled(&self) {
        self.check_cancelled();
        salsa::Database::unwind_if_revision_cancelled(&self.inner);
    }
    pub fn file_alias(&self, path: &Path) -> Option<&Path> {
        self.check_cancelled();
        self.inner.file_alias_in(self.project, path)
    }

    pub fn project_id(&self) -> ProjectId {
        self.check_cancelled();
        self.project
    }

    pub fn source_text(&self, source: SourceId) -> &Arc<SourceText> {
        self.check_cancelled();
        self.inner.input(source, self.project).text(&self.inner)
    }

    /// An editor overlay changes the effective shared physical source even when
    /// the editor belongs to another project. Provenance consumers must refuse it.
    pub fn source_has_overlay(&self, source: SourceId) -> bool {
        self.check_cancelled();
        self.inner
            .source_layers
            .get(&source)
            .is_some_and(|layers| layers.overlay.is_some())
    }

    pub fn installed_source_identity(
        &self,
        source: SourceId,
    ) -> Option<crate::project::installed_source::InstalledSourceId> {
        self.check_cancelled();
        *super::queries::installed_source_identity(
            &self.inner,
            self.inner.input(source, self.project),
        )
    }

    pub fn standard_package_source(
        &self,
        source: SourceId,
    ) -> Option<&'static crate::project::installed_source::InstalledSourceProfile> {
        self.installed_source_identity(source)
            .map(crate::project::installed_source::profile)
    }

    pub fn revision(&self) -> AnalysisRevision {
        self.check_cancelled();
        AnalysisRevision {
            project: self.project,
            epoch: self.inner.epoch,
            revision: self.inner.revision,
        }
    }

    pub fn query_log(&self) -> Vec<QueryLogEntry> {
        self.check_cancelled();
        self.inner.query_log()
    }

    /// Storage identity changes when memberships or observations are retired,
    /// not on ordinary edits.
    pub fn epoch(&self) -> u64 {
        self.check_cancelled();
        self.inner.epoch
    }

    pub fn source_version(&self, source: SourceId) -> Option<SourceVersion> {
        self.check_cancelled();
        let file = self.inner.find_input(source, self.project)?;
        Some(SourceVersion {
            project: self.project,
            source,
            revision: *file.revision(&self.inner),
        })
    }

    pub fn lookup_file(&self, path: &Path) -> Option<SourceId> {
        self.check_cancelled();
        self.inner
            .lookup_file_in(self.project, path)
            .map(|file| *file.identity(&self.inner))
    }
    pub fn tracked_files(&self) -> Vec<(PathBuf, SourceId)> {
        self.check_cancelled();
        self.project_members()
            .iter()
            .map(|member| (member.path.clone(), member.file))
            .collect()
    }
    /// The selected installation index owns this exact source path. A local
    /// source with the same basename is still authored project content.
    pub fn is_installed_source(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| self.texmf().by_name.get(name))
            .is_some_and(|installed| installed == path)
    }
    /// Workspace diagnostics cover authored files; installed dependencies remain
    /// available to navigation and other language features.
    pub fn workspace_diagnostic_files(&self) -> Vec<(PathBuf, SourceId)> {
        self.tracked_files()
            .into_iter()
            .filter(|(path, _)| !self.is_installed_source(path))
            .collect()
    }
    pub fn project_members(&self) -> &[ProjectMember] {
        self.check_cancelled();
        &workspace_project(&self.inner, self.inner.project(self.project)).members
    }
    pub fn file_line_index(
        &self,
        file: SourceId,
        encoding: crate::text::PositionEncoding,
    ) -> crate::text::LineIndex<'_> {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        file.text(&self.inner).line_index(encoding)
    }
    pub fn manual_bib_body_ends(&self, file: SourceId) -> &[rowan::TextSize] {
        self.check_cancelled();
        manual_bib_body_ends(&self.inner, self.inner.input(file, self.project))
    }
    pub fn name_occurrences(&self, file: SourceId) -> &crate::name_refs::NameOccurrences {
        self.check_cancelled();
        name_occurrences(&self.inner, self.inner.input(file, self.project))
    }
    pub fn file_operand_contexts(
        &self,
        file: SourceId,
    ) -> &tex_ls_parser::semantic::context::PrimitiveOperandContexts {
        self.check_cancelled();
        file_operand_contexts(&self.inner, self.inner.input(file, self.project))
    }

    pub fn definition_sites(&self, file: SourceId) -> &[crate::semantic::DefSite] {
        self.check_cancelled();
        definition_sites(&self.inner, self.inner.input(file, self.project))
    }
    pub fn folding_ranges(&self, file: SourceId) -> Vec<crate::folding::FoldingRange> {
        self.check_cancelled();
        crate::folding::folding_ranges(
            &self.parsed_tree(file),
            &self.file_line_index(file, crate::text::PositionEncoding::Utf16),
            self.file_outline(file),
        )
    }
    pub fn selection_chain(&self, file: SourceId, offset: usize) -> Vec<rowan::TextRange> {
        self.check_cancelled();
        crate::selection::selection_chain(&self.parsed_tree(file), offset)
    }
    pub fn file_label_contexts(&self, file: SourceId) -> &FileLabelContexts {
        self.check_cancelled();
        file_label_contexts(&self.inner, self.inner.input(file, self.project))
    }

    pub fn file_outline(&self, file: SourceId) -> &[crate::semantic::OutlineItem] {
        self.check_cancelled();
        file_outline(&self.inner, self.inner.input(file, self.project))
    }
    pub fn root_contexts(
        &self,
        root: &Path,
    ) -> Option<&std::collections::BTreeMap<PathBuf, Vec<PathBuf>>> {
        self.check_cancelled();
        crate::project::root_views::contexts(&self.inner, self.inner.project(self.project), root)
    }
    pub fn file_path_context(&self, file: SourceId) -> Option<&crate::external::FilePathContext> {
        self.check_cancelled();
        crate::project::root_views::file_context(&self.inner, self.inner.input(file, self.project))
            .as_ref()
    }
    pub fn file_references(&self, file: SourceId) -> &[crate::external::links::FileReference] {
        self.check_cancelled();
        file_references(&self.inner, self.inner.input(file, self.project))
    }

    pub fn file_discovery_references(
        &self,
        file: SourceId,
    ) -> &[crate::external::links::FileReference] {
        self.check_cancelled();
        file_discovery_references(&self.inner, self.inner.input(file, self.project))
    }
    pub fn document_links(&self, file: SourceId) -> Vec<crate::external::links::LinkTarget> {
        self.check_cancelled();
        self.file_references(file)
            .iter()
            .filter_map(
                |reference| match self.resolve_file(&reference.candidates).target {
                    crate::external::Observation::Present(target) => {
                        Some(crate::external::links::LinkTarget {
                            range: reference.range,
                            target,
                        })
                    }
                    _ => None,
                },
            )
            .collect()
    }
    pub fn latex_lint_findings(&self, file: SourceId) -> &[crate::linter::Diagnostic] {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        latex_lint_findings(&self.inner, file)
    }
    pub fn bib_lint_findings(&self, file: SourceId) -> &[crate::linter::Diagnostic] {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        bib_lint_findings(&self.inner, file)
    }
    pub fn file_text(&self, file: SourceId) -> &str {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.file_text(file)
    }
    pub fn text_is_current(&self, file: SourceId, text: &str) -> bool {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.text_is_current(file, text)
    }
    pub fn file_path(&self, file: SourceId) -> &Path {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.file_path(file)
    }
    pub fn declarations_for(&self, path: &Path) -> &ResolvedDeclarations {
        self.check_cancelled();
        self.inner.declarations_for_in(self.project, path)
    }

    /// Explicit source metadata, even when its value equals project defaults.
    /// Inherited defaults return `None`; the path is normalized within this
    /// snapshot's selected project.
    pub fn source_declaration_override(&self, path: &Path) -> Option<&ResolvedDeclarations> {
        self.check_cancelled();
        self.inner
            .source_declaration_override_in(self.project, path)
    }
    pub fn declarations(&self) -> &ResolvedDeclarations {
        self.check_cancelled();
        self.inner.project(self.project).declarations(&self.inner)
    }
    pub fn parse_diagnostics(&self, file: SourceId) -> &[ParseDiagnosticData] {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.parse_diagnostics(file)
    }
    pub fn parsed_tree(&self, file: SourceId) -> SyntaxNode {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.parsed_tree(file)
    }
    pub fn semantic_model(&self, file: SourceId) -> &SemanticModel {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.semantic_model(file)
    }
    pub fn file_is_document_root(&self, file: SourceId) -> bool {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.file_is_document_root(file)
    }
    pub fn document_signatures(&self, file: SourceId) -> &SignatureDb {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.document_signatures(file)
    }

    pub fn file_modes(&self, file: SourceId) -> &tex_ls_parser::semantic::mode::ModeIndex {
        self.check_cancelled();
        file_modes(&self.inner, self.inner.input(file, self.project))
    }
    pub fn file_glossary_keys(&self, file: SourceId) -> &[SmolStr] {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.file_glossary_keys(file)
    }
    pub fn bib_parse_diagnostics(&self, file: SourceId) -> &[ParseDiagnosticData] {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.bib_parse_diagnostics(file)
    }
    pub fn parsed_bib_tree(&self, file: SourceId) -> BibSyntaxNode {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.parsed_bib_tree(file)
    }
    pub fn bib_semantic_model(&self, file: SourceId) -> &BibModel {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        self.inner.bib_semantic_model(file)
    }
    pub fn resolve_project(&self) -> (&ResolvedLabels, &ResolvedCitations) {
        self.check_cancelled();
        (self.resolve_labels(), self.resolve_citations())
    }
    pub fn resolve_labels(&self) -> &ResolvedLabels {
        self.check_cancelled();
        resolved_labels(&self.inner, self.inner.project(self.project))
    }
    /// The same inclusion roots used by label resolution, without label facts.
    pub fn document_roots(&self, path: &Path) -> &[PathBuf] {
        self.check_cancelled();
        crate::project::root_views::candidate_roots(
            &self.inner,
            self.inner.project(self.project),
            path,
        )
    }
    pub fn document_scope_is_closed(&self, path: &Path) -> bool {
        self.check_cancelled();
        crate::project::root_views::is_closed(&self.inner, self.inner.project(self.project), path)
    }
    pub fn resolve_citations(&self) -> &ResolvedCitations {
        self.check_cancelled();
        resolved_citations(&self.inner, self.inner.project(self.project))
    }
    pub fn editor_symbols(&self, file: SourceId) -> &super::queries::EditorSymbols {
        self.check_cancelled();
        super::queries::editor_symbols(&self.inner, self.inner.input(file, self.project))
    }

    pub fn editor_signatures(&self, file: SourceId) -> &SignatureDb {
        &self.editor_symbols(file).signatures
    }

    pub fn scope_signatures(&self, file: SourceId) -> &SignatureDb {
        self.check_cancelled();
        let file = self.inner.input(file, self.project);
        scope_signatures(&self.inner, file)
    }
    pub fn package_graph(&self) -> &crate::project::PackageGraph {
        self.check_cancelled();
        package_graph(&self.inner, self.inner.project(self.project))
    }
    pub fn resolve_package_options(&self) -> &ResolvedPackageOptions {
        self.check_cancelled();
        resolved_package_options(&self.inner, self.inner.project(self.project))
    }

    pub fn file_labels(&self, id: SourceId) -> &[SmolStr] {
        self.check_cancelled();
        self.inner.file_labels(self.inner.input(id, self.project))
    }
    pub fn file_refs(&self, id: SourceId) -> &[SmolStr] {
        self.check_cancelled();
        self.inner.file_refs(self.inner.input(id, self.project))
    }
    pub fn include_edges(&self, id: SourceId) -> &[IncludeEdgeKey] {
        self.check_cancelled();
        self.inner.include_edges(self.inner.input(id, self.project))
    }
    pub fn doc_associations(&self, id: SourceId) -> &[DocAssociation] {
        self.check_cancelled();
        self.inner
            .doc_associations(self.inner.input(id, self.project))
    }
    pub fn bib_search_fields(&self, id: SourceId) -> &[(SmolStr, String)] {
        self.check_cancelled();
        super::bib_search_fields(&self.inner, self.inner.input(id, self.project))
    }
    pub fn file_roles(&self, id: SourceId) -> &crate::external::links::FileRoleContext {
        self.check_cancelled();
        super::file_role_contexts(&self.inner, self.inner.input(id, self.project))
    }
    pub fn glossary_search_fields(&self, id: SourceId) -> &[(SmolStr, String)] {
        self.check_cancelled();
        super::glossary_search_fields(&self.inner, self.inner.input(id, self.project))
    }
    pub fn file_bibliography_targets(&self, id: SourceId) -> &[BibTarget] {
        self.check_cancelled();
        super::file_bibliography_targets(&self.inner, self.inner.input(id, self.project))
    }
    pub fn file_cite_facts(&self, id: SourceId) -> &FileCiteFacts {
        self.check_cancelled();
        super::file_cite_facts(&self.inner, self.inner.input(id, self.project))
    }
    pub fn project_graph(&self) -> &crate::project::IncludeGraph {
        self.check_cancelled();
        crate::project::project_graph(&self.inner, self.inner.project(self.project))
    }
}

impl Analysis {
    /// Cached source-local names whose generated or literal uses prevent rename.
    pub(crate) fn uneditable_command_names(
        &self,
        file: SourceId,
    ) -> &std::collections::BTreeSet<SmolStr> {
        self.check_cancelled();
        super::uneditable_command_names(&self.inner, self.inner.input(file, self.project))
    }

    /// Borrow cached command-scope paths without rebuilding a path index.
    pub(crate) fn command_scope_paths(&self, file: SourceId) -> impl Iterator<Item = &Path> + '_ {
        self.check_cancelled();
        super::command_scope(&self.inner, self.inner.input(file, self.project))
            .iter()
            .map(|input| input.path(&self.inner).as_path())
    }

    /// Cached command-scope membership without exposing Salsa inputs to features.
    pub(crate) fn command_scope_files(
        &self,
        file: SourceId,
    ) -> impl Iterator<Item = SourceId> + '_ {
        self.check_cancelled();
        super::command_scope(&self.inner, self.inner.input(file, self.project))
            .iter()
            .map(|input| *input.identity(&self.inner))
    }
}
