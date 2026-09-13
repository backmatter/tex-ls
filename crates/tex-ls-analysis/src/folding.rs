//! `textDocument/foldingRange` computation: a pure single-file CST walk producing
//! the foldable regions of a LaTeX document — environments (`\begin…\end`),
//! sectioning spans (`\section` … `\subparagraph`), and runs of standalone comment
//! lines. No semantic model and no workspace lookup, so it runs straight on the read
//! pool like the document-symbol outline.
//!
//! Three sources, each emitting only multi-line spans (a single-line construct never
//! folds):
//!
//! - **Sectioning spans** reuse [`tex_ls_parser::semantic::outline()`]: an [`OutlineSymbol::Section`] item's
//!   `range` is already stretched by the outline's level-stack nesting to where the
//!   section closes (the next heading of equal/shallower level, or the end of the
//!   enclosing scope), which is exactly a section fold. We recurse the tree so nested
//!   subsections fold within their parent.
//! - **Environments** fold every `ENVIRONMENT` node from its `\begin` line to its
//!   `\end` line.
//! - **Comment runs** group maximal runs of consecutive *standalone* comment lines (a
//!   comment that is the only non-trivia token on its line; a trailing `code % x`
//!   comment is excluded) and fold each run of two or more lines under
//!   [`FoldingRangeKind::Comment`].

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldingRange {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: Option<FoldingRangeKind>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldingRangeKind {
    Comment,
}

use crate::text::LineIndex;
use tex_ls_parser::semantic::{OutlineItem, OutlineSymbol};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxNode};

/// The foldable regions of an already-parsed LaTeX `root`. `idx` must index the
/// same buffer `root` was parsed from.
pub fn folding_ranges(
    root: &SyntaxNode,
    idx: &LineIndex,
    outline: &[OutlineItem],
) -> Vec<FoldingRange> {
    let line_of = |offset: usize| idx.position(offset).0;
    let mut ranges = Vec::new();

    // 1. Sectioning spans — reuse the outline's stretched section ranges. A section's
    //    `range.end()` is exclusive (the next heading's start, or the scope end), so
    //    the last line that belongs to it is `end - 1`; using `end` would fold the
    //    following heading's line into the previous section.
    collect_section_folds(outline, &line_of, &mut ranges);

    // 2. Environments — every `\begin…\end`. Fold from the `\begin` line (the BEGIN
    //    child's start, *not* the node's: a run of leading `%` comments binds into the
    //    ENVIRONMENT node, so its own start can sit lines earlier — those fold as a
    //    comment run instead). The node end is just past the `\end` group's closing
    //    brace, on the `\end` line, so no off-by-one is needed there.
    for node in root
        .descendants()
        .filter(|n| matches!(n.kind(), SyntaxKind::ENVIRONMENT | SyntaxKind::DISPLAY_MATH))
    {
        let begin = node
            .children()
            .find(|c| c.kind() == SyntaxKind::BEGIN)
            .unwrap_or_else(|| node.clone());
        emit(
            &mut ranges,
            line_of(begin.text_range().start().into()),
            line_of(node.text_range().end().into()),
            None,
        );
    }

    // 3. Comment runs — collect the line of every standalone comment, then fold each
    //    maximal run of consecutive lines (length >= 2).
    let mut standalone: Vec<u32> = Vec::new();
    let mut code_on_line = false;
    for token in root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        match token.kind() {
            SyntaxKind::NEWLINE => code_on_line = false,
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::COMMENT => {
                if !code_on_line {
                    standalone.push(line_of(token.text_range().start().into()));
                }
            }
            _ => code_on_line = true,
        }
    }
    let mut i = 0;
    while i < standalone.len() {
        let mut j = i + 1;
        while j < standalone.len() && standalone[j] == standalone[j - 1] + 1 {
            j += 1;
        }
        if j - i >= 2 {
            ranges.push(FoldingRange {
                start_line: standalone[i],
                end_line: standalone[j - 1],
                kind: Some(FoldingRangeKind::Comment),
            });
        }
        i = j;
    }

    ranges
}

/// Emit a fold per `Section` item, recursing into children so nested subsections fold
/// within their parent. Non-section items (floats/theorems are folded by the
/// environment walk; labels are not foldable) are skipped but still recursed through.
fn collect_section_folds(
    items: &[OutlineItem],
    line_of: &impl Fn(usize) -> u32,
    ranges: &mut Vec<FoldingRange>,
) {
    for item in items {
        if matches!(item.kind, OutlineSymbol::Section | OutlineSymbol::Item) {
            let range = item.range;
            if range.end() > range.start() {
                // Start at the heading line via `selection_range` (the title group,
                // always on the `\section` line) rather than `range.start()`: a run of
                // leading `%` comments binds into the COMMAND node, so its own start
                // can sit lines earlier (those fold as a comment run instead).
                emit(
                    ranges,
                    line_of(item.selection_range.start().into()),
                    line_of(usize::from(range.end()) - 1),
                    None,
                );
            }
        }
        collect_section_folds(&item.children, line_of, ranges);
    }
}

/// Push a fold spanning `[start_line, end_line]`, dropping single-line spans.
fn emit(
    ranges: &mut Vec<FoldingRange>,
    start_line: u32,
    end_line: u32,
    kind: Option<FoldingRangeKind>,
) {
    if end_line > start_line {
        ranges.push(FoldingRange {
            start_line,
            end_line,
            kind,
        });
    }
}
