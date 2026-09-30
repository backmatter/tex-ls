//! Literal names manufactured by well-defined declaration commands.
use super::*;
use crate::ast::Group;
use crate::syntax::SyntaxToken;

fn token_after(command: &SyntaxNode, after: TextSize) -> Option<SyntaxToken> {
    let mut token = command.first_token()?;
    while token.text_range().start() < after || is_trivia(token.kind()) {
        token = token.next_token()?;
    }
    Some(token)
}

fn group_after(command: &SyntaxNode, after: TextSize) -> Option<SyntaxNode> {
    let token = token_after(command, after)?;
    if token.kind() != SyntaxKind::L_BRACE {
        return None;
    }
    token.parent_ancestors().find(|node| {
        node.kind() == SyntaxKind::GROUP && node.text_range().start() == token.text_range().start()
    })
}

/// TeX parameter text runs to the first opening brace, including for delimited
/// arguments whose call signature we cannot represent. The CST may attach that
/// replacement group to a sibling rather than the literal-name definer.
fn replacement_group_after(command: &SyntaxNode, after: TextSize) -> Option<SyntaxNode> {
    let mut token = token_after(command, after)?;
    loop {
        match token.kind() {
            SyntaxKind::L_BRACE => {
                let group = group_after(command, token.text_range().start())?;
                return (group.last_token()?.kind() == SyntaxKind::R_BRACE).then_some(group);
            }
            SyntaxKind::R_BRACE => return None,
            _ => token = token.next_token()?,
        }
    }
}

fn literal_group(group: &SyntaxNode) -> Option<(SmolStr, TextRange)> {
    let (range, text) = Group::cast(group.clone())?.inner()?;
    let name = text.trim();
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\\' | '#' | '{' | '}' | '%'))
    {
        return None;
    }
    let start = range.start() + TextSize::new((text.len() - text.trim_start().len()) as u32);
    Some((
        name.into(),
        TextRange::at(start, TextSize::new(name.len() as u32)),
    ))
}

fn literal_tex_group(group: &SyntaxNode) -> Option<(SmolStr, TextRange)> {
    let (_, text) = Group::cast(group.clone())?.inner()?;
    // Unlike expl3 c arguments, spaces in an ordinary \csname are name bytes.
    if text.as_str() != text.trim() {
        return None;
    }
    literal_group(group)
}

pub(super) fn literal_alias_after(command: &SyntaxNode, after: TextSize) -> Option<SmolStr> {
    group_after(command, after)
        .and_then(|group| literal_group(&group))
        .map(|(name, _)| name)
}

fn literal_list(group: &SyntaxNode) -> Option<Vec<String>> {
    let (_, text) = Group::cast(group.clone())?.inner()?;
    if text
        .chars()
        .any(|c| matches!(c, '\\' | '#' | '{' | '}' | '%'))
    {
        return None;
    }
    Some(text.split(',').map(|item| item.trim().to_owned()).collect())
}

/// Consecutive undelimited #1..#9 parameters prove an argument count.
pub(super) fn parameter_arity(command: &SyntaxNode, after: TextSize) -> Option<usize> {
    parameter_arity_impl(command, after, true)
}

pub(super) fn primitive_parameter_arity(command: &SyntaxNode, site: &DefSite) -> Option<usize> {
    let head = control_word_range(command)?;
    let target = token_after(command, head.end())?;
    if target.text_range() != site.name_range
        || !matches!(
            target.kind(),
            SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL
        )
    {
        return None;
    }
    // TeX ignores spaces after a control word, but a space after a control
    // symbol or inside parameter text is a delimiter, not a brace argument.
    let after = if target.kind() == SyntaxKind::CONTROL_WORD {
        token_after(command, target.text_range().end())?
            .text_range()
            .start()
    } else {
        target.text_range().end()
    };
    parameter_arity_impl(command, after, false)
}

