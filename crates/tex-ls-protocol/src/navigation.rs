//! Navigation operations.
pub(crate) use crate::convert::locations_for_ranges;
pub use crate::convert::{dedup_locations, location_for};
pub(crate) use crate::workspace_edits::occurrence_edits;
pub use crate::workspace_edits::{finalize_rename, is_valid_command_name, is_valid_key, push_edit};
use crate::{
    bib_strings,
    convert::{
        file_kind_for, lsp_range, lsp_range_with_cursor, lsp_ranges, path_to_uri, uri_to_fs_path,
    },
    glossary,
    workspace_edits::edits_for_locations,
};
use lsp_types::{
    DocumentHighlight, DocumentHighlightKind, Location, Position, Range, TextEdit, Uri,
    WorkspaceEdit,
};
use rowan::{TextRange, TextSize};
use smol_str::SmolStr;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use tex_ls_analysis::{
    incremental::Analysis,
    name_refs::{self, NameKind, NameTarget},
    project::{ResolvedCitations, ResolvedLabels},
    source::FileKind,
    text::{LineIndex, PositionEncoding},
};
use tex_ls_parser::{
    semantic::SemanticModel,
    syntax::{SyntaxKind, SyntaxNode},
};

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
            .or_else(|| bib_entry_under_cursor(snapshot, path, offset).map(|(_, range)| range))
    } else {
        let model = snapshot.semantic_model(origin_file);
        let root = snapshot.parsed_tree(origin_file);
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
                tex_ls_analysis::hover::signature_target_at(&root, offset)
                    .map(|target| target.range)
            })
            .or_else(|| {
                name_refs::navigation_target_under_cursor(
                    &root,
                    offset,
                    snapshot.definition_sites(origin_file),
                )
                .map(|target| target.span)
            })
    }
    .map(|range| lsp_range(&index, range));
    definition_links(
        snapshot,
        definition_locations(snapshot, path, position, enc),
        origin,
        enc,
    )
}

// Keep the byte span through wire conversion instead of decoding each result again.
type DefinitionLocation = (Location, Option<TextRange>);

fn definition_links(
    snapshot: &Analysis,
    locations: Vec<DefinitionLocation>,
    origin: Option<Range>,
    enc: PositionEncoding,
) -> Vec<lsp_types::LocationLink> {
    let mut files: HashMap<&Uri, Vec<usize>> = HashMap::new();
    for (slot, (location, _)) in locations.iter().enumerate() {
        files.entry(&location.uri).or_default().push(slot);
    }
    let mut links = vec![None; locations.len()];
    for (uri, slots) in files {
        let Some(path) = uri_to_fs_path(uri) else {
            continue;
        };
        let containers: Vec<_> = if let Some(file) = snapshot.lookup_file(&path) {
            let index = snapshot.file_line_index(file, enc);
            let mut starts: Vec<_> = slots
                .iter()
                .map(|&slot| {
                    let (location, range) = &locations[slot];
                    let at =
                        range.map(|range| range.start()).unwrap_or_else(|| {
                            TextSize::new(index.offset_at(
                                location.range.start.line,
                                location.range.start.character,
                            ) as u32)
                        });
                    (slot, at)
                })
                .collect();
            starts.sort_unstable_by_key(|(_, at)| *at);
            let ranges = if file_kind_for(&path) == FileKind::Bib {
                use tex_ls_parser::bib::syntax::SyntaxKind as Kind;
                let root = snapshot.parsed_bib_tree(file);
                starts
                    .iter()
                    .map(|(_, at)| {
                        tex_ls_parser::syntax::token_at_offset(&root, *at)
                            .right_biased()
                            .and_then(|token| {
                                token.parent_ancestors().find(|node| {
                                    matches!(node.kind(), Kind::ENTRY | Kind::STRING_ENTRY)
                                })
                            })
                            .map(|node| node.text_range())
                    })
                    .collect()
            } else {
                let model = snapshot.semantic_model(file);
                let mut facts = model
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
                    );
                if starts.len() == 1 {
                    vec![
                        facts
                            .find(|(range, _)| range.contains_inclusive(starts[0].1))
                            .map(|(_, container)| container),
                    ]
                } else {
                    definition_containers(
                        &facts.collect::<Vec<_>>(),
                        &starts.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
                    )
                }
            };
            let mut cursor = index.position_cursor();
            starts
                .into_iter()
                .zip(ranges)
                .map(|((slot, _), range)| {
                    (
                        slot,
                        range.map(|range| lsp_range_with_cursor(&mut cursor, range)),
                    )
                })
                .collect()
        } else {
            slots.into_iter().map(|slot| (slot, None)).collect()
        };
        for (slot, container) in containers {
            let location = &locations[slot].0;
            links[slot] = Some(lsp_types::LocationLink {
                origin_selection_range: origin,
                target_uri: location.uri.clone(),
                target_range: container.unwrap_or(location.range),
                target_selection_range: location.range,
            });
        }
    }
    links.into_iter().flatten().collect()
}

