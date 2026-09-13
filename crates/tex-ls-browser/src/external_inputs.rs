//! Generic embedding input protocol. Acquisition tokens belong to this session;
//! a host publishes a completed batch, then requests a fresh feature result.
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf};
use tex_ls_analysis::external::*;
use tex_ls_analysis::project::texmf::TexmfIndex;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum InputBatch {
    Files {
        #[serde(default)]
        locations: Vec<(PathBuf, LocationObservation)>,
        #[serde(default)]
        backing: Vec<(PathBuf, Option<String>)>,
        #[serde(default)]
        #[serde(rename = "fileResolution")]
        file_resolution: Vec<(PathBuf, Observation<PathBuf>)>,
    },
    Installed {
        value: Observation<Installed>,
    },
    Compiler {
        artifacts: Vec<(PathBuf, Observation<Artifact>)>,
    },
}

#[derive(Deserialize)]
pub(super) struct Installed {
    toolchain: String,
    files: HashMap<String, PathBuf>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Artifact {
    identity: String,
    text: String,
    working_directory: Option<PathBuf>,
}
fn map<T, U>(value: Observation<T>, f: impl FnOnce(T) -> U) -> Observation<U> {
    match value {
        Observation::Unknown => Observation::Unknown,
        Observation::Absent => Observation::Absent,
        Observation::Error(error) => Observation::Error(error),
        Observation::Present(value) => Observation::Present(f(value)),
    }
}
impl InputBatch {
    pub(super) fn into_inputs(self) -> ExternalInputs {
        use tex_ls_analysis::text::IntoSourceText;
        match self {
            Self::Files {
                locations,
                backing,
                file_resolution,
            } => ExternalInputs::Files(FileInputs {
                locations,
                file_resolution,
                backing: backing
                    .into_iter()
                    .map(|(path, text)| (path, text.map(IntoSourceText::into_source_text)))
                    .collect(),
            }),
            Self::Installed { value } => {
                ExternalInputs::Installed(map(value, |value| InstalledMetadata {
                    toolchain: value.toolchain,
                    index: TexmfIndex::from_files(value.files),
                }))
            }
            Self::Compiler { artifacts } => ExternalInputs::Compiler(
                artifacts
                    .into_iter()
                    .map(|(path, value)| {
                        let artifact = map(value, |value| {
                            CompilerArtifact::from_text_in(
                                &path,
                                &value.text,
                                value.identity,
                                value.working_directory.as_deref().unwrap_or_else(|| {
                                    path.parent().unwrap_or(std::path::Path::new(""))
                                }),
                            )
                        });
                        (path, artifact)
                    })
                    .collect(),
            ),
        }
    }
}
