use crate::{
    config::BuildConfig,
    file_discovery::ExcludeFilter,
    lsp::{
        file_acquisition,
        messages::Outbound,
        services,
        worker::{Worker, bibliography_requests},
    },
    texmf::InstalledPackages,
    text::PositionEncoding,
};
use lsp_types::Position;
use std::{path::Path, sync::Arc};
use tex_ls_analysis::{completion::CompletionContext, incremental::ProjectId};
use tex_ls_protocol::convert::file_kind_for;

#[cfg(test)]
std::thread_local! {
    pub(super) static ACQUISITION_PREFIX_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static ACQUISITION_PREFIX_SOURCE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static ACQUISITION_PREFIX_COMPARE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static ACQUISITION_PREFIX_OUTPUT_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone)]
pub(super) struct AcquisitionKey {
    prefix: Arc<str>,
    suffix: String,
}

impl PartialEq for AcquisitionKey {
    fn eq(&self, other: &Self) -> bool {
        self.suffix == other.suffix
            && (Arc::ptr_eq(&self.prefix, &other.prefix)
                || exact_prefix_equal(&self.prefix, &other.prefix))
    }
}
impl Eq for AcquisitionKey {}

#[cfg(test)]
impl std::fmt::Display for AcquisitionKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}{}", self.prefix, self.suffix)
    }
}

fn exact_prefix_equal(left: &str, right: &str) -> bool {
    #[cfg(test)]
    ACQUISITION_PREFIX_COMPARE_BYTES.with(|count| count.set(count.get() + left.len()));
    left == right
}

fn source_has_control(text: &str) -> bool {
    #[cfg(test)]
    ACQUISITION_PREFIX_SOURCE_BYTES.with(|count| count.set(count.get() + text.len()));
    text.contains('\\')
}

