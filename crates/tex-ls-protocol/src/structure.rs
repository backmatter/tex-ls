//! Structure operations over the source owned by an analysis snapshot.
use super::*;

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
                let (start_line, start_character) = idx.position(start.into());
                let (end_line, end_character) = idx.position(end.into());
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
    if kind == FileKind::Bib {
        let Some(file) = snapshot.lookup_file(path) else {
            return Vec::new();
        };
        let idx = snapshot.file_line_index(file, encoding);
        let root = snapshot.parsed_bib_tree(file);
        return positions
            .iter()
            .map(|position| {
                let offset =
                    TextSize::from(idx.offset_at(position.line, position.character) as u32);
                let mut ranges = Vec::new();
                if let Some(token) = root.token_at_offset(offset).right_biased() {
                    ranges.push(token.text_range());
                    ranges.extend(token.parent_ancestors().map(|node| node.text_range()));
                }
                if ranges.is_empty() {
                    ranges.push(root.text_range());
                }
                ranges.dedup();
                let mut parent = None;
                for range in ranges.into_iter().rev() {
                    parent = Some(Box::new(SelectionRange {
                        range: lsp_range(&idx, range),
                        parent,
                    }));
                }
                *parent.expect("selection includes root")
            })
            .collect();
    }
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    positions
        .iter()
        .map(|pos| {
            let ranges = snapshot.selection_chain(file, idx.offset_at(pos.line, pos.character));
            let mut parent = None;
            for range in ranges.into_iter().rev() {
                parent = Some(Box::new(SelectionRange {
                    range: lsp_range(&idx, range),
                    parent,
                }));
            }
            *parent.expect("selection includes root")
        })
        .collect()
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
    snapshot
        .document_links(file)
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
    tex_ls_analysis::bib::document_link::document_links(&snapshot.parsed_bib_tree(file))
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
