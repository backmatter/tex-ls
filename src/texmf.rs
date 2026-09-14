//! A read-only index of the installed TEXMF tree, for **LSP-only** package
//! resolution (document links, hover, go-to-definition, and installed-set
//! completion).
//!
//! This is the one place tex-ls looks past the document's own directory into the
//! wider TeX installation. It is deliberately fenced off from the formatter: the
//! index never feeds `scope_signatures`/`DiskPackageSource`, so `tex-ls format`
//! output depends only on the input and the shipped data, never on what is installed
//! (the "deterministic, rule-based formatting" tenet — see `AGENTS.md`). Reading
//! `.sty`/`.cls`/`.dtx` filenames off disk is not typesetting and runs no TeX engine.
//!
//! Two halves, per the design split:
//! - **Root discovery is delegated.** Reproducing kpathsea's `texmf.cnf` resolution
//!   is a fragile rabbit hole (and MiKTeX doesn't use it), so we ask the installed
//!   tool: `kpsewhich -var-value=TEXMF{HOME,LOCAL,DIST,MAIN}`. When `kpsewhich` is
//!   absent we fall back to default-path heuristics; when nothing resolves the index
//!   is empty and resolution degrades to today's local-only behavior.
//! - **Enumeration is ours.** Given the roots, a by-name lookup needs no kpathsea: we
//!   read each root's `ls-R` filename database when present (TeX Live ships it) or
//!   walk once, building a `filename -> path` map. This yields the whole installed
//!   set (which `kpsewhich`, one file per call, cannot) for completion.
//!
//! The built index is cached to the OS cache dir keyed by a distro fingerprint (root
//! paths + `ls-R`/root mtimes) and rebuilt when that changes, so the walk runs at most
//! once per unchanged installation. Each settings value owns shared refresh state;
//! cloned jobs share complete immutable indexes while background checks run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The `texmf` editor settings: how the language server discovers the installed TeX
/// tree for *LSP-only* package resolution (document links, hover, go-to-definition,
/// and installed-set completion). This never feeds the formatter — `tex-ls format`
/// stays hermetic regardless of what is installed (see `AGENTS.md`).
///
/// Where an installation lives is a fact about the *machine*, not the project, so
/// these settings come from the editor (`initializationOptions` or
/// `workspace/didChangeConfiguration`, camelCase JSON), never from `tex-ls.toml` —
/// a committed project config can't point at paths that only exist on one
/// contributor's system.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TexmfConfig {
    /// Whether to scan the TEXMF tree at all. When `false`, package resolution stays
    /// local to the document's directory.
    pub enabled: bool,
    /// Extra TEXMF root directories to index in addition to (and ahead of) the
    /// discovered ones. Useful for a non-standard install `kpsewhich` can't see.
    pub roots: Vec<PathBuf>,
    /// Whether to shell out to `kpsewhich -var-value=…` to discover the tree roots.
    /// When `false`, discovery falls back to default-path heuristics only.
    pub use_kpsewhich: bool,
    /// Index only configured roots; ignore kpsewhich, fallback paths and TEXINPUTS.
    pub explicit_only: bool,
}

impl Default for TexmfConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            roots: Vec::new(),
            use_kpsewhich: true,
            explicit_only: false,
        }
    }
}

/// File extensions the index tracks: package/class sources and their literate `.dtx`.
const INDEXED_EXTS: &[&str] = &["sty", "cls", "dtx", "tex", "def", "lco", "bib", "bibtex"];

/// The `kpsewhich` variables naming the standard content trees, most-specific first
/// (so a user/local override shadows the distribution copy).
const TEXMF_VARS: &[&str] = &["TEXMFHOME", "TEXMFLOCAL", "TEXMFDIST", "TEXMFMAIN"];

