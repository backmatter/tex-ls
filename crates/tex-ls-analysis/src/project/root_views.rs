//! Value-comparable include connectivity shared by label and citation queries.
use super::graph::IncludeGraph;
use crate::incremental::{IncrementalDb, ProjectInput, QueryKind, QueryLogEntry};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Directed document views. A shared source belongs to each reaching root;
/// its default view is local when no unique root can be selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RootViews {
    pub component_of: HashMap<PathBuf, usize>,
    pub scopes: Vec<Vec<PathBuf>>,
    pub roots: HashMap<PathBuf, Vec<PathBuf>>,
    pub representatives: Vec<PathBuf>,
    pub count: usize,
    pub closed: Vec<bool>,
    pub contexts: HashMap<PathBuf, std::collections::BTreeMap<PathBuf, Vec<PathBuf>>>,
}
impl RootViews {
    pub fn build<'a>(
        members: impl IntoIterator<Item = &'a Path>,
        graph: &IncludeGraph,
        declared_roots: impl IntoIterator<Item = &'a Path>,
    ) -> Self {
        let mut paths: Vec<PathBuf> = members.into_iter().map(Path::to_path_buf).collect();
        paths.sort_unstable();
        paths.dedup();
        let known: std::collections::HashSet<_> = paths.iter().cloned().collect();
        let children = |path: &Path| {
            graph
                .outgoing(path)
                .iter()
                .filter(|edge| edge.kind != super::include::IncludeKind::SubFilesParent)
                .map(|edge| edge.to.clone())
                .chain(
                    graph
                        .included_by(path)
                        .iter()
                        .filter(|child| {
                            graph.outgoing(child).iter().any(|edge| {
                                edge.to == path
                                    && edge.kind == super::include::IncludeKind::SubFilesParent
                            })
                        })
                        .cloned(),
                )
                .filter(|path| known.contains(path))
                .collect::<Vec<_>>()
        };
        let mut incoming = std::collections::HashSet::new();
        for path in &paths {
            incoming.extend(children(path));
        }
        let mut seeds: Vec<_> = declared_roots
            .into_iter()
            .map(Path::to_path_buf)
            .filter(|path| {
                known.contains(path)
                    && !graph
                        .outgoing(path)
                        .iter()
                        .any(|edge| edge.kind == super::include::IncludeKind::SubFilesParent)
            })
            .collect();
        seeds.sort_unstable();
        seeds.dedup();
        let mut contexts = HashMap::new();
        let mut reach = |seed: &Path| {
            let contexts = contexts
                .entry(seed.to_path_buf())
                .or_insert_with(|| graph.root_contexts(seed));
            contexts.keys().cloned().collect::<Vec<_>>()
        };
        let mut scopes: Vec<_> = seeds.iter().map(|seed| reach(seed)).collect();
        // Add unclaimed fragments, starting with parentless files. A root-relative
        // include can claim a file that has no source-relative incoming edge.
        for path in paths
            .iter()
            .filter(|path| !incoming.contains(*path))
            .chain(paths.iter())
        {
            if !scopes.iter().any(|scope| scope.contains(path)) {
                seeds.push(path.clone());
                scopes.push(reach(path));
            }
        }
        let roots: HashMap<_, Vec<_>> = paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    scopes
                        .iter()
                        .enumerate()
                        .filter(|(_, scope)| scope.contains(path))
                        .map(|(id, _)| seeds[id].clone())
                        .collect(),
                )
            })
            .collect();
        let mut component_of = HashMap::new();
        let mut representatives = seeds.clone();
        for path in &paths {
            let candidates = &roots[path];
            let id = if let Some(id) = seeds.iter().position(|seed| seed == path) {
                id
            } else if candidates.len() == 1 {
                seeds
                    .iter()
                    .position(|seed| seed == &candidates[0])
                    .unwrap()
            } else {
                // Do not union independent roots for an ambiguous shared child.
                let id = scopes.len();
                scopes.push(reach(path));
                representatives.push(path.clone());
                id
            };
            component_of.insert(path.clone(), id);
        }
        Self {
            closed: representatives
                .iter()
                .map(|root| graph.interpret(root).is_closed())
                .collect(),
            count: scopes.len(),
            scopes,
            roots,
            representatives,
            component_of,
            contexts,
        }
    }
}

