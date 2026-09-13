//! The bib lint driver: run every bib rule over one `.bib` file, stamp paths, and
//! sort. The bib analog of [`crate::linter::check`], and the entry point the CLI
//! calls for `.bib` inputs.
//!
//! BibTeX suppression directives live in structured `@comment{...}` entries.
//! The shared driver shape is the same as LaTeX: build the map once, expose it
//! to whole-file rules, and filter ordinary findings after dispatch.

use std::path::Path;

use crate::bib::parse;
use crate::bib::semantic::{self, Model};
use crate::bib::syntax::{SyntaxKind, SyntaxNode};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::is_unsuppressible_suppression_meta_rule;

use super::rules::{BibRule, BibRuleContext, all_rules};
use super::suppression::BibSuppressionMap;

/// Parse and lint a single `.bib` file's `text` from scratch, returning its parse
/// diagnostics plus rule findings. The self-contained analog of
/// [`crate::linter::check::check_document`] for `.bib`: callers that hold only text
/// (notably the CLI) get everything in one call.
pub fn check_document(path: &Path, text: &str) -> Vec<Diagnostic> {
    let parsed = parse(text);
    let mut diagnostics: Vec<Diagnostic> = parsed
        .errors
        .iter()
        .map(|err| Diagnostic::from_parse(path.to_path_buf(), err))
        .collect();
    let root = parsed.syntax();
    let model = Model::build(&root);
    diagnostics.extend(lint_document(path, &root, &model));
    diagnostics
}

/// Run all built-in bib rules against `root`/`model`, returning the diagnostics
/// (`path` stamped, sorted by position).
///
/// `root` and `model` must describe the same file as `path`. Mirrors
/// [`crate::linter::check::lint_document`], minus the cross-file `resolution`
/// argument (no bib rule is cross-file-sensitive yet).
pub fn lint_document(path: &Path, root: &SyntaxNode, model: &Model) -> Vec<Diagnostic> {
    lint_document_with_project(path, root, model, None)
}

pub fn lint_document_with_project(
    path: &Path,
    root: &SyntaxNode,
    model: &Model,
    project: Option<&super::project::ProjectFacts>,
) -> Vec<Diagnostic> {
    lint_document_with_rules(path, root, model, project, &all_rules())
}

