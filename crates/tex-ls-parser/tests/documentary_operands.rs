use tex_ls_parser::parser::parse;
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxKind;

#[test]
fn ltxdoc_command_names_are_literal_operands() {
    for operand in [r"\cmd\verb", r"\cmd{\verb}", r"\cmd\makeatletter"] {
        for ending in ["}", ", and support footnotes.}"] {
            let source = format!(
                r"\changes{{v1}}{{2000/01/01}}{{Added support for {operand}{ending}\ref{{live}}\verb|active|"
            );
            let parsed = parse(&source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let raw: Vec<_> = parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.kind() == SyntaxKind::VERB)
                .map(|token| token.text().to_owned())
                .collect();
            assert_eq!(raw, [r"\verb|active|"], "{source}");
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn retired_and_inert_documentary_heads_keep_active_operands() {
    for prefix in [
        r"\let\cmd\relax",
        r"\renewcommand{\cmd}[1]{#1}",
        r"\meaning",
    ] {
        let source = format!(r"{prefix}\cmd\verb|active|\ref{{live}}");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::VERB),
            "{source}"
        );
        assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
    }
}

#[test]
fn documentary_operand_roles_do_not_execute_printed_references() {
    let source = r"\cmd\ref{printed}\cmd{\ref}{printed}\ref{live}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "live");
}

#[test]
fn documentary_reader_retirement_starts_at_a_real_binding() {
    for binding in [r"\let\cmd\relax", r"\RenewCommandCopy{\cmd}{\relax}"] {
        let source = format!(r"\cmd\verb{binding}\cmd\verb|active|\ref{{live}}");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let raw: Vec<_> = parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::VERB)
            .map(|token| token.text().to_owned())
            .collect();
        assert_eq!(raw, [r"\verb|active|"], "{source}");
    }
    let source = r"\meaning\let\cmd\relax\cmd\verb\ref{live}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert!(
        !parsed
            .syntax()
            .descendants_with_tokens()
            .any(|element| element.kind() == SyntaxKind::VERB)
    );
    assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
}
