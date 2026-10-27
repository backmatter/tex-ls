//! Native IO queue. Waiting acquisition jobs own paths and tokens, never snapshots.
#[cfg(test)]
use crate::incremental::IncrementalDatabase;
use crate::{
    file_discovery::{ExcludeFilter, collect_lint_files_excluding_roots},
    lsp::{compiler_acquisition, services},
};
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    thread::JoinHandle,
};
use tex_ls_analysis::{
    external::{AcquisitionToken, ExternalInputs, FileInputs, LocationKind, Observation},
    incremental::ProjectId,
    text::IntoSourceText,
};

pub(super) struct Plan {
    pub project: ProjectId,
    pub token: AcquisitionToken,
    pub locations: BTreeSet<PathBuf>,
    pub sources: BTreeSet<PathBuf>,
    pub directories: BTreeSet<PathBuf>,
    pub scans: Vec<(PathBuf, PathBuf, ExcludeFilter)>,
    pub known: BTreeSet<PathBuf>,
    pub bibliographies: Vec<(PathBuf, Option<PathBuf>)>,
    pub excluded_roots: Vec<PathBuf>,
}
pub(super) struct Completed {
    pub project: ProjectId,
    pub token: AcquisitionToken,
    pub inputs: ExternalInputs,
    pub aliases: Vec<(PathBuf, Option<PathBuf>)>,
}
enum Job {
    Files(Plan),
    Compiler(compiler_acquisition::Plan),
    RetainProjects(HashSet<ProjectId>),
    #[cfg(test)]
    InspectPhysicalSources(Sender<usize>),
}
pub(super) struct FileAcquisition {
    tx: Option<Sender<Job>>,
    worker: Option<JoinHandle<()>>,
    pub rx: Receiver<Completed>,
    pub compiler_rx: Receiver<compiler_acquisition::Completed>,
}
impl FileAcquisition {
    pub fn new() -> Self {
        Self::with_before_read(|| {})
    }

