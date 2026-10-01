//! Completion operations over a captured analysis source.
use super::*;

pub fn compute_completion(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> lsp_types::CompletionList {
    let Some(file) = snapshot.lookup_file(path) else {
        return completion_rank::rank(
            Vec::new(),
            "",
            &HashMap::new(),
            completion_rank::RESULT_LIMIT,
            false,
        );
    };
    let offset = snapshot
        .file_line_index(file, enc)
        .offset_at(position.line, position.character);
    let mut items = if file_kind_for(path) == FileKind::Bib {
        compute_bib_completion(snapshot, path, offset)
    } else {
        compute_tex_completion_inner(snapshot, uri, path, offset, true)
    };
    let (query, relevance, recompute) =
        completion_context::prepare(snapshot, path, offset, &mut items);
    let mut list = completion_rank::rank(
        items,
        &query,
        &relevance,
        completion_rank::RESULT_LIMIT,
        recompute,
    );
    let revision = snapshot
        .source_version(file)
        .expect("captured source")
        .revision;
    let epoch = snapshot.epoch();
    for item in &mut list.items {
        if let Some(serde_json::Value::Object(data)) = &mut item.data {
            data.insert("sourceRevision".into(), revision.into());
            data.insert("storageEpoch".into(), epoch.into());
        }
    }
    completion_edits::attach(snapshot, path, offset, enc, &mut list.items);
    list
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
    let root = snapshot.parsed_bib_tree(file);
    if tex_ls_analysis::bib::completion::tex_command_prefix(&root, offset).is_some() {
        let prefix = String::new();
        let mut items: Vec<_> = tex_ls_analysis::completion::candidates_with_declarations(
            &CompletionContext::CommandName {
                prefix: prefix.clone(),
            },
            &SignatureDb::default(),
            &SemanticModel::default(),
            snapshot.declarations_for(path),
        )
        .into_iter()
        .map(|candidate| candidate_to_item(candidate, Some(path)))
        .collect();
        for name in [
            "'", "\"", "`", "^", "~", "=", ".", "c", "v", "u", "H", "r", "k",
        ] {
            if name.starts_with(&prefix) && !items.iter().any(|item| item.label == name) {
                items.push(CompletionItem {
                    label: name.into(),
                    kind: Some(CompletionItemKind::Function),
                    detail: Some("TeX text accent".into()),
                    ..Default::default()
                });
            }
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
        return items;
    }
    let mut ctx = classify_bib_context(&root, offset);
    match &mut ctx {
        BibCompletionContext::EntryType { prefix }
        | BibCompletionContext::FieldName { prefix, .. }
        | BibCompletionContext::ValueMacro { prefix } => prefix.clear(),
        BibCompletionContext::None => {}
    }
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
) -> Vec<CompletionItem> {
    compute_tex_completion_inner(snapshot, uri, path, offset, false)
}

fn compute_tex_completion_inner(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    offset: usize,
    filter_candidates: bool,
) -> Vec<CompletionItem> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let root = snapshot.parsed_tree(file);
    let declared = snapshot.declarations_for(path);
    let mut ctx =
        tex_ls_analysis::completion::classify_context_with_declarations(&root, offset, declared);
    let query = completion_context::prefix(&ctx).to_owned();
    completion_context::clear_prefix(&mut ctx);
    let matcher =
        (filter_candidates && !query.is_empty()).then(|| completion_rank::MatchQuery::new(&query));
    if let CompletionContext::Option {
        owner,
        packages,
        value,
        values,
        ..
    } = &mut ctx
    {
        if !declared.options.contains_key(owner)
            && crate::hover::lookup_command(snapshot.editor_signatures(file), owner).is_some_and(
                |(_, provenance)| matches!(provenance, crate::hover::Provenance::Document),
            )
        {
            return Vec::new();
        }
        if !*value {
            let resolved: std::collections::BTreeSet<_> = snapshot
                .file_references(file)
                .iter()
                .filter_map(
                    |reference| match snapshot.resolve_file(&reference.candidates).target {
                        tex_ls_analysis::external::Observation::Present(path) => {
                            Some(snapshot.file_alias(&path).unwrap_or(&path).to_owned())
                        }
                        _ => None,
                    },
                )
                .collect();
            for package in packages.iter() {
                for member in snapshot.project_members().iter().filter(|member| {
                    member
                        .path
                        .file_stem()
                        .is_some_and(|name| name == package.as_str())
                        && member.path.extension().is_some_and(|ext| ext == "sty")
                        && resolved.contains(&member.path)
                }) {
                    values.extend(
                        snapshot
                            .semantic_model(member.file)
                            .options()
                            .iter()
                            .filter_map(|option| option.name.as_ref().map(ToString::to_string)),
                    );
                }
            }
            values.sort();
            values.dedup();
        }
    }
    let path = snapshot.file_path(file);
    match ctx {
        CompletionContext::FilePath { ref prefix, kind } => {
            let Some(context) = snapshot.file_path_context(file) else {
                return Vec::new();
            };
            let dirs: Vec<_> = context
                .bases
                .iter()
                .flat_map(|base| {
                    tex_ls_analysis::external::completion_directories(
                        &root,
                        offset,
                        Some(base),
                        Some(&context.root_directory),
                        &context.graphics,
                    )
                })
                .collect();
            let mut items: Vec<_> = dirs
                .iter()
                .flat_map(|dir| file_completion_in_directory(dir, prefix, kind, snapshot))
                .collect();
            items.sort_by(|a, b| a.label.cmp(&b.label));
            items.dedup_by(|a, b| a.label == b.label);
            items
        }
        CompletionContext::CitationKey { .. } => {
            cite_completion_items(snapshot, snapshot.resolve_citations(), path)
        }
        CompletionContext::LabelDefinition { .. } => {
            let resolution = snapshot.resolve_labels();
            let mut names = std::collections::BTreeSet::new();
            for member in resolution.namespace_members(path) {
                if let Some(file) = snapshot.lookup_file(member) {
                    names.extend(
                        snapshot
                            .file_refs(file)
                            .iter()
                            .filter(|name| !resolution.is_defined(path, name))
                            .map(ToString::to_string),
                    );
                }
            }
            names
                .into_iter()
                .map(|label| CompletionItem {
                    label,
                    kind: Some(CompletionItemKind::Reference),
                    detail: Some("Referenced but not defined".into()),
                    ..Default::default()
                })
                .collect()
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
                    (snapshot.editor_signatures(file), &empty_model)
                }
                CompletionContext::ColorName { .. } => (&empty_sigs, snapshot.semantic_model(file)),
                _ => (&empty_sigs, &empty_model),
            };
            let mut items = build_completion_items(
                &ctx,
                sigs,
                model,
                declared,
                uri,
                snapshot,
                matcher.as_ref(),
            );
            if matches!(ctx, CompletionContext::CommandName { .. }) {
                let symbols = snapshot.editor_symbols(file);
                let unknown: Vec<_> = symbols
                    .commands
                    .iter()
                    .filter(|name| symbols.signatures.command(name).is_none())
                    .filter(|name| {
                        matcher
                            .as_ref()
                            .is_none_or(|m| m.score_text(name).is_some())
                    })
                    .collect();
                let replaced: HashSet<_> = unknown.iter().map(|name| name.as_str()).collect();
                items.retain(|item| !replaced.contains(item.label.as_str()));
                for name in unknown {
                    items.push(candidate_to_item(
                        CompletionCandidate {
                            label: name.clone(),
                            kind: tex_ls_analysis::completion::CandidateKind::Command,
                            insert_text: None,
                            snippet: false,
                        },
                        Some(path),
                    ));
                }
            }
            if matches!(ctx, CompletionContext::EnvironmentName { .. }) {
                let symbols = snapshot.editor_symbols(file);
                let unknown: Vec<_> = symbols
                    .environments
                    .iter()
                    .filter(|name| symbols.signatures.environment(name).is_none())
                    .filter(|name| {
                        matcher
                            .as_ref()
                            .is_none_or(|m| m.score_text(name).is_some())
                    })
                    .collect();
                let replaced: HashSet<_> = unknown.iter().map(|name| name.as_str()).collect();
                items.retain(|item| !replaced.contains(item.label.as_str()));
                for name in unknown {
                    items.push(candidate_to_item(
                        CompletionCandidate {
                            label: name.clone(),
                            kind: tex_ls_analysis::completion::CandidateKind::Environment,
                            insert_text: None,
                            snippet: false,
                        },
                        Some(path),
                    ));
                }
            }
            items
        }
    }
}

