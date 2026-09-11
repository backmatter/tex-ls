//! Ordered literal-file candidates shared by analysis and host acquisition.
use crate::project::package::dtx_source_of;
use crate::source::normalize_path;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCandidates {
    pub local: Vec<PathBuf>,
    pub installed: Option<(String, Vec<String>)>,
}

impl FileCandidates {
    pub fn new(
        raw: &str,
        extensions: &[&str],
        literate_fallback: bool,
        base: Option<&Path>,
    ) -> Self {
        let raw = Path::new(raw);
        let mut candidates: Vec<_> = if raw.extension().is_some() {
            vec![raw.to_path_buf()]
        } else {
            extensions
                .iter()
                .map(|ext| raw.with_extension(ext))
                .collect()
        };
        if literate_fallback {
            let literate: Vec<_> = candidates
                .iter()
                .filter_map(|path| dtx_source_of(path))
                .collect();
            candidates.extend(literate);
        }
        let local = candidates
            .into_iter()
            .map(|candidate| {
                normalize_path(&match base {
                    Some(base) if candidate.is_relative() => base.join(candidate),
                    _ => candidate,
                })
            })
            .collect();
        let installed = if raw
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            None
        } else {
            raw.file_stem().and_then(|stem| stem.to_str()).map(|stem| {
                let mut extensions: Vec<String> = match raw.extension().and_then(|ext| ext.to_str())
                {
                    Some(ext) => vec![ext.into()],
                    None => extensions.iter().map(|ext| (*ext).into()).collect(),
                };
                if literate_fallback {
                    extensions.push("dtx".into());
                }
                (stem.into(), extensions)
            })
        };
        Self { local, installed }
    }
}
