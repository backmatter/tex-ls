use tex_ls_parser::declarations::Declarations;
use tex_ls_parser::parser::{LatexFlavor, parse, parse_with_declarations};
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxKind;

#[test]
fn project_alias_of_curated_raw_profile_captures_literal_names() {
    let declarations: Declarations = serde_json::from_value(serde_json::json!({
        "environments": {"documentedcommands": {"like": "macro"}}
    }))
    .unwrap();
    let declarations = declarations.resolve().unwrap();
    for source in [
        r"\begin{documentedcommands}[EXP]{\ref{literal}}\end{documentedcommands}",
        r"\begin{documentedcommands}|\ref{literal}|\end{documentedcommands}",
    ] {
        let parsed = parse_with_declarations(source, LatexFlavor::Document, &declarations);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(SemanticModel::build(&parsed.syntax()).refs().is_empty());
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::VERB)
        );
    }
}

#[test]
fn arbitrary_xparse_verbatim_arguments_do_not_create_raw_readers() {
    for (name, spec) in [("custom", "O{} +v"), ("macro", "m +v"), ("function", "v")] {
        let source = format!(
            r"\DeclareDocumentEnvironment{{{name}}}{{{spec}}}{{}}{{}}\begin{{{name}}}{{\ref{{live}}}}\end{{{name}}}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert_eq!(
            SemanticModel::build(&parsed.syntax()).refs().len(),
            1,
            "{source}"
        );
    }
}

#[test]
fn environment_reader_edits_match_full_trees_and_errors() {
    use tex_ls_parser::declarations::ResolvedDeclarations;
    use tex_ls_parser::parser::{
        Edit, LexConfig, ReparseBase, parse_with_declarations_resolved, reparse,
    };

    let replacement: Declarations = serde_json::from_value(serde_json::json!({
        "environments": {"macro": {"like": "quote"}}
    }))
    .unwrap();
    let alias: Declarations = serde_json::from_value(serde_json::json!({
        "environments": {"documentedcommands": {"like": "macro"}}
    }))
    .unwrap();
    for declarations in [
        ResolvedDeclarations::default(),
        replacement.resolve().unwrap(),
        alias.resolve().unwrap(),
    ] {
        for source in [
            "\\newenvironment{macro}[1]{}{}\n\\begin{macro}{\\ref{café}}\\end{macro}\n",
            "\\DeclareDocumentEnvironment{macro}{O{} +v}{}{}\n\\begin{macro}[EXP]{\\ref{café}}\\end{macro}\n",
            "\\begin{documentedcommands}[EXP]{\\ref{café}}\\end{documentedcommands}\n",
        ] {
            for source in [source.to_owned(), source.replace('\n', "\r\n")] {
                let config = LexConfig::default();
                let (parsed, context) =
                    parse_with_declarations_resolved(&source, config, &declarations);
                let base = ReparseBase::from_parts(
                    &source,
                    &parsed.green,
                    &parsed.errors,
                    &context,
                    config,
                    &declarations,
                );
                for (at, character) in source
                    .char_indices()
                    .chain(std::iter::once((source.len(), '\0')))
                {
                    let mut edits: Vec<_> = ["x", "%", "\n", "{", "}", "é"]
                        .into_iter()
                        .map(|insert| Edit {
                            range: at..at,
                            insert: insert.into(),
                        })
                        .collect();
                    if at < source.len() {
                        edits.push(Edit {
                            range: at..at + character.len_utf8(),
                            insert: String::new(),
                        });
                    }
                    for edit in edits {
                        let mut edited = source.clone();
                        edited.replace_range(edit.range.clone(), &edit.insert);
                        let full =
                            parse_with_declarations_resolved(&edited, config, &declarations).0;
                        assert_eq!(full.syntax().to_string(), edited);
                        if let Some(incremental) = reparse(&base, &edit, &edited) {
                            assert_eq!(incremental.green, full.green, "{source:?}: {edit:?}");
                            assert_eq!(incremental.errors, full.errors, "{source:?}: {edit:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn ordinary_source_environments_override_curated_raw_names() {
    for name in ["macro", "function", "variable"] {
        for definition in [
            format!("\\newenvironment{{{name}}}[1]{{}}{{}}"),
            format!("\\renewenvironment{{{name}}}[1]{{}}{{}}"),
            format!("\\NewDocumentEnvironment{{{name}}}{{m}}{{}}{{}}"),
        ] {
            let source = format!("{definition}\\begin{{{name}}}{{\\ref{{live}}}}\\end{{{name}}}");
            let parsed = parse(&source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
            assert!(
                !parsed
                    .syntax()
                    .descendants_with_tokens()
                    .any(|element| element.kind() == SyntaxKind::VERB),
                "{source}"
            );
        }
    }
}

#[test]
fn project_environment_replacements_override_curated_raw_names() {
    for name in ["macro", "function", "variable"] {
        let declarations: Declarations =
            serde_json::from_value(serde_json::json!({"environments": {name: {"like": "quote"}}}))
                .unwrap();
        let declarations = declarations.resolve().unwrap();
        let source = format!("\\begin{{{name}}}{{\\ref{{live}}}}\\end{{{name}}}");
        let parsed = parse_with_declarations(&source, LatexFlavor::Document, &declarations);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn l3doc_source_definitions_retain_the_curated_raw_profile() {
    for name in ["macro", "function", "variable"] {
        let source = format!(
            "\\DeclareDocumentEnvironment{{{name}}}{{O{{}} +v}}{{}}{{}}\\begin{{{name}}}[EXP]{{\\ref{{literal}}}}\\end{{{name}}}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(SemanticModel::build(&parsed.syntax()).refs().is_empty());
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::VERB),
            "{source}"
        );
    }
}
