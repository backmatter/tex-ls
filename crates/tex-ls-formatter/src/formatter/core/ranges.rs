//! Document-level range selection and indexed source interval queries.
use rowan::TextRange;
use tex_ls_parser::ast::{AstNode, Environment};
use tex_ls_parser::syntax::{SyntaxElement, SyntaxKind, SyntaxNode, is_collapsible_trivia};

/// Whether two byte ranges overlap (share at least one byte). Half-open, so ranges
/// that merely touch at a boundary (`a.end == b.start`) do not overlap — used by
/// the range-formatting emission filter to keep a top-level block's leading/trailing
/// trivia (which abuts but does not overlap the block-aligned range) out of the
/// fragment.
pub(super) fn ranges_overlap(a: TextRange, b: TextRange) -> bool {
    a.start() < b.end() && b.start() < a.end()
}

/// The actual source elements emitted by range formatting. Tail normalization
/// uses this same selection because an intersecting block is emitted in full.
pub(super) fn range_elements(root: &SyntaxNode, range: TextRange) -> Vec<SyntaxElement> {
    let mut filtered = Vec::new();
    for element in root
        .children_with_tokens()
        .filter(|element| ranges_overlap(range, element.text_range()))
    {
        if let SyntaxElement::Node(child) = &element
            && document_body_contains(child, range)
        {
            filtered.extend(
                child
                    .children_with_tokens()
                    .filter(|element| ranges_overlap(range, element.text_range())),
            );
        } else {
            filtered.push(element);
        }
    }
    filtered
}

/// Find the final significant token of the actually emitted blocks. Start at
/// the indexed range end and walk backwards only until one is found, avoiding
/// unrelated suffix blocks and a second selection vector.
pub(super) fn range_tail_kind(root: &SyntaxNode, range: TextRange) -> Option<SyntaxKind> {
    if range.is_empty() || !ranges_overlap(range, root.text_range()) {
        return None;
    }
    let at = range.end().min(root.text_range().end());
    let token = tex_ls_parser::syntax::token_at_offset(root, at).left_biased()?;
    let mut current = SyntaxElement::Token(token);
    loop {
        let parent = match &current {
            SyntaxElement::Token(token) => token.parent()?,
            SyntaxElement::Node(node) => node.parent()?,
        };
        if parent == *root {
            break;
        }
        current = SyntaxElement::Node(parent);
    }
    let mut element = Some(current);
    while let Some(current) = element {
        if current.text_range().end() <= range.start() {
            break;
        }
        if ranges_overlap(range, current.text_range()) {
            let kind = match &current {
                SyntaxElement::Token(token) => {
                    (!is_collapsible_trivia(token.kind())).then_some(token.kind())
                }
                SyntaxElement::Node(node) if document_body_contains(node, range) => {
                    range_tail_kind(node, range)
                }
                SyntaxElement::Node(node) => {
                    let mut token = node.last_token();
                    let mut kind = None;
                    while let Some(current) = token {
                        if !node.text_range().contains_range(current.text_range()) {
                            break;
                        }
                        if !is_collapsible_trivia(current.kind()) {
                            kind = Some(current.kind());
                            break;
                        }
                        token = current.prev_token();
                    }
                    kind
                }
            };
            if kind.is_some() {
                return kind;
            }
        }
        element = current.prev_sibling_or_token();
    }
    None
}

/// Whether `range` lies wholly inside the body of the canonical `document`
/// environment. Its body is the formatter's canonical no-indent case, so exposing
/// direct body blocks does not discard any ancestor indentation context.
pub fn document_body_contains(node: &SyntaxNode, range: TextRange) -> bool {
    let Some(environment) = Environment::cast(node.clone()) else {
        return false;
    };
    if environment.name().as_deref() != Some("document") {
        return false;
    }
    let (Some(begin), Some(end)) = (environment.begin(), environment.end()) else {
        return false;
    };
    range.start() >= begin.syntax().text_range().end()
        && range.end() <= end.syntax().text_range().start()
}

/// Overlap over sorted, disjoint ranges, preserving the existing strict
/// endpoint predicate, including its answer for empty query ranges.
pub(super) fn sorted_ranges_overlap(ranges: &[TextRange], range: TextRange) -> bool {
    #[cfg(test)]
    record_range_lookup_query();
    let first = ranges.partition_point(|span| {
        #[cfg(test)]
        record_range_lookup_probe();
        span.end() <= range.start()
    });
    // Every earlier span ends before the query; every later one starts at or
    // beyond this candidate, so this one decides whether any overlap exists.
    ranges.get(first).is_some_and(|span| {
        #[cfg(test)]
        record_range_lookup_probe();
        span.start() < range.end()
    })
}

/// Containment over sorted, disjoint ranges. The last span starting at or
/// before the query decides whether any span contains it.
pub(super) fn sorted_ranges_contain(ranges: &[TextRange], range: TextRange) -> bool {
    #[cfg(test)]
    record_range_lookup_query();
    let after = ranges.partition_point(|span| {
        #[cfg(test)]
        record_range_lookup_probe();
        span.start() <= range.start()
    });
    after
        .checked_sub(1)
        .and_then(|index| ranges.get(index))
        .is_some_and(|span| {
            #[cfg(test)]
            record_range_lookup_probe();
            span.contains_range(range)
        })
}

