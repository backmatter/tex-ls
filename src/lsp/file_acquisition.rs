//! Native IO queue. Waiting acquisition jobs own paths and tokens, never snapshots.
use super::*;
use std::collections::BTreeSet;
use tex_ls_analysis::external::{
    AcquisitionToken, ExternalInputs, FileInputs, LocationKind, Observation,
};
use tex_ls_analysis::incremental::ProjectId;

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
                let mut physical_sources: std::collections::HashMap<(ProjectId, PathBuf), PathBuf> =
                    std::collections::HashMap::new();
                for job in jobs {
                    before_read();
                    let mut plan = match job {
                        Job::Files(plan) => plan,
                        Job::Compiler(plan) => {
                            if compiler_done.send(plan.read()).is_err() {
                                break;
                            }
                            continue;
                        }
                    };
                    let mut files = FileInputs::default();
                    for (anchor, directory, exclude) in plan.scans {
                        if anchor.is_file()
                            && let Ok(found) = collect_lint_files(&[directory], &exclude)
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
                                if let Some(actual) =
                                    physical_sources.get(&(plan.project, physical.clone()))
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
                                physical_sources.insert((plan.project, physical), path.clone());
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
                        }
                        files.locations.push((path.clone(), observation));
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
