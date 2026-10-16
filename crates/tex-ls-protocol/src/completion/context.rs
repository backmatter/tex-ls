//! Completion context, provenance and presentation before the shared ranker.
use super::rank as completion_rank;
use crate::{context::document_aux, convert::file_kind_for};
use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat};
use rowan::TextSize;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use tex_ls_analysis::{
    bib::completion::{BibCompletionContext, classify_bib_context},
    completion::{CompletionContext, FileArgKind},
    incremental::Analysis,
    source::FileKind,
};
use tex_ls_parser::{
    ast::{AstNode, Environment},
    semantic::{
        LabelContext, SignatureDb,
        signature::{builtin, cwl},
    },
    syntax::{SyntaxKind, SyntaxNode},
};

/// The innermost command's built-in file role at the completion cursor.
pub(super) fn file_role_at(
    root: &SyntaxNode,
    offset: usize,
) -> Option<tex_ls_parser::semantic::roles::FileRole> {
    tex_ls_analysis::completion::cursor_ancestors(root, offset)
        .into_iter()
        .filter(|node| {
            node.kind() == SyntaxKind::COMMAND
                && node
                    .text_range()
                    .contains_inclusive(TextSize::new(offset as u32))
        })
        .min_by_key(|node| node.text_range().len())
        .and_then(|node| tex_ls_parser::ast::command_name(&node))
        .and_then(|name| tex_ls_parser::semantic::roles::file_role(&name))
}

pub(super) fn prefix(context: &CompletionContext) -> &str {
    match context {
        CompletionContext::Option { prefix, .. }
        | CompletionContext::CommandName { prefix }
        | CompletionContext::EnvironmentName { prefix, .. }
        | CompletionContext::LabelRef { prefix }
        | CompletionContext::LabelDefinition { prefix }
        | CompletionContext::CitationKey { prefix }
        | CompletionContext::GlossaryKey { prefix }
        | CompletionContext::FilePath { prefix, .. }
        | CompletionContext::PackageName { prefix, .. }
        | CompletionContext::ColorName { prefix }
        | CompletionContext::ColorModel { prefix }
        | CompletionContext::TikzLibrary { prefix, .. }
        | CompletionContext::ArgumentEnum { prefix, .. } => prefix,
        CompletionContext::None => "",
    }
}
pub(super) fn clear_prefix(context: &mut CompletionContext) {
    match context {
        CompletionContext::FilePath { prefix, .. } => {
            if let Some(slash) = prefix.rfind('/') {
                prefix.truncate(slash + 1);
            } else {
                prefix.clear();
            }
        }
        CompletionContext::Option { prefix, .. }
        | CompletionContext::CommandName { prefix }
        | CompletionContext::EnvironmentName { prefix, .. }
        | CompletionContext::LabelRef { prefix }
        | CompletionContext::LabelDefinition { prefix }
        | CompletionContext::CitationKey { prefix }
        | CompletionContext::GlossaryKey { prefix }
        | CompletionContext::PackageName { prefix, .. }
        | CompletionContext::ColorName { prefix }
        | CompletionContext::ColorModel { prefix }
        | CompletionContext::TikzLibrary { prefix, .. }
        | CompletionContext::ArgumentEnum { prefix, .. } => prefix.clear(),
        CompletionContext::None => {}
    }
}

#[cfg(test)]
pub(crate) fn prepare(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
    items: &mut Vec<CompletionItem>,
) -> (String, HashMap<String, u8>, bool, bool) {
    let context = (file_kind_for(path) != FileKind::Bib)
        .then(|| super::classify_tex_context(snapshot, path, offset));
    prepare_with_context(snapshot, path, offset, context.as_ref(), items)
}

