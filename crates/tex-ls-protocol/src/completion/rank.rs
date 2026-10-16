//! One deterministic matching/ranking/limiting stage for every completion family.
use lsp_types::{CompletionItem, CompletionList};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
};

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

pub(crate) type RankKey = (bool, u8, usize, String, String);

struct Ranked<T> {
    key: RankKey,
    ordinal: usize,
    value: T,
}

impl<T> PartialEq for Ranked<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.ordinal == other.ordinal
    }
}

impl<T> Eq for Ranked<T> {}

impl<T> PartialOrd for Ranked<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Ranked<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| self.ordinal.cmp(&other.ordinal))
    }
}

/// Counts all matches while retaining only the best bounded candidate payloads.
pub(crate) struct BoundedRank<T> {
    heap: BinaryHeap<Ranked<T>>,
    limit: usize,
    matches: usize,
}

impl<T> BoundedRank<T> {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            limit,
            matches: 0,
        }
    }

    pub(crate) fn push(&mut self, key: RankKey, value: T) {
        let candidate = Ranked {
            key,
            ordinal: self.matches,
            value,
        };
        self.matches = self.matches.saturating_add(1);
        if self.limit == 0 {
            return;
        }
        if self.heap.len() < self.limit {
            self.heap.push(candidate);
        } else if self.heap.peek().is_some_and(|worst| candidate < *worst) {
            *self.heap.peek_mut().expect("nonempty bounded rank") = candidate;
        }
    }

    pub(crate) fn finish(self, recompute: bool) -> (Vec<T>, bool) {
        let incomplete = recompute || self.matches > self.limit;
        let winners = self
            .heap
            .into_sorted_vec()
            .into_iter()
            .map(|candidate| candidate.value)
            .collect();
        (winners, incomplete)
    }
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

    pub(crate) fn rank_key(&self, label: &str, search: &str, priority: u8) -> Option<RankKey> {
        let score = self.score_text(search)?;
        let lowercase = label.to_lowercase();
        Some((
            lowercase != self.query,
            priority,
            score,
            lowercase,
            label.to_owned(),
        ))
    }

    pub(crate) fn score_text(&self, text: &str) -> Option<usize> {
        if self.tokens.is_empty() {
            return Some(0);
        }
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
    let mut ranked = BoundedRank::new(limit);
    for item in items {
        let priority = relevance.get(&item.label).copied().unwrap_or(1);
        if let Some(key) = matcher.rank_key(
            &item.label,
            item.filter_text.as_deref().unwrap_or(&item.label),
            priority,
        ) {
            ranked.push(key, item);
        }
    }
    let (items, incomplete) = ranked.finish(recompute);
    let items = items
        .into_iter()
        .enumerate()
        .map(|(rank, mut item)| {
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

/// Original exhaustive stable-sort implementation used as a response oracle.
#[cfg(test)]
pub(crate) fn rank_exhaustive(
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
            let lowercase = item.label.to_lowercase();
            let score = matcher.score_item(&item)?;
            let priority = relevance.get(&item.label).copied().unwrap_or(1);
            Some((
                (
                    lowercase != matcher.query,
                    priority,
                    score,
                    lowercase,
                    item.label.clone(),
                ),
                item,
            ))
        })
        .collect();
    ranked.sort_by(|(a, _), (b, _)| a.cmp(b));
    let is_incomplete = recompute || ranked.len() > limit;
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
        is_incomplete,
        items,
        item_defaults: None,
        apply_kind: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_selection_matches_exhaustive_order_and_equal_key_stability() {
        let mut items: Vec<_> = (0..4096)
            .map(|i| CompletionItem {
                label: format!("key{:04}", (i * 4051) % 4096),
                filter_text: Some(format!("José García deterministic systems {i}")),
                detail: Some(i.to_string()),
                ..Default::default()
            })
            .collect();
        for i in 0..4 {
            items.push(CompletionItem {
                label: "key0042".into(),
                filter_text: Some("José García deterministic systems".into()),
                detail: Some(format!("duplicate {i}")),
                ..Default::default()
            });
        }
        let relevance = HashMap::from([("key4095".into(), 0), ("key0000".into(), 2)]);
        for query in [
            "",
            " \t",
            "key0042",
            "JOSÉ gar",
            "det sys",
            "k42",
            "missing",
        ] {
            for cap in [0, 1, 100, items.len(), items.len() + 1] {
                for recompute in [false, true] {
                    assert_eq!(
                        rank(items.clone(), query, &relevance, cap, recompute),
                        rank_exhaustive(items.clone(), query, &relevance, cap, recompute),
                        "query={query:?} cap={cap} recompute={recompute}"
                    );
                }
            }
        }
    }

    #[test]
    fn selector_never_retains_more_than_its_cap() {
        let matcher = MatchQuery::new("");
        for cap in [0, 1, 100] {
            let mut ranked = BoundedRank::new(cap);
            for i in (0..10_000).rev() {
                let label = format!("key{i:05}");
                ranked.push(matcher.rank_key(&label, &label, 1).unwrap(), label);
                assert!(ranked.heap.len() <= cap);
            }
            let (items, incomplete) = ranked.finish(false);
            assert!(incomplete);
            assert_eq!(items.len(), cap);
            assert_eq!(
                items,
                (0..cap).map(|i| format!("key{i:05}")).collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn empty_and_whitespace_queries_keep_zero_scores() {
        for query in ["", " ", "\t\n", "\u{2003}"] {
            assert_eq!(
                MatchQuery::new(query).score_text("LONG Ünicode heading"),
                Some(0)
            );
            let items = vec![
                CompletionItem {
                    label: "b".into(),
                    ..Default::default()
                },
                CompletionItem {
                    label: "a".into(),
                    ..Default::default()
                },
            ];
            let list = rank(items, query, &HashMap::new(), 100, false);
            assert_eq!(
                list.items
                    .iter()
                    .map(|item| item.label.as_str())
                    .collect::<Vec<_>>(),
                vec!["a", "b"]
            );
        }
    }
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