/// A session-owned installation index. Cloned read jobs share its lazy initialization.
/// New editor settings create a new index, so one session cannot fix another's roots.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "TexmfConfig")]
pub struct InstalledPackages {
    config: TexmfConfig,
    state: Arc<Mutex<IndexState>>,
}
#[derive(Debug, Default)]
struct IndexState {
    index: Option<Arc<TexmfIndex>>,
    checking: bool,
    checked: Option<Instant>,
    issues: Vec<String>,
    generation: u64,
    roots: Option<Vec<PathBuf>>,
    fingerprint: Option<String>,
    discovery_issues: Vec<String>,
}
impl From<TexmfConfig> for InstalledPackages {
    fn from(config: TexmfConfig) -> Self {
        Self {
            config,
            state: Default::default(),
        }
    }
}
impl Default for InstalledPackages {
    fn default() -> Self {
        TexmfConfig::default().into()
    }
}
impl PartialEq for InstalledPackages {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
    }
}
impl Eq for InstalledPackages {}
impl InstalledPackages {
    pub fn config(&self) -> &TexmfConfig {
        &self.config
    }
    /// Serve the last complete index while a background refresh checks the tree.
    /// Missing roots and trees without filename databases are checked too.
    pub fn ready_index(&self) -> Option<Arc<TexmfIndex>> {
        let mut state = self.state.lock().expect("installation state");
        if !self.config.enabled {
            return Some(
                state
                    .index
                    .get_or_insert_with(|| Arc::new(TexmfIndex::default()))
                    .clone(),
            );
        }
        if !state.checking
            && state
                .checked
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(5))
        {
            state.checking = true;
            let this = self.clone();
            std::thread::spawn(move || this.refresh());
        }
        state.index.clone()
    }

    pub fn generation(&self) -> u64 {
        self.state.lock().expect("installation state").generation
    }

    pub fn issues(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("installation state")
            .issues
            .clone()
    }

    fn refresh(&self) {
        let (roots, mut issues) = {
            let state = self.state.lock().expect("installation state");
            (state.roots.clone(), state.discovery_issues.clone())
        };
        let roots = roots.unwrap_or_else(|| {
            let discovered = discover_roots(&self.config, &mut issues);
            // Retain missing explicit roots so first materialization is observable.
            let mut roots = self.config.roots.clone();
            for root in discovered {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            if roots.is_empty() && !self.config.explicit_only {
                roots = heuristic_roots();
            }
            let mut state = self.state.lock().expect("installation state");
            state.roots = Some(roots.clone());
            state.discovery_issues = issues
                .iter()
                .filter(|issue| !issue.starts_with("Cannot read TEXMF root "))
                .cloned()
                .collect();
            roots
        });
        issues.retain(|issue| !issue.starts_with("Cannot read TEXMF root "));
        for root in &self.config.roots {
            if !root.is_dir() {
                issues.push(format!(
                    "Cannot read TEXMF root {}; check texmf.roots",
                    root.display()
                ));
            }
        }
        let fingerprint = fingerprint(&roots);
        // TEXINPUTS is intentionally rescanned in automatic mode; it can name
        // additional directories outside the discovered installation.
        if self.config.explicit_only {
            let mut state = self.state.lock().expect("installation state");
            if state.fingerprint.as_ref() == Some(&fingerprint) {
                state.checked = Some(Instant::now());
                state.checking = false;
                return;
            }
        }
        let (installed, cache_miss) = load_or_build(&roots, &fingerprint);
        let installed_cache = cache_miss.then(|| installed.by_name.clone());
        let mut files = if self.config.explicit_only {
            HashMap::new()
        } else {
            std::env::var_os("TEXINPUTS")
                .map(|value| texinput_files(&value))
                .unwrap_or_default()
        };
        for (name, path) in installed.by_name {
            files.entry(name).or_insert(path);
        }
        let index = TexmfIndex::from_files(files);
        if fingerprint != self::fingerprint(&roots) {
            // A build or clean raced enumeration. Neither publish nor cache a
            // mixed generation under an obsolete fingerprint; retry next poll.
            let mut state = self.state.lock().expect("installation state");
            state.fingerprint = None;
            state.checked = None;
            state.checking = false;
            return;
        }
        // Cache only the installed part, excluding process-local TEXINPUTS.
        if let Some(cache) = &installed_cache {
            save_cache(&fingerprint, cache);
        }
        let mut state = self.state.lock().expect("installation state");
        if state.index.as_deref() != Some(&index) {
            state.index = Some(Arc::new(index));
            state.generation += 1;
        }
        state.fingerprint = Some(fingerprint);
        state.issues = issues;
        state.checked = Some(Instant::now());
        state.checking = false;
    }

    #[cfg(test)]
    pub(crate) fn index(&self) -> Arc<TexmfIndex> {
        if !self.config.enabled {
            return self.ready_index().expect("disabled index");
        }
        if self
            .state
            .lock()
            .expect("installation state")
            .index
            .is_none()
        {
            self.refresh();
        }
        self.state
            .lock()
            .expect("installation state")
            .index
            .clone()
            .unwrap()
    }
}

