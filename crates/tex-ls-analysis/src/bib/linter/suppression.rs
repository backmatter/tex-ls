//! Comment-based suppression for `.bib`, carried inside `@comment{…}` entries.
//!
//! A `%` line comment only exists *inside* an entry in BibTeX — free text between
//! entries lexes as `JUNK`, with no comment token to hang a directive on — so
//! unlike the LaTeX side the carrier is a structured `@comment` entry, a
//! [`COMMENT_ENTRY`] node:
//!
//! ```text
//! @comment{tex-ls-lint skip <rule>: <reason>}       the next entry, one rule
//! @comment{tex-ls-lint skip: <reason>}              the next entry, every rule
//! @comment{tex-ls-lint off <rule>} … {… on <rule>}  a region of entries
//! @comment{tex-ls-lint skip-file <rule>: <reason>}  the whole file
//! ```
//!
//! The **grammar** is [`crate::directives::parse_directive`], shared with the
//! LaTeX side and the formatter, so the two carriers cannot drift; only the
//! carrier and the next-sibling attachment differ. The retired
//! `@comment{tex-ls-ignore …}` and `@comment{tex-ls-ignore-file …}` spellings
//! still resolve through the same path and are no longer documented.
//!
//! **Only the lint axis acts here.** `tex-ls-format` is accepted by the shared
//! grammar but does nothing in a `.bib`: the bib formatter is a canonical
//! re-emitter rather than a trivia-only pass, so "reproduce this span byte for
//! byte" is a different mechanism there, not a matter of routing these ranges
//! through. Such a directive is retained as [`DirectiveOutcome::Unsupported`]
//! for the `inert-suppression` rule.
//!
//! [`COMMENT_ENTRY`]: crate::bib::syntax::SyntaxKind::COMMENT_ENTRY

use std::collections::HashMap;

use rowan::NodeOrToken;

use crate::bib::syntax::{SyntaxKind, SyntaxNode};
use crate::directives::{DirectiveOutcome, Verb, parse_directive};

// Both carriers retain the same parsed directive, source ranges and outcome.
// Carrier acquisition and attachment remain specific to this suppression map.
pub(crate) use crate::directives::LocatedDirective;

#[derive(Debug, Clone, Default)]
pub struct BibSuppressionMap {
    /// Start-sorted containment witnesses for every-rule suppression. Ends are
    /// prefix maxima of recorded ranges, not merged-region boundaries.
    all_ranges: Vec<(usize, usize)>,
    /// `rule → containment witnesses` for rule-selective directives.
    rule_ranges: HashMap<String, Vec<(usize, usize)>>,
    directives: Vec<LocatedDirective>,
}

/// A region opened by an `off` and waiting for its `on`.
struct OpenRegion {
    rule: Option<String>,
    start: usize,
    directive_index: usize,
}