#[cfg(test)]
thread_local! {
    static RANGE_LOOKUP_COUNTS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
fn record_range_lookup_query() {
    RANGE_LOOKUP_COUNTS.with(|counts| {
        let (queries, probes) = counts.get();
        counts.set((queries + 1, probes));
    });
}

#[cfg(test)]
fn record_range_lookup_probe() {
    RANGE_LOOKUP_COUNTS.with(|counts| {
        let (queries, probes) = counts.get();
        counts.set((queries, probes + 1));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formatter::core::{expl3_regions, format_node};
    use crate::formatter::style::FormatStyle;
    use tex_ls_parser::directives;
    use tex_ls_parser::parser::parse;

    fn range(start: u32, end: u32) -> TextRange {
        TextRange::new(start.into(), end.into())
    }

    #[test]
    fn indexed_ranges_preserve_linear_boundary_answers() {
        // Exhaust all query endpoints over independently generated disjoint sets,
        // including empty queries, missing intervals and touching endpoints.
        for mask in 0..256 {
            let spans: Vec<_> = (0..8)
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| range(i * 3, i * 3 + 1 + (i % 2)))
                .collect();
            for start in 0..=25 {
                for end in start..=25 {
                    let query = range(start, end);
                    assert_eq!(
                        sorted_ranges_overlap(&spans, query),
                        spans
                            .iter()
                            .any(|span| span.start() < query.end() && query.start() < span.end()),
                        "overlap mask={mask}, query={query:?}"
                    );
                    assert_eq!(
                        sorted_ranges_contain(&spans, query),
                        spans.iter().any(|span| span.contains_range(query)),
                        "containment mask={mask}, query={query:?}"
                    );
                }
            }
        }
        for spans in [
            vec![],
            vec![range(0, 0)],
            vec![range(0, 4), range(4, 8)],
            vec![range(0, 4), range(4, 4), range(4, 8)],
            vec![range(1, u32::MAX)],
        ] {
            for query in [
                range(0, 0),
                range(0, 4),
                range(4, 4),
                range(4, 8),
                range(8, 8),
                range(1, u32::MAX),
                range(u32::MAX, u32::MAX),
            ] {
                assert_eq!(
                    sorted_ranges_overlap(&spans, query),
                    spans
                        .iter()
                        .any(|span| span.start() < query.end() && query.start() < span.end())
                );
                assert_eq!(
                    sorted_ranges_contain(&spans, query),
                    spans.iter().any(|span| span.contains_range(query))
                );
            }
        }
    }

    fn assert_lowering_work_is_bounded(block: &str, expl3: bool) {
        const COUNT: usize = 1024;
        let source = block.repeat(COUNT);
        let parsed = parse(&source);
        assert!(
            parsed.errors.is_empty(),
            "clean actual input: {:?}",
            parsed.errors
        );
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let regions = expl3_regions(&root);
        let suppressions = directives::Suppressions::build(&root);
        assert_eq!(regions.len(), if expl3 { COUNT } else { 0 });
        assert_eq!(
            suppressions.format_ranges().len(),
            if expl3 { 0 } else { COUNT }
        );
        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::PARAGRAPH)
                .count()
                >= COUNT
        );
        RANGE_LOOKUP_COUNTS.with(|counts| counts.set((0, 0)));
        let formatted = format_node(&root, FormatStyle::default()).expect("supported actual input");
        let (queries, probes) = RANGE_LOOKUP_COUNTS.with(std::cell::Cell::get);
        assert!(
            queries >= COUNT,
            "fixture must exercise actual lowering queries"
        );
        let after = parse(&formatted);
        assert!(after.errors.is_empty());
        assert_eq!(
            format_node(&after.syntax(), FormatStyle::default()).unwrap(),
            formatted,
            "idempotence"
        );
        if !expl3 {
            assert_eq!(
                formatted.matches("\\textbf{keep   this}").count(),
                COUNT,
                "protected bytes"
            );
        }
        let max_probes = COUNT.ilog2() as usize + 2;
        assert!(
            probes <= queries * max_probes,
            "range lookup scans exceed logarithmic bound: {queries} queries, {probes} probes, maximum {}",
            queries * max_probes
        );
    }

    #[test]
    fn many_expl3_regions_do_not_rescan_for_each_paragraph() {
        assert_lowering_work_is_bounded(
            "\\ExplSyntaxOn\n\\tl_clear:N \\l_tmpa_tl\n\\ExplSyntaxOff\n\nordinary alpha beta.\n\n",
            true,
        );
    }

    #[test]
    fn many_suppression_spans_do_not_rescan_for_each_element() {
        assert_lowering_work_is_bounded(
            "% tex-ls-format off\n\\textbf{keep   this}\n% tex-ls-format on\n\nordinary alpha beta.\n\n",
            false,
        );
    }
}
