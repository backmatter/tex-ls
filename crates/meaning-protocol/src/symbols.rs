//! Symbol conversion from an analysis snapshot.
use super::*;

/// Convert the source outline and supplied last-build numbers to LSP symbols.
pub fn compute_symbols(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    build: &dyn HostServices,
) -> Vec<DocumentSymbol> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    let items = outline(&snapshot.parsed_tree(file));
    let aux = document_aux(snapshot, snapshot.resolve_labels(), path, build);
    let mut toc_cursor = 0;
    items
        .iter()
        .map(|item| to_document_symbol(item, &idx, aux.as_ref(), &mut toc_cursor))
        .collect()
}

pub fn compute_bib_symbols(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
) -> Vec<DocumentSymbol> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    bib_outline(snapshot.bib_semantic_model(file))
        .iter()
        .map(|item| bib_to_document_symbol(item, &idx))
        .collect()
}

/// Combine project results, retaining distinct interpretations of shared sources.
pub fn compute_projects_workspace_symbols(
    snapshots: &[Analysis],
    query: &str,
    enc: PositionEncoding,
) -> WorkspaceSymbolResponse {
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();
    for snapshot in snapshots {
        let WorkspaceSymbolResponse::WorkspaceSymbolList(items) =
            compute_workspace_symbols(snapshot, query, enc)
        else {
            unreachable!("workspace symbols use the modern response form")
        };
        for item in items {
            // Symbols with different ranges or interpretations remain distinct.
            if seen.insert(serde_json::to_string(&item).expect("symbol serialization")) {
                symbols.push(item);
            }
        }
    }
    WorkspaceSymbolResponse::WorkspaceSymbolList(symbols)
}

pub fn compute_workspace_symbols(
    snapshot: &Analysis,
    query: &str,
    enc: PositionEncoding,
) -> WorkspaceSymbolResponse {
    let needle = query.to_ascii_lowercase();
    let mut symbols = Vec::new();
    for member in snapshot.project_members() {
        if member.kind == FileKind::Bib {
            continue;
        }
        let idx = snapshot.file_line_index(member.file, enc);
        let items = outline(&snapshot.parsed_tree(member.file));
        let container = member.path.file_stem().and_then(|s| s.to_str());
        collect_workspace_symbols(&items, &member.path, &idx, &needle, container, &mut symbols);
    }
    WorkspaceSymbolResponse::WorkspaceSymbolList(symbols)
}

/// Recursively flatten an [`OutlineItem`] tree into [`WorkspaceSymbol`]s, keeping
/// entries whose name contains `needle`. Children are always visited so a matching
/// label nested under a non-matching section still surfaces.
pub fn collect_workspace_symbols(
    items: &[OutlineItem],
    path: &Path,
    idx: &LineIndex,
    needle: &str,
    container: Option<&str>,
    out: &mut Vec<WorkspaceSymbol>,
) {
    for item in items {
        let matches = needle.is_empty() || item.name.to_ascii_lowercase().contains(needle);
        if matches && let Some(location) = location_for(path, idx, item.selection_range) {
            out.push(WorkspaceSymbol {
                base_symbol_information: lsp_types::BaseSymbolInformation {
                    name: item.name.clone(),
                    kind: outline_symbol_kind(item.kind),
                    tags: None,
                    container_name: container.map(str::to_owned),
                },
                location: location.into(),
                data: None,
            });
        }
        // Always recurse: a matching label can nest under a non-matching section.
        collect_workspace_symbols(&item.children, path, idx, needle, container, out);
    }
}

/// Convert an [`OutlineItem`] tree into an LSP [`DocumentSymbol`], mapping byte
/// ranges through the (encoding-aware) [`LineIndex`].
/// Map an [`OutlineSymbol`] to its LSP [`SymbolKind`]. Shared by the per-file
/// `documentSymbol` ([`to_document_symbol`]) and project-wide `workspace/symbol`
/// (`run_workspace_symbols`) outputs so the two never drift.
pub fn outline_symbol_kind(kind: OutlineSymbol) -> SymbolKind {
    match kind {
        OutlineSymbol::Section => SymbolKind::Module,
        OutlineSymbol::Frame => SymbolKind::Class,
        OutlineSymbol::Float => SymbolKind::Object,
        OutlineSymbol::Theorem => SymbolKind::Class,
        OutlineSymbol::Label => SymbolKind::Constant,
        OutlineSymbol::Macro => SymbolKind::Function,
        OutlineSymbol::Environment => SymbolKind::Interface,
    }
}

