//! Native fallback watching. Client watching owns the scope only after acknowledgement.
use crossbeam_channel::{Receiver, Sender};
use lsp_types::{FileChangeType, FileEvent};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};
use tex_ls_protocol::convert::path_to_uri;
#[cfg(test)]
use tex_ls_protocol::convert::uri_to_fs_path;

type Fingerprint = (Option<SystemTime>, u64, Option<u64>);
type Scope = (Vec<WatchRoot>, Vec<PathBuf>);
type Files = BTreeMap<PathBuf, Fingerprint>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct WatchRoot {
    path: PathBuf,
    recursive: bool,
}

impl WatchRoot {
    fn inferred(path: PathBuf) -> Self {
        let recursive = !(path.has_root() && path.parent().is_none());
        Self { path, recursive }
    }

    fn contains(&self, path: &Path) -> bool {
        if self.recursive {
            path.starts_with(&self.path)
        } else {
            path.parent() == Some(self.path.as_path())
        }
    }
}

pub(super) struct FallbackWatcher {
    control: Sender<Scope>,
    pub events: Receiver<Vec<FileEvent>>,
    settings: Option<Scope>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FallbackWatcher {
    pub fn new() -> Self {
        Self::with_scanner(scan_scope_cancellable)
    }

    fn with_scanner(
        mut scanner: impl FnMut(&[WatchRoot], &[PathBuf], &AtomicBool) -> Option<Files> + Send + 'static,
    ) -> Self {
        let (control, commands) = crossbeam_channel::unbounded::<Scope>();
        let (output, events) = crossbeam_channel::unbounded();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let worker = std::thread::Builder::new()
            .name("tex-ls-watch".into())
            .spawn(move || {
                let mut roots = Vec::new();
                let mut artifacts = Vec::new();
                let mut previous = BTreeMap::new();
                let interval = Duration::from_secs(1);
                let mut next_scan = Instant::now();
                loop {
                    if is_cancelled(&stop) {
                        break;
                    }
                    match commands.recv_timeout(next_scan.saturating_duration_since(Instant::now()))
                    {
                        Ok(mut scope) => {
                            // Only the latest scope matters to the next polling scan.
                            while let Ok(latest) = commands.try_recv() {
                                if is_cancelled(&stop) {
                                    return;
                                }
                                scope = latest;
                            }
                            let (next_roots, next_artifacts) = scope;
                            if roots != next_roots {
                                next_scan = Instant::now();
                            }
                            previous.retain(|path: &PathBuf, _| {
                                next_roots.iter().any(|root| root.contains(path))
                                    || next_artifacts.binary_search(path).is_ok()
                            });
                            roots = next_roots;
                            artifacts = next_artifacts;
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    }
                    if is_cancelled(&stop) {
                        break;
                    }
                    if Instant::now() < next_scan {
                        continue;
                    }
                    let started = Instant::now();
                    let Some(next) = scanner(&roots, &artifacts, &stop) else {
                        break;
                    };
                    if is_cancelled(&stop) {
                        break;
                    }
                    log::debug!(
                        "fallback watch: {} roots, {} files, {} artifact candidates, {:?}",
                        roots.len(),
                        next.len(),
                        artifacts.len(),
                        started.elapsed()
                    );
                    next_scan = Instant::now() + interval;
                    let changes = changes(&previous, &next);
                    if is_cancelled(&stop) {
                        break;
                    }
                    previous = next;
                    if !changes.is_empty() && output.send(changes).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn fallback watcher");
        Self {
            control,
            events,
            settings: None,
            cancelled,
            worker: Some(worker),
        }
    }

    pub fn update(&mut self, roots: Vec<PathBuf>, artifacts: Vec<PathBuf>) {
        self.update_scope(
            roots
                .into_iter()
                .map(|path| WatchRoot {
                    path,
                    recursive: true,
                })
                .collect(),
            artifacts,
        );
    }

    pub fn update_inferred(&mut self, roots: Vec<PathBuf>, artifacts: Vec<PathBuf>) {
        self.update_scope(
            roots.into_iter().map(WatchRoot::inferred).collect(),
            artifacts,
        );
    }

    fn update_scope(&mut self, roots: Vec<WatchRoot>, mut artifacts: Vec<PathBuf>) {
        let roots = normalize_roots(roots);
        artifacts.sort();
        artifacts.dedup();
        let next = (roots, artifacts);
        if self.settings.as_ref() != Some(&next) {
            let _ = self.control.send(next.clone());
            self.settings = Some(next);
        }
    }
}

fn normalize_roots(roots: Vec<WatchRoot>) -> Vec<WatchRoot> {
    let mut by_path = BTreeMap::new();
    for root in roots {
        *by_path.entry(root.path).or_insert(false) |= root.recursive;
    }
    // Distinct roots can have different ignore contexts. A nested workspace may
    // expose sources that an ancestor walk excludes, so preserve both walks.
    by_path
        .into_iter()
        .map(|(path, recursive)| WatchRoot { path, recursive })
        .collect()
}

impl Drop for FallbackWatcher {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        // Wake a worker waiting for its next polling interval.
        let _ = self.control.send((Vec::new(), Vec::new()));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn is_cancelled(cancelled: &AtomicBool) -> bool {
    cancelled.load(Ordering::Relaxed)
}

fn scan_scope_cancellable(
    roots: &[WatchRoot],
    artifacts: &[PathBuf],
    cancelled: &AtomicBool,
) -> Option<Files> {
    let mut files = scan_cancellable(roots, cancelled)?;
    // Explicit compiler candidates bypass discovery ignores at every depth.
    for path in artifacts {
        if is_cancelled(cancelled) {
            return None;
        }
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
        {
            files.insert(path.clone(), fingerprint(path, &meta, true, cancelled)?);
        }
    }
    Some(files)
}

fn scan_cancellable(roots: &[WatchRoot], cancelled: &AtomicBool) -> Option<Files> {
    let mut files = BTreeMap::new();
    for root in roots {
        if is_cancelled(cancelled) {
            return None;
        }
        let mut builder = ignore::WalkBuilder::new(&root.path);
        if !root.recursive {
            builder.max_depth(Some(1));
        }
        let mut walk = builder.build();
        loop {
            if is_cancelled(cancelled) {
                return None;
            }
            let Some(entry) = walk.next() else { break };
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let artifact = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ["aux", "log", "fls"].contains(&ext));
            if !(tex_ls_analysis::source::lint_file_kind(path).is_some()
                || artifact
                || path.file_name().is_some_and(|name| name == "tex-ls.toml"))
            {
                continue;
            }
            if let Entry::Vacant(file) = files.entry(path.to_path_buf())
                && let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                file.insert(fingerprint(path, &meta, false, cancelled)?);
            }
        }
    }
    Some(files)
}

fn fingerprint(
    path: &Path,
    meta: &std::fs::Metadata,
    artifact: bool,
    cancelled: &AtomicBool,
) -> Option<Fingerprint> {
    if is_cancelled(cancelled) {
        return None;
    }
    let content = if artifact {
        match std::fs::File::open(path) {
            Ok(file) => content_hash(file, cancelled)?,
            Err(_) => None,
        }
    } else {
        None
    };
    Some((meta.modified().ok(), meta.len(), content))
}

// Outer None aborts a cancelled scan; an unreadable file retains metadata only.
fn content_hash(mut reader: impl std::io::Read, cancelled: &AtomicBool) -> Option<Option<u64>> {
    use std::hash::Hasher;
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    let mut buffer = [0; 16384];
    loop {
        if is_cancelled(cancelled) {
            return None;
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Some(Some(digest.finish())),
            Ok(count) => digest.write(&buffer[..count]),
            Err(_) => return Some(None),
        }
    }
}

#[cfg(test)]
fn scan_scope(roots: &[PathBuf], artifacts: &[PathBuf]) -> Files {
    let roots: Vec<_> = roots
        .iter()
        .map(|path| WatchRoot {
            path: path.clone(),
            recursive: true,
        })
        .collect();
    scan_scope_cancellable(&roots, artifacts, &AtomicBool::new(false)).unwrap()
}

#[cfg(test)]
fn scan(roots: &[PathBuf]) -> Files {
    scan_scope(roots, &[])
}

fn changes(
    previous: &BTreeMap<PathBuf, Fingerprint>,
    next: &BTreeMap<PathBuf, Fingerprint>,
) -> Vec<FileEvent> {
    let mut events = Vec::new();
    for (path, fingerprint) in next {
        if previous.get(path) != Some(fingerprint)
            && let Some(uri) = path_to_uri(path)
        {
            events.push(FileEvent {
                uri,
                kind: if previous.contains_key(path) {
                    FileChangeType::Changed
                } else {
                    FileChangeType::Created
                },
            });
        }
    }
    for path in previous.keys().filter(|path| !next.contains_key(*path)) {
        if let Some(uri) = path_to_uri(path) {
            events.push(FileEvent {
                uri,
                kind: FileChangeType::Deleted,
            });
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inferred_filesystem_roots_are_shallow_on_the_native_platform() {
        #[cfg(not(windows))]
        let roots = [Path::new("/")];
        #[cfg(windows)]
        let roots = [Path::new(r"C:\"), Path::new(r"\\server\share\")];
        for root in roots {
            assert!(!WatchRoot::inferred(root.into()).recursive, "{root:?}");
            assert!(WatchRoot::inferred(root.join("project")).recursive);
        }
    }

    #[test]
    fn duplicate_scope_roots_keep_recursive_discovery_in_either_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = FallbackWatcher::new();
        for recursive in [[false, true], [true, false]] {
            watcher.update_scope(
                recursive
                    .into_iter()
                    .map(|recursive| WatchRoot {
                        path: dir.path().to_path_buf(),
                        recursive,
                    })
                    .collect(),
                vec![],
            );
            let roots = &watcher.settings.as_ref().unwrap().0;
            assert_eq!(roots.len(), 1);
            assert!(roots[0].recursive);
        }
    }

    #[test]
    fn distinct_nested_scope_roots_retain_their_own_walks() {
        let parent = PathBuf::from("project");
        let nested = parent.join("chapters");
        let lookalike = PathBuf::from("project-other");
        let roots = normalize_roots(vec![
            WatchRoot {
                path: nested.clone(),
                recursive: true,
            },
            WatchRoot {
                path: lookalike.clone(),
                recursive: true,
            },
            WatchRoot {
                path: parent.clone(),
                recursive: true,
            },
            WatchRoot {
                path: nested.join("appendix"),
                recursive: false,
            },
        ]);
        assert_eq!(
            roots,
            vec![
                WatchRoot {
                    path: parent.clone(),
                    recursive: true
                },
                WatchRoot {
                    path: nested.clone(),
                    recursive: true
                },
                WatchRoot {
                    path: nested.join("appendix"),
                    recursive: false
                },
                WatchRoot {
                    path: lookalike,
                    recursive: true
                },
            ]
        );
        let roots = normalize_roots(vec![
            WatchRoot {
                path: parent.clone(),
                recursive: false,
            },
            WatchRoot {
                path: nested.clone(),
                recursive: true,
            },
        ]);
        assert_eq!(
            roots,
            vec![
                WatchRoot {
                    path: parent,
                    recursive: false
                },
                WatchRoot {
                    path: nested,
                    recursive: true
                },
            ]
        );
    }

    #[test]
    fn nested_workspace_keeps_sources_ignored_by_its_parent_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "nested/\n").unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::create_dir(nested.join(".git")).unwrap();
        let source = nested.join("part.tex");
        std::fs::write(&source, "Nested workspace source.\n").unwrap();
        assert!(!scan(&[dir.path().to_path_buf()]).contains_key(&source));
        let roots = normalize_roots(vec![
            WatchRoot {
                path: dir.path().to_path_buf(),
                recursive: true,
            },
            WatchRoot {
                path: nested,
                recursive: true,
            },
        ]);
        let files = scan_scope_cancellable(&roots, &[], &AtomicBool::new(false)).unwrap();
        assert!(
            files.contains_key(&source),
            "explicit nested workspace must retain its own ignore context"
        );
    }

    #[test]
    fn shallow_roots_preserve_direct_events_and_deep_explicit_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let source = dir.path().join("main.tex");
        let config = dir.path().join("tex-ls.toml");
        let nested_source = nested.join("part.tex");
        let artifact = nested.join("main.aux");
        for path in [&source, &config, &nested_source, &artifact] {
            std::fs::write(path, "first").unwrap();
        }
        let roots = [WatchRoot {
            path: dir.path().to_path_buf(),
            recursive: false,
        }];
        let cancelled = AtomicBool::new(false);
        let first =
            scan_scope_cancellable(&roots, std::slice::from_ref(&artifact), &cancelled).unwrap();
        assert_eq!(first.len(), 3);
        assert!(!first.contains_key(&nested_source));
        assert!(
            changes(&BTreeMap::new(), &first)
                .iter()
                .all(|event| event.kind == FileChangeType::Created)
        );
        for path in [&source, &config, &artifact] {
            std::fs::write(path, "replacement").unwrap();
        }
        let changed =
            scan_scope_cancellable(&roots, std::slice::from_ref(&artifact), &cancelled).unwrap();
        let events = changes(&first, &changed);
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.kind == FileChangeType::Changed)
        );
        let recursive = scan(&[dir.path().to_path_buf()]);
        assert!(
            recursive.contains_key(&nested_source),
            "explicit workspaces retain recursive discovery"
        );
        for path in [&source, &config, &artifact] {
            std::fs::remove_file(path).unwrap();
        }
        let deleted =
            scan_scope_cancellable(&roots, std::slice::from_ref(&artifact), &cancelled).unwrap();
        let events = changes(&changed, &deleted);
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.kind == FileChangeType::Deleted)
        );
    }