#[salsa::tracked(returns(ref))]
pub(super) fn include_components(db: &dyn IncrementalDb, context: ProjectInput) -> RootViews {
    db.record_query(QueryLogEntry {
        kind: QueryKind::IncludeComponents,
        file: None,
    });
    let project = super::workspace_project(db, context);
    RootViews::build(
        project
            .members
            .iter()
            .filter(|member| member.kind.is_latex())
            .map(|member| member.path.as_path()),
        super::project_graph(db, context),
        project_sources(db, context)
            .iter()
            .filter(|(_, source)| *crate::incremental::file_is_document_root(db, **source))
            .map(|(path, _)| path.as_path()),
    )
}

/// One path index per membership revision, reused by component and signature queries.
#[salsa::tracked(returns(ref))]
pub(crate) fn project_sources(
    db: &dyn IncrementalDb,
    context: ProjectInput,
) -> HashMap<PathBuf, crate::incremental::SourceInput> {
    super::workspace_project(db, context)
        .members
        .iter()
        .map(|member| {
            (
                member.path.clone(),
                db.source_input(member.file, *context.identity(db)),
            )
        })
        .collect()
}

pub(super) fn representatives(
    db: &dyn IncrementalDb,
    context: ProjectInput,
) -> Vec<crate::incremental::SourceInput> {
    let partition = include_components(db, context);
    let sources = project_sources(db, context);
    partition
        .representatives
        .iter()
        .map(|path| sources[path])
        .collect()
}

#[salsa::tracked(returns(ref))]
pub(super) fn members(
    db: &dyn IncrementalDb,
    representative: crate::incremental::SourceInput,
) -> Vec<crate::incremental::SourceInput> {
    let context = db.project_input(*representative.project(db));
    let partition = include_components(db, context);
    let id = partition.component_of[representative.path(db)];
    let sources = project_sources(db, context);
    let mut members: Vec<_> = partition.scopes[id]
        .iter()
        .map(|path| sources[path])
        .collect();
    members.sort_by(|a, b| a.path(db).cmp(b.path(db)));
    members
}

#[salsa::tracked]
pub(super) fn closed(
    db: &dyn IncrementalDb,
    representative: crate::incremental::SourceInput,
) -> bool {
    let context = db.project_input(*representative.project(db));
    let views = include_components(db, context);
    views.closed[views.component_of[representative.path(db)]]
}

/// One proved source/import context, or no context for a multi-root source.
#[salsa::tracked(returns(ref))]
pub(crate) fn file_context(
    db: &dyn IncrementalDb,
    file: crate::incremental::SourceInput,
) -> Option<crate::external::FilePathContext> {
    let context = db.project_input(*file.project(db));
    let views = include_components(db, context);
    let path = file.path(db);
    let candidates = views.roots.get(path)?;
    let [root] = candidates.as_slice() else {
        return None;
    };
    let bases = views.contexts.get(root)?.get(path)?.clone();
    let root_directory = root.parent().unwrap_or(Path::new("")).to_path_buf();
    let graphics = project_sources(db, context)
        .get(root)
        .map(|root_file| {
            crate::external::inherited_graphics(
                &crate::incremental::parsed_tree_root(db, *root_file),
                &root_directory,
            )
        })
        .unwrap_or_default();
    Some(crate::external::FilePathContext {
        root_directory,
        bases,
        graphics,
    })
}

/// Cached contexts from the same root interpretation used by namespace views.
pub(crate) fn contexts<'a>(
    db: &'a dyn IncrementalDb,
    project: ProjectInput,
    root: &Path,
) -> Option<&'a std::collections::BTreeMap<PathBuf, Vec<PathBuf>>> {
    include_components(db, project).contexts.get(root)
}
