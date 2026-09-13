//! Symbol conversion from an analysis snapshot.
use super::*;

/// Convert the source outline and supplied last-build numbers to LSP symbols.
pub fn compute_symbols(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    options: &presentation::OutlineOptions,
) -> Vec<DocumentSymbol> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, encoding);
    let items = snapshot.file_outline(file);
    let aux = document_aux(snapshot, snapshot.resolve_labels(), path);
    let mut toc_cursor = 0;
    document_symbols(items, &idx, aux.as_ref(), &mut toc_cursor, options)
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
    bib_outline(
        snapshot.bib_semantic_model(file),
        &snapshot.parsed_bib_tree(file),
    )
    .iter()
    .map(|item| bib_to_document_symbol(item, &idx))
    .collect()
}

/// Bound result storage and perform position/URI construction only after ranking.
pub const WORKSPACE_SYMBOL_LIMIT: usize = 256;
type SymbolRank = (bool, u8, PathBuf, u32, String);
struct SymbolFact<'a> {
    snapshot: &'a Analysis,
    path: &'a Path,
    name: String,
    kind: SymbolKind,
    range: TextRange,
    container: String,
}
struct SymbolSearch<'a> {
    query: String,
    tokens: Vec<String>,
    ranked: std::collections::BTreeMap<SymbolRank, SymbolFact<'a>>,
}
impl<'a> SymbolSearch<'a> {
    fn insert(&mut self, fact: SymbolFact<'a>, detail: Option<&str>, proximity: u8) {
        let text =
            format!("{} {} {}", fact.name, detail.unwrap_or(""), fact.container).to_lowercase();
        if !self.tokens.iter().all(|token| text.contains(token)) {
            return;
        }
        let rank = (
            fact.name.to_lowercase() != self.query,
            proximity,
            fact.path.to_owned(),
            u32::from(fact.range.start()),
            fact.name.clone(),
        );
        self.ranked.entry(rank).or_insert(fact);
        if self.ranked.len() > WORKSPACE_SYMBOL_LIMIT {
            self.ranked.pop_last();
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn outline(
        &mut self,
        snapshot: &'a Analysis,
        path: &'a Path,
        items: &[OutlineItem],
        container: &str,
        aux: Option<&AuxData>,
        cursor: &mut usize,
        options: &presentation::OutlineOptions,
        proximity: u8,
    ) {
        for item in items {
            let (name, detail) = outline_presentation(item, aux, cursor, options);
            let visible = outline_visible(item, options);
            let nested = if visible {
                format!("{container} > {name}")
            } else {
                container.to_owned()
            };
            if visible {
                self.insert(
                    SymbolFact {
                        snapshot,
                        path,
                        name,
                        kind: outline_symbol_kind(item.kind),
                        range: item.selection_range,
                        container: container.into(),
                    },
                    detail.as_deref(),
                    proximity,
                );
            }
            self.outline(
                snapshot,
                path,
                &item.children,
                &nested,
                aux,
                cursor,
                options,
                proximity,
            );
        }
    }
}
pub fn compute_projects_workspace_symbols(
    snapshots: &[Analysis],
    query: &str,
    enc: PositionEncoding,
    options: &dyn Fn(&Path) -> presentation::OutlineOptions,
    active: Option<&Path>,
) -> WorkspaceSymbolResponse {
    let query = query.to_lowercase();
    let mut search = SymbolSearch {
        tokens: query.split_whitespace().map(str::to_owned).collect(),
        query,
        ranked: Default::default(),
    };
    for snapshot in snapshots {
        let active_roots = active
            .map(|path| snapshot.resolve_labels().candidate_roots(path))
            .unwrap_or_default();
        for member in snapshot.project_members() {
            let path = &member.path;
            let roots = snapshot.resolve_labels().candidate_roots(path);
            let proximity = if active == Some(path.as_path()) {
                0
            } else if roots.iter().any(|root| active_roots.contains(root)) {
                1
            } else if active.is_some_and(|p| snapshot.lookup_file(p).is_some()) {
                2
            } else {
                3
            };
            let container = path.file_stem().unwrap_or_default().to_string_lossy();
            if member.kind == FileKind::Bib {
                for item in bib_outline(
                    snapshot.bib_semantic_model(member.file),
                    &snapshot.parsed_bib_tree(member.file),
                ) {
                    search.insert(
                        SymbolFact {
                            snapshot,
                            path,
                            name: item.name,
                            kind: bib_symbol_kind(item.kind),
                            range: item.selection_range,
                            container: container.to_string(),
                        },
                        Some(&item.detail),
                        proximity,
                    );
                }
            } else {
                let aux = document_aux(snapshot, snapshot.resolve_labels(), path);
                search.outline(
                    snapshot,
                    path,
                    snapshot.file_outline(member.file),
                    &container,
                    aux.as_ref(),
                    &mut 0,
                    &options(path),
                    proximity,
                );
            }
        }
    }
    WorkspaceSymbolResponse::WorkspaceSymbolList(
        search
            .ranked
            .into_values()
            .filter_map(|fact| {
                let file = fact.snapshot.lookup_file(fact.path)?;
                let location = Location {
                    uri: path_to_uri(fact.path)?,
                    range: lsp_range(&fact.snapshot.file_line_index(file, enc), fact.range),
                };
                Some(WorkspaceSymbol {
                    base_symbol_information: lsp_types::BaseSymbolInformation {
                        name: fact.name,
                        kind: fact.kind,
                        tags: None,
                        container_name: Some(fact.container),
                    },
                    location: location.into(),
                    data: None,
                })
            })
            .collect(),
    )
}

fn document_symbols(
    items: &[OutlineItem],
    idx: &LineIndex,
    aux: Option<&AuxData>,
    cursor: &mut usize,
    options: &presentation::OutlineOptions,
) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    for item in items {
        let mut symbol = to_document_symbol(item, idx, aux, cursor, options);
        let visible = outline_visible(item, options);
        if visible {
            symbols.push(symbol);
        } else if let Some(children) = symbol.children.take() {
            symbols.extend(children);
        }
    }
    symbols
}

/// Convert an [`OutlineItem`] tree into an LSP [`DocumentSymbol`], mapping byte
/// ranges through the (encoding-aware) [`LineIndex`].
/// Map an [`OutlineSymbol`] to its LSP [`SymbolKind`]. Shared by the per-file
/// `documentSymbol` ([`to_document_symbol`]) and project-wide `workspace/symbol`
/// (`run_workspace_symbols`) outputs so the two never drift.
pub fn outline_symbol_kind(kind: OutlineSymbol) -> SymbolKind {
    match kind {
        OutlineSymbol::Section => SymbolKind::Module,
        OutlineSymbol::Equation => SymbolKind::Number,
        OutlineSymbol::Item => SymbolKind::EnumMember,
        OutlineSymbol::Container => SymbolKind::Namespace,
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
    options: &presentation::OutlineOptions,
) -> DocumentSymbol {
    let kind = outline_symbol_kind(item.kind);
    let range = item.range;
    let selection = item.selection_range;
    let (name, detail) = outline_presentation(item, aux, toc_cursor, options);
    let children = document_symbols(&item.children, idx, aux, toc_cursor, options);
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
        kind: bib_symbol_kind(item.kind),
        tags: None,
        deprecated: None,
        range: byte_range_to_lsp(idx, range.start().into(), range.end().into()),
        selection_range: byte_range_to_lsp(idx, selection.start().into(), selection.end().into()),
        children: (!item.children.is_empty()).then(|| {
            item.children
                .iter()
                .map(|child| bib_to_document_symbol(child, idx))
                .collect()
        }),
    }
}

fn bib_symbol_kind(kind: tex_ls_analysis::bib::outline::BibSymbolKind) -> SymbolKind {
    use tex_ls_analysis::bib::outline::BibSymbolKind;
    match kind {
        BibSymbolKind::Book => SymbolKind::Class,
        BibSymbolKind::Article => SymbolKind::Object,
        BibSymbolKind::Collection => SymbolKind::Namespace,
        BibSymbolKind::Entry => SymbolKind::Constant,
        BibSymbolKind::String => SymbolKind::String,
        BibSymbolKind::Field => SymbolKind::Field,
    }
}

fn outline_visible(item: &OutlineItem, options: &presentation::OutlineOptions) -> bool {
    match item.kind {
        OutlineSymbol::Section => options.sections,
        OutlineSymbol::Float => options.floats,
        OutlineSymbol::Frame => options.frames,
        OutlineSymbol::Theorem => options.theorems,
        OutlineSymbol::Label => options.labels,
        OutlineSymbol::Macro => options.macros,
        OutlineSymbol::Environment => options.environments,
        OutlineSymbol::Item => options.items,
        OutlineSymbol::Container => {
            options.environments
                && item
                    .environment
                    .as_ref()
                    .is_some_and(|name| options.environment_names.contains_key(name))
        }
        OutlineSymbol::Equation => {
            if item
                .children
                .iter()
                .any(|child| child.kind == OutlineSymbol::Label)
            {
                options.labelled_equations
            } else {
                options.unlabelled_equations
            }
        }
    }
}

fn outline_presentation(
    item: &OutlineItem,
    aux: Option<&AuxData>,
    toc_cursor: &mut usize,
    options: &presentation::OutlineOptions,
) -> (String, Option<String>) {
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
            OutlineSymbol::Float | OutlineSymbol::Theorem | OutlineSymbol::Equation => {
                // The float's own number is exactly its child label's.
                detail = item
                    .children
                    .iter()
                    .find(|c| c.kind == OutlineSymbol::Label)
                    .and_then(|label| aux.labels.get(label.name.as_str()).cloned());
            }
            OutlineSymbol::Frame
            | OutlineSymbol::Macro
            | OutlineSymbol::Environment
            | OutlineSymbol::Item
            | OutlineSymbol::Container => {}
        }
    }
    if let Some(display) = item
        .environment
        .as_ref()
        .and_then(|name| options.environment_names.get(name))
    {
        name = if matches!(
            item.kind,
            OutlineSymbol::Float | OutlineSymbol::Theorem | OutlineSymbol::Container
        ) || name == "Equation"
        {
            display.clone()
        } else {
            format!("{display}: {name}")
        };
    }
    (name, detail)
}
