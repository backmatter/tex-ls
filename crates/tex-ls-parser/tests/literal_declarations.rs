use tex_ls_parser::parser::{lex, parse};
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxKind;

#[test]
fn supported_literal_declarations_do_not_execute_raw_definees() {
    for declaration in [
        r"\chardef\verb=65",
        r"\mathchardef\verb=65",
        r"\countdef\verb=0",
        r"\dimendef\verb=0",
        r"\skipdef\verb=0",
        r"\muskipdef\verb=0",
        r"\toksdef\verb=0",
        r"\font\verb=cmr10",
        r"\newcount\verb",
        r"\newdimen\verb",
        r"\newskip\verb",
        r"\newmuskip\verb",
        r"\newtoks\verb",
        r"\newbox\verb",
        r"\newread\verb",
        r"\newwrite\verb",
        r"\newlength{\verb}",
        r"\newsavebox{\verb}",
        r"\DeclareMathSymbol{\verb}{\mathord}{letters}{42}",
        r"\DeclareMathDelimiter{\verb}{\mathopen}{letters}{42}{letters}{42}",
        r"\DeclareMathAccent{\verb}{\mathalpha}{letters}{42}",
        r"\DeclareMathOperator{\verb}{name}",
        r"\DeclareMathOperator*{\verb}{name}",
        r"\DeclareMathAlphabet{\verb}{OT1}{cmr}{m}{n}",
        r"\DeclareMathRadical{\verb}{letters}{42}{letters}{42}",
        r"\DeclareTextFontCommand{\verb}{\bfseries}",
        r"\DeclareOldFontCommand{\verb}{\bfseries}{\mathbf}",
        r"\DeclareTextCommand{\verb}{T1}[1]{#1}",
        r"\ProvideTextCommand{\verb}{T1}[1]{#1}",
        r"\DeclareTextCommandDefault{\verb}[1]{#1}",
        r"\ProvideTextCommandDefault{\verb}[1]{#1}",
        r"\DeclareTextSymbol{\verb}{T1}{42}",
        r"\DeclareTextAccent{\verb}{T1}{42}",
    ] {
        let source = format!("{declaration}\n\\ref{{live}}\n");
        let tokens = lex(&source);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            source
        );
        assert!(
            !tokens.iter().any(|token| token.kind == SyntaxKind::VERB),
            "{declaration}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(
            parsed.errors.is_empty(),
            "{declaration}: {:?}",
            parsed.errors
        );
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{declaration}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn literal_declaration_heads_respect_source_shadowing_and_inert_reads() {
    for source in [
        r"\let\newlength\relax\newlength{\verb|active|}\ref{live}",
        r"\let\DeclareMathOperator\relax\DeclareMathOperator{\verb|active|}\ref{live}",
        r"\meaning\DeclareMathOperator{\verb|active|}\ref{live}",
        r"\string\newlength{\verb|active|}\ref{live}",
        r"\let\copy\newlength{\verb|active|}\ref{live}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::VERB && token.text() == r"\verb|active|"),
            "{source}"
        );
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn supported_expl3_literal_n_definees_and_copies_are_inert() {
    for base in [
        "cs_new",
        "cs_set",
        "cs_gset",
        "cs_new_protected",
        "cs_set_protected",
        "cs_gset_protected",
        "cs_new_nopar",
        "cs_set_nopar",
        "cs_gset_nopar",
        "cs_new_protected_nopar",
        "cs_set_protected_nopar",
        "cs_gset_protected_nopar",
    ] {
        let source =
            format!("\\ExplSyntaxOn\\{base}:Npn\\verb#1{{#1}}\\ExplSyntaxOff\\ref{{live}}");
        assert_literal_n_definee(&source);
    }
    for base in ["cs_new_eq", "cs_set_eq", "cs_gset_eq"] {
        for operands in [r"\verb\original", r"\alias\verb"] {
            let source = format!("\\ExplSyntaxOn\\{base}:NN{operands}\\ExplSyntaxOff\\ref{{live}}");
            assert_literal_n_definee(&source);
        }
    }
}

fn assert_literal_n_definee(source: &str) {
    let tokens = lex(source);
    assert_eq!(
        tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>(),
        source
    );
    assert!(
        !tokens.iter().any(|token| token.kind == SyntaxKind::VERB),
        "{source}"
    );
    let parsed = parse(source);
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1, "{source}");
    assert_eq!(model.refs()[0].name, "live");
}

#[test]
fn expl3_c_names_and_shadowed_n_heads_keep_ordinary_readers() {
    for source in [
        r"\ExplSyntaxOn\cs_set:cpn{\string\verb}{x}\ExplSyntaxOff\ref{live}",
        r"\ExplSyntaxOn\cs_set_eq:Nc\alias{\string\verb}\ExplSyntaxOff\ref{live}",
    ] {
        let tokens = lex(source);
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::STRINGIFIED_TOKEN && token.text == r"\verb"),
            "{source}"
        );
        assert_literal_n_definee(source);
    }
    for source in [
        r"\ExplSyntaxOn\let\cs_set:Npn\relax\cs_set:Npn\verb|active|\ExplSyntaxOff\ref{live}",
        r"\ExplSyntaxOn\meaning\cs_new:Npn\verb|active|\ExplSyntaxOff\ref{live}",
        r"\ExplSyntaxOn\string\cs_new:Npn\verb|active|\ExplSyntaxOff\ref{live}",
        r"\ExplSyntaxOn\cs_set_eq:NN\copy\cs_new:Npn\verb|active|\ExplSyntaxOff\ref{live}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::VERB && token.text() == r"\verb|active|"),
            "{source}"
        );
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn expl3_known_replacement_forms_preserve_structural_string_braces() {
    for head in [
        "cs_new:Npn",
        "cs_set:Npn",
        "cs_gset_protected:Npn",
        "cs_new:Nn",
        "cs_set:Npx",
        "cs_set:Npe",
    ] {
        let source = format!("\\ExplSyntaxOn\\{head}\\foo{{\\string}}\\ExplSyntaxOff\\ref{{live}}");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn bare_verb_heads_cannot_consume_stored_replacement_closers() {
    for source in [
        r"\def\foo{\verb}\ref{live}",
        r"\def\foo{\verb*}\ref{live}",
        r"\def\MakeShortVerb{\@ifstar{\def\shortverb{\verb*}\next}{\def\shortverb{\verb}\next}}\ref{live}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(
            !parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::VERB),
            "{source}"
        );
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live");
    }
    // Existing source-delimited raw readers inside a body remain available.
    let source = r"\def\foo{\verb|safe|}\ref{live}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert!(
        parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| token.kind() == SyntaxKind::VERB && token.text() == r"\verb|safe|")
    );
}

