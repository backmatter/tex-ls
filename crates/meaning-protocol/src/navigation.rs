//! Navigation operations.
use super::*;

/// Find the environment name ranges in the captured source.
pub fn compute_change_environment(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> Option<(String, Vec<TextRange>)> {
    if file_kind_for(path) == FileKind::Bib {
        return None;
    }
    let file = snapshot.lookup_file(path)?;
    let offset = snapshot
        .file_line_index(file, enc)
        .offset_at(position.line, position.character);
    environment_change_target(&snapshot.parsed_tree(file), offset)
}

/// What the cursor points at inside a `.tex` buffer: the keys whose command range
/// covers the offset. Refs and citations are kept distinct so each resolves against
/// its own namespace (labels vs. bibliography). A multi-key list command
/// (`\cref{a,b}`, `\cite{a,b}`) shares one range, so every key at that offset is
/// returned and resolved — per-key sub-ranges are deferred (see
/// [`meaning_parser::semantic::label::LabelRef::range`]).
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
    target: CursorTarget,
    span: TextRange,
    placeholder: SmolStr,
}

/// Resolve the construct at `position` using the snapshot's source and project namespaces.
pub fn compute_goto_definition(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    texmf: &dyn HostServices,
    enc: PositionEncoding,
) -> Vec<Location> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);
    let base_dir = path.parent();

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
    let sites = scan_definition_sites(&root);
    if let Some(target) = name_refs::name_target_under_cursor(&root, offset, &sites) {
        let resolution = snapshot.resolve_labels();
        let packages = snapshot.package_graph();
        let defs = name_definition_sites(snapshot, resolution, packages, &lint_path, &target);
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
    file_target_under_cursor(&root, base_dir, offset, texmf)
}

