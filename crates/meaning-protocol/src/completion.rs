//! Completion operations over a captured analysis source.
use super::*;

pub fn compute_completion(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
    texmf: &dyn HostServices,
) -> Vec<CompletionItem> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let offset = snapshot
        .file_line_index(file, enc)
        .offset_at(position.line, position.character);
    if file_kind_for(path) == FileKind::Bib {
        return compute_bib_completion(snapshot, path, offset);
    }
    compute_tex_completion(snapshot, uri, path, offset, texmf)
}

/// Only value-macro completion requires the bibliography semantic model.
pub fn compute_bib_completion(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
) -> Vec<CompletionItem> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let ctx = classify_bib_context(&snapshot.parsed_bib_tree(file), offset);
    let empty = BibModel::default();
    let model = match ctx {
        BibCompletionContext::ValueMacro { .. } => snapshot.bib_semantic_model(file),
        _ => &empty,
    };
    bib_candidates(&ctx, model)
        .into_iter()
        .map(bib_candidate_to_item)
        .collect()
}

pub fn compute_tex_completion(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    offset: usize,
    texmf: &dyn HostServices,
) -> Vec<CompletionItem> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let root = snapshot.parsed_tree(file);
    let declared = snapshot.declarations_for(path);
    let ctx =
        meaning_analysis::completion::classify_context_with_declarations(&root, offset, declared);
    let path = snapshot.file_path(file);
    match ctx {
        CompletionContext::CitationKey { .. } => {
            cite_completion_items(snapshot, snapshot.resolve_citations(), path)
        }
        CompletionContext::LabelRef { prefix } => {
            let resolution = snapshot.resolve_labels();
            let mut names: Vec<_> = resolution
                .label_names(path)
                .filter(|name| name.starts_with(&prefix))
                .collect();
            names.sort_unstable();
            names
                .into_iter()
                .map(|name| CompletionItem {
                    label: name.to_owned(),
                    kind: Some(CompletionItemKind::Reference),
                    ..Default::default()
                })
                .collect()
        }
        CompletionContext::GlossaryKey { prefix } => {
            glossary_completion_items(snapshot, snapshot.resolve_labels(), path, &prefix)
        }
        _ => {
            // Demand only the facts consumed by this candidate family.
            let empty_sigs = SignatureDb::default();
            let empty_model = SemanticModel::default();
            let (sigs, model) = match &ctx {
                CompletionContext::CommandName { .. }
                | CompletionContext::EnvironmentName { .. } => {
                    (snapshot.scope_signatures(file), &empty_model)
                }
                CompletionContext::ColorName { .. } => (&empty_sigs, snapshot.semantic_model(file)),
                _ => (&empty_sigs, &empty_model),
            };
            build_completion_items(&ctx, sigs, model, declared, uri, texmf)
        }
    }
}

/// Cite-key candidates: every entry in the citing file's bibliography namespace,
/// deduped by folded key (first definer wins). The list is *not* prefix-filtered —
/// each item carries a `filterText` of key + title + authors, so the client filters
/// on any of those fields (LaTeX Workshop's `citation.filterText`). Mirrors
/// [`resolve_citation_locations`] but collects all entries rather than matching a target.
pub fn cite_completion_items(
    snapshot: &Analysis,
    citations: &ResolvedCitations,
    lint_path: &Path,
) -> Vec<CompletionItem> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut items: Vec<CompletionItem> = Vec::new();
    for bib_path in citations.bib_definers(lint_path) {
        let Some(file) = snapshot.lookup_file(bib_path) else {
            continue;
        };
        for entry in snapshot.bib_semantic_model(file).entries() {
            // Dedup case-insensitively (BibTeX folds key case); the first definer wins.
            if !seen.insert(entry.key.to_lowercase()) {
                continue;
            }
            items.push(CompletionItem {
                // Carry the citing file + key so `completionItem/resolve` can re-walk
                // the bibliography namespace and attach the entry card lazily.
                data: completion_resolve::CompletionResolveData::Citation {
                    lint_path: lint_path.to_path_buf(),
                    key: entry.key.to_string(),
                }
                .into_value(),
                label: entry.key.to_string(),
                filter_text: Some(citation_filter_text(entry)),
                // A deterministic tiebreak within the client's match-score bucket,
                // preserving the old alphabetical-by-key order.
                sort_text: Some(entry.key.to_lowercase()),
                kind: Some(CompletionItemKind::Reference),
                ..Default::default()
            });
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

/// The `filterText` for a citation item: key, then title, then authors, space-joined
/// and truncated to 128 chars on a char boundary. The key comes first so it always
/// survives the cap (VS Code truncates `filterText` at 128 chars).
pub fn citation_filter_text(entry: &meaning_analysis::bib::semantic::Entry) -> String {
    let mut text = entry.key.to_string();
    for extra in [entry.title.as_ref(), entry.authors.as_ref()]
        .into_iter()
        .flatten()
    {
        text.push(' ');
        text.push_str(extra);
    }
    if text.len() > 128 {
        let end = text
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= 128)
            .last()
            .unwrap_or(0);
        text.truncate(end);
    }
    text
}

/// Glossary/acronym key candidates: every `\newglossaryentry`/`\newacronym` key
/// defined in the completing file's namespace (its include-graph component,
/// [`ResolvedLabels::namespace_members`]), prefix-filtered and deduped. The
/// glossary analog of [`cite_completion_items`]; unlike BibTeX keys, glossary
/// keys are case-sensitive, so the prefix filter is exact.
pub fn glossary_completion_items(
    snapshot: &Analysis,
    labels: &ResolvedLabels,
    lint_path: &Path,
    prefix: &str,
) -> Vec<CompletionItem> {
    let mut keys: Vec<SmolStr> = Vec::new();
    for member in labels.namespace_members(lint_path) {
        let Some(file) = snapshot.lookup_file(member) else {
            continue;
        };
        keys.extend(
            snapshot
                .file_glossary_keys(file)
                .iter()
                .filter(|key| key.starts_with(prefix))
                .cloned(),
        );
    }
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|key| CompletionItem {
            label: key.to_string(),
            kind: Some(CompletionItemKind::Reference),
            ..Default::default()
        })
        .collect()
}

