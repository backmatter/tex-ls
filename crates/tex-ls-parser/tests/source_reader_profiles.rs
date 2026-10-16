use tex_ls_parser::parser::{LatexFlavor, LexConfig, parse_with_declarations};
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxKind;

const COMMENT_SETUP: &str = include_str!("source_readers/comment_setup.tex");

#[test]
fn verbatim_package_comment_keeps_suffix_active() {
    for ending in ["\n", "\r\n", "\r"] {
        for dtx in [false, true] {
            for gap in ["", " ", "\t", "  "] {
                let setup = if dtx {
                    format!("%    \\begin{{macrocode}}\n{COMMENT_SETUP}%    \\end{{macrocode}}\n")
                } else {
                    COMMENT_SETUP.into()
                };
                let source = format!(
                    "{setup}\\begin{{comment}}\n\\ref{{hidden}}\n  \\end{gap}{{comment}} \\ref{{live}}\n"
                ).replace('\n', ending);
                let parsed = parse_with_declarations(
                    &source,
                    LexConfig {
                        flavor: LatexFlavor::Package,
                        dtx,
                    },
                    &Default::default(),
                );
                assert_eq!(parsed.syntax().to_string(), source);
                assert!(parsed.errors.is_empty(), "{source:?}: {:?}", parsed.errors);
                let model = SemanticModel::build(&parsed.syntax());
                assert_eq!(model.refs().len(), 1, "{source:?}");
                assert_eq!(model.refs()[0].name, "live");
                assert!(
                    parsed
                        .syntax()
                        .descendants_with_tokens()
                        .any(|element| element.kind() == SyntaxKind::EXCLUDED_BODY)
                );
            }
        }
    }
}

#[test]
fn ordinary_comment_reader_requires_an_exact_physical_closer() {
    for setup in [
        "".to_owned(),
        COMMENT_SETUP.replace("\\verbatim@}", "\\relax}"),
        format!("{COMMENT_SETUP}\\let\\comment\\relax\n"),
    ] {
        let source = format!(
            "{setup}\\begin{{comment}}\n\\end {{comment}} \\ref{{hidden}}\n\\end{{comment}}\n\\ref{{live}}\n"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1);
        assert_eq!(model.refs()[0].name, "live");
    }
}

const RELEASE_SETUP: &str = include_str!("source_readers/release_setup.sty");

#[test]
fn standalone_kernel_release_headers_keep_name_and_description_token_data() {
    for flavor in [LatexFlavor::Document, LatexFlavor::Package] {
        for ending in ["\n", "\r\n", "\r"] {
            for optional in ["", "[2025/01/01]"] {
                for name in [r"\[", r"\]", r"\verb", r"\%", r"\Ω"] {
                    let source = format!(
                        "\\IncludeInRelease{{2026/06/01}}{optional}{{{name}}}{{Make \\] and \\verb robust \\ref{{hidden}}}}{ending}\\[x\\]\\ref{{live}}{ending}"
                    );
                    let parsed = parse_with_declarations(&source, flavor, &Default::default());
                    assert_eq!(parsed.syntax().to_string(), source);
                    assert!(parsed.errors.is_empty(), "{source:?}: {:?}", parsed.errors);
                    assert!(parsed.syntax().descendants_with_tokens().any(|element| {
                        element.kind() == SyntaxKind::STRINGIFIED_TOKEN
                            && element.as_token().is_some_and(|token| token.text() == name)
                    }));
                    let model = SemanticModel::build(&parsed.syntax());
                    assert_eq!(model.refs().len(), 1, "{source:?}");
                    assert_eq!(model.refs()[0].name, "live");
                }
            }
        }
    }
}

