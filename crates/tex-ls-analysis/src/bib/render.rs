//! Shared, bounded bibliography expansion and display; source bytes are untouched.
use crate::bib::{
    ast,
    syntax::{SyntaxKind as Kind, SyntaxNode as Node},
};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

const MONTHS: &[(&str, &str)] = &[
    ("jan", "January"),
    ("feb", "February"),
    ("mar", "March"),
    ("apr", "April"),
    ("may", "May"),
    ("jun", "June"),
    ("jul", "July"),
    ("aug", "August"),
    ("sep", "September"),
    ("oct", "October"),
    ("nov", "November"),
    ("dec", "December"),
];
const LIMIT: usize = 65_536;

struct Expansion<'a> {
    root: &'a Node,
    visiting: HashSet<String>,
    steps: usize,
}
impl Expansion<'_> {
    fn string(&mut self, key: &str) -> String {
        let key = key.to_lowercase();
        if self.steps >= 4096 {
            return format!("{key} [expansion limit]");
        }
        self.steps += 1;
        if self.visiting.len() >= 128 || !self.visiting.insert(key.clone()) {
            return format!("{key} [cyclic string]");
        }
        let value = self
            .root
            .children()
            .filter(|node| node.kind() == Kind::STRING_ENTRY)
            .find(|node| {
                ast::string_def_name(node).is_some_and(|(name, _)| name.eq_ignore_ascii_case(&key))
            })
            .and_then(|node| ast::fields(&node).next())
            .and_then(|field| ast::field_value(&field));
        let result = if let Some(value) = value {
            self.value(&value)
        } else if let Some((_, month)) = MONTHS.iter().find(|(name, _)| *name == key) {
            (*month).into()
        } else {
            format!("{key} [unresolved string]")
        };
        self.visiting.remove(&key);
        result
    }
    fn value(&mut self, value: &Node) -> String {
        let mut out = String::new();
        for piece in value.children() {
            let raw = piece.text().to_string();
            let text = if piece.kind() == Kind::LITERAL
                && piece
                    .first_token()
                    .is_some_and(|token| token.kind() == Kind::WORD)
            {
                self.string(&raw)
            } else {
                raw.strip_prefix('{')
                    .and_then(|text| text.strip_suffix('}'))
                    .or_else(|| {
                        raw.strip_prefix('"')
                            .and_then(|text| text.strip_suffix('"'))
                    })
                    .unwrap_or(&raw)
                    .into()
            };
            if out.len() + text.len() > LIMIT {
                out.push_str(" [expansion limit]");
                break;
            }
            out.push_str(&text);
        }
        out
    }
}

pub fn expanded(root: &Node, key: &str) -> String {
    Expansion {
        root,
        visiting: HashSet::new(),
        steps: 0,
    }
    .string(key)
}
pub fn value(root: &Node, value: &Node) -> String {
    Expansion {
        root,
        visiting: HashSet::new(),
        steps: 0,
    }
    .value(value)
}

/// Decode a curated TeX text surface, preserving unknown commands visibly.
pub fn text(source: &str) -> String {
    let mut chars = source.chars().peekable();
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '{' | '}' => {}
            '~' => out.push(' '),
            '\\' => {
                let Some(first) = chars.next() else {
                    out.push('\\');
                    break;
                };
                let mut command = first.to_string();
                if first.is_alphabetic() {
                    while chars.peek().is_some_and(|ch| ch.is_alphabetic()) {
                        command.push(chars.next().unwrap());
                    }
                }
                let accent = match command.as_str() {
                    "'" => Some('\u{301}'),
                    "`" => Some('\u{300}'),
                    "\"" => Some('\u{308}'),
                    "^" => Some('\u{302}'),
                    "~" => Some('\u{303}'),
                    "=" => Some('\u{304}'),
                    "." => Some('\u{307}'),
                    "c" => Some('\u{327}'),
                    "v" => Some('\u{30c}'),
                    "u" => Some('\u{306}'),
                    "H" => Some('\u{30b}'),
                    "r" => Some('\u{30a}'),
                    "k" => Some('\u{328}'),
                    "b" => Some('\u{331}'),
                    _ => None,
                };
                if let Some(accent) = accent {
                    while chars
                        .peek()
                        .is_some_and(|ch| ch.is_whitespace() || *ch == '{')
                    {
                        chars.next();
                    }
                    if let Some(mut base) = chars.next() {
                        if base == '\\' && chars.peek().is_some_and(|ch| matches!(ch, 'i' | 'j')) {
                            base = chars.next().unwrap();
                        }
                        out.push(base);
                        out.push(accent);
                    } else {
                        out.push('\\');
                        out.push_str(&command);
                    }
                } else {
                    let decoded = match command.as_str() {
                        "LaTeX" => "LaTeX",
                        "TeX" => "TeX",
                        "BibTeX" => "BibTeX",
                        "&" => "&",
                        "%" => "%",
                        "_" => "_",
                        "#" => "#",
                        "$" => "$",
                        "{" => "{",
                        "}" => "}",
                        " " => " ",
                        "ss" => "ß",
                        "ae" => "æ",
                        "AE" => "Æ",
                        "oe" => "œ",
                        "OE" => "Œ",
                        "o" => "ø",
                        "O" => "Ø",
                        "l" => "ł",
                        "L" => "Ł",
                        "i" => "ı",
                        "j" => "ȷ",
                        "textendash" => "–",
                        "textemdash" => "—",
                        _ => "",
                    };
                    if decoded.is_empty() {
                        out.push('\\');
                        out.push_str(&command);
                    } else {
                        out.push_str(decoded);
                    }
                }
            }
            _ => out.push(ch),
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect()
}

