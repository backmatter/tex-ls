//! Conservative top-level region reparse.
//!
//! The admitted regions are one top-level prose paragraph, or two adjacent
//! paragraphs plus their blank-line seam. A faithfulness parse must reproduce the
//! old fragment under the base parse's exact [`ParseCtx`]. An in-paragraph edit may
//! then touch only direct prose leaves; a seam edit may touch only root trivia; and
//! both may insert only lexer-state-invariant characters. Unchanged commands and
//! their state transitions therefore remain unchanged. This deliberately refuses
//! edits to commands, groups, math, comments, catcode-sensitive punctuation, and
//! arbitrary root runs. Those need a later general-region proof; treating blank
//! lines alone as a reset would be unsound.

use crate::parser::core::parse_fragment_with_ctx;
use rowan::{GreenNode, GreenToken, NodeOrToken};

use crate::parser::core::SyntaxError;
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use super::leaf::shifted_errors;
use super::{Edit, ReparseBase, ReparseTier, Reparsed, finish};

type GreenElement = NodeOrToken<GreenNode, GreenToken>;

/// Reparse an edit spanning multiple leaves inside one inert top-level paragraph.
pub(super) fn reparse_region(
    base: &ReparseBase<'_>,
    edit: &Edit,
    new_text: &str,
) -> Option<Reparsed> {
    reparse_paragraph_seam(base, edit, new_text)
        .or_else(|| reparse_one_paragraph(base, edit, new_text))
}

fn reparse_one_paragraph(base: &ReparseBase<'_>, edit: &Edit, new_text: &str) -> Option<Reparsed> {
    let root = base.syntax();
    let paragraph = containing_paragraph(&root, edit)?;

    let range = paragraph.text_range();
    let start = usize::from(range.start());
    let end = usize::from(range.end());

    // This tier exists for edits a one-leaf tier cannot express. Every touched
    // token must be a direct prose child: crossing a nested command or group could
    // alter a definition scan or a forward gate, even if the inserted bytes look
    // harmless in isolation.
    let touched: Vec<_> = paragraph
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            let token = token.text_range();
            usize::from(token.start()) < edit.range.end
                && usize::from(token.end()) > edit.range.start
        })
        .collect();
    if edit.range.is_empty()
        || touched.len() < 2
        || touched.iter().any(|token| {
            token.parent().as_ref() != Some(&paragraph)
                || !matches!(
                    token.kind(),
                    SyntaxKind::WORD | SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE
                )
        })
    {
        return None;
    }

    let old = base.text.get(start..end)?;
    if old.contains("\\ifx") {
        return None;
    }
    let local = Edit {
        range: edit.range.start.checked_sub(start)?..edit.range.end.checked_sub(start)?,
        insert: edit.insert.clone(),
    };
    let new = local.apply(old);

    // These are precisely the inserted surface characters whose classification
    // may depend on TeX/expl3/package lexer state or which can open structure.
    if !inert_insert(&edit.insert) {
        return None;
    }

    // Faithfulness is the entry-context proof: if a mode established before this
    // paragraph matters to its bytes, a fresh fragment parse will not reproduce
    // the paragraph and the tier refuses. Unchanged commands inside the fragment
    // then make the lexer exit state change by exactly the same transitions.
    let old_parsed = parse_fragment_with_ctx(old, base.ctx, base.config, base.implicit_expl, start);
    if !old_parsed.errors.is_empty() {
        return None;
    }
    let old_fragment = only_paragraph(&old_parsed.syntax())?;
    if old_fragment.green() != paragraph.green() {
        return None;
    }

    let parsed = parse_fragment_with_ctx(&new, base.ctx, base.config, base.implicit_expl, start);
    if !parsed.errors.is_empty() {
        return None;
    }
    let replacement = only_paragraph(&parsed.syntax())?;

    let errors = shifted_errors(base.errors, range, edit)?;
    let green = paragraph.replace_with(replacement.green().to_owned());
    finish(green, errors, ReparseTier::Region, base, new_text)
}

