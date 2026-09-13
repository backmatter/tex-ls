//! Identifier operations over BibTeX string definitions and literal macro uses.
use super::*;

pub fn target(model: &BibModel, offset: usize) -> Option<(String, TextRange)> {
    let at = TextSize::from(offset as u32);
    model
        .string_defs()
        .iter()
        .map(|site| (&site.name, site.range))
        .chain(
            model
                .string_uses()
                .iter()
                .map(|site| (&site.name, site.range)),
        )
        .find(|(_, range)| range.contains_inclusive(at))
        .map(|(name, range)| (name.to_string(), range))
}

pub fn occurrences(model: &BibModel, key: &str) -> Vec<(TextRange, bool)> {
    model
        .string_defs()
        .iter()
        .filter(|site| site.name.eq_ignore_ascii_case(key))
        .map(|site| (site.range, true))
        .chain(
            model
                .string_uses()
                .iter()
                .filter(|site| site.name.eq_ignore_ascii_case(key))
                .map(|site| (site.range, false)),
        )
        .collect()
}

pub fn rename_allowed(model: &BibModel, key: &str) -> bool {
    occurrences(model, key)
        .iter()
        .filter(|(_, definition)| *definition)
        .count()
        == 1
}

pub fn rename(
    snapshot: &Analysis,
    path: &Path,
    key: &str,
    name: &str,
    enc: PositionEncoding,
) -> Option<WorkspaceEdit> {
    let file = snapshot.lookup_file(path)?;
    let model = snapshot.bib_semantic_model(file);
    if name.is_empty()
        || !name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b':'))
        || name.as_bytes()[0].is_ascii_digit()
        || !rename_allowed(model, key)
        || (!name.eq_ignore_ascii_case(key) && !occurrences(model, name).is_empty())
    {
        return None;
    }
    let uri = path_to_uri(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let mut changes = HashMap::new();
    for (range, _) in occurrences(model, key) {
        push_edit(&mut changes, &uri, &idx, range, name);
    }
    finalize_rename(changes)
}

pub use tex_ls_analysis::bib::render::expanded;