    pub(super) fn with_before_read(mut before_read: impl FnMut() + Send + 'static) -> Self {
        let (tx, jobs) = unbounded::<Job>();
        let (done, rx) = unbounded();
        let (compiler_done, compiler_rx) = unbounded();
        let worker = std::thread::Builder::new()
            .name("tex-ls-acquisition".into())
            .spawn(move || {
                let mut physical_sources: HashMap<ProjectId, HashMap<PathBuf, PathBuf>> =
                    HashMap::new();
                for job in jobs {
                    let mut plan = match job {
                        #[cfg(test)]
                        Job::InspectPhysicalSources(reply) => {
                            let _ = reply.send(physical_sources.values().map(HashMap::len).sum());
                            continue;
                        }
                        Job::RetainProjects(live) => {
                            physical_sources.retain(|project, _| live.contains(project));
                            continue;
                        }
                        Job::Files(plan) => {
                            before_read();
                            plan
                        }
                        Job::Compiler(plan) => {
                            before_read();
                            if compiler_done.send(plan.read()).is_err() {
                                break;
                            }
                            continue;
                        }
                    };
                    // Identity reuse belongs to live logical owners, not the
                    // history of every source this IO worker has ever read.
                    let identities = physical_sources.entry(plan.project).or_default();
                    identities.retain(|_, logical| plan.known.contains(logical));
                    let mut files = FileInputs::default();
                    let mut absent_sources = HashSet::new();
                    for (anchor, directory, exclude) in plan.scans {
                        if anchor.is_file()
                            && let Ok(found) = collect_lint_files_excluding_roots(
                                &[directory],
                                &exclude,
                                &plan.excluded_roots,
                            )
                        {
                            plan.sources
                                .extend(found.into_iter().map(|(path, _)| path).filter(|path| {
                                    !plan.known.contains(path)
                                        && !plan
                                            .excluded_roots
                                            .iter()
                                            .any(|root| path.starts_with(root))
                                }));
                        }
                    }
                    let mut aliases = Vec::new();
                    for (requested, base) in plan.bibliographies {
                        let actual = crate::bibliography::resolve_bibliography_file(
                            &requested,
                            base.as_deref(),
                        );
                        if let Some(actual) = &actual
                            && !plan.known.contains(actual)
                        {
                            plan.sources.insert(actual.clone());
                        }
                        files.file_resolution.push((
                            requested.clone(),
                            actual
                                .clone()
                                .map_or(Observation::Absent, Observation::Present),
                        ));
                        aliases.push((requested, actual));
                    }
                    let explicitly_resolved: HashSet<_> = files
                        .file_resolution
                        .iter()
                        .map(|(path, _)| path.clone())
                        .collect();
                    for path in plan.locations.union(&plan.sources) {
                        let mut observation =
                            services::location(path, plan.directories.contains(path));
                        if plan.sources.contains(path)
                            && matches!(observation.kind, Observation::Present(LocationKind::File))
                        {
                            // Never open FIFOs or devices as source text.
                            if std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
                                let physical =
                                    std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                                if let Some(actual) = identities.get(&physical)
                                    && actual != path
                                    && plan.known.contains(actual)
                                {
                                    aliases.push((path.clone(), Some(actual.clone())));
                                    files
                                        .file_resolution
                                        .push((path.clone(), Observation::Present(actual.clone())));
                                    files.locations.push((path.clone(), observation));
                                    continue;
                                }
                                identities.insert(physical, path.clone());
                                match read_source(path) {
                                    Ok(text) => files
                                        .backing
                                        .push((path.clone(), Some(text.into_source_text()))),
                                    Err(error) => {
                                        observation.kind = Observation::Error(error.to_string())
                                    }
                                }
                            }
                        }
                        if plan.sources.contains(path)
                            && matches!(observation.kind, Observation::Absent)
                        {
                            files.backing.push((path.clone(), None));
                            absent_sources.insert(path.clone());
                        }
                        if plan.sources.contains(path) && !explicitly_resolved.contains(path) {
                            // Re-observation replaces an old physical alias even
                            // when its former target remains a live source. Keep
                            // explicit bibliography search results from this batch.
                            let resolution = match &observation.kind {
                                Observation::Absent => Observation::Absent,
                                Observation::Error(error) => Observation::Error(error.clone()),
                                _ => Observation::Unknown,
                            };
                            files.file_resolution.push((path.clone(), resolution));
                        }
                        files.locations.push((path.clone(), observation));
                    }
                    // A Close plan may still have included the overlay in
                    // `known`; confirmed absence releases that identity now.
                    if !absent_sources.is_empty() {
                        identities.retain(|_, logical| !absent_sources.contains(logical));
                    }
                    for path in plan.directories.difference(&plan.locations) {
                        files
                            .locations
                            .push((path.clone(), services::location(path, true)));
                    }
                    if done
                        .send(Completed {
                            project: plan.project,
                            token: plan.token,
                            inputs: ExternalInputs::Files(files),
                            aliases,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("acquisition worker");
        Self {
            tx: Some(tx),
            rx,
            compiler_rx,
            worker: Some(worker),
        }
    }
    pub fn submit(&self, plan: Plan) {
        let _ = self
            .tx
            .as_ref()
            .expect("active acquisition")
            .send(Job::Files(plan));
    }
    pub fn submit_compiler(&self, plan: compiler_acquisition::Plan) {
        let _ = self
            .tx
            .as_ref()
            .expect("active acquisition")
            .send(Job::Compiler(plan));
    }

    pub fn retain_projects(&self, live: HashSet<ProjectId>) {
        let _ = self
            .tx
            .as_ref()
            .expect("active acquisition")
            .send(Job::RetainProjects(live));
    }

    #[cfg(test)]
    pub(super) fn physical_source_count(&self) -> usize {
        let (reply, result) = unbounded();
        self.tx
            .as_ref()
            .unwrap()
            .send(Job::InspectPhysicalSources(reply))
            .unwrap();
        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    }
}

static SOURCE_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(super) fn source_reads() -> u64 {
    SOURCE_READS.load(std::sync::atomic::Ordering::Relaxed)
}
pub(super) fn read_source(path: &Path) -> std::io::Result<String> {
    SOURCE_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::fs::read_to_string(path)
}

impl Drop for FileAcquisition {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::external::ExternalInputKind;

    #[test]
    #[cfg(unix)]
    fn deleting_source_alias_retires_its_positive_resolution() {
        assert_source_alias_reobservation(false);
    }

    #[test]
    #[cfg(unix)]
    fn replacing_source_alias_with_ordinary_file_retires_its_positive_resolution() {
        assert_source_alias_reobservation(true);
    }

    #[cfg(unix)]
    fn assert_source_alias_reobservation(ordinary_file: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root.tex");
        let alias = dir.path().join("alias.tex");
        std::fs::write(&root, "root").unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let io = FileAcquisition::new();
        for phase in 0..3 {
            let path = if phase == 0 { &root } else { &alias };
            if phase == 2 {
                std::fs::remove_file(&alias).unwrap();
                if ordinary_file {
                    std::fs::write(&alias, "new logical source").unwrap();
                }
            }
            io.submit(Plan {
                project,
                token: db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .unwrap(),
                locations: BTreeSet::new(),
                sources: BTreeSet::from([path.clone()]),
                directories: BTreeSet::new(),
                scans: Vec::new(),
                known: if phase == 0 {
                    BTreeSet::new()
                } else {
                    BTreeSet::from([root.clone()])
                },
                bibliographies: Vec::new(),
                excluded_roots: Vec::new(),
            });
            let completed = io
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            db.apply_external_inputs(completed.token, completed.inputs)
                .unwrap();
            if phase == 1 {
                assert_eq!(db.snapshot().file_alias(&alias), Some(root.as_path()));
            }
        }
        assert!(
            db.lookup_file(&root).is_some(),
            "the physical source remains live"
        );
        assert!(
            db.snapshot().file_alias(&alias).is_none(),
            "reobserved logical source must not resolve through a live old alias target"
        );
        if ordinary_file {
            let source = db.lookup_file(&alias).unwrap();
            assert_eq!(db.snapshot().file_text(source), "new logical source");
        }
    }

    #[test]
    #[cfg(unix)]
    fn physical_identity_cache_preserves_live_alias_deduplication() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root.tex");
        let alias = dir.path().join("alias.tex");
        std::fs::write(&root, "root").unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let io = FileAcquisition::new();
        for (path, known) in [
            (&root, BTreeSet::new()),
            (&alias, BTreeSet::from([root.clone()])),
        ] {
            io.submit(Plan {
                project,
                token: db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .unwrap(),
                locations: BTreeSet::new(),
                sources: BTreeSet::from([path.clone()]),
                directories: BTreeSet::new(),
                scans: Vec::new(),
                known,
                bibliographies: Vec::new(),
                excluded_roots: Vec::new(),
            });
            let completed = io
                .rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            if path == &alias {
                assert_eq!(completed.aliases, vec![(alias.clone(), Some(root.clone()))]);
                let ExternalInputs::Files(files) = completed.inputs else {
                    unreachable!()
                };
                assert!(
                    files.backing.is_empty(),
                    "a live physical identity is not read twice"
                );
            }
            assert_eq!(io.physical_source_count(), 1);
        }
    }

    #[test]
    fn explicit_bibliography_resolution_survives_source_reobservation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("references.bib");
        std::fs::write(&path, "@article{key, title={Title}}").unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let io = FileAcquisition::new();
        io.submit(Plan {
            project,
            token: db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .unwrap(),
            locations: BTreeSet::new(),
            sources: BTreeSet::from([path.clone()]),
            directories: BTreeSet::new(),
            scans: Vec::new(),
            known: BTreeSet::new(),
            bibliographies: vec![(path.clone(), Some(dir.path().to_owned()))],
            excluded_roots: Vec::new(),
        });
        let completed = io
            .rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let ExternalInputs::Files(files) = &completed.inputs else {
            unreachable!()
        };
        assert_eq!(
            files.file_resolution,
            vec![(path.clone(), Observation::Present(path.clone()))]
        );
        db.apply_external_inputs(completed.token, completed.inputs)
            .unwrap();
        assert!(db.lookup_file(&path).is_some());
        assert_eq!(db.snapshot().file_alias(&path), Some(path.as_path()));
    }

    #[test]
    fn confirmed_absent_source_releases_identity_even_when_plan_known_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("closed.tex");
        std::fs::write(&path, "disk").unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let io = FileAcquisition::new();
        for missing in [false, true] {
            if missing {
                std::fs::remove_file(&path).unwrap();
            }
            io.submit(Plan {
                project,
                token: db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .unwrap(),
                locations: BTreeSet::new(),
                sources: BTreeSet::from([path.clone()]),
                directories: BTreeSet::new(),
                scans: Vec::new(),
                known: BTreeSet::from([path.clone()]),
                bibliographies: Vec::new(),
                excluded_roots: Vec::new(),
            });
            io.rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(io.physical_source_count(), usize::from(!missing));
        }
    }

    #[test]
    fn physical_identity_cache_retires_evicted_source_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let io = FileAcquisition::new();
        for index in 0..32 {
            let path = dir.path().join(format!("transient-{index}.tex"));
            std::fs::write(&path, "text").unwrap();
            io.submit(Plan {
                project,
                token: db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .unwrap(),
                locations: BTreeSet::new(),
                sources: BTreeSet::from([path.clone()]),
                directories: BTreeSet::new(),
                scans: Vec::new(),
                known: BTreeSet::new(),
                bibliographies: Vec::new(),
                excluded_roots: Vec::new(),
            });
            io.rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let identities = io.physical_source_count();
            std::fs::remove_file(&path).unwrap();
            assert_eq!(
                identities, 1,
                "only current source ownership may be retained"
            );
        }
    }
}
