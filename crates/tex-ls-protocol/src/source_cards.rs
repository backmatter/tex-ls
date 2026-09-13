//! Bounded source-derived cards; dynamic TeX remains visible, never evaluated.
use super::*;

pub fn excerpt(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut result: String = chars.by_ref().take(1200).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}
fn card(label: &str, body: &str) -> String {
    format!(
        "{label}\n\n{}",
        excerpt(body)
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}
pub fn manual_bodies(snapshot: &Analysis, path: &Path) -> Vec<(String, String)> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let model = snapshot.semantic_model(file);
    let mut ends: Vec<_> = model
        .bibitems()
        .iter()
        .map(|item| item.range.start())
        .chain(
            snapshot
                .parsed_tree(file)
                .descendants()
                .filter(|node| {
                    node.kind() == SyntaxKind::END
                        && tex_ls_parser::ast::environment_name(node)
                            .is_some_and(|name| name == "thebibliography")
                })
                .map(|node| node.text_range().start()),
        )
        .collect();
    ends.sort();
    model
        .bibitems()
        .iter()
        .map(|entry| {
            let start = entry.range.end();
            let end = ends
                .get(ends.partition_point(|end| *end < start))
                .copied()
                .map(usize::from)
                .unwrap_or(snapshot.file_text(file).len());
            (
                entry.name.to_string(),
                excerpt(&snapshot.file_text(file)[usize::from(start)..end]),
            )
        })
        .collect()
}
pub fn manual(snapshot: &Analysis, path: &Path, key: &str) -> Option<String> {
    let key = key.to_lowercase();
    let definitions: Vec<_> = snapshot
        .resolve_citations()
        .namespace_members(path)
        .into_iter()
        .flat_map(|path| manual_bodies(snapshot, path))
        .filter(|(name, _)| name.to_lowercase() == key)
        .map(|(_, body)| body)
        .collect();
    let [body] = definitions.as_slice() else {
        return None;
    };
    Some(card("Manual bibliography item", body))
}
pub fn glossary(snapshot: &Analysis, path: &Path, key: &str) -> Option<String> {
    let sites: Vec<_> = crate::glossary::occurrences(snapshot, path, key)
        .into_iter()
        .filter(|(_, _, definition)| *definition)
        .collect();
    let [(path, range, _)] = sites.as_slice() else {
        return None;
    };
    let file = snapshot.lookup_file(path)?;
    let command = snapshot
        .parsed_tree(file)
        .descendants()
        .filter(|node| {
            node.kind() == SyntaxKind::COMMAND && node.text_range().contains_range(*range)
        })
        .min_by_key(|node| node.text_range().len())?;
    let groups: Vec<_> = command
        .children()
        .filter(|node| {
            node.kind() == SyntaxKind::GROUP && node.text_range().start() > range.start()
        })
        .map(|node| node.text().to_string())
        .collect();
    Some(card("Glossary/acronym entry", &groups.join("\n")))
}
pub fn argument_docs(name: &str) -> &'static [&'static str] {
    match name {
        "frac" | "dfrac" | "tfrac" => &["Numerator", "Denominator"],
        "sqrt" => &["Root index (optional)", "Radicand"],
        "includegraphics" => &[
            "Graphic options (width, height, scale, trim, …)",
            "Image filename",
        ],
        "usepackage" => &["Package options", "Comma-separated package names"],
        "documentclass" => &["Class options", "Document class name"],
        "textbf" | "textit" | "emph" => &["Text to emphasize"],
        "href" => &["Target URL", "Displayed text"],
        "input" | "include" => &["Source filename"],
        "label" => &["Unique cross-reference key"],
        _ => &[],
    }
}
