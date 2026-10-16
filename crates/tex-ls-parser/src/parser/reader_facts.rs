//! Closed source-reader facts shared by lexical and declaration ownership.
use crate::semantic::signature::builtin;
use crate::syntax::SyntaxKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pending {
    Delim,
    Def,
    CharConstant,
    LiteralToken,
}

/// The LaTeX2e and xparse command-definition families. Unlike environment
/// definitions and hooks, these consume a control sequence as their name, so a
/// control-symbol name must remain data rather than opening live syntax.
pub(crate) fn is_command_definition_command(text: &str) -> bool {
    matches!(
        text,
        "\\newcommand"
            | "\\renewcommand"
            | "\\providecommand"
            | "\\DeclareRobustCommand"
            | "\\newrobustcmd"
            | "\\renewrobustcmd"
            | "\\providerobustcmd"
            | "\\NewDocumentCommand"
            | "\\NewExpandableDocumentCommand"
            | "\\RenewExpandableDocumentCommand"
            | "\\ProvideExpandableDocumentCommand"
            | "\\DeclareExpandableDocumentCommand"
            | "\\NewCommandCopy"
            | "\\RenewCommandCopy"
            | "\\DeclareCommandCopy"
            | "\\RenewDocumentCommand"
            | "\\ProvideDocumentCommand"
            | "\\DeclareDocumentCommand"
    )
}

/// Returns whether a control word introduces a definition.
pub(crate) fn is_definition_keyword(text: &str) -> bool {
    is_command_definition_command(text)
        || matches!(
            text,
            "\\def" | "\\edef" | "\\gdef" | "\\xdef" | "\\let" | "\\newif"
        )
}

/// Returns the number of following control words claimed as definition names.
pub(crate) fn definition_name_slots(text: &str) -> u8 {
    match text {
        "\\let" | "\\NewCommandCopy" | "\\RenewCommandCopy" | "\\DeclareCommandCopy" => 2,
        _ if is_definition_keyword(text) => 1,
        _ => text
            .strip_prefix('\\')
            .map_or(0, literal_definition_name_slots),
    }
}

/// Supported declarations that read a control-sequence name as a literal token.
pub(crate) fn literal_definition_name_slots(name: &str) -> u8 {
    if matches!(
        name,
        "chardef"
            | "mathchardef"
            | "countdef"
            | "dimendef"
            | "skipdef"
            | "muskipdef"
            | "toksdef"
            | "font"
            | "newcount"
            | "newdimen"
            | "newskip"
            | "newmuskip"
            | "newtoks"
            | "newbox"
            | "newread"
            | "newwrite"
            | "newlength"
            | "newsavebox"
            | "DeclareMathSymbol"
            | "DeclareMathDelimiter"
            | "DeclareMathAccent"
            | "DeclareMathOperator"
            | "DeclareMathAlphabet"
            | "DeclareMathRadical"
            | "DeclareTextFontCommand"
            | "DeclareOldFontCommand"
            | "DeclareTextCommand"
            | "ProvideTextCommand"
            | "DeclareTextCommandDefault"
            | "ProvideTextCommandDefault"
            | "DeclareTextSymbol"
            | "DeclareTextAccent"
    ) {
        return 1;
    }
    match expl3_definition_args(name) {
        Some(("NN", true)) => 2,
        Some((args, _)) if args.starts_with('N') => 1,
        _ => 0,
    }
}

pub(crate) fn expl3_definition_args(name: &str) -> Option<(&str, bool)> {
    let (base, args) = name.split_once(':')?;
    match base {
        "cs_new"
        | "cs_set"
        | "cs_gset"
        | "cs_new_protected"
        | "cs_set_protected"
        | "cs_gset_protected"
        | "cs_new_nopar"
        | "cs_set_nopar"
        | "cs_gset_nopar"
        | "cs_new_protected_nopar"
        | "cs_set_protected_nopar"
        | "cs_gset_protected_nopar" => Some((args, false)),
        "cs_new_eq" | "cs_set_eq" | "cs_gset_eq" => Some((args, true)),
        _ => None,
    }
}

pub(super) fn primitive_replacement_reader(name: &str) -> bool {
    matches!(name, "def" | "gdef" | "edef" | "xdef")
        || expl3_definition_args(name).is_some_and(|(args, copy)| {
            !copy && matches!(args, "Npn" | "Npx" | "Npe" | "Nn" | "Nx" | "Ne")
        })
}

pub(crate) fn primitive_operand_reader(name: &str) -> bool {
    matches!(
        name,
        "meaning"
            | "cmd"
            | "show"
            | "noexpand"
            | "futurelet"
            | "let"
            | "NewCommandCopy"
            | "RenewCommandCopy"
            | "DeclareCommandCopy"
            | "ifx"
            | "ifdefined"
    ) || literal_definition_name_slots(name) > 0
}

pub(crate) fn token_list_reader(name: &str) -> bool {
    matches!(name, "detokenize" | "DoNotIndex")
}