/// Discover roots, then return the cached index when the distro fingerprint matches,
/// else build fresh and cache it. An empty root set yields an empty (uncached) index.
fn load_or_build(roots: &[PathBuf], fingerprint: &str) -> (TexmfIndex, bool) {
    if roots.is_empty() {
        return (TexmfIndex::default(), false);
    }
    if let Some(files) = load_cache(fingerprint) {
        return (TexmfIndex::from_files(files), false);
    }
    (build_from_roots(roots), true)
}

/// Plain TEXINPUTS elements search one directory; a trailing `//` explicitly
/// enables recursion. Other Kpathsea expansion is left to the installed tool.
fn texinput_files(value: &std::ffi::OsStr) -> HashMap<String, PathBuf> {
    let mut files = HashMap::new();
    for entry in std::env::split_paths(value) {
        let text = entry.to_string_lossy();
        if text.is_empty() || text.starts_with('~') || text.contains(['{', '}', '$', '!']) {
            continue;
        }
        if let Some(root) = text.strip_suffix("//") {
            walk_root(Path::new(root), &mut files);
        } else if let Ok(entries) = std::fs::read_dir(&entry) {
            let mut paths: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            paths.sort();
            for path in paths {
                if path.is_file()
                    && let Some(name) = path.file_name().and_then(|name| name.to_str())
                    && has_indexed_ext(name)
                {
                    files.entry(name.into()).or_insert(path);
                }
            }
        }
    }
    files
}

/// The TEXMF root directories to index: the configured extras first, then the trees
/// `kpsewhich` reports, then default-path heuristics when `kpsewhich` yields nothing.
/// Only existing directories are kept, de-duplicated in first-seen order.
fn discover_roots(config: &TexmfConfig, issues: &mut Vec<String>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let push = |roots: &mut Vec<PathBuf>, dir: PathBuf| {
        if dir.is_dir() && !roots.contains(&dir) {
            roots.push(dir);
        }
    };

    for extra in &config.roots {
        if !extra.is_dir() {
            issues.push(format!(
                "Cannot read TEXMF root {}; check texmf.roots",
                extra.display()
            ));
        }
        push(&mut roots, extra.clone());
    }
    if config.use_kpsewhich && !config.explicit_only {
        let issue_count = issues.len();
        if let Some(texmf) = kpsewhich_var("TEXMF", issues) {
            match bounded_output(
                Command::new("kpsewhich").arg(format!("--expand-braces={}", texmf.display())),
            ) {
                Ok(output) if output.status.success() => {
                    let expanded = String::from_utf8_lossy(&output.stdout)
                        .trim()
                        .replace('!', "");
                    for path in std::env::split_paths(&expanded) {
                        push(&mut roots, path);
                    }
                }
                Err(error) => issues.push(format!(
                    "kpsewhich: {error}; check PATH or set texmf.useKpsewhich=false and texmf.roots"
                )),
                _ => {}
            }
        }
        // One unavailable or hung tool is enough evidence; do not repeat its
        // timeout for each variable while the local index remains useful.
        for var in TEXMF_VARS {
            if issues.len() != issue_count {
                break;
            }
            if let Some(dir) = kpsewhich_var(var, issues) {
                push(&mut roots, dir);
            }
        }
    }
    if roots.is_empty() && !config.explicit_only {
        for dir in heuristic_roots() {
            push(&mut roots, dir);
        }
    }
    roots
}

