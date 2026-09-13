//! Shared label/citation namespaces with current display and edit ranges.
use crate::incremental::Analysis;
use crate::source::{FileKind, lint_file_kind};
use rowan::TextRange;
use smol_str::SmolStr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOccurrence {
    pub path: PathBuf,
    pub range: TextRange,
    pub key_range: TextRange,
    pub definition: bool,
}
impl Analysis {
    pub fn citation_definition(
        &self,
        origin: &Path,
        key: &str,
    ) -> Option<(
        crate::incremental::SourceId,
        &tex_ls_parser::bib::semantic::Entry,
    )> {
        self.resolve_citations()
            .bib_definers(origin)
            .iter()
            .find_map(|path| {
                let file = self.lookup_file(path)?;
                let entry = self
                    .bib_semantic_model(file)
                    .entries()
                    .iter()
                    .find(|entry| entry.key.eq_ignore_ascii_case(key))?;
                Some((file, entry))
            })
    }
    pub fn label_context(
        &self,
        origin: &Path,
        name: &str,
    ) -> Option<tex_ls_parser::semantic::LabelContext> {
        std::iter::once(origin)
            .chain(
                self.resolve_labels()
                    .definers(origin, name)
                    .iter()
                    .map(PathBuf::as_path),
            )
            .find_map(|path| {
                let file = self.lookup_file(path)?;
                let label = self
                    .semantic_model(file)
                    .labels()
                    .iter()
                    .find(|label| label.name == name)?;
                tex_ls_parser::semantic::label_context(
                    &self.parsed_tree(file),
                    label.key_range.start(),
                )
            })
    }
    pub fn label_occurrences(&self, origin: &Path, names: &[SmolStr]) -> Vec<KeyOccurrence> {
        let mut occurrences = Vec::new();
        for path in self.resolve_labels().namespace_members(origin) {
            let Some(file) = self.lookup_file(path) else {
                continue;
            };
            let model = self.semantic_model(file);
            for reference in model
                .refs()
                .iter()
                .filter(|reference| names.contains(&reference.name))
            {
                occurrences.push(KeyOccurrence {
                    path: path.into(),
                    range: reference.range,
                    key_range: reference.key_range,
                    definition: false,
                });
            }
            for label in model
                .labels()
                .iter()
                .filter(|label| names.contains(&label.name))
            {
                occurrences.push(KeyOccurrence {
                    path: path.into(),
                    range: label.range,
                    key_range: label.key_range,
                    definition: true,
                });
            }
        }
        occurrences
    }
    pub fn citation_occurrences(&self, origin: &Path, names: &[SmolStr]) -> Vec<KeyOccurrence> {
        let citations = self.resolve_citations();
        let bib = lint_file_kind(origin) == Some(FileKind::Bib);
        let members = if bib {
            citations.bib_citers(origin)
        } else {
            citations.namespace_members(origin)
        };
        let mut occurrences = Vec::new();
        for path in members {
            let Some(file) = self.lookup_file(path) else {
                continue;
            };
            for item in self.semantic_model(file).bibitems().iter().filter(|item| {
                names
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&item.name))
            }) {
                occurrences.push(KeyOccurrence {
                    path: path.into(),
                    range: item.range,
                    key_range: item.key_range,
                    definition: true,
                });
            }
            for citation in self
                .semantic_model(file)
                .citations()
                .iter()
                .filter(|citation| {
                    names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&citation.name))
                })
            {
                occurrences.push(KeyOccurrence {
                    path: path.into(),
                    range: citation.range,
                    key_range: citation.key_range,
                    definition: false,
                });
            }
        }
        let definitions = if bib {
            vec![origin]
        } else {
            citations
                .bib_definers(origin)
                .iter()
                .map(PathBuf::as_path)
                .collect()
        };
        for path in definitions {
            let Some(file) = self.lookup_file(path) else {
                continue;
            };
            for entry in self
                .bib_semantic_model(file)
                .entries()
                .iter()
                .filter(|entry| {
                    names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&entry.key))
                })
            {
                occurrences.push(KeyOccurrence {
                    path: path.into(),
                    range: entry.key_range,
                    key_range: entry.key_range,
                    definition: true,
                });
            }
        }
        occurrences
    }
}