/// Match the original fact precedence, including inclusive endpoints and overlaps.
fn definition_containers(
    facts: &[(TextRange, TextRange)],
    starts: &[TextSize],
) -> Vec<Option<TextRange>> {
    if starts.len() <= 1 {
        return starts
            .iter()
            .map(|&at| {
                facts
                    .iter()
                    .find(|(range, _)| range.contains_inclusive(at))
                    .map(|(_, container)| *container)
            })
            .collect();
    }
    let mut events: Vec<_> = facts.iter().enumerate().collect();
    events.sort_unstable_by_key(|(_, (range, _))| range.start());
    let mut next = 0;
    let mut active = std::collections::BTreeMap::new();
    let mut ends = std::collections::BinaryHeap::new();
    starts
        .iter()
        .map(|&at| {
            while let Some(&(priority, (range, container))) = events
                .get(next)
                .filter(|(_, (range, _))| range.start() <= at)
            {
                active.insert(priority, *container);
                ends.push(std::cmp::Reverse((range.end(), priority)));
                next += 1;
            }
            while let Some(&std::cmp::Reverse((_, priority))) =
                ends.peek().filter(|std::cmp::Reverse((end, _))| *end < at)
            {
                ends.pop();
                active.remove(&priority);
            }
            active.first_key_value().map(|(_, range)| *range)
        })
        .collect()
}

fn definition_range_locations<'a>(
    snapshot: &Analysis,
    ranges: impl IntoIterator<Item = (&'a Path, TextRange)>,
    enc: PositionEncoding,
) -> Vec<DefinitionLocation> {
    let ranges: Vec<_> = ranges.into_iter().collect();
    locations_for_ranges(snapshot, ranges.iter().copied(), enc)
        .into_iter()
        .zip(ranges)
        .filter_map(|(location, (_, range))| Some((location?, Some(range))))
        .collect()
}

fn definition_occurrence_locations(
    snapshot: &Analysis,
    sites: Vec<tex_ls_analysis::navigation::KeyOccurrence>,
    enc: PositionEncoding,
) -> Vec<DefinitionLocation> {
    let mut seen = HashSet::new();
    definition_range_locations(
        snapshot,
        sites
            .iter()
            .filter(|site| site.definition)
            .map(|site| (site.path.as_path(), site.key_range)),
        enc,
    )
    .into_iter()
    .filter(|(location, _)| {
        seen.insert((
            location.uri.as_str().to_owned(),
            location.range.start,
            location.range.end,
        ))
    })
    .collect()
}

