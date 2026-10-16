//! Select citation responses before cloning cached search fields or wire metadata.
use super::{rank as completion_rank, resolve as completion_resolve};
use lsp_types::{CompletionItem, CompletionItemKind};
use std::{borrow::Cow, collections::HashSet, path::Path};
use tex_ls_analysis::{
    completion::CompletionContext, incremental::Analysis, project::ResolvedCitations,
};

struct Candidate<'a> {
    key: Cow<'a, str>,
    search: Cow<'a, str>,
    manual: bool,
}

impl Candidate<'_> {
    fn materialize(self, origin: &Path, namespace: &str) -> CompletionItem {
        #[cfg(test)]
        MATERIALIZED.with(|count| count.set(count.get() + 1));
        let insert_text = (!namespace.is_empty()).then(|| self.key[namespace.len()..].to_owned());
        CompletionItem {
            data: completion_resolve::CompletionResolveData::Citation {
                lint_path: origin.to_owned(),
                key: self.key.to_string(),
            }
            .into_value(),
            label: self.key.into_owned(),
            filter_text: Some(self.search.into_owned()),
            kind: Some(CompletionItemKind::Reference),
            detail: self.manual.then(|| "Manual bibliography item".into()),
            insert_text,
            ..Default::default()
        }
    }
}

