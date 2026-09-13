//! Navigation operations.
use super::*;

/// Linked editing is restricted to two identical, literal delimiter names.
pub fn compute_linked_editing(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> Option<lsp_types::LinkedEditingRanges> {
    if file_kind_for(path) == FileKind::Bib {
        return None;
    }
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    let (_, ranges) = proved_environment_pair(&snapshot.parsed_tree(file), offset)?;
    if !ranges
        .iter()
        .any(|range| range.contains_inclusive(TextSize::from(offset as u32)))
    {
        return None;
    }
    Some(lsp_types::LinkedEditingRanges {
        ranges: ranges
            .into_iter()
            .map(|range| lsp_range(&idx, range))
            .collect(),
        word_pattern: Some("[A-Za-z0-9@*:_-]+".into()),
    })
}

/// The selected literal key, kept distinct by namespace. A command-word cursor
/// falls through to command declarations; it never targets sibling argument keys.
#[derive(Debug)]
pub enum CursorTarget {
    Labels(Vec<SmolStr>),
    Citations(Vec<SmolStr>),
}

/// The renameable key under the cursor: which name(s) to rewrite project-wide
/// (`target`), the precise key-token span the cursor sits on (for
/// the `prepareRename` range), and the current key text as the rename placeholder.
#[derive(Debug)]
pub struct RenameTarget {
    pub(crate) target: CursorTarget,
    pub(crate) span: TextRange,
    pub(crate) placeholder: SmolStr,
}

/// Definition links retain exact origin/name spans and declaration containers.
/// ResponsePolicy lowers these same targets to Locations when needed.
pub fn compute_goto_definition(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    enc: PositionEncoding,
) -> Vec<lsp_types::LocationLink> {
    let Some(origin_file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let index = snapshot.file_line_index(origin_file, enc);
    let offset = index.offset_at(position.line, position.character);
    let at = TextSize::new(offset as u32);
    let origin = if file_kind_for(path) == FileKind::Bib {
        bib_strings::target(snapshot.bib_semantic_model(origin_file), offset)
            .map(|(_, range)| range)
    } else {
        let model = snapshot.semantic_model(origin_file);
        rename_target_under_cursor(model, offset)
            .map(|target| target.span)
            .or_else(|| glossary::target(model, offset).map(|(_, range)| range))
            .or_else(|| {
                snapshot
                    .file_references(origin_file)
                    .iter()
                    .find(|reference| reference.range.contains_inclusive(at))
                    .map(|reference| reference.range)
            })
            .or_else(|| {
                name_refs::name_target_under_cursor(
                    &snapshot.parsed_tree(origin_file),
                    offset,
                    snapshot.definition_sites(origin_file),
                )
                .map(|target| target.span)
            })
    }
    .map(|range| lsp_range(&index, range));
    definition_locations(snapshot, path, position, enc)
        .into_iter()
        .filter_map(|location| {
            let target = uri_to_fs_path(&location.uri)?;
            let file = snapshot.lookup_file(&target);
            let container = file
                .and_then(|file| {
                    let index = snapshot.file_line_index(file, enc);
                    let at = TextSize::new(
                        index.offset_at(location.range.start.line, location.range.start.character)
                            as u32,
                    );
                    let range = if file_kind_for(&target) == FileKind::Bib {
                        use tex_ls_parser::bib::syntax::SyntaxKind as Kind;
                        snapshot
                            .parsed_bib_tree(file)
                            .token_at_offset(at)
                            .right_biased()?
                            .parent_ancestors()
                            .find(|node| matches!(node.kind(), Kind::ENTRY | Kind::STRING_ENTRY))
                            .map(|node| node.text_range())
                    } else {
                        let model = snapshot.semantic_model(file);
                        model
                            .labels()
                            .iter()
                            .map(|item| (item.key_range, item.range))
                            .chain(
                                model
                                    .glossary_defs()
                                    .iter()
                                    .map(|item| (item.key_range, item.range)),
                            )
                            .chain(
                                model
                                    .bibitems()
                                    .iter()
                                    .map(|item| (item.key_range, item.range)),
                            )
                            .chain(
                                snapshot
                                    .definition_sites(file)
                                    .iter()
                                    .map(|item| (item.name_range, item.range)),
                            )
                            .find(|(selection, _)| selection.contains_inclusive(at))
                            .map(|(_, range)| range)
                    }?;
                    Some(lsp_range(&index, range))
                })
                .unwrap_or(location.range);
            Some(lsp_types::LocationLink {
                origin_selection_range: origin,
                target_uri: location.uri,
                target_range: container,
                target_selection_range: location.range,
            })
        })
        .collect()
}

/// Resolve the construct at `position` using the snapshot's source and project namespaces.
fn definition_locations(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    enc: PositionEncoding,
) -> Vec<Location> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib {
        let model = snapshot.bib_semantic_model(file);
        return bib_strings::target(model, offset)
            .map(|(key, _)| {
                bib_strings::occurrences(model, &key)
                    .into_iter()
                    .filter(|(_, definition)| *definition)
                    .filter_map(|(range, _)| location_for(path, &idx, range))
                    .collect()
            })
            .unwrap_or_default();
    }

    if let Some((key, _)) = glossary::target(snapshot.semantic_model(file), offset) {
        return glossary::locations(snapshot, path, &key, true, true, enc);
    }

    // Resolve the reference under the cursor. Keep the tree for name and file targets.
    let (root, target, lint_path) = (
        snapshot.parsed_tree(file),
        reference_under_cursor(snapshot.semantic_model(file), offset),
        snapshot.file_path(file).to_path_buf(),
    );
    if let Some(target) = target {
        let (resolution, citations) = snapshot.resolve_project();
        return match target {
            CursorTarget::Labels(names) => {
                resolve_label_locations(snapshot, resolution, &lint_path, &names, enc)
            }
            CursorTarget::Citations(names) => {
                resolve_citation_locations(snapshot, citations, &lint_path, &names, enc)
            }
        };
    }
    // Not a `\ref`/`\cite`: a user command/environment name jumps to its
    // definition sites (`\newcommand`/`\def`/xparse, `\newenvironment`) across
    // the macro namespace. A name with no project definition (a built-in) falls
    // through to the file-target tier below.
    let sites = snapshot.definition_sites(file);
    if let Some(target) = name_refs::name_target_under_cursor(&root, offset, sites) {
        let defs = name_definition_sites(snapshot, &lint_path, &target);
        if !defs.is_empty() {
            return defs
                .into_iter()
                .filter_map(|(def_path, range)| {
                    let file = snapshot.lookup_file(&def_path)?;
                    let idx = snapshot.file_line_index(file, enc);
                    location_for(&def_path, &idx, range)
                })
                .collect();
        }
    }
    // Fall back to a file-referencing argument
    // (include/package/class/bib/graphics) under the cursor, jumping to the
    // resolved on-disk target — the same resolution the document-link path uses,
    // so a system package resolves through the TEXMF index too.
    file_target_under_cursor(snapshot, file, offset)
}

/// The go-to-definition file target under `offset`: the document link whose argument
/// span covers the cursor, mapped to a whole-file [`Location`]. Reuses
/// [`document_link::document_links`] (disk-aware and TEXMF-aware), so every command it
/// resolves—`\input`, `\usepackage`, `\includegraphics`, …—becomes navigable. Empty
/// when the cursor is not on a resolvable file argument.
pub fn file_target_under_cursor(
    snapshot: &Analysis,
    file: tex_ls_analysis::incremental::SourceId,
    offset: usize,
) -> Vec<Location> {
    let at = TextSize::new(offset as u32);
    snapshot
        .document_links(file)
        .into_iter()
        .find(|link| link.range.contains_inclusive(at))
        .and_then(|link| path_to_uri(&link.target))
        .map(|uri| {
            vec![Location {
                uri,
                range: Range::default(),
            }]
        })
        .unwrap_or_default()
}

/// The cite/ref key whose key range covers `offset`, refs taking precedence
/// (a position is never both). Returns owned keys so the borrowed model can drop.
pub fn reference_under_cursor(model: &SemanticModel, offset: usize) -> Option<CursorTarget> {
    let at = TextSize::new(offset as u32);
    let label_names: Vec<SmolStr> = model
        .refs()
        .iter()
        .filter(|r| r.key_range.contains_inclusive(at))
        .map(|r| r.name.clone())
        .collect();
    if !label_names.is_empty() {
        return Some(CursorTarget::Labels(label_names));
    }
    let cite_names: Vec<SmolStr> = model
        .citations()
        .iter()
        .filter(|c| c.key_range.contains_inclusive(at))
        .map(|c| c.name.clone())
        .collect();
    (!cite_names.is_empty()).then_some(CursorTarget::Citations(cite_names))
}

/// Compute every use location for a find-references at `position`. The inverse of
/// [`compute_goto_definition`]: resolves a label/key (from a `\ref`/`\cite` use,
/// a `\label` definition, or — in a `.bib` buffer — an `@entry` key) to all of its
/// `\ref`/`\cite` use sites across the namespace, falling back to command/
/// environment *name* occurrences ([`name_reference_locations`]) when the cursor
/// is not on a key. `include_declaration` appends the
/// `\label`/`@entry`/definition-site occurrence to the results.
#[allow(clippy::too_many_arguments)]
pub fn compute_references(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    include_declaration: bool,
    enc: PositionEncoding,
) -> Vec<Location> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib
        && let Some((key, _)) = bib_strings::target(snapshot.bib_semantic_model(file), offset)
    {
        return bib_strings::occurrences(snapshot.bib_semantic_model(file), &key)
            .into_iter()
            .filter(|(_, definition)| include_declaration || !definition)
            .filter_map(|(range, _)| location_for(path, &idx, range))
            .collect();
    }

    if file_kind_for(path) != FileKind::Bib
        && let Some((key, _)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        return glossary::locations(snapshot, path, &key, false, include_declaration, enc);
    }

    let (resolution, citations) = snapshot.resolve_project();

    // `.bib` origin: the `@entry` key under the cursor → its `\cite` uses. A
    // `.bib` path is not keyed in the citation `component_of`, so resolution
    // goes through `bib_citers`.
    if file_kind_for(path) == FileKind::Bib {
        let Some((key, key_range)) = bib_entry_under_cursor(snapshot, path, offset) else {
            return Vec::new();
        };
        let origin = snapshot
            .lookup_file(path)
            .map(|file| snapshot.file_path(file).to_path_buf())
            .unwrap_or_else(|| path.to_path_buf());
        let decl = if include_declaration {
            location_for(&origin, &idx, key_range)
        } else {
            None
        };
        return reference_citation_locations(
            snapshot,
            citations,
            &origin,
            FileKind::Bib,
            &[key],
            include_declaration,
            decl,
            enc,
        );
    }

    // `.tex` origin: a `\ref`/`\cite` use *or* a `\label` definition. The parsed
    // `root` is kept for the command/environment name fallback below.
    let (root, target, origin) = (
        snapshot.parsed_tree(file),
        references_target_under_cursor(snapshot.semantic_model(file), offset),
        snapshot.file_path(file).to_path_buf(),
    );
    if let Some(target) = target {
        return match target {
            CursorTarget::Labels(names) => reference_label_locations(
                snapshot,
                resolution,
                &origin,
                &names,
                include_declaration,
                enc,
            ),
            CursorTarget::Citations(names) => reference_citation_locations(
                snapshot,
                citations,
                &origin,
                FileKind::Tex,
                &names,
                include_declaration,
                None,
                enc,
            ),
        };
    }
    // Not a key: a command or environment *name* under the cursor — a pure
    // occurrence search across the macro namespace, ungated (built-ins included;
    // only *rename* is gated to user-defined names).
    let sites = snapshot.definition_sites(file);
    let Some(target) = name_refs::name_target_under_cursor(&root, offset, sites) else {
        return Vec::new();
    };
    name_reference_locations(snapshot, &origin, &target, include_declaration, enc)
}

