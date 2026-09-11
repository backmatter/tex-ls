//! Filesystem fixtures for protocol tests only.
use super::*;
#[derive(Default)]
pub struct TestHost {
    pub index: TexmfIndex,
}
impl TestHost {
    pub fn from_roots(roots: &[PathBuf]) -> Self {
        let mut files = HashMap::new();
        let mut pending = roots.to_vec();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    files
                        .entry(entry.file_name().to_string_lossy().into_owned())
                        .or_insert(entry.path());
                }
            }
        }
        Self {
            index: TexmfIndex::from_files(files),
        }
    }
}
impl HostServices for TestHost {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn read_dir(&self, path: &Path) -> Vec<(String, bool)> {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.file_type().unwrap().is_dir(),
                )
            })
            .collect()
    }
    fn texmf(&self) -> &TexmfIndex {
        &self.index
    }
    fn aux_data(&self, namespace: &[&Path], _: &Path) -> Option<AuxData> {
        let mut data = AuxData::default();
        for path in namespace {
            if let Ok(text) = std::fs::read_to_string(path.with_extension("aux")) {
                let parsed = meaning_analysis::project::aux::parse_aux(&text);
                data.labels.extend(parsed.data.labels);
                data.toc.extend(parsed.data.toc);
            }
        }
        Some(data)
    }
}
