//! The cross-file inclusion graph: which files pull in which, assembled from the
//! per-file `crate::incremental::include_edges` firewall and wrapped as a
//! tracked salsa query so a body edit doesn't rebuild the whole graph.
//!
//! The layering, from the per-file firewall up:
//!
//! - `crate::incremental::include_edges` — a per-file projection (range-free
//!   [`IncludeEdgeKey`]s) that stays *equal* across a body edit (salsa backdates).
//! - `project_graph` — assembles those into the cross-file [`IncludeGraph`],
//!   built on the backdated `workspace_project` membership snapshot, so an
//!   unchanged project + backdated per-file facts means its memo is reused.
//!
//! [`IncludeGraph::build`] is the **pure** algorithm (no salsa, no disk); the
//! salsa query is a thin wrapper. Root views interpret include connectivity for
//! label and citation namespaces, while package dependency post-order supplies
//! the signature overlay order. Both traversals keep their state off the call
//! stack, independent of the parser's syntax-depth bound.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::incremental::{
    IncrementalDb, ProjectInput, QueryKind, QueryLogEntry, SourceId, include_edges, package_edges,
};
use crate::project::include::{IncludeEdgeKey, IncludeKind, IncludeTarget};
use crate::project::package::{PackageEdgeKey, PackageKind, PackageTarget, dtx_source_of};
use crate::source::{FileKind, file_kind_or_tex};

/// One file's contribution to the inclusion graph: its path and the range-free
/// inclusion edges it declares.
#[derive(Debug, Clone)]
pub struct FileFacts {
    pub include_only: tex_ls_parser::semantic::roles::IncludeOnly,
    pub path: PathBuf,
    pub include_edges: Vec<IncludeEdgeKey>,
}

/// A dependency interpreted in one compilation root and import context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootEdge {
    pub from: PathBuf,
    pub target: Option<PathBuf>,
    pub resolved: bool,
    pub kind: IncludeKind,
    pub build_participation: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootInterpretation {
    pub contexts: std::collections::BTreeMap<PathBuf, Vec<PathBuf>>,
    pub edges: Vec<RootEdge>,
}
impl RootInterpretation {
    pub fn is_closed(&self) -> bool {
        self.edges.iter().all(|edge| edge.resolved)
    }
}

/// A resolved inclusion edge: `from` includes `to` (a path within the analyzed
/// member set) via `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInclude {
    pub literal: Option<PathBuf>,
    pub build_participation: Option<bool>,
    pub from: PathBuf,
    pub to: PathBuf,
    pub kind: IncludeKind,
}

/// An inclusion edge that could not be resolved to an analyzed member: a dynamic
/// target, or a literal path to a file outside the set we were given. Callers
/// must stay conservative about such files (their contents are opaque).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedInclude {
    pub literal: Option<PathBuf>,
    pub from: PathBuf,
    pub target: IncludeTarget,
    pub kind: IncludeKind,
}

/// The inclusion graph over a set of files.
///
/// Value-comparable so unchanged graph results backdate dependent queries.
/// Built by [`IncludeGraph::build`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IncludeGraph {
    aliases: HashMap<PathBuf, PathBuf>,
    include_only: HashMap<PathBuf, tex_ls_parser::semantic::roles::IncludeOnly>,
    /// Per file: resolved outgoing edges, in source order.
    edges: HashMap<PathBuf, Vec<ResolvedInclude>>,
    /// Reverse map: included file → the files that include it.
    included_by: HashMap<PathBuf, Vec<PathBuf>>,
    /// Reverse parent relationships, retaining the reverse-edge visit order.
    subfile_children: HashMap<PathBuf, Vec<PathBuf>>,
    /// Edges that resolve to nothing in the analyzed set (dynamic or external).
    unresolved: Vec<UnresolvedInclude>,
    /// Per-file positions in `unresolved`, preserving source order.
    unresolved_by_source: HashMap<PathBuf, Vec<usize>>,
    /// Files reachable from the document root via resolved edges (root inclusive).
    /// Empty when [`build`](Self::build) was given no root — the salsa wrapper
    /// passes `None`, deferring "which file is the main document" to a consumer.
    reachable: HashSet<PathBuf>,
    /// Detected inclusion cycles (each a list of paths forming the cycle).
    /// `\input`/`\include` recursion is illegal in TeX; we expose cycles for a
    /// later linter rather than diagnosing here.
    cycles: Vec<Vec<PathBuf>>,
}

impl IncludeGraph {
    /// Assemble the inclusion graph for `files`. `root`, when given, seeds the
    /// reachability set (the main document and everything it transitively
    /// includes). Pure: resolves edge targets against the member set by path and
    /// never touches the disk.
    pub fn build(files: &[FileFacts], root: Option<&Path>) -> Self {
        Self::build_with_aliases(files, root, &[])
    }

