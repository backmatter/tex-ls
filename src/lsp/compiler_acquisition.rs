//! Background compiler IO, coalesced per project before snapshot admission.
mod artifacts;
pub(super) use artifacts::compiler_probes;

use super::{file_acquisition, messages::Outbound};
use crate::config::BuildConfig;
use crossbeam_channel::Sender;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};
use tex_ls_analysis::{
    external::{AcquisitionToken, CompilerArtifact, ExternalInputKind, Observation},
    incremental::{IncrementalDatabase, ProjectId},
};

type Members = Vec<(PathBuf, Vec<PathBuf>)>;
pub(super) struct Plan {
    project: ProjectId,
    token: AcquisitionToken,
    contexts: BTreeMap<PathBuf, Members>,
    retired_sources: Vec<PathBuf>,
}
pub(super) struct Completed {
    project: ProjectId,
    token: AcquisitionToken,
    artifacts: Vec<(PathBuf, Observation<CompilerArtifact>)>,
    watches: Vec<PathBuf>,
    watch_sets: Vec<(PathBuf, Vec<PathBuf>)>,
}
struct Context {
    build: BuildConfig,
    members: Members,
}
#[derive(Default)]
struct Project {
    contexts: BTreeMap<PathBuf, Context>,
    watches: Vec<PathBuf>,
    dirty: bool,
    inflight: Option<AcquisitionToken>,
    retired_sources: BTreeSet<PathBuf>,
}
impl Plan {
    pub(super) fn read(self) -> Completed {
        let mut seen = HashSet::new();
        let members: Members = self
            .contexts
            .values()
            .flatten()
            .filter(|(logical, _)| seen.insert(logical.clone()))
            .cloned()
            .collect();
        let mut watches: Vec<_> = members
            .iter()
            .flat_map(|(_, paths)| paths.iter().cloned())
            .collect();
        let acquired = artifacts::acquire_artifacts_with_candidates(members);
        let artifacts = acquired.artifacts;
        let catalog: HashMap<_, _> = artifacts
            .iter()
            .map(|(path, value)| (path.as_path(), value))
            .collect();
        let mut watch_sets: Vec<(PathBuf, Vec<PathBuf>)> = self
            .contexts
            .into_iter()
            .map(|(source, members)| {
                let mut paths: Vec<_> = members
                    .iter()
                    .flat_map(|(_, paths)| paths.iter().cloned())
                    .collect();
                let mut pending: Vec<_> = members.into_iter().map(|(path, _)| path).collect();
                let mut seen = HashSet::new();
                while let Some(path) = pending.pop() {
                    if !seen.insert(path.clone()) {
                        continue;
                    }
                    if let Some(candidates) = acquired.candidates.get(&path) {
                        paths.extend(candidates.iter().cloned());
                    }
                    if let Some(Observation::Present(artifact)) = catalog.get(path.as_path()) {
                        let inputs = artifact.resolved_aux_inputs.clone().unwrap_or_else(|| {
                            let base = path.parent().unwrap_or(Path::new(""));
                            artifact
                                .parsed
                                .inputs
                                .iter()
                                .map(|input| base.join(input))
                                .collect()
                        });
                        paths.extend(inputs.iter().cloned());
                        pending.extend(inputs);
                    }
                }
                paths.sort();
                paths.dedup();
                (source, paths)
            })
            .collect();
        watch_sets.extend(
            self.retired_sources
                .into_iter()
                .map(|source| (source, Vec::new())),
        );
        watches.extend(
            watch_sets
                .iter()
                .flat_map(|(_, paths)| paths.iter().cloned()),
        );
        watches.sort();
        watches.dedup();
        Completed {
            project: self.project,
            token: self.token,
            artifacts,
            watches,
            watch_sets,
        }
    }
}

#[derive(Default)]
pub(super) struct CompilerAcquisition {
    projects: HashMap<ProjectId, Project>,
}
impl CompilerAcquisition {
    pub fn observation_paths(&self, project: ProjectId) -> impl Iterator<Item = &Path> {
        self.projects
            .get(&project)
            .into_iter()
            .flat_map(|state| state.contexts.iter())
            .flat_map(|(source, context)| {
                std::iter::once(source.as_path()).chain(context.build.root.as_deref())
            })
    }

