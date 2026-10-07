//! Literal xcolor declarations. No macro expansion, color expressions or aliases.
use rowan::{TextRange, TextSize};
use tex_ls_parser::{
    ast::{command_name, nth_group, nth_group_inner},
    syntax::{SyntaxKind, SyntaxNode},
};

#[derive(Debug, Clone, PartialEq)]
pub struct LiteralColor {
    pub model: String,
    pub model_range: TextRange,
    pub range: TextRange,
    pub rgb: [f64; 3],
}

fn trimmed(range: TextRange, text: &str) -> (TextRange, &str) {
    let value = text.trim();
    let start = range.start() + TextSize::from((text.len() - text.trim_start().len()) as u32);
    (
        TextRange::at(start, TextSize::from(value.len() as u32)),
        value,
    )
}

pub fn literal_colors(root: &SyntaxNode) -> Vec<LiteralColor> {
    literal_colors_with_sites(root, &tex_ls_parser::semantic::scan_definition_sites(root))
}

pub fn literal_colors_with_sites(
    root: &SyntaxNode,
    sites: &[tex_ls_parser::semantic::DefSite],
) -> Vec<LiteralColor> {
    let operands =
        tex_ls_parser::semantic::context::PrimitiveOperandContexts::from_definition_sites(
            root, sites,
        );
    literal_colors_with_operands(root, &operands)
}

pub fn literal_colors_with_operands(
    root: &SyntaxNode,
    operands: &tex_ls_parser::semantic::context::PrimitiveOperandContexts,
) -> Vec<LiteralColor> {
    let protected = tex_ls_parser::semantic::context::TokenListContexts::new(root);
    root.descendants()
        .filter(|n| n.kind() == SyntaxKind::COMMAND)
        .filter(|node| !protected.contains(node.text_range()) && !operands.contains_head(node))
        .filter_map(|node| {
            let name = command_name(&node)?;
            if !matches!(name.as_str(), "definecolor" | "providecolor") {
                return None;
            }
            for index in 0..3 {
                let group = nth_group(&node, index)?;
                if group.first_token()?.kind() != SyntaxKind::L_BRACE
                    || group.last_token()?.kind() != SyntaxKind::R_BRACE
                {
                    return None;
                }
            }
            let (_, name) = nth_group_inner(&node, 0)?;
            if name.trim().is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_alphanumeric() || " -_:".contains(c))
            {
                return None;
            }
            let (model_range, model) = nth_group_inner(&node, 1)?;
            let (model_range, model) = trimmed(model_range, &model);
            let (range, spec) = nth_group_inner(&node, 2)?;
            let (range, spec) = trimmed(range, &spec);
            let rgb = match model {
                "HTML" if spec.len() == 6 && spec.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    let n = u32::from_str_radix(spec, 16).ok()?;
                    [
                        ((n >> 16) & 255) as f64 / 255.,
                        ((n >> 8) & 255) as f64 / 255.,
                        (n & 255) as f64 / 255.,
                    ]
                }
                "rgb" | "RGB" | "gray" => {
                    let values: Option<Vec<f64>> = spec
                        .split(',')
                        .map(|part| {
                            let part = part.trim();
                            if !part.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
                                return None;
                            }
                            let n: f64 = part.parse().ok()?;
                            let max = if model == "RGB" { 255. } else { 1. };
                            (n.is_finite()
                                && (0. ..=max).contains(&n)
                                && (model != "RGB" || n.fract() == 0.))
                                .then_some(n / max)
                        })
                        .collect();
                    let values = values?;
                    match values.as_slice() {
                        [v] if model == "gray" => [*v; 3],
                        [r, g, b] if model != "gray" => [*r, *g, *b],
                        _ => return None,
                    }
                }
                _ => return None,
            };
            Some(LiteralColor {
                model: model.into(),
                model_range,
                range,
                rgb,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_color_heads_are_ignored_and_nested_live_declarations_remain() {
        let source = r"\meaning\definecolor{fake}{HTML}{FF0000}\detokenize{\definecolor{printed}{HTML}{FFFFFF}}\meaning\ref{\definecolor{nested}{HTML}{00FF00}}\definecolor{live}{HTML}{0000FF}";
        let root = tex_ls_parser::parser::parse(source).syntax();
        let colors = literal_colors(&root);
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0].rgb, [0., 1., 0.]);
        assert_eq!(colors[1].rgb, [0., 0., 1.]);
        assert_eq!(
            &source[usize::from(colors[0].range.start())..usize::from(colors[0].range.end())],
            "00FF00"
        );
    }
}
