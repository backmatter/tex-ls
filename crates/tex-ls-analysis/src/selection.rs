//! Syntax-owned selection chains, innermost byte range first.
use rowan::{TextRange, TextSize, TokenAtOffset};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

pub fn selection_chain(root: &SyntaxNode, offset: usize) -> Vec<TextRange> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);

    // Collect the innermost-first stack of byte ranges. A cursor inside a token starts
    // from that token; a cursor at a token boundary prefers the non-trivia side so it
    // expands into real syntax first; a cursor past EOF has only the root.
    let mut ranges: Vec<TextRange> = Vec::new();
    match root.token_at_offset(at) {
        TokenAtOffset::Single(tok) => push_token_chain(&tok, &mut ranges),
        TokenAtOffset::Between(left, right) => {
            push_token_chain(&prefer_nontrivia(left, right), &mut ranges)
        }
        TokenAtOffset::None => ranges.push(root.text_range()),
    }
    // A node and its sole child can share a span; collapse so each level grows.
    ranges.dedup();

    ranges
}

/// Push `tok`'s range, then every ancestor's range up to and including `ROOT`.
fn push_token_chain(tok: &SyntaxToken, ranges: &mut Vec<TextRange>) {
    ranges.push(tok.text_range());
    ranges.extend(tok.parent_ancestors().map(|n| n.text_range()));
}

/// At a token boundary, expand into the non-trivia side when exactly one side is
/// trivia; otherwise default to the right token (rust-analyzer's convention).
fn prefer_nontrivia(left: SyntaxToken, right: SyntaxToken) -> SyntaxToken {
    let is_trivia = |k| {
        matches!(
            k,
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
        )
    };
    if is_trivia(right.kind()) && !is_trivia(left.kind()) {
        left
    } else {
        right
    }
}
