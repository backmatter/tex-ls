//! Completion operations over a captured analysis source.
mod citations;
pub(super) mod context;
mod edits;
pub mod rank;
pub mod resolve;

use self::{
    context as completion_context, edits as completion_edits, rank as completion_rank,
    resolve as completion_resolve,
};
use crate::convert::{file_kind_for, uri_to_fs_path, uri_to_path};
use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat, Position, Uri};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use tex_ls_analysis::{
    bib::{
        completion::{
            BibCandidateKind, BibCompletionCandidate, BibCompletionContext, bib_candidates,
            classify_bib_context,
        },
        semantic::Model as BibModel,
    },
    completion::{CandidateKind, CompletionCandidate, CompletionContext, FileArgKind},
    incremental::Analysis,
    project::{ResolvedCitations, ResolvedLabels, texmf::TexmfIndex},
    source::FileKind,
    text::PositionEncoding,
};
use tex_ls_parser::{
    declarations::ResolvedDeclarations,
    semantic::{SemanticModel, SignatureDb},
};

pub fn compute_completion(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
) -> lsp_types::CompletionList {
    compute_completion_impl(snapshot, uri, path, enc, position, true)
}

fn compute_completion_impl(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    enc: PositionEncoding,
    position: Position,
    use_bounded: bool,
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
    let context = (file_kind_for(path) != FileKind::Bib)
        .then(|| classify_tex_context(snapshot, path, offset));
    let bounded = use_bounded
        .then(|| citations::for_context(snapshot, path, offset, context.as_ref()))
        .flatten();
    let (mut list, deferred_label_details) = if let Some(list) = bounded {
        (list, false)
    } else {
        let mut items = if let Some(context) = &context {
            compute_tex_completion_with_context(snapshot, uri, path, offset, true, context.clone())
        } else {
            compute_bib_completion(snapshot, path, offset)
        };
        let (query, relevance, recompute, deferred_label_details) =
            completion_context::prepare_with_context(
                snapshot,
                path,
                offset,
                context.as_ref(),
                &mut items,
            );
        let list = completion_rank::rank(
            items,
            &query,
            &relevance,
            completion_rank::RESULT_LIMIT,
            recompute,
        );
        (list, deferred_label_details)
    };
    if deferred_label_details {
        completion_context::enrich_label_details(snapshot, path, &mut list.items);
    }
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
    completion_edits::attach_with_context(
        snapshot,
        path,
        offset,
        enc,
        context.as_ref(),
        &mut list.items,
    );
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
    let path = snapshot.file_path(file);
    let root = snapshot.parsed_bib_tree(file);
    if tex_ls_analysis::bib::completion::dependency_key_completion(&root, offset).is_some()
        && tex_ls_analysis::bib::completion::dependency_key_completion_for_backend(
            &root,
            offset,
            snapshot.resolve_citations().supports_aliases(path),
        )
        .is_some()
    {
        return cite_completion_items(snapshot, snapshot.resolve_citations(), path);
    }

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
    let mut items: Vec<_> = bib_candidates(&ctx, model)
        .into_iter()
        .map(bib_candidate_to_item)
        .collect();
    if matches!(ctx, BibCompletionContext::ValueMacro { .. }) {
        let mut seen: HashSet<_> = items
            .iter()
            .map(|item| item.label.to_ascii_lowercase())
            .collect();
        for path in snapshot.resolve_citations().bibliography_resources(path) {
            let Some(file) = snapshot.lookup_file(path) else {
                continue;
            };
            for site in snapshot.bib_semantic_model(file).string_defs() {
                if seen.insert(site.name.to_string()) {
                    items.push(CompletionItem {
                        label: site.name.to_string(),
                        kind: Some(CompletionItemKind::Constant),
                        ..Default::default()
                    });
                }
            }
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
    }
    items
}

pub fn compute_tex_completion(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    offset: usize,
) -> Vec<CompletionItem> {
    compute_tex_completion_inner(snapshot, uri, path, offset, false)
}

#[cfg(test)]
thread_local! {
    static TEX_CLASSIFICATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn classify_tex_context(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
) -> CompletionContext {
    #[cfg(test)]
    TEX_CLASSIFICATIONS.with(|count| count.set(count.get() + 1));
    let Some(file) = snapshot.lookup_file(path) else {
        return CompletionContext::None;
    };
    let root = snapshot.parsed_tree(file);
    tex_ls_analysis::completion::classify_context_with_roles(
        &root,
        offset,
        snapshot.declarations_for(path),
        || snapshot.file_roles(file),
    )
}

fn compute_tex_completion_inner(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    offset: usize,
    filter_candidates: bool,
) -> Vec<CompletionItem> {
    let context = classify_tex_context(snapshot, path, offset);
    compute_tex_completion_with_context(snapshot, uri, path, offset, filter_candidates, context)
}

fn compute_tex_completion_with_context(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    offset: usize,
    filter_candidates: bool,
    mut ctx: CompletionContext,
) -> Vec<CompletionItem> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let root = snapshot.parsed_tree(file);
    let declared = snapshot.declarations_for(path);
    let key_namespace = if matches!(
        ctx,
        CompletionContext::LabelDefinition { .. }
            | CompletionContext::LabelRef { .. }
            | CompletionContext::CitationKey { .. }
            | CompletionContext::GlossaryKey { .. }
    ) {
        tex_ls_analysis::completion::key_prefix_at_offset(&root, offset, declared).unwrap_or("")
    } else {
        ""
    };
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
            && tex_ls_analysis::hover::lookup_command(snapshot.editor_signatures(file), owner)
                .is_some_and(|(_, provenance)| {
                    matches!(provenance, tex_ls_analysis::hover::Provenance::Document)
                })
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
    let mut items = match ctx {
        CompletionContext::FilePath { ref prefix, kind } => {
            let Some(context) = snapshot.file_path_context(file) else {
                return Vec::new();
            };
            let command = tex_ls_analysis::completion::cursor_ancestors(&root, offset)
                .into_iter()
                .filter(|node| node.kind() == tex_ls_parser::syntax::SyntaxKind::COMMAND)
                .min_by_key(|node| node.text_range().len());
            let Some(command) = command else {
                return Vec::new();
            };
            let roles = snapshot.file_roles(file);
            let dirs: Vec<_> = context
                .bases
                .iter()
                .flat_map(|base| {
                    tex_ls_analysis::external::command_directories_with_roles(
                        &command,
                        Some(base),
                        Some(&context.root_directory),
                        &context.graphics,
                        roles,
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
            if matches!(
                ctx,
                CompletionContext::CommandName { .. } | CompletionContext::EnvironmentName { .. }
            ) {
                let symbols = snapshot.editor_symbols(file);
                let environment = matches!(ctx, CompletionContext::EnvironmentName { .. });
                let (names, kind) = if environment {
                    (
                        &symbols.environments,
                        tex_ls_analysis::completion::CandidateKind::Environment,
                    )
                } else {
                    (
                        &symbols.commands,
                        tex_ls_analysis::completion::CandidateKind::Command,
                    )
                };
                let unknown: Vec<_> = names
                    .iter()
                    .filter(|name| {
                        if environment {
                            symbols.signatures.environment(name).is_none()
                        } else {
                            symbols.signatures.command(name).is_none()
                        }
                    })
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
                            kind,
                            insert_text: None,
                            snippet: false,
                        },
                        Some(path),
                    ));
                }
            }
            items
        }
    };
    if !key_namespace.is_empty() {
        items.retain(|item| {
            item.label
                .strip_prefix(key_namespace)
                .is_some_and(tex_ls_parser::declarations::is_literal_key_suffix)
        });
        for item in &mut items {
            item.insert_text = item.label.strip_prefix(key_namespace).map(str::to_owned);
        }
    }
    items
}

