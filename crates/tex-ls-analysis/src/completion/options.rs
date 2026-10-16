//! Literal option segments; nesting and dynamic values never change parse shape.
use super::*;

pub fn classify(
    root: &SyntaxNode,
    offset: usize,
    declared: &ResolvedDeclarations,
    file_roles: &crate::external::links::FileRoleContext,
) -> Option<CompletionContext> {
    let ancestors = cursor_ancestors(root, offset);
    classify_with_ancestors(root, offset, declared, file_roles, &ancestors)
}

pub(super) fn classify_with_ancestors(
    root: &SyntaxNode,
    offset: usize,
    declared: &ResolvedDeclarations,
    file_roles: &crate::external::links::FileRoleContext,
    ancestors: &[SyntaxNode],
) -> Option<CompletionContext> {
    let at = TextSize::from(offset as u32);
    let group = ancestors
        .iter()
        .filter(|node| {
            node.kind() == SyntaxKind::OPTIONAL
                && node.text_range().start() < at
                && at <= node.text_range().end()
        })
        .min_by_key(|node| node.text_range().len());
    let (owner, raw, start) = if let Some(group) = group {
        let raw = group.text().to_string();
        record_classification_work(0, raw.len());
        (
            group.parent()?,
            raw,
            usize::from(group.text_range().start()) + 1,
        )
    } else {
        // Ordinary unclosed brackets can follow a command without belonging to
        // its CST. Find the nearest preceding command through its
        // cached end-offset index, then retain only its bracket suffix and terminator.
        let owner = preceding_owner(root, at, file_roles)?;
        let start = usize::from(owner.text_range().end());
        let raw = bare_option_text(root, start)?;
        let tail = raw.get(..offset.checked_sub(start)?)?;
        if !tail.starts_with('[') || tail.contains([']', '\n', '\r', '%']) {
            return None;
        }
        (owner, raw, start + 1)
    };
    classify_parts(owner, &raw, start, offset, declared, file_roles)
}