/// Cite-key candidates: every entry in the citing file's bibliography namespace,
/// deduped by folded key (first definer wins). The list is *not* prefix-filtered —
/// each item carries full expanded key/title/author/editor search fields for the
/// shared ranker before limiting. Mirrors
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
        for (key, search) in snapshot.bib_search_fields(file) {
            // Dedup case-insensitively (BibTeX folds key case); the first definer wins.
            if !seen.insert(key.to_lowercase()) {
                continue;
            }
            items.push(CompletionItem {
                // Carry the citing file + key so `completionItem/resolve` can re-walk
                // the bibliography namespace and attach the entry card lazily.
                data: completion_resolve::CompletionResolveData::Citation {
                    lint_path: lint_path.to_path_buf(),
                    key: key.to_string(),
                }
                .into_value(),
                label: key.to_string(),
                filter_text: Some(search.clone()),
                kind: Some(CompletionItemKind::Reference),
                ..Default::default()
            });
        }
    }
    for path in citations.namespace_members(lint_path) {
        for (name, body) in crate::source_cards::manual_bodies(snapshot, path) {
            if seen.insert(name.to_lowercase()) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::Reference),
                    detail: Some("Manual bibliography item".into()),
                    filter_text: Some(format!("{name} {body}")),
                    data: completion_resolve::CompletionResolveData::Citation {
                        lint_path: lint_path.to_owned(),
                        key: name,
                    }
                    .into_value(),
                    ..Default::default()
                });
            }
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
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
/// through the pure [`tex_ls_analysis::completion::candidates`]; a file-path context reads
/// the document's directory off disk (see [`file_completion_items`]).
pub(crate) fn build_completion_items(
    ctx: &CompletionContext,
    sigs: &SignatureDb,
    model: &SemanticModel,
    declared: &ResolvedDeclarations,
    uri: &Uri,
    texmf: &Analysis,
    matcher: Option<&completion_rank::MatchQuery>,
) -> Vec<CompletionItem> {
    match ctx {
        CompletionContext::FilePath { prefix, kind } => {
            file_completion_items(uri, prefix, *kind, texmf)
        }
        CompletionContext::PackageName { prefix, kind } => {
            // Read only the installed names already published by the host.
            let index = texmf.texmf();
            package_completion_items(uri, prefix, *kind, sigs, model, index, texmf)
        }
        CompletionContext::None => Vec::new(),
        _ => {
            // Preserve the same source identity as synchronization, including
            // synthetic untitled keys. Resolve only consults captured analysis.
            let file = uri_to_path(uri);
            tex_ls_analysis::completion::candidates_with_declarations_matching(
                ctx,
                sigs,
                model,
                declared,
                |name| matcher.is_none_or(|matcher| matcher.score_text(name).is_some()),
            )
            .into_iter()
            .map(|candidate| candidate_to_item(candidate, Some(&file)))
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
    host: &Analysis,
) -> Vec<CompletionItem> {
    let Some(doc_path) = uri_to_fs_path(uri) else {
        return Vec::new();
    };
    let Some(doc_dir) = doc_path.parent() else {
        return Vec::new();
    };
    file_completion_in_directory(doc_dir, prefix, kind, host)
}

fn file_completion_in_directory(
    doc_dir: &Path,
    prefix: &str,
    kind: FileArgKind,
    host: &Analysis,
) -> Vec<CompletionItem> {
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
        } else if kind.extensions().is_empty() || has_extension(&name, kind.extensions()) {
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
/// ([`tex_ls_parser::semantic::completion::package_names`], all of CTAN in rank order). Files/installed
/// names are offered as their **stem** (`\usepackage` takes a name, not a filename),
/// so `amsmath.sty` becomes `amsmath`; a name already emitted by an earlier tier is
/// dropped. Every item is enriched with the CTAN one-line description
/// ([`package_metadata`](tex_ls_parser::semantic::completion::package_metadata)) as `detail`,
/// before the shared ranking stage assigns sortText. An empty installed index
/// contributes no installed names.
pub fn package_completion_items(
    uri: &Uri,
    prefix: &str,
    kind: FileArgKind,
    sigs: &SignatureDb,
    model: &SemanticModel,
    texmf: &TexmfIndex,
    host: &Analysis,
) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<CompletionItem> = Vec::new();
    // Tier 1: local files (offered as stems).
    let file_prefix = format!("{}{prefix}", kind.file_prefix());
    for file_item in file_completion_items(uri, &file_prefix, kind, host) {
        // A directory can't be a package/class *name*; only files, as stems.
        if file_item.kind != Some(CompletionItemKind::File) {
            continue;
        }
        let Some(stem) = kind.name_from_filename(&file_item.label).map(str::to_owned) else {
            continue;
        };
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
    let mut installed: Vec<_> = match kind {
        FileArgKind::NamedFile {
            prefix,
            suffix: ".sty",
        } => texmf
            .sty_stems()
            .iter()
            .filter_map(|name| name.strip_prefix(prefix))
            .collect(),
        FileArgKind::NamedFile { .. } => texmf
            .by_name
            .keys()
            .filter_map(|name| kind.name_from_filename(name))
            .collect(),
        FileArgKind::Class => texmf.cls_stems().iter().map(String::as_str).collect(),
        _ => texmf.sty_stems().iter().map(String::as_str).collect(),
    };
    installed.sort_unstable();
    for stem in installed.into_iter().filter(|s| s.starts_with(prefix)) {
        if seen.insert(stem.to_owned()) {
            items.push(CompletionItem {
                label: stem.to_owned(),
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
    for candidate in tex_ls_analysis::completion::candidates(&ctx, sigs, model) {
        if seen.contains(&candidate.label) {
            continue;
        }
        items.push(candidate_to_item(candidate, file.as_deref()));
    }
    for item in &mut items {
        // Attach the CTAN description as detail (when this stem has metadata and no
        // tier already set one).
        if item.detail.is_none()
            && let Some(meta) = tex_ls_parser::semantic::completion::package_metadata(&item.label)
        {
            item.detail = meta.desc.map(str::to_string);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn unknown_arity_overrides_metadata_without_losing_known_signatures() {
        for (source, unknown, known, unknown_detail, known_detail) in [
            (
                "\\RenewDocumentCommand{\\section}{r()}{#1}\n\\NewDocumentCommand{\\auditblind}{r()}{#1}\n\\newcommand{\\auditknown}[1]{#1}\n\\aud",
                "section",
                "auditknown",
                "\\section",
                "\\auditknown{}",
            ),
            (
                "\\RenewDocumentEnvironment{tabular}{r()}{#1}{}\n\\NewDocumentEnvironment{auditblind}{r()}{#1}{}\n\\newenvironment{auditknown}[1]{}{}\n\\begin{aud",
                "tabular",
                "auditknown",
                "\\begin{tabular}",
                "\\begin{auditknown}{}",
            ),
        ] {
            let path = Path::new(fixture_path!("/completion/main.tex"));
            let mut db = IncrementalDatabase::default();
            db.apply_change(path, source, None);
            let snapshot = db.snapshot();
            let items =
                compute_tex_completion(&snapshot, &path_to_uri(path).unwrap(), path, source.len());
            for name in [unknown, known, "auditblind"] {
                assert_eq!(
                    items.iter().filter(|item| item.label == name).count(),
                    1,
                    "{name}"
                );
            }
            let resolve = |name: &str| {
                crate::completion_resolve::resolve(
                    &snapshot,
                    items
                        .iter()
                        .find(|item| item.label == name)
                        .unwrap()
                        .clone(),
                )
            };
            let unknown_item = resolve(unknown);
            assert_eq!(unknown_item.detail.as_deref(), Some(unknown_detail));
            assert!(
                matches!(unknown_item.documentation, Some(lsp_types::Documentation::MarkupContent(doc)) if doc.value.contains("argument signature is unknown"))
            );
            assert_eq!(resolve(known).detail.as_deref(), Some(known_detail));
        }
    }
}
