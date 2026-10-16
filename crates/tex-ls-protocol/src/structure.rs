//! Structure operations over the source owned by an analysis snapshot.
use crate::{
    convert::{lsp_range_with_cursor, path_to_uri},
    folding, selection_range,
};
use lsp_types::{DocumentLink, FoldingRange, Position, SelectionRange, Uri};
use rowan::TextSize;
use std::path::Path;
use tex_ls_analysis::{incremental::Analysis, source::FileKind, text::PositionEncoding};

pub fn compute_folding(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
) -> Vec<FoldingRange> {
    if kind == FileKind::Bib {
        use tex_ls_analysis::bib::syntax::SyntaxKind as Kind;
        let Some(file) = snapshot.lookup_file(path) else {
            return Vec::new();
        };
        let idx = snapshot.file_line_index(file, encoding);
        let mut positions = idx.position_cursor();
        return snapshot
            .parsed_bib_tree(file)
            .children()
            .filter(|node| {
                matches!(
                    node.kind(),
                    Kind::ENTRY | Kind::STRING_ENTRY | Kind::PREAMBLE_ENTRY | Kind::COMMENT_ENTRY
                )
            })
            .filter_map(|node| {
                let start = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .find(|token| matches!(token.kind(), Kind::L_BRACE | Kind::L_PAREN))?
                    .text_range()
                    .end();
                let end = node
                    .last_token()
                    .filter(|token| matches!(token.kind(), Kind::R_BRACE | Kind::R_PAREN))?
                    .text_range()
                    .start();
                let (start_line, start_character) = positions.position(start.into());
                let (end_line, end_character) = positions.position(end.into());
                (start_line < end_line).then_some(FoldingRange {
                    start_line,
                    start_character: Some(start_character),
                    end_line,
                    end_character: Some(end_character),
                    kind: None,
                    collapsed_text: None,
                })
            })
            .collect();
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    folding::convert(snapshot.folding_ranges(file))
}

pub fn compute_selection_range(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
    positions: &[Position],
) -> Vec<SelectionRange> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    let positions: Vec<_> = positions.iter().map(|p| (p.line, p.character)).collect();
    let offsets = idx.offsets_at(&positions);
    if kind == FileKind::Bib {
        let root = snapshot.parsed_bib_tree(file);
        let chains = offsets
            .into_iter()
            .map(|offset| {
                snapshot.check_cancelled();
                let offset = TextSize::from(offset as u32);
                let mut ranges = Vec::new();
                if let Some(token) =
                    tex_ls_parser::syntax::token_at_offset(&root, offset).right_biased()
                {
                    ranges.push(token.text_range());
                    ranges.extend(token.parent_ancestors().map(|node| node.text_range()));
                }
                if ranges.is_empty() {
                    ranges.push(root.text_range());
                }
                ranges.dedup();
                ranges
            })
            .collect();
        return selection_range::from_chains(&idx, chains);
    }
    let root = snapshot.parsed_tree(file);
    let chains = offsets
        .into_iter()
        .map(|offset| {
            snapshot.check_cancelled();
            tex_ls_analysis::selection::selection_chain(&root, offset)
        })
        .collect();
    selection_range::from_chains(&idx, chains)
}

/// Resolve links using one captured source and its corresponding line table.
pub fn compute_document_link(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    kind: FileKind,
) -> Vec<DocumentLink> {
    if kind == FileKind::Bib {
        return compute_bib_document_link(snapshot, path, encoding);
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    let mut positions = idx.position_cursor();
    snapshot
        .document_links(file)
        .into_iter()
        .filter_map(|target| {
            Some(DocumentLink {
                range: lsp_range_with_cursor(&mut positions, target.range),
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
    let mut positions = idx.position_cursor();
    tex_ls_analysis::bib::document_link::document_links(&snapshot.parsed_bib_tree(file))
        .into_iter()
        .filter_map(|link| {
            Some(DocumentLink {
                range: lsp_range_with_cursor(&mut positions, link.range),
                target: Some(link.target.parse::<Uri>().ok()?),
                tooltip: None,
                data: None,
            })
        })
        .collect()
}