fn classify_parts(
    owner: SyntaxNode,
    raw: &str,
    start: usize,
    offset: usize,
    declared: &ResolvedDeclarations,
    file_roles: &crate::external::links::FileRoleContext,
) -> Option<CompletionContext> {
    let name = if owner.kind() == SyntaxKind::BEGIN {
        crate::ast::environment_name(&owner)?.to_string()
    } else if owner.kind() == SyntaxKind::COMMAND {
        command_name(&owner)?.to_string()
    } else {
        return None;
    };
    if owner.kind() == SyntaxKind::COMMAND
        && !file_roles.allows(&owner)
        && !declared.options.contains_key(&name)
    {
        return None;
    }
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

fn preceding_owner(
    root: &SyntaxNode,
    at: TextSize,
    roles: &crate::external::links::FileRoleContext,
) -> Option<SyntaxNode> {
    let range = roles.preceding_command_range(at)?;
    let token = tex_ls_parser::syntax::token_at_offset(root, range.start()).right_biased()?;
    token.parent_ancestors().find(|node| {
        record_classification_work(1, 0);
        node.text_range() == range && matches!(node.kind(), SyntaxKind::COMMAND | SyntaxKind::BEGIN)
    })
}

fn bare_option_text(root: &SyntaxNode, start: usize) -> Option<String> {
    let mut current =
        tex_ls_parser::syntax::token_at_offset(root, TextSize::from(start as u32)).right_biased();
    let mut raw = String::new();
    let (mut braces, mut brackets, mut escaped) = (0usize, 0usize, false);
    while let Some(token) = current {
        record_classification_work(1, 0);
        let lo = start.saturating_sub(usize::from(token.text_range().start()));
        for ch in token.text().get(lo..)?.chars() {
            if matches!(ch, '\n' | '\r' | '%') {
                return Some(raw);
            }
            if raw.is_empty() && ch != '[' {
                return None;
            }
            raw.push(ch);
            record_classification_work(0, ch.len_utf8());
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
                        return Some(raw);
                    }
                }
                _ => {}
            }
        }
        current = token.next_token();
    }
    Some(raw)
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
    // Independent reference for the original CST-wide owner selection and
    // malformed bracket extraction. The schema/segment code is shared so this
    // checks the changed indexing and bounds rather than a copied implementation.
    fn reference_classify(
        root: &SyntaxNode,
        offset: usize,
        declared: &ResolvedDeclarations,
        file_roles: &crate::external::links::FileRoleContext,
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
        classify_parts(owner, &raw, start, offset, declared, file_roles)
    }

    #[test]
    fn indexed_options_match_reference_at_every_cursor_boundary() {
        let mut declared = ResolvedDeclarations::default();
        declared.options.insert(
            "custom".into(),
            std::collections::BTreeMap::from([
                ("width".into(), Vec::new()),
                ("mode".into(), vec!["a".into(), "b".into()]),
            ]),
        );
        for source in [
            r"\includegraphics[width=2cm,clip=true]{plot}",
            r"\includegraphics[trim={1,2,3,4},width=2cm]{plot}",
            r"\includegraphics[width=2cm",
            r"\custom[width=2cm, mode=b] remaining [ordinary] text",
            r"\custom[width={a[b]c}, mode=b]",
            r"\custom[width=é, mode=b]",
            r"\custom[width=\{escaped\},mode=b]",
            r"\custom[width=2cm% mode=b]",
            "\\custom[width=2cm\nmode=b]",
            "\\custom[width=2cm\rmode=b]",
            r"\custom[mode=\foo[mode=b]",
            r"\begin{tikzpicture}\draw[line width=2pt] (0,0);\end{tikzpicture}",
            r"\newcommand{\hidden}{\custom[mode=b]}\custom[mode=a]",
        ] {
            let root = SyntaxNode::new_root(tex_ls_parser::parser::parse(source).green);
            let roles = crate::external::links::FileRoleContext::new(&root);
            for offset in (0..=source.len()).filter(|offset| source.is_char_boundary(*offset)) {
                assert_eq!(
                    classify(&root, offset, &declared, &roles),
                    reference_classify(&root, offset, &declared, &roles),
                    "{source:?} at {offset}",
                );
            }
        }
    }

    #[test]
    fn classification_work_ignores_unrelated_source() {
        for tail in [
            r"\ref{label}",
            r"\cite{paper}",
            r"\input{chapter}",
            r"\includegraphics[wid",
            "ordinary",
        ] {
            let mut baseline = None;
            for lines in [1, 100, 2_048] {
                let source = format!(
                    "{}{tail}",
                    "\\section{Unrelated} text\\label{old}\n".repeat(lines)
                );
                let root = SyntaxNode::new_root(tex_ls_parser::parser::parse(&source).green);
                let sites = tex_ls_parser::semantic::scan_definition_sites(&root);
                let operands = tex_ls_parser::semantic::context::PrimitiveOperandContexts::from_definition_sites(&root, &sites);
                let roles =
                    crate::external::links::FileRoleContext::from_definition_sites_and_operands(
                        &root, &sites, &operands,
                    );
                let offset = source.len() - usize::from(tail.ends_with('}'));
                CLASSIFICATION_WORK.with(|work| work.set(ClassificationWork::default()));
                let _ = crate::completion::classify_context_with_roles(
                    &root,
                    offset,
                    &ResolvedDeclarations::default(),
                    || roles,
                );
                let work = CLASSIFICATION_WORK.with(std::cell::Cell::get);
                assert!(work.nodes < 64, "{tail}: {work:?}");
                assert!(work.copied_bytes < 64, "{tail}: {work:?}");
                if let Some(expected) = baseline {
                    assert_eq!(work, expected, "{tail}, {lines} unrelated lines");
                } else {
                    baseline = Some(work);
                }
            }
        }
    }

    #[test]
    fn classification_work_ignores_same_line_prose_before_cursor() {
        for tail in [" ordinary", r"\includegraphics[wid", r"\ref{label}"] {
            let mut baseline = None;
            for repetitions in [1, 100, 2_048] {
                let source = format!(
                    "\\section{{Intro}} {}{tail}",
                    "unrelated words ".repeat(repetitions)
                );
                let root = SyntaxNode::new_root(tex_ls_parser::parser::parse(&source).green);
                let roles = crate::external::links::FileRoleContext::new(&root);
                let offset = source.len() - usize::from(tail.ends_with('}'));
                CLASSIFICATION_WORK.with(|work| work.set(ClassificationWork::default()));
                let _ = crate::completion::classify_context_with_roles(
                    &root,
                    offset,
                    &ResolvedDeclarations::default(),
                    || roles,
                );
                let work = CLASSIFICATION_WORK.with(std::cell::Cell::get);
                assert!(
                    work.nodes < 64 && work.copied_bytes < 64,
                    "{tail}: {work:?}"
                );
                if let Some(expected) = baseline {
                    assert_eq!(work, expected, "{tail}");
                } else {
                    baseline = Some(work);
                }
            }
        }
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
