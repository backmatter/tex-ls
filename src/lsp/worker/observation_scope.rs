//! Retire observations for abandoned targets while retaining current misses.
use crate::lsp::{
    messages::WorkerJob,
    worker::{Worker, bibliography_candidates},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tex_ls_analysis::{incremental::ProjectId, source::normalize_path};
use tex_ls_protocol::convert::uri_to_fs_path;

fn include_path(scope: &mut BTreeSet<PathBuf>, path: &Path) {
    let path = normalize_path(path);
    if let Some(parent) = path.parent() {
        scope.insert(parent.to_owned());
    }
    scope.insert(path);
}

impl Worker {
    pub(super) fn retain_observation_scope(&mut self, project: ProjectId, observed: &[PathBuf]) {
        let Ok(snapshot) = self.db.snapshot_for(project) else {
            return;
        };
        let mut scope = BTreeSet::new();
        for member in snapshot.project_members() {
            include_path(&mut scope, &member.path);
            if !member.kind.is_latex() {
                continue;
            }
            for path in tex_ls_protocol::code_action::load_package::observation_paths(
                &snapshot,
                &member.path,
            ) {
                include_path(&mut scope, &path);
            }
            for root in tex_ls_analysis::external::explicit_root_hints(
                snapshot.file_text(member.file),
                &member.path,
            ) {
                include_path(&mut scope, &root);
            }
            for reference in snapshot.file_references(member.file) {
                for candidate in &reference.candidates.local {
                    include_path(&mut scope, candidate);
                }
                if let Some((name, extensions)) = &reference.candidates.installed {
                    let extensions: Vec<_> = extensions.iter().map(String::as_str).collect();
                    if let Some(target) = snapshot.texmf().resolve(name, &extensions) {
                        include_path(&mut scope, target);
                    }
                }
            }
        }
        // Include loaded/aliased bibliography targets too, before the acquisition
        // helper filters them out as already available.
        for (candidate, base) in bibliography_candidates(&snapshot) {
            include_path(&mut scope, &candidate);
            if let Some(base) = base {
                include_path(&mut scope, &base);
            }
        }
        for source in snapshot.format_source_paths() {
            include_path(&mut scope, &source);
        }
        for path in self.compiler_acquisition.observation_paths(project) {
            include_path(&mut scope, path);
        }
        if let Some((anchor, _)) = self.discovery_anchors.get(&project) {
            include_path(&mut scope, anchor);
        }
        for path in observed
            .iter()
            .chain(self.extra_sources.get(&project).into_iter().flatten())
            .chain(self.extra_locations.get(&project).into_iter().flatten())
        {
            include_path(&mut scope, path);
        }
        for job in self.pending_reads.iter().chain(&self.deferred_files) {
            let path = match job {
                WorkerJob::Format { path, .. }
                | WorkerJob::RangeFormat { path, .. }
                | WorkerJob::OnTypeFormat { path, .. }
                | WorkerJob::Colors { path, .. }
                | WorkerJob::SemanticTokens { path, .. }
                | WorkerJob::Symbols { path, .. }
                | WorkerJob::FoldingRange { path, .. }
                | WorkerJob::SelectionRange { path, .. }
                | WorkerJob::DocumentLink { path, .. }
                | WorkerJob::Hover { path, .. }
                | WorkerJob::SignatureHelp { path, .. }
                | WorkerJob::GotoDefinition { path, .. }
                | WorkerJob::References { path, .. }
                | WorkerJob::DocumentHighlight { path, .. }
                | WorkerJob::PrepareRename { path, .. }
                | WorkerJob::Rename { path, .. }
                | WorkerJob::InspectProject { path, .. }
                | WorkerJob::LinkedEditing { path, .. }
                | WorkerJob::Diagnostic { path, .. }
                | WorkerJob::InlayHints { path, .. }
                | WorkerJob::CodeAction { path, .. }
                | WorkerJob::ForwardSearch { path, .. } => Some(path.clone()),
                WorkerJob::Completion { uri, .. } => uri_to_fs_path(uri),
                _ => None,
            };
            if let Some(path) = path
                && self.project_for(&path) == project
            {
                include_path(&mut scope, &path);
            }
        }
        // Keep physical targets for current logical origins, including aliases
        // that are not separate source memberships.
        let aliases: Vec<_> = scope
            .iter()
            .filter_map(|path| snapshot.file_alias(path).map(Path::to_owned))
            .collect();
        for actual in aliases {
            include_path(&mut scope, &actual);
        }
        drop(snapshot);
        if let Some(lookups) = self.bib_lookups.get_mut(&project) {
            lookups.retain(|path, _| scope.contains(path));
        }
        if let Some(failures) = self.acquisition_failures.get_mut(&project) {
            failures.retain(|path, _| scope.contains(path));
        }
        self.db
            .retain_file_observations(project, &scope)
            .expect("registered observation scope");
    }
}