/// Enrichment from the compile's `.aux` (when one exists): a section name gets its
/// toc number prefixed (`"1.2 Intro"`), a label its `\newlabel` number as
/// `detail`, and a float/theorem its child label's number as `detail`. Section
/// matching consumes toc entries in document order (`toc_cursor`) against
/// whitespace-normalized titles, so a macro-heavy title that fails to match
/// degrades to its plain, numberless name.
#[allow(deprecated)] // `DocumentSymbol::deprecated` is a required struct field.
pub fn to_document_symbol(
    item: &OutlineItem,
    idx: &LineIndex,
    aux: Option<&AuxData>,
    toc_cursor: &mut usize,
) -> DocumentSymbol {
    let kind = outline_symbol_kind(item.kind);
    let range = item.range;
    let selection = item.selection_range;
    let mut name = item.name.clone();
    let mut detail = None;
    if let Some(aux) = aux {
        match item.kind {
            OutlineSymbol::Section => {
                if let Some(number) = next_toc_number(aux, &item.name, toc_cursor) {
                    name = format!("{number} {name}");
                }
            }
            OutlineSymbol::Label => {
                detail = aux.labels.get(item.name.as_str()).cloned();
            }
            OutlineSymbol::Float | OutlineSymbol::Theorem => {
                // The float's own number is exactly its child label's.
                detail = item
                    .children
                    .iter()
                    .find(|c| c.kind == OutlineSymbol::Label)
                    .and_then(|label| aux.labels.get(label.name.as_str()).cloned());
            }
            OutlineSymbol::Frame | OutlineSymbol::Macro | OutlineSymbol::Environment => {}
        }
    }
    let children: Vec<DocumentSymbol> = item
        .children
        .iter()
        .map(|child| to_document_symbol(child, idx, aux, toc_cursor))
        .collect();
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range: byte_range_to_lsp(idx, range.start().into(), range.end().into()),
        selection_range: byte_range_to_lsp(idx, selection.start().into(), selection.end().into()),
        children: (!children.is_empty()).then_some(children),
    }
}

/// The number of the next toc entry matching `title`, consuming entries up to and
/// including the match. Titles compare with all whitespace stripped: TeX pads the
/// written form (`\textsc  {Intro}`) where the CST title has none.
pub fn next_toc_number(aux: &AuxData, title: &str, cursor: &mut usize) -> Option<String> {
    let want = normalize_toc_title(title);
    if want.is_empty() {
        return None;
    }
    let (offset, entry) = aux.toc[(*cursor).min(aux.toc.len())..]
        .iter()
        .enumerate()
        .find(|(_, e)| e.number.is_some() && normalize_toc_title(&e.title) == want)?;
    *cursor += offset + 1;
    entry.number.clone()
}

/// Strip all whitespace for toc-title comparison.
pub fn normalize_toc_title(title: &str) -> String {
    title.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Convert a flat [`BibOutlineItem`] into an LSP [`DocumentSymbol`]. Bib entries
/// have no nesting, so there are never children; the cite key is the name and the
/// entry type the detail.
#[allow(deprecated)] // `DocumentSymbol::deprecated` is a required struct field.
pub fn bib_to_document_symbol(item: &BibOutlineItem, idx: &LineIndex) -> DocumentSymbol {
    let range = item.range;
    let selection = item.selection_range;
    DocumentSymbol {
        name: item.name.clone(),
        detail: Some(item.detail.clone()),
        kind: SymbolKind::Constant,
        tags: None,
        deprecated: None,
        range: byte_range_to_lsp(idx, range.start().into(), range.end().into()),
        selection_range: byte_range_to_lsp(idx, selection.start().into(), selection.end().into()),
        children: None,
    }
}
