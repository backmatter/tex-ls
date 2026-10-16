//! Command and environment navigation, namespace scope and rename permission.
use crate::incremental::{Analysis, SourceId};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[cfg(test)]
std::thread_local! {
    static DEFINITION_CONFLICT_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn conflicting_definition_ranges(
    sites: &[crate::semantic::DefSite],
    name: &str,
) -> HashSet<rowan::TextRange> {
    sites
        .iter()
        .filter_map(|other| {
            #[cfg(test)]
            DEFINITION_CONFLICT_PROBES.with(|count| count.set(count.get() + 1));
            (other.name != name).then_some(other.name_range)
        })
        .collect()
}

impl Analysis {
    /// All supported static definitions, retaining ambiguity in namespace order.
    pub fn name_definitions(
        &self,
        origin: &Path,
        target: &crate::name_refs::NameTarget,
    ) -> Vec<(SourceId, rowan::TextRange)> {
        self.check_cancelled();
        let want = match target.kind {
            crate::name_refs::NameKind::Command => crate::semantic::DefSiteKind::Command,
            crate::name_refs::NameKind::Environment => crate::semantic::DefSiteKind::Environment,
        };
        let mut result = Vec::new();
        for member in
            crate::name_refs::macro_namespace(self.resolve_labels(), self.package_graph(), origin)
        {
            let Some(file) = self.lookup_file(&member) else {
                continue;
            };
            if want == crate::semantic::DefSiteKind::Command
                && self.uneditable_command_names(file).contains(&target.name)
            {
                // Literal alias sources and generated names need edits beyond
                // this target's ordinary control-sequence occurrences.
                return Vec::new();
            }
            let sites = self.definition_sites(file);
            if !sites
                .iter()
                .any(|site| site.kind == want && site.name == target.name)
            {
                continue;
            }
            // Every candidate has the target name. A differently named site at
            // the exact same range therefore disqualifies it, regardless of kind.
            let conflicts = conflicting_definition_ranges(sites, &target.name);
            result.extend(
                sites
                    .iter()
                    .filter(|site| site.kind == want && site.name == target.name)
                    .filter(|site| !conflicts.contains(&site.name_range))
                    .filter(|site| {
                        site.kind != crate::semantic::DefSiteKind::Command
                            || self.file_text(file)[site.name_range] == format!("\\{}", site.name)
                    })
                    .map(|site| (file, site.name_range)),
            );
        }
        result
    }

    /// Root-scoped and acquired format sources that affect command diagnostics.
    pub fn command_source_files(&self, origin: &Path) -> Vec<PathBuf> {
        self.check_cancelled();
        let Some(file) = self.lookup_file(origin) else {
            return Vec::new();
        };
        let mut sources: std::collections::BTreeSet<_> =
            self.command_scope_paths(file).map(Path::to_owned).collect();
        for path in self.format_source_paths() {
            let actual = self.file_alias(&path).unwrap_or(&path);
            if let Some(file) = self.lookup_file(actual) {
                sources.extend(self.command_scope_paths(file).map(Path::to_owned));
            }
            sources.insert(path);
        }
        sources.into_iter().collect()
    }

    /// Navigation also follows acquired installed sources. Keep this separate
    /// from rename, which requires a project-owned definition.
    pub fn navigation_definitions(
        &self,
        origin: &Path,
        target: &crate::name_refs::NameTarget,
    ) -> Vec<(SourceId, rowan::TextRange)> {
        let want = match target.kind {
            crate::name_refs::NameKind::Command => crate::semantic::DefSiteKind::Command,
            crate::name_refs::NameKind::Environment => crate::semantic::DefSiteKind::Environment,
        };
        // Navigation is read-only: the extra alias checks required by rename
        // do not affect which definition sites we may show here.
        let mut local = Vec::new();
        for member in
            crate::name_refs::macro_namespace(self.resolve_labels(), self.package_graph(), origin)
        {
            let Some(file) = self.lookup_file(&member) else {
                continue;
            };
            local.extend(
                self.definition_sites(file)
                    .iter()
                    .filter(|site| site.kind == want && site.name == target.name)
                    .map(|site| (file, site.name_range)),
            );
        }
        let roots =
            crate::name_refs::macro_roots(self.resolve_labels(), self.package_graph(), origin);
        if !local.is_empty() && roots.len() <= 1 {
            return local;
        }
        let mut result = local;
        let Some(file) = self.lookup_file(origin) else {
            return result;
        };
        let mut origins = vec![file];
        if roots.len() > 1 {
            origins.extend(roots.iter().filter_map(|path| self.lookup_file(path)));
        }
        let inputs: std::collections::HashSet<_> = origins
            .into_iter()
            .flat_map(|file| self.command_scope_files(file))
            .collect();
        for file in inputs {
            self.check_cancelled();
            result.extend(
                self.definition_sites(file)
                    .iter()
                    .filter(|site| site.kind == want && site.name == target.name)
                    .map(|site| (file, site.name_range)),
            );
        }
        if result.is_empty() {
            for path in self.format_source_paths() {
                let Some(file) = self.lookup_file(&path).or_else(|| {
                    self.file_alias(&path)
                        .and_then(|actual| self.lookup_file(actual))
                }) else {
                    continue;
                };
                for file in self.command_scope_files(file) {
                    result.extend(
                        self.definition_sites(file)
                            .iter()
                            .filter(|site| site.kind == want && site.name == target.name)
                            .map(|site| (file, site.name_range)),
                    );
                }
            }
        }
        if result.is_empty() && target.kind == crate::name_refs::NameKind::Environment {
            // TeX environments can be implemented as \name / \endname macros.
            let command = crate::name_refs::NameTarget {
                kind: crate::name_refs::NameKind::Command,
                ..target.clone()
            };
            let end = crate::name_refs::NameTarget {
                name: format!("end{}", target.name).into(),
                ..command.clone()
            };
            if !self.navigation_definitions(origin, &end).is_empty() {
                result = self.navigation_definitions(origin, &command);
            }
        }
        result.sort_by_key(|(file, range)| {
            (self.file_path(*file).to_owned(), range.start(), range.end())
        });
        result.dedup();
        result
    }

    /// References and rename consume the same exact current occurrence set.
    pub fn name_references(
        &self,
        origin: &Path,
        target: &crate::name_refs::NameTarget,
        include_declaration: bool,
    ) -> Vec<(SourceId, rowan::TextRange)> {
        self.check_cancelled();
        let mut result = Vec::new();
        for member in
            crate::name_refs::macro_namespace(self.resolve_labels(), self.package_graph(), origin)
        {
            let Some(file) = self.lookup_file(&member) else {
                continue;
            };
            let definitions = self.definition_sites(file);
            let occurrences = self.name_occurrences(file);
            match target.kind {
                crate::name_refs::NameKind::Command => {
                    let occurrences = occurrences.commands(&target.name);
                    if occurrences.is_empty() {
                        continue;
                    }
                    let definition_ranges: HashSet<_> = if include_declaration {
                        HashSet::new()
                    } else {
                        definitions
                            .iter()
                            .filter(|site| {
                                site.kind == crate::semantic::DefSiteKind::Command
                                    && site.name == target.name
                            })
                            .map(|site| site.name_range)
                            .collect()
                    };
                    result.extend(
                        occurrences
                            .iter()
                            .filter(|range| {
                                include_declaration || !definition_ranges.contains(*range)
                            })
                            .map(|range| (file, *range)),
                    );
                }
                crate::name_refs::NameKind::Environment => {
                    result.extend(
                        occurrences
                            .environments(&target.name)
                            .iter()
                            .map(|range| (file, *range)),
                    );
                    if include_declaration {
                        result.extend(
                            definitions
                                .iter()
                                .filter(|site| {
                                    site.kind == crate::semantic::DefSiteKind::Environment
                                        && site.name == target.name
                                })
                                .map(|site| (file, site.name_range)),
                        );
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;

    #[test]
    fn definition_conflict_index_matches_exact_name_range_reference() {
        use crate::semantic::{DefSite, DefSiteKind};
        use rowan::{TextRange, TextSize};

        let mut sites = Vec::new();
        for (name, kind, start, end) in [
            ("same", DefSiteKind::Command, 1, 3),
            ("same", DefSiteKind::Command, 1, 3),
            ("same", DefSiteKind::Environment, 1, 3),
            ("first", DefSiteKind::Command, 4, 7),
            ("second", DefSiteKind::Environment, 4, 7),
            ("empty", DefSiteKind::Command, 7, 7),
            ("other", DefSiteKind::Environment, 7, 7),
            ("touching", DefSiteKind::Command, 7, 9),
            ("overlap", DefSiteKind::Command, 8, 10),
            ("different-end", DefSiteKind::Command, 7, 10),
        ] {
            let range = TextRange::new(TextSize::new(start), TextSize::new(end));
            sites.push(DefSite {
                name: name.into(),
                kind,
                name_range: range,
                range,
            });
        }
        for order in 0..sites.len() {
            sites.rotate_left(1);
            if order % 2 == 0 {
                sites.reverse();
            }
            for target in sites
                .iter()
                .map(|site| site.name.as_str())
                .chain(["absent"])
            {
                let indexed = conflicting_definition_ranges(&sites, target);
                for candidate in sites.iter().filter(|site| site.name == target) {
                    let original = sites.iter().any(|other| {
                        other.name_range == candidate.name_range && other.name != candidate.name
                    });
                    assert_eq!(
                        indexed.contains(&candidate.name_range),
                        original,
                        "{order}: {candidate:?}"
                    );
                }
            }
        }
        assert!(conflicting_definition_ranges(&[], "absent").is_empty());
    }

    #[test]
    fn repeated_name_definitions_preserve_exact_sites_with_linear_conflict_work() {
        use crate::name_refs::{NameKind, NameTarget};
        use crate::semantic::DefSiteKind;
        use rowan::{TextRange, TextSize};

        let path = Path::new("/p/redefinitions.tex");
        let mut source = String::from("\\newcommand{\\demo}{Initial}\n");
        for _ in 0..1024 {
            source.push_str("\\renewcommand{\\demo}{Updated}\n");
        }
        source.push_str("\\newcommand{\\other}{Unrelated}\n\\demo{}\n");
        let mut db = IncrementalDatabase::default();
        let file = db.apply_change(path, source.as_str(), None);
        let snapshot = db.snapshot();
        assert!(snapshot.parse_diagnostics(file).is_empty());
        assert_eq!(snapshot.parsed_tree(file).text().to_string(), source);
        let target = NameTarget {
            kind: NameKind::Command,
            name: "demo".into(),
            span: TextRange::empty(TextSize::new(source.rfind("\\demo{}").unwrap() as u32 + 1)),
        };
        let sites = snapshot.definition_sites(file);
        let expected: Vec<_> = sites
            .iter()
            .filter(|site| site.kind == DefSiteKind::Command && site.name == "demo")
            .map(|site| (file, site.name_range))
            .collect();
        assert_eq!(expected.len(), 1025);
        assert!(
            expected
                .windows(2)
                .all(|pair| pair[0].1.start() < pair[1].1.start())
        );
        assert!(
            expected
                .iter()
                .all(|(_, range)| &source[*range] == "\\demo")
        );
        DEFINITION_CONFLICT_PROBES.with(|count| count.set(0));
        let actual = snapshot.name_definitions(path, &target);
        assert_eq!(actual, expected);
        let probes = DEFINITION_CONFLICT_PROBES.with(std::cell::Cell::get);
        assert!(probes > 0, "the real ambiguity check must execute");
        assert!(
            probes <= 4 * sites.len(),
            "{probes} probes for {} definition sites",
            sites.len()
        );
        DEFINITION_CONFLICT_PROBES.with(|count| count.set(0));
        let absent = NameTarget {
            name: "absent".into(),
            ..target
        };
        assert!(snapshot.name_definitions(path, &absent).is_empty());
        assert_eq!(DEFINITION_CONFLICT_PROBES.with(std::cell::Cell::get), 0);
    }
}