#[cfg(test)]
thread_local! { static MATERIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// The exhaustive public API remains available; response selection borrows its
/// database vocabulary and preserves first-definer precedence before matching.
fn visit<'a>(
    snapshot: &'a Analysis,
    citations: &ResolvedCitations,
    origin: &Path,
    mut candidate: impl FnMut(Candidate<'a>),
) {
    let aliases = citations.supports_aliases(origin);
    let mut seen = HashSet::new();
    let mut seen_manual = HashSet::<Cow<'a, str>>::new();
    for path in citations.bibliography_resources(origin) {
        snapshot.check_cancelled();
        let Some(file) = snapshot.lookup_file(path) else {
            continue;
        };
        let primary_keys: Option<HashSet<&str>> = (!aliases).then(|| {
            snapshot
                .bib_semantic_model(file)
                .entries()
                .iter()
                .map(|entry| entry.key.as_str())
                .collect()
        });
        for (key, search) in snapshot.bib_search_fields(file) {
            snapshot.check_cancelled();
            if primary_keys
                .as_ref()
                .is_some_and(|keys| !keys.contains(key.as_str()))
                || !seen.insert(citations.key_identity(origin, key))
            {
                continue;
            }
            seen_manual.insert(Cow::Borrowed(key.as_str()));
            candidate(Candidate {
                key: Cow::Borrowed(key.as_str()),
                search: Cow::Borrowed(search),
                manual: false,
            });
        }
    }
    for path in citations.namespace_members(origin) {
        snapshot.check_cancelled();
        for (name, body) in crate::source_cards::manual_bodies(snapshot, path) {
            if seen_manual.insert(Cow::Owned(name.clone())) {
                let search = format!("{name} {body}");
                candidate(Candidate {
                    key: Cow::Owned(name),
                    search: Cow::Owned(search),
                    manual: true,
                });
            }
        }
    }
}

/// Materialize the full vocabulary in label order for exhaustive callers.
pub(super) fn exhaustive(
    snapshot: &Analysis,
    citations: &ResolvedCitations,
    origin: &Path,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    visit(snapshot, citations, origin, |candidate| {
        items.push(candidate.materialize(origin, ""));
    });
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

fn select(
    snapshot: &Analysis,
    origin: &Path,
    query: &str,
    namespace: &str,
    limit: usize,
) -> lsp_types::CompletionList {
    let matcher = completion_rank::MatchQuery::new(query);
    let mut selected = completion_rank::BoundedRank::new(limit);
    visit(
        snapshot,
        snapshot.resolve_citations(),
        origin,
        |candidate| {
            if !namespace.is_empty()
                && !candidate
                    .key
                    .strip_prefix(namespace)
                    .is_some_and(tex_ls_parser::declarations::is_literal_key_suffix)
            {
                return;
            }
            if let Some(key) = matcher.rank_key(&candidate.key, &candidate.search, 1) {
                selected.push(key, candidate);
            }
        },
    );
    let (candidates, is_incomplete) = selected.finish(false);
    let items = candidates
        .into_iter()
        .enumerate()
        .map(|(rank, candidate)| {
            let mut item = candidate.materialize(origin, namespace);
            item.sort_text = Some(format!("{rank:06}"));
            item
        })
        .collect();
    lsp_types::CompletionList {
        is_incomplete,
        items,
        item_defaults: None,
        apply_kind: None,
    }
}

pub(super) fn for_context(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
    context: Option<&CompletionContext>,
) -> Option<lsp_types::CompletionList> {
    let file = snapshot.lookup_file(path)?;
    let path = snapshot.file_path(file);
    match context {
        Some(CompletionContext::CitationKey { prefix }) => {
            let root = snapshot.parsed_tree(file);
            let namespace = tex_ls_analysis::completion::key_prefix_at_offset(
                &root,
                offset,
                snapshot.declarations_for(path),
            )
            .unwrap_or("");
            Some(select(
                snapshot,
                path,
                prefix,
                namespace,
                completion_rank::RESULT_LIMIT,
            ))
        }
        None => {
            let root = snapshot.parsed_bib_tree(file);
            let (_, prefix) =
                tex_ls_analysis::bib::completion::dependency_key_completion_for_backend(
                    &root,
                    offset,
                    snapshot.resolve_citations().supports_aliases(path),
                )?;
            Some(select(
                snapshot,
                path,
                &prefix,
                "",
                completion_rank::RESULT_LIMIT,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{cite_completion_items, compute_completion_impl};
    use super::*;
    use crate::convert::path_to_uri;
    use lsp_types::Position;
    use std::collections::HashMap;
    use tex_ls_analysis::{incremental::IncrementalDatabase, text::PositionEncoding};

    #[test]
    fn vocabulary_preserves_backend_identity_first_definer_and_manual_precedence() {
        let origin = Path::new(fixture_path!("/vocabulary/main.tex"));
        for backend in ["bibtex", "biber"] {
            let mut db = IncrementalDatabase::default();
            db.apply_change(
                origin,
                format!(r"\documentclass{{article}}\usepackage[backend={backend}]{{biblatex}}\addbibresource{{first.bib}}\addbibresource{{second.bib}}\bibitem{{Key}} Hidden manual\bibitem{{manual}} Visible manual"),
                None,
            );
            db.apply_change(
                &origin.with_file_name("first.bib"),
                "@misc{Key,title={First winner},ids={alias}}\n@misc{key,title={Lowercase winner}}",
                None,
            );
            db.apply_change(
                &origin.with_file_name("second.bib"),
                "@misc{Key,title={Later duplicate}}",
                None,
            );
            let snapshot = db.snapshot();
            let items = exhaustive(&snapshot, snapshot.resolve_citations(), origin);
            let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
            assert_eq!(
                labels,
                if backend == "biber" {
                    vec!["Key", "alias", "key", "manual"]
                } else {
                    vec!["Key", "manual"]
                }
            );
            let first = &items[0];
            assert!(
                first
                    .filter_text
                    .as_deref()
                    .unwrap()
                    .contains("First winner")
            );
            assert!(first.detail.is_none());
            assert!(
                items
                    .last()
                    .unwrap()
                    .filter_text
                    .as_deref()
                    .unwrap()
                    .contains("Visible manual")
            );
            let selected = select(&snapshot, origin, "Later duplicate", "", 100);
            assert!(selected.items.is_empty());
        }
    }

    #[test]
    fn bounded_selection_matches_exhaustive_case_alias_manual_and_search_policy() {
        let path = Path::new(fixture_path!("/bounded/main.tex"));
        for backend in ["bibtex", "biber"] {
            let mut db = IncrementalDatabase::default();
            db.apply_change(path, format!("\\documentclass{{article}}\\usepackage[backend={backend}]{{biblatex}}\\addbibresource{{first.bib}}\\addbibresource{{second.bib}}\\bibitem{{Manual}} Manual José\\bibitem{{manual}} Lowercase body"), None);
            let first = path.with_file_name("first.bib");
            let second = path.with_file_name("second.bib");
            let mut library = String::from(
                "@misc{Key,title={First definer},ids={alias}}\n@misc{key,title={Distinct Biber key}}\n@misc{Manual,title={Database wins exact manual collision}}\n",
            );
            for number in (0..350).rev() {
                library.push_str(&format!("@misc{{item{number:04},title={{{} José García Needle{number}}},author={{Author Name}}}}\n", "long title ".repeat(30)));
            }
            db.apply_change(&first, library, None);
            db.apply_change(&second, "@misc{Key,title={laterduplicateonly}}", None);
            let snapshot = db.snapshot();
            let all = cite_completion_items(&snapshot, snapshot.resolve_citations(), path);
            for query in [
                "",
                " \t",
                "Key",
                "KEY",
                "alias",
                "José gar",
                "Needle349",
                "itm49",
                "laterduplicateonly",
                "manual",
            ] {
                for limit in [0, 1, 100] {
                    let expected = completion_rank::rank_exhaustive(
                        all.clone(),
                        query,
                        &HashMap::new(),
                        limit,
                        false,
                    );
                    MATERIALIZED.with(|count| count.set(0));
                    let actual = select(&snapshot, path, query, "", limit);
                    assert_eq!(actual, expected, "{backend}: {query}: {limit}");
                    assert!(MATERIALIZED.with(|count| count.get()) <= limit);
                }
            }
        }
    }

    #[test]
    fn bounded_pipeline_matches_exhaustive_response_and_exact_edits() {
        let path = Path::new(fixture_path!("/bounded/main.tex"));
        let bib = path.with_file_name("refs.bib");
        let mut db = IncrementalDatabase::default();
        let declarations: tex_ls_parser::declarations::Declarations =
            serde_json::from_str(r#"{"commands":{"wrapper":{"like":"cite","prefix":"ns:"}}}"#)
                .unwrap();
        db.set_declarations(declarations.resolve().unwrap());
        let mut library = String::from(
            "@misc{ns:paper,title={A Paper}}\n@misc{ns:two words,title={Opaque}}\n@misc{other,title={Other}}\n@misc{child,xdata={other, ns:pa , other}}",
        );
        for number in 0..350 {
            library.push_str(&format!(
                "\n@misc{{item{number:04},title={{Shared needle}}}}"
            ));
        }
        db.apply_change(&bib, library.as_str(), None);
        for call in [
            r"\cite{}",
            r"\cite{needle}",
            r"\cite{pa}",
            r"\cite{other, pa}",
            r"\wrapper{pa}",
        ] {
            let source = format!(
                r"\documentclass{{article}}\usepackage{{biblatex}}\addbibresource{{refs.bib}}{call}"
            );
            let file = db.apply_change(path, source.as_str(), None);
            let snapshot = db.snapshot();
            let at = source.len() - 1;
            for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                let (line, character) = snapshot.file_line_index(file, encoding).position(at);
                let position = Position::new(line, character);
                let uri = path_to_uri(path).unwrap();
                assert_eq!(
                    compute_completion_impl(&snapshot, &uri, path, encoding, position, true),
                    compute_completion_impl(&snapshot, &uri, path, encoding, position, false)
                );
            }
        }
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&bib).unwrap();
        let at = library.rfind("ns:pa").unwrap() + 5;
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(at);
        let uri = path_to_uri(&bib).unwrap();
        assert_eq!(
            compute_completion_impl(
                &snapshot,
                &uri,
                &bib,
                PositionEncoding::Utf16,
                Position::new(line, character),
                true
            ),
            compute_completion_impl(
                &snapshot,
                &uri,
                &bib,
                PositionEncoding::Utf16,
                Position::new(line, character),
                false
            )
        );
    }
}
