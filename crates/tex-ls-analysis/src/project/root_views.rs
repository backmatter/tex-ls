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
                .chain(graph.subfile_children(path).iter().cloned())
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
        let mut claimed: std::collections::HashSet<_> = scopes.iter().flatten().cloned().collect();
        // Add unclaimed fragments, starting with parentless files. A root-relative
        // include can claim a file that has no source-relative incoming edge.
        for path in paths
            .iter()
            .filter(|path| !incoming.contains(*path))
            .chain(paths.iter())
        {
            if !claimed.contains(path) {
                seeds.push(path.clone());
                let scope = reach(path);
                claimed.extend(scope.iter().cloned());
                scopes.push(scope);
            }
        }
        let mut roots: HashMap<_, Vec<_>> = paths
            .iter()
            .map(|path| (path.clone(), Vec::new()))
            .collect();
        for (seed, scope) in seeds.iter().zip(&scopes) {
            for path in scope {
                if let Some(candidates) = roots.get_mut(path) {
                    candidates.push(seed.clone());
                }
            }
        }
        let seed_indices: HashMap<_, _> = seeds
            .iter()
            .enumerate()
            .map(|(index, path)| (path.clone(), index))
            .collect();
        let mut component_of = HashMap::new();
        let mut representatives = seeds.clone();
        for path in &paths {
            let candidates = &roots[path];
            let id = if let Some(&id) = seed_indices.get(path) {
                id
            } else if candidates.len() == 1 {
                seed_indices[&candidates[0]]
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
        .map(|root_file| crate::incremental::root_graphics(db, *root_file).clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{
        graph::FileFacts,
        include::{IncludeEdgeKey, IncludeKind, IncludeTarget},
    };

    fn graph(files: &[(&str, &[&str])]) -> IncludeGraph {
        let facts: Vec<_> = files
            .iter()
            .map(|(path, targets)| FileFacts {
                path: PathBuf::from(path),
                include_only: Default::default(),
                include_edges: targets
                    .iter()
                    .map(|target| IncludeEdgeKey {
                        literal: None,
                        build_participation: Some(true),
                        kind: IncludeKind::Input,
                        target: IncludeTarget::Path(PathBuf::from(target)),
                    })
                    .collect(),
            })
            .collect();
        IncludeGraph::build(&facts, None)
    }

    #[test]
    fn shared_children_keep_ordered_roots_and_independent_default_views() {
        let files: &[(&str, &[&str])] = &[
            ("/p/b.tex", &["/p/shared.tex"]),
            ("/p/a.tex", &["/p/shared.tex"]),
            ("/p/shared.tex", &["/p/leaf.tex"]),
            ("/p/leaf.tex", &[]),
            ("/p/fragment.tex", &[]),
        ];
        let views = RootViews::build(
            files.iter().map(|(path, _)| Path::new(path)),
            &graph(files),
            [Path::new("/p/b.tex"), Path::new("/p/a.tex")],
        );
        let roots = vec![PathBuf::from("/p/a.tex"), PathBuf::from("/p/b.tex")];
        for child in ["/p/shared.tex", "/p/leaf.tex"] {
            assert_eq!(views.roots[Path::new(child)], roots);
        }
        assert_eq!(
            views.representatives,
            [
                "/p/a.tex",
                "/p/b.tex",
                "/p/fragment.tex",
                "/p/leaf.tex",
                "/p/shared.tex"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(views.count, 5);
        assert_eq!(
            views.scopes[views.component_of[Path::new("/p/leaf.tex")]],
            [PathBuf::from("/p/leaf.tex")]
        );
        assert_eq!(
            views.scopes[views.component_of[Path::new("/p/shared.tex")]],
            [PathBuf::from("/p/leaf.tex"), PathBuf::from("/p/shared.tex")]
        );
        assert!(views.closed.iter().all(|&closed| closed));
    }

    #[test]
    fn a_rootless_cycle_keeps_one_deterministic_view() {
        let files: &[(&str, &[&str])] = &[("/p/b.tex", &["/p/a.tex"]), ("/p/a.tex", &["/p/b.tex"])];
        let views = RootViews::build(
            files.iter().map(|(path, _)| Path::new(path)),
            &graph(files),
            [],
        );
        assert_eq!(views.count, 1);
        assert_eq!(views.representatives, [PathBuf::from("/p/a.tex")]);
        assert_eq!(
            views.component_of[Path::new("/p/a.tex")],
            views.component_of[Path::new("/p/b.tex")]
        );
    }

    #[test]
    fn many_independent_sources_keep_their_own_views() {
        let paths: Vec<_> = (0..1000)
            .map(|index| format!("/p/{index:04}.tex"))
            .collect();
        let files: Vec<(&str, &[&str])> = paths
            .iter()
            .rev()
            .map(|path| (path.as_str(), &[][..]))
            .collect();
        let views = RootViews::build(
            files.iter().map(|(path, _)| Path::new(path)),
            &graph(&files),
            [],
        );
        assert_eq!(views.count, paths.len());
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(views.component_of[Path::new(path)], index);
            assert_eq!(views.roots[Path::new(path)], [PathBuf::from(path)]);
            assert_eq!(views.scopes[index], [PathBuf::from(path)]);
        }
    }
}
