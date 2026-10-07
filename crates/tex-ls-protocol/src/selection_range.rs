//! `textDocument/selectionRange` computation: a pure single-file CST walk producing,
//! for each cursor position, the nested "expand selection" chain that grows outward
//! through the syntax hierarchy (token -> group -> argument -> command -> environment
//! -> ... -> root). No semantic model and no workspace lookup, so it runs straight on
//! the read pool like the folding outline.
//!
//! The chain is exactly the CST ancestor stack: the leaf token at the cursor, then
//! every enclosing node up to and including `ROOT`. Because the enclosing-environment
//! stack is a subsequence of those ancestors, this subsumes texlab's `findEnvironments`
//! command for free. We add no kind filtering—the tree's own
//! nesting is the intended granularity—and only collapse consecutive equal ranges
//! (a `COMMAND` and its sole `ARGUMENT` often share a span) so each step strictly
//! widens, as clients expect.

use lsp_types::{Position, SelectionRange};
use rowan::TextRange;

use tex_ls_analysis::text::LineIndex;
use tex_ls_parser::syntax::SyntaxNode;

/// The expand-selection chain for each cursor in `positions`, one [`SelectionRange`]
/// per input position (in order). `idx` must index the same buffer `root` was
/// parsed from.
pub fn selection_ranges(
    root: &SyntaxNode,
    idx: &LineIndex,
    positions: &[Position],
) -> Vec<SelectionRange> {
    let positions: Vec<_> = positions
        .iter()
        .map(|pos| (pos.line, pos.character))
        .collect();
    let chains = idx
        .offsets_at(&positions)
        .into_iter()
        .map(|offset| tex_ls_analysis::selection::selection_chain(root, offset))
        .collect();
    from_chains(idx, chains)
}

