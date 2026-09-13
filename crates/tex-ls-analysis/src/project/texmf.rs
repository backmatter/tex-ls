//! Immutable installed-source index supplied by the host.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
/// An immutable `filename -> absolute path` index over one or more TEXMF roots, plus
/// the sorted `.sty`/`.cls` stem lists for completion. Cheap to clone-by-reference
/// (it lives behind a `OnceLock`); built once per install.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TexmfIndex {
    /// `"amsmath.sty" -> /usr/share/texmf-dist/tex/latex/amsmath/amsmath.sty`.
    pub by_name: HashMap<String, PathBuf>,
    /// Sorted, de-duplicated `.sty` stems (`amsmath`, `tikz`, …).
    sty_stems: Vec<String>,
    /// Sorted, de-duplicated `.cls` stems (`article`, `beamer`, …).
    cls_stems: Vec<String>,
}

impl TexmfIndex {
    /// Assemble the index (and its derived stem lists) from a `filename -> path` map.
    pub fn from_files(by_name: HashMap<String, PathBuf>) -> Self {
        let sty_stems = sorted_stems(&by_name, "sty");
        let cls_stems = sorted_stems(&by_name, "cls");
        Self {
            by_name,
            sty_stems,
            cls_stems,
        }
    }

    /// The installed path for `stem` under the first of `exts` that exists in the
    /// tree, or `None`. `exts` are tried in order (e.g. `["sty", "dtx"]` for a
    /// package with a `.dtx`-only literate source).
    pub fn resolve(&self, stem: &str, exts: &[&str]) -> Option<&Path> {
        for ext in exts {
            if let Some(path) = self.by_name.get(&format!("{stem}.{ext}")) {
                return Some(path);
            }
        }
        None
    }

    /// The sorted `.sty` stems, for `\usepackage` installed-set completion.
    pub fn sty_stems(&self) -> &[String] {
        &self.sty_stems
    }

    /// The sorted `.cls` stems, for `\documentclass` installed-set completion.
    pub fn cls_stems(&self) -> &[String] {
        &self.cls_stems
    }

    /// Whether the index found nothing (no TeX install, or scanning disabled). Callers
    /// treat an empty index the same as no index — resolution stays local-only.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

fn sorted_stems(by_name: &HashMap<String, PathBuf>, ext: &str) -> Vec<String> {
    let suffix = format!(".{ext}");
    let mut stems: Vec<String> = by_name
        .keys()
        .filter_map(|name| name.strip_suffix(&suffix))
        .map(str::to_string)
        .collect();
    stems.sort_unstable();
    stems.dedup();
    stems
}
