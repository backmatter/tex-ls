//! Structure operations over the source owned by an analysis snapshot.
use super::*;

pub fn compute_folding(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
) -> Vec<FoldingRange> {
    if kind == FileKind::Bib {
        return Vec::new();
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    folding::folding_ranges(
        &snapshot.parsed_tree(file),
        &snapshot.file_line_index(file, encoding),
    )
}

pub fn compute_selection_range(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    positions: &[Position],
) -> Vec<SelectionRange> {
    if kind == FileKind::Bib {
        return positions
            .iter()
            .map(|&pos| SelectionRange {
                range: Range::new(pos, pos),
                parent: None,
            })
            .collect();
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    selection_range::selection_ranges(
        &snapshot.parsed_tree(file),
        &snapshot.file_line_index(file, encoding),
        positions,
    )
}

/// Resolve links using one captured source and its corresponding line table.
pub fn compute_document_link(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    texmf: &dyn HostServices,
) -> Vec<DocumentLink> {
    if kind == FileKind::Bib {
        return compute_bib_document_link(snapshot, path, encoding);
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    document_link::document_links(&snapshot.parsed_tree(file), path.parent(), texmf)
        .into_iter()
        .filter_map(|target| {
            Some(DocumentLink {
                range: lsp_range(&idx, target.range),
                target: Some(path_to_uri(&target.target)?),
                tooltip: None,
                data: None,
            })
        })
        .collect()
}

pub fn compute_bib_document_link(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
) -> Vec<DocumentLink> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    meaning_analysis::bib::document_link::document_links(&snapshot.parsed_bib_tree(file))
        .into_iter()
        .filter_map(|link| {
            Some(DocumentLink {
                range: lsp_range(&idx, link.range),
                target: Some(link.target.parse::<Uri>().ok()?),
                tooltip: None,
                data: None,
            })
        })
        .collect()
}