impl BibSuppressionMap {
    pub fn build(root: &SyntaxNode) -> Self {
        let mut map = Self::default();
        let mut open: HashMap<Option<String>, OpenRegion> = HashMap::new();
        let mut targets: Option<FollowingTargets> = None;

        // Comment entries are top-level siblings. Field values cannot contain
        // directives, so do not walk every value in a large bibliography.
        for node in root.children() {
            #[cfg(test)]
            record_constructor_visit();
            if node.kind() != SyntaxKind::COMMENT_ENTRY {
                continue;
            }
            let Some(body) = comment_directive_text(&node) else {
                continue;
            };
            let Some(directive) = parse_directive(body.trim()) else {
                continue;
            };
            let text = node.to_string();
            let family = if directive.deprecated {
                match directive.verb {
                    Verb::Skip => "tex-ls-ignore",
                    Verb::SkipFile => "tex-ls-ignore-file",
                    Verb::Off | Verb::On => unreachable!("retired directives have no regions"),
                }
            } else {
                match directive.axis {
                    crate::directives::Axis::Format => "tex-ls-format",
                    crate::directives::Axis::Lint => "tex-ls-lint",
                    crate::directives::Axis::Both => "tex-ls",
                }
            };
            let relative = text
                .find(family)
                .expect("parsed directive contains its family name");
            let start = usize::from(node.text_range().start()) + relative;
            let directive_index = map.directives.len();
            map.directives.push(LocatedDirective {
                directive: directive.clone(),
                range: node.text_range(),
                family_range: rowan::TextRange::new(
                    rowan::TextSize::from(start as u32),
                    rowan::TextSize::from((start + family.len()) as u32),
                ),
                outcome: if directive.axis.covers_lint() {
                    DirectiveOutcome::Honored
                } else {
                    DirectiveOutcome::Unsupported
                },
            });
            if !directive.axis.covers_lint() {
                continue;
            }
            match directive.verb {
                Verb::SkipFile => {
                    map.record(span(root.text_range()), &directive.rule);
                }
                Verb::Skip => {
                    if let Some(target) = targets.get_or_insert_with(|| following_targets(root))
                        [&node.text_range().start()]
                    {
                        map.record(target, &directive.rule);
                    } else {
                        map.directives[directive_index].outcome = DirectiveOutcome::DanglingSkip;
                    }
                }
                // Unlike the LaTeX side there is no forward comment binding to
                // work around here — an `@comment` entry is a sibling, never
                // reparented into the entry below it — so the region simply
                // opens at the next meaningful sibling.
                Verb::Off => {
                    #[cfg(test)]
                    record_constructor_visit();
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        open.entry(directive.rule.clone())
                    {
                        map.directives[directive_index].outcome = DirectiveOutcome::UnclosedOff;
                        let start = targets.get_or_insert_with(|| following_targets(root))
                            [&node.text_range().start()]
                            .map(|(s, _)| s)
                            .unwrap_or_else(|| usize::from(node.text_range().end()));
                        entry.insert(OpenRegion {
                            rule: directive.rule.clone(),
                            start,
                            directive_index,
                        });
                    }
                }
                Verb::On => {
                    #[cfg(test)]
                    record_constructor_visit();
                    if let Some(region) = open.remove(&directive.rule) {
                        map.directives[region.directive_index].outcome = DirectiveOutcome::Honored;
                        let end = usize::from(node.text_range().start());
                        map.record((region.start, end), &region.rule);
                    } else {
                        map.directives[directive_index].outcome = DirectiveOutcome::UnmatchedOn;
                    }
                }
            }
        }

        // Unclosed regions run to end of file.
        let eof = usize::from(root.text_range().end());
        for region in open.into_values() {
            #[cfg(test)]
            record_constructor_visit();
            map.record((region.start, eof), &region.rule);
        }
        map.prepare_indexes();
        map
    }

    fn prepare_indexes(&mut self) {
        prepare_bib_ranges(&mut self.all_ranges);
        for ranges in self.rule_ranges.values_mut() {
            prepare_bib_ranges(ranges);
        }
    }

    fn record(&mut self, range: (usize, usize), rule: &Option<String>) {
        match rule {
            Some(rule) => self
                .rule_ranges
                .entry(rule.clone())
                .or_default()
                .push(range),
            None => self.all_ranges.push(range),
        }
    }

    /// Whether a `[start, end)` diagnostic for `rule` is suppressed.
    pub fn is_suppressed(&self, rule: &str, start: usize, end: usize) -> bool {
        bib_ranges_cover(&self.all_ranges, start, end)
            || self
                .rule_ranges
                .get(rule)
                .is_some_and(|ranges| bib_ranges_cover(ranges, start, end))
    }

    pub(crate) fn directives(&self) -> &[LocatedDirective] {
        &self.directives
    }
}

/// Index containment witnesses without merging overlapping regions. Each end
/// is the maximum original end at this start or an earlier start. Consequently
/// every successful query is witnessed by one original containing region,
/// including queries with reversed endpoints.
fn prepare_bib_ranges(ranges: &mut [(usize, usize)]) {
    ranges.sort_unstable_by_key(|&(start, _)| start);
    let mut maximum = 0;
    for (_, end) in ranges {
        #[cfg(test)]
        record_constructor_visit();
        maximum = maximum.max(*end);
        *end = maximum;
    }
}

