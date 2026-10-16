//! Read the compiler's `.aux` files: resolved label numbers (`\newlabel`) and
//! toc entries (`\@writefile{toc}{\contentsline …}`), plus `\@input` links to
//! per-chapter aux files.
//!
//! **LSP-only, like [`super::texmf`].** The `.aux` is a project-local build
//! artifact read to enrich hover and document symbols with the numbers the last
//! compile assigned; it must never feed the formatter or linter (the formatter's
//! output is a pure function of the input).
//!
//! A dedicated line-oriented scanner, deliberately **not** the LaTeX parser: aux
//! files are machine-generated under `\makeatletter`, so their commands
//! (`\@input`, `\@writefile`, `\caption@xref`) contain `@`, which the
//! catcode-faithful lexer treats as a non-letter outside `\makeatletter` regions.
//! A small brace matcher over the raw text is simpler and immune to that.
//!
//! Number extraction reads only the first tuple field in
//! `\newlabel{key}{{num}{page}…}`. Empty or dynamic numbers stay absent;
//! page and name fields cannot replace them. Literal nested braces flatten,
//! so ntheorem's `{1.{1}}` yields `1.1`.

use std::collections::HashMap;

use smol_str::SmolStr;

/// Aux facts for one document: everything hover and document symbols consume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuxData {
    /// `\newlabel{key}{{num}…}`: label key → resolved number (`"1.2"`).
    pub labels: HashMap<SmolStr, String>,
    /// Standard second and third `\newlabel` fields, when literal.
    pub pages: HashMap<SmolStr, String>,
    pub names: HashMap<SmolStr, String>,
    /// `\@writefile{toc}{\contentsline …}` entries, in document order.
    pub toc: Vec<TocEntry>,
}

/// One `\contentsline` written to the toc stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    /// The sectioning unit name as written (`part`, `chapter`, `section`, …).
    pub level: SmolStr,
    /// The `\numberline{…}` number; `None` for unnumbered (starred) entries.
    pub number: Option<String>,
    /// The title source after `\numberline`, as written by TeX (may contain
    /// macros, with TeX's inserted spacing — normalize before matching).
    pub title: String,
}

/// One parsed `.aux` file: its facts plus the `\@input{…}` targets it pulls in
/// (per-chapter aux files under `\include`), not yet resolved to paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedAux {
    pub data: AuxData,
    pub inputs: Vec<String>,
}

/// Scan one `.aux` text. Never fails: unrecognized or malformed lines are
/// skipped (an aborted compile truncates the file mid-entry).
pub fn parse_aux(text: &str) -> ParsedAux {
    let mut out = ParsedAux::default();
    let mut i = 0;
    while let Some(off) = text[i..].find('\\') {
        let start = i + off + 1;
        i = if let Some(rest) = command_at(text, start, "newlabel") {
            newlabel(text, rest, &mut out).unwrap_or(start)
        } else if let Some(rest) = command_at(text, start, "@writefile") {
            writefile(text, rest, &mut out).unwrap_or(start)
        } else if let Some(rest) = command_at(text, start, "@input") {
            input(text, rest, &mut out).unwrap_or(start)
        } else {
            start
        };
    }
    out
}

/// If the control word at `start` (just past the `\`) is exactly `name`, return
/// the offset past it; aux command names are letters and `@`.
fn command_at(text: &str, start: usize, name: &str) -> Option<usize> {
    let rest = &text[start..];
    if !rest.starts_with(name) {
        return None;
    }
    let end = start + name.len();
    match text.as_bytes().get(end) {
        Some(b) if b.is_ascii_alphabetic() || *b == b'@' => None,
        _ => Some(end),
    }
}

/// `\newlabel{key}{{num}{page}…}`: record `key → num`. Returns the offset past
/// the second group.
fn newlabel(text: &str, at: usize, out: &mut ParsedAux) -> Option<usize> {
    let (key, at) = group(text, at)?;
    let (value, end) = group(text, at)?;
    let key = key.trim();
    // A later tuple replaces these facts even when its fields are empty or
    // dynamic. Retaining the earlier literal would show stale build metadata.
    out.data.labels.remove(key);
    out.data.pages.remove(key);
    out.data.names.remove(key);
    if !key.is_empty()
        && let Some(number) = extract_number(value)
    {
        out.data.labels.insert(SmolStr::new(key), number);
    }
    // Page and name fields have fixed positions in the standard newlabel
    // tuple. Never scan forward from an empty/dynamic field into another one.
    if !key.is_empty()
        && let Some((_, after_number)) = group(value, 0)
        && let Some((page, after_page)) = group(value, after_number)
    {
        if let Some(page) = literal_hint_text(page) {
            out.data.pages.insert(key.into(), page);
        }
        if let Some((name, _)) = group(value, after_page)
            && let Some(name) = literal_hint_text(name)
        {
            out.data.names.insert(key.into(), name);
        }
    }
    Some(end)
}

fn literal_hint_text(text: &str) -> Option<String> {
    if text
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '\\' | '%' | '#'))
    {
        return None;
    }
    let text = text
        .chars()
        .filter(|ch| !matches!(ch, '{' | '}'))
        .collect::<String>();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `\@writefile{toc}{\contentsline {level}{\numberline {num}Title}{page}…}`:
