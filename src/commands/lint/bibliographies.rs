//! Acquire referenced bibliography sources without adding lint targets.
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};
use tex_ls_analysis::{
    project::{
        BibTarget, FileFacts, IncludeGraph, ResolvedLabels, collect_bib_resource_targets,
        collect_include_edge_keys, labels::is_document_root,
    },
    source::FileKind,
};
use tex_ls_parser::{
    declarations::ResolvedDeclarations, parser::parse_with_declarations, semantic::SemanticModel,
    syntax::SyntaxNode,
};

#[derive(Default)]
pub(super) struct SupplementalBibliographies {
    pub(super) sources: Vec<(PathBuf, String)>,
    pub(super) aliases: HashMap<PathBuf, PathBuf>,
}

/// Load literal bibliography resources that were not among the user's lint
/// inputs. These files feed citation resolution but are not themselves lint
/// targets, so a system-wide database cannot unexpectedly add standalone
/// BibTeX findings to `tex-ls lint doc.tex`.
pub(super) fn supplemental_bibliographies(
    sources: &[(PathBuf, String, FileKind)],
    declared: &ResolvedDeclarations,
) -> SupplementalBibliographies {
    let mut out = SupplementalBibliographies::default();
    let mut loaded: BTreeSet<PathBuf> = sources.iter().map(|(path, _, _)| path.clone()).collect();

    let analyzed: Vec<_> = sources
        .iter()
        .filter_map(|(path, content, kind)| {
            if !kind.is_latex() {
                return None;
            }
            let parsed = parse_with_declarations(content, kind.lex_config(), declared);
            let root = SyntaxNode::new_root(parsed.green);
            let model = SemanticModel::build(&root);
            Some((path, root, model))
        })
        .collect();
    let graph = IncludeGraph::build(
        &analyzed
            .iter()
            .map(|(path, root, model)| FileFacts {
                path: (*path).clone(),
                include_edges: collect_include_edge_keys(root, path.parent()),
                include_only: model.include_only().clone(),
            })
            .collect::<Vec<_>>(),
        None,
    );
    let labels = ResolvedLabels::build(
        &analyzed
            .iter()
            .map(|(path, root, _)| {
                (
                    (*path).clone(),
                    Vec::new(),
                    Vec::new(),
                    is_document_root(root),
                )
            })
            .collect::<Vec<_>>(),
        &graph,
    );
    for (path, root, _) in &analyzed {
        let base_dir = path.parent().filter(|base| !base.as_os_str().is_empty());
        let mut bases = Vec::new();
        for compilation_root in labels.candidate_roots(path) {
            bases.extend(
                graph
                    .root_contexts(compilation_root)
                    .remove(*path)
                    .unwrap_or_default(),
            );
        }
        if bases.is_empty() {
            bases.extend(base_dir.map(Path::to_path_buf));
        }
        for target in tex_ls_analysis::project::citations::contextual_targets(
            &collect_bib_resource_targets(root, None),
            &bases,
        ) {
            let BibTarget::Path(requested) = target else {
                continue;
            };
            if loaded.contains(&requested) {
                continue;
            }
            let Some(actual) =
                tex_ls::bibliography::resolve_bibliography_file(&requested, base_dir)
            else {
                continue;
            };
            if actual != requested {
                out.aliases.insert(requested, actual.clone());
            }
            if !loaded.insert(actual.clone()) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&actual) {
                out.sources.push((actual, content));
            }
        }
    }
    out
}
