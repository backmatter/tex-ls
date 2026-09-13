//! Observations acquired by a host and captured by analysis snapshots.
use crate::incremental::{AnalysisRevision, ProjectId};
use crate::project::{aux::ParsedAux, texmf::TexmfIndex};
use crate::text::SourceText;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

/// Unknown observations never establish absence. Errors remain distinct so a
/// host can report acquisition failures without inventing an empty result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "camelCase")]
pub enum Observation<T> {
    #[default]
    Unknown,
    Present(T),
    Absent,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocationKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryContents {
    pub entries: BTreeMap<String, LocationKind>,
    pub complete: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationObservation {
    pub kind: Observation<LocationKind>,
    pub directory: Observation<DirectoryContents>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileInputs {
    pub locations: Vec<(PathBuf, LocationObservation)>,
    pub backing: Vec<(PathBuf, Option<Arc<SourceText>>)>,
    pub file_resolution: Vec<(PathBuf, Observation<PathBuf>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstalledMetadata {
    pub toolchain: String,
    pub index: TexmfIndex,
}

/// Compiler facts describe an artifact, not current source definitions. A host
/// that cannot identify the compiled revision leaves `built_from` unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerArtifact {
    pub identity: String,
    pub content_fingerprint: u64,
    pub built_from: Option<AnalysisRevision>,
    pub parsed: ParsedAux,
    pub messages: Vec<compiler::Message>,
    pub recorder: Option<compiler::Recorder>,
}

impl CompilerArtifact {
    pub fn from_text(path: &std::path::Path, text: &str, identity: String) -> Self {
        Self::from_text_in(
            path,
            text,
            identity,
            path.parent().unwrap_or(std::path::Path::new("")),
        )
    }
    pub fn from_text_in(
        path: &std::path::Path,
        text: &str,
        identity: String,
        directory: &std::path::Path,
    ) -> Self {
        let extension = path.extension().and_then(|value| value.to_str());
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hash);
        directory.to_string_lossy().as_ref().hash(&mut hash);
        Self {
            identity,
            content_fingerprint: hash.finish(),
            built_from: None,
            parsed: if extension == Some("aux") {
                crate::project::aux::parse_aux(text)
            } else {
                ParsedAux::default()
            },
            messages: if extension == Some("log") {
                compiler::parse_log(text, directory)
            } else {
                Vec::new()
            },
            recorder: (extension == Some("fls")).then(|| compiler::parse_recorder(text, directory)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalInputKind {
    Files,
    Installed,
    Compiler,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExternalGenerations {
    pub files: u64,
    pub installed: u64,
    pub compiler: u64,
}

/// An acquisition belongs to a live project and one reserved refresh generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcquisitionToken {
    pub(crate) project: ProjectId,
    pub(crate) kind: ExternalInputKind,
    pub(crate) generation: u64,
    pub(crate) epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalInputs {
    Files(FileInputs),
    Installed(Observation<InstalledMetadata>),
    Compiler(Vec<(PathBuf, Observation<CompilerArtifact>)>),
}

pub mod compiler;
mod resolution;
pub use resolution::is_literal_file_name;
pub use resolution::{
    FileCandidates, FilePathContext, command_directories, completion_directories,
    inherited_graphics,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "path", rename_all = "camelCase")]
pub enum InputNeed {
    Location(PathBuf),
    Source(PathBuf),
    Installed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResolution {
    pub target: Observation<PathBuf>,
    pub needs: Vec<InputNeed>,
}

pub mod links;

/// Ordered compiler-artifact candidates for the selected source namespace.
pub fn compiler_candidates(
    namespace: &[&std::path::Path],
    root: &std::path::Path,
    aux_dir: Option<&std::path::Path>,
) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let base = aux_dir.map(|dir| {
        if dir.is_absolute() {
            dir.to_owned()
        } else {
            root.join(dir)
        }
    });
    namespace
        .iter()
        .filter(|member| member.extension().is_some_and(|ext| ext == "tex"))
        .map(|member| {
            let logical = member.with_extension("aux");
            let mut candidates = Vec::new();
            if let Some(base) = &base {
                if let Ok(relative) = member.strip_prefix(root) {
                    candidates.push(base.join(relative).with_extension("aux"));
                }
                if let Some(name) = member.file_name() {
                    candidates.push(base.join(name).with_extension("aux"));
                }
            } else {
                candidates.push(logical.clone());
            }
            let mut candidates: Vec<_> = candidates
                .into_iter()
                .map(|path| crate::source::normalize_path(&path))
                .collect();
            candidates.dedup();
            (logical, candidates)
        })
        .collect()
}

/// Map physical job artifacts to stable source-owned identities. Child AUX names
/// remain source-relative; only the selected compilation root uses the job name.
pub fn compiler_candidates_for_job(
    namespace: &[&std::path::Path],
    root_document: &std::path::Path,
    aux_dir: Option<&std::path::Path>,
    job_name: Option<&str>,
) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let root = root_document.parent().unwrap_or(std::path::Path::new(""));
    let mut candidates = compiler_candidates(namespace, root, aux_dir);
    if let Some(job_name) = job_name {
        for (logical, physical) in &mut candidates {
            if *logical == root_document.with_extension("aux") {
                for path in physical {
                    path.set_file_name(format!("{job_name}.aux"));
                }
            }
        }
    }
    candidates
}

/// Literal magic-root comments are acquisition hints, never parser signatures.
/// All distinct hints are retained so hosts can expose conflicting declarations.
pub fn explicit_root_hints(text: &str, source: &std::path::Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for line in text.lines().take(100) {
        let Some(comment) = line.trim_start().strip_prefix('%') else {
            continue;
        };
        let Some((key, value)) = comment.trim().split_once('=') else {
            continue;
        };
        if !key
            .split_whitespace()
            .collect::<String>()
            .eq_ignore_ascii_case("!texroot")
        {
            continue;
        }
        let value = value.trim();
        if value.is_empty() || value.contains(['\0', '{', '}', '\\']) {
            continue;
        }
        let path = crate::source::normalize_path(
            &source
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .join(value),
        );
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}