/// record a [`TocEntry`] per `\contentsline` in a toc-stream group. Other
/// streams (`lof`, `lot`, …) are ignored. Returns the offset past the payload.
fn writefile(text: &str, at: usize, out: &mut ParsedAux) -> Option<usize> {
    let (stream, at) = group(text, at)?;
    let (payload, end) = group(text, at)?;
    if stream.trim() != "toc" {
        return Some(end);
    }
    let mut i = 0;
    while let Some(off) = payload[i..].find('\\') {
        let start = i + off + 1;
        i = match command_at(payload, start, "contentsline").and_then(|rest| {
            let (level, rest) = group(payload, rest)?;
            let (heading, entry_end) = group(payload, rest)?;
            let level = level.trim();
            if !level.is_empty() {
                let (number, title) = split_numberline(heading);
                out.data.toc.push(TocEntry {
                    level: SmolStr::new(level),
                    number,
                    title: title.trim().to_owned(),
                });
            }
            Some(entry_end)
        }) {
            Some(next) => next,
            None => start,
        };
    }
    Some(end)
}

/// `\@input{chapter.aux}`: record the target. Returns the offset past the group.
fn input(text: &str, at: usize, out: &mut ParsedAux) -> Option<usize> {
    let (target, end) = group(text, at)?;
    let target = target.trim();
    if !target.is_empty() {
        out.inputs.push(target.to_owned());
    }
    Some(end)
}

/// The next `{…}` group at or after `at` (skipping ASCII whitespace): its inner
/// content and the offset past the closing `}`. `None` when the next
/// non-whitespace byte is not `{` or the group never closes (truncated file).
fn group(text: &str, at: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut i = at;
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let open = i;
    while i < bytes.len() {
        match bytes[i] {
            // Skip the escaped byte so `\{`/`\}` never count. (Skipping one byte
            // inside a multi-byte UTF-8 sequence is safe: continuation bytes are
            // ≥ 0x80 and match no arm.)
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[open + 1..i], i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The number occupies the first tuple field. A missing or dynamic number
/// must not borrow the page, name, or anchor from a later tuple field.
fn extract_number(value: &str) -> Option<String> {
    let (content, _) = group(value, 0)?;
    literal_hint_text(content)
}

/// Split a `\contentsline` heading group into its `\numberline{…}` number (if
/// any) and the remaining title source.
fn split_numberline(heading: &str) -> (Option<String>, &str) {
    let trimmed = heading.trim_start();
    let Some(rest) = trimmed.strip_prefix("\\numberline") else {
        return (None, heading);
    };
    // `\numberline` must end the control word (guard against `\numberlinefoo`).
    if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '@') {
        return (None, heading);
    }
    let offset = heading.len() - rest.len();
    match group(heading, offset) {
        Some((number, end)) => {
            let number: String = number.chars().filter(|&c| c != '{' && c != '}').collect();
            let number = number.trim().to_owned();
            ((!number.is_empty()).then_some(number), &heading[end..])
        }
        None => (None, heading),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_page_and_name_fields_use_fixed_tuple_positions() {
        let parsed = parse_aux(r"\newlabel{a}{{1}{iv}{A {title}}{section.1}{}}");
        assert_eq!(parsed.data.pages.get("a").map(String::as_str), Some("iv"));
        assert_eq!(
            parsed.data.names.get("a").map(String::as_str),
            Some("A title")
        );
        let parsed = parse_aux(
            r"\newlabel{b}{{2}{}{Name}{anchor}{}}\newlabel{c}{{3}{\thepage}{\macro}{anchor}{}}",
        );
        assert!(!parsed.data.pages.contains_key("b"));
        assert_eq!(parsed.data.names.get("b").map(String::as_str), Some("Name"));
        assert!(!parsed.data.pages.contains_key("c"));
        assert!(!parsed.data.names.contains_key("c"));
    }

    #[test]
    fn later_empty_or_dynamic_fields_retire_previous_hint_text() {
        for replacement in [r"{{2}{}{}}", r"{{2}{\thepage}{\macro}}", r"{{2}}"] {
            let parsed = parse_aux(&format!(
                r"\newlabel{{a}}{{{{1}}{{iv}}{{Old title}}}}\newlabel{{a}}{replacement}"
            ));
            assert!(!parsed.data.pages.contains_key("a"), "{replacement}");
            assert!(!parsed.data.names.contains_key("a"), "{replacement}");
        }
        for replacement in ["{}", "{{}{}}", r"{{\dynamic}{\thepage}{\macro}}"] {
            let parsed = parse_aux(&format!(
                r"\newlabel{{a}}{{{{1}}{{iv}}{{Old title}}}}\newlabel{{a}}{replacement}"
            ));
            assert!(!parsed.data.labels.contains_key("a"), "{replacement}");
            assert!(!parsed.data.pages.contains_key("a"), "{replacement}");
            assert!(!parsed.data.names.contains_key("a"), "{replacement}");
        }
    }
}

#[cfg(test)]
mod number_tuple_tests {
    use super::*;
    #[test]
    fn missing_numbers_do_not_use_page_name_or_anchor_fields() {
        for number in ["", r"\dynamic", r"1\relax"] {
            let parsed = parse_aux(&format!(
                r"\newlabel{{a}}{{{{{number}}}{{15}}{{Title}}{{anchor}}}}"
            ));
            assert!(!parsed.data.labels.contains_key("a"), "{number}");
            assert_eq!(parsed.data.pages.get("a").map(String::as_str), Some("15"));
        }
        let parsed = parse_aux(r"\newlabel{a}{{1.{2}}{15}{Title}{anchor}}");
        assert_eq!(parsed.data.labels.get("a").map(String::as_str), Some("1.2"));
    }
}