/// Query one `kpsewhich -var-value=<var>`; `None` when `kpsewhich` is missing, errors,
/// or prints nothing (an unset variable).
fn kpsewhich_var(var: &str, issues: &mut Vec<String>) -> Option<PathBuf> {
    let output = match bounded_output(Command::new("kpsewhich").arg(format!("-var-value={var}"))) {
        Ok(output) => output,
        Err(error) => {
            issues.push(format!(
                "kpsewhich: {error}; check PATH or set texmf.useKpsewhich=false and texmf.roots"
            ));
            return None;
        }
    };
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Best-effort default TEXMF locations for when no `kpsewhich` is on `PATH`. Not
/// exhaustive — the "pure mimic" tier; a real install almost always ships `kpsewhich`.
fn heuristic_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join("texmf"));
    }
    out.push(PathBuf::from("/usr/share/texmf-dist"));
    out.push(PathBuf::from("/usr/local/share/texmf-dist"));
    out.push(PathBuf::from("/usr/share/texmf"));
    out
}

/// A change-detection string for `roots`: each root path plus the mtime of its `ls-R`
/// (or the root dir itself when there is none). A differing fingerprint forces a
/// rebuild, so an install/removal invalidates the cache.
fn fingerprint(roots: &[PathBuf]) -> String {
    use std::hash::{Hash, Hasher};
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    "tex-ls-installed-index-v2".hash(&mut fingerprint);
    for root in roots {
        root.hash(&mut fingerprint);
        let mut anchors = vec![root.clone()];
        if root.join("ls-R").is_file() {
            anchors.push(root.join("ls-R"));
        } else if let Ok(entries) = std::fs::read_dir(root.join("miktex/data/le")) {
            anchors.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
        } else {
            anchors.extend(
                ignore::WalkBuilder::new(root)
                    .standard_filters(false)
                    .build()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_dir()))
                    .map(|entry| entry.into_path()),
            );
        }
        anchors.sort();
        anchors.dedup();
        for anchor in anchors {
            anchor.hash(&mut fingerprint);
            if let Ok(meta) = anchor.metadata() {
                meta.len().hash(&mut fingerprint);
                meta.modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .hash(&mut fingerprint);
            }
        }
    }
    format!("{:x}", fingerprint.finish())
}

/// The on-disk cache file (`<cache>/tex-ls/texmf-index.json`), or `None` when no OS
/// cache dir is available.
fn cache_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("tex-ls").join("texmf-index.json"))
}

/// The cached `filename -> path` map when the stored fingerprint matches `fingerprint`.
fn load_cache(fingerprint: &str) -> Option<HashMap<String, PathBuf>> {
    load_cache_from(&cache_path()?, fingerprint)
}

/// Persist `files` under `fingerprint`. Best-effort: any I/O error is ignored (the
/// index still works this session, just uncached).
fn save_cache(fingerprint: &str, files: &HashMap<String, PathBuf>) {
    let Some(path) = cache_path() else { return };
    save_cache_to(&path, fingerprint, files);
}

/// [`load_cache`] against an explicit path (the testable core).
fn load_cache_from(path: &Path, fingerprint: &str) -> Option<HashMap<String, PathBuf>> {
    let text = std::fs::read_to_string(path).ok()?;
    let cached: CachedIndex = serde_json::from_str(&text).ok()?;
    (cached.fingerprint == fingerprint).then_some(cached.files)
}

/// [`save_cache`] against an explicit path (the testable core).
fn save_cache_to(path: &Path, fingerprint: &str, files: &HashMap<String, PathBuf>) {
    let cached = CachedIndex {
        fingerprint: fingerprint.to_string(),
        files: files.clone(),
    };
    let Ok(text) = serde_json::to_string(&cached) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

/// The serialized cache: the distro fingerprint plus the `filename -> path` map (the
/// stem lists are recomputed on load, so they aren't stored).
#[derive(Serialize, Deserialize)]
struct CachedIndex {
    fingerprint: String,
    files: HashMap<String, PathBuf>,
}

/// Parse a root's `ls-R` filename database into `(filename, absolute path)` pairs for
/// the indexed extensions, or `None` when the root has no `ls-R`.
///
/// The format is a flat listing: a line `./sub/dir:` opens a directory (relative to
/// the root), and the following non-empty, non-`%`-comment lines are its filenames
/// until the next directory header or blank line.
fn read_ls_r(root: &Path) -> Option<Vec<(String, PathBuf)>> {
    let text = std::fs::read_to_string(root.join("ls-R")).ok()?;
    let mut out = Vec::new();
    let mut dir = PathBuf::from(root);
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        if let Some(rel) = line.strip_suffix(':') {
            dir = root.join(rel.trim_start_matches("./"));
        } else if has_indexed_ext(line) {
            out.push((line.to_string(), dir.join(line)));
        }
    }
    Some(out)
}

