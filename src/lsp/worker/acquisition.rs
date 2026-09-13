use super::*;

impl Worker {
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
    /// a `tex-ls.toml` `exclude`/`extend-exclude` prunes the same siblings here as
    /// it does for the CLI. It is exclude-nothing when no config governs.
    pub(super) fn acquire_files(
        &mut self,
        project: ProjectId,
        path: &Path,
        completion: Option<(Position, PositionEncoding)>,
        installed: &InstalledPackages,
    ) -> bool {
        if installed.ready_index().is_none() && !self.loading_indexes.contains(installed) {
            self.loading_indexes.push(installed.clone());
            let _ = self.out_tx.send(Outbound::Loading(true));
        }
        let key = {
            let snapshot = self.snapshot_for(path);
            let references: Vec<_> = snapshot
                .project_members()
                .iter()
                .filter(|m| m.kind.is_latex())
                .map(|member| {
                    (
                        &member.path,
                        snapshot
                            .file_references(member.file)
                            .iter()
                            .map(|r| &r.candidates)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            let context = completion.and_then(|(position, enc)| {
                let file = snapshot.lookup_file(path)?;
                if !file_kind_for(path).is_latex() {
                    return None;
                }
                let offset = snapshot
                    .file_line_index(file, enc)
                    .offset_at(position.line, position.character);
                let context = tex_ls_analysis::completion::classify_context_with_declarations(
                    &snapshot.parsed_tree(file),
                    offset,
                    snapshot.declarations_for(path),
                );
                match context {
                    CompletionContext::FilePath { prefix, .. }
                    | CompletionContext::PackageName { prefix, .. } => Some(prefix),
                    _ => None,
                }
            });
            format!(
                "{references:?}{context:?}{:?}{}",
                installed.config(),
                installed.ready_index().is_some()
            )
        };
        if self.acquisition_keys.get(&(project, path.to_owned())) == Some(&key)
            && self.pending_scans.get(&project).is_none_or(Vec::is_empty)
            && self
                .extra_sources
                .get(&project)
                .is_none_or(std::collections::BTreeSet::is_empty)
            && self
                .extra_locations
                .get(&project)
                .is_none_or(std::collections::BTreeSet::is_empty)
        {
            return self.files_inflight.contains(&project);
        }
        if self.files_inflight.contains(&project) {
            return true;
        }
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, InputNeed, InstalledMetadata, Observation,
        };
        if let Some(index) = installed.ready_index() {
            let snapshot = self.snapshot_for(path);
            let changed = snapshot.texmf() != index;
            drop(snapshot);
            if changed {
                let token = self
                    .db
                    .begin_external_refresh(project, ExternalInputKind::Installed)
                    .expect("project");
                self.db
                    .apply_external_inputs(
                        token,
                        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                            toolchain: "native".into(),
                            index: index.clone(),
                        })),
                    )
                    .expect("current installation");
            }
        }
        let snapshot = self.snapshot_for(path);
        let (_, directories) = services::completion_locations(&snapshot, path, completion);
        let mut locations = self.extra_locations.remove(&project).unwrap_or_default();
        let mut sources = self.extra_sources.remove(&project).unwrap_or_default();
        let known: std::collections::BTreeSet<_> = snapshot
            .project_members()
            .iter()
            .map(|member| member.path.clone())
            .collect();
        let bibliographies = bibliography_requests(&snapshot)
            .into_iter()
            .filter(|(requested, _)| {
                !self
                    .bib_lookups
                    .get(&project)
                    .is_some_and(|lookups| lookups.contains_key(requested))
            })
            .collect::<Vec<_>>();
        for member in snapshot.project_members() {
            for need in snapshot.file_discovery_needs(&member.path) {
                match need {
                    InputNeed::Location(path) => {
                        if tex_ls_analysis::source::lint_file_kind(&path).is_some()
                            && snapshot.lookup_file(&path).is_none()
                        {
                            sources.insert(path.clone());
                        }
                        locations.insert(path);
                    }
                    InputNeed::Source(path) => {
                        sources.insert(path);
                    }
                    InputNeed::Installed => {}
                }
            }
        }
        drop(snapshot);
        let scans = self.pending_scans.remove(&project).unwrap_or_default();
        self.discovery_anchors
            .insert(project, (path.to_owned(), installed.clone()));
        self.acquisition_keys
            .insert((project, path.to_owned()), key);
        if locations.is_empty()
            && sources.is_empty()
            && directories.is_empty()
            && scans.is_empty()
            && bibliographies.is_empty()
        {
            return false;
        }
        let token = self
            .db
            .begin_external_refresh(project, ExternalInputKind::Files)
            .expect("project");
        self.files_inflight.insert(project);
        let _ = self.out_tx.send(Outbound::Loading(true));
        self.file_acquisition.submit(file_acquisition::Plan {
            project,
            token,
            locations,
            sources,
            directories,
            scans,
            known,
            bibliographies,
            excluded_roots: self
                .projects
                .roots()
                .into_iter()
                .filter(|root| self.projects.select(root, None).ok() != Some(project))
                .collect(),
        });
        true
    }

    pub(super) fn acquire_compiler(
        &mut self,
        project: ProjectId,
        path: &Path,
        build: &BuildConfig,
    ) {
        self.compiler_acquisition.prepare(
            &mut self.db,
            project,
            path,
            build,
            &self.file_acquisition,
        );
        if self.compiler_acquisition.pending(project) {
            let _ = self.out_tx.send(Outbound::Loading(true));
        }
    }

    pub(in crate::lsp) fn seed_dir(&mut self, path: &Path, exclude: &ExcludeFilter) -> bool {
        let Some(directory) = path
            .parent()
            .filter(|directory| directory.parent().is_some())
        else {
            return false;
        };
        let directory = self.projects.root_for(path).unwrap_or(directory).to_owned();
        let project = self.project_for(path);
        if self.seeded_dirs.insert((project, directory.clone())) {
            self.pending_scans.entry(project).or_default().push((
                path.to_owned(),
                directory,
                exclude.clone(),
            ));
            self.acquisition_keys.remove(&(project, path.to_owned()));
        }
        false
    }

    pub(super) fn source_version_at(
        &self,
        project: ProjectId,
        path: &Path,
    ) -> Option<tex_ls_analysis::incremental::SourceVersion> {
        let snapshot = self.db.snapshot_for(project).expect("registered project");
        snapshot
            .lookup_file(path)
            .and_then(|source| snapshot.source_version(source))
    }

    /// Refresh backing data on the IO queue; editor overlays remain authoritative.
    pub(super) fn refresh_backing(
        &mut self,
        project: ProjectId,
        path: &Path,
        deleted: bool,
    ) -> bool {
        // A watched write supersedes any older discovery read of this source.
        let _ = self
            .db
            .begin_external_refresh(project, tex_ls_analysis::external::ExternalInputKind::Files);
        self.extra_sources
            .entry(project)
            .or_default()
            .insert(path.to_owned());
        self.extra_locations
            .entry(project)
            .or_default()
            .insert(path.to_owned());
        self.acquisition_keys.clear();
        if deleted {
            self.db
                .set_backing(project, path, None)
                .expect("registered project");
        }
        let (anchor, installed) = self
            .discovery_anchors
            .get(&project)
            .cloned()
            .unwrap_or_else(|| (path.to_owned(), InstalledPackages::default()));
        self.acquire_files(project, &anchor, None, &installed);
        deleted
    }

    pub(in crate::lsp) fn apply_watched_change(&mut self, path: &Path, deleted: bool) -> bool {
        if path
            .extension()
            .is_some_and(|ext| matches!(ext.to_str(), Some("aux" | "log" | "fls")))
        {
            self.compiler_acquisition
                .watched(&mut self.db, path, &self.file_acquisition);
            if self.compiler_acquisition.pending_count() != 0 {
                let _ = self.out_tx.send(Outbound::Loading(true));
            }
            return false;
        }
        if tex_ls_analysis::source::lint_file_kind(path).is_none() {
            return false;
        }
        let project = self.project_for(path);
        let tracked = self.source_version_at(project, path).is_some();
        let in_seeded_dir = path
            .parent()
            .is_some_and(|dir| self.seeded_dirs.contains(&(project, dir.to_path_buf())));
        if !tracked && !in_seeded_dir && self.projects.root_for(path).is_none() {
            return false;
        }
        self.refresh_backing(project, path, deleted)
    }
}