/// Reparse two paragraphs and the blank-line seam between them.
///
/// The unchanged outer seams keep paragraph-anchored gates outside the fragment
/// unaffected. Gates without paragraph anchors cannot change because the edit may
/// touch only root trivia and inserts no structural spelling.
fn reparse_paragraph_seam(base: &ReparseBase<'_>, edit: &Edit, new_text: &str) -> Option<Reparsed> {
    if base.config.dtx || edit.range.is_empty() || !inert_insert(&edit.insert) {
        return None;
    }

    let root = base.syntax();
    let selection = seam_selection(&root, edit)?;
    let first = selection.first;
    let last = selection.last;
    let range = selection.range;
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    let old = base.text.get(start..end)?;
    if old.contains("\\ifx") {
        return None;
    }
    let local = Edit {
        range: edit.range.start.checked_sub(start)?..edit.range.end.checked_sub(start)?,
        insert: edit.insert.clone(),
    };
    let new = local.apply(old);

    let old_green: Vec<GreenElement> = base
        .green
        .children()
        .skip(first)
        .take(last - first + 1)
        .map(|element| element.to_owned())
        .collect();
    let expected = GreenNode::new(SyntaxKind::ROOT.into(), old_green);
    let old_parsed = parse_fragment_with_ctx(old, base.ctx, base.config, base.implicit_expl, start);
    if old_parsed.green != expected || !old_parsed.errors.is_empty() {
        return None;
    }

    let parsed = parse_fragment_with_ctx(&new, base.ctx, base.config, base.implicit_expl, start);
    let errors = replaced_errors(base.errors, range, &parsed.errors, edit)?;
    let replacement: Vec<GreenElement> = parsed
        .green
        .children()
        .map(|element| element.to_owned())
        .collect();
    let mut children =
        Vec::with_capacity(base.green.children().len() - (last - first + 1) + replacement.len());
    children.extend(
        base.green
            .children()
            .take(first)
            .map(|element| element.to_owned()),
    );
    children.extend(replacement);
    children.extend(
        base.green
            .children()
            .skip(last + 1)
            .map(|element| element.to_owned()),
    );
    let green = GreenNode::new(SyntaxKind::ROOT.into(), children);
    finish(green, errors, ReparseTier::Region, base, new_text)
}