    pub fn prepare(
        &mut self,
        db: &mut IncrementalDatabase,
        project: ProjectId,
        path: &Path,
        build: &BuildConfig,
        io: &file_acquisition::FileAcquisition,
    ) {
        let available = db.snapshot_for(project).is_ok_and(|snapshot| {
            snapshot.lookup_file(path).is_some()
                || snapshot
                    .file_alias(path)
                    .and_then(|actual| snapshot.lookup_file(actual))
                    .is_some()
        });
        if !available {
            return;
        }
        let members = artifacts::compiler_members(db, project, path, build);
        let state = self.projects.entry(project).or_default();
        state.retired_sources.remove(path);
        let changed = state
            .contexts
            .get(path)
            .is_none_or(|context| context.members != members);
        state.contexts.insert(
            path.to_owned(),
            Context {
                build: build.clone(),
                members,
            },
        );
        if changed {
            Self::invalidate(db, project, state);
        }
        self.submit(db, project, io);
    }

    fn invalidate(db: &mut IncrementalDatabase, project: ProjectId, state: &mut Project) {
        state.dirty = true;
        // Reject the in-flight batch even if it finishes before the replacement.
        let _ = db.begin_external_refresh(project, ExternalInputKind::Compiler);
    }

    pub fn watched(
        &mut self,
        db: &mut IncrementalDatabase,
        path: &Path,
        io: &file_acquisition::FileAcquisition,
    ) {
        let mut dirty = Vec::new();
        for (project, state) in &mut self.projects {
            if state.watches.iter().any(|candidate| candidate == path)
                || state
                    .contexts
                    .values()
                    .flat_map(|context| &context.members)
                    .any(|(_, candidates)| candidates.iter().any(|candidate| candidate == path))
            {
                Self::invalidate(db, *project, state);
                dirty.push(*project);
            }
        }
        for project in dirty {
            self.submit(db, project, io);
        }
    }

    pub fn sources_changed(
        &mut self,
        db: &mut IncrementalDatabase,
        project: ProjectId,
        io: &file_acquisition::FileAcquisition,
    ) {
        self.retire_contexts(db, project);
        let contexts: Vec<_> = self
            .projects
            .get(&project)
            .into_iter()
            .flat_map(|state| &state.contexts)
            .map(|(path, context)| (path.clone(), context.build.clone()))
            .collect();
        let members: Vec<_> = contexts
            .into_iter()
            .map(|(path, build)| {
                let members = artifacts::compiler_members(db, project, &path, &build);
                (path, members)
            })
            .collect();
        let Some(state) = self.projects.get_mut(&project) else {
            return;
        };
        let mut changed = false;
        for (path, members) in members {
            let context = state.contexts.get_mut(&path).expect("surviving context");
            if context.members != members {
                context.members = members;
                changed = true;
            }
        }
        if changed {
            Self::invalidate(db, project, state);
        }
        self.submit(db, project, io);
    }

    /// Retire only after a backing batch has confirmed that a closed source is
    /// unavailable. A pending close refresh may still restore physical backing.
    pub fn retire_unavailable_sources(
        &mut self,
        db: &mut IncrementalDatabase,
        project: ProjectId,
        io: &file_acquisition::FileAcquisition,
    ) {
        if self.retire_contexts(db, project) {
            self.submit(db, project, io);
        }
    }

    fn retire_contexts(&mut self, db: &mut IncrementalDatabase, project: ProjectId) -> bool {
        let Some(state) = self.projects.get_mut(&project) else {
            return false;
        };
        let unavailable: Vec<_> = {
            let Ok(snapshot) = db.snapshot_for(project) else {
                return false;
            };
            state
                .contexts
                .keys()
                .filter(|path| {
                    snapshot.lookup_file(path).is_none()
                        && snapshot
                            .file_alias(path)
                            .and_then(|actual| snapshot.lookup_file(actual))
                            .is_none()
                })
                .cloned()
                .collect()
        };
        if unavailable.is_empty() {
            return false;
        }
        for source in unavailable {
            state.contexts.remove(&source);
            state.retired_sources.insert(source);
        }
        Self::invalidate(db, project, state);
        true
    }

    pub fn clear(&mut self, db: &mut IncrementalDatabase) {
        for (project, state) in &mut self.projects {
            Self::invalidate(db, *project, state);
        }
        self.projects.clear();
    }