impl Worker {
    pub(super) fn project_acquisition_prefix(
        &mut self,
        snapshot: &tex_ls_analysis::incremental::Analysis,
    ) -> Arc<str> {
        let project = snapshot.project_id();
        let revision = snapshot.revision();
        if let Some((cached_revision, prefix)) = self.acquisition_project_prefixes.get(&project)
            && *cached_revision == revision
        {
            return prefix.clone();
        }
        #[cfg(test)]
        ACQUISITION_PREFIX_BUILDS.with(|count| count.set(count.get() + 1));
        let references: Vec<_> = snapshot
            .project_members()
            .iter()
            .filter(|m| m.kind.is_latex())
            .map(|member| {
                (
                    &member.path,
                    source_has_control(snapshot.file_text(member.file)),
                    snapshot
                        .file_discovery_references(member.file)
                        .iter()
                        .map(|r| &r.candidates)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let prefix = format!("{references:?}");
        #[cfg(test)]
        ACQUISITION_PREFIX_OUTPUT_BYTES.with(|count| count.set(count.get() + prefix.len()));
        // Reuse the canonical allocation when a broad writer revision changed
        // without changing the acquisition projection. Existing per-path keys
        // then keep their constant-time pointer comparison.
        let prefix = self
            .acquisition_project_prefixes
            .get(&project)
            .filter(|(_, previous)| exact_prefix_equal(previous, &prefix))
            .map_or_else(
                || Arc::<str>::from(prefix),
                |(_, previous)| previous.clone(),
            );
        self.acquisition_project_prefixes
            .insert(project, (revision, prefix.clone()));
        prefix
    }
}

impl Worker {
    pub(super) fn file_acquisition_key(
        &mut self,
        path: &Path,
        completion: Option<(Position, PositionEncoding)>,
        installed: &InstalledPackages,
    ) -> AcquisitionKey {
        let snapshot = self.snapshot_for(path);
        let prefix = self.project_acquisition_prefix(&snapshot);
        let context = completion.and_then(|(position, enc)| {
            let file = snapshot.lookup_file(path)?;
            if !file_kind_for(path).is_latex() {
                return None;
            }
            let offset = snapshot
                .file_line_index(file, enc)
                .offset_at(position.line, position.character);
            // Acquisition runs before the warm-key comparison. Reuse revision
            // facts so ordinary argument cursors do not rescan the whole source.
            let context = tex_ls_analysis::completion::classify_context_with_roles(
                &snapshot.parsed_tree(file),
                offset,
                snapshot.declarations_for(path),
                || snapshot.file_roles(file),
            );
            match context {
                CompletionContext::FilePath { prefix, .. }
                | CompletionContext::PackageName { prefix, .. } => Some(prefix),
                _ => None,
            }
        });
        AcquisitionKey {
            prefix,
            suffix: format!(
                "{context:?}{:?}{}",
                installed.config(),
                installed.generation()
            ),
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
        let key = self.file_acquisition_key(path, completion, installed);
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
            let changed = snapshot.texmf() != index.as_ref();
            let obsolete: Vec<_> = snapshot
                .texmf()
                .by_name
                .iter()
                .filter(|(name, old)| {
                    index.by_name.get(*name) != Some(*old) && snapshot.lookup_file(old).is_some()
                })
                .map(|(_, path)| path.clone())
                .collect();
            let revived: Vec<_> = if changed {
                index
                    .by_name
                    .iter()
                    .filter(|(name, selected)| {
                        snapshot.texmf().by_name.get(*name) != Some(*selected)
                            && snapshot.lookup_file(selected).is_none()
                            && matches!(
                                snapshot.location_kind(selected),
                                Observation::Absent | Observation::Error(_)
                            )
                    })
                    .map(|(_, selected)| selected.clone())
                    .collect()
            } else {
                Vec::new()
            };
            drop(snapshot);
            if changed {
                self.extra_sources
                    .entry(project)
                    .or_default()
                    .extend(revived.iter().cloned());
                self.extra_locations
                    .entry(project)
                    .or_default()
                    .extend(revived);
                // Previously loaded installed sources also need re-observation;
                // replacing the name index alone leaves stale backing files alive.
                self.extra_sources
                    .entry(project)
                    .or_default()
                    .extend(obsolete.iter().cloned());
                self.extra_locations
                    .entry(project)
                    .or_default()
                    .extend(obsolete);
                let token = self
                    .db
                    .begin_external_refresh(project, ExternalInputKind::Installed)
                    .expect("project");
                self.db
                    .apply_external_inputs(
                        token,
                        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                            toolchain: "native".into(),
                            index: index.as_ref().clone(),
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
        // File creation/deletion can change installed filename selection before
        // its periodic refresh. Requests must await the watched generation.
        for (_, installed) in self.discovery_anchors.values() {
            installed.watched_source_change(path, deleted);
        }
        // Installed sources can belong to several workspace roots while their
        // physical path lies outside every root. Refresh each owning project.
        let owners: std::collections::BTreeSet<_> =
            self.projects
                .projects()
                .filter(|project| {
                    if self.source_version_at(*project, path).is_some() {
                        return true;
                    }
                    // Deletion removes source membership and can remove the name
                    // from the published installation index. Keep refreshing a
                    // previously observed miss while this project's installation
                    // owns its path, including before a new index is published.
                    self.db.snapshot_for(*project).is_ok_and(|snapshot| {
                        matches!(
                            snapshot.location_kind(path),
                            tex_ls_analysis::external::Observation::Absent
                                | tex_ls_analysis::external::Observation::Error(_)
                        ) && (self
                            .discovery_anchors
                            .get(project)
                            .is_some_and(|(_, installed)| installed.contains_source_path(path))
                            || path.file_name().and_then(|name| name.to_str()).is_some_and(
                                |name| {
                                    snapshot
                                        .texmf()
                                        .by_name
                                        .get(name)
                                        .is_some_and(|p| p == path)
                                },
                            ))
                    })
                })
                .collect();
        if !owners.is_empty() {
            let mut removed = false;
            for project in owners {
                removed |= self.refresh_backing(project, path, deleted);
            }
            return removed;
        }
        let project = self.project_for(path);
        let in_seeded_dir = path
            .parent()
            .is_some_and(|dir| self.seeded_dirs.contains(&(project, dir.to_path_buf())));
        if !in_seeded_dir && self.projects.root_for(path).is_none() {
            return false;
        }
        self.refresh_backing(project, path, deleted)
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn exact_key_equality_preserves_native_suffixes_and_distinct_allocations() {
        for prefix in ["[]", "[(\"/p/é😀.tex\", true, [])]"] {
            let canonical: Arc<str> = Arc::from(prefix);
            for suffix in ["Noneconfig0", "Some(\"input\")config0", "Noneconfig1"] {
                let key = AcquisitionKey {
                    prefix: canonical.clone(),
                    suffix: suffix.into(),
                };
                let shared = key.clone();
                let independently_allocated = AcquisitionKey {
                    prefix: Arc::from(prefix),
                    suffix: suffix.into(),
                };
                assert_eq!(key, shared);
                assert_eq!(key, independently_allocated);
                assert_eq!(key.to_string(), format!("{prefix}{suffix}"));
                let changed_suffix = AcquisitionKey {
                    prefix: canonical.clone(),
                    suffix: format!("{suffix}different"),
                };
                assert_ne!(key, changed_suffix);
                let changed_prefix = AcquisitionKey {
                    prefix: Arc::from(format!("{prefix}different")),
                    suffix: suffix.into(),
                };
                assert_ne!(key, changed_prefix);
            }
        }
    }
}
