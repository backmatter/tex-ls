//! One deterministic matching/ranking/limiting stage for every completion family.
use lsp_types::{CompletionItem, CompletionList};
use std::collections::HashMap;

/// Selected by the large-library benchmark in examples/completion_benchmark.rs.
pub const RESULT_LIMIT: usize = 100;

fn score(query: &str, field: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    if field == query {
        return Some(0);
    }
    if field.starts_with(query) {
        return Some(1);
    }
    if let Some(index) = field.find(query) {
        return Some(10 + index);
    }
    let mut source = field.char_indices();
    let mut last = 0;
    let mut gaps = 0;
    for wanted in query.chars() {
        let (index, _) = source.find(|(_, ch)| *ch == wanted)?;
        gaps += index.saturating_sub(last);
        last = index + wanted.len_utf8();
    }
    Some(100 + gaps)
}

pub(crate) struct MatchQuery {
    query: String,
    tokens: Vec<String>,
}

impl MatchQuery {
    pub(crate) fn new(query: &str) -> Self {
        let query = query.to_lowercase();
        let tokens = query.split_whitespace().map(str::to_owned).collect();
        Self { query, tokens }
    }

    pub(crate) fn score_item(&self, item: &CompletionItem) -> Option<usize> {
        self.score_text(item.filter_text.as_deref().unwrap_or(&item.label))
    }

    pub(crate) fn score_text(&self, text: &str) -> Option<usize> {
        let search = text.to_lowercase();
        self.tokens.iter().try_fold(0usize, |total, token| {
            score(token, &search).map(|score| total + score)
        })
    }
}

pub fn rank(
    items: Vec<CompletionItem>,
    query: &str,
    relevance: &HashMap<String, u8>,
    limit: usize,
    recompute: bool,
) -> CompletionList {
    let matcher = MatchQuery::new(query);
    let mut ranked: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            let label = item.label.to_lowercase();
            let score = matcher.score_item(&item)?;
            let priority = relevance.get(&item.label).copied().unwrap_or(1);
            Some((
                (
                    label != matcher.query,
                    priority,
                    score,
                    label,
                    item.label.clone(),
                ),
                item,
            ))
        })
        .collect();
    ranked.sort_by(|(a, _), (b, _)| a.cmp(b));
    let incomplete = recompute || ranked.len() > limit;
    ranked.truncate(limit);
    let items = ranked
        .into_iter()
        .enumerate()
        .map(|(rank, (_, mut item))| {
            item.sort_text = Some(format!("{rank:06}"));
            item
        })
        .collect();
    CompletionList {
        is_incomplete: incomplete,
        items,
        item_defaults: None,
        apply_kind: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_fields_unicode_and_provenance_precede_the_cap_deterministically() {
        let items: Vec<_> = (0..300)
            .map(|i| CompletionItem {
                label: format!("key{i:03}"),
                filter_text: Some(format!("{} José García", "A long title ".repeat(40))),
                ..Default::default()
            })
            .collect();
        let relevance = HashMap::from([("key299".into(), 0)]);
        let result = rank(items.clone(), "JOSÉ gar", &relevance, 50, false);
        assert!(result.is_incomplete);
        assert_eq!(result.items[0].label, "key299");
        let reverse = rank(
            items.into_iter().rev().collect(),
            "JOSÉ gar",
            &relevance,
            50,
            false,
        );
        assert_eq!(result, reverse);
        assert!(!rank(Vec::new(), "missing", &relevance, 50, false).is_incomplete);
    }
}
