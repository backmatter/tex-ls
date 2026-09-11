//! Native filesystem and installed-toolchain facts.
use crate::config::BuildConfig;
use crate::texmf::InstalledPackages;
use meaning_analysis::project::{aux::AuxData, texmf::TexmfIndex};
use meaning_protocol::host::HostServices;
use std::path::Path;
pub(crate) struct NativeServices<'a> {
    pub aux: &'a crate::aux::AuxCache,
    pub texmf: &'a InstalledPackages,
    pub build: &'a BuildConfig,
}
impl HostServices for NativeServices<'_> {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn read_dir(&self, path: &Path) -> Vec<(String, bool)> {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.file_type().is_ok_and(|kind| kind.is_dir()),
                )
            })
            .collect()
    }
    fn texmf(&self) -> &TexmfIndex {
        self.texmf.index()
    }
    fn aux_data(&self, namespace: &[&Path], root: &Path) -> Option<AuxData> {
        self.aux
            .data_for(namespace, root, self.build.aux_dir.as_deref())
    }
}
