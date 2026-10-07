use tex_ls_parser::parser::{LatexFlavor, lex, parse};
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxKind;

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    source: String,
    references: usize,
}

#[test]
fn listing_ownership_matches_verified_package_fixtures() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/embedded-coloring.json")).unwrap();
    for case in cases {
        for source in [case.source.clone(), case.source.replace('\n', "\r\n")] {
            let parsed = parse(&source);
            assert_eq!(parsed.syntax().text().to_string(), source, "{}", case.name);
            assert!(
                parsed.errors.is_empty(),
                "{}: {:?}",
                case.name,
                parsed.errors
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), case.references, "{}", case.name);
        }
    }
}

#[test]
fn lexical_modes_restore_groups_and_explicit_assignments() {
    for source in [
        r"{\makeatletter \demo@name}\demo@name",
        r"\begingroup\makeatletter\demo@name\endgroup\demo@name",
        r"\catcode`\@=11 \demo@name \catcode`\@=12 \demo@name",
    ] {
        let names: Vec<_> = lex(source)
            .into_iter()
            .filter(|t| t.kind == SyntaxKind::CONTROL_WORD && t.text.starts_with("\\demo"))
            .map(|t| t.text.to_string())
            .collect();
        assert_eq!(names, ["\\demo@name", "\\demo"]);
    }
    let parsed = tex_ls_parser::parser::parse_with_flavor(
        r"{\makeatother\demo@name}\demo@name",
        LatexFlavor::Package,
    );
    let names: Vec<_> = parsed
        .syntax()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::CONTROL_WORD && t.text().starts_with("\\demo"))
        .map(|t| t.text().to_owned())
        .collect();
    assert_eq!(names, ["\\demo", "\\demo@name"]);
}

#[test]
fn unknown_escape_settings_and_unbalanced_islands_remain_inert() {
    for source in [
        "\\begin{minted}[escapeinside=\\unknown]{python}\nx=|\\ref{fake}|\n\\end{minted}\\ref{real}",
        "\\begin{lstlisting}[escapeinside={(*@}{@*)}]\nx=(*@\\ref{fake@*)\n\\end{lstlisting}\\ref{real}",
        "\\begin{minted}[escapeinside=||]{python}\nx=\"\"\"|\\ref{fake}|\"\"\"\n\\end{minted}\\ref{real}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
    }
}

#[test]
fn ordinary_words_in_escape_islands_do_not_disable_references() {
    let source =
        "\\begin{minted}[escapeinside=||]{python}\nx=|mint \\ref{mint:Verbatim}|\n\\end{minted}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "mint:Verbatim");
}

#[test]
fn explicit_single_line_headers_delegate_only_the_foreign_body() {
    for language in [
        "c",
        "c++",
        "javascript",
        "typescript",
        "json",
        "css",
        "html",
    ] {
        for header in [
            format!("\\begin{{minted}}{{{language}}}"),
            format!("\\begin{{lstlisting}}[language={language}]"),
        ] {
            let environment = if header.contains("minted") {
                "minted"
            } else {
                "lstlisting"
            };
            let source = format!("{header}\n\\ref{{fake}}\n\\end{{{environment}}}\\ref{{real}}");
            let parsed = parse(&source);
            assert_eq!(parsed.syntax().text().to_string(), source);
            assert!(
                parsed
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(|e| e.into_token())
                    .any(|t| t.kind() == SyntaxKind::DELEGATED_CODE),
                "{source}"
            );
            assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
        }
    }
    for source in [
        "\\begin{minted}\n{c}\n\\ref{fake}\n\\end{minted}",
        "\\begin{lstlisting}[language=c,\n numbers=left]\n\\ref{fake}\n\\end{lstlisting}",
        "\\begin{minted}[escapeinside=||]{c}\n\\ref{fake}\n\\end{minted}",
        "\\begin{lstlisting}[language=C,language=javascript]\ncode\n\\end{lstlisting}",
        "\\begin{lstlisting}[caption={language=C},language=javascript]\ncode\n\\end{lstlisting}",
        "\\begin{lstlisting}[caption=language=C,language=javascript]\ncode\n\\end{lstlisting}",
        "\\begin{lstlisting}[other-language=C,language=javascript]\ncode\n\\end{lstlisting}",
    ] {
        assert!(
            !lex(source)
                .iter()
                .any(|t| t.kind == SyntaxKind::DELEGATED_CODE)
        );
    }
}

