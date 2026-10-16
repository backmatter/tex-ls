//! Comment-based suppression, as the linter consumes it.
//!
//! The directive grammar itself lives in [`crate::directives`], shared with the
//! formatter — one grammar for all three families:
//!
//! ```text
//! % tex-ls-lint skip <rule>: <reason>       the next construct, one rule
//! % tex-ls-lint skip: <reason>              the next construct, every rule
//! % tex-ls-lint off <rule> … on <rule>      a region
//! % tex-ls-lint skip-file <rule>: <reason>  the whole file
//! % tex-ls skip / off / on / skip-file      the same, plus the formatter
//! ```
//!
//! Omitting `<rule>` means every rule. The retired `% tex-ls-ignore` and
//! `% tex-ls-ignore-file` spellings still resolve, through the same path, and
//! are no longer documented.
//!
//! This module is only the *lookup*: it flattens the resolved ranges into the
//! plain `usize` offsets [`Diagnostic`](super::Diagnostic) stores (a rowan
//! `TextRange` never reaches a diagnostic) and answers containment queries. A
//! finding is suppressed when its `[start, end)` falls fully inside a range
//! registered for its rule, or inside an every-rule range.

use std::collections::HashMap;

use rowan::TextRange;

use crate::syntax::SyntaxNode;

#[derive(Debug, Clone, Default)]
pub struct SuppressionMap {
    /// Byte ranges in which *every* rule is suppressed.
    all_ranges: Vec<(usize, usize)>,
    /// `rule → byte ranges` for the rule-selective directives.
    rule_ranges: HashMap<String, Vec<(usize, usize)>>,
}

impl SuppressionMap {
    pub fn build(root: &SyntaxNode) -> Self {
        let resolved = crate::directives::Suppressions::build(root);
        Self::from_suppressions(&resolved)
    }

    pub fn from_suppressions(resolved: &crate::directives::Suppressions) -> Self {
        Self {
            all_ranges: spans(resolved.lint_all_ranges()),
            rule_ranges: resolved
                .lint_rule_ranges()
                .iter()
                .map(|(rule, ranges)| (rule.clone(), spans(ranges)))
                .collect(),
        }
    }

    /// Whether a `[start, end)` diagnostic for `rule` is suppressed.
    pub fn is_suppressed(&self, rule: &str, start: usize, end: usize) -> bool {
        sorted_spans_cover(&self.all_ranges, start, end)
            || self
                .rule_ranges
                .get(rule)
                .is_some_and(|ranges| sorted_spans_cover(ranges, start, end))
    }
}

/// Logarithmic containment over sorted, merged directive resolver spans.
/// The diagnostic may be empty, reversed, or beyond source bounds: retain the
/// existing predicate's answer without indexing source text.
fn sorted_spans_cover(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    #[cfg(test)]
    SUPPRESSION_LOOKUP_WORK.with(|work| {
        let (queries, probes) = work.get();
        work.set((queries + 1, probes));
    });
    let after = ranges.partition_point(|(range_start, _)| {
        #[cfg(test)]
        SUPPRESSION_LOOKUP_WORK.with(|work| {
            let (queries, probes) = work.get();
            work.set((queries, probes + 1));
        });
        *range_start <= start
    });
    // Sorted, merged spans have increasing ends. The last eligible start also
    // has the greatest eligible end, including for reversed diagnostic ranges.
    after
        .checked_sub(1)
        .and_then(|index| ranges.get(index))
        .is_some_and(|(_, range_end)| {
            #[cfg(test)]
            SUPPRESSION_LOOKUP_WORK.with(|work| {
                let (queries, probes) = work.get();
                work.set((queries, probes + 1));
            });
            end <= *range_end
        })
}