#[test]
fn typing_in_standalone_release_descriptions_uses_a_lossless_full_fallback() {
    use tex_ls_parser::parser::{Edit, ReparseBase, parse_with_declarations_resolved, reparse};

    for flavor in [LatexFlavor::Document, LatexFlavor::Package] {
        for ending in ["\n", "\r\n", "\r"] {
            let source = format!(
                "\\IncludeInRelease{{date}}{{\\]}}{{First description \\ref{{hidden}}}}{ending}\\IncludeInRelease{{date}}[previous]{{\\verb}}{{Second description \\ref{{alsohidden}}}}{ending}\\ref{{live}}{ending}"
            );
            let config = LexConfig { flavor, dtx: false };
            let declared = Default::default();
            for description in ["First description", "Second description"] {
                let mut current = source.clone();
                let (mut parsed, mut ctx) =
                    parse_with_declarations_resolved(&current, config, &declared);
                let description_token = parsed
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                    .find(|token| {
                        token.kind() == SyntaxKind::VERB && token.text().contains(description)
                    })
                    .expect("release descriptions retain protected token ownership");
                // Match the corpus driver's repeated typing just before the
                // protected description's closing brace, with a fixed caret.
                let at = usize::from(description_token.text_range().end()) - 1;
                for character in "typed".chars() {
                    let edit = Edit {
                        range: at..at,
                        insert: character.to_string(),
                    };
                    let changed = edit.apply(&current);
                    let base = ReparseBase::from_parts(
                        &current,
                        &parsed.green,
                        &parsed.errors,
                        &ctx,
                        config,
                        &declared,
                    );
                    assert!(
                        reparse(&base, &edit, &changed).is_none(),
                        "release descriptions require the full-file reader proof"
                    );
                    let (full, full_ctx) =
                        parse_with_declarations_resolved(&changed, config, &declared);
                    assert_eq!(full.syntax().to_string(), changed);
                    assert!(full.errors.is_empty(), "{changed:?}: {:?}", full.errors);
                    let model = SemanticModel::build(&full.syntax());
                    assert_eq!(
                        model.refs().len(),
                        1,
                        "description references remain stored data"
                    );
                    assert_eq!(model.refs()[0].name, "live");
                    current = changed;
                    parsed = full;
                    ctx = full_ctx;
                }
            }
        }
    }
}

#[test]
fn standalone_release_calls_in_docstrip_do_not_create_an_unused_context() {
    use tex_ls_parser::parser::parse_with_declarations_resolved;

    let source = "%    \\begin{macrocode}\n\\IncludeInRelease{date}{name}{\\ref{visible}}\n%    \\end{macrocode}\n";
    let config = LexConfig {
        flavor: LatexFlavor::Package,
        dtx: true,
    };
    let declared = Default::default();
    let (parsed, ctx) = parse_with_declarations_resolved(source, config, &declared);
    assert!(
        ctx.is_empty(),
        "docstrip must not pay for an inapplicable default profile"
    );
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "visible");
}

