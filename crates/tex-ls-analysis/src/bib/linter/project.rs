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
        for (members, resources, complete, wildcard) in citations.bibliography_scopes(path) {
            count += 1;
            result.complete &= complete;
            result.wildcard |= wildcard;
            for member in members {
                let Some(model) = tex.get(member.as_path()) else {
                    result.complete = false;
                    continue;
                };
                result.used.extend(
                    model
                        .citations()
                        .iter()
                        .map(|cite| cite.name.to_lowercase()),
                );
                for item in model.bibitems() {
                    result
                        .duplicates
                        .entry(item.name.to_lowercase())
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
                if resource == path {
                    continue;
                }
                for item in model.entries() {
                    result
                        .duplicates
                        .entry(item.key.to_lowercase())
                        .or_default()
                        .push((
                            resource.clone(),
                            item.key_range.start().into(),
                            item.key_range.end().into(),
                        ));
                }
            }
        }
        result.complete &= count > 0;
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