/// The go-to-definition file target under `offset`: the document link whose argument
/// span covers the cursor, mapped to a whole-file [`Location`]. Reuses
/// [`document_link::document_links`] (disk-aware and TEXMF-aware), so every command it
/// resolves—`\input`, `\usepackage`, `\includegraphics`, …—becomes navigable. Empty
/// when the cursor is not on a resolvable file argument.
pub fn file_target_under_cursor(
    root: &SyntaxNode,
    base_dir: Option<&Path>,
    offset: usize,
    texmf: &dyn HostServices,
) -> Vec<Location> {
    let at = TextSize::new(offset as u32);
    let index = texmf;
    document_link::document_links(root, base_dir, index)
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

/// The cite/ref keys whose command range covers `offset`, refs taking precedence
/// (a position is never both). Returns owned keys so the borrowed model can drop.
pub fn reference_under_cursor(model: &SemanticModel, offset: usize) -> Option<CursorTarget> {
    let at = TextSize::new(offset as u32);
    let label_names: Vec<SmolStr> = model
        .refs()
        .iter()
        .filter(|r| r.range.contains_inclusive(at))
        .map(|r| r.name.clone())
        .collect();
    if !label_names.is_empty() {
        return Some(CursorTarget::Labels(label_names));
    }
    let cite_names: Vec<SmolStr> = model
        .citations()
        .iter()
        .filter(|c| c.range.contains_inclusive(at))
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
    let sites = scan_definition_sites(&root);
    let Some(target) = name_refs::name_target_under_cursor(&root, offset, &sites) else {
        return Vec::new();
    };
    let packages = snapshot.package_graph();
    name_reference_locations(
        snapshot,
        resolution,
        packages,
        &origin,
        &target,
        include_declaration,
        enc,
    )
}

/// Every occurrence of a command/environment name across `origin`'s macro
/// namespace — the name-based tier of [`compute_references`]. Command occurrences
/// are full `\name` token ranges (definition-site names included: the `\mycmd` in
/// `\newcommand{\mycmd}` is itself a `CONTROL_WORD`, filtered out by range equality
/// against its [`DefSite`] unless `include_declaration`). Environment occurrences
/// are `\begin`/`\end` name spans; their `\newenvironment{name}` definition names
/// are invisible to that walk, so `include_declaration` *adds* them.
///
/// [`DefSite`]: meaning_parser::semantic::DefSite
pub fn name_reference_locations(
    snapshot: &Analysis,
    resolution: &ResolvedLabels,
    packages: &PackageGraph,
    origin: &Path,
    target: &NameTarget,
    include_declaration: bool,
    enc: PositionEncoding,
) -> Vec<Location> {
    let mut locations = Vec::new();
    for member in name_refs::macro_namespace(resolution, packages, origin) {
        let Some(file) = snapshot.lookup_file(&member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        let root = snapshot.parsed_tree(file);
        let sites = scan_definition_sites(&root);
        match target.kind {
            NameKind::Command => {
                let def_ranges: HashSet<TextRange> = sites
                    .iter()
                    .filter(|s| s.kind == DefSiteKind::Command && s.name == target.name)
                    .map(|s| s.name_range)
                    .collect();
                for range in name_refs::command_occurrences(&root, &target.name) {
                    if include_declaration || !def_ranges.contains(&range) {
                        locations.push(location_for(&member, &idx, range));
                    }
                }
            }
            NameKind::Environment => {
                for range in name_refs::environment_occurrences(&root, &target.name) {
                    locations.push(location_for(&member, &idx, range));
                }
                if include_declaration {
                    for site in sites
                        .iter()
                        .filter(|s| s.kind == DefSiteKind::Environment && s.name == target.name)
                    {
                        locations.push(location_for(&member, &idx, site.name_range));
                    }
                }
            }
        }
    }
    dedup_locations(locations)
}

/// The matching [`DefSite`]s of `target` across `origin`'s macro namespace, as
/// `(file, name span)` pairs in member order. Serves three consumers: the
/// user-defined rename gate (non-empty = defined in the project), goto-definition
/// (each pair becomes a [`Location`]), and environment rename (each name span is
/// rewritten alongside the `\begin`/`\end` occurrences).
///
/// [`DefSite`]: meaning_parser::semantic::DefSite
pub fn name_definition_sites(
    snapshot: &Analysis,
    resolution: &ResolvedLabels,
    packages: &PackageGraph,
    origin: &Path,
    target: &NameTarget,
) -> Vec<(PathBuf, TextRange)> {
    let want = match target.kind {
        NameKind::Command => DefSiteKind::Command,
        NameKind::Environment => DefSiteKind::Environment,
    };
    let mut out = Vec::new();
    for member in name_refs::macro_namespace(resolution, packages, origin) {
        let Some(file) = snapshot.lookup_file(&member) else {
            continue;
        };
        let root = snapshot.parsed_tree(file);
        for site in scan_definition_sites(&root) {
            if site.kind == want && site.name == target.name {
                out.push((member.clone(), site.name_range));
            }
        }
    }
    out
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
    let label_names: Vec<SmolStr> = model
        .labels()
        .iter()
        .filter(|l| l.range.contains_inclusive(at))
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
            .filter_map(|c| meaning_parser::ast::environment_name_range(&c))
            .collect(),
        // A stray `\end` with no open environment: highlight it alone.
        _ => meaning_parser::ast::environment_name_range(&delimiter)
            .into_iter()
            .collect(),
    }
}

/// The change-environment target at byte `offset`: the *innermost* `ENVIRONMENT`
/// node containing the cursor (anywhere in the body or on either delimiter — the
/// refactor names the environment "around the cursor", unlike
/// [`environment_pair_ranges`]'s delimiter-only gate), as its current begin name
/// plus the name spans to rewrite. The parser's structural pairing is
/// authoritative: an unclosed environment rewrites just its `\begin` name, and a
/// mismatched-but-paired `\end` is rewritten too (making the pair consistent).
/// Correctness-only (tenet #1): when any paired delimiter's name is not a plain
/// token run (so a textual rewrite could corrupt it), decline the whole edit
/// rather than rewrite half a pair.
pub fn environment_change_target(
    root: &SyntaxNode,
    offset: usize,
) -> Option<(String, Vec<TextRange>)> {
    use meaning_parser::ast::AstNode;

    let at = TextSize::new(offset.min(u32::MAX as usize) as u32);
    let (left, right) = match root.token_at_offset(at) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(t) => (Some(t.clone()), Some(t)),
        rowan::TokenAtOffset::Between(l, r) => (Some(l), Some(r)),
    };
    let env = [left, right].into_iter().flatten().find_map(|token| {
        token
            .parent_ancestors()
            .find_map(meaning_parser::ast::Environment::cast)
    })?;
    let begin = env.begin()?;
    let old_name = begin.name()?;
    let ranges = env
        .syntax()
        .children()
        .filter(|child| matches!(child.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
        .map(|child| meaning_parser::ast::environment_name_range(&child))
        .collect::<Option<Vec<_>>>()?;
    (!ranges.is_empty()).then_some((old_name, ranges))
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
                        .collect()
                }
            };
        }
        // Otherwise, a `\begin`/`\end` delimiter pair.
        environment_pair_ranges(root, offset)
            .into_iter()
            .map(|range| highlight(range, DocumentHighlightKind::Text))
            .collect()
    };
    collect(&snapshot.parsed_tree(file), snapshot.semantic_model(file))
}