fn parameter_arity_impl(
    command: &SyntaxNode,
    after: TextSize,
    ignore_spaces: bool,
) -> Option<usize> {
    let mut token = command.first_token()?;
    while token.text_range().start() < after {
        token = token.next_token()?;
    }
    let mut parameters = String::new();
    loop {
        if token.kind() == SyntaxKind::L_BRACE {
            let arity = parameters.len() / 2;
            return (arity <= 9
                && parameters == (1..=arity).map(|i| format!("#{i}")).collect::<String>())
            .then_some(arity);
        }
        if !ignore_spaces && is_collapsible_trivia(token.kind()) {
            return None;
        }
        if !is_trivia(token.kind()) {
            // Stop at an unrelated command or structural boundary instead of
            // interpreting later document text as the declaration's body.
            if token.text().chars().any(|c| !matches!(c, '#' | '1'..='9')) {
                return None;
            }
            parameters.push_str(token.text());
        }
        token = token.next_token()?;
    }
}

pub(super) fn name_arity(name: &str) -> Option<usize> {
    use crate::semantic::expl3::{Expl3Slot, expl3_slots};
    let slots = expl3_slots(name)?;
    (!slots.contains(&Expl3Slot::ParameterText) && slots.len() <= 9).then_some(slots.len())
}

fn declaration(site: DefSite, arity: Option<usize>, alias: Option<SmolStr>) -> EditorDeclaration {
    let mut signatures = SignatureDb::default();
    if let Some(arity) = arity {
        signatures.insert_command(site.name.clone(), command_sig(latex2e_args(arity, false)));
    }
    EditorDeclaration {
        site,
        signatures,
        alias,
        alias_is_name_text: false,
    }
}

fn named_command(command: &SyntaxNode, group: &SyntaxNode) -> Option<DefSite> {
    let (name, range) = literal_group(group)?;
    Some(named_site(command, name, range, DefSiteKind::Command))
}

pub(super) fn declarations(command: &SyntaxNode, name: &str) -> Option<Vec<EditorDeclaration>> {
    let result = match name {
        "csdef" | "csgdef" | "csedef" | "csxdef" | "protected@csedef" | "protected@csxdef"
        | "cslet" | "csletcs" => {
            let group = nth_group(command, 0)?;
            let (target, range) = literal_tex_group(&group)?;
            let mut site = named_site(command, target, range, DefSiteKind::Command);
            let after = group.text_range().end();
            if !matches!(name, "cslet" | "csletcs")
                && let Some(body) = replacement_group_after(command, after)
            {
                site.range = TextRange::new(site.range.start(), body.text_range().end());
            }
            let alias = match name {
                "cslet" => next_control_name(command, after, false, true),
                "csletcs" => group_after(command, after)
                    .and_then(|group| literal_tex_group(&group))
                    .map(|(name, _)| name),
                _ => None,
            };
            let arity = (!matches!(name, "cslet" | "csletcs"))
                .then(|| parameter_arity_impl(command, after, false))
                .flatten();
            vec![EditorDeclaration {
                alias_is_name_text: name == "csletcs",
                ..declaration(site, arity, alias)
            }]
        }
        "letcs" => {
            let site = literal_command_def_site(command)?;
            let after = nth_group(command, 0)
                .filter(|group| group.text_range().contains_range(site.name_range))
                .map_or(site.name_range.end(), |group| group.text_range().end());
            let alias = group_after(command, after)
                .and_then(|group| literal_tex_group(&group))
                .map(|(name, _)| name);
            vec![EditorDeclaration {
                alias_is_name_text: true,
                ..declaration(site, None, alias)
            }]
        }
        "newcounter" => {
            let (counter, range) = literal_tex_group(&nth_group(command, 0)?)?;
            ["c@", "the", "p@", "cl@"]
                .into_iter()
                .map(|prefix| {
                    declaration(
                        named_site(
                            command,
                            format!("{prefix}{counter}").into(),
                            range,
                            DefSiteKind::Command,
                        ),
                        (prefix != "c@").then_some(0),
                        None,
                    )
                })
                .collect()
        }
        "cs_generate_variant:Nn" | "cs_generate_variant:cn" => variants(command, name)?,
        _ => {
            let (base, arguments) = name.split_once(':')?;
            if matches!(
                base,
                "prg_new_conditional"
                    | "prg_set_conditional"
                    | "prg_gset_conditional"
                    | "prg_new_protected_conditional"
                    | "prg_set_protected_conditional"
                    | "prg_gset_protected_conditional"
            ) && matches!(arguments, "Npnn" | "Nnn" | "cpnn" | "cnn")
            {
                conditionals(command, arguments, base.contains("_protected_"))?
            } else if variable_declarer(base, arguments) {
                let site = target(command, arguments)?.0;
                vec![declaration(site, None, None)]
            } else {
                return None;
            }
        }
    };
    Some(result)
}

