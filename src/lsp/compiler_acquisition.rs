//! Background compiler IO, coalesced per project before snapshot admission.
use super::*;
use std::collections::BTreeMap;
use tex_ls_analysis::external::{
    AcquisitionToken, CompilerArtifact, ExternalInputKind, ExternalInputs, Observation,
};
use tex_ls_analysis::incremental::ProjectId;

type Members = Vec<(PathBuf, Vec<PathBuf>)>;
pub(super) struct Plan {
    project: ProjectId,
    token: AcquisitionToken,
    contexts: BTreeMap<PathBuf, Members>,
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
        let artifacts = services::acquire_artifacts(members);
        let catalog: HashMap<_, _> = artifacts
            .iter()
            .map(|(path, value)| (path.as_path(), value))
            .collect();
        let watch_sets: Vec<(PathBuf, Vec<PathBuf>)> = self
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
                    if let Some(Observation::Present(artifact)) = catalog.get(path.as_path()) {
                        paths.push(PathBuf::from(&artifact.identity));
                        for input in &artifact.parsed.inputs {
                            let input = PathBuf::from(input);
                            paths.push(input.clone());
                            pending.push(input);
                        }
                    }
                }
                paths.sort();
                paths.dedup();
                (source, paths)
            })
            .collect();
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
    pub fn prepare(
        &mut self,
        db: &mut IncrementalDatabase,
        project: ProjectId,
        path: &Path,
        build: &BuildConfig,
        io: &file_acquisition::FileAcquisition,
    ) {
        let members = services::compiler_members(db, project, path, build);
        let state = self.projects.entry(project).or_default();
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
        let contexts: Vec<_> = self
            .projects
            .get(&project)
            .into_iter()
            .flat_map(|state| &state.contexts)
            .map(|(path, context)| (path.clone(), context.build.clone()))
            .collect();
        for (path, build) in contexts {
            self.prepare(db, project, &path, &build, io);
        }
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
            completed.artifacts.iter().any(|(path, value)| {
                previous
                    .get(path.as_path())
                    .map_or(!matches!(value, Observation::Absent), |old| *old != value)
            })
        });
        if db
            .apply_external_inputs(
                completed.token,
                ExternalInputs::Compiler(completed.artifacts),
            )
            .is_ok()
        {
            state.watches = completed.watches;
            for (source, paths) in completed.watch_sets {
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
}