    pub fn build_with_aliases(
        files: &[FileFacts],
        root: Option<&Path>,
        aliases: &[(PathBuf, PathBuf)],
    ) -> Self {
        let aliases: HashMap<_, _> = aliases.iter().cloned().collect();
        let members: HashSet<&Path> = files.iter().map(|f| f.path.as_path()).collect();

        let mut edges: HashMap<PathBuf, Vec<ResolvedInclude>> = HashMap::new();
        let mut included_by: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut unresolved: Vec<UnresolvedInclude> = Vec::new();

        for file in files {
            let mut outgoing = Vec::new();
            for edge in &file.include_edges {
                match &edge.target {
                    IncludeTarget::Path(to) if members.contains(to.as_path()) => {
                        outgoing.push(ResolvedInclude {
                            literal: edge.literal.clone(),
                            build_participation: edge.build_participation,
                            from: file.path.clone(),
                            to: to.clone(),
                            kind: edge.kind,
                        });
                        included_by
                            .entry(to.clone())
                            .or_default()
                            .push(file.path.clone());
                    }
                    // A resolved path to a file we didn't analyze is just as
                    // opaque as a dynamic target.
                    _ => unresolved.push(UnresolvedInclude {
                        literal: edge.literal.clone(),
                        from: file.path.clone(),
                        target: edge.target.clone(),
                        kind: edge.kind,
                    }),
                }
            }
            edges.insert(file.path.clone(), outgoing);
        }

        // The document tree, for reachability and cycle detection. A subfile's
        // `\documentclass[parent]{subfiles}` edge is deliberately excluded: it
        // points *up*, at the preamble the subfile borrows, so pairing it with
        // the parent's own `\subfile{…}` would read as an inclusion cycle when
        // it is in fact the package's normal arrangement. Namespace membership
        // (`outgoing`/`included_by`, both undirected) still sees it — merging
        // the two files is the whole point of the edge.
        let adj: HashMap<PathBuf, Vec<PathBuf>> = edges
            .iter()
            .map(|(from, outgoing)| {
                (
                    from.clone(),
                    outgoing
                        .iter()
                        .filter(|e| {
                            e.kind != IncludeKind::SubFilesParent
                                && e.build_participation != Some(false)
                        })
                        .map(|e| e.to.clone())
                        .collect(),
                )
            })
            .collect();
        let reachable = match root {
            Some(root) => reachable_over(root, &adj),
            None => HashSet::new(),
        };
        let roots: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        let cycles = detect_cycles_over(&roots, &adj);

        let mut unresolved_by_source: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (index, edge) in unresolved.iter().enumerate() {
            unresolved_by_source
                .entry(edge.from.clone())
                .or_default()
                .push(index);
        }
        let mut subfile_parents: HashMap<PathBuf, HashSet<PathBuf>> = HashMap::new();
        for (child, outgoing) in &edges {
            for edge in outgoing
                .iter()
                .filter(|edge| edge.kind == IncludeKind::SubFilesParent)
            {
                subfile_parents
                    .entry(edge.to.clone())
                    .or_default()
                    .insert(child.clone());
            }
        }
        let subfile_children = subfile_parents
            .into_iter()
            .map(|(parent, children)| {
                let ordered = included_by[&parent]
                    .iter()
                    .filter(|child| children.contains(*child))
                    .cloned()
                    .collect();
                (parent, ordered)
            })
            .collect();
        Self {
            aliases,
            include_only: files
                .iter()
                .map(|file| (file.path.clone(), file.include_only.clone()))
                .collect(),
            edges,
            included_by,
            subfile_children,
            unresolved,
            unresolved_by_source,
            reachable,
            cycles,
        }
    }

    /// Interpret literal edges in a compilation root's directory and imported
    /// directory context. Source-only views retain excluded dependencies.
    pub fn root_contexts(&self, root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<PathBuf>> {
        self.interpret(root).contexts
    }

    pub fn interpret(&self, root: &Path) -> RootInterpretation {
        use IncludeKind::*;
        let mut result = RootInterpretation::default();
        let filter = self.include_only.get(root).cloned().unwrap_or_default();
        let root_dir = root.parent().unwrap_or(Path::new(""));
        let mut contexts = std::collections::BTreeMap::<PathBuf, Vec<PathBuf>>::new();
        let mut pending = vec![(root.to_path_buf(), root_dir.to_path_buf())];
        while let Some((path, base)) = pending.pop() {
            let bases = contexts.entry(path.clone()).or_default();
            if bases.contains(&base) {
                continue;
            }
            bases.push(base.clone());
            bases.sort();
            for edge in self
                .unresolved_from(&path)
                .filter(|edge| edge.kind == SubFilesParent)
            {
                result.edges.push(RootEdge {
                    from: path.clone(),
                    target: match &edge.target {
                        IncludeTarget::Path(path) => Some(path.clone()),
                        IncludeTarget::Dynamic => None,
                    },
                    resolved: false,
                    kind: SubFilesParent,
                    build_participation: Some(false),
                });
            }
            let edges = self
                .outgoing(&path)
                .iter()
                .filter(|edge| edge.kind != SubFilesParent)
                .map(|edge| (edge.kind, edge.literal.as_deref(), Some(edge.to.as_path())))
                .chain(
                    self.unresolved_from(&path)
                        .filter(|edge| edge.kind != SubFilesParent)
                        .map(|edge| {
                            (
                                edge.kind,
                                edge.literal.as_deref(),
                                match &edge.target {
                                    IncludeTarget::Path(path) => Some(path.as_path()),
                                    IncludeTarget::Dynamic => None,
                                },
                            )
                        }),
                );
            for (kind, literal, fallback) in edges {
                let target = if let Some(literal) = literal {
                    let directory = if matches!(kind, Include | Import) {
                        root_dir
                    } else {
                        &base
                    };
                    Some(crate::source::normalize_path(&directory.join(literal)))
                } else {
                    fallback.map(Path::to_path_buf)
                };
                let target = target.map(|target| {
                    if self.edges.contains_key(&target) {
                        target
                    } else {
                        self.aliases.get(&target).cloned().unwrap_or(target)
                    }
                });
                result.edges.push(RootEdge {
                    from: path.clone(),
                    resolved: target
                        .as_ref()
                        .is_some_and(|target| self.edges.contains_key(target)),
                    target: target.clone(),
                    kind,
                    build_participation: if kind == Include {
                        literal
                            .and_then(Path::to_str)
                            .and_then(|name| filter.participates(name))
                    } else {
                        Some(true)
                    },
                });
                if let Some(target) = target
                    && self.edges.contains_key(&target)
                {
                    let next_base = if matches!(kind, Import | SubImport | SubFile | SubFileInclude)
                    {
                        target.parent().unwrap_or(&base).to_path_buf()
                    } else {
                        base.clone()
                    };
                    pending.push((target, next_base));
                }
            }
            for child in self.subfile_children(&path) {
                pending.push((child.clone(), child.parent().unwrap_or(&base).to_path_buf()));
            }
        }
        result.contexts = contexts;
        result
    }

    /// Resolved outgoing edges of `file`, in source order.
    pub fn outgoing(&self, file: &Path) -> &[ResolvedInclude] {
        self.edges.get(file).map_or(&[], Vec::as_slice)
    }

    /// Files that include `file`.
    pub fn included_by(&self, file: &Path) -> &[PathBuf] {
        self.included_by.get(file).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn subfile_children(&self, file: &Path) -> &[PathBuf] {
        self.subfile_children.get(file).map_or(&[], Vec::as_slice)
    }

    /// Whether `file` is reachable from the document root (always `false` when
    /// the graph was built without a root).
    pub fn is_reachable(&self, file: &Path) -> bool {
        self.reachable.contains(file)
    }

    /// Edges that resolve to nothing in the analyzed set.
    pub fn unresolved(&self) -> &[UnresolvedInclude] {
        &self.unresolved
    }

    fn unresolved_from(&self, file: &Path) -> impl Iterator<Item = &UnresolvedInclude> {
        self.unresolved_by_source
            .get(file)
            .into_iter()
            .flatten()
            .map(|&index| &self.unresolved[index])
    }

    /// Detected inclusion cycles.
    pub fn cycles(&self) -> &[Vec<PathBuf>] {
        &self.cycles
    }
}

/// The set of paths reachable from `root` (inclusive) over an adjacency map of
/// resolved successor paths. Shared by the include and package graphs.
fn reachable_over(root: &Path, adj: &HashMap<PathBuf, Vec<PathBuf>>) -> HashSet<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        for to in adj.get(&path).map_or(&[][..], Vec::as_slice) {
            stack.push(to.clone());
        }
    }
    seen
}