/// Read MiKTeX's version-5 filename tables with checked offsets. Malformed
/// databases fall back to directory enumeration instead of panicking.
fn read_miktex_database(root: &Path) -> Option<Vec<(String, PathBuf)>> {
    let directory = root.join("miktex/data/le");
    let mut tables: Vec<_> = std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "fndb-5"))
        .collect();
    tables.sort();
    if tables.is_empty() {
        return None;
    }
    let mut result = Vec::new();
    for table in tables {
        for path in parse_miktex_database(&std::fs::read(table).ok()?)? {
            let path = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            if let Some(name) = path.file_name().and_then(|name| name.to_str())
                && has_indexed_ext(name)
            {
                result.push((name.to_owned(), path));
            }
        }
    }
    Some(result)
}

fn parse_miktex_database(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    let word = |offset: usize| -> Option<usize> {
        Some(
            u32::from_le_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?)
                as usize,
        )
    };
    let string = |offset: usize| -> Option<&str> {
        let tail = bytes.get(offset..)?;
        std::str::from_utf8(tail.get(..tail.iter().position(|byte| *byte == 0)?)?).ok()
    };
    if word(0)? != 0x42444e46 {
        return None;
    }
    let address = word(16)?;
    let count = word(24)?;
    bytes.get(address..address.checked_add(count.checked_mul(16)?)?)?;
    let mut paths = Vec::new();
    for index in 0..count {
        let entry = address + index * 16;
        paths.push(PathBuf::from(string(word(entry + 4)?)?).join(string(word(entry)?)?));
    }
    Some(paths)
}

/// Walk `root` for indexed files, inserting each `filename -> path` (first-seen wins),
/// used when a root has no `ls-R`. Standard ignore filters (`.gitignore`, hidden) are
/// disabled — a TEXMF tree is not a source repo.
fn walk_root(root: &Path, by_name: &mut HashMap<String, PathBuf>) {
    if !root.is_dir() {
        return;
    }
    for entry in ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .build()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && has_indexed_ext(name)
        {
            by_name
                .entry(name.to_string())
                .or_insert_with(|| path.to_path_buf());
        }
    }
}

