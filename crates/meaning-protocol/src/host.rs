//! External facts supplied explicitly by each host.
use meaning_analysis::project::{aux::AuxData, texmf::TexmfIndex};
use std::path::Path;
pub trait HostServices {
    fn is_file(&self, path: &Path) -> bool;
    fn read_dir(&self, path: &Path) -> Vec<(String, bool)>;
    fn texmf(&self) -> &TexmfIndex;
    fn aux_data(&self, namespace: &[&Path], root: &Path) -> Option<AuxData>;
}