/// Convert all chain endpoints together, then link each selection to its parent.
pub(crate) fn from_chains(idx: &LineIndex, chains: Vec<Vec<TextRange>>) -> Vec<SelectionRange> {
    let ranges: Vec<_> = chains.iter().flatten().copied().collect();
    let converted = super::convert::lsp_nested_ranges(idx, &ranges);
    let mut offset = 0;
    chains
        .into_iter()
        .map(|chain| {
            let end = offset + chain.len();
            let mut parent = None;
            for &range in converted[offset..end].iter().rev() {
                parent = Some(Box::new(SelectionRange { range, parent }));
            }
            offset = end;
            *parent.expect("selection includes root")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_parser::parser::parse;

    /// A `((start_line, start_col), (end_line, end_col))` range pair for terse
    /// assertions.
    type Pair = ((u32, u32), (u32, u32));

    /// The expand-selection chain at `(line, character)`, flattened innermost ->
    /// outermost as `(start, end)` LSP position pairs for terse assertions.
    fn chain(src: &str, line: u32, character: u32) -> Vec<Pair> {
        let root = SyntaxNode::new_root(parse(src).green);
        let idx = LineIndex::new(src);
        let sr = selection_ranges(&root, &idx, &[Position::new(line, character)]).remove(0);
        let mut out = Vec::new();
        let mut cur = Some(Box::new(sr));
        while let Some(node) = cur {
            let r = node.range;
            out.push((
                (r.start.line, r.start.character),
                (r.end.line, r.end.character),
            ));
            cur = node.parent;
        }
        out
    }

    /// Assert the chain strictly widens: each range contains the previous, and no two
    /// consecutive ranges are equal (dedup should have collapsed those).
    fn assert_strictly_nested(chain: &[Pair]) {
        for pair in chain.windows(2) {
            let (inner, outer) = (pair[0], pair[1]);
            assert!(
                outer.0 <= inner.0 && inner.1 <= outer.1 && outer != inner,
                "each step must strictly widen: {inner:?} inside {outer:?}",
            );
        }
    }

    #[test]
    fn command_argument_expands_group_to_command_to_root() {
        // \section{Intro}\n — cursor on the 'n' of Intro (byte 10).
        let src = "\\section{Intro}\n";
        let c = chain(src, 0, 10);
        assert_strictly_nested(&c);
        // Innermost is the WORD "Intro" (chars 9..14).
        assert_eq!(
            c[0],
            ((0, 9), (0, 14)),
            "innermost is the title word: {c:?}"
        );
        // A GROUP {Intro} (8..15) and the whole \section command (0..15) sit between.
        assert!(
            c.contains(&((0, 8), (0, 15))),
            "the {{Intro}} group is a level: {c:?}"
        );
        assert!(
            c.contains(&((0, 0), (0, 15))),
            "the \\section command is a level: {c:?}"
        );
        // Outermost is the whole document (through the trailing newline).
        assert_eq!(
            *c.last().unwrap(),
            ((0, 0), (1, 0)),
            "root spans the doc: {c:?}"
        );
    }

    #[test]
    fn cursor_in_environment_body_has_environment_ancestor() {
        // The enclosing \begin..\end must appear as a widening step (findEnvironments).
        let src = "\\begin{itemize}\n\\item hello\n\\end{itemize}\n";
        // Cursor on "hello" (line 1).
        let c = chain(src, 1, 8);
        assert_strictly_nested(&c);
        // Some level spans from the \begin line to just past \end (the ENVIRONMENT).
        assert!(
            c.iter().any(|&(s, e)| s == (0, 0) && e == (2, 13)),
            "an ENVIRONMENT level spans \\begin..\\end: {c:?}"
        );
        assert_eq!(
            *c.last().unwrap(),
            ((0, 0), (3, 0)),
            "root spans the doc: {c:?}"
        );
    }

    #[test]
    fn math_script_expands_outward() {
        // $x^2$ — cursor on the script '2'. Just assert a well-formed widening chain
        // ending at the document root; the exact math node kinds are the parser's.
        let src = "$x^2$\n";
        let c = chain(src, 0, 3);
        assert_strictly_nested(&c);
        assert!(c.len() >= 2, "at least token + root: {c:?}");
        assert_eq!(
            *c.last().unwrap(),
            ((0, 0), (1, 0)),
            "root spans the doc: {c:?}"
        );
    }

    #[test]
    fn bare_word_in_paragraph_expands_to_root() {
        let src = "hello world\n";
        let c = chain(src, 0, 2);
        assert_strictly_nested(&c);
        // Innermost covers the "hello" word only, not the whole line.
        assert_eq!(c[0], ((0, 0), (0, 5)), "innermost is the word: {c:?}");
        assert_eq!(
            *c.last().unwrap(),
            ((0, 0), (1, 0)),
            "root spans the doc: {c:?}"
        );
    }

    #[test]
    fn out_of_range_position_clamps_and_ends_at_root() {
        let src = "\\section{A}\n";
        // Far past EOF: `offset_at` clamps to the buffer end, so we still return a
        // well-formed chain (from the last token) terminating at the document root.
        let c = chain(src, 99, 0);
        assert_strictly_nested(&c);
        assert_eq!(
            *c.last().unwrap(),
            ((0, 0), (1, 0)),
            "root spans the doc: {c:?}"
        );
    }

    #[test]
    fn empty_document_yields_a_single_empty_range() {
        let c = chain("", 0, 0);
        assert_eq!(c.len(), 1, "only the root range: {c:?}");
        assert_eq!(c[0], ((0, 0), (0, 0)), "empty root: {c:?}");
    }

    #[test]
    fn multiple_positions_map_one_to_one() {
        let src = "\\section{A}\n\\section{B}\n";
        let root = SyntaxNode::new_root(parse(src).green);
        let idx = LineIndex::new(src);
        let out = selection_ranges(&root, &idx, &[Position::new(0, 9), Position::new(1, 9)]);
        assert_eq!(out.len(), 2, "one chain per input position");
    }
}
