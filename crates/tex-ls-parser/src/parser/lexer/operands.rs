//! Bounded ownership of literal control-sequence operands.
use super::ParseCtx;
use crate::parser::reader_facts::{declaration_header, definition_name_slots};
use crate::syntax::SyntaxKind;

/// Literal control-sequence operands do not execute their spelling. This
/// bounded scan covers declaration names, copies, direct token inspection and
/// ltxdoc's `\cmd` literal command-name printer.
/// Futurelet's destination is an operand; its following tokens execute again.
#[derive(Debug, Default)]
pub(crate) struct PrimitiveOperandScan {
    remaining: u8,
    braced: bool,
    let_equals: bool,
    compared: bool,
    skip_spaces: bool,
    comment_line: bool,
    star: bool,
}

impl PrimitiveOperandScan {
    pub(super) fn has_operand(&self) -> bool {
        self.remaining > 0
    }

    pub(crate) fn observe(&mut self, kind: SyntaxKind, text: &str, ctx: &ParseCtx) -> bool {
        self.observe_at(kind, text, ctx, 0)
    }

    pub(crate) fn observe_at(
        &mut self,
        kind: SyntaxKind,
        text: &str,
        ctx: &ParseCtx,
        offset: usize,
    ) -> bool {
        if self.compared && self.remaining > 0 {
            if kind == SyntaxKind::COMMENT {
                self.comment_line = true;
                return false;
            }
            if kind == SyntaxKind::NEWLINE && self.comment_line {
                self.comment_line = false;
                self.skip_spaces = true;
                return false;
            }
            if matches!(kind, SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) {
                if !self.skip_spaces {
                    self.remaining -= 1;
                    self.skip_spaces = true;
                }
                return false;
            }
            if matches!(kind, SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD) {
                return false;
            }
            self.comment_line = false;
            let control = matches!(kind, SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL);
            // WORD coalesces source characters, but ifx compares exactly two
            // tokens. A run of two ordinary characters exhausts both operands.
            let count = if kind == SyntaxKind::WORD {
                text.chars().take(self.remaining as usize).count() as u8
            } else {
                1
            };
            self.remaining = self.remaining.saturating_sub(count);
            self.skip_spaces = kind == SyntaxKind::CONTROL_WORD;
            return control;
        }
        if crate::syntax::is_trivia(kind)
            || matches!(kind, SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD)
        {
            return false;
        }
        if self.remaining > 0 {
            if self.star {
                self.star = false;
                if kind == SyntaxKind::WORD && text == "*" {
                    return false;
                }
            }
            if self.braced && matches!(kind, SyntaxKind::L_BRACE | SyntaxKind::R_BRACE) {
                return false;
            }
            if self.let_equals
                && self.remaining == 1
                && kind == SyntaxKind::WORD
                && text.starts_with('=')
            {
                self.let_equals = false;
                if text.len() > 1 {
                    self.remaining = 0;
                }
                return false;
            }
            let operand = matches!(kind, SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL);
            if operand {
                self.remaining -= 1;
            } else {
                self.remaining = 0;
            }
            return operand;
        }
        if kind == SyntaxKind::CONTROL_WORD
            && !text
                .get(1..)
                .is_some_and(|name| ctx.is_reader_suppressed_at(name, offset))
        {
            let name = text.get(1..).unwrap_or_default();
            self.remaining = match name {
                "meaning" | "show" | "noexpand" | "futurelet" | "ifdefined" | "cmd" => 1,
                "let" | "NewCommandCopy" | "RenewCommandCopy" | "DeclareCommandCopy" => 2,
                "ifx" => 2,
                _ => definition_name_slots(text),
            };
            self.braced = name == "cmd"
                || (definition_name_slots(text) > 0 && name != "let")
                || matches!(
                    name,
                    "NewCommandCopy" | "RenewCommandCopy" | "DeclareCommandCopy"
                );
            self.let_equals = name == "let";
            self.compared = name == "ifx";
            self.skip_spaces = true;
            self.comment_line = false;
            self.star = name == "DeclareMathOperator"
                || declaration_header(name)
                    .is_some_and(|(_, optional, environment)| optional > 0 && !environment);
        }
        false
    }
}