pub(crate) fn raw_builtin_reader(name: &str) -> bool {
    name == "verb"
        || builtin()
            .command(name)
            .is_some_and(|sig| sig.verbatim || sig.args.iter().any(|arg| arg.verbatim))
}

fn is_literal_token_command(text: &str) -> bool {
    matches!(
        text,
        "\\string" | "\\noexpand" | "\\meaning" | "\\expandafter" | "\\show"
    )
}

fn is_char_constant_command(text: &str) -> bool {
    matches!(
        text,
        "\\char"
            | "\\catcode"
            | "\\lccode"
            | "\\uccode"
            | "\\sfcode"
            | "\\mathcode"
            | "\\delcode"
            | "\\number"
            | "\\the"
            | "\\romannumeral"
            | "\\numexpr"
            | "\\dimexpr"
            | "\\ifnum"
            | "\\ifodd"
            | "\\ifdim"
    )
}

/// Heads whose bounded headers or curated macro arguments are scanned before
/// their tokens execute. Source overrides are ordered, unlike project hints.
pub(crate) fn stored_definition_reader(name: &str) -> bool {
    primitive_replacement_reader(name)
        || declaration_header(name).is_some()
        || builtin()
            .command(name)
            .is_some_and(|sig| !sig.args.is_empty())
}

pub(super) fn declaration_header(name: &str) -> Option<(u8, u8, bool)> {
    match name {
        "newcommand"
        | "renewcommand"
        | "providecommand"
        | "DeclareRobustCommand"
        | "newrobustcmd"
        | "renewrobustcmd"
        | "providerobustcmd" => Some((1, 2, false)),
        "newenvironment" | "renewenvironment" | "provideenvironment" => Some((2, 2, true)),
        "NewDocumentCommand"
        | "RenewDocumentCommand"
        | "ProvideDocumentCommand"
        | "DeclareDocumentCommand"
        | "NewExpandableDocumentCommand"
        | "RenewExpandableDocumentCommand"
        | "ProvideExpandableDocumentCommand"
        | "DeclareExpandableDocumentCommand" => Some((2, 0, false)),
        "NewDocumentEnvironment"
        | "RenewDocumentEnvironment"
        | "ProvideDocumentEnvironment"
        | "DeclareDocumentEnvironment" => Some((3, 0, true)),
        _ => None,
    }
}

pub(crate) fn short_verb_observer(name: &str) -> bool {
    matches!(
        name,
        "MakeShortVerb"
            | "DefineShortVerb"
            | "DeleteShortVerb"
            | "UndefineShortVerb"
            | "documentclass"
            | "LoadClass"
    )
}

pub(crate) fn reads_following_text(text: &str) -> bool {
    matches!(text, "\\DoNotIndex" | "\\catcode")
        || text
            .strip_prefix('\\')
            .is_some_and(|name| short_verb_observer(name) || primitive_operand_reader(name))
        || next_pending(None, SyntaxKind::CONTROL_WORD, text).is_some()
}

pub(super) fn next_pending(
    pending: Option<Pending>,
    kind: SyntaxKind,
    text: &str,
) -> Option<Pending> {
    if kind == SyntaxKind::CONTROL_WORD {
        return if text == "\\left" || text == "\\right" {
            Some(Pending::Delim)
        } else if is_definition_keyword(text) {
            Some(Pending::Def)
        } else if is_char_constant_command(text) {
            Some(Pending::CharConstant)
        } else if is_literal_token_command(text) {
            Some(Pending::LiteralToken)
        } else {
            None
        };
    }
    match pending? {
        p @ (Pending::Delim | Pending::Def)
            if matches!(kind, SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) =>
        {
            Some(p)
        }
        Pending::Def if kind == SyntaxKind::L_BRACE => Some(Pending::Def),
        p @ (Pending::CharConstant | Pending::LiteralToken) if kind == SyntaxKind::WHITESPACE => {
            Some(p)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pending_arming_sets_are_disjoint() {
        for name in [
            "\\left",
            "\\right",
            "\\newcommand",
            "\\renewcommand",
            "\\providecommand",
            "\\DeclareRobustCommand",
            "\\NewDocumentCommand",
            "\\RenewDocumentCommand",
            "\\ProvideDocumentCommand",
            "\\DeclareDocumentCommand",
            "\\def",
            "\\edef",
            "\\gdef",
            "\\xdef",
            "\\let",
            "\\char",
            "\\catcode",
            "\\lccode",
            "\\uccode",
            "\\sfcode",
            "\\mathcode",
            "\\delcode",
            "\\number",
            "\\the",
            "\\romannumeral",
            "\\numexpr",
            "\\dimexpr",
            "\\ifnum",
            "\\ifodd",
            "\\ifdim",
            "\\string",
            "\\noexpand",
            "\\meaning",
            "\\expandafter",
            "\\show",
        ] {
            let armed = [
                name == "\\left" || name == "\\right",
                is_definition_keyword(name),
                is_char_constant_command(name),
                is_literal_token_command(name),
            ];
            assert_eq!(
                armed.iter().filter(|&&x| x).count(),
                1,
                "{name} arms {armed:?} — the `Pending` slot needs disjoint sets"
            );
        }
    }
}
