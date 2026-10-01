//! Editor declaration facts. These never supply parser or formatter inputs.
use super::*;

mod generated;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDeclaration {
    pub site: DefSite,
    pub signatures: SignatureDb,
    pub alias: Option<SmolStr>,
    /// The alias source is name text, such as an expl3 c argument, rather than
    /// a control-sequence token that ordinary command rename can update.
    pub alias_is_name_text: bool,
}

fn command_sig(args: Vec<ArgSpec>) -> CommandSig {
    CommandSig {
        args: args.into(),
        ..Default::default()
    }
}

/// Read a literal name in a braced group, rejecting expansion and parameter text.
fn literal_name(command: &SyntaxNode, index: usize) -> Option<(SmolStr, TextRange)> {
    let (range, text) = nth_group_inner(command, index)?;
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

fn named_site(
    command: &SyntaxNode,
    name: SmolStr,
    name_range: TextRange,
    kind: DefSiteKind,
) -> DefSite {
    DefSite {
        name,
        name_range,
        kind,
        range: command.text_range().cover(name_range),
    }
}

/// Additional declarations with statically named targets, including generated
/// conditional names whose declaration location is the original condition.
pub fn extra_declarations(command: &SyntaxNode) -> Vec<EditorDeclaration> {
    let Some(name) = command_name(command) else {
        return Vec::new();
    };
    if let Some(declarations) = generated::declarations(command, &name) {
        return declarations;
    }
    let mut signatures = SignatureDb::default();
    let mut alias = None;
    let mut alias_is_name_text = false;
    let site = match name.as_str() {
        "NewCommandCopy" | "RenewCommandCopy" | "DeclareCommandCopy" => {
            let Some(site) = literal_command_def_site(command) else {
                return Vec::new();
            };
            let braced_target = nth_group(command, 0)
                .is_some_and(|group| group.text_range().contains_range(site.name_range));
            alias = next_control_name(command, site.name_range.end(), braced_target, true);
            site
        }
        "newif" => {
            let Some(site) = literal_command_def_site(command) else {
                return Vec::new();
            };
            let Some(stem) = site.name.strip_prefix("if").filter(|name| !name.is_empty()) else {
                return Vec::new();
            };
            return [
                site.name.clone(),
                format!("{stem}true").into(),
                format!("{stem}false").into(),
            ]
            .into_iter()
            .map(|name| {
                let mut signatures = SignatureDb::default();
                signatures.insert_command(name.clone(), command_sig(Vec::new()));
                EditorDeclaration {
                    site: DefSite {
                        name,
                        ..site.clone()
                    },
                    signatures,
                    alias: None,
                    alias_is_name_text: false,
                }
            })
            .collect();
        }
        "newtheorem" | "declaretheorem" => {
            let Some((name, span)) = literal_name(command, 0) else {
                return Vec::new();
            };
            let mut sig = environment_sig(latex2e_args(1, true));
            sig.outline = Some(crate::semantic::signature::OutlineKind::Theorem);
            signatures.insert_environment(name.clone(), sig);
            named_site(command, name, span, DefSiteKind::Environment)
        }
        _ => {
            let Some((base, arguments)) = name.split_once(':') else {
                return Vec::new();
            };
            if !matches!(
                base,
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
                    | "cs_gset_protected_nopar"
                    | "cs_new_eq"
                    | "cs_set_eq"
                    | "cs_gset_eq"
            ) {
                return Vec::new();
            }
            let site = if arguments.starts_with('c') {
                let Some((name, range)) = literal_name(command, 0) else {
                    return Vec::new();
                };
                named_site(command, name, range, DefSiteKind::Command)
            } else if arguments.starts_with('N') {
                let Some(site) = literal_command_def_site(command) else {
                    return Vec::new();
                };
                site
            } else {
                return Vec::new();
            };
            if base.ends_with("_eq") {
                if arguments.ends_with('c') {
                    let after = if arguments.starts_with('c') {
                        nth_group(command, 0).map(|group| group.text_range().end())
                    } else {
                        Some(site.name_range.end())
                    };
                    alias = after.and_then(|after| generated::literal_alias_after(command, after));
                    alias_is_name_text = true;
                } else {
                    alias = next_control_name(
                        command,
                        site.name_range.end(),
                        arguments.starts_with('c'),
                        false,
                    );
                }
            } else if matches!(arguments, "Npn" | "Npx" | "Npe" | "cpn" | "cpx" | "cpe") {
                let after = if arguments.starts_with('c') {
                    nth_group(command, 0).map(|group| group.text_range().end())
                } else {
                    Some(site.name_range.end())
                };
                if let Some(arity) =
                    after.and_then(|after| generated::parameter_arity(command, after))
                {
                    signatures
                        .insert_command(site.name.clone(), command_sig(latex2e_args(arity, false)));
                }
            } else if matches!(arguments, "Nn" | "Nx" | "Ne" | "cn" | "cx" | "ce")
                && let Some(arity) = generated::name_arity(&site.name)
            {
                signatures
                    .insert_command(site.name.clone(), command_sig(latex2e_args(arity, false)));
            }
            site
        }
    };
    vec![EditorDeclaration {
        site,
        signatures,
        alias,
        alias_is_name_text,
    }]
}

fn next_control_name(
    command: &SyntaxNode,
    after: TextSize,
    skip_close: bool,
    allow_braced: bool,
) -> Option<SmolStr> {
    let mut token = command.first_token()?;
    while token.text_range().start() < after {
        token = token.next_token()?;
    }
    if skip_close {
        while is_trivia(token.kind()) {
            token = token.next_token()?;
        }
        if token.text() != "}" {
            return None;
        }
        token = token.next_token()?;
    }
    while is_trivia(token.kind()) || (!allow_braced && token.text() == "=") {
        token = token.next_token()?;
    }
    let braced = allow_braced && token.kind() == SyntaxKind::L_BRACE;
    if braced {
        token = token.next_token()?;
        while is_trivia(token.kind()) {
            token = token.next_token()?;
        }
    }
    if !matches!(
        token.kind(),
        SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL
    ) {
        return None;
    }
    if braced {
        let mut close = token.next_token()?;
        while is_trivia(close.kind()) {
            close = close.next_token()?;
        }
        if close.kind() != SyntaxKind::R_BRACE {
            return None;
        }
    }
    token.text().strip_prefix('\\').map(Into::into)
}

pub fn editor_declarations(root: &SyntaxNode) -> Vec<EditorDeclaration> {
    let mut declarations = Vec::new();
    for command in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::COMMAND)
    {
        let Some(name) = command_name(&command) else {
            continue;
        };
        let extra = extra_declarations(&command);
        if !extra.is_empty() {
            declarations.extend(extra);
            continue;
        }
        let mut signatures = SignatureDb::default();
        let mut bodies = HashMap::new();
        let mut aliases = HashMap::new();
        let mut env_bodies = HashMap::new();
        match DefKind::of(&name) {
            Some(DefKind::Command) => {
                scan_newcommand(&command, &mut signatures, &mut bodies, &mut aliases)
            }
            Some(DefKind::Def) => {
                if let Some(site) = literal_command_def_site(&command)
                    && generated::primitive_parameter_arity(&command, &site).is_some()
                {
                    scan_def(&command, &mut signatures, &mut bodies, &mut aliases);
                }
            }
            Some(DefKind::XparseCommand) => {
                if let Some(def) = resolve_command_def(&command)
                    && let Some(spec) = nth_group(&def.host, def.first_arg_group)
                    && let Some(args) = xparse::parse_editor_spec(&group_inner_source(&spec))
                {
                    scan_xparse_command(&command, &mut signatures, &mut bodies, &mut aliases);
                    if let Some(mut signature) = signatures.command(&def.name).cloned() {
                        signature.args = args.into();
                        signatures.insert_command(def.name, signature);
                    }
                }
            }
            Some(DefKind::Environment) => {
                scan_newenvironment(&command, &mut signatures, &mut env_bodies)
            }
            Some(DefKind::XparseEnvironment) => {
                if let Some(spec) = nth_group(&command, 1)
                    && let Some(args) = xparse::parse_editor_spec(&group_inner_source(&spec))
                {
                    scan_xparse_environment(&command, &mut signatures, &mut env_bodies);
                    if let Some(site) = ordinary_definition_site(&command)
                        && let Some(mut signature) = signatures.environment(&site.name).cloned()
                    {
                        signature.args = args.into();
                        signatures.insert_environment(site.name, signature);
                    }
                }
            }
            Some(DefKind::VerbatimEnvironment) => {
                scan_verbatim_environment(&name, &command, &mut signatures)
            }
            None => {}
        }
        // These declarers have a fixed public argument contract even when
        // their implementations manufacture internal control sequences.
        if let Some(site) = ordinary_definition_site(&command) {
            let arity = match name.as_str() {
                "DeclareTextFontCommand"
                | "DeclareMathAlphabet"
                | "DeclareTextAccent"
                | "DeclareMathAccent"
                | "DeclareMathRadical" => Some(1),
                "DeclareTextSymbol"
                | "DeclareMathSymbol"
                | "DeclareMathDelimiter"
                | "DeclareMathOperator"
                | "DeclareOldFontCommand" => Some(0),
                _ => None,
            };
            if let Some(arity) = arity {
                signatures
                    .insert_command(site.name.clone(), command_sig(latex2e_args(arity, false)));
            }
            if matches!(
                name.as_str(),
                "DeclareTextCommand"
                    | "ProvideTextCommand"
                    | "DeclareTextCommandDefault"
                    | "ProvideTextCommandDefault"
            ) {
                let (arity, optional) = newcommand_arity(&command);
                signatures.insert_command(site.name, command_sig(latex2e_args(arity, optional)));
            }
        }
        apply_verbatim_flags(&mut signatures, &bodies);
        apply_verbatim_env_flags(&mut signatures, &env_bodies, &bodies);
        // Site scanning includes known declarations with an unknown argument shape.
        if let Some(site) = ordinary_definition_site(&command) {
            let alias = if name == "let" {
                next_control_name(&command, site.name_range.end(), false, false)
            } else {
                None
            };
            declarations.push(EditorDeclaration {
                site,
                signatures,
                alias,
                alias_is_name_text: false,
            });
        }
    }
    declarations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn command_copies_accept_each_literal_argument_form() {
        for definer in ["NewCommandCopy", "RenewCommandCopy", "DeclareCommandCopy"] {
            for target in [r"\copy", r"{\copy}"] {
                for (original, alias) in [
                    (r"\original", "original"),
                    (r"{\original}", "original"),
                    (r"\%", "%"),
                    (r"{\%}", "%"),
                    (r"\\", "\\"),
                    (r"{\\}", "\\"),
                    (r"{\csname}", "csname"),
                ] {
                    let source = format!("\\{definer}{target}{original}");
                    let declarations = editor_declarations(&parse(&source).syntax());
                    assert_eq!(declarations.len(), 1, "{source}");
                    assert_eq!(declarations[0].site.name, "copy", "{source}");
                    assert_eq!(declarations[0].alias.as_deref(), Some(alias), "{source}");
                }
            }
        }
    }

    #[test]
    fn command_copies_reject_nonliteral_source_arguments() {
        for source in [r"{\original\other}", r"{plain}", r"{{\original}}"] {
            let source = format!("\\NewCommandCopy{{\\copy}}{source}");
            let declarations = editor_declarations(&parse(&source).syntax());
            assert_eq!(declarations.len(), 1, "{source}");
            assert_eq!(declarations[0].alias, None, "{source}");
        }
    }

    #[test]
    fn literal_generated_declarations_have_proved_signatures() {
        for (source, name, arity, alias) in [
            (r"\csdef{audiname}#1#2{#1#2}", "audiname", Some(2), None),
            (r"\csedef{audiname}#1{#1}", "audiname", Some(1), None),
            (r"\csdef{audiname}#1stop{#1}", "audiname", None, None),
            (
                r"\cslet{audiname}{\textbf}",
                "audiname",
                None,
                Some("textbf"),
            ),
            (
                r"\csletcs{audiname}{textbf}",
                "audiname",
                None,
                Some("textbf"),
            ),
            (r"\letcs\audiname{textbf}", "audiname", None, Some("textbf")),
            (
                r"\letcs{\audiname}{textbf}",
                "audiname",
                None,
                Some("textbf"),
            ),
            (r"\newcounter{audicount}", "theaudicount", Some(0), None),
            (
                r"\ExplSyntaxOn\tl_new:N \l_audi_tl",
                "l_audi_tl",
                None,
                None,
            ),
            (
                r"\ExplSyntaxOn\int_new:c {g_audi_int}",
                "g_audi_int",
                None,
                None,
            ),
            (
                r"\ExplSyntaxOn\tl_const:Nn \c_audi_tl {text}",
                "c_audi_tl",
                None,
                None,
            ),
            (
                r"\ExplSyntaxOn\cs_new:Nn \audi:nn {#1#2}",
                "audi:nn",
                Some(2),
                None,
            ),
            (
                r"\ExplSyntaxOn\cs_new:Npe \audi:n #1 {#1}",
                "audi:n",
                Some(1),
                None,
            ),
            (
                r"\ExplSyntaxOn\cs_new:cpn {audi:n} #1 {#1}",
                "audi:n",
                Some(1),
                None,
            ),
            (
                r"\ExplSyntaxOn\cs_generate_variant:Nn \audi:nn { V, ne }",
                "audi:Vn",
                Some(2),
                None,
            ),
            (
                r"\ExplSyntaxOn\cs_generate_variant:cn {audi:Nn} { cV }",
                "audi:cV",
                Some(2),
                None,
            ),
            (
                r"\ExplSyntaxOn\prg_new_conditional:Npnn \audi:n #1 {p,T,F,TF} {\prg_return_true:}",
                "audi:nTF",
                Some(3),
                None,
            ),
            (
                r"\ExplSyntaxOn\prg_new_conditional:Nnn \audi:n {p,T,F,TF} {\prg_return_true:}",
                "audi_p:n",
                Some(1),
                None,
            ),
        ] {
            let parsed = parse(source);
            assert_eq!(parsed.syntax().text().to_string(), source);
            let declarations = editor_declarations(&parsed.syntax());
            let declaration = declarations
                .iter()
                .find(|d| d.site.name == name)
                .unwrap_or_else(|| panic!("missing {name} in {source}: {declarations:?}"));
            assert_eq!(
                declaration.signatures.command(name).map(|s| s.args.len()),
                arity,
                "{source}"
            );
            assert_eq!(declaration.alias.as_deref(), alias, "{source}");
            assert!(
                declaration
                    .site
                    .range
                    .contains_range(declaration.site.name_range)
            );
        }
    }

    #[test]
    fn generated_declarations_reject_dynamic_names_and_invalid_variants() {
        for source in [
            r"\csdef{audi\suffix}#1{#1}",
            r"\newcounter{audi#1}",
            r"\ExplSyntaxOn\tl_new:c {audi\suffix}",
            r"\ExplSyntaxOn\cs_generate_variant:Nn \audi:n { c, nnn, \dynamic }",
            r"\ExplSyntaxOn\cs_generate_variant:Nn \audi:N { V }",
            r"\ExplSyntaxOn\prg_new_conditional:Nnn \audi:n {\dynamic} {body}",
        ] {
            assert!(
                editor_declarations(&parse(source).syntax()).is_empty(),
                "{source}"
            );
        }
        let declarations = editor_declarations(
            &parse(r"\ExplSyntaxOn\prg_new_protected_conditional:Nnn \audi:n {p,TF} {body}")
                .syntax(),
        );
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].site.name, "audi:nTF");
    }

    #[test]
    fn literal_definitions_cover_replacement_bodies_without_extending_past_them() {
        for definer in [
            "csdef",
            "csgdef",
            "csedef",
            "csxdef",
            "protected@csedef",
            "protected@csxdef",
        ] {
            for parameters in ["", "#1", "#1#2", "#1stop", "#1 #2", "#1% comment\n#2"] {
                let definition =
                    format!("\\{definer}{{audiname}}{parameters}{{\\unknowninside{{nested}}}}");
                let source = format!("\\makeatletter\n{definition}\n\\unknownoutside{{value}}\n");
                let root = parse(&source).syntax();
                assert_eq!(root.text().to_string(), source);
                let declarations = editor_declarations(&root);
                let declaration = declarations
                    .iter()
                    .find(|d| d.site.name == "audiname")
                    .unwrap();
                assert_eq!(&source[declaration.site.range], definition, "{source}");
                assert_eq!(&source[declaration.site.name_range], "audiname");
            }
        }
        let source = "\\cslet{audiname}\\textbf\n\\unknownoutside{value}";
        let declaration = editor_declarations(&parse(source).syntax()).remove(0);
        assert!(
            !declaration
                .site
                .range
                .contains(TextSize::new(source.find("unknownoutside").unwrap() as u32))
        );
    }

    #[test]
    fn primitive_definitions_with_unproved_parameters_have_no_editor_signature() {
        for definer in ["def", "gdef", "edef", "xdef"] {
            for target in [r"\section", r"\audiprimitive", r"\%", r"\\"] {
                for parameters in ["#1;", "#1stop", r"#1\stop", "#1 #2", "#2", "#1#1"] {
                    let source = format!("\\{definer}{target}{parameters}{{#1}}");
                    let root = parse(&source).syntax();
                    assert_eq!(root.text().to_string(), source);
                    let name = target.strip_prefix('\\').unwrap();
                    let declarations = editor_declarations(&root);
                    let declaration = declarations.iter().find(|d| d.site.name == name).unwrap();
                    assert!(declaration.signatures.command(name).is_none(), "{source}");
                    assert_eq!(&source[declaration.site.name_range], target);
                }
            }
        }
        for (source, count) in [
            (r"\def\audiprimitive{}", 0),
            (r"\def\audiprimitive #1{#1}", 1),
            (r"\def\audiprimitive#1#2{#1#2}", 2),
        ] {
            let declaration = editor_declarations(&parse(source).syntax()).remove(0);
            assert_eq!(
                declaration
                    .signatures
                    .command("audiprimitive")
                    .unwrap()
                    .args
                    .len(),
                count,
                "{source}"
            );
        }
        // The formatter's existing projection remains independent of the
        // stricter editor signature contract.
        assert_eq!(
            scan_definitions(&parse(r"\def\section#1;{#1}").syntax())
                .command("section")
                .unwrap()
                .args
                .len(),
            1
        );
    }
}