/// Resolve the construct at `position` using the snapshot's source and project namespaces.
fn definition_locations(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    enc: PositionEncoding,
) -> Vec<DefinitionLocation> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib {
        let model = snapshot.bib_semantic_model(file);
        if let Some((key, _)) = bib_entry_under_cursor(snapshot, path, offset) {
            return definition_occurrence_locations(
                snapshot,
                snapshot.citation_occurrences(path, &[key]),
                enc,
            );
        }
        return bib_strings::target(model, offset)
            .map(|(key, _)| {
                definition_range_locations(
                    snapshot,
                    snapshot
                        .bibliography_string_occurrences(path, &key)
                        .iter()
                        .filter(|site| site.definition)
                        .map(|site| (site.path.as_path(), site.key_range)),
                    enc,
                )
            })
            .unwrap_or_default();
    }

    if let Some((key, _)) = glossary::target(snapshot.semantic_model(file), offset) {
        let sites = glossary::occurrences(snapshot, path, &key);
        return definition_range_locations(
            snapshot,
            sites
                .iter()
                .filter(|(_, _, definition)| *definition)
                .map(|(path, range, _)| (path.as_path(), *range)),
            enc,
        );
    }

    // Resolve the reference under the cursor. Keep the tree for name and file targets.
    let (root, target, lint_path) = (
        snapshot.parsed_tree(file),
        reference_under_cursor(snapshot.semantic_model(file), offset),
        snapshot.file_path(file).to_path_buf(),
    );
    if let Some(target) = target {
        return match target {
            CursorTarget::Labels(names) => definition_occurrence_locations(
                snapshot,
                snapshot.label_occurrences(&lint_path, &names),
                enc,
            ),
            CursorTarget::Citations(names) => definition_occurrence_locations(
                snapshot,
                snapshot.citation_occurrences(&lint_path, &names),
                enc,
            ),
        };
    }
    // A command/environment name jumps to static definitions in the project,
    // loaded packages, or the preloaded format. An unresolved name falls through
    // to literal file arguments below. Rename uses a separate project-only scope.
    let sites = snapshot.definition_sites(file);
    if let Some(target) = name_refs::navigation_target_under_cursor(&root, offset, sites) {
        let defs = snapshot.navigation_definitions(&lint_path, &target);
        if !defs.is_empty() {
            return definition_range_locations(
                snapshot,
                defs.into_iter()
                    .map(|(file, range)| (snapshot.file_path(file), range)),
                enc,
            );
        }
        if target.kind == NameKind::Environment
            && let Some(pair) = matching_environment_delimiter(&root, offset)
        {
            return definition_range_locations(snapshot, [(path, pair.match_range)], enc);
        }
    }
    // Fall back to a file-referencing argument
    // (include/package/class/bib/graphics) under the cursor, jumping to the
    // resolved on-disk target — the same resolution the document-link path uses,
    // so a system package resolves through the TEXMF index too.
    file_target_under_cursor(snapshot, file, offset)
        .into_iter()
        .map(|location| (location, None))
        .collect()
}