pub(super) fn prepare_with_context(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
    context: Option<&CompletionContext>,
    items: &mut Vec<CompletionItem>,
) -> (String, HashMap<String, u8>, bool, bool) {
    let file = snapshot.lookup_file(path).expect("completion source");
    let mut relevance = HashMap::new();
    if file_kind_for(path) == FileKind::Bib {
        let root = snapshot.parsed_bib_tree(file);
        let query = if let Some((_, prefix)) =
            tex_ls_analysis::bib::completion::dependency_key_completion(&root, offset)
        {
            prefix
        } else if let Some(prefix) =
            tex_ls_analysis::bib::completion::tex_command_prefix(&root, offset)
        {
            prefix
        } else {
            match classify_bib_context(&root, offset) {
                BibCompletionContext::EntryType { prefix }
                | BibCompletionContext::FieldName { prefix, .. }
                | BibCompletionContext::ValueMacro { prefix } => prefix,
                BibCompletionContext::None => String::new(),
            }
        };
        add_glyphs(items, None);
        return (query, relevance, false, false);
    }
    let root = snapshot.parsed_tree(file);
    let context = context.expect("classified TeX completion");
    let query = prefix(context).to_owned();
    if matches!(
        context,
        CompletionContext::FilePath { .. } | CompletionContext::PackageName { .. }
    ) {
        let list = file_role_at(&root, offset).is_some_and(|role| role.list);
        items.retain(|item| tex_ls_analysis::external::is_literal_file_name(&item.label, list));
    }
    let members = snapshot.resolve_labels().namespace_members(path);
    if let CompletionContext::PackageName { kind, .. } = context {
        let kind = *kind;
        let installed: HashSet<_> = match kind {
            FileArgKind::NamedFile {
                prefix,
                suffix: ".sty",
            } => snapshot
                .texmf()
                .sty_stems()
                .iter()
                .filter_map(|name| name.strip_prefix(prefix))
                .collect(),
            FileArgKind::NamedFile { .. } => snapshot
                .texmf()
                .by_name
                .keys()
                .filter_map(|name| kind.name_from_filename(name))
                .collect(),
            FileArgKind::Class => snapshot
                .texmf()
                .cls_stems()
                .iter()
                .map(String::as_str)
                .collect(),
            _ => snapshot
                .texmf()
                .sty_stems()
                .iter()
                .map(String::as_str)
                .collect(),
        };
        let local: HashSet<_> = snapshot
            .read_dir(path.parent().unwrap_or(Path::new("")))
            .into_iter()
            .filter(|(name, directory)| {
                !directory
                    && kind
                        .extensions()
                        .iter()
                        .any(|extension| name.ends_with(&format!(".{extension}")))
            })
            .filter_map(|(name, _)| kind.name_from_filename(&name).map(str::to_owned))
            .collect();
        for item in items.iter() {
            relevance.insert(
                item.label.clone(),
                if local.contains(&item.label) {
                    0
                } else if installed.contains(item.label.as_str()) {
                    1
                } else {
                    2
                },
            );
        }
    }
    if matches!(context, CompletionContext::LabelRef { .. }) {
        let equation = tex_ls_parser::syntax::token_at_offset(&root, TextSize::from(offset as u32))
            .left_biased()
            .into_iter()
            .flat_map(|token| token.parent_ancestors())
            .filter(|node| node.kind() == SyntaxKind::COMMAND)
            .any(|node| tex_ls_parser::ast::command_name(&node).as_deref() == Some("eqref"));
        let aux = (!query.is_empty())
            .then(|| document_aux(snapshot, snapshot.resolve_labels(), path))
            .flatten();
        for item in items.iter_mut() {
            let context = snapshot.label_context_ref(path, &item.label);
            let number = aux
                .as_ref()
                .and_then(|aux| aux.labels.get(item.label.as_str()));
            if !query.is_empty() {
                set_label_details(item, context, number.map(String::as_str));
            }
            if equation {
                relevance.insert(
                    item.label.clone(),
                    if matches!(context, Some(LabelContext::Equation)) {
                        0
                    } else {
                        1
                    },
                );
            }
            item.kind = Some(match context {
                Some(LabelContext::Equation) => CompletionItemKind::Value,
                Some(LabelContext::Section { .. }) => CompletionItemKind::Module,
                Some(LabelContext::Float { .. }) => CompletionItemKind::Struct,
                _ => CompletionItemKind::Reference,
            });
        }
    }
    if matches!(
        context,
        CompletionContext::CommandName { .. } | CompletionContext::EnvironmentName { .. }
    ) {
        if !query.is_empty() {
            let matcher = completion_rank::MatchQuery::new(&query);
            items.retain(|item| matcher.score_item(item).is_some());
        }
        let environment = matches!(context, CompletionContext::EnvironmentName { .. });
        let mut packages = HashSet::new();
        let mut package_members: std::collections::BTreeSet<PathBuf> =
            members.iter().map(|path| path.to_path_buf()).collect();
        for member in &members {
            package_members.extend(snapshot.package_graph().transitively_loaded(member));
        }
        for member in &package_members {
            for load in snapshot.package_graph().loads(member) {
                if let Some(name) = load.to.file_stem().and_then(|name| name.to_str()) {
                    packages.insert(name.to_owned());
                }
            }
        }
        for load in snapshot
            .package_graph()
            .unresolved()
            .iter()
            .filter(|load| package_members.contains(&load.from))
        {
            if let tex_ls_analysis::project::package::PackageTarget::Path(path) = &load.target
                && let Some(name) = path.file_stem().and_then(|name| name.to_str())
            {
                packages.insert(name.to_owned());
            }
        }
        let symbols = snapshot.editor_symbols(file);
        for item in items.iter() {
            let name = item.label.as_str();
            let project = if environment {
                symbols.environments.contains(name)
            } else {
                symbols.commands.contains(name)
            };
            let loaded = (if environment {
                cwl().environment_packages(name)
            } else {
                cwl().command_packages(name)
            })
            .iter()
            .any(|package| packages.contains(*package));
            let kernel = if environment {
                builtin().environment(name).is_some()
            } else {
                builtin().command(name).is_some()
            };
            relevance.insert(
                item.label.clone(),
                if project || loaded {
                    0
                } else if kernel {
                    1
                } else {
                    2
                },
            );
        }
        let mut seen: HashSet<_> = items.iter().map(|item| item.label.clone()).collect();
        for member in &members {
            let Some(source) = snapshot.lookup_file(member) else {
                continue;
            };
            let occurrences = snapshot.name_occurrences(source);
            let names: Vec<_> = if environment {
                occurrences.environment_names().collect()
            } else {
                occurrences.command_names().collect()
            };
            for name in names {
                let ranges = if environment {
                    occurrences.environments(name)
                } else {
                    occurrences.commands(name)
                };
                if *member == path
                    && !ranges
                        .iter()
                        .any(|range| !range.contains_inclusive(TextSize::from(offset as u32)))
                {
                    continue;
                }
                if seen.insert(name.to_owned()) {
                    relevance.insert(name.to_owned(), 3);
                    items.push(CompletionItem {
                        label: name.into(),
                        kind: Some(if environment {
                            CompletionItemKind::Class
                        } else {
                            CompletionItemKind::Function
                        }),
                        detail: Some("Observed in project; no known declaration".into()),
                        ..Default::default()
                    });
                }
            }
        }
        if matches!(
            context,
            CompletionContext::EnvironmentName { closing: true, .. }
        ) {
            let at = TextSize::from(offset as u32);
            if let Some(environment) = tex_ls_analysis::completion::cursor_ancestors(&root, offset)
                .into_iter()
                .filter_map(Environment::cast)
                .filter(|env| env.syntax().text_range().contains_inclusive(at))
                .min_by_key(|env| env.syntax().text_range().len())
                && let Some(name) = environment.name()
                && let Some(item) = items.iter_mut().find(|item| item.label == name.as_str())
            {
                item.preselect = Some(true);
                relevance.insert(item.label.clone(), 0);
            }
        }
        if matches!(context, CompletionContext::CommandName { .. }) {
            for name in ["begin", "end"] {
                if !items.iter().any(|item| item.label == name) {
                    items.push(CompletionItem {
                        label: name.into(),
                        kind: Some(CompletionItemKind::Function),
                        ..Default::default()
                    });
                    relevance.insert(name.into(), 1);
                }
            }
        }
        add_glyphs(items, Some(&symbols.signatures));
        if matches!(context, CompletionContext::CommandName { .. })
            && snapshot.file_text(file)[offset..].trim().is_empty()
        {
            if let Some(item) = items.iter_mut().find(|item| item.label == "begin") {
                item.insert_text = Some("begin{${1:environment}}\n\t$0\n\\\\end{$1}".into());
                item.insert_text_format = Some(InsertTextFormat::Snippet);
                relevance.insert(item.label.clone(), 0);
            }
            // Pair insertion is restricted to a source tail needing new structure.
            let text_mode = snapshot.file_modes(file).mode_at(offset.saturating_sub(1))
                == tex_ls_parser::semantic::mode::Mode::Text;
            for (open, close) in [("(", ")"), ("[", "]")].into_iter().filter(|_| text_mode) {
                relevance.insert(open.into(), 0);
                items.push(CompletionItem {
                    label: open.into(),
                    kind: Some(CompletionItemKind::Snippet),
                    insert_text: Some(format!("{open}$0\\\\{close}")),
                    insert_text_format: Some(InsertTextFormat::Snippet),
                    ..Default::default()
                });
            }
        }
    }
    let file_path = matches!(context, CompletionContext::FilePath { .. });
    let deferred_label_details =
        matches!(context, CompletionContext::LabelRef { .. }) && query.is_empty();
    (
        if file_path {
            query.rsplit('/').next().unwrap_or(&query).into()
        } else {
            query
        },
        relevance,
        file_path,
        deferred_label_details,
    )
}