#[test]
fn local_overrides_disable_source_settings_and_generated_aliases() {
    for definition in ["\\renewcommand{\\lstset}[1]{}", "\\let\\lstset\\relax"] {
        let source = format!(
            "{definition}\\lstset{{language=Python,escapeinside={{(*@}}{{@*)}}}}\\begin{{lstlisting}}\n(*@\\ref{{fake}}@*)\n\\end{{lstlisting}}\\ref{{real}}"
        );
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
    }
    for (definition, invocation, environment) in [
        (
            "\\renewcommand{\\newminted}[2]{}",
            "\\newminted{python}{}",
            "pythoncode",
        ),
        (
            "\\let\\newminted\\relax",
            "\\newminted[pycode]{python}{}",
            "pycode",
        ),
        (
            "\\renewcommand{\\lstnewenvironment}[3]{}",
            "\\lstnewenvironment{pycode}{}{}",
            "pycode",
        ),
    ] {
        let source = format!(
            "{definition}{invocation}\\begin{{{environment}}}\n\\ref{{real}}\n\\end{{{environment}}}"
        );
        let parsed = parse(&source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert!(
            tex_ls_parser::semantic::scan_definitions(&root)
                .environment(environment)
                .is_none()
        );
        assert!(
            !tex_ls_parser::semantic::define::editor::editor_declarations(&root)
                .iter()
                .any(|declaration| declaration.site.name == environment)
        );
        assert!(
            !tex_ls_parser::semantic::scan_definition_sites(&root)
                .iter()
                .any(|site| site.name == environment)
        );
    }
}

#[test]
fn local_nonraw_comment_environments_keep_their_active_body() {
    for source in [
        "\\renewenvironment{comment}{}{}\\begin{comment}\n\\ref{real}\n\\end{comment}",
        "\\excludecomment{private}\\renewenvironment{private}{}{}\\begin{private}\n\\ref{real}\n\\end{private}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert!(
            !parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::EXCLUDED_BODY)
        );
    }
}

#[test]
fn explicit_raw_declarations_override_source_environment_modes() {
    use tex_ls_parser::declarations::Declarations;
    use tex_ls_parser::parser::{LexConfig, parse_with_declarations};
    for (name, source) in [
        (
            "custom",
            "\\newenvironment{custom}{}{}\\begin{custom}\n\\ref{fake}\n\\end{custom}\\ref{real}",
        ),
        (
            "minted",
            "\\renewenvironment{minted}{}{}\\begin{minted}{python}\n\\ref{fake}\n\\end{minted}\\ref{real}",
        ),
        (
            "minted",
            "\\begin{minted}[escapeinside=||]{python}\n|\\ref{fake}|\n\\end{minted}\\ref{real}",
        ),
    ] {
        let declared: Declarations = serde_json::from_value(serde_json::json!({
            "environments": {name: {"like": "verbatim"}}
        }))
        .unwrap();
        let parsed =
            parse_with_declarations(source, LexConfig::default(), &declared.resolve().unwrap());
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::VERBATIM_BODY)
        );
        assert!(
            !parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::ESCAPE_OPEN)
        );
    }
}