/// Every occurrence of a command/environment name across `origin`'s macro
/// namespace — the name-based tier of [`compute_references`]. Command occurrences
/// are full `\name` token ranges (definition-site names included: the `\mycmd` in
/// `\newcommand{\mycmd}` is itself a `CONTROL_WORD`, filtered out by range equality
/// against its [`DefSite`] unless `include_declaration`). Environment occurrences
/// are `\begin`/`\end` name spans; their `\newenvironment{name}` definition names
/// are invisible to that walk, so `include_declaration` *adds* them.
///
/// [`DefSite`]: tex_ls_parser::semantic::DefSite
pub fn name_reference_locations(
    snapshot: &Analysis,
    origin: &Path,
    target: &NameTarget,
    include_declaration: bool,
    enc: PositionEncoding,
) -> Vec<Location> {
    let locations = snapshot
        .name_references(origin, target, include_declaration)
        .into_iter()
        .map(|(file, range)| {
            location_for(
                snapshot.file_path(file),
                &snapshot.file_line_index(file, enc),
                range,
            )
        })
        .collect();
    dedup_locations(locations)
}

/// The matching [`DefSite`]s of `target` across `origin`'s macro namespace, as
/// `(file, name span)` pairs in member order. Serves three consumers: the
/// user-defined rename gate (non-empty = defined in the project), goto-definition
/// (each pair becomes a [`Location`]), and environment rename (each name span is
/// rewritten alongside the `\begin`/`\end` occurrences).
///
/// [`DefSite`]: tex_ls_parser::semantic::DefSite
pub fn name_definition_sites(
    snapshot: &Analysis,
    origin: &Path,
    target: &NameTarget,
) -> Vec<(PathBuf, TextRange)> {
    snapshot
        .name_definitions(origin, target)
        .into_iter()
        .map(|(file, range)| (snapshot.file_path(file).to_path_buf(), range))
        .collect()
}

