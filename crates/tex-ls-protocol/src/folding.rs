//! Wire conversion for analysis-owned folding regions.
use lsp_types::{FoldingRange, FoldingRangeKind};
use tex_ls_analysis::text::LineIndex;
use tex_ls_parser::syntax::SyntaxNode;

pub fn folding_ranges(root: &SyntaxNode, idx: &LineIndex) -> Vec<FoldingRange> {
    convert(tex_ls_analysis::folding::folding_ranges(
        root,
        idx,
        &tex_ls_parser::semantic::outline(root),
    ))
}
pub(crate) fn convert(ranges: Vec<tex_ls_analysis::folding::FoldingRange>) -> Vec<FoldingRange> {
    ranges
        .into_iter()
        .map(|range| FoldingRange {
            start_line: range.start_line,
            end_line: range.end_line,
            kind: range.kind.map(|_| FoldingRangeKind::Comment),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_parser::parser::parse;

    fn folds(src: &str) -> Vec<FoldingRange> {
        let root = SyntaxNode::new_root(parse(src).green);
        let idx = LineIndex::new(src);
        folding_ranges(&root, &idx)
    }

    /// A `(start_line, end_line, kind)` triple for terse assertions.
    fn triples(ranges: &[FoldingRange]) -> Vec<(u32, u32, Option<FoldingRangeKind>)> {
        ranges
            .iter()
            .map(|r| (r.start_line, r.end_line, r.kind.clone()))
            .collect()
    }

    #[test]
    fn sibling_sections_fold_each_to_before_next() {
        // line 0: \section{A}, 1: text, 2: \section{B}, 3: more
        let src = "\\section{A}\ntext\n\\section{B}\nmore\n";
        let t = triples(&folds(src));
        assert!(t.contains(&(0, 1, None)), "section A folds 0..1, got {t:?}");
        assert!(t.contains(&(2, 3, None)), "section B folds 2..3, got {t:?}");
    }

    #[test]
    fn nested_subsection_folds_within_section() {
        // 0: \section{A}, 1: \subsection{B}, 2: body, 3: \section{C}
        let src = "\\section{A}\n\\subsection{B}\nbody\n\\section{C}\nx\n";
        let t = triples(&folds(src));
        // A spans to just before C (line 2), B spans to the same body line.
        assert!(t.contains(&(0, 2, None)), "section A, got {t:?}");
        assert!(t.contains(&(1, 2, None)), "subsection B, got {t:?}");
    }

    #[test]
    fn single_line_section_does_not_fold() {
        let src = "\\section{A}\n";
        assert!(folds(src).is_empty());
    }

    #[test]
    fn last_section_runs_to_eof() {
        let src = "\\section{A}\nl1\nl2\n";
        let t = triples(&folds(src));
        assert!(t.contains(&(0, 2, None)), "got {t:?}");
    }

    #[test]
    fn multiline_environment_folds() {
        let src = "\\begin{itemize}\n\\item x\n\\end{itemize}\n";
        let t = triples(&folds(src));
        assert!(t.contains(&(0, 2, None)), "itemize folds 0..2, got {t:?}");
    }

    #[test]
    fn single_line_environment_does_not_fold() {
        let src = "\\begin{a}x\\end{a}\n";
        assert!(folds(src).is_empty());
    }

    #[test]
    fn comment_run_folds_as_comment() {
        // 0,1,2 are standalone comments; 3 is code.
        let src = "% a\n% b\n% c\ntext\n";
        let t = triples(&folds(src));
        assert_eq!(t, vec![(0, 2, Some(FoldingRangeKind::Comment))]);
    }

    #[test]
    fn single_comment_does_not_fold() {
        let src = "% lonely\ntext\n";
        assert!(folds(src).is_empty());
    }

    #[test]
    fn leading_comments_fold_separately_from_their_construct() {
        // The `%` run binds into the following ENVIRONMENT/COMMAND node, but each
        // construct must fold from its own `\begin`/`\section` line, leaving the
        // comment run as its own fold.
        let src = "% a\n% b\n\\begin{itemize}\n\\item x\n\\end{itemize}\n";
        let t = triples(&folds(src));
        assert!(
            t.contains(&(0, 1, Some(FoldingRangeKind::Comment))),
            "comment run, got {t:?}"
        );
        assert!(t.contains(&(2, 4, None)), "itemize from \\begin, got {t:?}");

        let src = "% a\n% b\n\\section{A}\nbody\nmore\n";
        let t = triples(&folds(src));
        assert!(
            t.contains(&(0, 1, Some(FoldingRangeKind::Comment))),
            "comment run, got {t:?}"
        );
        assert!(t.contains(&(2, 4, None)), "section from heading, got {t:?}");
    }

    #[test]
    fn trailing_comment_does_not_join_a_run() {
        // A trailing comment after code is not standalone, so the two `%` lines do
        // not form a run.
        let src = "code % a\n% b\nmore\n";
        assert!(folds(src).is_empty(), "got {:?}", triples(&folds(src)));
    }
}
