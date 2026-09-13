//! Color-picker results and exact edits over proven literal declarations.
use crate::{Analysis, Path, PositionEncoding, convert::lsp_range};
use lsp_types::{Color, ColorInformation, ColorPresentation, Range, TextEdit};

pub fn document_colors(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
) -> Vec<ColorInformation> {
    if !crate::convert::file_kind_for(path).is_latex() {
        return vec![];
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return vec![];
    };
    let idx = snapshot.file_line_index(file, enc);
    tex_ls_analysis::colors::literal_colors(&snapshot.parsed_tree(file))
        .into_iter()
        .map(|item| ColorInformation {
            range: lsp_range(&idx, item.range),
            color: Color {
                red: item.rgb[0] as f32,
                green: item.rgb[1] as f32,
                blue: item.rgb[2] as f32,
                alpha: 1.,
            },
        })
        .collect()
}

pub fn color_presentations(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    range: Range,
    color: Color,
) -> Vec<ColorPresentation> {
    let channels = [color.red, color.green, color.blue];
    if color.alpha != 1.
        || channels
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
        || !crate::convert::file_kind_for(path).is_latex()
    {
        return vec![];
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return vec![];
    };
    let idx = snapshot.file_line_index(file, enc);
    let Some(item) = tex_ls_analysis::colors::literal_colors(&snapshot.parsed_tree(file))
        .into_iter()
        .find(|item| lsp_range(&idx, item.range) == range)
    else {
        return vec![];
    };
    let model = if item.model == "gray" && !(color.red == color.green && color.green == color.blue)
    {
        "rgb"
    } else {
        &item.model
    };
    let spec = match model {
        "HTML" => channels
            .iter()
            .map(|v| format!("{:02X}", (v * 255.).round() as u8))
            .collect::<String>(),
        "RGB" => channels
            .iter()
            .map(|v| format!("{}", (v * 255.).round() as u8))
            .collect::<Vec<_>>()
            .join(","),
        "gray" => decimal(color.red),
        _ => channels
            .iter()
            .map(|v| decimal(*v))
            .collect::<Vec<_>>()
            .join(","),
    };
    vec![ColorPresentation {
        label: format!("{model}: {spec}"),
        text_edit: Some(TextEdit {
            range,
            new_text: spec,
        }),
        additional_text_edits: (model != item.model).then(|| {
            vec![TextEdit {
                range: lsp_range(&idx, item.model_range),
                new_text: model.into(),
            }]
        }),
    }]
}
fn decimal(value: f32) -> String {
    if value == 0. {
        return "0".into();
    }
    let text = format!("{value:.8}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;
    fn database(source: &str) -> IncrementalDatabase {
        let mut db = IncrementalDatabase::default();
        db.apply_change(Path::new(fixture_path!("/colors.tex")), source, None);
        db
    }
    #[test]
    fn literal_models_and_exact_picker_edits_in_both_encodings() {
        let source = "😀 \\definecolor{red}{HTML}{ FF0000 }\n\\providecolor{green}{rgb}{0,1,0}\n\\definecolor{blue}{RGB}{0,0,255}\n\\definecolor{shade}{ gray }% preserve\n{ .5 }\n";
        let db = database(source);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let snapshot = db.snapshot();
            let path = Path::new(fixture_path!("/colors.tex"));
            let colors = document_colors(&snapshot, path, enc);
            assert_eq!(colors.len(), 4);
            assert_eq!(
                [
                    colors[0].color.red,
                    colors[1].color.green,
                    colors[2].color.blue
                ],
                [1.; 3]
            );
            assert_eq!(colors[3].color.red, 0.5);
            for (i, item) in colors.iter().enumerate() {
                let chosen = Color {
                    red: 0.,
                    green: 1.,
                    blue: 0.,
                    alpha: 1.,
                };
                let presentations = color_presentations(&snapshot, path, enc, item.range, chosen);
                let presentation = &presentations[0];
                let mut edits = presentation
                    .additional_text_edits
                    .clone()
                    .unwrap_or_default();
                edits.push(presentation.text_edit.clone().unwrap());
                let idx = snapshot.file_line_index(snapshot.lookup_file(path).unwrap(), enc);
                let mut edits: Vec<_> = edits
                    .into_iter()
                    .map(|e| {
                        (
                            idx.offset_at(e.range.start.line, e.range.start.character)
                                ..idx.offset_at(e.range.end.line, e.range.end.character),
                            e.new_text,
                        )
                    })
                    .collect();
                edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
                let mut changed = source.to_owned();
                for (range, text) in edits {
                    changed.replace_range(range, &text);
                }
                assert!(changed.contains("% preserve\n"));
                let updated = database(&changed);
                let actual = document_colors(&updated.snapshot(), path, enc);
                assert_eq!(actual.len(), 4);
                assert_eq!(actual[i].color, chosen);
            }
        }
    }
    #[test]
    fn malformed_dynamic_commented_and_protected_values_are_skipped() {
        for source in [
            "\\definecolor{x}{HTML}{FF0000",
            "\\definecolor{x}{rgb}{1,0,2}",
            "\\definecolor{x}{rgb}{NaN,0,0}",
            "\\definecolor{x}{RGB}{1.5,0,0}",
            "\\definecolor{x}{rgb}{1e0,0,0}",
            "\\definecolor{x}{HTML}{fff}",
            "\\definecolor{x}{HTML}{FF% comment\n0000}",
            "\\definecolor{x}{HTML}{\\value}",
            "\\definecolor{x}{rgb/HTML}{1,0,0/FF0000}",
            "\\colorlet{x}{red!50!blue}",
            "% \\definecolor{x}{HTML}{FF0000}\n",
            "\\begin{verbatim}\n\\definecolor{x}{HTML}{FF0000}\n\\end{verbatim}",
        ] {
            assert!(
                document_colors(
                    &database(source).snapshot(),
                    Path::new(fixture_path!("/colors.tex")),
                    PositionEncoding::Utf16
                )
                .is_empty(),
                "{source}"
            );
        }
    }
    #[test]
    fn negative_zero_is_a_valid_literal_after_picking() {
        assert_eq!(decimal(-0.0), "0");
    }
    #[test]
    fn invalid_picker_ranges_and_alpha_are_refused() {
        let db = database("\\definecolor{x}{gray}{.5}");
        let snapshot = db.snapshot();
        let path = Path::new(fixture_path!("/colors.tex"));
        let enc = PositionEncoding::Utf16;
        let item = &document_colors(&snapshot, path, enc)[0];
        assert!(color_presentations(&snapshot, path, enc, Range::default(), item.color).is_empty());
        for color in [
            Color {
                alpha: 0.5,
                ..item.color
            },
            Color {
                red: f32::NAN,
                ..item.color
            },
            Color {
                blue: 2.,
                ..item.color
            },
        ] {
            assert!(color_presentations(&snapshot, path, enc, item.range, color).is_empty());
        }
    }
}