pub fn compute_prepare_rename(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> Option<(Range, String)> {
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);

    // `.bib` origin: the `@entry` key under the cursor.
    if file_kind_for(path) == FileKind::Bib {
        let (key, key_range) = bib_entry_under_cursor(snapshot, path, offset)?;
        return Some((lsp_range(&idx, key_range), key.to_string()));
    }
    // `.tex` origin: a `\ref`/`\cite` use or a `\label` definition. The parsed
    // `root` is kept for the command/environment name fallback below.
    let (root, target) = (
        snapshot.parsed_tree(file),
        rename_target_under_cursor(snapshot.semantic_model(file), offset),
    );
    if let Some(target) = target {
        return Some((lsp_range(&idx, target.span), target.placeholder.to_string()));
    }
    // Not a key: a command or environment name, gated to user-defined names
    // (a project definition site must exist — renaming `\textbf` or
    // `verbatim` over a partial namespace view is a footgun).
    let sites = scan_definition_sites(&root);
    let target = name_refs::name_target_under_cursor(&root, offset, &sites)?;
    if !name_rename_allowed(snapshot, path, &sites, &target) {
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
    own_sites: &[meaning_parser::semantic::DefSite],
    target: &NameTarget,
) -> bool {
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
    let resolution = snapshot.resolve_labels();
    let packages = snapshot.package_graph();
    !name_definition_sites(snapshot, resolution, packages, origin, target).is_empty()
}

/// Compute the [`WorkspaceEdit`] renaming the key — or, in the name-based fallback
/// tier, the user-defined command/environment name — under the cursor to `new_name`
/// across its namespace — the write mirror of [`compute_references`]. Rewrites only
/// the per-key `key_range` of each occurrence (so a sibling key in `\cref{a,b}` is
/// untouched), always including the definition. Best-effort: every occurrence in the
/// *visible* namespace is rewritten (an unresolved/dynamic `\input` may hide a use we
/// cannot see). `None` when `new_name` is not syntactically safe for the target
/// ([`is_valid_key`], or [`is_valid_command_name`] for a command), or nothing
/// resolves.
pub fn compute_rename(
    snapshot: &Analysis,
    path: &Path,
    position: Position,
    new_name: &str,
    enc: PositionEncoding,
) -> Option<WorkspaceEdit> {
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let offset = idx.offset_at(position.line, position.character);

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
            if !is_valid_key(new_name) {
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
        let sites = scan_definition_sites(&root);
        let Some(target) = name_refs::name_target_under_cursor(&root, offset, &sites) else {
            return HashMap::new();
        };
        if !name_rename_allowed(snapshot, path, &sites, &target) {
            return HashMap::new();
        }
        let packages = snapshot.package_graph();
        match target.kind {
            NameKind::Command => {
                // The placeholder is the bare name, but a typed `\newname` is
                // accepted too — strip one leading backslash so both agree.
                let bare = new_name.strip_prefix('\\').unwrap_or(new_name);
                if !is_valid_command_name(bare, &target.name) {
                    return HashMap::new();
                }
                rename_command_edits(
                    snapshot,
                    resolution,
                    packages,
                    &origin,
                    &target.name,
                    bare,
                    enc,
                )
            }
            NameKind::Environment => {
                if !is_valid_key(new_name) {
                    return HashMap::new();
                }
                rename_environment_edits(
                    snapshot, resolution, packages, &origin, &target, new_name, enc,
                )
            }
        }
    })();
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
    resolution: &ResolvedLabels,
    origin: &Path,
    names: &[SmolStr],
    include_declaration: bool,
    enc: PositionEncoding,
) -> Vec<Location> {
    let mut locations = Vec::new();
    for member in resolution.namespace_members(origin) {
        let Some(file) = snapshot.lookup_file(member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        let model = snapshot.semantic_model(file);
        for r in model.refs() {
            if names.contains(&r.name) {
                locations.push(location_for(member, &idx, r.range));
            }
        }
        if include_declaration {
            for label in model.labels() {
                if names.contains(&label.name) {
                    locations.push(location_for(member, &idx, label.range));
                }
            }
        }
    }
    dedup_locations(locations)
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
    citations: &ResolvedCitations,
    origin: &Path,
    kind: FileKind,
    names: &[SmolStr],
    include_declaration: bool,
    decl_for_bib: Option<Location>,
    enc: PositionEncoding,
) -> Vec<Location> {
    let members = if kind == FileKind::Bib {
        citations.bib_citers(origin)
    } else {
        citations.namespace_members(origin)
    };
    let mut locations = Vec::new();
    for member in members {
        let Some(file) = snapshot.lookup_file(member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        for c in snapshot.semantic_model(file).citations() {
            if names.iter().any(|n| n.eq_ignore_ascii_case(&c.name)) {
                locations.push(location_for(member, &idx, c.range));
            }
        }
    }
    let mut locations = dedup_locations(locations);
    if include_declaration {
        match kind {
            FileKind::Bib => locations.extend(decl_for_bib),
            _ => locations.extend(resolve_citation_locations(
                snapshot, citations, origin, names, enc,
            )),
        }
    }
    locations
}

/// For each `\ref` key, the `\label{key}` definition sites across the file's
/// namespace: `resolution.definers` gives the defining files, each file's
/// `semantic_model` the matching `LabelDef.range`.
pub fn resolve_label_locations(
    snapshot: &Analysis,
    resolution: &ResolvedLabels,
    lint_path: &Path,
    names: &[SmolStr],
    enc: PositionEncoding,
) -> Vec<Location> {
    let mut locations = Vec::new();
    for name in names {
        for def_path in resolution.definers(lint_path, name) {
            let Some(file) = snapshot.lookup_file(def_path) else {
                continue;
            };
            let idx = snapshot.file_line_index(file, enc);
            for label in snapshot.semantic_model(file).labels() {
                if &label.name == name {
                    locations.push(location_for(def_path, &idx, label.range));
                }
            }
        }
    }
    dedup_locations(locations)
}

/// For each `\cite` key, the `@entry{key,…}` sites in the `.bib` files of the
/// citation namespace: `citations.bib_definers` gives the analyzed bibliographies,
/// each `bib_semantic_model` the matching `Entry.key_range` (case-insensitive, as
/// BibTeX folds key case).
pub fn resolve_citation_locations(
    snapshot: &Analysis,
    citations: &ResolvedCitations,
    lint_path: &Path,
    names: &[SmolStr],
    enc: PositionEncoding,
) -> Vec<Location> {
    let mut locations = Vec::new();
    for bib_path in citations.bib_definers(lint_path) {
        let Some(file) = snapshot.lookup_file(bib_path) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        for entry in snapshot.bib_semantic_model(file).entries() {
            if names.iter().any(|n| n.eq_ignore_ascii_case(&entry.key)) {
                locations.push(location_for(bib_path, &idx, entry.key_range));
            }
        }
    }
    dedup_locations(locations)
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
    resolution: &ResolvedLabels,
    origin: &Path,
    names: &[SmolStr],
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for member in resolution.namespace_members(origin) {
        let Some(file) = snapshot.lookup_file(member) else {
            continue;
        };
        let Some(uri) = path_to_uri(member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        let model = snapshot.semantic_model(file);
        for r in model.refs() {
            if names.contains(&r.name) {
                push_edit(&mut changes, &uri, &idx, r.key_range, new_name);
            }
        }
        for label in model.labels() {
            if names.contains(&label.name) {
                push_edit(&mut changes, &uri, &idx, label.key_range, new_name);
            }
        }
    }
    changes
}

/// Every `\cite`-family use of `names` across `origin`'s citation namespace, plus the
/// bibliography `@entry` keys, rewritten to `new_name` at each precise `key_range`.
/// The rename mirror of [`reference_citation_locations`]: `.tex` use sites come from
/// `bib_citers` (a `.bib` origin) or `namespace_members` (a `.tex` origin); the
/// definition sites are the origin bib itself (`.bib` origin) or `bib_definers` (a
/// `.tex` origin). Matching is case-insensitive, as BibTeX folds key case.
pub fn rename_citation_edits(
    snapshot: &Analysis,
    citations: &ResolvedCitations,
    origin: &Path,
    kind: FileKind,
    names: &[SmolStr],
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    let tex_members = if kind == FileKind::Bib {
        citations.bib_citers(origin)
    } else {
        citations.namespace_members(origin)
    };
    for member in tex_members {
        let Some(file) = snapshot.lookup_file(member) else {
            continue;
        };
        let Some(uri) = path_to_uri(member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        for c in snapshot.semantic_model(file).citations() {
            if names.iter().any(|n| n.eq_ignore_ascii_case(&c.name)) {
                push_edit(&mut changes, &uri, &idx, c.key_range, new_name);
            }
        }
    }
    match kind {
        // From a `.bib` cursor, rewrite the entry in the origin bibliography itself.
        FileKind::Bib => push_bib_entry_edits(snapshot, &mut changes, origin, names, new_name, enc),
        _ => {
            for bib_path in citations.bib_definers(origin) {
                push_bib_entry_edits(snapshot, &mut changes, bib_path, names, new_name, enc);
            }
        }
    }
    changes
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
    resolution: &ResolvedLabels,
    packages: &PackageGraph,
    origin: &Path,
    name: &str,
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for member in name_refs::macro_namespace(resolution, packages, origin) {
        let Some(file) = snapshot.lookup_file(&member) else {
            continue;
        };
        let Some(uri) = path_to_uri(&member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        let root = snapshot.parsed_tree(file);
        for range in name_refs::command_occurrences(&root, name) {
            push_edit(
                &mut changes,
                &uri,
                &idx,
                name_refs::strip_backslash(range),
                new_name,
            );
        }
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
    resolution: &ResolvedLabels,
    packages: &PackageGraph,
    origin: &Path,
    target: &NameTarget,
    new_name: &str,
    enc: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for member in name_refs::macro_namespace(resolution, packages, origin) {
        let Some(file) = snapshot.lookup_file(&member) else {
            continue;
        };
        let Some(uri) = path_to_uri(&member) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        let root = snapshot.parsed_tree(file);
        for range in name_refs::environment_occurrences(&root, &target.name) {
            push_edit(&mut changes, &uri, &idx, range, new_name);
        }
    }
    for (def_path, name_range) in
        name_definition_sites(snapshot, resolution, packages, origin, target)
    {
        let Some(file) = snapshot.lookup_file(&def_path) else {
            continue;
        };
        let Some(uri) = path_to_uri(&def_path) else {
            continue;
        };
        let idx = snapshot.file_line_index(file, enc);
        push_edit(&mut changes, &uri, &idx, name_range, new_name);
    }
    changes
}

/// Push the `@entry` key edits for `names` in the bibliography at `bib_path` (case-
/// insensitive match), rewriting each `key_range` to `new_name`.
pub fn push_bib_entry_edits(
    snapshot: &Analysis,
    changes: &mut HashMap<Uri, Vec<TextEdit>>,
    bib_path: &Path,
    names: &[SmolStr],
    new_name: &str,
    enc: PositionEncoding,
) {
    let Some(file) = snapshot.lookup_file(bib_path) else {
        return;
    };
    let Some(uri) = path_to_uri(bib_path) else {
        return;
    };
    let idx = snapshot.file_line_index(file, enc);
    for entry in snapshot.bib_semantic_model(file).entries() {
        if names.iter().any(|n| n.eq_ignore_ascii_case(&entry.key)) {
            push_edit(changes, &uri, &idx, entry.key_range, new_name);
        }
    }
}

/// Append a `key_range → new_name` [`TextEdit`] to `uri`'s edit list.
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
