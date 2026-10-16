use tex_ls_formatter::formatter::{FormatStyle, WrapMode, format_with_style_flavored, perturb};
use tex_ls_parser::parser::{LatexFlavor, LexConfig, parse_with_flavor};
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxKind;

const COMMENT_SETUP: &str =
    include_str!("../../tex-ls-parser/tests/source_readers/comment_setup.tex");
const RELEASE_SETUP: &str =
    include_str!("../../tex-ls-parser/tests/source_readers/release_setup.sty");

#[test]
fn qualified_readers_survive_formatting_without_changing_token_data() {
    for (source, dtx) in [
        (
            format!(
                "{COMMENT_SETUP}\\begin{{comment}}\n\\ref{{hidden}}\nignored \\end {{comment}} \\ref{{live}}\n"
            ),
            false,
        ),
        (
            format!(
                "%    \\begin{{macrocode}}\n{COMMENT_SETUP}%    \\end{{macrocode}}\n\\begin{{comment}}\n\\ref{{hidden}}\n  \\end {{comment}} \\ref{{live}}\n"
            ),
            true,
        ),
        (
            format!(
                "{RELEASE_SETUP}\\IncludeInRelease{{date}}[previous]%\n{{\\feature}}{{Literal \\verb and \\ref{{printed}}}}%\n\\ref{{live}}\n"
            ),
            false,
        ),
        (
            format!(
                "{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n\\IncludeInRelease{{\\releasedate}}[\\releasedate]{{\\verb}}{{Literal \\verb* and \\ref{{printed}}}}\\ref{{live}}\n"
            ),
            false,
        ),
        (
            format!(
                "{RELEASE_SETUP}\\def\\releasedate{{2026/06/01}}\n\\IncludeInRelease{{2026/06/01}}{{% name\n  \\verb prefix\\releasedate}}{{Literal \\verb and \\ref{{printed}}}}\\ref{{live}}\n"
            ),
            false,
        ),
        (
            format!(
                "{RELEASE_SETUP}\\IncludeInRelease{{2026/06/01}}{{alphabet}}{{Literal \\verb and \\ref{{printed}}}}\n\\IncludeInRelease{{2026/06/01}}{{$plain}}{{Printed \\verb*}}\n\\IncludeInRelease{{2026/06/01}}{{Ωword}}{{Printed \\verb}}\\ref{{live}}\n"
            ),
            false,
        ),
    ] {
        let config = LexConfig {
            flavor: LatexFlavor::Package,
            dtx,
        };
        let protected = |source: &str| {
            parse_with_flavor(source, config)
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| {
                    matches!(
                        token.kind(),
                        SyntaxKind::VERB
                            | SyntaxKind::EXCLUDED_BODY
                            | SyntaxKind::STRINGIFIED_TOKEN
                    )
                })
                .map(|token| token.text().to_owned())
                .collect::<Vec<_>>()
        };
        for source in [source.clone(), source.replace('\n', "\r\n")] {
            for wrap in [WrapMode::Reflow, WrapMode::Stable, WrapMode::Preserve] {
                for line_width in [20, 80] {
                    let style = FormatStyle {
                        wrap,
                        line_width,
                        ..Default::default()
                    };
                    let output = format_with_style_flavored(&source, style, config).unwrap();
                    assert_eq!(protected(&output), protected(&source));
                    assert_eq!(
                        perturb::nontrivia_content(&output, config),
                        perturb::nontrivia_content(&source, config)
                    );
                    assert_eq!(
                        format_with_style_flavored(&output, style, config).unwrap(),
                        output
                    );
                    let parsed = parse_with_flavor(&output, config);
                    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
                    let model = SemanticModel::build(&parsed.syntax());
                    assert_eq!(model.refs().len(), 1);
                    assert_eq!(model.refs()[0].name, "live");
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
