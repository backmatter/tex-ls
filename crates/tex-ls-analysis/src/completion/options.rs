//! Literal option segments; nesting and dynamic values never change parse shape.
use super::*;

pub fn classify(
    root: &SyntaxNode,
    offset: usize,
    declared: &ResolvedDeclarations,
) -> Option<CompletionContext> {
    let at = TextSize::from(offset as u32);
    let group = root
        .descendants()
        .filter(|node| {
            node.kind() == SyntaxKind::OPTIONAL
                && node.text_range().start() < at
                && at <= node.text_range().end()
        })
        .min_by_key(|node| node.text_range().len());
    let (owner, raw, start) = if let Some(group) = group {
        (
            group.parent()?,
            group.text().to_string(),
            usize::from(group.text_range().start()) + 1,
        )
    } else {
        // A bare unclosed bracket is valid ordinary text in the parser. At a
        // known command's immediate tail, completion can still offer its schema.
        let owner = root
            .descendants()
            .filter(|node| {
                matches!(node.kind(), SyntaxKind::COMMAND | SyntaxKind::BEGIN)
                    && node.text_range().end() < at
            })
            .max_by_key(|node| node.text_range().end())?;
        let start = usize::from(owner.text_range().end());
        let source = root.text().to_string();
        let tail = source.get(start..offset)?;
        if !tail.starts_with('[') || tail.contains([']', '\n', '\r', '%']) {
            return None;
        }
        // Retain the suffix too: an explicit schema may describe a command
        // whose brackets are ordinary text in the CST. Replacing only the
        // typed prefix would leave the rest of its key/value behind.
        let rest = source[start..].split(['\n', '\r', '%']).next()?;
        let (mut braces, mut brackets, mut escaped) = (0usize, 0usize, false);
        let mut end = rest.len();
        for (i, ch) in rest.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '{' => braces += 1,
                '}' => braces = braces.saturating_sub(1),
                '[' if braces == 0 => brackets += 1,
                ']' if braces == 0 => {
                    brackets = brackets.saturating_sub(1);
                    if brackets == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        (owner, rest[..end].to_owned(), start + 1)
    };
    let name = if owner.kind() == SyntaxKind::BEGIN {
        crate::ast::environment_name(&owner)?.to_string()
    } else if owner.kind() == SyntaxKind::COMMAND {
        command_name(&owner)?.to_string()
    } else {
        return None;
    };
    let end = start - 1 + raw.len() - usize::from(raw.ends_with(']'));
    if offset > end {
        return None;
    }
    let inner = &raw[1..raw.len() - usize::from(raw.ends_with(']'))];
    let relative = offset - start;
    let (mut depth, mut escaped, mut segment_start, mut segment_end, mut equals) =
        (0usize, false, 0, inner.len(), None);
    for (i, ch) in inner.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        match ch {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if i < relative {
                    segment_start = i + 1;
                    equals = None;
                } else {
                    segment_end = i;
                    break;
                }
            }
            '=' if depth == 0 => equals = Some(i),
            _ => {}
        }
    }
    let value_key = equals
        .filter(|i| relative > *i)
        .map(|i| inner[segment_start..i].trim());
    let lo = equals
        .filter(|i| relative > *i)
        .map_or(segment_start, |i| i + 1);
    let hi = if value_key.is_none() {
        equals.unwrap_or(segment_end)
    } else {
        segment_end
    };
    let selected = &inner[lo..hi];
    if selected.contains(['{', '}', '\\', '%', '[', ']']) {
        return None;
    }
    let (lo, hi) = if selected.trim().is_empty() {
        (relative, relative)
    } else {
        (
            lo + selected.len() - selected.trim_start().len(),
            hi - selected.len() + selected.trim_end().len(),
        )
    };
    if relative < lo || relative > hi {
        return None;
    }
    let mut schema = std::collections::BTreeMap::<String, Vec<String>>::new();
    let entries: &[(&str, &[&str])] = match name.as_str() {
        "includegraphics" => &[
            ("width", &[]),
            ("height", &[]),
            ("scale", &[]),
            ("angle", &[]),
            ("trim", &[]),
            ("clip", &["true", "false"]),
            ("keepaspectratio", &["true", "false"]),
            ("page", &[]),
            ("draft", &["true", "false"]),
            ("origin", &["c", "l", "r", "t", "b", "B"]),
        ],
        "draw" | "path" | "node" | "fill" | "filldraw" | "tikz" | "tikzpicture" => &[
            ("line width", &[]),
            ("draw", &[]),
            ("fill", &[]),
            ("color", &[]),
            ("opacity", &[]),
            ("line cap", &["butt", "rect", "round"]),
            ("line join", &["miter", "bevel", "round"]),
            ("dashed", &[]),
            ("thick", &[]),
        ],
        "documentclass" | "LoadClass" => &[
            ("10pt", &[]),
            ("11pt", &[]),
            ("12pt", &[]),
            ("a4paper", &[]),
            ("letterpaper", &[]),
            ("oneside", &[]),
            ("twoside", &[]),
            ("draft", &[]),
            ("final", &[]),
        ],
        "usepackage" | "RequirePackage" => &[],
        _ => &[],
    };
    for (key, values) in entries {
        schema.insert((*key).into(), values.iter().map(|v| (*v).into()).collect());
    }
    let mut package_names = Vec::new();
    if matches!(name.as_str(), "usepackage" | "RequirePackage") {
        let packages = owner
            .children()
            .find(|n| n.kind() == SyntaxKind::GROUP)?
            .text()
            .to_string();
        for package in packages.trim_matches(['{', '}']).split(',').map(str::trim) {
            package_names.push(package.to_owned());
            let keys: &[&str] = match package {
                "graphicx" => &["draft", "final", "pdftex", "luatex", "xetex"],
                "hyperref" => &[
                    "colorlinks",
                    "hidelinks",
                    "bookmarks",
                    "unicode",
                    "linkcolor",
                    "citecolor",
                    "urlcolor",
                    "pdfauthor",
                    "pdftitle",
                ],
                "geometry" => &[
                    "margin",
                    "left",
                    "right",
                    "top",
                    "bottom",
                    "a4paper",
                    "landscape",
                    "includehead",
                    "includefoot",
                ],
                "xcolor" => &["dvipsnames", "svgnames", "x11names", "table"],
                _ => &[],
            };
            for key in keys {
                schema.insert((*key).into(), Vec::new());
            }
            if let Some(custom) = declared.options.get(&format!("package:{package}")) {
                schema.extend(custom.clone());
            }
        }
    }
    if let Some(custom) = declared.options.get(&name) {
        schema = custom.clone();
    }
    if schema.is_empty() && package_names.is_empty() {
        return None;
    }
    let values = if let Some(key) = value_key {
        schema.get(key)?.clone()
    } else {
        schema.into_keys().collect()
    };
    Some(CompletionContext::Option {
        owner: name,
        packages: package_names,
        value: value_key.is_some(),
        prefix: inner[lo..relative].to_owned(),
        values,
        range: rowan::TextRange::new(
            TextSize::from((start + lo) as u32),
            TextSize::from((start + hi) as u32),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(source: &str) -> CompletionContext {
        let offset = source.find('|').unwrap();
        let text = source.replace('|', "");
        crate::completion::classify_context(
            &SyntaxNode::new_root(tex_ls_parser::parser::parse(&text).green),
            offset,
        )
    }
    #[test]
    fn keys_values_and_nested_segments() {
        for source in [
            r"\includegraphics[wid|th=2cm]{plot}",
            r"\includegraphics[trim={1,2,3,4},wid|]{plot}",
            r"\includegraphics[wid|",
        ] {
            let CompletionContext::Option {
                prefix,
                values,
                range,
                ..
            } = context(source)
            else {
                panic!("{source}")
            };
            assert_eq!(prefix, "wid");
            assert!(values.contains(&"width".into()));
            assert!(range.len() >= TextSize::from(3));
        }
        let CompletionContext::Option { values, .. } = context(r"\includegraphics[clip=tr|ue]{x}")
        else {
            panic!("value")
        };
        assert_eq!(values, ["true", "false"]);
        assert!(matches!(
            context(r"\includegraphics[width={\foo|}]{x}"),
            CompletionContext::CommandName { .. }
        ));
    }
    #[test]
    fn tikz_multiword_key() {
        let CompletionContext::Option { prefix, values, .. } =
            context(r"\begin{tikzpicture}\draw[line w|] (0,0)--(1,1);\end{tikzpicture}")
        else {
            panic!("TikZ options")
        };
        assert_eq!(prefix, "line w");
        assert!(values.contains(&"line width".into()));
    }
}
