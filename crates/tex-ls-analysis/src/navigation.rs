//! Shared label/citation namespaces with current display and edit ranges.
mod names;

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
        if self.resolve_citations().manual_is_defined(origin, key) {
            return None;
        }
        self.resolve_citations()
            .bibliography_resources(origin)
            .into_iter()
            .find_map(|path| {
                let file = self.lookup_file(path)?;
                let entry = self
                    .bib_semantic_model(file)
                    .entries()
                    .iter()
                    .find(|entry| {
                        self.resolve_citations().keys_equal(origin, &entry.key, key)
                            || (self.resolve_citations().supports_aliases(origin)
                                && entry.dependencies.iter().any(|dependency| {
                                    dependency.field == "ids"
                                        && dependency.key.as_ref().is_some_and(|alias| {
                                            self.resolve_citations().keys_equal(origin, alias, key)
                                        })
                                }))
                    })?;
                Some((file, entry))
            })
    }
    /// An opaque entry-key expression might resolve to any selected key. Refuse
    /// edits rather than changing declarations while leaving those uses behind.
    pub fn citation_dependencies_editable(&self, origin: &Path) -> bool {
        let citations = self.resolve_citations();
        citations.has_consistent_key_case(origin)
            && citations
                .bibliography_resources(origin)
                .into_iter()
                .all(|path| {
                    self.lookup_file(path).is_some_and(|file| {
                        self.bib_semantic_model(file)
                            .entries()
                            .iter()
                            .flat_map(|entry| &entry.dependencies)
                            .all(|dependency| dependency.key.is_some() && dependency.field != "ids")
                    })
                })
    }
    pub fn bibliography_string_occurrences(&self, origin: &Path, key: &str) -> Vec<KeyOccurrence> {
        self.resolve_citations()
            .bibliography_resources(origin)
            .into_iter()
            .flat_map(|path| {
                let Some(file) = self.lookup_file(path) else {
                    return Vec::new();
                };
                let model = self.bib_semantic_model(file);
                model
                    .string_occurrences(key)
                    .map(|(range, definition)| KeyOccurrence {
                        path: path.into(),
                        range,
                        key_range: range,
                        definition,
                    })
                    .collect()
            })
            .collect()
    }
    pub fn bibliography_string_rename_allowed(&self, origin: &Path, key: &str) -> bool {
        let sites = self.bibliography_string_occurrences(origin, key);
        let definitions: Vec<_> = sites.iter().filter(|site| site.definition).collect();
        if definitions.len() != 1 {
            return false;
        }
        let resources = self.resolve_citations().bibliography_resources(origin);
        // Editing a shared macro must include all scopes loading its definition.
        self.resolve_citations()
            .bibliography_resources(&definitions[0].path)
            .iter()
            .all(|path| resources.contains(path))
            && self
                .resolve_citations()
                .bibliography_scopes(&definitions[0].path)
                .all(|(_, _, closed, _, _, _)| closed)
    }
    pub fn bibliography_string_expansion(&self, origin: &Path, key: &str) -> String {
        let mut seen = std::collections::HashSet::new();
        let mut ambiguous = std::collections::HashSet::new();
        let roots: Vec<_> = self
            .resolve_citations()
            .bibliography_resources(origin)
            .into_iter()
            .filter_map(|path| {
                let file = self.lookup_file(path)?;
                for site in self.bib_semantic_model(file).string_defs() {
                    if !seen.insert(site.name.to_string()) {
                        ambiguous.insert(site.name.to_string());
                    }
                }
                Some(self.parsed_bib_tree(file))
            })
            .collect();
        crate::bib::render::expanded_roots(&roots, ambiguous, key)
    }
    /// Translate a canonical identity to the authored suffix at this occurrence.
    pub fn key_replacement(
        &self,
        path: &Path,
        range: TextRange,
        canonical: &str,
    ) -> Option<String> {
        let file = self.lookup_file(path)?;
        let prefix = if lint_file_kind(path) == Some(FileKind::Bib) {
            ""
        } else {
            let root = self.parsed_tree(file);
            // The declaration string belongs to the project input, not the CST.
            let prefix = crate::completion::key_prefix_at_offset(
                &root,
                usize::from(range.start()),
                self.declarations_for(path),
            )
            .unwrap_or("");
            return canonical
                .strip_prefix(prefix)
                .filter(|suffix| {
                    !suffix.is_empty()
                        && (prefix.is_empty()
                            || tex_ls_parser::declarations::is_literal_key_suffix(suffix))
                })
                .map(str::to_owned);
        };
        canonical
            .strip_prefix(prefix)
            .filter(|suffix| !suffix.is_empty())
            .map(str::to_owned)
    }
    pub fn canonical_key_at(&self, path: &Path, range: TextRange, suffix: &str) -> Option<String> {
        let file = self.lookup_file(path)?;
        if lint_file_kind(path) == Some(FileKind::Bib) {
            return Some(suffix.into());
        }
        let root = self.parsed_tree(file);
        let prefix = crate::completion::key_prefix_at_offset(
            &root,
            usize::from(range.start()),
            self.declarations_for(path),
        )
        .unwrap_or("");
        Some(format!("{prefix}{suffix}"))
    }
    pub fn label_context(
        &self,
        origin: &Path,
        name: &str,
    ) -> Option<tex_ls_parser::semantic::LabelContext> {
        self.label_context_ref(origin, name).cloned()
    }

    /// Borrow the cached description without copying a section or caption title.
    pub fn label_context_ref(
        &self,
        origin: &Path,
        name: &str,
    ) -> Option<&tex_ls_parser::semantic::LabelContext> {
        std::iter::once(origin)
            .chain(
                self.resolve_labels()
                    .definers(origin, name)
                    .iter()
                    .map(PathBuf::as_path),
            )
            .find_map(|path| {
                let file = self.lookup_file(path)?;
                self.file_label_contexts(file).get(name).flatten()
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
            for item in self
                .semantic_model(file)
                .bibitems()
                .iter()
                .filter(|item| names.iter().any(|name| name == &item.name))
            {
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
                    names.iter().any(|name| {
                        if citations.manual_is_defined(path, &citation.name) {
                            name == &citation.name
                        } else {
                            citations.keys_equal(path, name, &citation.name)
                        }
                    })
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
        let definitions = citations.bibliography_resources(origin);
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
                        .any(|name| citations.keys_equal(origin, name, &entry.key))
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
        for path in citations.bibliography_resources(origin) {
            let Some(file) = self.lookup_file(path) else {
                continue;
            };
            for dependency in self
                .bib_semantic_model(file)
                .entries()
                .iter()
                .flat_map(|entry| &entry.dependencies)
            {
                if citations.supports_dependency(origin, &dependency.field)
                    && dependency.key.as_ref().is_some_and(|key| {
                        names
                            .iter()
                            .any(|name| citations.keys_equal(origin, name, key))
                    })
                {
                    occurrences.push(KeyOccurrence {
                        path: path.into(),
                        range: dependency.range,
                        key_range: dependency.range,
                        definition: dependency.field == "ids",
                    });
                }
            }
        }
        occurrences.sort_by(|a, b| {
            (&a.path, a.key_range.start(), a.definition).cmp(&(
                &b.path,
                b.key_range.start(),
                b.definition,
            ))
        });
        occurrences.dedup();
        occurrences
    }
}