/// Like [`reference_under_cursor`] but also recognizes a `\label` *definition*
/// under the cursor, so find-references can be invoked from the definition site
/// (a `\ref` and a `\label` both resolve to the same label name). Precedence
/// matches [`reference_under_cursor`] (refs, then citations), with label defs
/// slotted last; a position is in at most one of the three.
pub fn references_target_under_cursor(
    model: &SemanticModel,
    offset: usize,
) -> Option<CursorTarget> {
    if let Some(target) = reference_under_cursor(model, offset) {
        return Some(target);
    }
    let at = TextSize::new(offset as u32);
    if let Some(item) = model
        .bibitems()
        .iter()
        .find(|item| item.key_range.contains_inclusive(at))
    {
        return Some(CursorTarget::Citations(vec![item.name.clone()]));
    }
    let label_names: Vec<SmolStr> = model
        .labels()
        .iter()
        .filter(|l| l.key_range.contains_inclusive(at))
        .map(|l| l.name.clone())
        .collect();
    (!label_names.is_empty()).then_some(CursorTarget::Labels(label_names))
}

/// The renameable key whose **key-token** range (not the whole-command range)
/// covers `offset`: a `\ref`/`\cite` use or a `\label` definition. Keyed on
/// `key_range` so the cursor must sit on the key itself — a position on the command
/// word, the braces, or a sibling key in a `\cref{a,b}` resolves to `None`, which is
/// what makes `prepareRename` decline outside a key. Precedence mirrors
/// [`reference_under_cursor`] (refs, then citations, then label defs); the spans are
/// disjoint, so at most one matches.
pub fn rename_target_under_cursor(model: &SemanticModel, offset: usize) -> Option<RenameTarget> {
    let at = TextSize::new(offset as u32);
    if let Some(r) = model
        .refs()
        .iter()
        .find(|r| r.key_range.contains_inclusive(at))
    {
        return Some(RenameTarget {
            target: CursorTarget::Labels(vec![r.name.clone()]),
            span: r.key_range,
            placeholder: r.name.clone(),
        });
    }
    if let Some(c) = model
        .citations()
        .iter()
        .find(|c| c.key_range.contains_inclusive(at))
    {
        return Some(RenameTarget {
            target: CursorTarget::Citations(vec![c.name.clone()]),
            span: c.key_range,
            placeholder: c.name.clone(),
        });
    }
    if let Some(item) = model
        .bibitems()
        .iter()
        .find(|item| item.key_range.contains_inclusive(at))
    {
        return Some(RenameTarget {
            target: CursorTarget::Citations(vec![item.name.clone()]),
            span: item.key_range,
            placeholder: item.name.clone(),
        });
    }
    let label = model
        .labels()
        .iter()
        .find(|l| l.key_range.contains_inclusive(at))?;
    Some(RenameTarget {
        target: CursorTarget::Labels(vec![label.name.clone()]),
        span: label.key_range,
        placeholder: label.name.clone(),
    })
}