/// Map a neutral [`BibCompletionCandidate`] onto an `lsp_types::CompletionItem`.
pub fn bib_candidate_to_item(candidate: BibCompletionCandidate) -> CompletionItem {
    let kind = match candidate.kind {
        BibCandidateKind::EntryType => CompletionItemKind::Struct,
        BibCandidateKind::FieldName => CompletionItemKind::Field,
        BibCandidateKind::StringMacro => CompletionItemKind::Constant,
    };
    CompletionItem {
        label: candidate.label,
        kind: Some(kind),
        ..Default::default()
    }
}

/// Turn a classified [`CompletionContext`] into LSP items. Name/label contexts go
/// through the pure [`meaning_analysis::completion::candidates`]; a file-path context reads
/// the document's directory off disk (see [`file_completion_items`]).
pub fn build_completion_items(
    ctx: &CompletionContext,
    sigs: &SignatureDb,
    model: &SemanticModel,
    declared: &ResolvedDeclarations,
    uri: &Uri,
    texmf: &dyn HostServices,
) -> Vec<CompletionItem> {
    match ctx {
        CompletionContext::FilePath { prefix, kind } => {
            file_completion_items(uri, prefix, *kind, texmf)
        }
        CompletionContext::PackageName { prefix, kind } => {
            // Resolve the installed-tree index lazily, here, so only a package/class
            // completion pays for the (first-time) tree walk.
            let index = texmf.texmf();
            package_completion_items(uri, prefix, *kind, sigs, model, index, texmf)
        }
        CompletionContext::None => Vec::new(),
        _ => {
            // The document path keys the scope-first signature lookup that
            // `completionItem/resolve` repeats; unsaved buffers have none.
            let file = uri_to_fs_path(uri);
            meaning_analysis::completion::candidates_with_declarations(ctx, sigs, model, declared)
                .into_iter()
                .map(|candidate| candidate_to_item(candidate, file.as_deref()))
                .collect()
        }
    }
}

/// Map a neutral [`CompletionCandidate`] onto an `lsp_types::CompletionItem`. A
/// command/environment carries resolve `data` (its name + originating `file`) so
/// its signature can be attached lazily; a label carries none.
pub fn candidate_to_item(candidate: CompletionCandidate, file: Option<&Path>) -> CompletionItem {
    let kind = match candidate.kind {
        CandidateKind::Command => CompletionItemKind::Function,
        CandidateKind::Environment => CompletionItemKind::Class,
        CandidateKind::Label => CompletionItemKind::Reference,
        CandidateKind::Package => CompletionItemKind::Module,
        CandidateKind::Color => CompletionItemKind::Color,
        CandidateKind::ColorModel => CompletionItemKind::EnumMember,
        CandidateKind::TikzLibrary => CompletionItemKind::Module,
        CandidateKind::ArgumentEnum => CompletionItemKind::EnumMember,
    };
    let data = file.and_then(|file| {
        let payload = match candidate.kind {
            CandidateKind::Command => completion_resolve::CompletionResolveData::Command {
                name: candidate.label.clone(),
                file: file.to_path_buf(),
            },
            CandidateKind::Environment => completion_resolve::CompletionResolveData::Environment {
                name: candidate.label.clone(),
                file: file.to_path_buf(),
            },
            // A package/class name carries no resolvable signature (yet); a future
            // description payload would attach here. Colors and TikZ libraries are
            // likewise static labels with nothing to resolve lazily.
            CandidateKind::Label
            | CandidateKind::Package
            | CandidateKind::Color
            | CandidateKind::ColorModel
            | CandidateKind::TikzLibrary
            | CandidateKind::ArgumentEnum => return None,
        };
        payload.into_value()
    });
    CompletionItem {
        label: candidate.label,
        kind: Some(kind),
        insert_text: candidate.insert_text,
        insert_text_format: candidate.snippet.then_some(InsertTextFormat::Snippet),
        data,
        ..Default::default()
    }
}