#[test]
fn semantic_project_command_declarations_preserve_release_header_syntax() {
    let declarations: tex_ls_parser::declarations::Declarations =
        serde_json::from_str(r#"{"commands":{"IncludeInRelease":{"like":"ref"}}}"#).unwrap();
    let declared = declarations.resolve().unwrap();
    let source = "\\IncludeInRelease{date}{\\]}{Printed \\] \\ref{hidden}}\\ref{live}\n";
    let ordinary = parse_with_declarations(source, LatexFlavor::Package, &Default::default());
    let configured = parse_with_declarations(source, LatexFlavor::Package, &declared);
    assert!(ordinary.errors.is_empty());
    assert_eq!(configured.green, ordinary.green);
    assert_eq!(configured.errors, ordinary.errors);
}

#[test]
fn standalone_release_reader_declines_main_helper_alias_and_partial_bindings() {
    for name in [
        "IncludeInRelease",
        "finish@module@release",
        "@IncludeInRelease",
        "@IncludeInRele@se",
    ] {
        for binding in [
            format!("\\def\\{name}#1{{#1}}\n"),
            format!("\\renewcommand{{\\{name}}}[3]{{#3}}\n"),
            format!("\\let\\{name}\\relax\n"),
            format!("\\NewCommandCopy{{\\{name}}}{{\\relax}}\n"),
            format!("\\newenvironment{{{name}}}{{}}{{}}\n"),
        ] {
            let source = format!("{binding}\\IncludeInRelease{{date}}{{name}}{{\\ref{{live}}}}\n");
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source:?}: {:?}", parsed.errors);
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source:?}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn copied_and_stored_release_names_do_not_activate_or_disable_the_reader() {
    use tex_ls_parser::parser::parse_with_declarations_resolved;

    for mention in [
        r"\let\saved\IncludeInRelease",
        r"\string\IncludeInRelease",
        r"\meaning\IncludeInRelease",
        r"\detokenize{\IncludeInRelease{date}{name}{description}}",
        r"\detokenize{\def\IncludeInRelease#1{#1}}",
        r"\let\saved\renewcommand{\IncludeInRelease}[3]{#3}",
    ] {
        let inert = format!("{mention}\n");
        let (_, ctx) =
            parse_with_declarations_resolved(&inert, LatexFlavor::Package, &Default::default());
        // The copied declaration's following group executes independently, so
        // its incomplete call can establish the default, but cannot bind it.
        if !mention.contains("renewcommand") {
            assert!(ctx.is_empty(), "inert names must not establish a reader");
        }
        let source = format!(
            "{inert}\\IncludeInRelease{{date}}{{\\]}}{{Printed \\] \\ref{{hidden}}}}\\ref{{live}}\n"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source:?}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "{source:?}");
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn standalone_release_profile_preserves_incremental_leaf_equivalence_on_wide_roots() {
    use tex_ls_parser::parser::{Edit, ReparseBase, parse_with_declarations_resolved, reparse};

    for ending in ["\n", "\r\n"] {
        let prefix = format!("Ordinary alphabet prose.{ending}{ending}").repeat(1024);
        let source = format!(
            "{prefix}\\IncludeInRelease{{date}}[previous]{{\\]}}{{Printed \\]}}{ending}Afterword alphabet{ending}"
        );
        let config = LatexFlavor::Package.into();
        let declared = Default::default();
        let (original, ctx) = parse_with_declarations_resolved(&source, config, &declared);
        assert!(original.errors.is_empty());
        for at in [
            source.find("alphabet").unwrap() + 3,
            source.rfind("alphabet").unwrap() + 3,
        ] {
            for insert in ["Q", "longer"] {
                let edit = Edit {
                    range: at..at + 1,
                    insert: insert.into(),
                };
                let changed = edit.apply(&source);
                let (full, full_ctx) =
                    parse_with_declarations_resolved(&changed, config, &declared);
                let base = ReparseBase::from_parts(
                    &source,
                    &original.green,
                    &original.errors,
                    &ctx,
                    config,
                    &declared,
                );
                let result =
                    reparse(&base, &edit, &changed).expect("ordinary prose stays incremental");
                assert_eq!(result.green, full.green);
                assert_eq!(result.errors, full.errors);
                assert_eq!(result.context(&ctx), &full_ctx);
            }
        }
        let edit = Edit {
            range: 0..0,
            insert: "\\let\\IncludeInRelease\\relax\n".into(),
        };
        let changed = edit.apply(&source);
        let base = ReparseBase::from_parts(
            &source,
            &original.green,
            &original.errors,
            &ctx,
            config,
            &declared,
        );
        assert!(
            reparse(&base, &edit, &changed).is_none(),
            "reader changes require a full parse"
        );
        let (_, replaced_ctx) = parse_with_declarations_resolved(&changed, config, &declared);
        assert!(
            replaced_ctx.is_empty(),
            "ordinary alias must retire the default reader"
        );
        let (_, ordinary_ctx) = parse_with_declarations_resolved(&prefix, config, &declared);
        assert!(
            ordinary_ctx.is_empty(),
            "ordinary prose must not acquire a release profile"
        );
    }
}

#[test]
fn blank_lines_before_parameters_cannot_qualify_release_readers() {
    for name in [
        "IncludeInRelease",
        "finish@module@release",
        "@IncludeInRelease",
        "@IncludeInRele@se",
    ] {
        for gap in ["\n\n", "\r\n \r\n", "% continued\n\n"] {
            let setup =
                RELEASE_SETUP.replace(&format!("\\def\\{name}#"), &format!("\\def\\{name}{gap}#"));
            assert_ne!(setup, RELEASE_SETUP);
            let source = format!("{setup}\\IncludeInRelease{{date}}{{name}}{{\\ref{{visible}}}}\n");
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(
                parsed.errors.is_empty(),
                "{name} {gap:?}: {:?}",
                parsed.errors
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "blank parameter line {name} {gap:?}");
            assert_eq!(model.refs()[0].name, "visible");
        }
    }
}

#[test]
fn blank_lines_before_comment_replacement_cannot_qualify_its_reader() {
    let setup = COMMENT_SETUP.replace("\\def\\comment{", "\\def\\comment\n\n{");
    assert_ne!(setup, COMMENT_SETUP);
    let source = format!(
        "{setup}\\begin{{comment}}\nignored \\end {{comment}} \\ref{{hidden}}\n\\end{{comment}}\n\\ref{{visible}}\n"
    );
    let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "visible");
}

#[test]
fn one_endline_after_release_definee_keeps_the_curated_reader() {
    for name in [
        "IncludeInRelease",
        "finish@module@release",
        "@IncludeInRelease",
        "@IncludeInRele@se",
    ] {
        for gap in [" ", "\n  ", "\r\n\t", "% continued\n  "] {
            let setup =
                RELEASE_SETUP.replace(&format!("\\def\\{name}#"), &format!("\\def\\{name}{gap}#"));
            assert_ne!(setup, RELEASE_SETUP);
            let source = format!(
                "{setup}\\IncludeInRelease{{date}}{{name}}{{\\ref{{hidden}}}}\\ref{{visible}}\n"
            );
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(
                parsed.errors.is_empty(),
                "{name} {gap:?}: {:?}",
                parsed.errors
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "ignored definee gap {name} {gap:?}");
            assert_eq!(model.refs()[0].name, "visible");
        }
    }
}

#[test]
fn qualified_release_headers_preserve_token_data_and_following_calls() {
    for ending in ["\n", "\r\n"] {
        for optional in ["", "[2025/01/01]"] {
            let source = format!("{RELEASE_SETUP}\\IncludeInRelease{{2026/06/01}}{optional}%\n{{\\feature}}{{Literal \\verb and \\ref{{printed}}}}%\n\\ref{{live}}\n").replace('\n', ending);
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source:?}: {:?}", parsed.errors);
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source:?}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn ordinary_release_replacement_keeps_live_argument_calls() {
    for setup in [
        "\\renewcommand{\\IncludeInRelease}[3]{#3}\n".to_owned(),
        format!("{RELEASE_SETUP}\\renewcommand{{\\IncludeInRelease}}[3]{{#3}}\n"),
        RELEASE_SETUP.replace("\\toks@{[#1] #3}", "#3"),
    ] {
        let source = format!("{setup}\\IncludeInRelease{{date}}{{name}}{{\\ref{{live}}}}\n");
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
    }
}

#[test]
fn release_parameter_delimiters_cannot_qualify_as_the_curated_reader() {
    for signature in [
        "\\def\\IncludeInRelease#1",
        "\\def\\finish@module@release#1#2#3",
        "\\def\\@IncludeInRelease#1[#2]",
        "\\def\\@IncludeInRele@se#1#2#3",
    ] {
        for gap in [" ", "\t", "\n", "\r\n"] {
            let setup =
                RELEASE_SETUP.replace(&format!("{signature}{{"), &format!("{signature}{gap}{{"));
            assert_ne!(setup, RELEASE_SETUP, "fixture must alter the signature");
            let source = format!("{setup}\\IncludeInRelease{{date}}{{name}}{{\\ref{{visible}}}}\n");
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(
                parsed.errors.is_empty(),
                "{signature} {gap:?}: {:?}",
                parsed.errors
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(
                model.refs().len(),
                1,
                "altered delimiter {signature} {gap:?}"
            );
            assert_eq!(model.refs()[0].name, "visible");
        }
    }
}

#[test]
fn release_parameter_comment_continuations_keep_the_curated_reader() {
    for signature in [
        "\\def\\IncludeInRelease#1",
        "\\def\\finish@module@release#1#2#3",
        "\\def\\@IncludeInRelease#1[#2]",
        "\\def\\@IncludeInRele@se#1#2#3",
    ] {
        let setup = RELEASE_SETUP.replace(
            &format!("{signature}{{"),
            &format!("{signature}% continued parameter text\n  {{"),
        );
        assert_ne!(setup, RELEASE_SETUP);
        let source = format!(
            "{setup}\\IncludeInRelease{{date}}{{name}}{{\\ref{{hidden}}}}\\ref{{visible}}\n"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{signature}: {:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1, "comment continuation {signature}");
        assert_eq!(model.refs()[0].name, "visible");
    }
}

#[test]
fn qualified_release_literal_verb_cannot_steal_header_closers() {
    for description in [
        "Literal \\verb",
        "Literal \\verb*",
        "[unmatched bracket \\verb",
        "{nested} % ignored }\n \\verb",
    ] {
        let source = format!(
            "{RELEASE_SETUP}\\IncludeInRelease{{date}}{{\\feature}}{{{description}}}\\ref{{live}}"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{source:?}: {:?}", parsed.errors);
        assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
    }
}

#[test]
fn qualified_comment_closes_midline_but_not_across_a_line() {
    let source = format!(
        "{COMMENT_SETUP}\\begin{{comment}}ignored \\end\n{{comment}} \\ref{{hidden}}\nignored \\end {{comment}} \\ref{{live}}\n"
    );
    let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
    assert_eq!(parsed.syntax().to_string(), source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let model = SemanticModel::build(&parsed.syntax());
    assert_eq!(model.refs().len(), 1);
    assert_eq!(model.refs()[0].name, "live");
}

#[test]
fn release_rollback_date_forwarding_keeps_the_same_token_data_roles() {
    for alternate in [
        "\\def\\@IncludeInRelease#1[#2]{\\@IncludeInRele@se{#1}}\n",
        "\\def\\@IncludeInRelease#1[#2]{\\@IncludeInRele@se{#2}}\n",
    ] {
        let source = format!(
            "{RELEASE_SETUP}{alternate}\\IncludeInRelease{{date}}[previous]{{name}}{{\\ref{{printed}} and \\verb}}\\ref{{live}}"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1);
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn release_headers_keep_expanded_dates_and_compound_names_visible() {
    for header in [
        r"\IncludeInRelease{\releasedate}{name}{description}",
        r"\IncludeInRelease{2026/06/01}[\releasedate]{name}{description}",
        r"\IncludeInRelease{2026/06/01}{prefix\releasedate}{description}",
        r"\IncludeInRelease{2026/06/01}{ \releasedate}{description}",
    ] {
        let source =
            format!("{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n{header}\\ref{{live}}\n");
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{header}: {:?}", parsed.errors);
        let visible = parsed
            .syntax()
            .descendants_with_tokens()
            .filter(|element| {
                element.kind() == SyntaxKind::CONTROL_WORD
                    && element
                        .as_token()
                        .is_some_and(|token| token.text() == "\\releasedate")
            })
            .count();
        assert_eq!(
            visible, 2,
            "binding and expanded date/name call must remain visible: {header}"
        );
        assert_eq!(SemanticModel::build(&parsed.syntax()).refs().len(), 1);
    }
}

#[test]
fn release_literal_dates_and_single_control_sequence_names_stay_token_data() {
    for name in [r"\releasedate", r"\verb", "plain-name"] {
        let source = format!(
            "{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n\\IncludeInRelease{{2026/06/01}}[2025/01/01]{{{name}}}{{Printed \\verb* and \\ref{{hidden}}}}\\ref{{live}}\n"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        let visible = parsed
            .syntax()
            .descendants_with_tokens()
            .filter(|element| {
                element.kind() == SyntaxKind::CONTROL_WORD
                    && element
                        .as_token()
                        .is_some_and(|token| token.text() == "\\releasedate")
            })
            .count();
        assert_eq!(visible, 1, "single CS name remains stringified");
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1);
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn release_expanded_date_keeps_literal_verb_name_and_stored_description() {
    for ending in ["\n", "\r\n"] {
        for optional in ["", "[\\releasedate]"] {
            let source = format!("{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n\\IncludeInRelease{{\\releasedate}}{optional}{{\\verb}}{{Printed \\verb* and \\ref{{hidden}}}}\\ref{{live}}\n").replace('\n', ending);
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(
                parsed.errors.is_empty(),
                "{optional:?}: {:?}",
                parsed.errors
            );
            let visible = parsed
                .syntax()
                .descendants_with_tokens()
                .filter(|element| {
                    element.kind() == SyntaxKind::CONTROL_WORD
                        && element
                            .as_token()
                            .is_some_and(|token| token.text() == "\\releasedate")
                })
                .count();
            assert_eq!(visible, 2 + usize::from(!optional.is_empty()));
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1);
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn release_name_stringifies_first_control_word_but_keeps_later_expansion_visible() {
    for ending in ["\n", "\r\n"] {
        for name in [r"\verb\releasedate", r"\verb prefix\releasedate"] {
            let source = format!("{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n\\IncludeInRelease{{2026/06/01}}{{{name}}}{{Printed \\verb and \\ref{{hidden}}}}\\ref{{live}}\n").replace('\n', ending);
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
            let visible = parsed
                .syntax()
                .descendants_with_tokens()
                .filter(|element| {
                    element.kind() == SyntaxKind::CONTROL_WORD
                        && element
                            .as_token()
                            .is_some_and(|token| token.text() == "\\releasedate")
                })
                .count();
            assert_eq!(visible, 2, "binding plus expanded later name token");
            assert!(
                parsed
                    .syntax()
                    .descendants_with_tokens()
                    .any(|element| element.kind() == SyntaxKind::STRINGIFIED_TOKEN
                        && element
                            .as_token()
                            .is_some_and(|token| token.text() == "\\verb")),
                "first name token has explicit stringified ownership"
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1);
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn release_name_first_token_respects_comment_and_line_states() {
    for ending in ["\n", "\r\n", "\r"] {
        for (prefix, stringified) in [
            ("", true),
            ("% ignored\n", true),
            ("% ignored\n  \t", true),
            ("% first\n  % second\n  ", true),
            (" ", false),
            ("\t", false),
            ("\n", false),
            ("% ignored\n\n", false),
        ] {
            let source = format!("{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n\\IncludeInRelease{{2026/06/01}}{{{prefix}\\releasedate}}{{Printed \\verb and \\ref{{hidden}}}}\\ref{{live}}\n").replace('\n', ending);
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{prefix:?}: {:?}", parsed.errors);
            let calls = parsed
                .syntax()
                .descendants_with_tokens()
                .filter(|element| {
                    element.kind() == SyntaxKind::CONTROL_WORD
                        && element
                            .as_token()
                            .is_some_and(|token| token.text() == "\\releasedate")
                })
                .count();
            assert_eq!(calls, if stringified { 1 } else { 2 }, "{prefix:?}");
            assert_eq!(
                parsed
                    .syntax()
                    .descendants_with_tokens()
                    .filter(|element| element.kind() == SyntaxKind::STRINGIFIED_TOKEN
                        && element
                            .as_token()
                            .is_some_and(|token| token.text() == "\\releasedate"))
                    .count(),
                usize::from(stringified),
                "{prefix:?}"
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1);
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn release_name_stringifies_control_symbols_without_consuming_the_description() {
    for name in [r"\%", r"\{", r"\}", r"\Ω"] {
        let source = format!(
            "{RELEASE_SETUP}\\IncludeInRelease{{2026/06/01}}{{{name}}}{{Printed \\verb and \\ref{{hidden}}}}\\ref{{live}}\n"
        );
        let parsed = parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{name:?}: {:?}", parsed.errors);
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::STRINGIFIED_TOKEN
                    && element.as_token().is_some_and(|token| token.text() == name))
        );
        let model = SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1);
        assert_eq!(model.refs()[0].name, "live");
    }
}

#[test]
fn release_name_first_token_stringifies_special_characters() {
    for ending in ["\n", "\r\n"] {
        for name in [
            "$", "&", "_", "^", "#", "$plain", "&plain", "_plain", "^plain", "alphabet", "Ωword",
        ] {
            let source = format!("{RELEASE_SETUP}\\IncludeInRelease{{2026/06/01}}{{{name}}}{{Printed \\verb and \\ref{{hidden}}}}\\ref{{live}}\n").replace('\n', ending);
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{name:?}: {:?}", parsed.errors);
            let first = name.chars().next().unwrap().to_string();
            let start = source.rfind("\\IncludeInRelease{2026/06/01}").unwrap()
                + "\\IncludeInRelease{2026/06/01}{".len();
            let syntax = parsed.syntax();
            let token = syntax
                .token_at_offset(start.try_into().unwrap())
                .right_biased()
                .unwrap();
            assert_eq!(token.kind(), SyntaxKind::STRINGIFIED_TOKEN, "{name}");
            assert_eq!(token.text(), first);
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1);
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}

#[test]
fn late_source_qualified_readers_keep_exact_roles_on_wide_roots() {
    for ending in ["\n", "\r\n"] {
        let prefix = "Ordinary prose block.\n\n".repeat(1024);
        for (setup, suffix, role) in [
            (
                COMMENT_SETUP,
                "\\begin{comment}\n\\ref{hidden}\n\\end {comment} \\ref{live}\n",
                SyntaxKind::EXCLUDED_BODY,
            ),
            (
                RELEASE_SETUP,
                "\\IncludeInRelease{2020/01/01}{\\verb}{\\ref{hidden}}\n\\ref{live}\n",
                SyntaxKind::STRINGIFIED_TOKEN,
            ),
        ] {
            let source = format!("{prefix}{setup}{suffix}").replace('\n', ending);
            let parsed =
                parse_with_declarations(&source, LatexFlavor::Package, &Default::default());
            assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
            assert_eq!(parsed.syntax().text().to_string(), source);
            assert!(parsed.syntax().children_with_tokens().count() >= 1024);
            assert!(
                parsed
                    .syntax()
                    .descendants_with_tokens()
                    .any(|element| element.kind() == role)
            );
            let model = SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1);
            assert_eq!(model.refs()[0].name, "live");
        }
    }
}