/// The go-to-definition file target under `offset`: the document link whose argument
/// span covers the cursor, mapped to a whole-file [`Location`]. Reuses
/// [`document_link::document_links`](crate::document_link::document_links) (disk-aware and TEXMF-aware), so every command it
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
        .filter(|link| link.range.contains_inclusive(at))
        .filter_map(|link| path_to_uri(&link.target))
        .map(|uri| Location {
            uri,
            range: Range::default(),
        })
        .collect()
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
        let sites = snapshot.bibliography_string_occurrences(path, &key);
        return locations_for_ranges(
            snapshot,
            sites
                .iter()
                .filter(|site| include_declaration || !site.definition)
                .map(|site| (site.path.as_path(), site.key_range)),
            enc,
        )
        .into_iter()
        .flatten()
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
    dedup_locations(locations_for_ranges(
        snapshot,
        snapshot
            .name_references(origin, target, include_declaration)
            .into_iter()
            .map(|(file, range)| (snapshot.file_path(file), range)),
        enc,
    ))
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
    let (left, right) = match tex_ls_parser::syntax::token_at_offset(root, at) {
        rowan::TokenAtOffset::None => return Vec::new(),
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };
    let delimiter = [left, right]
        .into_iter()
        .flatten()
        .filter(|token| {
            !token
                .parent_ancestors()
                .any(|node| node.kind() == SyntaxKind::TOKEN_LIST)
        })
        .find_map(|token| {
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

/// A literal begin/end pair at the cursor. Only the name itself is a target;
/// matching a delimiter command or brace would make Ctrl-hover claim a link that
/// the environment-name hover does not cover. The parser supplies the enclosing
/// environment, so this inspects two delimiters without walking the document.
pub(crate) struct MatchingEnvironmentDelimiter {
    pub match_range: TextRange,
    pub origin_is_begin: bool,
}

pub(crate) fn matching_environment_delimiter(
    root: &SyntaxNode,
    offset: usize,
) -> Option<MatchingEnvironmentDelimiter> {
    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let delimiter = tex_ls_parser::syntax::token_at_offset(root, at)
        .filter(|token| {
            !token
                .parent_ancestors()
                .any(|node| node.kind() == SyntaxKind::TOKEN_LIST)
        })
        .find_map(|token| {
            token
                .parent_ancestors()
                .find(|node| matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
                .filter(|node| {
                    tex_ls_parser::ast::environment_name_range(node)
                        .is_some_and(|range| range.contains(at))
                })
        })?;
    let environment = delimiter
        .parent()
        .filter(|node| node.kind() == SyntaxKind::ENVIRONMENT)?;
    let begin = environment
        .children()
        .find(|node| node.kind() == SyntaxKind::BEGIN)?;
    let end = environment
        .children()
        .find(|node| node.kind() == SyntaxKind::END)?;
    let name = tex_ls_parser::ast::environment_name(&begin)?;
    if name != tex_ls_parser::ast::environment_name(&end)? {
        return None;
    }
    let origin_is_begin = delimiter.kind() == SyntaxKind::BEGIN;
    let other = if origin_is_begin { &end } else { &begin };
    Some(MatchingEnvironmentDelimiter {
        match_range: tex_ls_parser::ast::environment_name_range(other)?,
        origin_is_begin,
    })
}

/// A proved local pair for editing. Body positions are accepted for code actions;
/// linked editing separately requires the cursor on a name.
pub fn proved_environment_pair(
    root: &SyntaxNode,
    offset: usize,
) -> Option<(String, Vec<TextRange>)> {
    use tex_ls_parser::ast::AstNode;

    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let tokens = tex_ls_parser::syntax::token_candidates_at_offset(root, at)?;
    let env = tokens
        .into_iter()
        .filter(|token| {
            !token
                .parent_ancestors()
                .any(|node| node.kind() == SyntaxKind::TOKEN_LIST)
        })
        .find_map(|token| {
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
/// Glossary keys and BibTeX string names highlight their definitions and uses.
/// An ordinary bibliography entry key has no in-file uses and yields no highlights.
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
        return document_highlights(
            &idx,
            bib_strings::occurrences(snapshot.bib_semantic_model(file), &key)
                .into_iter()
                .map(|(range, definition)| {
                    (
                        range,
                        if definition {
                            DocumentHighlightKind::Write
                        } else {
                            DocumentHighlightKind::Read
                        },
                    )
                })
                .collect(),
        );
    }

    if file_kind_for(path) != FileKind::Bib
        && let Some((key, _)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        return document_highlights(
            &idx,
            glossary::occurrences(snapshot, path, &key)
                .into_iter()
                .filter(|(source, _, _)| source == path)
                .map(|(_, range, definition)| {
                    (
                        range,
                        if definition {
                            DocumentHighlightKind::Write
                        } else {
                            DocumentHighlightKind::Read
                        },
                    )
                })
                .collect(),
        );
    }

    if file_kind_for(path) == FileKind::Bib {
        let Some((key, _)) = bib_entry_under_cursor(snapshot, path, offset) else {
            return Vec::new();
        };
        return document_highlights(
            &idx,
            snapshot
                .citation_occurrences(path, &[key])
                .into_iter()
                .filter(|site| site.path == path)
                .map(|site| {
                    (
                        site.key_range,
                        if site.definition {
                            DocumentHighlightKind::Write
                        } else {
                            DocumentHighlightKind::Read
                        },
                    )
                })
                .collect(),
        );
    }
    let highlight = |range: TextRange, kind: DocumentHighlightKind| (range, kind);
    let collect =
        |root: &SyntaxNode, model: &SemanticModel| -> Vec<(TextRange, DocumentHighlightKind)> {
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
                let definition_ranges: HashSet<_> = snapshot
                    .definition_sites(file)
                    .iter()
                    .map(|site| site.name_range)
                    .collect();
                return name_refs::command_occurrences(root, &target.name)
                    .into_iter()
                    .map(|range| {
                        let definition = definition_ranges.contains(&range);
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
    document_highlights(
        &idx,
        collect(&snapshot.parsed_tree(file), snapshot.semantic_model(file)),
    )
}

fn document_highlights(
    idx: &LineIndex,
    sites: Vec<(TextRange, DocumentHighlightKind)>,
) -> Vec<DocumentHighlight> {
    let ranges: Vec<_> = sites.iter().map(|(range, _)| *range).collect();
    sites
        .into_iter()
        .zip(lsp_ranges(idx, &ranges))
        .map(|((_, kind), range)| DocumentHighlight {
            range,
            kind: Some(kind),
        })
        .collect()
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
            if !snapshot.citation_dependencies_editable(path) {
                return false;
            }
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
                !names.iter().any(|old| citations.keys_equal(path, old, new))
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
    closed
        && !collision
        && occurrences.iter().filter(|site| site.definition).count() == 1
        && replacement.is_none_or(|new| {
            occurrences.iter().all(|site| {
                snapshot
                    .key_replacement(&site.path, site.key_range, new)
                    .is_some()
            })
        })
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
        return snapshot
            .bibliography_string_rename_allowed(path, &key)
            .then(|| (lsp_range(&idx, range), key));
    }

    if file_kind_for(path) != FileKind::Bib
        && let Some((key, range)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        return glossary::rename_allowed(snapshot, path, &key).then(|| {
            (
                lsp_range(&idx, range),
                snapshot.file_text(file)[range].to_string(),
            )
        });
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
        return key_rename_allowed(snapshot, path, &target.target, None).then(|| {
            (
                lsp_range(&idx, target.span),
                snapshot.file_text(file)[target.span].to_string(),
            )
        });
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

/// The user-defined rename gate requires an editable definition in a unique,
/// closed macro namespace. Built-in and CWL names alone do not qualify; the
/// definition lookup also retains generated-name and shared-range ambiguity
/// checks.
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
    let _ = own_sites;
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
        && let Some((key, range)) = glossary::target(snapshot.semantic_model(file), offset)
    {
        let canonical = snapshot.canonical_key_at(path, range, new_name)?;
        return glossary::rename(snapshot, path, &key, &canonical, enc);
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
            let Some(canonical) = snapshot.canonical_key_at(path, target.span, new_name) else {
                return HashMap::new();
            };
            let new_name = canonical.as_str();
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
                        && (snapshot.editor_symbols(file).commands.contains(bare)
                            || tex_ls_analysis::hover::lookup_command(
                                snapshot.editor_signatures(file),
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
                        && (snapshot
                            .editor_symbols(file)
                            .environments
                            .contains(new_name)
                            || tex_ls_analysis::hover::lookup_environment(
                                snapshot.editor_signatures(file),
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
        .or_else(|| {
            snapshot
                .bib_semantic_model(file)
                .entries()
                .iter()
                .flat_map(|entry| &entry.dependencies)
                .find(|dependency| {
                    dependency.field != "ids"
                        && snapshot
                            .resolve_citations()
                            .supports_dependency(path, &dependency.field)
                        && dependency.range.contains_inclusive(at)
                })
                .and_then(|dependency| dependency.key.clone().map(|key| (key, dependency.range)))
        })
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
    edits_for_locations(
        locations_for_ranges(
            snapshot,
            snapshot
                .name_references(origin, &target, true)
                .into_iter()
                .map(|(file, range)| (snapshot.file_path(file), name_refs::strip_backslash(range))),
            enc,
        )
        .into_iter()
        .flatten(),
        new_name,
    )
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
    edits_for_locations(
        locations_for_ranges(
            snapshot,
            snapshot
                .name_references(origin, target, true)
                .into_iter()
                .map(|(file, range)| (snapshot.file_path(file), range)),
            enc,
        )
        .into_iter()
        .flatten(),
        new_name,
    )
}

fn occurrence_locations(
    snapshot: &Analysis,
    occurrences: impl IntoIterator<Item = tex_ls_analysis::navigation::KeyOccurrence>,
    encoding: PositionEncoding,
) -> Vec<Location> {
    let occurrences: Vec<_> = occurrences.into_iter().collect();
    dedup_locations(locations_for_ranges(
        snapshot,
        occurrences
            .iter()
            .map(|item| (item.path.as_path(), item.key_range)),
        encoding,
    ))
}
#[cfg(test)]
mod batch_definition_tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn container_sweep_matches_first_match_precedence_and_inclusive_endpoints() {
        let range = |start, end| TextRange::new(TextSize::new(start), TextSize::new(end));
        let choices = [
            range(0, 0),
            range(0, 4),
            range(2, 3),
            range(3, 8),
            range(8, 8),
        ];
        let starts: Vec<_> = [0, 0, 1, 2, 3, 4, 5, 8, 8, 9]
            .into_iter()
            .map(TextSize::new)
            .collect();
        for first in choices {
            for second in choices {
                for third in choices {
                    let facts = [
                        (first, range(10, 11)),
                        (second, range(12, 13)),
                        (third, range(14, 15)),
                    ];
                    for starts in [&starts[..], &starts[..1], &starts[..0]] {
                        let expected: Vec<_> = starts
                            .iter()
                            .map(|&at| {
                                facts
                                    .iter()
                                    .find(|(range, _)| range.contains_inclusive(at))
                                    .map(|(_, container)| *container)
                            })
                            .collect();
                        assert_eq!(
                            definition_containers(&facts, starts),
                            expected,
                            "{facts:?}, {starts:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn definition_batches_match_independent_uri_position_and_container_resolution() {
        let main = PathBuf::from(fixture_path!("/project/main.tex"));
        let child = main.with_file_name("child.tex");
        let bib = main.with_file_name("refs.bib");
        let missing = main.with_file_name("missing.tex");
        let mut db = IncrementalDatabase::default();
        db.apply_change(
            &main,
            "é😀 \\newcommand{\\demo}{Example} \\label{demo}"
                .repeat(32)
                .as_str(),
            None,
        );
        db.apply_change(
            &child,
            "é😀 \\label{demo} \\newglossaryentry{term}{name=Term,description=Term}"
                .repeat(32)
                .as_str(),
            None,
        );
        db.apply_change(
            &bib,
            "@string{brand={é😀 Example}}".repeat(32).as_str(),
            None,
        );
        let snapshot = db.snapshot();
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let mut locations = Vec::new();
            for path in [&main, &child, &bib] {
                let file = snapshot.lookup_file(path).unwrap();
                let index = snapshot.file_line_index(file, encoding);
                let ranges: Vec<_> = if path == &bib {
                    snapshot
                        .bib_semantic_model(file)
                        .string_defs()
                        .iter()
                        .map(|site| site.range)
                        .collect()
                } else {
                    let model = snapshot.semantic_model(file);
                    model
                        .labels()
                        .iter()
                        .map(|site| site.key_range)
                        .chain(model.glossary_defs().iter().map(|site| site.key_range))
                        .chain(
                            snapshot
                                .definition_sites(file)
                                .iter()
                                .map(|site| site.name_range),
                        )
                        .collect()
                };
                locations.extend(
                    ranges
                        .into_iter()
                        .map(|range| (location_for(path, &index, range).unwrap(), Some(range))),
                );
                locations.push((
                    Location {
                        uri: path_to_uri(path).unwrap(),
                        range: Range::default(),
                    },
                    None,
                ));
            }
            locations.reverse();
            locations.push(locations[3].clone());
            locations.push((
                Location {
                    uri: path_to_uri(&missing).unwrap(),
                    range: Range::default(),
                },
                None,
            ));
            locations.push((
                Location {
                    uri: "untitled:buffer".parse().unwrap(),
                    range: Range::default(),
                },
                None,
            ));
            let origin = Some(Range::new(Position::new(0, 2), Position::new(0, 6)));
            let expected: Vec<_> = locations
                .iter()
                .filter_map(|(location, _)| {
                    let path = uri_to_fs_path(&location.uri)?;
                    let container = snapshot
                        .lookup_file(&path)
                        .and_then(|file| {
                            let index = snapshot.file_line_index(file, encoding);
                            let at = TextSize::new(index.offset_at(
                                location.range.start.line,
                                location.range.start.character,
                            ) as u32);
                            let container = if file_kind_for(&path) == FileKind::Bib {
                                use tex_ls_parser::bib::syntax::SyntaxKind as Kind;
                                snapshot
                                    .parsed_bib_tree(file)
                                    .token_at_offset(at)
                                    .right_biased()?
                                    .parent_ancestors()
                                    .find(|node| {
                                        matches!(node.kind(), Kind::ENTRY | Kind::STRING_ENTRY)
                                    })
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
                                    .find(|(range, _)| range.contains_inclusive(at))
                                    .map(|(_, range)| range)
                            }?;
                            Some(lsp_range(&index, container))
                        })
                        .unwrap_or(location.range);
                    Some(lsp_types::LocationLink {
                        origin_selection_range: origin,
                        target_uri: location.uri.clone(),
                        target_range: container,
                        target_selection_range: location.range,
                    })
                })
                .collect();
            assert_eq!(
                definition_links(&snapshot, locations, origin, encoding),
                expected
            );
        }
    }
}

#[cfg(test)]
mod indexed_bibliography_cursor_tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn wide_bibliography_definition_batches_preserve_container_ranges_and_order() {
        for newline in ["\n", "\r\n"] {
            let mut source = String::new();
            let mut entries = Vec::new();
            for item in 0..1024 {
                let key = format!("key{item}");
                let entry = format!("@article{{{key},title={{é😀 Research}}}}");
                let start = source.len();
                let key_start = start + "@article{".len();
                entries.push((start..start + entry.len(), key_start..key_start + key.len()));
                source.push_str(&entry);
                source.push_str(newline);
            }
            let path = PathBuf::from(fixture_path!("/indexed/batch.bib"));
            let mut db = IncrementalDatabase::default();
            db.apply_change(&path, source.as_str(), None);
            let snapshot = db.snapshot();
            let file = snapshot.lookup_file(&path).unwrap();
            assert!(
                snapshot
                    .parsed_bib_tree(file)
                    .children_with_tokens()
                    .count()
                    >= 1024
            );
            for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                let index = snapshot.file_line_index(file, encoding);
                let uri = path_to_uri(&path).unwrap();
                let range = |bytes: &std::ops::Range<usize>| {
                    TextRange::new(
                        TextSize::from(bytes.start as u32),
                        TextSize::from(bytes.end as u32),
                    )
                };
                let order = (960..1024)
                    .rev()
                    .chain([0, 1023, 17, 1023])
                    .collect::<Vec<_>>();
                let locations = order
                    .iter()
                    .map(|&item| {
                        let (_, key) = &entries[item];
                        let key = range(key);
                        (
                            Location {
                                uri: uri.clone(),
                                range: lsp_range(&index, key),
                            },
                            Some(key),
                        )
                    })
                    .collect();
                let origin = Some(Range::new(Position::new(0, 1), Position::new(0, 2)));
                let links = definition_links(&snapshot, locations, origin, encoding);
                assert_eq!(links.len(), order.len());
                for (link, item) in links.iter().zip(order) {
                    let (entry, key) = &entries[item];
                    assert_eq!(link.target_uri, uri);
                    assert_eq!(link.origin_selection_range, origin);
                    assert_eq!(link.target_selection_range, lsp_range(&index, range(key)));
                    assert_eq!(link.target_range, lsp_range(&index, range(entry)));
                }
            }
        }
    }
}