/// Run an explicit rule set through the ordinary traversal and suppression path.
/// Useful for isolating rule costs without introducing host clocks into analysis.
pub fn lint_document_with_rules(
    path: &Path,
    root: &SyntaxNode,
    model: &Model,
    project: Option<&super::project::ProjectFacts>,
    rules: &[Box<dyn BibRule>],
) -> Vec<Diagnostic> {
    let suppress = BibSuppressionMap::build(root);
    let ctx = BibRuleContext {
        project,
        path,
        root,
        model,
        db: semantic::builtin(),
        suppressions: &suppress,
    };
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    // Build the node-dispatch table: kind discriminant -> indices of subscribed
    // rules. Bib `SyntaxKind` is a contiguous `#[repr(u16)]` with `ROOT` last, so a
    // flat Vec indexed by `kind as usize` beats a hash map. Same shape as the LaTeX
    // driver.
    let mut by_kind: Vec<Vec<usize>> = vec![Vec::new(); SyntaxKind::COUNT];
    let mut any_node_rules = false;
    for (i, rule) in rules.iter().enumerate() {
        for kind in rule.interests() {
            by_kind[*kind as usize].push(i);
            any_node_rules = true;
        }
    }

    // Entry/field subscribers cannot observe the contents below FIELD. Other
    // rule sets retain the full token traversal, including future token rules.
    let entry_field_only = rules
        .iter()
        .flat_map(|rule| rule.interests())
        .all(|kind| matches!(kind, SyntaxKind::ENTRY | SyntaxKind::FIELD));
    if any_node_rules && entry_field_only {
        let mut walk = root.preorder();
        while let Some(event) = walk.next() {
            let rowan::WalkEvent::Enter(node) = event else {
                continue;
            };
            let kind = node.kind();
            if !by_kind[kind as usize].is_empty() {
                let el = node.into();
                for &i in &by_kind[kind as usize] {
                    rules[i].check(&el, &ctx, &mut diagnostics);
                }
            }
            if kind == SyntaxKind::FIELD {
                walk.skip_subtree();
            }
        }
    } else if any_node_rules {
        for el in root.descendants_with_tokens() {
            for &i in &by_kind[el.kind() as usize] {
                rules[i].check(&el, &ctx, &mut diagnostics);
            }
        }
    }

    // Whole-file pass for model-driven rules.
    for rule in rules {
        rule.check_file(&ctx, &mut diagnostics);
    }

    // Filter out findings suppressed by a `@comment{tex-ls-lint …}` carrier.
    diagnostics.retain(|d| {
        is_unsuppressible_suppression_meta_rule(d.rule)
            || !suppress.is_suppressed(d.rule, d.start, d.end)
    });

    for d in &mut diagnostics {
        d.path = path.to_path_buf();
    }
    diagnostics.sort_by_key(|d| (d.start, d.end, d.rule));
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_of(src: &str) -> Vec<&'static str> {
        check_document(Path::new("x.bib"), src)
            .iter()
            .map(|d| d.rule)
            .collect()
    }

    #[test]
    fn clean_file_produces_nothing() {
        let src = "@article{k,\n  author = {A},\n  title = {T},\n  journaltitle = {J},\n  year = 2020,\n}\n";
        assert!(rules_of(src).is_empty(), "got: {:?}", rules_of(src));
    }

    #[test]
    fn collects_multiple_rule_families_sorted() {
        // A duplicate key and an unused @string, sorted by position.
        let src = "@string{unused = {U}}\n@misc{k, title = {A}}\n@misc{k, title = {B}}\n";
        let rules = rules_of(src);
        assert!(rules.contains(&"unused-string"));
        assert!(rules.contains(&"duplicate-key"));
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;
    use crate::bib::syntax::SyntaxElement;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct TokenProbe(Arc<AtomicUsize>);
    impl BibRule for TokenProbe {
        fn id(&self) -> &'static str {
            "test-token-probe"
        }
        fn interests(&self) -> &'static [SyntaxKind] {
            &[SyntaxKind::WORD]
        }
        fn check(&self, _: &SyntaxElement, _: &BibRuleContext<'_>, _: &mut Vec<Diagnostic>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[test]
    fn pruned_dispatch_matches_full_walk_and_token_rules_reach_values() {
        for source in [
            "@string{foo=bar}@book{k,title={DNA},title={DNA},author={},year=2020}",
            "@comment{tex-ls-lint off unknown-field}@misc{key,wrong={É text}}@comment{tex-ls-lint on unknown-field}",
            "@misc{k,title={Nested {words} @misc{x}},author=missing # {Other}}",
            "@misc{,title=,title={},author={}}@misc{k,title={partial",
        ] {
            for removed in 0..=source.chars().count() {
                let text: String = source
                    .chars()
                    .enumerate()
                    .filter_map(|(i, c)| (i != removed).then_some(c))
                    .collect();
                let root = parse(&text).syntax();
                let model = Model::build(&root);
                let path = Path::new("test.bib");
                let expected = lint_document(path, &root, &model);
                let count = Arc::new(AtomicUsize::new(0));
                let mut rules = all_rules();
                rules.push(Box::new(TokenProbe(count.clone())));
                assert_eq!(
                    expected,
                    lint_document_with_rules(path, &root, &model, None, &rules),
                    "{text}"
                );
                assert_eq!(
                    count.load(Ordering::Relaxed),
                    root.descendants_with_tokens()
                        .filter(|el| el.kind() == SyntaxKind::WORD)
                        .count()
                );
            }
        }
    }
}