#[test]
fn raw_builtin_readers_honor_ordered_source_alias_overrides() {
    for (name, before, before_body) in [
        ("verb", r"\verb|literal|", r"\verb|literal|"),
        ("code", r"\code{literal}", "{literal}"),
        ("url", r"\url|literal|", "|literal|"),
        (
            "href",
            r"\href{https://example.test}{literal}",
            "{https://example.test}",
        ),
    ] {
        let source = format!("{before}\\let\\{name}\\relax\\{name}{{a_b}}\\ref{{live}}");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let tokens: Vec<_> = parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .collect();
        let raw: Vec<_> = tokens
            .iter()
            .filter(|token| token.kind() == SyntaxKind::VERB)
            .map(|token| token.text())
            .collect();
        assert_eq!(
            raw,
            [before_body],
            "{name}: only the pre-override reader remains active"
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind() == SyntaxKind::UNDERSCORE),
            "{name}: the later group has ordinary syntax"
        );
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{name}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn source_generated_inline_readers_retire_after_literal_alias_override() {
    let source = r"\newmintinline[py]{python}{}\py{\ref{fake}}\let\py\relax\py{\ref{live}}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "live");
}

#[test]
fn generated_inline_profiles_replace_known_readers_before_later_retirement() {
    for name in ["show", "newcommand", "code"] {
        let source = format!(
            "\\newmintinline[{name}]{{python}}{{}}\\{name}{{\\ref{{fake}}}}\\ref{{real}}\\let\\{name}\\relax\\{name}{{\\ref{{live}}}}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(
            model
                .refs()
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["real", "live"],
            "{name}"
        );
    }
}

#[test]
fn source_shadowed_definition_factories_cannot_create_false_sites() {
    for (source, expected) in [
        (r"\let\newcommand\relax\newcommand{\ghost}{x}", "newcommand"),
        (r"\let\def\relax\def\ghost{x}", "def"),
        (r"\let\newlength\relax\newlength{\ghost}", "newlength"),
        (
            r"\let\newenvironment\relax\newenvironment{ghost}{}{}",
            "newenvironment",
        ),
        (
            r"\ExplSyntaxOn\let\cs_set:Npn\relax\cs_set:Npn\ghost{x}",
            "cs_set:Npn",
        ),
        (
            r"\ExplSyntaxOn\let\cs_set:cpn\relax\cs_set:cpn{ghost}{x}",
            "cs_set:cpn",
        ),
        (r"\let\csdef\relax\csdef{ghost}{x}", "csdef"),
        (
            r"\let\newtheorem\relax\newtheorem{ghost}{Ghost}",
            "newtheorem",
        ),
        (r"\let\let\relax\let\ghost\relax", "let"),
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        let sites = tex_ls_parser::semantic::scan_definition_sites(&parsed.syntax());
        assert_eq!(
            sites
                .iter()
                .map(|site| site.name.as_str())
                .collect::<Vec<_>>(),
            [expected],
            "{source}"
        );
        assert!(
            tex_ls_parser::semantic::scan_definitions(&parsed.syntax())
                .command("ghost")
                .is_none(),
            "{source}"
        );
    }
    for (source, expected) in [
        (
            r"\meaning\newcommand{\ghost}{x}\newcommand{\real}{x}",
            vec!["real"],
        ),
        (r"\string\let\newcommand{\real}{x}", vec!["real"]),
        (
            r"\let\copy\newcommand\newcommand{\real}{x}",
            vec!["copy", "real"],
        ),
        (
            r"\def\newcommand{x}\newcommand{\ghost}{x}",
            vec!["newcommand"],
        ),
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        let sites = tex_ls_parser::semantic::scan_definition_sites(&parsed.syntax());
        assert_eq!(
            sites
                .iter()
                .map(|site| site.name.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn shortverb_observers_retire_only_after_actual_literal_bindings() {
    for name in ["MakeShortVerb", "DefineShortVerb"] {
        let source = format!(
            "\\{name}{{\\|}}|\\ref{{fake}}|\\let\\{name}\\relax\\{name}{{\\!}}!\\ref{{live}}!"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{name}");
        assert_eq!(model.refs()[0].name, "live", "{name}");
    }
    for name in ["documentclass", "LoadClass"] {
        let source = format!(
            "\\{name}{{ltxdoc}}|\\ref{{fake}}|\\let\\{name}\\relax\\DeleteShortVerb{{\\|}}\\{name}{{ltxdoc}}|\\ref{{live}}|"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{name}");
        assert_eq!(model.refs()[0].name, "live", "{name}");
    }
    for name in ["DeleteShortVerb", "UndefineShortVerb"] {
        let source = format!(
            "\\MakeShortVerb{{\\|}}\\{name}{{\\|}}|\\ref{{before}}|\\MakeShortVerb{{\\|}}\\let\\{name}\\relax\\{name}{{\\|}}|\\ref{{fake}}|\\ref{{live}}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(
            model
                .refs()
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["before", "live"],
            "{name}"
        );
    }
}

#[test]
fn inert_shortverb_observer_bindings_cannot_retire_live_helpers() {
    for source in [
        r"\meaning\let\MakeShortVerb{\|}|\ref{fake}|\ref{live}",
        r"\string\let\MakeShortVerb{\|}|\ref{fake}|\ref{live}",
        r"\let\copy\MakeShortVerb\MakeShortVerb{\|}|\ref{fake}|\ref{live}",
        r"\MakeShortVerb{\|}\meaning\let\DeleteShortVerb{\|}|\ref{live}|",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live", "{source}");
    }
}

#[test]
fn inert_delimiter_reader_operands_cannot_disable_following_shortverbs() {
    for source in [
        r"\MakeShortVerb{\|}\meaning\left|\ref{fake}|\ref{live}",
        r"\MakeShortVerb{\|}\let\copy\right|\ref{fake}|\ref{live}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live", "{source}");
    }
}

#[test]
fn retired_definition_heads_cannot_block_following_shortverb_observers() {
    for name in ["let", "def", "newcommand"] {
        let source = format!(
            "\\let\\{name}\\relax\\{name}\\MakeShortVerb{{\\|}}|\\ref{{fake}}|\\ref{{live}}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{name}");
        assert_eq!(model.refs()[0].name, "live", "{name}");
    }
}
