//! Identifier operations over BibTeX string definitions and literal macro uses.
use crate::{
    convert::locations_for_ranges,
    workspace_edits::{edits_for_locations, finalize_rename},
};
use lsp_types::WorkspaceEdit;
use rowan::{TextRange, TextSize};
use std::path::Path;
use tex_ls_analysis::{
    bib::semantic::Model as BibModel, incremental::Analysis, text::PositionEncoding,
};

pub use tex_ls_analysis::bib::render::expanded;

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
    model.string_occurrences(key).collect()
}

pub fn rename(
    snapshot: &Analysis,
    path: &Path,
    key: &str,
    name: &str,
    enc: PositionEncoding,
) -> Option<WorkspaceEdit> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b':'))
        || name.as_bytes()[0].is_ascii_digit()
        || !snapshot.bibliography_string_rename_allowed(path, key)
        || (!name.eq_ignore_ascii_case(key)
            && !snapshot
                .bibliography_string_occurrences(path, name)
                .is_empty())
    {
        return None;
    }
    let sites = snapshot.bibliography_string_occurrences(path, key);
    finalize_rename(edits_for_locations(
        locations_for_ranges(
            snapshot,
            sites
                .iter()
                .map(|site| (site.path.as_path(), site.key_range)),
            enc,
        )
        .into_iter()
        .flatten(),
        name,
    ))
}