    fn submit(
        &mut self,
        db: &mut IncrementalDatabase,
        project: ProjectId,
        io: &file_acquisition::FileAcquisition,
    ) {
        let state = self.projects.get_mut(&project).expect("compiler project");
        if state.inflight.is_some() || !state.dirty {
            return;
        }
        let Ok(token) = db.begin_external_refresh(project, ExternalInputKind::Compiler) else {
            return;
        };
        state.inflight = Some(token);
        state.dirty = false;
        io.submit_compiler(Plan {
            project,
            token,
            contexts: state
                .contexts
                .iter()
                .map(|(path, context)| (path.clone(), context.members.clone()))
                .collect(),
            retired_sources: state.retired_sources.iter().cloned().collect(),
        });
    }

    pub fn complete(
        &mut self,
        db: &mut IncrementalDatabase,
        completed: Completed,
        out: &Sender<Outbound>,
        io: &file_acquisition::FileAcquisition,
    ) {
        let Some(state) = self.projects.get_mut(&completed.project) else {
            return;
        };
        if state.inflight != Some(completed.token) {
            return;
        }
        state.inflight = None;
        let changed = db.snapshot_for(completed.project).is_ok_and(|snapshot| {
            let previous: HashMap<_, _> = snapshot.compiler_artifacts().collect();
            let retained: HashSet<_> = completed
                .artifacts
                .iter()
                .map(|(path, _)| path.as_path())
                .collect();
            completed.artifacts.iter().any(|(path, value)| {
                previous
                    .get(path.as_path())
                    .map_or(!matches!(value, Observation::Absent), |old| *old != value)
            }) || previous.iter().any(|(path, value)| {
                !retained.contains(path)
                    && !matches!(value, Observation::Absent | Observation::Unknown)
            })
        });
        if db
            .replace_compiler_inputs(completed.token, completed.artifacts)
            .is_ok()
        {
            state.watches = completed.watches;
            for (source, paths) in completed.watch_sets {
                if paths.is_empty() {
                    state.retired_sources.remove(&source);
                }
                let _ = out.send(Outbound::WatchArtifacts { source, paths });
            }
            if changed {
                let _ = out.send(Outbound::RelintAll);
            }
        } else {
            state.dirty = true;
        }
        self.submit(db, completed.project, io);
    }