fn bib_ranges_cover(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    #[cfg(test)]
    BIB_SUPPRESSION_WORK.with(|work| {
        let (visits, queries, probes) = work.get();
        work.set((visits, queries + 1, probes));
    });
    let after = ranges.partition_point(|&(left, _)| {
        #[cfg(test)]
        BIB_SUPPRESSION_WORK.with(|work| {
            let (visits, queries, probes) = work.get();
            work.set((visits, queries, probes + 1));
        });
        left <= start
    });
    after.checked_sub(1).is_some_and(|index| {
        #[cfg(test)]
        BIB_SUPPRESSION_WORK.with(|work| {
            let (visits, queries, probes) = work.get();
            work.set((visits, queries, probes + 1));
        });
        end <= ranges[index].1
    })
}

#[cfg(test)]
thread_local! {
    static BIB_SUPPRESSION_WORK: std::cell::Cell<(usize, usize, usize)> = const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg(test)]
fn record_constructor_visit() {
    BIB_SUPPRESSION_WORK.with(|work| {
        let (visits, queries, probes) = work.get();
        work.set((visits + 1, queries, probes));
    });
}

/// The inner text of a `@comment{…}` / `@comment(…)` entry — everything between
/// the opening and closing delimiter. Returns `None` if no delimiter pair is
/// found. Used only to read a directive, so nested braces (which never occur in a
/// directive line) need no special handling.
fn comment_directive_text(node: &SyntaxNode) -> Option<String> {
    let text = node.to_string();
    let open = text.find(['{', '('])?;
    let close = text.rfind(['}', ')'])?;
    if close <= open {
        return None;
    }
    Some(text[open + 1..close].to_string())
}

/// The byte range of the next non-trivia block after the directive entry,
/// skipping whitespace/newlines and further `@comment` entries (so two stacked
/// directives both attach to the entry that follows them).
type FollowingTargets = HashMap<rowan::TextSize, Option<(usize, usize)>>;

fn following_targets(root: &SyntaxNode) -> FollowingTargets {
    let elements: Vec<_> = root.children_with_tokens().collect();
    let mut targets = HashMap::new();
    let mut following = None;
    for element in elements.into_iter().rev() {
        #[cfg(test)]
        record_constructor_visit();
        match &element {
            NodeOrToken::Token(t)
                if matches!(t.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) => {}
            NodeOrToken::Node(n) if n.kind() == SyntaxKind::COMMENT_ENTRY => {
                targets.insert(n.text_range().start(), following);
            }
            _ => following = Some(span(element.text_range())),
        }
    }
    targets
}