#[test]
fn inert_token_lists_keep_proved_outer_catcodes_and_editor_declarations() {
    for source in [
        "\\catcode37=12\\relax\\detokenize{%printed}\\ref{real}",
        "\\catcode37=12\\relax\\detokenize{%\\ref{fake}\\newcommand{\\ghost}{x}}\\ref{real}",
    ] {
        let parsed = parse(source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert!(tex_ls_parser::semantic::define::editor::editor_declarations(&root).is_empty());
        assert!(
            !root
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::COMMENT)
        );
    }
}

#[test]
fn catcode_assignment_requires_a_proved_numeric_boundary_before_detokenize() {
    // TeX expands detokenize while reading this unterminated number. Percent
    // still has its old comment catcode and consumes the argument's closer.
    let source = "\\catcode37=12\\detokenize{% two  words\\ref{fake}}\n";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(!parsed.errors.is_empty());
    assert!(SemanticModel::build(&parsed.syntax()).refs().is_empty());
}

#[test]
fn inspected_and_copied_primitive_heads_leave_following_tex_active() {
    for source in [
        "\\meaning\\detokenize{\\ref{real}}",
        "\\show\\detokenize{\\ref{real}}",
        "\\noexpand\\detokenize{\\ref{real}}",
        "\\let\\copy\\detokenize{\\ref{real}}",
        "\\let\\copy=\\detokenize{\\ref{real}}",
        "\\NewCommandCopy{\\copy}{\\detokenize}{\\ref{real}}",
        "\\meaning\\string\\ref{real}",
        "\\show\\string\\ref{real}",
        "\\let\\copy\\string\\ref{real}",
        "\\let\\copy=\\string\\ref{real}",
        "\\NewCommandCopy{\\copy}{\\string}\\ref{real}",
        "\\futurelet\\detokenize\\relax\\relax{\\ref{real}}",
        "\\futurelet\\string\\relax\\relax{\\ref{real}}",
        "\\string\\newcommand\\ref{real}",
    ] {
        let parsed = parse(source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert!(
            tex_ls_parser::semantic::context::TokenListContexts::new(&root)
                .ranges()
                .is_empty(),
            "{source}"
        );
    }
    // Futurelet reinserts its next two tokens. The third token executes as an
    // ordinary detokenize head, so its argument retains primitive ownership.
    let source = "\\futurelet\\copy\\relax\\detokenize{\\ref{fake}}\\ref{real}";
    let parsed = parse(source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "real");
}

#[test]
fn overridden_operand_readers_do_not_suppress_active_primitives() {
    for declaration in [
        "\\renewcommand{\\meaning}{}",
        "\\let\\meaning\\relax",
        "\\renewcommand{\\show}{}",
        "\\let\\show\\relax",
    ] {
        let reader = if declaration.contains("meaning") {
            "meaning"
        } else {
            "show"
        };
        let source = format!("{declaration}\\{reader}\\detokenize{{\\ref{{fake}}}}\\ref{{real}}");
        let parsed = parse(&source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        let model = SemanticModel::build(&root);
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert_eq!(
            tex_ls_parser::semantic::context::TokenListContexts::new(&root)
                .ranges()
                .len(),
            1
        );
    }
}

#[test]
fn active_foreign_islands_keep_the_parent_parse_context() {
    for body in [
        "\\begin{lstlisting}[escapeinside={(*@}{@*)}]\n(*@\\code{\\ref{real}}@*)\n\\end{lstlisting}",
        "\\begin{Verbatim}[commandchars=\\\\\\{\\}]\n\\code{\\ref{real}}\n\\end{Verbatim}",
        "\\begin{minted}[texcomments]{python}\n# \\code{\\ref{real}}\n\\end{minted}",
    ] {
        let source = format!("\\renewcommand{{\\code}}[1]{{#1}}{body}");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
    }
}

#[test]
fn fancy_command_only_lines_keep_active_syntax_without_literal_prose() {
    let source = "\\begin{Verbatim}[commandchars=\\\\\\{\\}]\n\\ref{real}\n\\end{Verbatim}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "real");
}

#[test]
fn python_function_names_keep_complete_unicode_identifiers() {
    let source = "\\begin{minted}{python}\ndef café(δ):\n    return δ\n\\end{minted}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let names: Vec<_> = parsed
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::FOREIGN_FUNCTION)
        .map(|token| token.text().to_owned())
        .collect();
    assert_eq!(names, ["café"]);
}

#[test]
fn nested_literal_captures_and_included_comments_do_not_recurse_in_the_lexer() {
    let fancy = "\\begin{Verbatim}[commandchars=\\\\\\{\\}]\n";
    let source = format!("{}\\ref{{fake}}\n\\end{{Verbatim}}", fancy.repeat(512));
    let parsed = parse(&source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(SemanticModel::build(&parsed.syntax()).refs().is_empty());
    let included = "\\includecomment{comment}\\begin{comment}\n";
    let source = format!("{}body\n\\end{{comment}}", included.repeat(512));
    let parsed = parse(&source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(!parsed.errors.is_empty());
}

#[test]
fn fancy_custom_characters_and_comments_have_real_ownership() {
    let source = "\\begin{Verbatim}[commandchars=+(),commentchar=!]\nSee +ref(eq:live) ! +ref(eq:fake)\n\\ref{eq:printed}\n\\end{Verbatim}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "eq:live");
}

#[test]
fn fancy_comment_characters_stop_adjacent_text_and_special_characters() {
    for comment in ['!', '#', '$', '&', '^', '_', '~'] {
        for prefix in ["hello", "hello "] {
            let source = format!(
                "\\begin{{Verbatim}}[commandchars=+(),commentchar={comment}]\n{prefix}{comment}+ref(fake)\n+ref(real)\n\\end{{Verbatim}}"
            );
            let parsed = parse(&source);
            assert_eq!(parsed.syntax().text().to_string(), source);
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "real");
        }
    }
}

#[test]
fn minted_defaults_cannot_change_custom_listings_ownership() {
    let source = "\\setminted[Python]{mathescape=true}\\lstnewenvironment{mycode}{\\lstset{language=Python}}{}\\begin{mycode}\n$\\ref{fake}$\n\\end{mycode}\\ref{real}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "real");
}

#[test]
fn editing_minted_defaults_reparses_the_following_foreign_body() {
    use tex_ls_parser::declarations::ResolvedDeclarations;
    use tex_ls_parser::parser::{
        Edit, LexConfig, ReparseBase, parse_with_declarations_resolved, reparse,
    };

    let source = "\\setminted{escapechar=|}\\begin{minted}{c}\nint n = 1;\n\\end{minted}";
    let declared = ResolvedDeclarations::default();
    let config = LexConfig::default();
    let (parsed, ctx) = parse_with_declarations_resolved(source, config, &declared);
    let at = source.find("escapechar").unwrap();
    let edit = Edit {
        range: at..at + "escapechar".len(),
        insert: "unusedchar".into(),
    };
    let changed = edit.apply(source);
    let base = ReparseBase::from_parts(
        source,
        &parsed.green,
        &parsed.errors,
        &ctx,
        config,
        &declared,
    );
    assert!(reparse(&base, &edit, &changed).is_none());
    assert!(
        !parsed
            .syntax()
            .descendants_with_tokens()
            .any(|element| element.kind() == SyntaxKind::DELEGATED_CODE)
    );
    assert!(
        parse(&changed)
            .syntax()
            .descendants_with_tokens()
            .any(|element| element.kind() == SyntaxKind::DELEGATED_CODE)
    );
}

#[test]
fn local_settings_and_comment_declarations_restore_at_scope_end() {
    let source = "{\\lstset{language=Python,escapeinside={(*@}{@*)}}\\begin{lstlisting}\nx=(*@\\ref{real}@*)\n\\end{lstlisting}}\\begin{lstlisting}\nx=(*@\\ref{fake}@*)\n\\end{lstlisting}";
    let parsed = parse(source);
    assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
    let source = "{\\includecomment{comment}\\begin{comment}\n\\ref{real}\n\\end{comment}}\\begin{comment}\n\\ref{fake}\n\\end{comment}";
    let parsed = parse(source);
    assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
}

#[test]
fn conditional_listing_settings_and_declarations_cannot_prove_tex_ownership() {
    for (before, settings, listing) in [
        (
            "",
            r"\lstset{language=Python,mathescape=true}",
            "\\begin{lstlisting}\n# x=$\\ref{fake}$\n\\end{lstlisting}",
        ),
        (
            "",
            r"\fvset{commandchars=+\[\]}",
            "\\begin{Verbatim}\n+ref[fake]\n\\end{Verbatim}",
        ),
        (
            "",
            r"\setminted{mathescape=true}",
            "\\begin{minted}{python}\n# x=$\\ref{fake}$\n\\end{minted}",
        ),
        (
            "",
            r"\setminted[python]{mathescape=true}",
            "\\begin{minted}{python}\n# x=$\\ref{fake}$\n\\end{minted}",
        ),
        (
            "",
            r"\setmintedinline{mathescape=true}",
            r"\mintinline{python}|# x=$\ref{fake}$|",
        ),
        (
            "",
            r"\setmintedinline[python]{mathescape=true}",
            r"\mintinline{python}|# x=$\ref{fake}$|",
        ),
        (
            "",
            r"\newmintinline[code]{python}{mathescape=true}",
            r"\code|# x=$\ref{fake}$|",
        ),
        (
            "",
            r"\lstdefinestyle{mycode}{language=Python,mathescape=true}",
            "\\begin{lstlisting}[style=mycode]\n# x=$\\ref{fake}$\n\\end{lstlisting}",
        ),
        (
            "",
            r"\newminted[code]{python}{mathescape=true}",
            "\\begin{code}\n# x=$\\ref{fake}$\n\\end{code}",
        ),
        (
            "",
            r"\lstnewenvironment{code}{\lstset{language=Python,mathescape=true}}{}",
            "\\begin{code}\n# x=$\\ref{fake}$\n\\end{code}",
        ),
        (
            "",
            r"\DefineVerbatimEnvironment{code}{Verbatim}{commandchars=+\[\]}",
            "\\begin{code}\n+ref[fake]\n\\end{code}",
        ),
        (
            "",
            r"\includecomment{comment}",
            "\\begin{comment}\n\\ref{fake}\n\\end{comment}",
        ),
        (
            r"\includecomment{comment}",
            r"\excludecomment{comment}",
            "\\begin{comment}\n\\ref{fake}\n\\end{comment}",
        ),
        (
            "",
            r"\includecomment{review}",
            "\\begin{review}\n\\ref{fake}\n\\end{review}",
        ),
        (
            r"\includecomment{review}",
            r"\excludecomment{review}",
            "\\begin{review}\n\\ref{fake}\n\\end{review}",
        ),
    ] {
        let active_settings = if settings.starts_with("\\excludecomment") {
            before.to_owned()
        } else {
            format!("{before}{settings}")
        };
        let active = format!("{active_settings}\n{listing}\n");
        let active_parse = parse(&active);
        let active_model = SemanticModel::build(&active_parse.syntax());
        assert_eq!(active_model.refs().len(), 1, "{active}");
        assert_eq!(active_model.refs()[0].name, "fake", "{active}");
        for conditional in [r"\iffalse", r"\ifnum\count0>0", r"\ifcase0\or"] {
            for newline in ["\n", "\r\n", "\r"] {
                let source =
                    format!("{before}{conditional}{settings}\\fi\n{listing}\n\\ref{{real}}")
                        .replace('\n', newline);
                let parsed = parse(&source);
                let root = parsed.syntax();
                assert_eq!(root.text().to_string(), source);
                assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
                let model = SemanticModel::build(&root);
                assert_eq!(model.refs().len(), 1, "{source}");
                assert_eq!(model.refs()[0].name, "real", "{source}");
                assert!(
                    root.descendants_with_tokens()
                        .any(|element| element.kind() == SyntaxKind::VERBATIM_BODY),
                    "conditional ownership should remain protected: {source}"
                );
            }
        }
    }
}

#[test]
fn conditional_listing_uncertainty_restores_with_its_scope() {
    for (settings, before, listing) in [
        (
            r"\lstset{language=Python,mathescape=true}",
            r"\lstset{mathescape=false}",
            "\\begin{lstlisting}\nx=$\\ref{KEY}$\n\\end{lstlisting}",
        ),
        (
            r"\includecomment{comment}",
            r"\excludecomment{comment}",
            "\\begin{comment}\n\\ref{KEY}\n\\end{comment}",
        ),
    ] {
        let uncertain = listing.replace("KEY", "fake");
        let restored = listing.replace("KEY", "real");
        let source = format!("{settings}{{\\iffalse{before}\\fi{uncertain}\n}}{restored}\n");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
    }
}

#[test]
fn fancy_comment_character_stops_at_every_line_ending() {
    for newline in ["\n", "\r\n", "\r"] {
        for comment in ["!", "λ"] {
            let source = format!(
                "\\begin{{Verbatim}}[commentchar={comment}]{newline}kept {comment} discard{newline}next line{newline}\\end{{Verbatim}}"
            );
            let parsed = parse(&source);
            let root = parsed.syntax();
            assert_eq!(root.text().to_string(), source);
            assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
            let comments: Vec<_> = root
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.kind() == SyntaxKind::EXCLUDED_BODY)
                .map(|token| token.text().to_owned())
                .collect();
            assert_eq!(comments, [format!("{comment} discard")]);
            assert!(root.descendants_with_tokens().any(|element| {
                element.as_token().is_some_and(|token| {
                    token.kind() == SyntaxKind::VERBATIM_BODY && token.text().contains("next line")
                })
            }));
        }
    }
}

#[test]
fn compared_primitive_names_do_not_execute_but_leftover_groups_do() {
    for (comparison, references) in [
        (r"\ifx\relax\detokenize", 1),
        (r"\ifx a\detokenize", 1),
        (r"\ifx ab\detokenize", 0),
        (r"\ifx a \detokenize", 0),
        (r"\ifx\a \detokenize", 1),
        (r"\ifx\! \detokenize", 0),
        ("\\ifx a% ignored\n\\detokenize", 1),
        ("\\ifx a% ignored\n  \\detokenize", 1),
        ("\\ifx a% ignored\r\n  \\detokenize", 1),
        ("\\ifx a% ignored\r  \\detokenize", 1),
    ] {
        let source = format!("{comparison}{{\\ref{{key}}}}\\fi");
        let parsed = parse(&source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert_eq!(
            SemanticModel::build(&parsed.syntax()).refs().len(),
            references,
            "{source}"
        );
    }
}

#[test]
fn copied_environment_and_math_delimiters_do_not_change_source_structure() {
    for source in [
        r"\meaning\begin{equation}a_b\meaning\end{equation}\ref{real}",
        r"\let\copy\begin{equation}a_b\let\other\end{equation}\ref{real}",
        r"\ifx\begin\end{equation}a_b\fi\ref{real}",
        r"\meaning\begin{\begin{equation}a_b\end{equation}}\ref{real}",
        r"\meaning\newcommand{\begin{equation}a_b\end{equation}}\ref{real}",
        r"\let\copy\[ a_b \let\other\]\ref{real}",
        r"\[a+\meaning\]b\]\ref{real}",
        r"$\meaning\left(a_b\meaning\right)$\ref{real}",
        r"$\left(a+\meaning\right b\right)$\ref{real}",
        r"\ifx\fi\relax\ref{real}\fi",
        r"\ifdefined\begin{equation}a_b\fi\ref{real}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
    }
    for source in [
        r"\meaning\begin{\begin{equation}a_b\end{equation}}",
        r"\meaning\newcommand{\begin{equation}a_b\end{equation}}",
    ] {
        let parsed = parse(source);
        assert_eq!(
            parsed
                .syntax()
                .descendants()
                .filter(|node| node.kind() == tex_ls_parser::syntax::SyntaxKind::ENVIRONMENT)
                .count(),
            1,
            "{source}"
        );
    }
}

#[test]
fn copied_argument_groups_parse_scripts_in_the_surrounding_mode() {
    use tex_ls_parser::syntax::SyntaxKind;
    for (source, scripts) in [
        (r"\let\copy\ensuremath{a_b}", 0),
        (r"$\let\copy\text{a_b}$", 1),
        (r"\ensuremath{\let\copy\text{a_b}}", 1),
        (r"\text{$\let\copy\text{a_b}$}", 1),
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert_eq!(
            parsed
                .syntax()
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::SCRIPTED)
                .count(),
            scripts,
            "{source}"
        );
    }
}

#[test]
fn copied_math_command_groups_respect_the_nesting_limit() {
    let source = format!("${}x{}$", r"\meaning\text{".repeat(512), "}".repeat(512));
    let parsed = parse(&source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(
        parsed
            .errors
            .iter()
            .any(|error| error.message.contains("maximum parser nesting")),
        "{:?}",
        parsed.errors
    );
}

#[test]
fn malformed_math_and_inline_recovery_cannot_capture_following_source() {
    for source in [
        "\\begin{minted}[escapeinside=||]{python}\nx=|$unfinished|\n\\end{minted}\\ref{real}",
        "\\mintinline{python}|unfinished\n\\ref{real}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
    }
}

#[test]
fn starred_minted_alias_uses_declared_language_and_local_escape_options() {
    let source = "\\newminted{python}{}\\begin{pythoncode*}{escapeinside=||}\nx=1 |\\ref{real}|\n\\end{pythoncode*}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
    let source = "\\newminted{c++}{}\\begin{c++code}\n\\ref{fake}\n\\end{c++code}";
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    assert!(SemanticModel::build(&parsed.syntax()).refs().is_empty());
}

#[test]
fn explicit_global_catcodes_survive_groups_and_unknown_at_codes_do_not_invent_names() {
    let source = r"{\global\catcode`\@=11}\demo@name";
    let tokens = lex(source);
    assert!(
        tokens
            .iter()
            .any(|t| t.kind == SyntaxKind::CONTROL_WORD && t.text == "\\demo@name")
    );
    let source = "\\ifexample\\catcode`\\@=11\\else\\catcode`\\@=12\\fi\\ref@name{fake}";
    let parsed = parse(source);
    assert_eq!(parsed.syntax().text().to_string(), source);
    assert!(SemanticModel::build(&parsed.syntax()).refs().is_empty());
}

#[test]
fn primitive_string_prints_one_token_without_executing_it() {
    for (source, names) in [
        ("\\string\\ref{printed}\\ref{real}", vec!["real"]),
        ("\\string\\ref{\\ref{real}}", vec!["real"]),
        (
            "\\string\\catcode37=12\n%\\ref{fake}\n\\ref{real}",
            vec!["real"],
        ),
        (
            "\\string\\lstset{mathescape=true}\\begin{lstlisting}\n$\\ref{fake}$\n\\end{lstlisting}\\ref{real}",
            vec!["real"],
        ),
        ("\\string\\verb|\\ref{real}|", vec!["real"]),
        ("\\string\\string\\ref{real}", vec!["real"]),
        (
            "\\string % comment\n\\ref{printed}\\ref{real}",
            vec!["real"],
        ),
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(
            model
                .refs()
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            names,
            "{source}"
        );
    }
    let source = "\\string\\newcommand{\\fake}{x}\\newcommand{\\real}{y}";
    let parsed = parse(source);
    let definitions = tex_ls_parser::semantic::scan_definitions(&parsed.syntax());
    assert!(definitions.command("fake").is_none());
    assert!(definitions.command("real").is_some());
    let tokens = lex("\\string\\makeatletter\\demo@name");
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\demo")
    );
    assert!(
        !tokens
            .iter()
            .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\demo@name")
    );
}

#[test]
fn string_consumes_one_character_and_local_overrides_disable_the_primitive() {
    let tokens = lex("\\string abc");
    let printed: Vec<_> = tokens
        .iter()
        .filter(|token| token.kind == SyntaxKind::STRINGIFIED_TOKEN)
        .map(|token| token.text.as_str())
        .collect();
    assert_eq!(printed, ["a"]);
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == SyntaxKind::WORD && token.text == "bc")
    );
    for source in [
        "\\newcommand{\\string}[1]{#1}\\string{\\ref{real}}",
        "\\let\\string\\relax\\string\\ref{real}",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text().to_string(), source);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source}");
        assert_eq!(model.refs()[0].name, "real");
        assert!(
            !parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::STRINGIFIED_TOKEN)
        );
    }
    for source in [
        "\\string",
        "\\string% comment",
        "\\string\\",
        "\\string{",
        "\\string$",
    ] {
        assert_eq!(parse(source).syntax().text().to_string(), source);
    }
}