    pub fn pending(&self, project: ProjectId) -> bool {
        self.projects
            .get(&project)
            .is_some_and(|state| state.inflight.is_some() || state.dirty)
    }
    pub fn pending_count(&self) -> usize {
        self.projects
            .values()
            .filter(|state| state.inflight.is_some() || state.dirty)
            .count()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, IncrementalDatabase, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tex");
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, "\\documentclass{article}\\label{intro}", None);
        std::fs::write(path.with_extension("aux"), "\\newlabel{intro}{{1}{1}}\n").unwrap();
        (dir, db, path)
    }

    fn finish(
        queue: &mut CompilerAcquisition,
        db: &mut IncrementalDatabase,
        io: &file_acquisition::FileAcquisition,
    ) {
        let (tx, _) = unbounded();
        while queue.pending_count() != 0 {
            let completed = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            queue.complete(db, completed, &tx, io);
        }
    }

    fn number(db: &IncrementalDatabase, path: &Path) -> String {
        let snapshot = db.snapshot();
        let artifact = snapshot
            .compiler_artifacts()
            .find(|(known, _)| *known == path.with_extension("aux"))
            .unwrap()
            .1;
        let Observation::Present(artifact) = artifact else {
            panic!("present AUX");
        };
        artifact.parsed.data.labels["intro"].clone()
    }

    #[cfg(unix)]
    #[test]
    fn missing_aux_child_stays_watched_at_physical_alias_directory() {
        let dir = tempfile::tempdir().unwrap();
        let logical = dir.path().join("logical");
        let physical = dir.path().join("physical");
        std::fs::create_dir(&logical).unwrap();
        std::fs::create_dir(&physical).unwrap();
        let source = logical.join("main.tex");
        let actual = physical.join("main.aux");
        let child = physical.join("child.aux");
        std::fs::write(&actual, "\\@input{child.aux}").unwrap();
        std::os::unix::fs::symlink(&actual, source.with_extension("aux")).unwrap();
        let mut db = IncrementalDatabase::default();
        db.apply_change(&source, "text", None);
        let project = db.project_id();
        let run = |db: &mut IncrementalDatabase| {
            let token = db
                .begin_external_refresh(project, ExternalInputKind::Compiler)
                .unwrap();
            Plan {
                project,
                token,
                retired_sources: Vec::new(),
                contexts: [(
                    source.clone(),
                    vec![(
                        source.with_extension("aux"),
                        vec![source.with_extension("aux")],
                    )],
                )]
                .into_iter()
                .collect(),
            }
            .read()
        };
        let before = run(&mut db);
        assert!(before.watches.contains(&child));
        assert!(
            before
                .watch_sets
                .iter()
                .find(|(path, _)| path == &source)
                .unwrap()
                .1
                .contains(&child)
        );
        std::fs::write(&child, "\\newlabel{child}{{2}{1}}").unwrap();
        let after = run(&mut db);
        db.replace_compiler_inputs(after.token, after.artifacts)
            .unwrap();
        let snapshot = db.snapshot();
        let data = snapshot.aux_data(&[&source], &logical).unwrap();
        assert_eq!(data.labels.get("child").map(String::as_str), Some("2"));
    }

    #[test]
    fn slow_compiler_io_allows_source_writes_and_warm_requests_reuse_artifacts() {
        let (_dir, mut db, path) = setup();
        let (started_tx, started_rx) = unbounded();
        let (release_tx, release_rx) = unbounded();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::with_before_read(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let project = db.project_id();
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        db.apply_change(&path, "\\documentclass{article}\\label{intro}edited", None);
        assert!(io.compiler_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        finish(&mut queue, &mut db, &io);
        assert_eq!(number(&db, &path), "1");
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        assert!(
            !queue.pending(project),
            "unchanged artifact layout does not read again"
        );
        assert!(started_rx.try_recv().is_err());
    }

    #[test]
    fn watched_build_burst_discards_old_bytes_and_coalesces_replacement() {
        let (_dir, mut db, path) = setup();
        let project = db.project_id();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        let old = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::fs::write(path.with_extension("aux"), "\\newlabel{intro}{{2}{1}}\n").unwrap();
        for _ in 0..10 {
            queue.watched(&mut db, &path.with_extension("aux"), &io);
        }
        let (tx, rx) = unbounded();
        queue.complete(&mut db, old, &tx, &io);
        assert!(db.snapshot().compiler_artifacts().next().is_none());
        assert!(rx.try_recv().is_err(), "superseded bytes never publish");
        finish(&mut queue, &mut db, &io);
        assert_eq!(number(&db, &path), "2");
        assert!(
            io.compiler_rx.try_recv().is_err(),
            "one replacement for the burst"
        );
        std::fs::remove_file(path.with_extension("aux")).unwrap();
        queue.watched(&mut db, &path.with_extension("aux"), &io);
        finish(&mut queue, &mut db, &io);
        assert!(
            db.snapshot()
                .compiler_artifacts()
                .any(|(known, value)| known == path.with_extension("aux")
                    && matches!(value, Observation::Absent))
        );
    }

    #[test]
    fn retired_context_result_cannot_complete_a_new_acquisition() {
        let (_dir, mut db, path) = setup();
        let project = db.project_id();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        let old = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        queue.clear(&mut db);
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        let (tx, _) = unbounded();
        queue.complete(&mut db, old, &tx, &io);
        assert!(queue.pending(project));
        assert!(db.snapshot().compiler_artifacts().next().is_none());
        finish(&mut queue, &mut db, &io);
        assert_eq!(number(&db, &path), "1");
    }

    #[test]
    fn evicted_source_retires_compiler_context_and_rejects_its_inflight_result() {
        let (_dir, mut db, path) = setup();
        let project = db.project_id();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        let old = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        db.remove_file(&path);
        assert!(db.lookup_file(&path).is_none());
        queue.sources_changed(&mut db, project, &io);
        let (tx, rx) = unbounded();
        queue.complete(&mut db, old, &tx, &io);
        while queue.pending_count() != 0 {
            let completed = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            queue.complete(&mut db, completed, &tx, &io);
        }
        assert!(
            queue
                .projects
                .get(&project)
                .is_none_or(|state| !state.contexts.contains_key(&path)),
            "an evicted source must relinquish its compiler context"
        );
        assert!(db.snapshot().compiler_artifacts().next().is_none());
        assert!(rx.try_iter().any(|message| {
            matches!(message, Outbound::WatchArtifacts { source, paths } if source == path && paths.is_empty())
        }), "retirement must withdraw the source's artifact watch scope");
    }

    #[test]
    fn retiring_one_source_preserves_shared_artifacts_and_live_missing_candidates() {
        let (_dir, mut db, root) = setup();
        let source = root.with_file_name("transient.tex");
        db.apply_change(&source, "transient", None);
        let project = db.project_id();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        let build = BuildConfig {
            root: Some(root.clone()),
            ..Default::default()
        };
        queue.prepare(&mut db, project, &root, &build, &io);
        queue.prepare(&mut db, project, &source, &build, &io);
        finish(&mut queue, &mut db, &io);
        db.remove_file(&source);
        queue.sources_changed(&mut db, project, &io);
        finish(&mut queue, &mut db, &io);
        assert!(!queue.projects[&project].contexts.contains_key(&source));
        assert!(queue.projects[&project].contexts.contains_key(&root));
        assert_eq!(number(&db, &root), "1");
        assert!(
            queue.projects[&project]
                .watches
                .contains(&root.with_extension("log")),
            "a live source keeps missing candidates for its first build"
        );
    }

    #[test]
    fn unavailable_source_does_not_create_compiler_context_or_acquisition() {
        let (_dir, mut db, root) = setup();
        let project = db.project_id();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        let unknown = root.with_file_name("unknown.tex");
        for path in [&unknown, &root] {
            if path == &root {
                db.remove_file(&root);
            }
            assert!(db.lookup_file(path).is_none());
            queue.prepare(&mut db, project, path, &BuildConfig::default(), &io);
            assert!(queue.projects.is_empty());
            assert_eq!(queue.pending_count(), 0);
            assert!(io.compiler_rx.try_recv().is_err());
            assert!(db.snapshot().compiler_artifacts().next().is_none());
        }
    }

    #[test]
    fn logical_origin_with_live_physical_alias_keeps_its_compiler_context() {
        let (_dir, mut db, root) = setup();
        let alias = root.with_file_name("alias.tex");
        let project = db.project_id();
        db.set_project_file_alias(project, &alias, &root).unwrap();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        let build = BuildConfig {
            root: Some(root.clone()),
            ..Default::default()
        };
        queue.prepare(&mut db, project, &alias, &build, &io);
        finish(&mut queue, &mut db, &io);
        assert!(db.lookup_file(&alias).is_none());
        queue.sources_changed(&mut db, project, &io);
        finish(&mut queue, &mut db, &io);
        assert!(queue.projects[&project].contexts.contains_key(&alias));
        assert!(queue.projects[&project].retired_sources.is_empty());
        assert_eq!(number(&db, &root), "1");
    }

    #[test]
    fn reopened_source_cancels_retirement_before_stale_io_completes() {
        let (_dir, mut db, path) = setup();
        let project = db.project_id();
        let mut queue = CompilerAcquisition::default();
        let io = file_acquisition::FileAcquisition::new();
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        let old = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        db.remove_file(&path);
        queue.sources_changed(&mut db, project, &io);
        db.apply_change(
            &path,
            "\\documentclass{article}\\label{intro}reopened",
            None,
        );
        queue.prepare(&mut db, project, &path, &BuildConfig::default(), &io);
        let (tx, rx) = unbounded();
        queue.complete(&mut db, old, &tx, &io);
        while queue.pending_count() != 0 {
            let completed = io.compiler_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            queue.complete(&mut db, completed, &tx, &io);
        }
        assert!(queue.projects[&project].contexts.contains_key(&path));
        assert!(queue.projects[&project].retired_sources.is_empty());
        assert_eq!(number(&db, &path), "1");
        assert!(rx.try_iter().all(|message| {
            !matches!(message, Outbound::WatchArtifacts { source, paths } if source == path && paths.is_empty())
        }), "stale retirement must not withdraw the reopened source's watch scope");
    }
}
