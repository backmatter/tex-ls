//! Project analysis over source text acquired by the lint command.
use super::bibliographies::SupplementalBibliographies;
use rayon::prelude::*;
use rowan::GreenNode;
use smol_str::SmolStr;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tex_ls_analysis::{
    linter::{Diagnostic, lint_document_with_kind},
    project::{
        CiteFileFacts, FileFacts, IncludeGraph, PackageOptionFacts, ResolvedCitations,
        ResolvedLabels, ResolvedPackageOptions, collect_bib_resource_targets,
        collect_include_edge_keys,
        labels::{document_label_names, document_ref_names, is_document_root},
        package_option_facts,
    },
    source::FileKind,
};
use tex_ls_parser::{
    declarations::ResolvedDeclarations, parser::parse_with_declarations, semantic::SemanticModel,
    syntax::SyntaxNode,
};

enum FileAnalysis {
    Bib {
        diagnostics: Vec<Diagnostic>,
        path: PathBuf,
        keys: Vec<SmolStr>,
        green: GreenNode,
        model: Box<tex_ls_analysis::bib::semantic::Model>,
    },
    // Boxed: the `.tex` payload is far larger than the `.bib` one, so an unboxed
    // variant would bloat every `FileAnalysis` to its size.
    Tex(Box<TexAnalysis>),
}

/// The `.tex`/`.sty`/… parse+analyze payload carried by [`FileAnalysis::Tex`].
struct TexAnalysis {
    diagnostics: Vec<Diagnostic>,
    path: PathBuf,
    kind: FileKind,
    green: GreenNode,
    model: SemanticModel,
    facts: FileFacts,
    label_input: (PathBuf, Vec<SmolStr>, Vec<SmolStr>, bool),
    cite_fact: CiteFileFacts,
    /// The file's declared-option surface when it is a `.sty`, feeding the
    /// cross-file package-option model (`unknown-option`).
    option_facts: Option<PackageOptionFacts>,
}

/// Parse and analyze one source. Pure and thread-safe (no shared mutable state,
/// no environment access), so [`super::run_lint`] maps it over all files with rayon. The
/// resolver-feeding facts use the same pure helpers the salsa queries do, so CLI
/// and LSP agree.
fn analyze_source(
    path: &Path,
    content: &str,
    kind: FileKind,
    declared: &ResolvedDeclarations,
) -> FileAnalysis {
    match kind {
        FileKind::Bib => {
            // Build the model once: it yields both the lint diagnostics and the
            // cite keys this `.bib` contributes to the citation resolver.
            let parsed = tex_ls_analysis::bib::parse(content);
            let diagnostics: Vec<Diagnostic> = parsed
                .errors
                .iter()
                .map(|err| Diagnostic {
                    rule: "parse",
                    severity: tex_ls_analysis::linter::Severity::Error,
                    path: path.to_path_buf(),
                    start: err.start,
                    end: err.end,
                    message: err.message.clone(),
                    fix: None,
                    related: Vec::new(),
                })
                .collect();
            let root = parsed.syntax();
            let model = tex_ls_analysis::bib::semantic::Model::build(&root);
            let keys = model.entries().iter().map(|e| e.key.clone()).collect();
            FileAnalysis::Bib {
                diagnostics,
                path: path.to_path_buf(),
                keys,
                green: parsed.green,
                model: Box::new(model),
            }
        }
        FileKind::Tex
        | FileKind::CodeTex
        | FileKind::Sty
        | FileKind::Cls
        | FileKind::Dtx
        | FileKind::Ins => {
            let parsed = parse_with_declarations(content, kind.lex_config(), declared);
            let diagnostics: Vec<Diagnostic> = parsed
                .errors
                .iter()
                .map(|err| Diagnostic::from_parse(path.to_path_buf(), err))
                .collect();
            let green = parsed.green;
            let root = SyntaxNode::new_root(green.clone());
            let model = SemanticModel::build_with_declarations(&root, declared);
            let facts = FileFacts {
                include_only: tex_ls_parser::semantic::roles::include_only(&root),
                path: path.to_path_buf(),
                include_edges: collect_include_edge_keys(&root, path.parent()),
            };
            let label_input = (
                path.to_path_buf(),
                document_label_names(&model),
                document_ref_names(&model),
                is_document_root(&root),
            );
            let cite_fact = CiteFileFacts {
                case_sensitive: tex_ls_analysis::project::citations::citation_case_sensitive(&root),
                path: path.to_path_buf(),
                bib_targets: collect_bib_resource_targets(&root, None),
                manual_keys: model
                    .bibitems()
                    .iter()
                    .map(|item| item.name.clone())
                    .collect(),
                nocite_all: model.has_wildcard_nocite(),
                is_document_root: is_document_root(&root),
            };
            let option_facts = package_option_facts(path, &root, &model);
            FileAnalysis::Tex(Box::new(TexAnalysis {
                diagnostics,
                path: path.to_path_buf(),
                kind,
                green,
                model,
                facts,
                label_input,
                cite_fact,
                option_facts,
            }))
        }
    }
}