#[cfg(test)]
thread_local! {
    static SUPPRESSION_LOOKUP_WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Resolved ranges as the plain `(start, end)` offsets the map stores.
fn spans(ranges: &[TextRange]) -> Vec<(usize, usize)> {
    ranges
        .iter()
        .map(|r| (usize::from(r.start()), usize::from(r.end())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn map_of(src: &str) -> SuppressionMap {
        SuppressionMap::build(&SyntaxNode::new_root(parse(src).green))
    }

    #[test]
    fn skip_file_without_a_rule_suppresses_everything() {
        let m = map_of("% tex-ls-lint skip-file: noisy\n\\bf\n");
        assert!(m.is_suppressed("anything", 0, 1));
    }

    #[test]
    fn skip_file_with_a_rule_suppresses_only_that_rule() {
        let m = map_of("% tex-ls-lint skip-file deprecated-command: legacy\n\\bf\n");
        assert!(m.is_suppressed("deprecated-command", 0, 1));
        assert!(!m.is_suppressed("duplicate-label", 0, 1));
    }

    #[test]
    fn skip_targets_the_next_construct_only() {
        let src = "% tex-ls-lint skip deprecated-command\n\\bf x\n\n\\bf y\n";
        let m = map_of(src);
        let first = src.find("\\bf x").expect("has first");
        assert!(m.is_suppressed("deprecated-command", first, first + 3));
        let second = src.find("\\bf y").expect("has second");
        assert!(!m.is_suppressed("deprecated-command", second, second + 3));
    }

    /// Region scope, which the retired family never had.
    #[test]
    fn lint_region_suppresses_one_rule_between_its_markers() {
        let src = "\\bf a\n% tex-ls-lint off deprecated-command\n\\bf b\n\
                   % tex-ls-lint on deprecated-command\n\\bf c\n";
        let m = map_of(src);
        let inside = src.find("\\bf b").expect("has b");
        assert!(m.is_suppressed("deprecated-command", inside, inside + 3));
        assert!(
            !m.is_suppressed("duplicate-label", inside, inside + 3),
            "a rule-selective region must not silence other rules"
        );
        let after = src.find("\\bf c").expect("has c");
        assert!(!m.is_suppressed("deprecated-command", after, after + 3));
    }

    /// The combined family covers every rule, and the formatter besides.
    #[test]
    fn combined_family_suppresses_every_rule_in_a_region() {
        let src = "\\bf\n% tex-ls off\n\\it\n% tex-ls on\n\\bf\n";
        let m = map_of(src);
        let inside = src.find("\\it").expect("has \\it");
        assert!(m.is_suppressed("any-rule", inside, inside + 3));
        let outside = src.rfind("\\bf").expect("has trailing \\bf");
        assert!(!m.is_suppressed("any-rule", outside, outside + 3));
    }

    /// …but the format-only family must leave the linter alone.
    #[test]
    fn format_family_does_not_suppress_lint() {
        let src = "% tex-ls-format off\n\\bf\n% tex-ls-format on\n";
        let m = map_of(src);
        let at = src.find("\\bf").expect("has \\bf");
        assert!(!m.is_suppressed("deprecated-command", at, at + 3));
    }

    /// The retired spellings keep working — deprecated in the docs, not in
    /// behavior. Deleting these tests is how the promise quietly breaks.
    #[test]
    fn retired_ignore_family_still_suppresses() {
        let node = map_of("% tex-ls-ignore deprecated-command: legacy\n\\bf\n");
        let at = "% tex-ls-ignore deprecated-command: legacy\n".len();
        assert!(node.is_suppressed("deprecated-command", at, at + 3));
        assert!(!node.is_suppressed("duplicate-label", at, at + 3));

        let file_rule = map_of("% tex-ls-ignore-file deprecated-command: legacy\n\\bf\n");
        assert!(file_rule.is_suppressed("deprecated-command", 0, 1));
        assert!(!file_rule.is_suppressed("duplicate-label", 0, 1));

        let file_all = map_of("% tex-ls-ignore-file: noisy\n\\bf\n");
        assert!(file_all.is_suppressed("anything", 0, 1));
    }

    #[test]
    fn non_directive_comment_is_inert() {
        let m = map_of("% just a note\n\\bf\n");
        assert!(!m.is_suppressed("deprecated-command", 0, 1));
    }

    #[test]
    fn suppression_lookup_preserves_all_linear_endpoint_answers() {
        for mask in 0u32..256 {
            let spans: Vec<_> = (0usize..8)
                .filter(|index| mask & (1 << index) != 0)
                .map(|index| (index * 3, index * 3 + 1 + index % 2))
                .collect();
            for start in 0..=25 {
                // Include reversed diagnostic ranges, whose previous answer is
                // observable through this public method even for malformed input.
                for end in 0..=25 {
                    let expected = spans
                        .iter()
                        .any(|&(left, right)| left <= start && end <= right);
                    for every_rule in [false, true] {
                        let map = if every_rule {
                            SuppressionMap {
                                all_ranges: spans.clone(),
                                rule_ranges: HashMap::new(),
                            }
                        } else {
                            SuppressionMap {
                                all_ranges: Vec::new(),
                                rule_ranges: HashMap::from([("selected".into(), spans.clone())]),
                            }
                        };
                        assert_eq!(
                            map.is_suppressed("selected", start, end),
                            expected,
                            "mask={mask}, {start}..{end}, every_rule={every_rule}"
                        );
                        assert_eq!(
                            map.is_suppressed("other", start, end),
                            every_rule && expected
                        );
                    }
                }
            }
        }
        for spans in [
            vec![],
            vec![(0, 0)],
            vec![(0, 4), (4, 8)],
            vec![(0, 4), (4, 4), (4, 8)],
            vec![(1, usize::MAX)],
        ] {
            for (start, end) in [
                (0, 0),
                (0, 4),
                (4, 4),
                (4, 8),
                (8, 8),
                (1, usize::MAX),
                (usize::MAX, usize::MAX),
                (usize::MAX, 0),
                (0, usize::MAX),
                (7, 2),
            ] {
                assert_eq!(
                    sorted_spans_cover(&spans, start, end),
                    spans
                        .iter()
                        .any(|&(left, right)| left <= start && end <= right)
                );
            }
        }
    }

    fn assert_actual_lint_lookup_work(every_rule: bool) {
        use crate::linter::{Applicability, apply_fixes, lint_document};
        use crate::semantic::SemanticModel;
        use std::path::Path;
        const COUNT: usize = 1024;
        let qualifier = if every_rule {
            ""
        } else {
            " deprecated-command"
        };
        let block = format!(
            "% tex-ls-lint off{qualifier}\n\\bf hidden ...\n% tex-ls-lint on{qualifier}\n\n\\bf live ...\n\n"
        );
        let source = format!(
            "\\documentclass{{article}}\n\\begin{{document}}\n{}\\end{{document}}\n",
            block.repeat(COUNT)
        );
        let parsed = parse(&source);
        assert!(
            parsed.errors.is_empty(),
            "supported real fixture: {:?}",
            parsed.errors
        );
        let root = SyntaxNode::new_root(parsed.green);
        assert_eq!(root.text().to_string(), source);
        let map = SuppressionMap::build(&root);
        if every_rule {
            assert_eq!(map.all_ranges.len(), COUNT);
            assert!(map.rule_ranges.is_empty());
        } else {
            assert!(map.all_ranges.is_empty());
            assert_eq!(map.rule_ranges["deprecated-command"].len(), COUNT);
        }
        let model = SemanticModel::build(&root);
        SUPPRESSION_LOOKUP_WORK.with(|work| work.set((0, 0)));
        let diagnostics = lint_document(
            Path::new("suppression.tex"),
            &root,
            &model,
            None,
            None,
            None,
        );
        let (queries, probes) = SUPPRESSION_LOOKUP_WORK.with(std::cell::Cell::get);
        let deprecated: Vec<_> = diagnostics
            .iter()
            .filter(|finding| finding.rule == "deprecated-command")
            .collect();
        let expected_offsets: Vec<_> = source
            .match_indices("\\bf live")
            .map(|(start, _)| (start, start + 3))
            .collect();
        assert_eq!(
            deprecated
                .iter()
                .map(|finding| (finding.start, finding.end))
                .collect::<Vec<_>>(),
            expected_offsets
        );
        assert_eq!(deprecated.len(), COUNT);
        assert!(deprecated.iter().all(|finding| {
            finding
                .fix
                .as_ref()
                .is_some_and(|fix| fix.applicability == Applicability::Unsafe)
        }));
        let ellipses: Vec<_> = diagnostics
            .iter()
            .filter(|finding| finding.rule == "ellipsis")
            .collect();
        assert_eq!(ellipses.len(), if every_rule { COUNT } else { 2 * COUNT });
        assert!(ellipses.iter().all(|finding| {
            finding
                .fix
                .as_ref()
                .is_some_and(|fix| fix.applicability == Applicability::Safe)
        }));
        let fixes: Vec<_> = deprecated
            .iter()
            .chain(&ellipses)
            .filter_map(|finding| finding.fix.clone())
            .collect();
        let safe = apply_fixes(&source, &fixes, false);
        let expected_block = if every_rule {
            block.replace("\\bf live ...", "\\bf live \\dots")
        } else {
            block.replace("...", "\\dots")
        };
        let expected = format!(
            "\\documentclass{{article}}\n\\begin{{document}}\n{}\\end{{document}}\n",
            expected_block.repeat(COUNT)
        );
        assert_eq!(safe.output, expected, "safe fixes need no later formatting");
        assert_eq!(safe.applied, ellipses.len());
        assert_eq!(safe.skipped_conflicts, 0);
        assert!(parse(&safe.output).errors.is_empty());
        let opted_in = apply_fixes(&source, &fixes, true);
        assert_eq!(
            opted_in.output,
            expected.replace("\\bf live", "\\bfseries live")
        );
        assert_eq!(opted_in.applied, ellipses.len() + COUNT);
        assert_eq!(opted_in.skipped_conflicts, 0);
        assert!(
            queries >= 2 * COUNT,
            "actual lint driver must query suppression"
        );
        let bound = COUNT.ilog2() as usize + 2;
        assert!(
            probes <= queries * bound,
            "suppression scans exceed logarithmic work: {queries} queries, {probes} probes, bound {}",
            queries * bound
        );
    }

    #[test]
    fn actual_lint_all_rule_suppression_lookup_has_bounded_work() {
        assert_actual_lint_lookup_work(true);
    }

    #[test]
    fn actual_lint_selected_rule_suppression_lookup_has_bounded_work() {
        assert_actual_lint_lookup_work(false);
    }
}