    #[test]
    fn dropping_watcher_cancels_an_active_scan_and_joins_its_worker() {
        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (exited_tx, exited_rx) = crossbeam_channel::bounded(1);
        struct OnExit(Sender<()>);
        impl Drop for OnExit {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }
        let exited = OnExit(exited_tx);
        let watcher = FallbackWatcher::with_scanner(move |_, _, cancelled| {
            let _retain_until_thread_exit = &exited;
            entered_tx.send(()).unwrap();
            while !is_cancelled(cancelled) {
                std::thread::yield_now();
            }
            None
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(watcher);
        exited_rx
            .try_recv()
            .expect("Drop must join the cancelled worker");
    }

    #[test]
    fn dropping_watcher_wakes_and_joins_a_waiting_worker() {
        let (control, commands) = crossbeam_channel::unbounded::<Scope>();
        let (_output, events) = crossbeam_channel::unbounded();
        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (woke_tx, woke_rx) = crossbeam_channel::bounded(1);
        let worker = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            if commands.recv_timeout(Duration::from_secs(1)).is_ok() {
                woke_tx.send(()).unwrap();
            }
        });
        let watcher = FallbackWatcher {
            control,
            events,
            settings: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            worker: Some(worker),
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(watcher);
        woke_rx
            .try_recv()
            .expect("Drop must wake the polling receive before joining");
    }

    #[test]
    fn unreadable_artifact_content_retains_the_metadata_fallback() {
        struct Unreadable;
        impl std::io::Read for Unreadable {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("unreadable artifact"))
            }
        }
        assert_eq!(
            content_hash(Unreadable, &AtomicBool::new(false)),
            Some(None)
        );
    }

    #[test]
    fn cancellation_stops_artifact_hashing_between_chunks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cancelled = AtomicBool::new(false);
        struct CancellingReader<'a> {
            cancelled: &'a AtomicBool,
            reads: usize,
        }
        impl std::io::Read for CancellingReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                buffer.fill(b'x');
                self.cancelled.store(true, Ordering::Relaxed);
                Ok(buffer.len())
            }
        }
        let mut reader = CancellingReader {
            cancelled: &cancelled,
            reads: 0,
        };
        assert!(content_hash(&mut reader, &cancelled).is_none());
        assert_eq!(
            reader.reads, 1,
            "cancel before reading another artifact chunk"
        );
        assert!(
            scan_cancellable(&[WatchRoot::inferred(PathBuf::from("unused"))], &cancelled).is_none()
        );
    }

    #[test]
    fn scope_bursts_keep_the_latest_ignored_artifact_candidate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
        std::fs::write(dir.path().join("main.tex"), "Text.\n").unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir(&build).unwrap();
        let mut watcher = FallbackWatcher::new();
        watcher.update(vec![dir.path().to_path_buf()], vec![]);
        watcher.events.recv_timeout(Duration::from_secs(5)).unwrap();
        let latest = build.join("file255.aux");
        std::fs::write(&latest, "result").unwrap();
        for index in 0..256 {
            watcher.update(
                vec![dir.path().to_path_buf()],
                vec![build.join(format!("file{index}.aux"))],
            );
        }
        let events = watcher.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            events
                .iter()
                .any(|event| uri_to_fs_path(&event.uri).as_ref() == Some(&latest)
                    && event.kind == FileChangeType::Created)
        );
    }

    #[test]
    fn fallback_detects_artifact_changes_with_identical_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.aux");
        std::fs::write(&path, "\\newlabel{intro}{{1}{1}}\n").unwrap();
        let roots = [dir.path().to_path_buf()];
        let before = scan_scope(&roots, std::slice::from_ref(&path));
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "\\newlabel{intro}{{2}{1}}\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let after = scan_scope(&roots, std::slice::from_ref(&path));
        assert_eq!(before[&path].0, after[&path].0);
        assert_eq!(before[&path].1, after[&path].1);
        let events = changes(&before, &after);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FileChangeType::Changed);
    }
    #[test]
    fn ignored_artifacts_refresh_without_discovering_ignored_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "build/\n*.log\n").unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir(&build).unwrap();
        std::fs::write(build.join("generated.tex"), "ignored source").unwrap();
        let roots = [dir.path().to_path_buf()];
        let empty = scan(&roots);
        assert!(empty.is_empty());
        let paths = [
            dir.path().join("main.log"),
            build.join("main.aux"),
            build.join("main.fls"),
        ];
        for path in &paths {
            std::fs::write(path, "first").unwrap();
        }
        let first = scan_scope(&roots, &paths);
        assert_eq!(first.len(), 3);
        assert!(
            changes(&empty, &first)
                .iter()
                .all(|event| event.kind == FileChangeType::Created)
        );
        for path in &paths {
            std::fs::write(path, "replacement").unwrap();
        }
        let next = scan_scope(&roots, &paths);
        let events = changes(&first, &next);
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.kind == FileChangeType::Changed)
        );
        for path in &paths {
            std::fs::remove_file(path).unwrap();
        }
        let events = changes(&next, &scan_scope(&roots, &paths));
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.kind == FileChangeType::Deleted)
        );
    }

    #[test]
    fn fallback_tracks_source_config_artifact_replacement_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        // Isolate the fixture from ancestor ignore files, including when TMPDIR
        // points into the repository's ignored target directory.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let source = dir.path().join("part.def");
        let log = dir.path().join("main.log");
        std::fs::write(&source, "first").unwrap();
        std::fs::write(&log, "warning").unwrap();
        let roots = [dir.path().to_path_buf()];
        let first = scan(&roots);
        assert_eq!(first.len(), 2);
        assert!(changes(&first, &first).is_empty());
        std::fs::write(&source, "replacement").unwrap();
        std::fs::remove_file(log).unwrap();
        std::fs::write(dir.path().join("tex-ls.toml"), "").unwrap();
        let events = changes(&first, &scan(&roots));
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .any(|event| event.kind == FileChangeType::Deleted)
        );
    }
}