/// Whether `filename` ends in one of the [`INDEXED_EXTS`] (ASCII-lowercased match).
fn has_indexed_ext(filename: &str) -> bool {
    Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| INDEXED_EXTS.contains(&e.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp TEXMF root with a couple of package/class files laid out in subdirs.
    fn fixture_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let latex = dir.path().join("tex/latex");
        std::fs::create_dir_all(latex.join("amsmath")).unwrap();
        std::fs::create_dir_all(latex.join("koma-script")).unwrap();
        std::fs::write(latex.join("amsmath/amsmath.sty"), "").unwrap();
        std::fs::write(latex.join("amsmath/amstext.sty"), "").unwrap();
        std::fs::write(latex.join("koma-script/scrartcl.cls"), "").unwrap();
        // A non-indexed file must be ignored.
        std::fs::write(latex.join("amsmath/amsmath.pdf"), "").unwrap();
        dir
    }

    #[test]
    fn walk_indexes_sty_and_cls_by_name() {
        let dir = fixture_tree();
        let index = build_from_roots(&[dir.path().to_path_buf()]);
        assert!(!index.is_empty());
        assert_eq!(
            index.resolve("amsmath", &["sty"]),
            Some(dir.path().join("tex/latex/amsmath/amsmath.sty").as_path())
        );
        assert_eq!(
            index.resolve("scrartcl", &["cls"]),
            Some(
                dir.path()
                    .join("tex/latex/koma-script/scrartcl.cls")
                    .as_path()
            )
        );
        // A `.pdf` sibling never enters the index.
        assert!(index.resolve("amsmath", &["pdf"]).is_none());
        assert_eq!(index.sty_stems(), &["amsmath", "amstext"]);
        assert_eq!(index.cls_stems(), &["scrartcl"]);
    }

    #[test]
    fn ls_r_is_preferred_over_walking() {
        let dir = tempfile::tempdir().unwrap();
        // No real files on disk: only an `ls-R` describing them. If `read_ls_r` is
        // used, the index is populated; a disk walk would find nothing.
        std::fs::write(
            dir.path().join("ls-R"),
            "% ls-R -- filename database\n./tex/latex/booktabs:\nbooktabs.sty\n",
        )
        .unwrap();
        let index = build_from_roots(&[dir.path().to_path_buf()]);
        assert_eq!(
            index.resolve("booktabs", &["sty"]),
            Some(dir.path().join("tex/latex/booktabs/booktabs.sty").as_path())
        );
    }

    #[test]
    fn resolve_tries_extensions_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("d/foo.dtx"), "").unwrap();
        let index = build_from_roots(&[dir.path().to_path_buf()]);
        // `.sty` is absent, so the `.dtx` literate source is the fallback hit.
        assert_eq!(
            index.resolve("foo", &["sty", "dtx"]),
            Some(dir.path().join("d/foo.dtx").as_path())
        );
    }

    #[test]
    fn cache_round_trips_and_invalidates_on_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tex-ls/texmf-index.json");
        let files: HashMap<String, PathBuf> =
            HashMap::from([("amsmath.sty".to_string(), PathBuf::from("/t/amsmath.sty"))]);
        save_cache_to(&path, "fingerprint-abc", &files);
        // Same fingerprint round-trips.
        assert_eq!(
            load_cache_from(&path, "fingerprint-abc"),
            Some(files.clone())
        );
        // A different fingerprint misses (stale cache is not returned).
        assert_eq!(load_cache_from(&path, "fingerprint-xyz"), None);
    }
}

/// Build an index over `roots` by reading each root's `ls-R` (or walking it when
/// absent). Earlier roots win a filename collision (local shadows distribution).
pub fn build_from_roots(roots: &[PathBuf]) -> TexmfIndex {
    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    for root in roots {
        match read_ls_r(root).or_else(|| read_miktex_database(root)) {
            Some(entries) => {
                for (name, path) in entries {
                    by_name.entry(name).or_insert(path);
                }
            }
            None => walk_root(root, &mut by_name),
        }
    }
    TexmfIndex::from_files(by_name)
}