fn set_label_details(
    item: &mut CompletionItem,
    context: Option<&LabelContext>,
    number: Option<&str>,
) {
    item.detail = crate::hover::render_label_markdown(context, number);
    item.filter_text = Some(format!(
        "{} {}",
        item.label,
        item.detail.as_deref().unwrap_or("")
    ));
}

pub(super) fn enrich_label_details(snapshot: &Analysis, path: &Path, items: &mut [CompletionItem]) {
    let aux = document_aux(snapshot, snapshot.resolve_labels(), path);
    for item in items {
        let context = snapshot.label_context_ref(path, &item.label);
        let number = aux
            .as_ref()
            .and_then(|aux| aux.labels.get(item.label.as_str()));
        set_label_details(item, context, number.map(String::as_str));
    }
}
fn add_glyphs(items: &mut [CompletionItem], scope: Option<&SignatureDb>) {
    for item in items {
        if item.kind == Some(CompletionItemKind::Function)
            && !scope.is_some_and(|scope| scope.command(&item.label).is_some())
            && let Some(glyph) = tex_ls_parser::semantic::math::command_glyph(&item.label)
        {
            item.detail = Some(
                format!("{glyph}  {}", item.detail.as_deref().unwrap_or(""))
                    .trim_end()
                    .into(),
            );
        }
    }
}
