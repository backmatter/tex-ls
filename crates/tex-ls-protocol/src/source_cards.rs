//! Bounded source-derived cards; dynamic TeX remains visible, never evaluated.
use rowan::{TextRange, TextSize};
use std::path::Path;
use tex_ls_analysis::incremental::Analysis;
use tex_ls_parser::syntax::SyntaxKind;

pub fn excerpt(text: &str) -> String {
    excerpt_chars(text.trim().chars())
}

fn excerpt_chars(mut chars: impl Iterator<Item = char>) -> String {
    let mut result: String = chars.by_ref().take(1200).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}
fn card(label: &str, body: &str) -> String {
    format!("{label}\n\n{}", literal(body))
}

pub(crate) fn literal(text: &str) -> String {
    literal_excerpt(&excerpt(text))
}

fn literal_excerpt(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
#[cfg(test)]
thread_local! {
    pub(crate) static COMMENT_CARD_COPY_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Lines have already had their comment prefixes and outer whitespace removed.
pub(crate) fn literal_comment_lines(lines: &[&str]) -> String {
    literal_joined(lines, "\n")
}

fn literal_joined(parts: &[&str], separator: &str) -> String {
    // Separators are whitespace. Trim only the outer margins before streaming
    // the joined prefix, preserving internal blank lines and group whitespace.
    let Some(first) = parts.iter().position(|part| !part.trim().is_empty()) else {
        return String::new();
    };
    let last = parts
        .iter()
        .rposition(|part| !part.trim().is_empty())
        .unwrap();
    let chars = parts[first..=last]
        .iter()
        .enumerate()
        .flat_map(|(index, part)| {
            let part = if index == 0 { part.trim_start() } else { part };
            let part = if index == last - first {
                part.trim_end()
            } else {
                part
            };
            (index > 0)
                .then_some(separator)
                .into_iter()
                .flat_map(str::chars)
                .chain(part.chars())
        });
    #[cfg(test)]
    let chars = chars.inspect(|ch| {
        COMMENT_CARD_COPY_BYTES.with(|bytes| bytes.set(bytes.get() + ch.len_utf8()));
    });
    literal_excerpt(&excerpt_chars(chars))
}

#[cfg(test)]
thread_local! {
    static MANUAL_CARD_EXCERPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn manual_bodies(snapshot: &Analysis, path: &Path) -> Vec<(String, String)> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let model = snapshot.semantic_model(file);
    if model.bibitems().is_empty() {
        return Vec::new();
    }
    let ends = snapshot.manual_bib_body_ends(file);
    let text = snapshot.file_text(file);
    model
        .bibitems()
        .iter()
        .map(|entry| {
            (
                entry.name.to_string(),
                manual_excerpt(text, ends, entry.range),
            )
        })
        .collect()
}

fn manual_body(
    snapshot: &Analysis,
    file: tex_ls_analysis::incremental::SourceId,
    range: TextRange,
) -> String {
    let ends = snapshot.manual_bib_body_ends(file);
    let text = snapshot.file_text(file);
    manual_excerpt(text, ends, range)
}

fn manual_excerpt(text: &str, ends: &[TextSize], range: TextRange) -> String {
    #[cfg(test)]
    MANUAL_CARD_EXCERPTS.with(|count| count.set(count.get() + 1));
    let start = range.end();
    let end = ends
        .get(ends.partition_point(|end| *end < start))
        .copied()
        .map(usize::from)
        .unwrap_or(text.len());
    excerpt(&text[usize::from(start)..end])
}

pub fn manual(snapshot: &Analysis, path: &Path, key: &str) -> Option<String> {
    let mut definition = None;
    for member in snapshot.resolve_citations().namespace_members(path) {
        let Some(file) = snapshot.lookup_file(member) else {
            continue;
        };
        for entry in snapshot.semantic_model(file).bibitems() {
            if entry.name != key {
                continue;
            }
            if definition.is_some() {
                return None;
            }
            definition = Some((file, entry.range));
        }
    }
    let (file, range) = definition?;
    Some(card(
        "Manual bibliography item",
        &manual_body(snapshot, file, range),
    ))
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
    let root = snapshot.parsed_tree(file);
    let command = tex_ls_parser::syntax::token_at_offset(&root, range.start())
        .right_biased()?
        .parent_ancestors()
        .filter(|node| {
            node.kind() == SyntaxKind::COMMAND && node.text_range().contains_range(*range)
        })
        .min_by_key(|node| node.text_range().len())?;
    let text = snapshot.file_text(file);
    let groups: Vec<_> = command
        .children()
        .filter(|node| {
            node.kind() == SyntaxKind::GROUP && node.text_range().start() > range.start()
        })
        .map(|node| &text[node.text_range()])
        .collect();
    Some(format!(
        "Glossary/acronym entry\n\n{}",
        literal_joined(&groups, "\n")
    ))
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
        "textbf" => &["Text to set in boldface"],
        "textit" => &["Text to set in italic"],
        "texttt" => &["Text to set in a typewriter font"],
        "emph" => &["Text to emphasize"],
        "href" => &["Target URL", "Displayed text"],
        "input" | "include" => &["Source filename"],
        "label" => &["Unique cross-reference key"],
        _ => &[],
    }
}

#[cfg(test)]
mod bounded_card_tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn glossary_card_copies_only_the_visible_excerpt() {
        let path = Path::new(fixture_path!("/source-cards/glossary.tex"));
        let body = format!("{{name={{Wide}},description={{{}}}}}", "😀".repeat(262_144));
        let source = format!("\\newglossaryentry{{wide}}{body}\\gls{{wide}}");
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, source.as_str(), None);
        COMMENT_CARD_COPY_BYTES.with(|bytes| bytes.set(0));
        assert_eq!(
            glossary(&db.snapshot(), path, "wide"),
            Some(card("Glossary/acronym entry", &body))
        );
        let copied = COMMENT_CARD_COPY_BYTES.with(std::cell::Cell::get);
        assert!(
            copied <= 4 * 1201,
            "copied {copied} source bytes for one glossary excerpt"
        );
    }

    #[test]
    fn manual_card_extracts_only_the_unique_requested_body() {
        let path = Path::new(fixture_path!("/source-cards/manual.tex"));
        let mut source = String::from("\\begin{thebibliography}{9}\n");
        for index in 0..1024 {
            source.push_str(&format!("\\bibitem{{k{index}}} Body {index}\n"));
        }
        source.push_str("\\bibitem{target} Late body 😀\n\\end{thebibliography}\n");
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, source.as_str(), None);
        MANUAL_CARD_EXCERPTS.with(|count| count.set(0));
        assert_eq!(
            manual(&db.snapshot(), path, "target"),
            Some("Manual bibliography item\n\n    Late body 😀".into())
        );
        assert_eq!(manual(&db.snapshot(), path, "TARGET"), None);
        let excerpts = MANUAL_CARD_EXCERPTS.with(std::cell::Cell::get);
        assert_eq!(excerpts, 1, "built unused manual body excerpts");
        source.push_str("\\bibitem{target} Duplicate\n");
        db.apply_change(path, source.as_str(), None);
        MANUAL_CARD_EXCERPTS.with(|count| count.set(0));
        assert_eq!(manual(&db.snapshot(), path, "target"), None);
        assert_eq!(MANUAL_CARD_EXCERPTS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn bounded_join_matches_literal_whole_join() {
        for separator in ["\n", " "] {
            for length in [0, 1, 1199, 1200, 1201, 5000] {
                let long = "😀".repeat(length);
                for parts in [
                    vec!["", long.as_str(), ""],
                    vec![" First ", long.as_str(), " Last "],
                    vec![" \t", long.as_str(), "\r\n"],
                ] {
                    assert_eq!(
                        literal_joined(&parts, separator),
                        literal(&parts.join(separator))
                    );
                }
            }
        }
        for length in [1198, 1199, 1200, 1201] {
            let prefix = "x".repeat(length);
            for parts in [
                vec![prefix.as_str(), "\r", "\nrest"],
                vec!["\u{3000}\r\n", prefix.as_str(), "\r\n\u{3000}"],
                vec![prefix.as_str(), "\r", "rest"],
            ] {
                assert_eq!(literal_joined(&parts, "\n"), literal(&parts.join("\n")));
            }
        }
    }
}