/// File-path candidates for a `\includegraphics`/`\input`/… argument: read the
/// directory the partial path points into (relative to the document's on-disk
/// directory) and offer matching files (by [`FileArgKind`] extension) and
/// subdirectories. Empty for an unsaved buffer (no `file://` path) or an
/// unreadable directory. The label is the bare entry name; editors treat `/` as a
/// word boundary, so completing after `img/` replaces only the trailing segment.
pub fn file_completion_items(
    uri: &Uri,
    prefix: &str,
    kind: FileArgKind,
    host: &dyn HostServices,
) -> Vec<CompletionItem> {
    let Some(doc_path) = uri_to_fs_path(uri) else {
        return Vec::new();
    };
    let Some(doc_dir) = doc_path.parent() else {
        return Vec::new();
    };
    // Split the typed prefix into its directory part and the trailing filename
    // prefix; the directory part is resolved relative to the document.
    let (dir_part, file_prefix) = match prefix.rfind('/') {
        Some(slash) => (&prefix[..=slash], &prefix[slash + 1..]),
        None => ("", prefix),
    };
    let entries = host.read_dir(&doc_dir.join(dir_part));

    let mut items = Vec::new();
    for (name, is_dir) in entries {
        // Skip hidden entries and those not matching the typed filename prefix.
        if name.starts_with('.') || !name.starts_with(file_prefix) {
            continue;
        }
        if is_dir {
            items.push(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::Folder),
                ..Default::default()
            });
        } else if has_extension(&name, kind.extensions()) {
            items.push(CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::File),
                ..Default::default()
            });
        }
    }
    items
}

/// Package/class name candidates for `\usepackage`/`\documentclass`, in three tiers
/// of decreasing relevance: local `.sty`/`.cls` files in the document directory, then
/// the **installed set** from the TEXMF index (`texmf`), then the baked name list
/// ([`meaning_parser::semantic::completion::package_names`], all of CTAN in rank order). Files/installed
/// names are offered as their **stem** (`\usepackage` takes a name, not a filename),
/// so `amsmath.sty` becomes `amsmath`; a name already emitted by an earlier tier is
/// dropped. Every item is enriched with the CTAN one-line description
/// ([`package_metadata`](meaning_parser::semantic::completion::package_metadata)) as `detail`,
/// and `sortText` is assigned by final position so the client preserves the tiering
/// instead of re-sorting alphabetically. An empty `texmf` simply skips the middle
/// tier (the pre-index behavior).
pub fn package_completion_items(
    uri: &Uri,
    prefix: &str,
    kind: FileArgKind,
    sigs: &SignatureDb,
    model: &SemanticModel,
    texmf: &TexmfIndex,
    host: &dyn HostServices,
) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<CompletionItem> = Vec::new();
    // Tier 1: local files (offered as stems).
    for file_item in file_completion_items(uri, prefix, kind, host) {
        // A directory can't be a package/class *name*; only files, as stems.
        if file_item.kind != Some(CompletionItemKind::File) {
            continue;
        }
        let stem = file_stem(&file_item.label);
        if seen.insert(stem.clone()) {
            items.push(CompletionItem {
                label: stem,
                kind: Some(CompletionItemKind::Module),
                ..Default::default()
            });
        }
    }
    // Tier 2: the installed set (what the user actually has), prefix-filtered here
    // (the baked tier filters inside `candidates`).
    let installed = match kind {
        FileArgKind::Class => texmf.cls_stems(),
        _ => texmf.sty_stems(),
    };
    for stem in installed.iter().filter(|s| s.starts_with(prefix)) {
        if seen.insert(stem.clone()) {
            items.push(CompletionItem {
                label: stem.clone(),
                kind: Some(CompletionItemKind::Module),
                ..Default::default()
            });
        }
    }
    // Tier 3: the baked all-of-CTAN name list.
    let ctx = CompletionContext::PackageName {
        prefix: prefix.to_string(),
        kind,
    };
    let file = uri_to_fs_path(uri);
    for candidate in meaning_analysis::completion::candidates(&ctx, sigs, model) {
        if seen.contains(&candidate.label) {
            continue;
        }
        items.push(candidate_to_item(candidate, file.as_deref()));
    }
    for (i, item) in items.iter_mut().enumerate() {
        // Attach the CTAN description as detail (when this stem has metadata and no
        // tier already set one).
        if item.detail.is_none()
            && let Some(meta) = meaning_parser::semantic::completion::package_metadata(&item.label)
        {
            item.detail = meta.desc.map(str::to_string);
        }
        item.sort_text = Some(format!("{i:06}"));
    }
    items
}

/// The stem of a filename label (`amsmath.sty` -> `amsmath`); unchanged if no dot.
pub fn file_stem(label: &str) -> String {
    label
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| label.to_string())
}

/// Whether `name`'s extension (case-insensitive) is one of `exts`.
pub fn has_extension(name: &str, exts: &[&str]) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => {
            let ext = ext.to_ascii_lowercase();
            exts.contains(&ext.as_str())
        }
        None => false,
    }
}
