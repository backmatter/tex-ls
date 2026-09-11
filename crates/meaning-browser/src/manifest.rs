//! File manifest owned by the browser adapter. No filesystem access.
use meaning_analysis::project::{aux::AuxData, texmf::TexmfIndex};
use meaning_protocol::host::HostServices;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Default)]
pub struct ManifestHost {
    pub files: BTreeSet<PathBuf>,
    pub overlays: BTreeSet<PathBuf>,
    pub texmf: TexmfIndex,
}
impl HostServices for ManifestHost {
    fn is_file(&self, path: &Path) -> bool {
        let path = normalize(path);
        self.files.contains(&path) || self.overlays.contains(&path)
    }
    fn texmf(&self) -> &TexmfIndex {
        &self.texmf
    }
    fn aux_data(&self, _: &[&Path], _: &Path) -> Option<AuxData> {
        None
    }
    fn read_dir(&self, path: &Path) -> Vec<(String, bool)> {
        let path = normalize(path);
        let mut entries = BTreeMap::new();
        for file in self.files.iter().chain(&self.overlays) {
            let Ok(relative) = file.strip_prefix(&path) else {
                continue;
            };
            let mut components = relative.components();
            if let Some(name) = components.next() {
                let directory = components.next().is_some();
                entries
                    .entry(name.as_os_str().to_string_lossy().into_owned())
                    .and_modify(|existing| *existing |= directory)
                    .or_insert(directory);
            }
        }
        entries.into_iter().collect()
    }
}

pub use meaning_analysis::source::normalize_path as normalize;