/// The name spans to highlight when the cursor at byte `offset` sits on a
/// `\begin{env}` or `\end{env}` delimiter: both paired names of the enclosing
/// `ENVIRONMENT` (or just the begin's when the environment is unclosed). Purely
/// syntactic — the parser already pairs begin/end structurally. Empty when the
/// cursor isn't inside a `BEGIN`/`END` node (a cursor in the body walks up to the
/// `ENVIRONMENT` without passing through `BEGIN`/`END`, so it resolves to nothing).
/// A stray `\end` (no `ENVIRONMENT` parent) self-highlights.
pub fn environment_pair_ranges(root: &SyntaxNode, offset: usize) -> Vec<TextRange> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return Vec::new(),
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };
    let delimiter = [left, right].into_iter().flatten().find_map(|token| {
        token
            .parent_ancestors()
            .find(|n| matches!(n.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
    });
    let Some(delimiter) = delimiter else {
        return Vec::new();
    };
    match delimiter.parent() {
        Some(env) if env.kind() == SyntaxKind::ENVIRONMENT => env
            .children()
            .filter(|c| matches!(c.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
            .filter_map(|c| tex_ls_parser::ast::environment_name_range(&c))
            .collect(),
        // A stray `\end` with no open environment: highlight it alone.
        _ => tex_ls_parser::ast::environment_name_range(&delimiter)
            .into_iter()
            .collect(),
    }
}

/// A proved local pair for editing. Body positions are accepted for code actions;
/// linked editing separately requires the cursor on a name.
pub fn proved_environment_pair(
    root: &SyntaxNode,
    offset: usize,
) -> Option<(String, Vec<TextRange>)> {
    use tex_ls_parser::ast::AstNode;

    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };
    let env = [left, right].into_iter().flatten().find_map(|token| {
        token
            .parent_ancestors()
            .find_map(tex_ls_parser::ast::Environment::cast)
    })?;
    let begin = env.begin()?;
    let old_name = begin.name()?;
    let end = env.end()?;
    if end.name()? != old_name {
        return None;
    }
    let ranges = env
        .syntax()
        .children()
        .filter(|child| matches!(child.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
        .map(|child| tex_ls_parser::ast::environment_name_range(&child))
        .collect::<Option<Vec<_>>>()?;
    if ranges.len() != 2 || ranges[0].end() > ranges[1].start() {
        return None;
    }
    let text = root.text().to_string();
    let base = root.text_range().start();
    let text_at = |range: TextRange| {
        text.get(usize::from(range.start() - base)..usize::from(range.end() - base))
    };
    (text_at(ranges[0]) == text_at(ranges[1]) && text_at(ranges[0]).is_some())
        .then_some((old_name, ranges))
}

/// Compute the `prepareRename` range + placeholder at `position`: the key-token span
/// under the cursor in the snapshot. A `.bib` cursor resolves
/// to its `@entry` key. `None` when the cursor isn't on a renameable key.
/// Compute the document highlights for `position`. Two cases, tried in order:
///
/// - **Cross-reference key** under the cursor (a `\ref`/`\cite` use or a `\label`
///   definition): every same-key occurrence in the *same* buffer. Single-file, so no
///   project resolution — the `\label` definition shades as
///   [`DocumentHighlightKind::Write`] and every `\ref`/`\cite` use as
///   [`DocumentHighlightKind::Read`]. Strict key gating (via
///   [`rename_target_under_cursor`]): a cursor on the command word, the braces, or a
///   sibling key in `\cref{a,b}` highlights nothing for that key.
/// - **Environment delimiter** under the cursor (a `\begin{env}`/`\end{env}`): the
///   matching pair's name spans, shaded [`DocumentHighlightKind::Text`] (via
///   [`environment_pair_ranges`]).
///
/// The two positions are disjoint (a key sits in a command `GROUP`, a name in a
/// `BEGIN`/`END` `NAME_GROUP`), so key-first ordering is behavior-preserving.
/// `.bib` buffers yield no highlights (an `@entry` key has no in-file uses).
pub fn compute_document_highlight(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> Vec<DocumentHighlight> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib
        && let Some((key, _)) = bib_strings::target(snapshot.bib_semantic_model(file), offset)
    {
        return bib_strings::occurrences(snapshot.bib_semantic_model(file), &key)
            .into_iter()
            .map(|(range, definition)| DocumentHighlight {
                range: lsp_range(&idx, range),
                kind: Some(if definition {
                    DocumentHighlightKind::Write
                } else {
                    DocumentHighlightKind::Read
                }),
            })
            .collect();
    }

    if file_kind_for(path) != FileKind::Bib
        && let Some((key, _)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        return glossary::occurrences(snapshot, path, &key)
            .into_iter()
            .filter(|(source, _, _)| source == path)
            .map(|(_, range, definition)| DocumentHighlight {
                range: lsp_range(&idx, range),
                kind: Some(if definition {
                    DocumentHighlightKind::Write
                } else {
                    DocumentHighlightKind::Read
                }),
            })
            .collect();
    }

    if file_kind_for(path) == FileKind::Bib {
        return Vec::new();
    }
    let highlight = |range: TextRange, kind: DocumentHighlightKind| DocumentHighlight {
        range: lsp_range(&idx, range),
        kind: Some(kind),
    };
    let collect = |root: &SyntaxNode, model: &SemanticModel| -> Vec<DocumentHighlight> {
        // A cross-reference key takes precedence when the cursor is on one.
        if let Some(target) = rename_target_under_cursor(model, offset) {
            return match &target.target {
                CursorTarget::Labels(_) => {
                    let name = &target.placeholder;
                    let defs = model
                        .labels()
                        .iter()
                        .filter(|l| &l.name == name)
                        .map(|l| highlight(l.key_range, DocumentHighlightKind::Write));
                    let uses = model
                        .refs()
                        .iter()
                        .filter(|r| &r.name == name)
                        .map(|r| highlight(r.key_range, DocumentHighlightKind::Read));
                    defs.chain(uses).collect()
                }
                CursorTarget::Citations(_) => {
                    let name = &target.placeholder;
                    model
                        .citations()
                        .iter()
                        .filter(|c| &c.name == name)
                        .map(|c| highlight(c.key_range, DocumentHighlightKind::Read))
                        .chain(
                            model
                                .bibitems()
                                .iter()
                                .filter(|item| &item.name == name)
                                .map(|item| {
                                    highlight(item.key_range, DocumentHighlightKind::Write)
                                }),
                        )
                        .collect()
                }
            };
        }
        if let Some(target) =
            name_refs::name_target_under_cursor(root, offset, snapshot.definition_sites(file))
            && target.kind == NameKind::Command
            && name_rename_allowed(snapshot, path, snapshot.definition_sites(file), &target)
        {
            return name_refs::command_occurrences(root, &target.name)
                .into_iter()
                .map(|range| {
                    let definition = snapshot
                        .definition_sites(file)
                        .iter()
                        .any(|site| site.name_range == range);
                    highlight(
                        name_refs::strip_backslash(range),
                        if definition {
                            DocumentHighlightKind::Write
                        } else {
                            DocumentHighlightKind::Read
                        },
                    )
                })
                .collect();
        }
        // Otherwise, a `\begin`/`\end` delimiter pair.
        environment_pair_ranges(root, offset)
            .into_iter()
            .map(|range| highlight(range, DocumentHighlightKind::Text))
            .collect()
    };
    collect(&snapshot.parsed_tree(file), snapshot.semantic_model(file))
}

fn key_rename_allowed(
    snapshot: &Analysis,
    path: &Path,
    target: &CursorTarget,
    replacement: Option<&str>,
) -> bool {
    let (occurrences, collision, closed) = match target {
        CursorTarget::Labels(names) => (
            snapshot.label_occurrences(path, names),
            replacement.is_some_and(|new| {
                !names.iter().any(|old| old == new)
                    && !snapshot.label_occurrences(path, &[new.into()]).is_empty()
            }),
            snapshot.resolve_labels().is_closed(path),
        ),
        CursorTarget::Citations(names) => {
            let citations = snapshot.resolve_citations();
            // A source-origin rename edits only that source's namespace. A
            // bibliography declaration shared with another root cannot be changed
            // without also proving and editing that root's uses.
            if file_kind_for(path) != FileKind::Bib {
                let members = citations.namespace_members(path);
                if snapshot
                    .citation_occurrences(path, names)
                    .iter()
                    .any(|site| {
                        site.definition
                            && file_kind_for(&site.path) == FileKind::Bib
                            && citations
                                .bib_citers(&site.path)
                                .iter()
                                .any(|citer| !members.contains(citer))
                    })
                {
                    return false;
                }
            }
            let scopes = if file_kind_for(path) == FileKind::Bib {
                citations.bib_citers(path)
            } else {
                vec![path]
            };
            let closed = scopes.iter().all(|scope| {
                citations.is_closed(scope)
                    && snapshot
                        .citation_occurrences(scope, names)
                        .iter()
                        .filter(|site| site.definition)
                        .count()
                        == 1
            });
            let collision = replacement.is_some_and(|new| {
                !names.iter().any(|old| old.eq_ignore_ascii_case(new))
                    && (scopes.iter().any(|scope| {
                        !snapshot
                            .citation_occurrences(scope, &[new.into()])
                            .is_empty()
                    }) || !snapshot
                        .citation_occurrences(path, &[new.into()])
                        .is_empty())
            });
            (
                snapshot.citation_occurrences(path, names),
                collision,
                closed,
            )
        }
    };
    closed && !collision && occurrences.iter().filter(|site| site.definition).count() == 1
}

pub fn compute_prepare_rename(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> Option<(Range, String)> {
    if file_kind_for(path) != FileKind::Bib && !snapshot.resolve_labels().has_unique_root(path) {
        return None;
    }
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib
        && let Some((key, range)) = bib_strings::target(snapshot.bib_semantic_model(file), offset)
    {
        return bib_strings::rename_allowed(snapshot.bib_semantic_model(file), &key)
            .then(|| (lsp_range(&idx, range), key));
    }

    if file_kind_for(path) != FileKind::Bib
        && let Some((key, range)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        return glossary::rename_allowed(snapshot, path, &key)
            .then(|| (lsp_range(&idx, range), key.to_string()));
    }

    // `.bib` origin: the `@entry` key under the cursor.
    if file_kind_for(path) == FileKind::Bib {
        let (key, key_range) = bib_entry_under_cursor(snapshot, path, offset)?;
        return key_rename_allowed(
            snapshot,
            path,
            &CursorTarget::Citations(vec![key.clone()]),
            None,
        )
        .then(|| (lsp_range(&idx, key_range), key.to_string()));
    }
    // `.tex` origin: a `\ref`/`\cite` use or a `\label` definition. The parsed
    // `root` is kept for the command/environment name fallback below.
    let (root, target) = (
        snapshot.parsed_tree(file),
        rename_target_under_cursor(snapshot.semantic_model(file), offset),
    );
    if let Some(target) = target {
        return key_rename_allowed(snapshot, path, &target.target, None)
            .then(|| (lsp_range(&idx, target.span), target.placeholder.to_string()));
    }
    // Not a key: a command or environment name, gated to user-defined names
    // (a project definition site must exist — renaming `\textbf` or
    // `verbatim` over a partial namespace view is a footgun).
    let sites = snapshot.definition_sites(file);
    let target = name_refs::name_target_under_cursor(&root, offset, sites)?;
    if !name_rename_allowed(snapshot, path, sites, &target) {
        return None;
    }
    Some((lsp_range(&idx, target.span), target.name.to_string()))
}

/// The user-defined rename gate: `target` is renameable when a matching
/// definition site exists in `origin`'s macro namespace. The source's own sites
/// are checked first to avoid an unnecessary namespace query. This is
/// deliberately the *user tier* only: built-in and CWL names never pass, so
/// `\alpha` and `verbatim` decline. References stay ungated.
pub fn name_rename_allowed(
    snapshot: &Analysis,
    origin: &Path,
    own_sites: &[tex_ls_parser::semantic::DefSite],
    target: &NameTarget,
) -> bool {
    if !snapshot.resolve_labels().has_unique_root(origin) {
        return false;
    }
    if name_refs::macro_roots(snapshot.resolve_labels(), snapshot.package_graph(), origin).len() > 1
    {
        return false;
    }
    if !snapshot.resolve_labels().is_closed(origin) {
        return false;
    }
    let want = match target.kind {
        NameKind::Command => DefSiteKind::Command,
        NameKind::Environment => DefSiteKind::Environment,
    };
    if own_sites
        .iter()
        .any(|site| site.kind == want && site.name == target.name)
    {
        return true;
    }
    !name_definition_sites(snapshot, origin, target).is_empty()
}

/// Compute the [`WorkspaceEdit`] renaming the key — or, in the name-based fallback
/// tier, the user-defined command/environment name — under the cursor to `new_name`
/// across its namespace — the write mirror of [`compute_references`]. Rewrites only
/// the per-key `key_range` of each occurrence (so a sibling key in `\cref{a,b}` is
/// untouched), always including the definition. A unique, complete namespace and
/// collision-free destination are required. `None` when `new_name` is not syntactically safe for the target
/// ([`is_valid_key`], or [`is_valid_command_name`] for a command), or nothing
/// resolves.
pub fn compute_rename(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    new_name: &str,
    enc: PositionEncoding,
) -> Option<WorkspaceEdit> {
    if file_kind_for(path) != FileKind::Bib && !snapshot.resolve_labels().has_unique_root(path) {
        return None;
    }
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib
        && let Some((key, _)) = bib_strings::target(snapshot.bib_semantic_model(file), offset)
    {
        return bib_strings::rename(snapshot, path, &key, new_name, enc);
    }

    if file_kind_for(path) != FileKind::Bib
        && let Some((key, _)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        return glossary::rename(snapshot, path, &key, new_name, enc);
    }

    let changes = (|| {
        let (resolution, citations) = snapshot.resolve_project();

        // `.bib` origin: the `@entry` key under the cursor → its `\cite` uses + the
        // entry itself.
        if file_kind_for(path) == FileKind::Bib {
            if !is_valid_key(new_name) {
                return HashMap::new();
            }
            let Some((key, _)) = bib_entry_under_cursor(snapshot, path, offset) else {
                return HashMap::new();
            };
            if !key_rename_allowed(
                snapshot,
                path,
                &CursorTarget::Citations(vec![key.clone()]),
                Some(new_name),
            ) {
                return HashMap::new();
            }
            let origin = snapshot
                .lookup_file(path)
                .map(|file| snapshot.file_path(file).to_path_buf())
                .unwrap_or_else(|| path.to_path_buf());
            return rename_citation_edits(
                snapshot,
                citations,
                &origin,
                FileKind::Bib,
                &[key],
                new_name,
                enc,
            );
        }

        // `.tex` origin: a `\ref`/`\cite` use or a `\label` definition. The parsed
        // `root` is kept for the command/environment name fallback below.
        let (root, target, origin) = (
            snapshot.parsed_tree(file),
            rename_target_under_cursor(snapshot.semantic_model(file), offset),
            snapshot.file_path(file).to_path_buf(),
        );
        if let Some(target) = target {
            if !is_valid_key(new_name)
                || !key_rename_allowed(snapshot, path, &target.target, Some(new_name))
            {
                return HashMap::new();
            }
            return match target.target {
                CursorTarget::Labels(names) => {
                    rename_label_edits(snapshot, resolution, &origin, &names, new_name, enc)
                }
                CursorTarget::Citations(names) => rename_citation_edits(
                    snapshot,
                    citations,
                    &origin,
                    FileKind::Tex,
                    &names,
                    new_name,
                    enc,
                ),
            };
        }
        // Not a key: a command or environment name, gated to user-defined names
        // like `compute_prepare_rename` (a client may skip prepareRename).
        let sites = snapshot.definition_sites(file);
        let Some(target) = name_refs::name_target_under_cursor(&root, offset, sites) else {
            return HashMap::new();
        };
        if !name_rename_allowed(snapshot, path, sites, &target) {
            return HashMap::new();
        }
        match target.kind {
            NameKind::Command => {
                // The placeholder is the bare name, but a typed `\newname` is
                // accepted too — strip one leading backslash so both agree.
                let bare = new_name.strip_prefix('\\').unwrap_or(new_name);
                if !is_valid_command_name(bare, &target.name)
                    || (bare != target.name
                        && (tex_ls_analysis::hover::lookup_command(
                            snapshot.scope_signatures(file),
                            bare,
                        )
                        .is_some()
                            || !snapshot
                                .name_references(
                                    &origin,
                                    &NameTarget {
                                        name: bare.into(),
                                        ..target.clone()
                                    },
                                    true,
                                )
                                .is_empty()))
                {
                    return HashMap::new();
                }
                rename_command_edits(snapshot, &origin, &target.name, bare, enc)
            }
            NameKind::Environment => {
                if !is_valid_key(new_name)
                    || (new_name != target.name
                        && (tex_ls_analysis::hover::lookup_environment(
                            snapshot.scope_signatures(file),
                            new_name,
                        )
                        .is_some()
                            || !snapshot
                                .name_references(
                                    &origin,
                                    &NameTarget {
                                        name: new_name.into(),
                                        ..target.clone()
                                    },
                                    true,
                                )
                                .is_empty()))
                {
                    return HashMap::new();
                }
                rename_environment_edits(snapshot, &origin, &target, new_name, enc)
            }
        }
    })();
    if changes.keys().filter_map(uri_to_fs_path).any(|target| {
        file_kind_for(&target) != FileKind::Bib
            && name_refs::macro_roots(snapshot.resolve_labels(), snapshot.package_graph(), &target)
                .len()
                > 1
    }) {
        return None;
    }
    finalize_rename(changes)
}

/// Find the bibliography key covering the byte offset in the captured source.
pub fn bib_entry_under_cursor(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
) -> Option<(SmolStr, TextRange)> {
    let file = snapshot.lookup_file(path)?;
    let at = TextSize::new(offset as u32);
    snapshot
        .bib_semantic_model(file)
        .entries()
        .iter()
        .find(|entry| entry.key_range.contains_inclusive(at))
        .map(|entry| (entry.key.clone(), entry.key_range))
}

/// Every `\ref`-family use of `names` across `origin`'s label namespace, plus the
/// `\label` definitions when `include_declaration`. The inverse of
/// [`resolve_label_locations`]: scans each namespace member's uses, not its defs.
pub fn reference_label_locations(
    snapshot: &Analysis,
    _resolution: &ResolvedLabels,
    origin: &Path,
    names: &[SmolStr],
    include_declaration: bool,
    enc: PositionEncoding,
) -> Vec<Location> {
    occurrence_locations(
        snapshot,
        snapshot
            .label_occurrences(origin, names)
            .into_iter()
            .filter(|item| include_declaration || !item.definition),
        enc,
    )
}

/// Every `\cite`-family use of `names` across `origin`'s citation namespace, plus
/// the bibliography `@entry` definitions when `include_declaration`. Use sites
/// live in `.tex` members — `bib_citers` for a `.bib` origin (whose path is not
/// keyed in the citation `component_of`), else `namespace_members`. The
/// declaration is the cursor's own entry (`decl_for_bib`) for a `.bib` origin, or
/// [`resolve_citation_locations`] for a `.tex` origin.
#[allow(clippy::too_many_arguments)]
pub fn reference_citation_locations(
    snapshot: &Analysis,
    _citations: &ResolvedCitations,
    origin: &Path,
    kind: FileKind,
    names: &[SmolStr],
    include_declaration: bool,
    decl_for_bib: Option<Location>,
    enc: PositionEncoding,
) -> Vec<Location> {
    let mut locations = occurrence_locations(
        snapshot,
        snapshot
            .citation_occurrences(origin, names)
            .into_iter()
            .filter(|item| !item.definition || (include_declaration && kind != FileKind::Bib)),
        enc,
    );
    if include_declaration && kind == FileKind::Bib {
        locations.extend(decl_for_bib);
    }
    locations
}

/// For each `\ref` key, the `\label{key}` definition sites across the file's
/// namespace: `resolution.definers` gives the defining files, each file's
/// `semantic_model` the matching `LabelDef.range`.
pub fn resolve_label_locations(
    snapshot: &Analysis,
    _resolution: &ResolvedLabels,
    lint_path: &Path,
    names: &[SmolStr],
    enc: PositionEncoding,
) -> Vec<Location> {
    occurrence_locations(
        snapshot,
        snapshot
            .label_occurrences(lint_path, names)
            .into_iter()
            .filter(|item| item.definition),
        enc,
    )
}

/// For each `\cite` key, the `@entry{key,…}` sites in the `.bib` files of the
/// citation namespace: `citations.bib_definers` gives the analyzed bibliographies,
/// each `bib_semantic_model` the matching `Entry.key_range` (case-insensitive, as
/// BibTeX folds key case).
pub fn resolve_citation_locations(
    snapshot: &Analysis,
    _citations: &ResolvedCitations,
    lint_path: &Path,
    names: &[SmolStr],
    enc: PositionEncoding,
) -> Vec<Location> {
    occurrence_locations(
        snapshot,
        snapshot
            .citation_occurrences(lint_path, names)
            .into_iter()
            .filter(|item| item.definition),
        enc,
    )
}

/// Build an LSP [`Location`] from a definer file's path and a byte range in its
/// text. A path that cannot form a `file://` URI yields `None` (skipped).
pub fn location_for(path: &Path, idx: &LineIndex, range: TextRange) -> Option<Location> {
    Some(Location {
        uri: path_to_uri(path)?,
        range: byte_range_to_lsp(idx, usize::from(range.start()), usize::from(range.end())),
    })
}

/// Drop duplicate locations (same URI + range), which can arise when several keys
/// in a list command resolve to the same site.
pub fn dedup_locations(locations: Vec<Option<Location>>) -> Vec<Location> {
    let mut seen = HashSet::new();
    locations
        .into_iter()
        .flatten()
        .filter(|loc| seen.insert((loc.uri.as_str().to_owned(), loc.range.start, loc.range.end)))
        .collect()
}

/// Every `\ref`-family use of `names` across `origin`'s label namespace, plus every
/// `\label` definition, each rewritten to `new_name` at its precise `key_range`. The
/// rename mirror of [`reference_label_locations`] — `TextEdit`s grouped by URI
/// instead of `Location`s, and the definition is *always* included (a rename rewrites
/// the def, unlike find-references' optional declaration).
pub fn rename_label_edits(
    snapshot: &Analysis,
    _resolution: &ResolvedLabels,
    origin: &Path,
    names: &[SmolStr],
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    occurrence_edits(
        snapshot,
        snapshot.label_occurrences(origin, names),
        new_name,
        enc,
    )
}

/// Every `\cite`-family use of `names` across `origin`'s citation namespace, plus the
/// bibliography `@entry` keys, rewritten to `new_name` at each precise `key_range`.
/// The rename mirror of [`reference_citation_locations`]: `.tex` use sites come from
/// `bib_citers` (a `.bib` origin) or `namespace_members` (a `.tex` origin); the
/// definition sites are the origin bib itself (`.bib` origin) or `bib_definers` (a
/// `.tex` origin). Matching is case-insensitive, as BibTeX folds key case.
pub fn rename_citation_edits(
    snapshot: &Analysis,
    _citations: &ResolvedCitations,
    origin: &Path,
    _kind: FileKind,
    names: &[SmolStr],
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    occurrence_edits(
        snapshot,
        snapshot.citation_occurrences(origin, names),
        new_name,
        enc,
    )
}

/// Every `\name` occurrence across `origin`'s macro namespace rewritten to the bare
/// `new_name` — the rename mirror of the command arm of
/// [`name_reference_locations`]. Each edit rewrites the token's name span *behind*
/// the backslash ([`name_refs::strip_backslash`]), leaving the `\` byte untouched.
/// Definition-site names (`\newcommand{\name}`, `\def\name`) are themselves
/// `CONTROL_WORD` tokens, so the occurrence walk rewrites them too — no separate
/// definition pass. Letter-globbing safety is by construction: the old token only
/// lexed as `\name` because the next byte is a non-letter, and
/// [`is_valid_command_name`] keeps the new name letters-only, so the new control
/// word ends at the same boundary (`\foo bar`, `\foo{x}`, `\foo\bar`, `\foo*` all
/// stay correct).
pub fn rename_command_edits(
    snapshot: &Analysis,
    origin: &Path,
    name: &str,
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let target = NameTarget {
        kind: NameKind::Command,
        name: name.into(),
        span: TextRange::empty(TextSize::new(0)),
    };
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for (file, range) in snapshot.name_references(origin, &target, true) {
        let Some(uri) = path_to_uri(snapshot.file_path(file)) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        push_edit(
            &mut changes,
            &uri,
            &idx,
            name_refs::strip_backslash(range),
            new_name,
        );
    }
    changes
}

/// Every `\begin{name}`/`\end{name}` occurrence across `origin`'s macro namespace,
/// plus every `\newenvironment{name}`-family definition name, rewritten to
/// `new_name` — the rename mirror of the environment arm of
/// [`name_reference_locations`]. Name-based, not pair-based: an unbalanced
/// `\begin` is still renamed. The definition names come from
/// [`name_definition_sites`], since the `\begin`/`\end` walk cannot see them.
pub fn rename_environment_edits(
    snapshot: &Analysis,
    origin: &Path,
    target: &NameTarget,
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for (file, range) in snapshot.name_references(origin, target, true) {
        let Some(uri) = path_to_uri(snapshot.file_path(file)) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        push_edit(&mut changes, &uri, &idx, range, new_name);
    }
    changes
}

/// Convert a semantics-preserving byte edit to the requested position encoding.
pub fn push_edit(
    changes: &mut HashMap<Uri, Vec<TextEdit>>,
    uri: &Uri,
    idx: &LineIndex,
    range: TextRange,
    new_name: &str,
) {
    changes.entry(uri.clone()).or_default().push(TextEdit {
        range: lsp_range(idx, range),
        new_text: new_name.to_owned(),
    });
}

/// Sort and dedup each file's edits, drop empty files, and wrap the rest in a
/// [`WorkspaceEdit`]. `None` when nothing is left to rewrite (so the handler replies
/// `null`).
pub fn finalize_rename(mut changes: HashMap<Uri, Vec<TextEdit>>) -> Option<WorkspaceEdit> {
    changes.retain(|_, edits| {
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        edits.dedup();
        !edits.is_empty()
    });
    (!changes.is_empty()).then(|| WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Whether `new_name` is a safe replacement key: non-empty after trimming and free of
/// characters that would break the surface syntax or the comma key-list split (so an
/// applied rename can never introduce a parse/format error). Conservative — a few
/// exotic-but-legal key characters are rejected rather than risk a corrupt edit.
pub fn is_valid_key(new_name: &str) -> bool {
    !new_name.trim().is_empty()
        && !new_name.chars().any(|c| {
            matches!(
                c,
                '{' | '}' | '%' | '\\' | ',' | '#' | '~' | '$' | '^' | '&' | '\n' | '\r'
            )
        })
}

/// Whether `new_name` is a safe replacement *command* name (leading `\` already
/// stripped): non-empty ASCII letters, plus `@`/`_`/`:` only when `old_name`
/// already used that character. The old token lexing as one `CONTROL_WORD` proves
/// every occurrence site is inside the right letter-mode region (`\makeatletter`
/// for `@`, expl3 for `_`/`:`), so the new name re-lexes identically there — while
/// a plain name never gains `@`, which would mis-lex in ordinary text. Letters-only
/// also preserves the token boundary at every occurrence (a control word ends at
/// the first non-letter, exactly where the old one did).
pub fn is_valid_command_name(new_name: &str, old_name: &str) -> bool {
    !new_name.is_empty()
        && new_name.chars().all(|c| {
            c.is_ascii_alphabetic() || (matches!(c, '@' | '_' | ':') && old_name.contains(c))
        })
}

fn occurrence_locations(
    snapshot: &Analysis,
    occurrences: impl IntoIterator<Item = tex_ls_analysis::navigation::KeyOccurrence>,
    encoding: PositionEncoding,
) -> Vec<Location> {
    dedup_locations(
        occurrences
            .into_iter()
            .map(|item| {
                let file = snapshot.lookup_file(&item.path)?;
                location_for(
                    &item.path,
                    &snapshot.file_line_index(file, encoding),
                    item.key_range,
                )
            })
            .collect(),
    )
}
fn occurrence_edits(
    snapshot: &Analysis,
    occurrences: Vec<tex_ls_analysis::navigation::KeyOccurrence>,
    new_name: &str,
    encoding: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes = HashMap::new();
    for item in occurrences {
        let Some(file) = snapshot.lookup_file(&item.path) else {
            continue;
        };
        let Some(uri) = path_to_uri(&item.path) else {
            continue;
        };
        push_edit(
            &mut changes,
            &uri,
            &snapshot.file_line_index(file, encoding),
            item.key_range,
            new_name,
        );
    }
    changes
}