/// Cite-key candidates: every entry in the citing file's bibliography namespace,
/// deduped by backend key identity (first definer wins). The list is *not* prefix-filtered —
/// each item carries full expanded key/title/author/editor search fields for the
/// shared ranker before limiting. Mirrors
/// [`crate::navigation::resolve_citation_locations`] but collects all entries rather than matching a target.
pub fn cite_completion_items(
    snapshot: &Analysis,
    citations: &ResolvedCitations,
    lint_path: &Path,
) -> Vec<CompletionItem> {
    citations::exhaustive(snapshot, citations, lint_path)
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
    let mut items = std::collections::BTreeMap::new();
    for path in labels.namespace_members(lint_path) {
        let Some(file) = snapshot.lookup_file(path) else {
            continue;
        };
        for (key, search) in snapshot.glossary_search_fields(file) {
            if !key.starts_with(prefix) {
                continue;
            }
            items.entry(key.clone()).or_insert_with(|| CompletionItem {
                label: key.to_string(),
                filter_text: Some(search.clone()),
                kind: Some(CompletionItemKind::Reference),
                data: completion_resolve::CompletionResolveData::Glossary {
                    file: lint_path.into(),
                    key: key.to_string(),
                }
                .into_value(),
                ..Default::default()
            });
        }
    }
    items.into_values().collect()
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
/// the document's supplied directory observations (see [`file_completion_items`]).
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
    use crate::convert::path_to_uri;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn prefixed_wrapper_completion_matches_canonical_names_and_inserts_suffix() {
        let path = Path::new(fixture_path!("/completion/wrappers.tex"));
        let uri = path_to_uri(path).unwrap();
        let mut db = IncrementalDatabase::default();
        let declarations: tex_ls_parser::declarations::Declarations = serde_json::from_str(r#"{"commands":{"defkey":{"like":"label","argument-index":1,"prefix":"sec:"},"usekey":{"like":"ref","argument-index":1,"prefix":"sec:"}}}"#).unwrap();
        db.set_declarations(declarations.resolve().unwrap());
        let source = r"\defkey{ignored}{intro}\label{other}\usekey{ignored}{in}";
        let file = db.apply_change(path, source, None);
        let snapshot = db.snapshot();
        let offset = source.len() - 1;
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(offset);
        let result = compute_completion(
            &snapshot,
            &uri,
            path,
            PositionEncoding::Utf16,
            Position::new(line, character),
        );
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].label, "sec:intro");
        let Some(lsp_types::CompletionItemTextEdit::TextEdit(edit)) = &result.items[0].text_edit
        else {
            panic!("replacement");
        };
        assert_eq!(edit.new_text, "intro");
        let index = snapshot.file_line_index(file, PositionEncoding::Utf16);
        let start = index.offset_at(edit.range.start.line, edit.range.start.character);
        let end = index.offset_at(edit.range.end.line, edit.range.end.character);
        assert_eq!(&source[start..end], "in");
    }

    #[test]
    fn prefixed_completion_only_inserts_literal_suffixes() {
        let path = Path::new(fixture_path!("/completion/wrappers.tex"));
        let uri = path_to_uri(path).unwrap();
        let mut db = IncrementalDatabase::default();
        let declarations: tex_ls_parser::declarations::Declarations =
            serde_json::from_str(r#"{"commands":{"wrapper":{"like":"ref","prefix":"sec:"}}}"#)
                .unwrap();
        db.set_declarations(declarations.resolve().unwrap());
        let source = "\\label{sec:literal}\\label{sec:two words}\\label{sec:tab\tkey}\\label{sec:active~}\\label{sec:bracket[key]}\\wrapper{}";
        let file = db.apply_change(path, source, None);
        let snapshot = db.snapshot();
        let (line, character) = snapshot
            .file_line_index(file, PositionEncoding::Utf16)
            .position(source.len() - 1);
        let result = compute_completion(
            &snapshot,
            &uri,
            path,
            PositionEncoding::Utf16,
            Position::new(line, character),
        );
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["sec:literal"]
        );
        let Some(lsp_types::CompletionItemTextEdit::TextEdit(edit)) = &result.items[0].text_edit
        else {
            panic!("replacement");
        };
        assert_eq!(edit.new_text, "literal");
    }

    #[test]
    fn bibliography_dependency_completion_filters_and_replaces_one_literal_key() {
        let path = Path::new(fixture_path!("/completion/dependencies.bib"));
        let uri = path_to_uri(path).unwrap();
        for value in [
            "{pa|per}",
            "{ | }",
            "{pa|",
            "\"pa|per\"",
            "{other, pa|per , last}",
        ] {
            let fixture = format!(
                "@misc{{paper,title={{A}}}}\n@misc{{other,title={{B}}}}\n@misc{{child,related={value}}}"
            );
            let offset = fixture.find('|').unwrap();
            let source = fixture.replace('|', "");
            let mut db = IncrementalDatabase::default();
            db.apply_change(
                &path.with_extension("tex"),
                r"\documentclass{article}\usepackage{biblatex}\addbibresource{dependencies.bib}",
                None,
            );
            let file = db.apply_change(path, source.as_str(), None);
            let snapshot = db.snapshot();
            let (line, character) = snapshot
                .file_line_index(file, PositionEncoding::Utf16)
                .position(offset);
            let result = compute_completion(
                &snapshot,
                &uri,
                path,
                PositionEncoding::Utf16,
                Position::new(line, character),
            );
            let item = result
                .items
                .iter()
                .find(|item| item.label == "paper")
                .expect("entry candidate");
            let Some(lsp_types::CompletionItemTextEdit::TextEdit(edit)) = &item.text_edit else {
                panic!("exact replacement edit");
            };
            let index = snapshot.file_line_index(file, PositionEncoding::Utf16);
            let start = index.offset_at(edit.range.start.line, edit.range.start.character);
            let end = index.offset_at(edit.range.end.line, edit.range.end.character);
            let expected = if value.contains("per") {
                "paper"
            } else if value.contains("pa|") {
                "pa"
            } else {
                ""
            };
            assert_eq!(&source[start..end], expected, "{value}");
            assert_eq!(edit.new_text, "paper");
            if !value.contains(" | ") {
                assert!(
                    result.items.iter().all(|item| item.label != "other"),
                    "query filters {value}"
                );
            }
        }
    }

    #[test]
    fn response_reuses_one_classification_for_candidates_ranking_and_edits() {
        let path = Path::new(fixture_path!("/completion/shared-context.tex"));
        let uri = path_to_uri(path).unwrap();
        for source in [
            r"\label{label}\ref{lab}",
            r"\bibitem{paper}\cite{pap}",
            r"\input{chapter}",
            r"\includegraphics[wid",
            r"\sec",
        ] {
            let mut db = IncrementalDatabase::default();
            db.apply_change(path, source, None);
            let snapshot = db.snapshot();
            let offset = source.len() - usize::from(source.ends_with('}'));
            let (line, character) = snapshot
                .file_line_index(snapshot.lookup_file(path).unwrap(), PositionEncoding::Utf16)
                .position(offset);
            TEX_CLASSIFICATIONS.with(|count| count.set(0));
            let _ = compute_completion(
                &snapshot,
                &uri,
                path,
                PositionEncoding::Utf16,
                Position::new(line, character),
            );
            assert_eq!(
                TEX_CLASSIFICATIONS.with(std::cell::Cell::get),
                1,
                "{source}"
            );
        }
    }

    #[test]
    fn deferred_label_details_match_eager_full_responses_with_long_titles_and_eqref() {
        use tex_ls_analysis::external::{
            CompilerArtifact, ExternalInputKind, ExternalInputs, Observation,
        };
        let path = Path::new(fixture_path!("/project/deferred-labels.tex"));
        let uri = path_to_uri(path).unwrap();
        let title = "長い heading ".repeat(2_800);
        let mut body = format!("\\documentclass{{article}}\n\\section{{FIRST {title}}}\n");
        for index in 0..256 {
            body.push_str(&format!("\\label{{a:first{index:03}}}\n"));
        }
        body.push_str(&format!("\\section{{SECOND MATCH {title}}}\n"));
        for index in 0..256 {
            body.push_str(&format!("\\label{{b:second{index:03}}}\n"));
        }
        for index in 0..128 {
            body.push_str(&format!(
                "\\begin{{equation}}x\\label{{z:eq{index:03}}}\\end{{equation}}\n"
            ));
        }
        for (command, query) in [("ref", ""), ("eqref", ""), ("ref", "SECOND MATCH")] {
            let source = format!("{body}\\{command}{{{query}}}");
            let offset = source.len() - 1;
            let mut db = tex_ls_analysis::incremental::IncrementalDatabase::default();
            let file = db.apply_change(path, source.as_str(), None);
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
                .unwrap();
            db.apply_external_inputs(token, ExternalInputs::Compiler(vec![(path.with_extension("aux"),
                Observation::Present(CompilerArtifact::from_text(&path.with_extension("aux"),
                    "\\newlabel{a:first000}{{7}{1}}\n\\newlabel{b:second000}{{8}{1}}\n\\newlabel{z:eq000}{{99}{1}}", "build".into())))]))
                .unwrap();
            let snapshot = db.snapshot();
            assert!(std::ptr::eq(
                snapshot.label_context_ref(path, "a:first000").unwrap(),
                snapshot.label_context_ref(path, "a:first001").unwrap()
            ));
            let mut eager = compute_tex_completion(&snapshot, &uri, path, offset);
            let (prepared_query, relevance, recompute, deferred) =
                completion_context::prepare(&snapshot, path, offset, &mut eager);
            assert_eq!(deferred, query.is_empty());
            if deferred {
                assert!(
                    eager
                        .iter()
                        .all(|item| item.detail.is_none() && item.filter_text.is_none())
                );
                completion_context::enrich_label_details(&snapshot, path, &mut eager);
            }
            for item in &eager {
                let number = match item.label.as_str() {
                    "a:first000" => Some("7"),
                    "b:second000" => Some("8"),
                    "z:eq000" => Some("99"),
                    _ => None,
                };
                assert_eq!(
                    snapshot.label_context_ref(path, &item.label).cloned(),
                    snapshot.label_context(path, &item.label)
                );
                assert_eq!(
                    item.detail,
                    super::super::hover::render_label_markdown(
                        snapshot.label_context(path, &item.label).as_ref(),
                        number
                    )
                );
            }
            let mut expected = completion_rank::rank(
                eager,
                &prepared_query,
                &relevance,
                completion_rank::RESULT_LIMIT,
                recompute,
            );
            let revision = snapshot.source_version(file).unwrap().revision;
            for item in &mut expected.items {
                if let Some(serde_json::Value::Object(data)) = &mut item.data {
                    data.insert("sourceRevision".into(), revision.into());
                    data.insert("storageEpoch".into(), snapshot.epoch().into());
                }
            }
            completion_edits::attach(
                &snapshot,
                path,
                offset,
                PositionEncoding::Utf16,
                &mut expected.items,
            );
            let position = snapshot
                .file_line_index(file, PositionEncoding::Utf16)
                .position(offset);
            let actual = compute_completion(
                &snapshot,
                &uri,
                path,
                PositionEncoding::Utf16,
                Position::new(position.0, position.1),
            );
            assert_eq!(actual, expected, "{command} query {query:?}");
            assert_eq!(actual.items.len(), completion_rank::RESULT_LIMIT);
            assert!(actual.is_incomplete);
            if command == "eqref" {
                assert!(
                    actual
                        .items
                        .iter()
                        .all(|item| item.kind == Some(CompletionItemKind::Value))
                );
            } else if !query.is_empty() {
                assert!(
                    actual
                        .items
                        .iter()
                        .all(|item| item.label.starts_with("b:second"))
                );
            }
        }
    }

    #[test]
    fn inspected_definitions_cannot_add_command_completion_candidates() {
        let source = r"\meaning\newcommand{\ghost}[1]{#1}\newcommand{\real}[1]{#1}\gh";
        let path = Path::new(fixture_path!("/completion/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, source, None);
        let snapshot = db.snapshot();
        let items =
            compute_tex_completion(&snapshot, &path_to_uri(path).unwrap(), path, source.len());
        assert!(items.iter().all(|item| item.label != "ghost"));
        assert!(items.iter().any(|item| item.label == "real"));
    }

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
