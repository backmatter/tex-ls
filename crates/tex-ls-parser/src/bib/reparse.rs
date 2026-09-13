//! BibTeX leaf reparsing. The context-free lexer and grammar depend only on token
//! kinds except for entry-type names. Preserve the kind sequence, exclude that
//! position, and remap diagnostics at token boundaries. No recovery is skipped.
use rowan::{GreenToken, NodeOrToken, TextRange, TextSize};

use super::{Parse, SyntaxError, lex, syntax::SyntaxKind};
use crate::parser::Edit;

/// Splice one WORD/NUMBER token when its lexical neighbours and grammar decisions
/// are unchanged. Unproved edits return `None` and must use the ordinary parser.
pub fn reparse(text: &str, base: &Parse, edit: &Edit, new_text: &str) -> Option<Parse> {
    if !edit.fits(text) || edit.insert.len() > 4096 {
        return None;
    }
    let end = edit.range.start.checked_add(edit.insert.len())?;
    if new_text.get(..edit.range.start) != text.get(..edit.range.start)
        || new_text.get(edit.range.start..end) != Some(edit.insert.as_str())
        || new_text.get(end..) != text.get(edit.range.end..)
    {
        return None;
    }
    let range = TextRange::new(
        TextSize::try_from(edit.range.start).ok()?,
        TextSize::try_from(edit.range.end).ok()?,
    );
    let root = base.syntax();
    let leaves = if range.is_empty() {
        root.token_at_offset(range.start()).collect::<Vec<_>>()
    } else {
        match root.covering_element(range) {
            NodeOrToken::Token(t) => vec![t],
            _ => vec![],
        }
    };
    for leaf in leaves {
        if !leaf.text_range().contains_range(range)
            || !matches!(leaf.kind(), SyntaxKind::WORD | SyntaxKind::NUMBER)
            || leaf
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::ENTRY_TYPE)
        {
            continue;
        }
        let start = usize::from(leaf.text_range().start());
        let old_end = usize::from(leaf.text_range().end());
        let delta = edit.insert.len() as isize - edit.range.len() as isize;
        let new_end = old_end.checked_add_signed(delta)?;
        let replacement = new_text.get(start..new_end)?;
        if replacement.len() > 8192 {
            continue;
        }
        let tokens = lex(replacement);
        if tokens.len() != 1 || tokens[0].kind != leaf.kind() {
            continue;
        }
        // Relex the complete adjacent leaves, so merging across either join is
        // detected, including a digit becoming a word. Their sizes are bounded.
        let prev = leaf.prev_token();
        let next = leaf.next_token();
        if prev.as_ref().is_some_and(|t| t.text().len() > 1024)
            || next.as_ref().is_some_and(|t| t.text().len() > 1024)
        {
            continue;
        }
        let mut probe = String::new();
        let mut expected = Vec::new();
        if let Some(t) = &prev {
            probe.push_str(t.text());
            expected.push(t.kind());
        }
        probe.push_str(replacement);
        expected.push(leaf.kind());
        if let Some(t) = &next {
            probe.push_str(t.text());
            expected.push(t.kind());
        }
        if lex(&probe).iter().map(|t| t.kind).collect::<Vec<_>>() != expected {
            continue;
        }
        let mut errors = Vec::with_capacity(base.errors.len());
        let mut valid = true;
        for error in &base.errors {
            // Errors address whole tokens or EOF. A partial overlap has no proof.
            let (a, b) = (error.start, error.end);
            let mapped = if b <= start {
                Some((a, b))
            } else if a >= old_end {
                a.checked_add_signed(delta).zip(b.checked_add_signed(delta))
            } else if a == start && b == old_end {
                Some((start, new_end))
            } else {
                None
            };
            if let Some((start, end)) = mapped {
                errors.push(SyntaxError {
                    start,
                    end,
                    message: error.message.clone(),
                });
            } else {
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }
        let green = leaf.replace_with(GreenToken::new(leaf.kind().into(), replacement));
        if usize::from(green.text_len()) != new_text.len() {
            return None;
        }
        let result = Parse { green, errors };
        #[cfg(debug_assertions)]
        {
            let full = super::parse(new_text);
            assert_eq!(result.green, full.green, "BibTeX leaf reparse tree");
            assert_eq!(result.errors, full.errors, "BibTeX leaf reparse errors");
        }
        return Some(result);
    }
    None
}
