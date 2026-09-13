//! Native fallback watching. Client watching owns the scope only after acknowledgement.
use super::*;
use lsp_types::FileEvent;
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

type Fingerprint = (Option<SystemTime>, u64, Option<u64>);
type Scope = (Vec<PathBuf>, Vec<PathBuf>);

pub(super) struct FallbackWatcher {
    control: Sender<Scope>,
    pub events: Receiver<Vec<FileEvent>>,
    settings: Option<Scope>,
}

impl FallbackWatcher {
    pub fn new() -> Self {
        let (control, commands) = crossbeam_channel::unbounded::<Scope>();
        let (output, events) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let mut roots = Vec::new();
            let mut artifacts = Vec::new();
            let mut previous = BTreeMap::new();
            let interval = Duration::from_secs(1);
            let mut next_scan = Instant::now();
            loop {
                match commands.recv_timeout(next_scan.saturating_duration_since(Instant::now())) {
                    Ok(mut scope) => {
                        // Artifact discovery may publish thousands of scope updates.
                        // Only the latest scope matters to the next polling scan.
                        while let Ok(latest) = commands.try_recv() {
                            scope = latest;
                        }
                        let (next_roots, next_artifacts) = scope;
                        if roots != next_roots {
                            next_scan = Instant::now();
                        }
                        previous.retain(|path: &PathBuf, _| {
                            next_roots.iter().any(|root| path.starts_with(root))
                                || next_artifacts.binary_search(path).is_ok()
                        });
                        roots = next_roots;
                        artifacts = next_artifacts;
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                }
                if Instant::now() < next_scan {
                    continue;
                }
                let started = Instant::now();
                let next = scan_scope(&roots, &artifacts);
                log::debug!(
                    "fallback watch: {} roots, {} files, {} artifact candidates, {:?}",
                    roots.len(),
                    next.len(),
                    artifacts.len(),
                    started.elapsed()
                );
                next_scan = Instant::now() + interval;
                let changes = changes(&previous, &next);
                previous = next;
                if !changes.is_empty() && output.send(changes).is_err() {
                    break;
                }
            }
        });
        Self {
            control,
            events,
            settings: None,
        }
    }

    pub fn update(&mut self, mut roots: Vec<PathBuf>, mut artifacts: Vec<PathBuf>) {
        roots.sort();
        roots.dedup();
        artifacts.sort();
        artifacts.dedup();
        let next = (roots, artifacts);
        if self.settings.as_ref() != Some(&next) {
            let _ = self.control.send(next.clone());
            self.settings = Some(next);
        }
    }
}

fn scan_scope(roots: &[PathBuf], artifacts: &[PathBuf]) -> BTreeMap<PathBuf, Fingerprint> {
    let mut files = scan(roots);
    // Hash only the compiler files this server actually acquired or requested.
    // Other files retain the existing metadata-only discovery behavior.
    for path in artifacts {
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
        {
            files.insert(path.clone(), fingerprint(path, &meta, true));
        }
    }
    files
}

fn scan(roots: &[PathBuf]) -> BTreeMap<PathBuf, Fingerprint> {
    let mut files = BTreeMap::new();
    for root in roots {
        // Ignored build outputs are covered by explicit compiler candidates in
        // scan_scope, including missing paths and acquired AUX chains.
        for entry in ignore::WalkBuilder::new(root).build().flatten() {
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
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                files
                    .entry(path.to_path_buf())
                    .or_insert_with(|| fingerprint(path, &meta, false));
            }
        }
    }
    files
}

fn fingerprint(path: &Path, meta: &std::fs::Metadata, artifact: bool) -> Fingerprint {
    use std::hash::Hasher;
    use std::io::Read;
    // Artifact acquisition is cached until a watch event. Detect equal-size,
    // equal-mtime rebuilds here on the polling thread, never on the writer.
    let content = artifact
        .then(|| {
            let mut file = std::fs::File::open(path).ok()?;
            let mut digest = std::collections::hash_map::DefaultHasher::new();
            let mut buffer = [0; 16384];
            loop {
                let count = file.read(&mut buffer).ok()?;
                if count == 0 {
                    break;
                }
                digest.write(&buffer[..count]);
            }
            Some(digest.finish())
        })
        .flatten();
    (meta.modified().ok(), meta.len(), content)
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