fn split_top_level<'a>(value: &'a str, separator: &str) -> Vec<&'a str> {
    let mut depth = 0usize;
    let mut escaped = false;
    let mut start = 0;
    let mut out = Vec::new();
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && index >= start && value[index..].starts_with(separator) {
            out.push(&value[start..index]);
            start = index + separator.len();
        }
    }
    out.push(&value[start..]);
    out
}

pub fn names(value: &str) -> String {
    split_top_level(value, " and ")
        .into_iter()
        .map(|name| {
            let parts = split_top_level(name, ",");
            match parts.as_slice() {
                [family, given] => format!("{} {}", text(given), text(family)),
                [family, suffix, given] => {
                    format!("{} {}, {}", text(given), text(family), text(suffix))
                }
                _ => text(name),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn field(node: &Node, name: &str) -> Option<String> {
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    ast::fields(node)
        .find(|field| ast::field_name(field).is_some_and(|key| key.eq_ignore_ascii_case(name)))
        .and_then(|field| ast::field_value(&field))
        .map(|field| value(&root, &field))
}

/// Normalize supported ISO partial dates and intervals; preserve other date syntax.
pub fn date(source: &str) -> String {
    let source = text(source);
    source
        .split('/')
        .map(|part| {
            let fields: Vec<_> = part.split('-').collect();
            let parsed: Option<Vec<u32>> = fields.iter().map(|field| field.parse().ok()).collect();
            let Some(fields) = parsed else {
                return part.to_owned();
            };
            match fields.as_slice() {
                [year] if *year <= 9999 => format!("{year:04}"),
                [year, month] if *year <= 9999 && (1..=12).contains(month) => {
                    format!("{year:04}-{month:02}")
                }
                [year, month, day] if *year <= 9999 && (1..=12).contains(month) => {
                    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
                    let days = [
                        31,
                        if leap { 29 } else { 28 },
                        31,
                        30,
                        31,
                        30,
                        31,
                        31,
                        30,
                        31,
                        30,
                        31,
                    ][*month as usize - 1];
                    if (1..=days).contains(day) {
                        format!("{year:04}-{month:02}-{day:02}")
                    } else {
                        part.to_owned()
                    }
                }
                _ => part.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn inline(node: &Node) -> Option<String> {
    let authors = field(node, "author")
        .or_else(|| field(node, "editor"))
        .map(|value| names(&value));
    let date = field(node, "date")
        .or_else(|| field(node, "year"))
        .map(|value| date(&value));
    match (authors, date) {
        (Some(authors), Some(date)) => Some(format!(
            "{} ({date})",
            authors.split("; ").next().unwrap_or(&authors)
        )),
        (Some(authors), None) => Some(authors.split("; ").next().unwrap_or(&authors).into()),
        (None, Some(date)) => Some(format!("({date})")),
        _ => None,
    }
}

pub fn markdown_text(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub fn entry(entry_type: &str, key: &str, node: &Node) -> String {
    use std::fmt::Write;
    let mut out = format!(
        "@{} · `{}`",
        markdown_text(entry_type),
        key.replace('`', "\\`")
    );
    for name in [
        "author",
        "editor",
        "title",
        "date",
        "year",
        "month",
        "day",
        "journal",
        "journaltitle",
        "booktitle",
        "publisher",
        "volume",
        "number",
        "pages",
        "doi",
        "url",
    ] {
        if matches!(name, "year" | "month" | "day") && field(node, "date").is_some() {
            continue;
        }
        let Some(raw) = field(node, name) else {
            continue;
        };
        let display = if matches!(name, "author" | "editor") {
            names(&raw)
        } else if name == "date" {
            date(&raw)
        } else if matches!(name, "url" | "doi") {
            raw.trim().to_owned()
        } else {
            text(&raw)
        };
        if display.is_empty() {
            continue;
        }
        let link = match name {
            "doi" => Some(if display.starts_with("https://doi.org/") {
                display.clone()
            } else {
                format!("https://doi.org/{display}")
            }),
            "url" if display.starts_with("https://") || display.starts_with("http://") => {
                Some(display.clone())
            }
            _ => None,
        };
        let display = if let Some(link) = link {
            format!(
                "[{}](<{}>)",
                markdown_text(&display),
                link.replace('<', "%3C")
                    .replace('>', "%3E")
                    .replace(' ', "%20")
            )
        } else {
            markdown_text(&display)
        };
        let _ = write!(out, "\n\n**{name}:** {display}");
    }
    out
}

pub fn documentation(root: &Node, offset: usize) -> Option<(rowan::TextRange, String)> {
    use crate::bib::semantic::{FieldCategory, RequiredField, builtin};
    let token = root
        .token_at_offset(rowan::TextSize::from(offset as u32))
        .right_biased()?;
    for node in token.parent_ancestors() {
        if node.kind() == Kind::FIELD_NAME {
            let name = node.text().to_string();
            builtin().field(&name)?;
            let prose = match name.to_lowercase().as_str() {
                "title" => {
                    "The title of the work. Braces protect capitalization; TeX text commands and string concatenation are supported."
                }
                "author" => {
                    "The authors, separated by top-level `and`. Use `Family, Given` or `Given Family`; braces keep an organization together."
                }
                "editor" => "The editors, using the same name-list syntax as author.",
                "doi" => {
                    "The digital object identifier, retained literally and linked through doi.org."
                }
                "url" => "The resource URL, retained literally.",
                "date" => {
                    "The publication date in ISO year-month-day form, including partial dates and slash-separated intervals."
                }
                _ => match builtin().category(&name) {
                    FieldCategory::Name => {
                        "A list of people or organizations, separated by top-level `and`."
                    }
                    FieldCategory::Date => {
                        "A date or date component; string macros such as month names are expanded for display."
                    }
                    FieldCategory::Verbatim => {
                        "A literal identifier or path; TeX text decoding is not applied."
                    }
                    FieldCategory::Literal => {
                        "Bibliographic text. Braced or quoted literals may be joined with `#` and string macros."
                    }
                },
            };
            return Some((
                node.text_range(),
                format!("**{}**\n\n{prose}", markdown_text(&name)),
            ));
        }
        if node.kind() == Kind::ENTRY_TYPE {
            let name = node.text().to_string();
            let signature = builtin().entry(&name)?;
            let required = signature
                .required
                .iter()
                .map(|field| match field {
                    RequiredField::One(name) => name.to_string(),
                    RequiredField::OneOf(names) => names
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" or "),
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Some((
                node.text_range(),
                format!(
                    "**@{}** bibliography entry\n\nRequired fields: {}.\n\nOptional fields: {}.",
                    markdown_text(&name),
                    if required.is_empty() {
                        "none"
                    } else {
                        &required
                    },
                    signature
                        .optional
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        if node.kind() == Kind::KEY {
            let entry_node = node.parent()?;
            let name = ast::entry_type(&entry_node)?;
            let (key, range) = ast::cite_key(&entry_node)?;
            return Some((range, entry(&name, &key, &entry_node)));
        }
    }
    None
}

pub fn search_text(
    entry: &crate::bib::semantic::Entry,
    root: &crate::bib::syntax::SyntaxNode,
) -> String {
    let mut text = entry.key.to_string();
    if let Some(node) = crate::bib::ast::entry_at_range(root, entry.range) {
        for name in ["title", "author", "editor"] {
            if let Some(value) = crate::bib::render::field(&node, name) {
                text.push(' ');
                text.push_str(&if name == "title" {
                    crate::bib::render::text(&value)
                } else {
                    crate::bib::render::names(&value)
                });
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates_and_adversarial_expansion_are_bounded_and_visible() {
        assert_eq!(date("2024-2-9/2024-3"), "2024-02-09/2024-03");
        assert_eq!(date("2023-2-29"), "2023-2-29");
        assert_eq!(date("2024?"), "2024?");
        let mut source = String::from("@string{x0={word}}\n");
        for i in 1..30 {
            source.push_str(&format!("@string{{x{i}=x{} # x{}}}\n", i - 1, i - 1));
        }
        let root = crate::bib::parse(&source).syntax();
        let display = expanded(&root, "x29");
        assert!(display.len() < LIMIT + 100);
        assert!(display.contains("expansion limit"));
    }

    #[test]
    fn expands_and_normalizes_without_hiding_unknown_or_cyclic_values() {
        let root = crate::bib::parse(r#"@string{a = {Hello } # b} @string{b = {World}} @string{cycle = cycle} @book{k, title = a, author = {Garc\'ia, Jos\'e and {Research and Development}}, month = jan, doi = {10.1/test}}"#).syntax();
        assert_eq!(expanded(&root, "a"), "Hello World");
        assert!(expanded(&root, "cycle").contains("cyclic"));
        assert!(expanded(&root, "missing").contains("unresolved"));
        assert_eq!(expanded(&root, "jan"), "January");
        let node = root
            .children()
            .find(|node| node.kind() == Kind::ENTRY)
            .unwrap();
        let card = entry("book", "k", &node);
        assert!(
            card.contains("José García; Research and Development"),
            "{card}"
        );
        assert!(card.contains("Hello World"));
        assert!(card.contains("https://doi.org/10.1/test"));
    }
}
