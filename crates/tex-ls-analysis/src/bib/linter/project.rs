//! Root-scoped bibliography facts, shared by the CLI and incremental linter.
use crate::{bib::semantic::Model, project::ResolvedCitations, semantic::SemanticModel};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFacts {
    pub complete: bool,
    pub wildcard: bool,
    pub used: BTreeSet<String>,
    pub duplicates: BTreeMap<String, Vec<(PathBuf, usize, usize)>>,
    pub local_duplicates: BTreeSet<usize>,
    pub scoped: bool,
    pub undefined_dependencies: BTreeMap<usize, (String, usize)>,
    pub defined_strings: BTreeSet<String>,
    pub used_strings: BTreeSet<String>,
}
impl ProjectFacts {
    pub fn build<'a>(
        path: &Path,
        citations: &ResolvedCitations,
        tex: impl IntoIterator<Item = (&'a Path, &'a SemanticModel)>,
        bib: impl IntoIterator<Item = (&'a Path, &'a Model)>,
    ) -> Self {
        let tex: HashMap<_, _> = tex.into_iter().collect();
        let bib: HashMap<_, _> = bib.into_iter().collect();
        let mut result = Self {
            complete: true,
            ..Default::default()
        };
        let mut count = 0;
        let mut resolved_dependencies = BTreeSet::new();
        for (members, resources, complete, wildcard, case_sensitive, supports_aliases) in
            citations.bibliography_scopes(path)
        {
            count += 1;
            result.scoped = true;
            result.complete &= complete;
            result.wildcard |= wildcard;
            let identity = |key: &str| {
                if case_sensitive {
                    key.to_string()
                } else {
                    key.to_ascii_lowercase()
                }
            };
            let mut used = BTreeSet::new();
            let mut definitions: BTreeMap<String, Vec<(PathBuf, usize, usize)>> = BTreeMap::new();
            let mut manual_definitions: BTreeMap<String, Vec<(PathBuf, usize, usize)>> =
                BTreeMap::new();
            for member in members {
                let Some(model) = tex.get(member.as_path()) else {
                    result.complete = false;
                    continue;
                };
                used.extend(model.citations().iter().map(|cite| identity(&cite.name)));
                for item in model.bibitems() {
                    manual_definitions
                        .entry(item.name.to_string())
                        .or_default()
                        .push((
                            member.clone(),
                            item.key_range.start().into(),
                            item.key_range.end().into(),
                        ));
                }
            }
            for resource in resources {
                let Some(model) = bib.get(resource.as_path()) else {
                    result.complete = false;
                    continue;
                };
                result.complete &= !supports_aliases
                    || crate::project::citations::document_cite_identity_complete(model);
                result
                    .defined_strings
                    .extend(model.string_defs().iter().map(|site| site.name.to_string()));
                result
                    .used_strings
                    .extend(model.string_uses().iter().map(|site| site.name.to_string()));
                let mut seen = BTreeSet::new();
                for item in model.entries() {
                    if resource == path {
                        if !seen.insert(identity(&item.key)) {
                            result
                                .local_duplicates
                                .insert(usize::from(item.key_range.start()));
                        }
                        continue;
                    }
                    definitions.entry(identity(&item.key)).or_default().push((
                        resource.clone(),
                        item.key_range.start().into(),
                        item.key_range.end().into(),
                    ));
                }
            }
            // Reachability counts inherited and related entries as used. Each
            // identity is expanded once, including cycles and shared parents.
            let mut entries: HashMap<_, Vec<_>> = HashMap::new();
            for resource in resources {
                if let Some(model) = bib.get(resource.as_path()) {
                    for entry in model.entries() {
                        entries
                            .entry(identity(&entry.key))
                            .or_default()
                            .push((resource.as_path(), entry));
                        for alias in entry
                            .dependencies
                            .iter()
                            .filter(|dependency| supports_aliases && dependency.field == "ids")
                            .filter_map(|dependency| dependency.key.as_ref())
                        {
                            entries
                                .entry(identity(alias))
                                .or_default()
                                .push((resource.as_path(), entry));
                        }
                    }
                }
            }
            let mut expanded = std::collections::HashSet::new();
            let mut pending: Vec<_> = used.iter().cloned().collect();
            while let Some(key) = pending.pop() {
                if let Some(matches) = entries.get(&key) {
                    for &(resource, entry) in matches {
                        if !expanded.insert((resource, entry.key_range)) {
                            continue;
                        }
                        let primary = identity(&entry.key);
                        if used.insert(primary.clone()) {
                            pending.push(primary);
                        }
                        for dependency in &entry.dependencies {
                            if dependency.field == "ids"
                                || (dependency.field != "crossref" && !supports_aliases)
                            {
                                continue;
                            }
                            if let Some(key) = &dependency.key {
                                let key = identity(key);
                                if used.insert(key.clone()) {
                                    pending.push(key);
                                }
                            } else {
                                result.complete = false;
                            }
                        }
                    }
                }
            }
            if let Some(model) = bib.get(path) {
                for entry in model.entries() {
                    for dependency in entry.dependencies.iter().filter(|dependency| {
                        dependency.field == "crossref"
                            || (supports_aliases && dependency.field != "ids")
                    }) {
                        if let Some(key) = &dependency.key {
                            let start = usize::from(dependency.range.start());
                            if entries.contains_key(&identity(key)) {
                                resolved_dependencies.insert(start);
                            } else {
                                result.undefined_dependencies.insert(
                                    start,
                                    (key.to_string(), usize::from(dependency.range.end())),
                                );
                            }
                        }
                    }
                    if used.contains(identity(&entry.key).as_str()) {
                        result.used.insert(entry.key.to_string());
                    }
                    for others in definitions
                        .get(identity(&entry.key).as_str())
                        .into_iter()
                        .chain(manual_definitions.get(entry.key.as_str()))
                    {
                        result
                            .duplicates
                            .entry(entry.key.to_string())
                            .or_default()
                            .extend(others.iter().cloned());
                    }
                }
            }
        }
        result.complete &= count > 0;
        result
            .undefined_dependencies
            .retain(|start, _| !resolved_dependencies.contains(start));
        for definitions in result.duplicates.values_mut() {
            definitions.sort();
            definitions.dedup();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    #[test]
    fn duplicate_and_usage_facts_follow_independent_roots() {
        let mut db = IncrementalDatabase::default();
        let root = Path::new("/bib/main.tex");
        let a = Path::new("/bib/a.bib");
        let b = Path::new("/bib/b.bib");
        db.apply_change(
            root,
            "\\documentclass{article}\\bibliography{a}\\cite{used}",
            None,
        );
        db.apply_change(
            Path::new("/bib/other.tex"),
            "\\documentclass{article}\\bibliography{b}",
            None,
        );
        db.apply_change(a, "@misc{used,title={A}}\n@misc{unused,title={B}}", None);
        db.apply_change(b, "@misc{used,title={C}}", None);
        let findings = db.bib_lint_findings(db.lookup_file(a).unwrap());
        assert!(!findings.iter().any(|d| d.rule == "duplicate-key"));
        assert_eq!(
            findings.iter().filter(|d| d.rule == "unused-entry").count(),
            1
        );
        db.apply_change(
            root,
            "\\documentclass{article}\\bibliography{a,b}\\nocite{*}\\cite{missing}",
            None,
        );
        let findings = db.bib_lint_findings(db.lookup_file(a).unwrap());
        assert!(
            findings
                .iter()
                .any(|d| d.rule == "duplicate-key" && d.related[0].path == b)
        );
        assert!(!findings.iter().any(|d| d.rule == "unused-entry"));
        assert!(
            db.latex_lint_findings(db.lookup_file(root).unwrap())
                .iter()
                .any(|d| d.rule == "undefined-citation")
        );
        db.apply_change(
            root,
            "\\documentclass{article}\\bibliography{a}\\input{missing}",
            None,
        );
        assert!(
            !db.bib_lint_findings(db.lookup_file(a).unwrap())
                .iter()
                .any(|d| d.rule == "unused-entry")
        );
    }
}