#[test]
fn stored_string_parameters_keep_their_substitution_role() {
    for source in [
        "\\newcommand{\\show}[1]{\\string#1}",
        "\\def\\show#1{\\string#1}",
        "\\NewDocumentCommand{\\show}{m}{\\string#1}",
        "\\newcommand{\\outer}{\\newcommand{\\show}[1]{\\string##1}}",
        "\\newcommand{\\show}[1]{\\string#1\\string\\verb|\\ref{real}|}",
    ] {
        let parsed = parse(source);
        let root = parsed.syntax();
        assert_eq!(root.text().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        assert!(
            !root
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| {
                    token.kind() == SyntaxKind::STRINGIFIED_TOKEN && token.text() == "#"
                }),
            "{source}"
        );
        assert!(
            root.descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::HASH),
            "{source}"
        );
        assert!(
            !root
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::VERB),
            "{source}"
        );
    }
    let parsed = parse("\\string#1");
    assert!(
        parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| { token.kind() == SyntaxKind::STRINGIFIED_TOKEN && token.text() == "#" })
    );
    assert!(
        !parsed
            .syntax()
            .descendants_with_tokens()
            .any(|element| element.kind() == SyntaxKind::HASH)
    );
}

#[test]
fn string_operand_edits_fall_back_to_full_lexical_ownership() {
    use tex_ls_parser::declarations::ResolvedDeclarations;
    use tex_ls_parser::parser::{
        Edit, LexConfig, ReparseBase, parse_with_declarations_resolved, reparse,
    };
    let source = "\\string\\ref{printed}\\ref{real}";
    let declared = ResolvedDeclarations::default();
    let config = LexConfig::default();
    let (parsed, ctx) = parse_with_declarations_resolved(source, config, &declared);
    let base = ReparseBase::from_parts(
        source,
        &parsed.green,
        &parsed.errors,
        &ctx,
        config,
        &declared,
    );
    for at in source
        .char_indices()
        .map(|(at, _)| at)
        .chain([source.len()])
    {
        let edit = Edit {
            range: at..at,
            insert: "x".into(),
        };
        let changed = edit.apply(source);
        assert!(reparse(&base, &edit, &changed).is_none(), "{at}: {changed}");
        assert_eq!(parse(&changed).syntax().text().to_string(), changed);
    }
}
