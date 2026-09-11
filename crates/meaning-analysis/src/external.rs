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
    pub bibliography: Vec<(PathBuf, Observation<PathBuf>)>,
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
    pub built_from: Option<AnalysisRevision>,
    pub parsed: ParsedAux,
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

mod resolution;
pub use resolution::FileCandidates;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputNeed {
    Location(PathBuf),
    Installed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResolution {
    pub target: Observation<PathBuf>,
    pub needs: Vec<InputNeed>,
}