use tex_ls_analysis::project::texmf::TexmfIndex;

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[test]
    fn texinputs_distinguishes_plain_and_recursive_elements() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("local.tex"), "").unwrap();
        std::fs::write(dir.path().join("nested/deep.tex"), "").unwrap();
        let short_name = dir.path().join("USER~1");
        std::fs::create_dir(&short_name).unwrap();
        std::fs::write(short_name.join("short.tex"), "").unwrap();
        assert!(texinput_files(short_name.as_os_str()).contains_key("short.tex"));
        let plain = texinput_files(dir.path().as_os_str());
        assert!(plain.contains_key("local.tex"));
        assert!(!plain.contains_key("deep.tex"));
        let recursive =
            texinput_files(std::ffi::OsStr::new(&format!("{}//", dir.path().display())));
        assert!(recursive.contains_key("deep.tex"));
    }

    #[test]
    fn installed_cache_cold_and_warm_measurement() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("ls-R");
        let mut contents = String::from("./tex/latex/fixture:\n");
        for index in 0..10000 {
            contents.push_str(&format!("package{index}.sty\n"));
        }
        std::fs::write(database, contents).unwrap();
        let start = std::time::Instant::now();
        let index = build_from_roots(&[root.path().to_path_buf()]);
        let cold = start.elapsed();
        let cache = root.path().join("cache.json");
        save_cache_to(&cache, "fixture", &index.by_name);
        let start = std::time::Instant::now();
        let warm = load_cache_from(&cache, "fixture").unwrap();
        let elapsed = start.elapsed();
        assert_eq!(warm, index.by_name);
        eprintln!("installed index 10000 names: cold={cold:?} warm={elapsed:?}");
    }

    #[test]
    fn miktex_database_checks_bounds_and_resolves_literal_paths() {
        let mut bytes = vec![0u8; 48];
        bytes[0..4].copy_from_slice(&0x42444e46u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&32u32.to_le_bytes());
        bytes[24..28].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&48u32.to_le_bytes());
        bytes.extend_from_slice(b"sample.sty\0");
        let directory = bytes.len() as u32;
        bytes[36..40].copy_from_slice(&directory.to_le_bytes());
        bytes.extend_from_slice(b"tex/latex/sample\0");
        assert_eq!(
            parse_miktex_database(&bytes).unwrap(),
            [PathBuf::from("tex/latex/sample/sample.sty")]
        );
        for end in 0..bytes.len() {
            assert!(parse_miktex_database(&bytes[..end]).is_none());
        }
        bytes[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_miktex_database(&bytes).is_none());
    }

    #[test]
    fn explicit_roots_refresh_after_creation_addition_removal_and_clean() {
        for database in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("missing");
            let installed = InstalledPackages::from(TexmfConfig {
                roots: vec![root.clone()],
                explicit_only: true,
                ..Default::default()
            });
            assert!(discover_roots(installed.config(), &mut Vec::new()).is_empty());
            assert!(installed.index().by_name.is_empty());
            std::fs::create_dir_all(root.join("tex/latex/pkg")).unwrap();
            let package = root.join("tex/latex/pkg/fresh.sty");
            std::fs::write(&package, "").unwrap();
            if database {
                std::fs::write(root.join("ls-R"), "./tex/latex/pkg:\nfresh.sty\n").unwrap();
            }
            installed.refresh();
            assert_eq!(
                installed.index().resolve("fresh", &["sty"]),
                Some(package.as_path())
            );
            let old = installed.index();
            std::fs::remove_file(&package).unwrap();
            if database {
                std::fs::write(root.join("ls-R"), "./tex/latex/pkg:\n").unwrap();
            }
            installed.refresh();
            assert!(installed.index().resolve("fresh", &["sty"]).is_none());
            assert!(old.resolve("fresh", &["sty"]).is_some());
            std::fs::remove_dir_all(&root).unwrap();
            installed.refresh();
            assert!(installed.index().by_name.is_empty());
        }
    }

    #[test]
    fn installations_are_owned_by_settings_and_shared_with_read_jobs() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("sessionone.sty"), "").unwrap();
        std::fs::write(second.path().join("sessiontwo.sty"), "").unwrap();
        let create = |path: &Path| {
            InstalledPackages::from(TexmfConfig {
                enabled: true,
                roots: vec![path.to_owned()],
                use_kpsewhich: false,
                explicit_only: true,
            })
        };
        let one = create(first.path());
        let two = create(second.path());
        assert!(one.index().resolve("sessionone", &["sty"]).is_some());
        assert!(two.index().resolve("sessiontwo", &["sty"]).is_some());
        assert!(two.index().resolve("sessionone", &["sty"]).is_none());
        assert!(Arc::ptr_eq(&one.index(), &one.clone().index()));
    }
}

/// Bound external lookup processes and reap them on timeout. Temporary output
/// files avoid pipe deadlock and reader threads retained by inherited handles.
pub(crate) fn bounded_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    use std::io::{Read, Seek};
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "TeX lookup exceeded two seconds",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.take(4 * 1024 * 1024).read_to_end(&mut out)?;
    stderr.take(64 * 1024).read_to_end(&mut err)?;
    Ok(std::process::Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(all(test, unix))]
mod timeout_tests {
    use super::*;
    #[test]
    fn stalled_lookup_is_killed_and_reaped() {
        let start = std::time::Instant::now();
        let error = bounded_output(Command::new("sh").args(["-c", "exec sleep 30"])).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }
}
