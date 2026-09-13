//! Acquire explicit fixture observations before protocol computations run.
use super::*;
use tex_ls_analysis::external::*;
use tex_ls_analysis::incremental::IncrementalDatabase;
#[derive(Default)]
pub struct TestHost {
    db: IncrementalDatabase,
    pub index: TexmfIndex,
}
impl std::ops::Deref for TestHost {
    type Target = Analysis;
    fn deref(&self) -> &Analysis {
        &self.db
    }
}
impl TestHost {
    pub fn for_directory(path: &Path) -> Self {
        Self::with_directory(path, TexmfIndex::default())
    }
    pub fn with_directory(path: &Path, index: TexmfIndex) -> Self {
        let mut result = Self {
            db: IncrementalDatabase::default(),
            index,
        };
        let mut pending = vec![path.to_path_buf()];
        let mut files = FileInputs::default();
        while let Some(path) = pending.pop() {
            let mut contents = DirectoryContents {
                complete: true,
                ..Default::default()
            };
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let directory = entry.file_type().unwrap().is_dir();
                    contents.entries.insert(
                        entry.file_name().to_string_lossy().into_owned(),
                        if directory {
                            LocationKind::Directory
                        } else {
                            LocationKind::File
                        },
                    );
                    if directory {
                        pending.push(entry.path());
                    }
                }
            }
            files.locations.push((
                path,
                LocationObservation {
                    kind: Observation::Present(LocationKind::Directory),
                    directory: Observation::Present(contents),
                },
            ));
        }
        let project = result.db.project_id();
        let token = result
            .db
            .begin_external_refresh(project, ExternalInputKind::Files)
            .unwrap();
        result
            .db
            .apply_external_inputs(token, ExternalInputs::Files(files))
            .unwrap();
        let token = result
            .db
            .begin_external_refresh(project, ExternalInputKind::Installed)
            .unwrap();
        result
            .db
            .apply_external_inputs(
                token,
                ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                    toolchain: "fixture".into(),
                    index: result.index.clone(),
                })),
            )
            .unwrap();
        result
    }
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
        Self::with_directory(&roots[0], TexmfIndex::from_files(files))
    }
}