fn target(command: &SyntaxNode, arguments: &str) -> Option<(DefSite, TextSize)> {
    if arguments.starts_with('c') {
        let group = nth_group(command, 0)?;
        Some((named_command(command, &group)?, group.text_range().end()))
    } else {
        let site = literal_command_def_site(command)?;
        let after = site.name_range.end();
        Some((site, after))
    }
}

fn variable_declarer(base: &str, arguments: &str) -> bool {
    let Some((kind, operation)) = base.split_once('_') else {
        return false;
    };
    let known_type = matches!(
        kind,
        "tl" | "str"
            | "seq"
            | "clist"
            | "prop"
            | "int"
            | "dim"
            | "skip"
            | "muskip"
            | "fp"
            | "bool"
            | "box"
            | "coffin"
            | "ior"
            | "iow"
            | "quark"
            | "scan"
    );
    known_type
        && match operation {
            "new" => matches!(arguments, "N" | "c"),
            "clear_new" | "gclear_new" => {
                matches!(kind, "tl" | "str" | "seq" | "clist" | "prop" | "box")
                    && matches!(arguments, "N" | "c")
            }
            "const" => {
                matches!(
                    kind,
                    "tl" | "str" | "clist" | "int" | "dim" | "skip" | "muskip" | "fp" | "bool"
                ) && matches!(arguments, "Nn" | "Ne" | "Nx" | "cn" | "ce" | "cx")
            }
            _ => false,
        }
}

fn variants(command: &SyntaxNode, name: &str) -> Option<Vec<EditorDeclaration>> {
    let (site, after) = target(command, name.split_once(':')?.1)?;
    let (base, original) = site.name.rsplit_once(':')?;
    if !original.is_ascii() {
        return None;
    }
    let group = group_after(command, after)?;
    let mut result = Vec::new();
    for variant in literal_list(&group)? {
        if variant.is_empty() || !variant.is_ascii() || variant.len() > original.len() {
            continue;
        }
        if !original.bytes().zip(variant.bytes()).all(|(from, to)| {
            from == to
                || (from == b'N' && to == b'c')
                || (from == b'n' && matches!(to, b'o' | b'e' | b'x' | b'V' | b'v' | b'f'))
        }) {
            continue;
        }
        let name: SmolStr = format!("{base}:{variant}{}", &original[variant.len()..]).into();
        let arity = name_arity(&name);
        result.push(declaration(
            DefSite {
                name,
                ..site.clone()
            },
            arity,
            None,
        ));
    }
    Some(result)
}

fn conditionals(
    command: &SyntaxNode,
    arguments: &str,
    protected: bool,
) -> Option<Vec<EditorDeclaration>> {
    let (site, after) = target(command, arguments)?;
    let (base, args) = site.name.rsplit_once(':')?;
    let mut token = token_after(command, after)?;
    let arity = if arguments.contains('p') {
        let arity = parameter_arity(command, after);
        while token.kind() != SyntaxKind::L_BRACE {
            if matches!(
                token.kind(),
                SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL | SyntaxKind::R_BRACE
            ) {
                return None;
            }
            token = token.next_token()?;
        }
        arity
    } else {
        name_arity(&site.name)
    };
    let group = group_after(command, token.text_range().start())?;
    let mut result = Vec::new();
    for kind in literal_list(&group)? {
        let (name, branches) = match kind.as_str() {
            "p" if !protected => (format!("{base}_p:{args}"), 0),
            "T" | "F" | "TF" => (format!("{base}:{args}{kind}"), kind.len()),
            _ => continue,
        };
        result.push(declaration(
            DefSite {
                name: name.into(),
                ..site.clone()
            },
            arity.map(|n| n + branches).filter(|n| *n <= 9),
            None,
        ));
    }
    Some(result)
}