/// Find cycles via DFS over a resolved-edge digraph (an adjacency map of successor
/// paths). Each cycle is returned once, rotated to start at its lexicographically
/// smallest path so equivalent rotations dedupe. `roots` gives the deterministic
/// start order (callers sort their members). Shared by the include and package
/// graphs (`\input` recursion and mutual `\RequirePackage` are both real cycles).
fn detect_cycles_over(
    roots: &[PathBuf],
    adj: &HashMap<PathBuf, Vec<PathBuf>>,
) -> Vec<Vec<PathBuf>> {
    let mut on_stack: HashMap<&Path, usize> = HashMap::new();
    let mut visited: HashSet<&Path> = HashSet::new();
    let mut path: Vec<&Path> = Vec::new();
    let mut pending: Vec<(&Path, usize)> = Vec::new();
    let mut found: HashSet<Vec<PathBuf>> = HashSet::new();

    for root in roots {
        if !visited.insert(root.as_path()) {
            continue;
        }
        on_stack.insert(root, path.len());
        path.push(root);
        pending.push((root, 0));
        while let Some((node, next)) = pending.last_mut() {
            if let Some(to) = adj.get(*node).and_then(|edges| edges.get(*next)) {
                *next += 1;
                if let Some(&start) = on_stack.get(to.as_path()) {
                    found.insert(normalize_cycle(&path[start..]));
                } else if visited.insert(to.as_path()) {
                    on_stack.insert(to, path.len());
                    path.push(to);
                    pending.push((to, 0));
                }
            } else {
                on_stack.remove(*node);
                path.pop();
                pending.pop();
            }
        }
    }

    found.into_iter().collect()
}

/// Rotate a cycle so it starts at its smallest path, giving every rotation of
/// the same cycle one canonical form.
fn normalize_cycle(cycle: &[&Path]) -> Vec<PathBuf> {
    let Some(min_at) = (0..cycle.len()).min_by_key(|&i| &cycle[i]) else {
        return Vec::new();
    };
    cycle[min_at..]
        .iter()
        .chain(&cycle[..min_at])
        .map(|path| path.to_path_buf())
        .collect()
}

/// One file's contribution to the package-load graph: its path and the range-free
/// load edges it declares. The load-graph analog of [`FileFacts`].
#[derive(Debug, Clone)]
pub struct PackageFileFacts {
    pub path: PathBuf,
    pub package_edges: Vec<PackageEdgeKey>,
}

/// A resolved load edge: `from` loads `to` (a `.sty`/`.cls` within the analyzed
/// member set) via `kind`. The load-graph analog of [`ResolvedInclude`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLoad {
    pub from: PathBuf,
    pub to: PathBuf,
    pub kind: PackageKind,
}

/// A load edge that could not be resolved to an analyzed member: a dynamic
/// target, or a literal name with no local `.sty`/`.cls`/`.dtx` in the set (the
/// common case for a TEXMF package like `amsmath`, which we do not search for).
/// The load-graph analog of [`UnresolvedInclude`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedLoad {
    pub from: PathBuf,
    pub target: PackageTarget,
    pub kind: PackageKind,
}

/// The package-load graph over a set of files: which files load which local
/// `.sty`/`.cls` (falling back to a package's `.dtx` source when no generated file
/// is a member). The value-comparable load-graph analog of [`IncludeGraph`],
/// built by the pure
/// [`PackageGraph::build`]. Reachability is left to the caller — different open
/// files have different scopes — via [`transitively_loaded`](Self::transitively_loaded).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PackageGraph {
    /// Per file: resolved outgoing loads, in source order.
    loads: HashMap<PathBuf, Vec<ResolvedLoad>>,
    /// Reverse map: loaded file → the files that load it.
    loaded_by: HashMap<PathBuf, Vec<PathBuf>>,
    /// Edges that resolve to nothing in the analyzed set (dynamic or non-local).
    unresolved: Vec<UnresolvedLoad>,
    /// Detected load cycles (mutual `\RequirePackage`). Exposed for a later linter.
    cycles: Vec<Vec<PathBuf>>,
}