/// Collect parse and lint findings using project label and citation resolution.
pub(super) fn collect_project_diagnostics_with_bibliographies(
    sources: &[(PathBuf, String, FileKind)],
    declared: &ResolvedDeclarations,
    supplemental: &SupplementalBibliographies,
) -> Vec<Diagnostic> {
    // Phase 1 — parse + analyze every source in parallel. Each task is pure and
    // returns only `Send` data (`analyze_source`); rayon preserves input order.
    let analyses: Vec<FileAnalysis> = sources
        .par_iter()
        .map(|(path, content, kind)| analyze_source(path, content, *kind, declared))
        .collect();

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut analyzed: Vec<(PathBuf, FileKind, GreenNode, SemanticModel)> = Vec::new();
    let mut bib_analyzed = Vec::new();
    let mut supplemental_models = Vec::new();
    let mut facts: Vec<FileFacts> = Vec::new();
    let mut label_inputs = Vec::new();
    let mut cite_facts: Vec<CiteFileFacts> = Vec::new();
    let mut option_facts: Vec<PackageOptionFacts> = Vec::new();
    // Cite keys per analyzed `.bib` path, feeding the cross-file citation resolver.
    let mut bib_keys: HashMap<PathBuf, Vec<SmolStr>> = HashMap::new();
    for analysis in analyses {
        match analysis {
            FileAnalysis::Bib {
                diagnostics: d,
                path,
                keys,
                green,
                model,
            } => {
                diagnostics.extend(d);
                bib_keys.insert(path.clone(), keys);
                bib_analyzed.push((path, green, model));
            }
            FileAnalysis::Tex(tex) => {
                let TexAnalysis {
                    diagnostics: d,
                    path,
                    kind,
                    green,
                    model,
                    facts: f,
                    label_input,
                    cite_fact,
                    option_facts: o,
                } = *tex;
                diagnostics.extend(d);
                facts.push(f);
                label_inputs.push(label_input);
                cite_facts.push(cite_fact);
                option_facts.extend(o);
                analyzed.push((path, kind, green, model));
            }
        }
    }
    for (path, content) in &supplemental.sources {
        let parsed = tex_ls_analysis::bib::parse(content);
        let model = tex_ls_analysis::bib::semantic::Model::build(&parsed.syntax());
        bib_keys.insert(
            path.clone(),
            model
                .entries()
                .iter()
                .map(|entry| entry.key.clone())
                .collect(),
        );
        supplemental_models.push((path.clone(), model));
    }

    // Phase 2 — cross-file resolution: a serial barrier (needs the whole analyzed
    // set) over the collected facts. Pure graph work, no re-parsing.
    let graph = IncludeGraph::build(&facts, None);
    let resolved = ResolvedLabels::build(&label_inputs, &graph);
    let resolved_citations = ResolvedCitations::build_with_aliases(
        &cite_facts,
        &graph,
        &bib_keys,
        &supplemental.aliases,
    )
    .with_entry_aliases(
        bib_analyzed
            .iter()
            .map(|(path, _, model)| (path.as_path(), model.as_ref()))
            .chain(
                supplemental_models
                    .iter()
                    .map(|(path, model)| (path.as_path(), model)),
            ),
    )
    .open_bibliographies(
        bib_analyzed
            .iter()
            .filter(|(_, _, model)| {
                !tex_ls_analysis::project::citations::document_cite_identity_complete(model)
            })
            .map(|(path, _, _)| path.as_path())
            .chain(
                supplemental_models
                    .iter()
                    .filter(|(_, model)| {
                        !tex_ls_analysis::project::citations::document_cite_identity_complete(model)
                    })
                    .map(|(path, _)| path.as_path()),
            ),
    );
    let resolved_packages = ResolvedPackageOptions::build(option_facts);
    for (path, green, model) in &bib_analyzed {
        let project = tex_ls_analysis::bib::linter::project::ProjectFacts::build(
            path,
            &resolved_citations,
            analyzed
                .iter()
                .map(|(path, _, _, model)| (path.as_path(), model)),
            bib_analyzed
                .iter()
                .map(|(path, _, model)| (path.as_path(), model.as_ref()))
                .chain(
                    supplemental_models
                        .iter()
                        .map(|(path, model)| (path.as_path(), model)),
                ),
        );
        diagnostics.extend(tex_ls_analysis::bib::linter::lint_document_with_project(
            path,
            &tex_ls_analysis::bib::syntax::SyntaxNode::new_root(green.clone()),
            model,
            Some(&project),
        ));
    }

    // Phase 3 — lint every analyzed file in parallel, sharing the resolution by
    // reference. The red tree is materialized thread-locally from each green node
    // (red trees are not `Send`).
    let command_names: HashMap<_, Vec<_>> = analyzed
        .iter()
        .map(|(path, _, green, _)| {
            let names = tex_ls_parser::semantic::scan_definition_sites(&SyntaxNode::new_root(
                green.clone(),
            ))
            .into_iter()
            .filter(|site| site.kind == tex_ls_parser::semantic::DefSiteKind::Command)
            .map(|site| site.name)
            .collect();
            (path.clone(), names)
        })
        .collect();
    let package_facts: Vec<_> = analyzed
        .iter()
        .map(
            |(path, _, green, _)| tex_ls_analysis::project::PackageFileFacts {
                path: path.clone(),
                package_edges: tex_ls_analysis::project::collect_package_edge_keys(
                    &SyntaxNode::new_root(green.clone()),
                    path.parent(),
                ),
            },
        )
        .collect();
    let command_packages = tex_ls_analysis::project::PackageGraph::build(&package_facts);
    let lint_results: Vec<Vec<Diagnostic>> = analyzed
        .par_iter()
        .map(|(path, kind, green, model)| {
            let root = SyntaxNode::new_root(green.clone());
            let mut findings = lint_document_with_kind(
                path,
                *kind,
                &root,
                model,
                Some(&resolved),
                Some(&resolved_citations),
                Some(&resolved_packages),
            );
            let scope =
                tex_ls_analysis::name_refs::macro_namespace(&resolved, &command_packages, path);
            tex_ls_analysis::linter::rules::unknown_command::suppress_defined(
                &mut findings,
                &green.to_string(),
                scope
                    .iter()
                    .filter_map(|path| command_names.get(path))
                    .flatten()
                    .map(|name| name.as_str())
                    .chain(declared.command_names()),
            );
            tex_ls_analysis::linter::rules::unknown_command::suppress_from_packages(
                &mut findings,
                &green.to_string(),
                package_facts
                    .iter()
                    .filter(|fact| scope.contains(&fact.path))
                    .flat_map(|fact| &fact.package_edges),
            );
            findings
        })
        .collect();
    for result in lint_results {
        diagnostics.extend(result);
    }
    diagnostics
}