/// A rowan `TextRange` as the plain `(start, end)` offsets the map stores.
fn span(range: rowan::TextRange) -> (usize, usize) {
    (usize::from(range.start()), usize::from(range.end()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bib::parse;
    use crate::directives::DirectiveOutcome;

    fn map_of(src: &str) -> BibSuppressionMap {
        BibSuppressionMap::build(&parse(src).syntax())
    }

    #[test]
    fn classifies_inert_and_incomplete_directives() {
        for (src, expected) in [
            (
                "@comment{tex-ls-lint skip missing-required-field}\n",
                DirectiveOutcome::DanglingSkip,
            ),
            (
                "@comment{tex-ls-lint on missing-required-field}\n",
                DirectiveOutcome::UnmatchedOn,
            ),
            (
                "@comment{tex-ls-lint off missing-required-field}\n@book{a}\n",
                DirectiveOutcome::UnclosedOff,
            ),
            (
                "@comment{tex-ls-format skip-file}\n@book{a}\n",
                DirectiveOutcome::Unsupported,
            ),
        ] {
            let map = map_of(src);
            assert_eq!(map.directives().len(), 1, "{src:?}");
            assert_eq!(map.directives()[0].outcome, expected, "{src:?}");
        }
    }

    #[test]
    fn retains_the_full_bib_directive_range() {
        let src = "@comment{tex-ls-format skip-file}\n";
        let map = map_of(src);
        let [located] = map.directives() else {
            panic!("expected one retained directive: {:?}", map.directives());
        };
        assert_eq!(
            &src[usize::from(located.range.start())..usize::from(located.range.end())],
            "@comment{tex-ls-format skip-file}"
        );
    }

    #[test]
    fn skip_file_without_a_rule_suppresses_everything() {
        let m = map_of("@comment{tex-ls-lint skip-file: noisy}\n@book{a, title={T}}\n");
        assert!(m.is_suppressed("anything", 0, 1));
    }

    #[test]
    fn skip_file_with_a_rule_suppresses_only_that_rule() {
        let m = map_of("@comment{tex-ls-lint skip-file missing-required-field: gone}\n");
        assert!(m.is_suppressed("missing-required-field", 0, 1));
        assert!(!m.is_suppressed("duplicate-key", 0, 1));
    }

    #[test]
    fn skip_targets_the_following_entry() {
        let src = "@comment{tex-ls-lint skip missing-required-field: gone}\n@book{a, title={T}}\n";
        let m = map_of(src);
        let at = src.find("@book").expect("has the entry");
        assert!(m.is_suppressed("missing-required-field", at, at + 5));
    }

    #[test]
    fn skip_does_not_leak_to_later_entries() {
        let src = "@comment{tex-ls-lint skip missing-required-field: gone}\n\
                   @book{a, title={T}}\n@book{b, title={U}}\n";
        let m = map_of(src);
        let later = src.rfind("@book").expect("has a second entry");
        assert!(!m.is_suppressed("missing-required-field", later, later + 5));
    }

    /// Region scope, which the retired family never had on either side.
    #[test]
    fn region_covers_the_entries_between_its_markers() {
        let src = "@comment{tex-ls-lint off missing-required-field}\n\
                   @book{a, title={T}}\n\
                   @comment{tex-ls-lint on missing-required-field}\n\
                   @book{b, title={U}}\n";
        let m = map_of(src);
        let inside = src.find("@book{a").expect("has a");
        assert!(m.is_suppressed("missing-required-field", inside, inside + 5));
        let outside = src.find("@book{b").expect("has b");
        assert!(!m.is_suppressed("missing-required-field", outside, outside + 5));
    }

    /// The retired spellings keep working — deprecated in the docs, not in
    /// behavior.
    #[test]
    fn retired_ignore_family_still_suppresses() {
        let src = "@comment{tex-ls-ignore missing-required-field: gone}\n@book{a, title={T}}\n";
        let m = map_of(src);
        let at = src.find("@book").expect("has the entry");
        assert!(m.is_suppressed("missing-required-field", at, at + 5));

        let file = map_of("@comment{tex-ls-ignore-file: noisy}\n@book{a, title={T}}\n");
        assert!(file.is_suppressed("anything", 0, 1));
    }

    /// `tex-ls-format` parses but has no bib semantics; it must not silence a
    /// diagnostic on its way through.
    #[test]
    fn format_family_does_not_suppress_lint() {
        let m = map_of("@comment{tex-ls-format skip-file}\n@book{a, title={T}}\n");
        assert!(!m.is_suppressed("missing-required-field", 0, 1));
    }

    #[test]
    fn non_directive_comment_is_inert() {
        let m = map_of("@comment{just a note}\n@book{a, title={T}}\n");
        assert!(!m.is_suppressed("missing-required-field", 0, 1));
    }

    #[test]
    fn bib_lookup_matches_raw_unsorted_overlap_and_endpoint_answers() {
        let candidates = [(8, 12), (0, 19), (3, 6), (4, 4), (12, 16)];
        for mask in 0..32 {
            let ranges: Vec<_> = candidates
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, range)| *range)
                .collect();
            for every_rule in [false, true] {
                let mut map = if every_rule {
                    BibSuppressionMap {
                        all_ranges: ranges.clone(),
                        ..Default::default()
                    }
                } else {
                    BibSuppressionMap {
                        rule_ranges: HashMap::from([("selected".into(), ranges.clone())]),
                        ..Default::default()
                    }
                };
                map.prepare_indexes();
                for start in 0..=21 {
                    for end in 0..=21 {
                        let expected = ranges
                            .iter()
                            .any(|&(left, right)| left <= start && end <= right);
                        assert_eq!(
                            map.is_suppressed("selected", start, end),
                            expected,
                            "{mask}, {start}..{end}"
                        );
                        assert_eq!(
                            map.is_suppressed("other", start, end),
                            every_rule && expected
                        );
                    }
                }
            }
        }
        for ranges in [
            vec![],
            vec![(0, 0)],
            vec![(4, 8), (0, 4), (4, 4)],
            vec![(10, 20), (0, usize::MAX), (5, 6)],
        ] {
            let mut indexed = ranges.clone();
            prepare_bib_ranges(&mut indexed);
            for (start, end) in [
                (0, 0),
                (4, 4),
                (0, 8),
                (7, 2),
                (usize::MAX, 0),
                (0, usize::MAX),
                (usize::MAX, usize::MAX),
            ] {
                assert_eq!(
                    bib_ranges_cover(&indexed, start, end),
                    ranges
                        .iter()
                        .any(|&(left, right)| left <= start && end <= right)
                );
            }
        }
    }

    #[test]
    fn nested_bib_regions_and_skip_file_keep_raw_containment_semantics() {
        let source = "@comment{tex-ls-lint off first}\n@misc{a}\n\
                      @comment{tex-ls-lint skip second}\n@misc{b}\n\
                      @comment{tex-ls-lint off second}\n@misc{c}\n\
                      @comment{tex-ls-lint skip-file first}\n@misc{d}\n\
                      @comment{tex-ls-lint on first}\n@misc{e}\n\
                      @comment{tex-ls-lint on second}\n@misc{f}\n";
        let parsed = parse(source);
        assert!(parsed.errors.is_empty());
        let map = BibSuppressionMap::build(&parsed.syntax());
        let first = [
            (0, source.len()),
            (
                source.find("@misc{a}").unwrap(),
                source.find("@comment{tex-ls-lint on first}").unwrap(),
            ),
        ];
        let b = source.find("@misc{b}").unwrap();
        let second = [
            (b, b + "@misc{b}".len()),
            (
                source.find("@misc{c}").unwrap(),
                source.find("@comment{tex-ls-lint on second}").unwrap(),
            ),
        ];
        for start in (0..=source.len()).step_by(7) {
            for end in (0..=source.len()).step_by(11) {
                assert_eq!(
                    map.is_suppressed("first", start, end),
                    first
                        .iter()
                        .any(|&(left, right)| left <= start && end <= right)
                );
                assert_eq!(
                    map.is_suppressed("second", start, end),
                    second
                        .iter()
                        .any(|&(left, right)| left <= start && end <= right)
                );
                assert!(!map.is_suppressed("other", start, end));
            }
        }
    }

    #[test]
    fn stacked_bib_directives_keep_junk_and_entry_targets() {
        let source = "@comment{tex-ls-lint skip first}\n\
                      @comment{tex-ls-lint skip second}\n\
                      @comment{ordinary documentary text}\n\
                      free text\n@misc{after}\n";
        let parsed = parse(source);
        assert!(parsed.errors.is_empty());
        let root = parsed.syntax();
        let junk = root
            .children_with_tokens()
            .find(|element| element.kind() == SyntaxKind::JUNK)
            .expect("free text is an actual top-level JUNK target");
        assert!(junk.text_range().contains(rowan::TextSize::from(
            source.find("free text").unwrap() as u32
        )));
        let map = BibSuppressionMap::build(&root);
        let (start, end) = span(junk.text_range());
        for rule in ["first", "second"] {
            assert!(map.is_suppressed(rule, start, end));
            let after = source.find("@misc{after}").unwrap();
            assert!(!map.is_suppressed(rule, after, after + "@misc{after}".len()));
        }
        assert!(
            map.directives
                .iter()
                .all(|directive| directive.outcome == DirectiveOutcome::Honored)
        );

        let source = "@comment{tex-ls-lint off first}\n\
                      @comment{tex-ls-lint off first}\n\
                      @comment{tex-ls-lint off second}\n\
                      @comment{ordinary text}\n@misc{inside}\n\
                      @comment{tex-ls-lint on first}\n@misc{later}\n\
                      @comment{tex-ls-lint on second}\n";
        let parsed = parse(source);
        assert!(parsed.errors.is_empty());
        let map = BibSuppressionMap::build(&parsed.syntax());
        let inside = source.find("@misc{inside}").unwrap();
        let later = source.find("@misc{later}").unwrap();
        assert!(map.is_suppressed("first", inside, inside + "@misc{inside}".len()));
        assert!(map.is_suppressed("second", inside, inside + "@misc{inside}".len()));
        assert!(!map.is_suppressed("first", later, later + "@misc{later}".len()));
        assert!(map.is_suppressed("second", later, later + "@misc{later}".len()));
        assert!(
            map.directives
                .iter()
                .all(|directive| directive.outcome == DirectiveOutcome::Honored)
        );
        assert_eq!(
            map.rule_ranges["first"].len(),
            1,
            "duplicate off keeps its original region"
        );
        assert_eq!(map.rule_ranges["second"].len(), 1);
    }

    fn actual_bib_source(count: usize, every_rule: bool, skip: bool, fixed: bool) -> String {
        let qualifier = if every_rule {
            ""
        } else {
            " missing-required-field"
        };
        let mut source = String::new();
        for index in 0..count {
            source.push_str(&format!(
                "@comment{{tex-ls-lint {}{qualifier}}}\n",
                if skip { "skip" } else { "off" }
            ));
            let mut entry = |key: String, duplicate: bool| {
                source.push_str(&format!(
                    "@article{{{key}, author={{A}}, title={{T}}, year=2020, note={{x}}{}}}\n",
                    if duplicate { ", note={x}" } else { "" }
                ));
            };
            entry(format!("hidden{index}"), !fixed || every_rule);
            if !skip {
                source.push_str(&format!("@comment{{tex-ls-lint on{qualifier}}}\n"));
            }
            source.push_str(&format!(
                "@article{{live{index}, author={{A}}, title={{T}}, year=2020, note={{x}}{}}}\n",
                if fixed { "" } else { ", note={x}" }
            ));
        }
        source
    }

    fn assert_actual_bib_lint_work(every_rule: bool) {
        use crate::bib::linter::{
            check::lint_document_with_rules,
            rules::{BibRule, DuplicateField, MissingRequiredField},
        };
        use crate::bib::semantic::Model;
        use crate::linter::{Applicability, apply_fixes};
        use std::path::Path;
        const COUNT: usize = 1024;
        let source = actual_bib_source(COUNT, every_rule, false, false);
        let parsed = parse(&source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let root = parsed.syntax();
        assert_eq!(root.to_string(), source);
        let map = BibSuppressionMap::build(&root);
        assert_eq!(
            if every_rule {
                map.all_ranges.len()
            } else {
                map.rule_ranges["missing-required-field"].len()
            },
            COUNT
        );
        let model = Model::build(&root);
        let rules: Vec<Box<dyn BibRule>> =
            vec![Box::new(MissingRequiredField), Box::new(DuplicateField)];
        BIB_SUPPRESSION_WORK.with(|work| work.set((0, 0, 0)));
        let findings =
            lint_document_with_rules(Path::new("suppression.bib"), &root, &model, None, &rules);
        let (_, queries, probes) = BIB_SUPPRESSION_WORK.with(std::cell::Cell::get);
        let missing: Vec<_> = findings
            .iter()
            .filter(|finding| finding.rule == "missing-required-field")
            .collect();
        assert_eq!(missing.len(), COUNT);
        let expected: Vec<_> = (0..COUNT)
            .map(|index| {
                let key = format!("live{index}");
                let at = source.find(&format!("@article{{{key},")).unwrap() + "@article{".len();
                (at, at + key.len())
            })
            .collect();
        assert_eq!(
            missing
                .iter()
                .map(|finding| (finding.start, finding.end))
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            missing
                .iter()
                .all(|finding| finding.fix.is_none() && finding.message.contains("journaltitle"))
        );
        let duplicates: Vec<_> = findings
            .iter()
            .filter(|finding| finding.rule == "duplicate-field")
            .collect();
        assert_eq!(duplicates.len(), if every_rule { COUNT } else { COUNT * 2 });
        assert!(duplicates.iter().all(|finding| {
            finding
                .fix
                .as_ref()
                .is_some_and(|fix| fix.applicability == Applicability::Safe)
        }));
        assert_eq!(findings.len(), missing.len() + duplicates.len());
        let fixes: Vec<_> = duplicates
            .iter()
            .filter_map(|finding| finding.fix.clone())
            .collect();
        let safe = apply_fixes(&source, &fixes, false);
        assert_eq!(
            safe.output,
            actual_bib_source(COUNT, every_rule, false, true),
            "safe deletion is correct without formatting"
        );
        assert_eq!(safe.applied, duplicates.len());
        assert_eq!(safe.skipped_conflicts, 0);
        let fixed = parse(&safe.output);
        assert!(fixed.errors.is_empty());
        let fixed_model = Model::build(&fixed.syntax());
        assert_eq!(
            fixed_model.entries().len(),
            model.entries().len(),
            "safe fixes retain entries"
        );
        assert!(queries >= COUNT * 2);
        let bound = COUNT.ilog2() as usize + 2;
        assert!(
            probes <= queries * bound,
            "Bib containment exceeds logarithmic work: {queries} queries, {probes} probes, bound {}",
            queries * bound
        );
    }

    #[test]
    fn actual_bib_all_rule_suppression_lookup_has_bounded_work() {
        assert_actual_bib_lint_work(true);
    }

    #[test]
    fn actual_bib_selected_rule_suppression_lookup_has_bounded_work() {
        assert_actual_bib_lint_work(false);
    }

    #[test]
    fn bib_off_and_skip_target_construction_visits_are_linear() {
        const COUNT: usize = 1024;
        for skip in [false, true] {
            let source = actual_bib_source(COUNT, true, skip, false);
            let parsed = parse(&source);
            assert!(parsed.errors.is_empty());
            let root = parsed.syntax();
            let elements = root.children_with_tokens().count();
            BIB_SUPPRESSION_WORK.with(|work| work.set((0, 0, 0)));
            let map = BibSuppressionMap::build(&root);
            let (visits, _, _) = BIB_SUPPRESSION_WORK.with(std::cell::Cell::get);
            assert_eq!(map.all_ranges.len(), COUNT);
            assert!(
                map.directives
                    .iter()
                    .all(|directive| directive.outcome == DirectiveOutcome::Honored)
            );
            for index in 0..COUNT {
                let hidden = source.find(&format!("@article{{hidden{index},")).unwrap();
                let live = source.find(&format!("@article{{live{index},")).unwrap();
                assert!(map.is_suppressed("anything", hidden, hidden + 8));
                assert!(!map.is_suppressed("anything", live, live + 8));
            }
            assert!(
                visits <= elements * 4,
                "Bib target construction rescans siblings: skip={skip}, {visits} visits for {elements} elements"
            );
        }
    }

    #[test]
    fn distinct_bib_open_rules_have_bounded_constructor_work() {
        const COUNT: usize = 1024;
        let source: String = (0..COUNT)
            .map(|index| {
                format!("@comment{{tex-ls-lint off custom-{index}}}\n@misc{{key{index}}}\n")
            })
            .collect();
        let parsed = parse(&source);
        assert!(parsed.errors.is_empty());
        let root = parsed.syntax();
        let elements = root.children_with_tokens().count();
        BIB_SUPPRESSION_WORK.with(|work| work.set((0, 0, 0)));
        let map = BibSuppressionMap::build(&root);
        let (visits, _, _) = BIB_SUPPRESSION_WORK.with(std::cell::Cell::get);
        assert_eq!(map.rule_ranges.len(), COUNT);
        assert!(
            map.directives
                .iter()
                .all(|directive| directive.outcome == DirectiveOutcome::UnclosedOff)
        );
        for index in 0..COUNT {
            let start = source.find(&format!("@misc{{key{index}}}")).unwrap();
            let ranges = &map.rule_ranges[&format!("custom-{index}")];
            assert_eq!(ranges, &[(start, source.len())]);
        }
        assert!(
            visits <= elements * 4,
            "Bib open-key construction exceeds linear visits: {visits} visits for {elements} elements"
        );
    }
}