impl PackageGraph {
    /// Assemble the load graph for `files`. Pure: resolves edge targets against the
    /// member set by path and never touches the disk.
    pub fn build(files: &[PackageFileFacts]) -> Self {
        let members: HashSet<&Path> = files.iter().map(|f| f.path.as_path()).collect();

        let mut loads: HashMap<PathBuf, Vec<ResolvedLoad>> = HashMap::new();
        let mut loaded_by: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut unresolved: Vec<UnresolvedLoad> = Vec::new();

        for file in files {
            let mut outgoing = Vec::new();
            for edge in &file.package_edges {
                // Prefer the literal `.sty`/`.cls` target; when it isn't a member,
                // fall back to the package's `.dtx` literate source.
                let resolved = match &edge.target {
                    PackageTarget::Path(to) if members.contains(to.as_path()) => Some(to.clone()),
                    PackageTarget::Path(to) => {
                        dtx_source_of(to).filter(|d| members.contains(d.as_path()))
                    }
                    PackageTarget::Dynamic => None,
                };
                match resolved {
                    Some(to) => {
                        loaded_by
                            .entry(to.clone())
                            .or_default()
                            .push(file.path.clone());
                        outgoing.push(ResolvedLoad {
                            from: file.path.clone(),
                            to,
                            kind: edge.kind,
                        });
                    }
                    // A name with no local `.sty`/`.cls`/`.dtx` (e.g. a TEXMF package)
                    // is just as opaque as a dynamic target — we resolve local only.
                    None => unresolved.push(UnresolvedLoad {
                        from: file.path.clone(),
                        target: edge.target.clone(),
                        kind: edge.kind,
                    }),
                }
            }
            loads.insert(file.path.clone(), outgoing);
        }

        let adj: HashMap<PathBuf, Vec<PathBuf>> = loads
            .iter()
            .map(|(from, outgoing)| {
                (
                    from.clone(),
                    outgoing.iter().map(|e| e.to.clone()).collect(),
                )
            })
            .collect();
        let roots: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        let cycles = detect_cycles_over(&roots, &adj);

        Self {
            loads,
            loaded_by,
            unresolved,
            cycles,
        }
    }

    /// Resolved outgoing loads of `file`, in source order.
    pub fn loads(&self, file: &Path) -> &[ResolvedLoad] {
        self.loads.get(file).map_or(&[], Vec::as_slice)
    }

    /// Files that load `file`.
    pub fn loaded_by(&self, file: &Path) -> &[PathBuf] {
        self.loaded_by.get(file).map_or(&[], Vec::as_slice)
    }

    /// Edges that resolve to nothing in the analyzed set.
    pub fn unresolved(&self) -> &[UnresolvedLoad] {
        &self.unresolved
    }

    /// Detected load cycles.
    pub fn cycles(&self) -> &[Vec<PathBuf>] {
        &self.cycles
    }

    /// All members transitively loaded from `start` (excluding `start` itself), in
    /// **post-order** DFS over resolved load edges (a package's own dependencies
    /// before the package, later siblings after earlier), each listed once. The
    /// signature-scope query folds each one's definitions in this order (later
    /// wins), so a package overrides the dependencies it pulled in and a later
    /// `\usepackage` overrides an earlier one — approximating TeX's
    /// last-definition-wins. The document's own definitions are then overlaid on
    /// top. A load cycle is terminated by the visited set.
    pub fn transitively_loaded(&self, start: &Path) -> Vec<PathBuf> {
        let mut order = Vec::new();
        let mut visited: HashSet<&Path> = HashSet::new();
        visited.insert(start);
        // Retain the parent's next edge while descending so sibling ordering
        // and visited-on-entry match the recursive post-order exactly.
        let mut pending = vec![(start, self.loads(start).iter())];
        while let Some((_, edges)) = pending.last_mut() {
            if let Some(load) = edges.next() {
                if visited.insert(load.to.as_path()) {
                    pending.push((load.to.as_path(), self.loads(&load.to).iter()));
                }
            } else {
                let (node, _) = pending.pop().unwrap();
                if !pending.is_empty() {
                    order.push(node.to_path_buf());
                }
            }
        }
        order
    }
}

/// One member of a project: its tracked input, on-disk path, and which pipeline
/// it feeds. No `Debug`: `SourceInput` is a salsa input id without a standalone
/// `Debug` impl.
///
/// `kind` lets the project-level queries split `.tex` from `.bib` members
/// without re-sniffing the extension: the include graph and label resolution see
/// only `.tex` members, while the citation resolver folds `.bib` cite keys in.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProjectMember {
    pub file: SourceId,
    pub path: PathBuf,
    pub kind: FileKind,
}

/// A project membership snapshot derived by `workspace_project`.
///
/// This is plain `Eq` data rather than a `#[salsa::interned]` value. Salsa keeps
/// interned values for the database lifetime, so each membership generation
/// would otherwise retain its id and every project-level memo keyed by it. A
/// tracked query is keyed by the live project input and backdates on equality.
/// Project removal reconstructs storage to release its input and derived values.
#[derive(Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct Project {
    pub members: Vec<ProjectMember>,
}

/// Derive the current membership from the explicit [`ProjectInput`] input.
///
/// The writer publishes files in normalized path order. The query remains pure:
/// paths are immutable `HIGH`-durability input fields, and file kinds are a
/// deterministic function of those paths.
#[salsa::tracked(returns(ref))]
pub(crate) fn workspace_project(db: &dyn IncrementalDb, context: ProjectInput) -> Project {
    db.record_query(QueryLogEntry {
        kind: QueryKind::WorkspaceProject,
        file: None,
    });
    Project {
        members: context
            .files(db)
            .iter()
            .map(|&file| {
                let path = file.path(db).clone();
                let kind = file_kind_or_tex(&path);
                ProjectMember {
                    file: *file.identity(db),
                    path,
                    kind,
                }
            })
            .collect(),
    }
}

/// The inclusion graph for the current project, built from the per-file firewall
/// query.
///
/// Value equality preserves downstream caches when reconstruction changes no
/// graph facts. `unsafe(non_salsa_values)` is sound because this owned value
/// contains no Salsa references.
///
#[salsa::tracked(returns(ref))]
fn file_include_only(
    db: &dyn IncrementalDb,
    file: crate::incremental::SourceInput,
) -> tex_ls_parser::semantic::roles::IncludeOnly {
    let root = crate::incremental::parsed_tree_root(db, file);
    let roles = crate::external::links::FileRoleContext::from_definition_sites_and_operands(
        &root,
        crate::incremental::definition_sites(db, file),
        crate::incremental::file_operand_contexts(db, file),
    );
    tex_ls_parser::semantic::roles::include_only_with_ownership(&root, |command| {
        roles.allows(command)
    })
}

