use tex_ls_formatter::formatter::{FormatStyle, WrapMode, format_with_style_flavored, perturb};
use tex_ls_parser::parser::{LatexFlavor, LexConfig, parse_with_flavor};
use tex_ls_parser::syntax::SyntaxKind;

#[test]
fn documentary_optional_headers_keep_literal_names_in_the_header() {
    let config = LexConfig {
        flavor: LatexFlavor::Document,
        dtx: true,
    };
    let literal_names = |text: &str| {
        parse_with_flavor(text, config)
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::VERB)
            .map(|token| token.text().to_owned())
            .collect::<Vec<_>>()
    };
    for name in ["macro", "function", "variable"] {
        for option in ["", "[pTF, EXP]"] {
            for body in [
                "% Explanation.\n",
                "% Explanation.\n%    \\begin{macrocode}\n\\ExplSyntaxOn\n\\cs_new:Npn \\example:n #1 {#1}\n%    \\end{macrocode}\n",
            ] {
                for newline in ["\n", "\r\n"] {
                    let body = body.replace('\n', newline);
                    let source = format!(
                        "% \\begin{{{name}}}{option} {{ \\example:n }}{newline}{body}% \\end{{{name}}}{newline}"
                    );
                    assert_eq!(literal_names(&source), [" \\example:n "]);
                    for wrap in [WrapMode::Reflow, WrapMode::Stable, WrapMode::Preserve] {
                        for width in [20, 80] {
                            let style = FormatStyle {
                                wrap,
                                line_width: width,
                                ..FormatStyle::default()
                            };
                            let output =
                                format_with_style_flavored(&source, style, config).unwrap();
                            assert_eq!(
                                literal_names(&output),
                                literal_names(&source),
                                "{name}, {option}, {wrap:?}, width {width}: {output:?}"
                            );
                            assert_eq!(
                                perturb::nontrivia_content(&output, config),
                                perturb::nontrivia_content(&source, config)
                            );
                            assert_eq!(
                                format_with_style_flavored(&output, style, config).unwrap(),
                                output
                            );
                            assert!(parse_with_flavor(&output, config).errors.is_empty());
                            perturb::check_trivia_convergence(
                                &source,
                                config,
                                perturb::DEFAULT_SINGLE_FLIP_SAMPLES,
                                |text| {
                                    format_with_style_flavored(text, style, config)
                                        .map_err(|error| error.to_string())
                                },
                            )
                            .unwrap();
                        }
                    }
                }
            }
        }
    }
}
