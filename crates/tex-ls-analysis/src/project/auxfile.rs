//! Read the compiler's `.aux` files: resolved label numbers (`\newlabel`) and
//! toc entries (`\@writefile{toc}{\contentsline …}`), plus `\@input` links to
//! per-chapter aux files.
//!
//! **LSP-only, like [`super::texmf`].** The `.aux` is a project-local build
//! artifact read to enrich hover and document symbols with the numbers the last
//! compile assigned; it must never feed the formatter or linter (the formatter's
//! output is a pure function of the input — see AGENTS.md, "Non-goals").
//!
//! A dedicated line-oriented scanner, deliberately **not** the LaTeX parser: aux
//! files are machine-generated under `\makeatletter`, so their commands
//! (`\@input`, `\@writefile`, `\caption@xref`) contain `@`, which the
//! catcode-faithful lexer treats as a non-letter outside `\makeatletter` regions.
//! A small brace matcher over the raw text is simpler and immune to that.
//!
//! Number extraction mirrors texlab (`crates/base-db/src/semantics/auxiliary.rs`):
//! from `\newlabel{key}{{num}{page}…}`'s second argument, take the first
//! top-level `{…}` whose content starts with text (skipping `\caption@xref`-style
//! command groups and empty groups) and strip any remaining braces, so
//! ntheorem's `{1.{1}}` yields `1.1`.

use std::collections::HashMap;

use smol_str::SmolStr;

/// Aux facts for one document: everything hover and document symbols consume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuxData {
    /// `\newlabel{key}{{num}…}`: label key → resolved number (`"1.2"`).
    pub labels: HashMap<SmolStr, String>,
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
    if !key.is_empty()
        && let Some(number) = extract_number(value)
    {
        out.data.labels.insert(SmolStr::new(key), number);
    }
    Some(end)
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

/// The resolved number inside `\newlabel`'s second argument: the first
/// top-level group whose content starts with text (not a command or another
/// group), with any nested braces stripped (`1.{1}` → `1.1`).
fn extract_number(value: &str) -> Option<String> {
    let mut at = 0;
    while let Some((content, end)) = group(value, at) {
        let t = content.trim();
        if !t.is_empty() && !t.starts_with('\\') && !t.starts_with('{') {
            let number: String = t.chars().filter(|&c| c != '{' && c != '}').collect();
            let number = number.trim().to_owned();
            if !number.is_empty() {
                return Some(number);
            }
        }
        at = end;
    }
    None
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