/// The root is passed as `None` — no consumer designates the main document yet,
/// so reachability is left to a future caller of [`IncludeGraph::build`]. Edges,
/// reverse map, unresolved targets, and cycles are all order-independent and
/// populated regardless.
#[salsa::tracked(returns(ref), unsafe(non_salsa_values))]
pub(crate) fn project_graph(db: &dyn IncrementalDb, context: ProjectInput) -> IncludeGraph {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ProjectGraph,
        file: None,
    });

    // Only LaTeX members (`.tex`/`.sty`/`.cls`) are inclusion-graph nodes; `.bib`
    // members carry no `\input` edges and feed the citation resolver instead.
    let facts: Vec<FileFacts> = workspace_project(db, context)
        .members
        .iter()
        .filter(|member| member.kind.is_latex())
        .map(|member| FileFacts {
            include_only: file_include_only(
                db,
                db.source_input(member.file, *context.identity(db)),
            )
            .clone(),
            path: member.path.clone(),
            include_edges: include_edges(db, db.source_input(member.file, *context.identity(db)))
                .clone(),
        })
        .collect();

    IncludeGraph::build_with_aliases(&facts, None, crate::incremental::file_aliases(db, context))
}

/// The package-load graph for the current project, built from the per-file
/// [`package_edges`] firewall. Equality preserves downstream caches; the owned
/// graph contains no Salsa references.
#[salsa::tracked(returns(ref), unsafe(non_salsa_values))]
pub(crate) fn package_graph(db: &dyn IncrementalDb, context: ProjectInput) -> PackageGraph {
    db.record_query(QueryLogEntry {
        kind: QueryKind::PackageGraph,
        file: None,
    });

    // Only LaTeX members (`.tex`/`.sty`/`.cls`) declare load edges; `.bib` members
    // feed the citation resolver instead.
    let facts: Vec<PackageFileFacts> = workspace_project(db, context)
        .members
        .iter()
        .filter(|member| member.kind.is_latex())
        .map(|member| PackageFileFacts {
            path: member.path.clone(),
            package_edges: package_edges(db, db.source_input(member.file, *context.identity(db)))
                .clone(),
        })
        .collect();

    PackageGraph::build(&facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_dependency_chain_and_cycle_do_not_use_the_call_stack() {
        let roots: Vec<_> = (0..12_000)
            .map(|index| PathBuf::from(format!("/p/{index:05}.tex")))
            .collect();
        let mut adjacency: HashMap<_, _> = roots
            .windows(2)
            .map(|pair| (pair[0].clone(), vec![pair[1].clone()]))
            .collect();
        assert!(detect_cycles_over(&roots, &adjacency).is_empty());
        adjacency.insert(roots.last().unwrap().clone(), vec![roots[6_000].clone()]);
        assert_eq!(
            detect_cycles_over(&roots, &adjacency),
            [roots[6_000..].to_vec()]
        );
    }

    #[test]
    fn iterative_cycles_match_bounded_recursive_reference() {
        fn visit(
            node: usize,
            edges: &[Vec<usize>],
            active: &mut Vec<usize>,
            visited: &mut [bool],
            cycles: &mut BTreeSet<Vec<usize>>,
        ) {
            if let Some(start) = active.iter().position(|&value| value == node) {
                let mut cycle = active[start..].to_vec();
                let first = cycle
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, value)| *value)
                    .unwrap()
                    .0;
                cycle.rotate_left(first);
                cycles.insert(cycle);
                return;
            }
            if visited[node] {
                return;
            }
            visited[node] = true;
            active.push(node);
            for &next in &edges[node] {
                visit(next, edges, active, visited, cycles);
            }
            active.pop();
        }
        use std::collections::BTreeSet;
        for count in [3, 4] {
            let pairs: Vec<_> = (0..count)
                .flat_map(|from| (0..count).map(move |to| (from, to)))
                .filter(|&(from, to)| count == 3 || from != to)
                .collect();
            let nodes: Vec<_> = (0..count)
                .map(|index| PathBuf::from(index.to_string()))
                .collect();
            for mask in 0..1usize << pairs.len() {
                let mut edges = vec![Vec::new(); count];
                for (bit, &(from, to)) in pairs.iter().enumerate() {
                    if mask & (1 << bit) != 0 {
                        edges[from].push(to);
                    }
                }
                let mut order: Vec<_> = (0..count).collect();
                if mask % 2 == 0 {
                    order.reverse();
                    for outgoing in &mut edges {
                        outgoing.reverse();
                    }
                }
                let mut expected = BTreeSet::new();
                let mut visited = vec![false; count];
                for &root in &order {
                    visit(root, &edges, &mut Vec::new(), &mut visited, &mut expected);
                }
                let roots: Vec<_> = order.iter().map(|&index| nodes[index].clone()).collect();
                let adjacency = nodes
                    .iter()
                    .enumerate()
                    .map(|(index, node)| {
                        (
                            node.clone(),
                            edges[index]
                                .iter()
                                .map(|&next| nodes[next].clone())
                                .collect(),
                        )
                    })
                    .collect();
                let actual: BTreeSet<_> =
                    detect_cycles_over(&roots, &adjacency).into_iter().collect();
                let expected: BTreeSet<Vec<PathBuf>> = expected
                    .into_iter()
                    .map(|cycle| {
                        cycle
                            .into_iter()
                            .map(|index| nodes[index].clone())
                            .collect()
                    })
                    .collect();
                assert_eq!(actual, expected, "{count} nodes, mask {mask}");
            }
        }
    }

    #[test]
    fn root_paths_preserve_import_context_and_inherited_includeonly() {
        let sources = [
            (
                "/p/main.tex",
                r"\includeonly{appendix}\input{chapters/one}\import{imports/}{entry}",
            ),
            (
                "/p/chapters/one.tex",
                r"\input{shared}\include{appendix}\include{omitted}",
            ),
            ("/p/shared.tex", ""),
            ("/p/appendix.tex", ""),
            ("/p/omitted.tex", ""),
            ("/p/imports/entry.tex", r"\subimport{nested/}{leaf}"),
            ("/p/imports/nested/leaf.tex", r"\input{sibling}"),
            ("/p/imports/nested/sibling.tex", ""),
        ];
        let files: Vec<_> = sources
            .into_iter()
            .map(|(path, text)| {
                let root = tex_ls_parser::parser::parse(text).syntax();
                FileFacts {
                    path: path.into(),
                    include_only: tex_ls_parser::semantic::roles::include_only(&root),
                    include_edges: crate::project::include::collect_include_edge_keys(
                        &root,
                        Path::new(path).parent(),
                    ),
                }
            })
            .collect();
        let graph = IncludeGraph::build(&files, None);
        let view = graph.interpret(Path::new("/p/main.tex"));
        assert!(view.is_closed(), "{:?}", view.edges);
        assert_eq!(view.contexts.len(), sources.len());
        assert_eq!(
            view.contexts[Path::new("/p/imports/nested/leaf.tex")],
            [PathBuf::from("/p/imports/nested")]
        );
        assert_eq!(
            view.edges
                .iter()
                .find(|edge| edge.target.as_deref() == Some(Path::new("/p/omitted.tex")))
                .unwrap()
                .build_participation,
            Some(false)
        );
        assert_eq!(
            view.edges
                .iter()
                .find(|edge| edge.target.as_deref() == Some(Path::new("/p/appendix.tex")))
                .unwrap()
                .build_participation,
            Some(true)
        );
    }

    #[test]
    fn workspace_project_is_ordered_by_normalized_path() {
        use crate::incremental::Database;

        let mut db = Database::default();
        let b = db.upsert_file(Path::new("/p/b.bib"), String::new());
        let a = db.upsert_file(Path::new("/p/a.tex"), String::new());

        let project = workspace_project(&db, db.project_input(*a.project(&db)));
        assert_eq!(project.members.len(), 2);
        assert!(project.members[0].file == *a.identity(&db));
        assert_eq!(project.members[0].kind, FileKind::Tex);
        assert!(project.members[1].file == *b.identity(&db));
        assert_eq!(project.members[1].kind, FileKind::Bib);
        assert!(project.members[0].path < project.members[1].path);
    }

    fn facts(path: &str, edges: &[(IncludeKind, &str)]) -> FileFacts {
        FileFacts {
            include_only: Default::default(),
            path: PathBuf::from(path),
            include_edges: edges
                .iter()
                .map(|(kind, target)| IncludeEdgeKey {
                    literal: None,
                    build_participation: Some(true),
                    kind: *kind,
                    target: IncludeTarget::Path(PathBuf::from(target)),
                })
                .collect(),
        }
    }

    #[test]
    fn indexed_unresolved_edges_preserve_root_interpretation_order() {
        use IncludeKind::{Input, SubFilesParent};
        let files = [
            facts(
                "/p/a.tex",
                &[
                    (Input, "/p/missing-a.tex"),
                    (SubFilesParent, "/p/parent-a.tex"),
                    (Input, "/p/b.tex"),
                    (Input, "/p/missing-a2.tex"),
                ],
            ),
            facts(
                "/p/b.tex",
                &[
                    (Input, "/p/missing-b.tex"),
                    (SubFilesParent, "/p/parent-b.tex"),
                ],
            ),
        ];
        let graph = IncludeGraph::build(&files, None);
        let interpretation = graph.interpret(Path::new("/p/a.tex"));
        let actual: Vec<_> = interpretation
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.from.as_path(),
                    edge.target.as_deref().unwrap(),
                    edge.kind,
                    edge.resolved,
                    edge.build_participation,
                )
            })
            .collect();
        let expected = [
            (
                "/p/a.tex",
                "/p/parent-a.tex",
                SubFilesParent,
                false,
                Some(false),
            ),
            ("/p/a.tex", "/p/b.tex", Input, true, Some(true)),
            ("/p/a.tex", "/p/missing-a.tex", Input, false, Some(true)),
            ("/p/a.tex", "/p/missing-a2.tex", Input, false, Some(true)),
            (
                "/p/b.tex",
                "/p/parent-b.tex",
                SubFilesParent,
                false,
                Some(false),
            ),
            ("/p/b.tex", "/p/missing-b.tex", Input, false, Some(true)),
        ]
        .map(|(from, target, kind, resolved, participation)| {
            (
                Path::new(from),
                Path::new(target),
                kind,
                resolved,
                participation,
            )
        });
        assert_eq!(actual, expected);
        assert!(!interpretation.is_closed());
        assert_eq!(interpretation.contexts.len(), 2);
    }

    #[test]
    fn indexed_subfile_children_keep_duplicate_reverse_order_and_exclude_ordinary_inputs() {
        use IncludeKind::{Input, SubFilesParent};
        let files = [
            facts("/p/main.tex", &[]),
            facts(
                "/p/a.tex",
                &[(Input, "/p/main.tex"), (SubFilesParent, "/p/main.tex")],
            ),
            facts("/p/ordinary.tex", &[(Input, "/p/main.tex")]),
            facts("/p/b.tex", &[(SubFilesParent, "/p/main.tex")]),
        ];
        let graph = IncludeGraph::build(&files, None);
        assert_eq!(
            graph.subfile_children(Path::new("/p/main.tex")),
            ["/p/a.tex", "/p/a.tex", "/p/b.tex"].map(PathBuf::from)
        );
        let interpretation = graph.interpret(Path::new("/p/main.tex"));
        assert_eq!(
            interpretation.contexts.keys().cloned().collect::<Vec<_>>(),
            ["/p/a.tex", "/p/b.tex", "/p/main.tex"].map(PathBuf::from)
        );
        assert!(interpretation.is_closed());
    }

    #[test]
    fn resolves_edges_within_the_member_set() {
        let files = vec![
            facts("/p/main.tex", &[(IncludeKind::Input, "/p/a.tex")]),
            facts("/p/a.tex", &[]),
        ];
        let g = IncludeGraph::build(&files, None);
        assert_eq!(
            g.outgoing(Path::new("/p/main.tex")),
            &[ResolvedInclude {
                literal: None,
                build_participation: Some(true),
                from: PathBuf::from("/p/main.tex"),
                to: PathBuf::from("/p/a.tex"),
                kind: IncludeKind::Input,
            }]
        );
        assert_eq!(
            g.included_by(Path::new("/p/a.tex")),
            &[PathBuf::from("/p/main.tex")]
        );
        assert!(g.unresolved().is_empty());
    }

    #[test]
    fn target_outside_member_set_is_unresolved() {
        let files = vec![facts(
            "/p/main.tex",
            &[(IncludeKind::Input, "/p/missing.tex")],
        )];
        let g = IncludeGraph::build(&files, None);
        assert!(g.outgoing(Path::new("/p/main.tex")).is_empty());
        assert_eq!(g.unresolved().len(), 1);
        assert_eq!(g.unresolved()[0].from, PathBuf::from("/p/main.tex"));
    }

    #[test]
    fn dynamic_target_is_unresolved() {
        let files = vec![FileFacts {
            include_only: Default::default(),
            path: PathBuf::from("/p/main.tex"),
            include_edges: vec![IncludeEdgeKey {
                literal: None,
                build_participation: Some(true),
                kind: IncludeKind::Input,
                target: IncludeTarget::Dynamic,
            }],
        }];
        let g = IncludeGraph::build(&files, None);
        assert_eq!(g.unresolved().len(), 1);
        assert_eq!(g.unresolved()[0].target, IncludeTarget::Dynamic);
    }

    #[test]
    fn reachability_follows_transitive_edges_from_root() {
        let files = vec![
            facts("/p/main.tex", &[(IncludeKind::Input, "/p/a.tex")]),
            facts("/p/a.tex", &[(IncludeKind::Input, "/p/b.tex")]),
            facts("/p/b.tex", &[]),
            facts("/p/orphan.tex", &[]),
        ];
        let g = IncludeGraph::build(&files, Some(Path::new("/p/main.tex")));
        assert!(g.is_reachable(Path::new("/p/main.tex")));
        assert!(g.is_reachable(Path::new("/p/a.tex")));
        assert!(g.is_reachable(Path::new("/p/b.tex")));
        assert!(!g.is_reachable(Path::new("/p/orphan.tex")));
    }

    #[test]
    fn no_root_means_nothing_reachable() {
        let files = vec![facts("/p/main.tex", &[])];
        let g = IncludeGraph::build(&files, None);
        assert!(!g.is_reachable(Path::new("/p/main.tex")));
    }

    #[test]
    fn detects_a_cycle() {
        let files = vec![
            facts("/p/a.tex", &[(IncludeKind::Input, "/p/b.tex")]),
            facts("/p/b.tex", &[(IncludeKind::Input, "/p/a.tex")]),
        ];
        let g = IncludeGraph::build(&files, None);
        assert_eq!(g.cycles().len(), 1);
        assert_eq!(
            g.cycles()[0],
            vec![PathBuf::from("/p/a.tex"), PathBuf::from("/p/b.tex")]
        );
    }

    #[test]
    fn self_inclusion_is_a_cycle() {
        let files = vec![facts("/p/a.tex", &[(IncludeKind::Input, "/p/a.tex")])];
        let g = IncludeGraph::build(&files, None);
        assert_eq!(g.cycles(), &[vec![PathBuf::from("/p/a.tex")]]);
    }

    #[test]
    fn a_subfiles_pair_is_one_namespace_but_not_a_cycle() {
        // The normal `subfiles` arrangement: the main document `\subfile`s the
        // chapter, and the chapter names the main document as its parent.
        let files = vec![
            facts(
                "/p/chap.tex",
                &[(IncludeKind::SubFilesParent, "/p/main.tex")],
            ),
            facts("/p/main.tex", &[(IncludeKind::SubFile, "/p/chap.tex")]),
        ];
        let g = IncludeGraph::build(&files, None);
        // Both directions resolve, so the namespace union-find merges the two...
        assert_eq!(
            g.included_by(Path::new("/p/main.tex")),
            &[PathBuf::from("/p/chap.tex")]
        );
        assert_eq!(
            g.included_by(Path::new("/p/chap.tex")),
            &[PathBuf::from("/p/main.tex")]
        );
        // ...but the up-edge is not part of the document tree, so this is not a
        // cycle a future inclusion-cycle lint should ever flag.
        assert!(g.cycles().is_empty(), "got cycles: {:?}", g.cycles());
    }

    #[test]
    fn acyclic_graph_has_no_cycles() {
        let files = vec![
            facts("/p/main.tex", &[(IncludeKind::Input, "/p/a.tex")]),
            facts("/p/a.tex", &[]),
        ];
        let g = IncludeGraph::build(&files, None);
        assert!(g.cycles().is_empty());
    }

    fn pkg_facts(path: &str, edges: &[(PackageKind, &str)]) -> PackageFileFacts {
        PackageFileFacts {
            path: PathBuf::from(path),
            package_edges: edges
                .iter()
                .map(|(kind, target)| PackageEdgeKey {
                    kind: *kind,
                    target: PackageTarget::Path(PathBuf::from(target)),
                })
                .collect(),
        }
    }

    #[test]
    fn package_graph_resolves_a_local_load() {
        let files = vec![
            pkg_facts("/p/main.tex", &[(PackageKind::UsePackage, "/p/mypkg.sty")]),
            pkg_facts("/p/mypkg.sty", &[]),
        ];
        let g = PackageGraph::build(&files);
        assert_eq!(
            g.loads(Path::new("/p/main.tex")),
            &[ResolvedLoad {
                from: PathBuf::from("/p/main.tex"),
                to: PathBuf::from("/p/mypkg.sty"),
                kind: PackageKind::UsePackage,
            }]
        );
        assert_eq!(
            g.loaded_by(Path::new("/p/mypkg.sty")),
            &[PathBuf::from("/p/main.tex")]
        );
        assert!(g.unresolved().is_empty());
    }

    #[test]
    fn non_local_package_is_unresolved() {
        // `amsmath` has no sibling `.sty` in the member set — local-only resolution.
        let files = vec![pkg_facts(
            "/p/main.tex",
            &[(PackageKind::UsePackage, "/p/amsmath.sty")],
        )];
        let g = PackageGraph::build(&files);
        assert!(g.loads(Path::new("/p/main.tex")).is_empty());
        assert_eq!(g.unresolved().len(), 1);
    }

    #[test]
    fn load_falls_back_to_dtx_when_no_sty() {
        // `mypkg.sty` isn't a member, but its `.dtx` source is — resolve to the `.dtx`.
        let files = vec![
            pkg_facts("/p/main.tex", &[(PackageKind::UsePackage, "/p/mypkg.sty")]),
            pkg_facts("/p/mypkg.dtx", &[]),
        ];
        let g = PackageGraph::build(&files);
        assert_eq!(
            g.loads(Path::new("/p/main.tex")),
            &[ResolvedLoad {
                from: PathBuf::from("/p/main.tex"),
                to: PathBuf::from("/p/mypkg.dtx"),
                kind: PackageKind::UsePackage,
            }]
        );
        assert_eq!(
            g.loaded_by(Path::new("/p/mypkg.dtx")),
            &[PathBuf::from("/p/main.tex")]
        );
        assert!(g.unresolved().is_empty());
    }

    #[test]
    fn load_prefers_sty_over_dtx_when_both_present() {
        // Both the generated `mypkg.sty` and its `mypkg.dtx` are members — the
        // generated file wins; the `.dtx` fallback only fires when it is absent.
        let files = vec![
            pkg_facts("/p/main.tex", &[(PackageKind::UsePackage, "/p/mypkg.sty")]),
            pkg_facts("/p/mypkg.sty", &[]),
            pkg_facts("/p/mypkg.dtx", &[]),
        ];
        let g = PackageGraph::build(&files);
        assert_eq!(
            g.loads(Path::new("/p/main.tex")),
            &[ResolvedLoad {
                from: PathBuf::from("/p/main.tex"),
                to: PathBuf::from("/p/mypkg.sty"),
                kind: PackageKind::UsePackage,
            }]
        );
        assert!(g.loaded_by(Path::new("/p/mypkg.dtx")).is_empty());
    }

    #[test]
    fn deep_package_postorder_does_not_use_the_call_stack() {
        let paths: Vec<_> = (0..65_536)
            .map(|index| PathBuf::from(format!("/p/{index:05}.sty")))
            .collect();
        let mut files: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| PackageFileFacts {
                path: path.clone(),
                package_edges: paths
                    .get(index + 1)
                    .map(|next| PackageEdgeKey {
                        kind: PackageKind::RequirePackage,
                        target: PackageTarget::Path(next.clone()),
                    })
                    .into_iter()
                    .collect(),
            })
            .collect();
        let expected: Vec<_> = paths[1..].iter().rev().cloned().collect();
        assert_eq!(
            PackageGraph::build(&files).transitively_loaded(&paths[0]),
            expected
        );
        files
            .last_mut()
            .unwrap()
            .package_edges
            .push(PackageEdgeKey {
                kind: PackageKind::RequirePackage,
                target: PackageTarget::Path(paths[32_768].clone()),
            });
        assert_eq!(
            PackageGraph::build(&files).transitively_loaded(&paths[0]),
            expected
        );
    }

    #[test]
    fn package_postorder_matches_bounded_recursive_reference() {
        fn visit(node: usize, edges: &[Vec<usize>], seen: &mut [bool], order: &mut Vec<usize>) {
            for &next in &edges[node] {
                if !seen[next] {
                    seen[next] = true;
                    visit(next, edges, seen, order);
                    order.push(next);
                }
            }
        }
        let paths: Vec<_> = (0..12)
            .map(|index| PathBuf::from(format!("/p/{index}.sty")))
            .collect();
        let mut random = 0x42fa_916d_u32;
        for _ in 0..256 {
            let edges: Vec<Vec<usize>> = (0..paths.len())
                .map(|_| {
                    (0..paths.len())
                        .filter(|_| {
                            random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                            random >> 28 < 3
                        })
                        .collect()
                })
                .collect();
            let files: Vec<_> = edges
                .iter()
                .enumerate()
                .map(|(index, targets)| PackageFileFacts {
                    path: paths[index].clone(),
                    package_edges: targets
                        .iter()
                        .map(|&target| PackageEdgeKey {
                            kind: PackageKind::RequirePackage,
                            target: PackageTarget::Path(paths[target].clone()),
                        })
                        .collect(),
                })
                .collect();
            let graph = PackageGraph::build(&files);
            for start in 0..paths.len() {
                let mut seen = vec![false; paths.len()];
                seen[start] = true;
                let mut expected = Vec::new();
                visit(start, &edges, &mut seen, &mut expected);
                assert_eq!(
                    graph.transitively_loaded(&paths[start]),
                    expected
                        .iter()
                        .map(|&index| paths[index].clone())
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn transitively_loaded_is_post_order() {
        // main → a → b, and main → c. Post-order: b, a, c.
        let files = vec![
            pkg_facts(
                "/p/main.tex",
                &[
                    (PackageKind::UsePackage, "/p/a.sty"),
                    (PackageKind::UsePackage, "/p/c.sty"),
                ],
            ),
            pkg_facts("/p/a.sty", &[(PackageKind::RequirePackage, "/p/b.sty")]),
            pkg_facts("/p/b.sty", &[]),
            pkg_facts("/p/c.sty", &[]),
        ];
        let g = PackageGraph::build(&files);
        assert_eq!(
            g.transitively_loaded(Path::new("/p/main.tex")),
            vec![
                PathBuf::from("/p/b.sty"),
                PathBuf::from("/p/a.sty"),
                PathBuf::from("/p/c.sty"),
            ]
        );
    }

    #[test]
    fn mutual_requirepackage_is_a_cycle() {
        let files = vec![
            pkg_facts("/p/a.sty", &[(PackageKind::RequirePackage, "/p/b.sty")]),
            pkg_facts("/p/b.sty", &[(PackageKind::RequirePackage, "/p/a.sty")]),
        ];
        let g = PackageGraph::build(&files);
        assert_eq!(g.cycles().len(), 1);
        // transitively_loaded still terminates on the cycle.
        assert_eq!(
            g.transitively_loaded(Path::new("/p/a.sty")),
            vec![PathBuf::from("/p/b.sty")]
        );
    }
}