#[cfg(test)]
thread_local! { static ROOT_SELECTION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[inline]
fn selection_visit() {
    #[cfg(test)]
    ROOT_SELECTION_VISITS.with(|n| n.set(n.get() + 1));
}
#[derive(Debug, PartialEq, Eq)]
struct SeamSelection {
    first: usize,
    last: usize,
    range: rowan::TextRange,
}
/// Find the earliest direct child containing the edit start. Rowan may choose
/// either side of an empty range, so rewind all children sharing that boundary.
fn first_at_start(root: &SyntaxNode, edit: &Edit) -> Option<SyntaxElement> {
    let start = rowan::TextSize::try_from(edit.range.start).ok()?;
    let point = rowan::TextRange::empty(start);
    if !root.text_range().contains_range(point) {
        return None;
    }
    let mut element = root.child_or_token_at_range(point)?;
    while let Some(previous) = element.prev_sibling_or_token() {
        selection_visit();
        if previous.text_range().end() < start {
            break;
        }
        element = previous;
    }
    Some(element)
}

fn containing_paragraph(root: &SyntaxNode, edit: &Edit) -> Option<SyntaxNode> {
    let mut element = first_at_start(root, edit)?;
    loop {
        selection_visit();
        let range = element.text_range();
        if usize::from(range.start()) > edit.range.start {
            return None;
        }
        if element.kind() == SyntaxKind::PARAGRAPH
            && edit.range.start >= usize::from(range.start())
            && edit.range.end <= usize::from(range.end())
        {
            return element.into_node();
        }
        element = element.next_sibling_or_token()?;
    }
}

fn seam_selection(root: &SyntaxNode, edit: &Edit) -> Option<SeamSelection> {
    let mut element = first_at_start(root, edit)?;
    let mut touched_first = None;
    let mut touched_last = None;
    let mut touched_count = 0;
    loop {
        selection_visit();
        let range = element.text_range();
        if usize::from(range.start()) >= edit.range.end {
            break;
        }
        // Keep the old strict overlap predicate, including zero-length children
        // strictly inside the edit. Such children must not be silently skipped.
        if usize::from(range.end()) > edit.range.start {
            if !root_trivia(&element) {
                return None;
            }
            touched_count += 1;
            touched_first.get_or_insert_with(|| element.clone());
            touched_last = Some(element.clone());
        }
        match element.next_sibling_or_token() {
            Some(next) => element = next,
            None => break,
        }
    }
    if touched_count < 2 {
        return None;
    }
    let mut first = touched_first?;
    loop {
        first = first.prev_sibling_or_token()?;
        selection_visit();
        if first.kind() == SyntaxKind::PARAGRAPH {
            break;
        }
        if !root_trivia(&first) {
            return None;
        }
    }
    let mut last = touched_last?;
    loop {
        last = last.next_sibling_or_token()?;
        selection_visit();
        if last.kind() == SyntaxKind::PARAGRAPH {
            break;
        }
        if !root_trivia(&last) {
            return None;
        }
    }
    Some(SeamSelection {
        first: first.index(),
        last: last.index(),
        range: rowan::TextRange::new(first.text_range().start(), last.text_range().end()),
    })
}

fn root_trivia(element: &SyntaxElement) -> bool {
    matches!(element.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE)
}

pub(super) fn replaced_errors(
    errors: &[SyntaxError],
    old_range: rowan::TextRange,
    replacement: &[SyntaxError],
    edit: &Edit,
) -> Option<Vec<SyntaxError>> {
    let start = usize::from(old_range.start());
    let end = usize::from(old_range.end());
    let mut out = Vec::with_capacity(errors.len() + replacement.len());
    for error in errors {
        if error.end <= start {
            out.push(error.clone());
        } else if error.start < end {
            return None;
        }
    }
    out.extend(replacement.iter().map(|error| SyntaxError {
        message: error.message.clone(),
        start: error.start + start,
        end: error.end + start,
    }));
    for error in errors.iter().filter(|error| error.start >= end) {
        out.push(SyntaxError {
            message: error.message.clone(),
            start: error.start.checked_add_signed(edit.delta())?,
            end: error.end.checked_add_signed(edit.delta())?,
        });
    }
    Some(out)
}

fn only_paragraph(root: &SyntaxNode) -> Option<SyntaxNode> {
    let mut elements = root.children_with_tokens();
    let paragraph = elements.next()?.into_node()?;
    (paragraph.kind() == SyntaxKind::PARAGRAPH && elements.next().is_none()).then_some(paragraph)
}

fn inert_insert(text: &str) -> bool {
    !text.chars().any(|c| {
        matches!(
            c,
            '\\' | '{' | '}' | '[' | ']' | '$' | '&' | '#' | '^' | '_' | '%' | '~' | ':' | '@'
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::declarations::ResolvedDeclarations;
    use crate::parser::core::parse_with_declarations_resolved;
    use crate::parser::lexer::LatexFlavor;

    use super::*;

    fn assert_region(text: &str, range: std::ops::Range<usize>, insert: &str) {
        let declared = ResolvedDeclarations::default();
        let (parse, ctx) = parse_with_declarations_resolved(text, LatexFlavor::Document, &declared);
        let base = ReparseBase::from_parts(
            text,
            &parse.green,
            &parse.errors,
            &ctx,
            LatexFlavor::Document.into(),
            &declared,
        );
        let edit = Edit {
            range,
            insert: insert.into(),
        };
        let new_text = edit.apply(text);
        let out = reparse_region(&base, &edit, &new_text).expect("region should splice");
        assert_eq!(out.tier, ReparseTier::Region);
    }

    #[test]
    fn replaces_a_multi_token_span_in_one_paragraph() {
        assert_region(
            "alpha beta gamma.\n\nnext paragraph.\n",
            6..16,
            "better words",
        );
    }

    #[test]
    fn handles_crlf_around_the_paragraph_without_inspecting_literal_seams() {
        assert_region(
            "alpha beta gamma.\r\n\r\nnext paragraph.\r\n",
            6..16,
            "better words",
        );
    }

    #[test]
    fn unchanged_commands_may_share_the_paragraph() {
        assert_region(
            "alpha beta and \\emph{nested words} after gamma delta.\n",
            6..14,
            "better prose",
        );
    }

    #[test]
    fn deleting_a_blank_line_merges_adjacent_paragraphs() {
        assert_region("alpha beta.\n\ngamma delta.\n", 11..13, " ");
    }

    #[test]
    fn refuses_stateful_or_structural_text() {
        let declared = ResolvedDeclarations::default();
        let text = "alpha \\emph{beta} gamma.\n";
        let (parse, ctx) = parse_with_declarations_resolved(text, LatexFlavor::Document, &declared);
        let base = ReparseBase::from_parts(
            text,
            &parse.green,
            &parse.errors,
            &ctx,
            LatexFlavor::Document.into(),
            &declared,
        );
        let edit = Edit {
            range: 3..20,
            insert: "plain".into(),
        };
        assert!(reparse_region(&base, &edit, &edit.apply(text)).is_none());
    }

    #[test]
    fn faithfulness_refuses_a_paragraph_whose_entry_mode_is_not_reproduced() {
        let declared = ResolvedDeclarations::default();
        let text = "\\ExplSyntaxOn\n\nalpha_beta gamma delta.\n";
        let (parse, ctx) = parse_with_declarations_resolved(text, LatexFlavor::Document, &declared);
        let base = ReparseBase::from_parts(
            text,
            &parse.green,
            &parse.errors,
            &ctx,
            LatexFlavor::Document.into(),
            &declared,
        );
        let edit = Edit {
            range: 26..37,
            insert: "better prose".into(),
        };
        assert!(reparse_region(&base, &edit, &edit.apply(text)).is_none());
    }

    fn reference_paragraph(root: &SyntaxNode, edit: &Edit) -> Option<SyntaxNode> {
        let paragraph = root.children().find(|node| {
            if node.kind() != SyntaxKind::PARAGRAPH {
                return false;
            }
            let range = node.text_range();
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            edit.range.start >= start && edit.range.end <= end
        })?;
        Some(paragraph)
    }
    fn reference_seam(root: &SyntaxNode, edit: &Edit) -> Option<SeamSelection> {
        let elements: Vec<SyntaxElement> = root.children_with_tokens().collect();
        let touched: Vec<usize> = elements
            .iter()
            .enumerate()
            .filter_map(|(i, element)| {
                let range = element.text_range();
                (usize::from(range.start()) < edit.range.end
                    && usize::from(range.end()) > edit.range.start)
                    .then_some(i)
            })
            .collect();
        if touched.len() < 2
            || touched.iter().any(|&i| {
                !matches!(
                    elements[i].kind(),
                    SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE
                )
            })
        {
            return None;
        }

        let seam_first = *touched.first()?;
        let seam_last = *touched.last()?;
        let first = (0..seam_first)
            .rev()
            .find(|&i| elements[i].kind() == SyntaxKind::PARAGRAPH)?;
        let last = (seam_last + 1..elements.len())
            .find(|&i| elements[i].kind() == SyntaxKind::PARAGRAPH)?;
        if elements[first + 1..last]
            .iter()
            .any(|element| !matches!(element.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE))
        {
            return None;
        }

        let range = rowan::TextRange::new(
            elements[first].text_range().start(),
            elements[last].text_range().end(),
        );

        Some(SeamSelection { first, last, range })
    }

    #[test]
    fn region_selection_matches_independent_linear_reference() {
        for source in [
            "",
            "alpha beta.\n\ngamma delta.\n",
            "é😀 alpha.\r\n\r\nbeta.\r\n",
            "prefix {\\textbf{one}\n\nend",
            "\\begin{quote}alpha\\end{quote}\n\ntail.\n",
            "% comment\n\nalpha beta.\n\n\\section{X} tail.\n",
        ] {
            let parsed = crate::parser::parse(source);
            assert_eq!(parsed.syntax().text().to_string(), source);
            let root = parsed.syntax();
            for start in 0..=source.len() {
                if !source.is_char_boundary(start) {
                    continue;
                }
                for end in start..=source.len() {
                    if !source.is_char_boundary(end) {
                        continue;
                    }
                    let edit = Edit {
                        range: start..end,
                        insert: String::new(),
                    };
                    assert_eq!(
                        containing_paragraph(&root, &edit),
                        reference_paragraph(&root, &edit),
                        "{source:?} {edit:?}"
                    );
                    assert_eq!(
                        seam_selection(&root, &edit),
                        reference_seam(&root, &edit),
                        "{source:?} {edit:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn region_selection_matches_reference_with_empty_boundary_elements() {
        for empty_kind in [SyntaxKind::PARAGRAPH, SyntaxKind::GROUP] {
            let empty = || {
                NodeOrToken::Node(GreenNode::new(
                    empty_kind.into(),
                    Vec::<GreenElement>::new(),
                ))
            };
            let paragraph = || {
                NodeOrToken::Node(GreenNode::new(
                    SyntaxKind::PARAGRAPH.into(),
                    [NodeOrToken::Token(GreenToken::new(
                        SyntaxKind::WORD.into(),
                        "a",
                    ))],
                ))
            };
            let newline = || NodeOrToken::Token(GreenToken::new(SyntaxKind::NEWLINE.into(), "\n"));
            let root = SyntaxNode::new_root(GreenNode::new(
                SyntaxKind::ROOT.into(),
                [
                    empty(),
                    paragraph(),
                    empty(),
                    newline(),
                    empty(),
                    newline(),
                    empty(),
                    paragraph(),
                    empty(),
                ],
            ));
            for start in 0..=4 {
                for end in start..=4 {
                    let edit = Edit {
                        range: start..end,
                        insert: String::new(),
                    };
                    assert_eq!(
                        containing_paragraph(&root, &edit),
                        reference_paragraph(&root, &edit),
                        "{empty_kind:?} {edit:?}"
                    );
                    assert_eq!(
                        seam_selection(&root, &edit),
                        reference_seam(&root, &edit),
                        "{empty_kind:?} {edit:?}"
                    );
                }
            }
        }
    }

    fn check_wide_selection(seam: bool) {
        for newline in ["\n", "\r\n"] {
            let source = format!("alpha beta gamma.{newline}{newline}").repeat(1024);
            let declared = ResolvedDeclarations::default();
            let (parsed, ctx) =
                parse_with_declarations_resolved(&source, LatexFlavor::Document, &declared);
            assert!(parsed.errors.is_empty());
            assert_eq!(parsed.syntax().text().to_string(), source);
            assert_eq!(
                parsed
                    .syntax()
                    .children()
                    .filter(|n| n.kind() == SyntaxKind::PARAGRAPH)
                    .count(),
                1024
            );
            let edit = if seam {
                let gap = format!("{newline}{newline}alpha");
                let at = source.rfind(&gap).unwrap();
                Edit {
                    range: at..at + 2 * newline.len(),
                    insert: " ".into(),
                }
            } else {
                let at = source.rfind("beta gamma").unwrap();
                Edit {
                    range: at..at + "beta gamma".len(),
                    insert: "better prose".into(),
                }
            };
            let base = ReparseBase::from_parts(
                &source,
                &parsed.green,
                &parsed.errors,
                &ctx,
                LatexFlavor::Document.into(),
                &declared,
            );
            let changed = edit.apply(&source);
            ROOT_SELECTION_VISITS.with(|n| n.set(0));
            let result =
                super::super::reparse(&base, &edit, &changed).expect("actual region admission");
            let visits = ROOT_SELECTION_VISITS.with(|n| n.get());
            assert_eq!(result.tier, ReparseTier::Region);
            let (full, full_ctx) =
                parse_with_declarations_resolved(&changed, LatexFlavor::Document, &declared);
            assert!(full.errors.is_empty());
            assert_eq!(result.green, full.green);
            assert_eq!(result.errors, full.errors);
            assert_eq!(result.context(&ctx), &full_ctx);
            assert_eq!(
                SyntaxNode::new_root(result.green.clone())
                    .text()
                    .to_string(),
                changed
            );
            assert!(
                visits <= 32,
                "region root selection visited {visits} unrelated siblings, bound 32"
            );
        }
    }
    #[test]
    fn region_selection_work_is_local_for_last_paragraph_edit() {
        check_wide_selection(false);
    }
    #[test]
    fn region_selection_work_is_local_for_last_seam_edit() {
        check_wide_selection(true);
    }
}
