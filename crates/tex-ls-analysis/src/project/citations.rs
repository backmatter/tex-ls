//! Cross-file citation resolution: union the cite keys reachable from each `.tex`
//! file's namespace (via its `\bibliography`/`\addbibresource` resources) so a
//! `\cite{key}` can be checked against the whole bibliography.
//!
//! The citation analog of [`crate::project::labels`]. Namespaces are the same
//! directed document views of the include graph, but the "definitions"
//! are cite keys gathered from the `.bib` files each component's members
//! reference, not `\label`s. [`ResolvedCitations::build`] is the **pure** algorithm
//! the CLI calls directly.
//!
//! A namespace is **closed** for citations only when its include graph is closed
//! *and* every bibliography resource resolves to an analyzed `.bib` file — else a
//! `\cite` key we cannot see might still be defined. `undefined-citation` fires
//! only in a closed, rooted namespace with no `\nocite{*}` wildcard, mirroring
//! `undefined-ref`'s gate.
//!
//! Include connectivity is shared with label resolution through one tracked query.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use smol_str::SmolStr;

use crate::bib::semantic::Model as BibModel;
use crate::incremental::{
    IncrementalDb, ProjectInput, QueryKind, QueryLogEntry, file_cite_facts, file_cite_names,
    file_is_document_root,
};
use crate::project::graph::IncludeGraph;
use crate::project::include::BibTarget;
use crate::source::FileKind;

/// The distinct cite keys defined in a `.bib` `model`, sorted and deduped — the
/// per-file cite-key input to [`ResolvedCitations::build`]. Shared by the CLI
/// (one-shot, non-salsa) and the `crate::incremental::file_cite_names` firewall
/// so both feed identical data into the resolver. Kept raw (not lowercased);
/// [`ResolvedCitations::build`] folds case when it indexes the keys.
pub fn document_cite_names(model: &BibModel) -> Vec<SmolStr> {
    let mut names: Vec<SmolStr> = model.entries().iter().map(|e| e.key.clone()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Per-`.tex`-file facts feeding [`ResolvedCitations::build`]: the file's
/// bibliography resource targets, whether it has a `\nocite{*}` wildcard, and
/// whether it is a document root.
#[derive(Debug, Clone)]
pub struct CiteFileFacts {
    pub path: PathBuf,
    pub bib_targets: Vec<BibTarget>,
    pub manual_keys: Vec<SmolStr>,
    pub nocite_all: bool,
    pub is_document_root: bool,
}

/// Interpret source-only bibliography literals in each root/import base.
pub fn contextual_targets(targets: &[BibTarget], bases: &[PathBuf]) -> Vec<BibTarget> {
    targets
        .iter()
        .flat_map(|target| match target {
            BibTarget::Path(path) if path.is_relative() && !bases.is_empty() => bases
                .iter()
                .map(|base| BibTarget::Path(crate::source::normalize_path(&base.join(path))))
                .collect(),
            _ => vec![target.clone()],
        })
        .collect()
}

/// One directed document citation namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Component {
    /// The cite keys available in this namespace (lowercased for case-insensitive
    /// matching, as BibTeX folds key case).
    keys: HashSet<SmolStr>,
    /// The analyzed `.bib` files this namespace draws keys from, sorted and deduped.
    /// Unlike [`keys`](Self::keys) (existence only), this carries provenance so
    /// go-to-definition can locate the entry behind a cite key. Parallel to
    /// [`labels::ResolvedLabels`](crate::project::labels)'s per-name `defs`.
    bib_paths: Vec<PathBuf>,
    /// Whether the include graph is closed *and* every bibliography resource
    /// resolved to an analyzed `.bib`. Only then is "cited but undefined"
    /// trustworthy.
    closed: bool,
    /// Whether any member is a document root.
    rooted: bool,
    /// Whether any member has a `\nocite{*}` wildcard.
    wildcard: bool,
}

impl Component {
    fn include<'a>(
        &mut self,
        rooted: bool,
        wildcard: bool,
        targets: &[BibTarget],
        manual: &[SmolStr],
        mut resolve: impl FnMut(&Path) -> Option<(&'a Path, &'a [SmolStr])>,
    ) {
        self.keys
            .extend(manual.iter().map(|key| SmolStr::new(key.to_lowercase())));
        self.rooted |= rooted;
        self.wildcard |= wildcard;
        for target in targets {
            match target {
                BibTarget::Path(path) => match resolve(path) {
                    Some((actual, keys)) => {
                        self.keys
                            .extend(keys.iter().map(|key| SmolStr::from(key.to_lowercase())));
                        self.bib_paths.push(actual.to_path_buf());
                    }
                    None => self.closed = false,
                },
                BibTarget::Dynamic => self.closed = false,
            }
        }
    }
}

/// The resolved cross-file citation model over a set of analyzed `.tex` files and
/// the `.bib` key sets they reference.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResolvedCitations {
    component_of: HashMap<PathBuf, usize>,
    scopes: Vec<Vec<PathBuf>>,
    roots: HashMap<PathBuf, Vec<PathBuf>>,
    components: Vec<Arc<Component>>,
}

impl ResolvedCitations {
    /// Resolve citations for `files`, partitioned by the inclusion `graph`, with
    /// `bib_keys` mapping each analyzed `.bib` path to its cite keys.
    ///
    /// Pure and deterministic (components assigned in sorted-path order).
    pub fn build(
        files: &[CiteFileFacts],
        graph: &IncludeGraph,
        bib_keys: &HashMap<PathBuf, Vec<SmolStr>>,
    ) -> Self {
        Self::build_with_aliases(files, graph, bib_keys, &HashMap::new())
    }

    /// Resolve citations with an explicit mapping from the project-local path a
    /// document names to the actual file found through BibTeX's search path.
    /// Keeping this mapping as data preserves the pure resolver and lets
    /// navigation retain the real bibliography path.
    pub fn build_with_aliases(
        files: &[CiteFileFacts],
        graph: &IncludeGraph,
        bib_keys: &HashMap<PathBuf, Vec<SmolStr>>,
        bib_aliases: &HashMap<PathBuf, PathBuf>,
    ) -> Self {
        let partition = super::root_views::RootViews::build(
            files.iter().map(|file| file.path.as_path()),
            graph,
            files
                .iter()
                .filter(|file| file.is_document_root)
                .map(|file| file.path.as_path()),
        );
        Self::build_in_components(files, bib_keys, bib_aliases, &partition)
    }

    fn build_in_components(
        files: &[CiteFileFacts],
        bib_keys: &HashMap<PathBuf, Vec<SmolStr>>,
        bib_aliases: &HashMap<PathBuf, PathBuf>,
        partition: &super::root_views::RootViews,
    ) -> Self {
        let component_of = partition.component_of.clone();
        let mut components: Vec<Component> = (0..partition.count)
            .map(|_| Component {
                closed: true,
                ..Component::default()
            })
            .collect();

        // Gather keys, flags, and bib-resource openness per component.
        for facts in files {
            for (id, scope) in partition.scopes.iter().enumerate() {
                if !scope.contains(&facts.path) {
                    continue;
                }
                components[id].include(
                    facts.is_document_root,
                    facts.nocite_all,
                    &contextual_targets(
                        &facts.bib_targets,
                        partition
                            .contexts
                            .get(&partition.representatives[id])
                            .and_then(|contexts| contexts.get(&facts.path))
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                    ),
                    &facts.manual_keys,
                    |path| {
                        let actual = bib_keys
                            .contains_key(path)
                            .then_some(path)
                            .or_else(|| bib_aliases.get(path).map(PathBuf::as_path))?;
                        bib_keys
                            .get_key_value(actual)
                            .map(|(path, keys)| (path.as_path(), keys.as_slice()))
                    },
                );
            }
        }

        // An unresolved `.tex` include (dynamic or out-of-set) opens its component,
        // just as it does for labels.
        for (id, closed) in partition.closed.iter().enumerate() {
            components[id].closed &= closed;
        }

        // A `.bib` reached from several members lands in `bib_paths` once per
        // reference; collapse to a stable, deduped set (mirrors the label resolver's
        // per-name `definers` sort/dedup).
        for comp in &mut components {
            comp.bib_paths.sort_unstable();
            comp.bib_paths.dedup();
        }

        Self {
            component_of,
            scopes: partition.scopes.clone(),
            roots: partition.roots.clone(),
            components: components.into_iter().map(Arc::new).collect(),
        }
    }

    /// Candidate document roots in deterministic order. Multiple roots require
    /// explicit selection for root-dependent edits.
    pub fn candidate_roots(&self, file: &Path) -> &[PathBuf] {
        self.roots.get(file).map_or(&[], Vec::as_slice)
    }

    pub fn has_unique_root(&self, file: &Path) -> bool {
        self.candidate_roots(file).len() <= 1
    }

    /// Each independent compilation scope loading this bibliography.
    pub fn bibliography_scopes<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl Iterator<Item = (&'a [PathBuf], &'a [PathBuf], bool, bool)> + 'a {
        self.components
            .iter()
            .enumerate()
            .filter(move |(_, component)| component.bib_paths.iter().any(|bib| bib == path))
            .map(|(id, component)| {
                (
                    self.scopes[id].as_slice(),
                    component.bib_paths.as_slice(),
                    component.closed && component.rooted,
                    component.wildcard,
                )
            })
    }

    /// The analyzed `.bib` files in `file`'s namespace, sorted. Empty when `file`
    /// is unknown or its component references no analyzed bibliography. Go-to-def
    /// searches these for the entry behind a cite key (the location analog of
    /// [`is_defined`](Self::is_defined), which only answers existence).
    pub fn bib_definers(&self, file: &Path) -> &[PathBuf] {
        self.component_of
            .get(file)
            .map_or(&[], |&id| self.components[id].bib_paths.as_slice())
    }

    /// All LaTeX member files sharing `file`'s namespace (its connected
    /// component), sorted; empty when `file` is unknown. `.bib` files are not
    /// keyed in `component_of`, so this is the `.tex`/`.sty`/`.cls` members only —
    /// the search set for find-references, which scans each for `\cite` use sites.
    /// Parallel to [`labels::ResolvedLabels::namespace_members`](crate::project::labels::ResolvedLabels::namespace_members).
    pub fn namespace_members(&self, file: &Path) -> Vec<&Path> {
        let Some(&id) = self.component_of.get(file) else {
            return Vec::new();
        };
        self.scopes[id].iter().map(PathBuf::as_path).collect()
    }

    /// The LaTeX members of every component whose bibliography includes
    /// `bib_path`, sorted and deduped. A `.bib` is not keyed in `component_of`
    /// (it lives in each citing component's `bib_paths`) and may be shared by
    /// several independent documents, so this unions the citers across
    /// components — the search set for find-references invoked on a `.bib` entry.
    pub fn bib_citers(&self, bib_path: &Path) -> Vec<&Path> {
        let ids: HashSet<usize> = self
            .components
            .iter()
            .enumerate()
            .filter(|(_, comp)| comp.bib_paths.iter().any(|p| p == bib_path))
            .map(|(id, _)| id)
            .collect();
        let mut members: Vec<&Path> = ids
            .into_iter()
            .flat_map(|id| self.scopes[id].iter().map(PathBuf::as_path))
            .collect();
        members.sort_unstable();
        members.dedup();
        members
    }

    /// Whether cite `key` is defined anywhere in `file`'s namespace
    /// (case-insensitive).
    pub fn is_defined(&self, file: &Path, key: &str) -> bool {
        self.component_of.get(file).is_some_and(|&id| {
            self.components[id]
                .keys
                .contains(&SmolStr::from(key.to_lowercase()))
        })
    }

    /// Whether `file`'s namespace is closed — every `.tex` include and every
    /// bibliography resource resolved to an analyzed file. Gates
    /// `undefined-citation`.
    pub fn is_closed(&self, file: &Path) -> bool {
        self.component_of
            .get(file)
            .is_some_and(|&id| self.components[id].closed)
    }

    /// Whether `file`'s namespace contains a document root. Gates
    /// `undefined-citation` so a bare fragment is never flagged.
    pub fn is_root_component(&self, file: &Path) -> bool {
        self.component_of
            .get(file)
            .is_some_and(|&id| self.components[id].rooted)
    }

    /// Whether `file`'s namespace has a `\nocite{*}` wildcard, which makes every
    /// entry "cited" and so suppresses `undefined-citation`.
    pub fn has_wildcard_nocite(&self, file: &Path) -> bool {
        self.component_of
            .get(file)
            .is_some_and(|&id| self.components[id].wildcard)
    }
}

/// The cross-file citation resolution for `project`, built from the per-file
/// [`file_cite_names`] (the `.bib` cite-key firewall), [`file_cite_facts`] (the
/// `.tex` resource/wildcard firewall), and the [`crate::project::project_graph`].
///
/// Range-free input projections and value equality preserve unchanged answers.
#[salsa::tracked(returns(ref))]
pub(crate) fn resolved_citations(
    db: &dyn IncrementalDb,
    context: ProjectInput,
) -> ResolvedCitations {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ResolvedCitations,
        file: None,
    });

    ResolvedCitations {
        scopes: super::root_views::include_components(db, context)
            .scopes
            .clone(),
        roots: super::root_views::include_components(db, context)
            .roots
            .clone(),
        component_of: super::root_views::include_components(db, context)
            .component_of
            .clone(),
        components: super::root_views::representatives(db, context)
            .into_iter()
            .map(|representative| component_citations(db, representative).clone())
            .collect(),
    }
}

#[salsa::tracked(returns(ref))]
fn component_citations(
    db: &dyn IncrementalDb,
    representative: crate::incremental::SourceInput,
) -> Arc<Component> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ComponentCitations,
        file: Some(*representative.identity(db)),
    });
    let context = db.project_input(*representative.project(db));
    let sources = super::root_views::project_sources(db, context);
    let aliases: HashMap<_, _> = crate::incremental::file_aliases(db, context)
        .iter()
        .map(|(requested, actual)| (requested.as_path(), actual.as_path()))
        .collect();
    let mut component = Component {
        closed: *super::root_views::closed(db, representative),
        ..Default::default()
    };
    for &member in super::root_views::members(db, representative) {
        let facts = file_cite_facts(db, member);
        component.include(
            *file_is_document_root(db, member),
            facts.nocite_all,
            &contextual_targets(
                &facts.bib_targets,
                super::root_views::contexts(db, context, representative.path(db))
                    .and_then(|contexts| contexts.get(member.path(db)))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            ),
            &facts.manual_keys,
            |requested| {
                let actual = if sources.contains_key(requested) {
                    requested
                } else {
                    aliases.get(requested).copied().unwrap_or(requested)
                };
                let &source = sources
                    .get(actual)
                    .filter(|_| crate::source::lint_file_kind(actual) == Some(FileKind::Bib))?;
                Some((
                    source.path(db).as_path(),
                    file_cite_names(db, source).as_slice(),
                ))
            },
        );
    }

    component.bib_paths.sort_unstable();
    component.bib_paths.dedup();
    Arc::new(component)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::graph::FileFacts;
    use crate::project::include::{IncludeEdgeKey, IncludeKind, IncludeTarget};

    fn graph(files: &[(&str, &[(IncludeKind, &str)])]) -> IncludeGraph {
        let facts: Vec<FileFacts> = files
            .iter()
            .map(|(path, edges)| FileFacts {
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
            })
            .collect();
        IncludeGraph::build(&facts, None)
    }

    fn keys(list: &[&str]) -> Vec<SmolStr> {
        list.iter().map(SmolStr::new).collect()
    }

    fn facts(path: &str, bib: &[&str], root: bool) -> CiteFileFacts {
        CiteFileFacts {
            path: PathBuf::from(path),
            bib_targets: bib
                .iter()
                .map(|b| BibTarget::Path(PathBuf::from(b)))
                .collect(),
            manual_keys: Vec::new(),
            nocite_all: false,
            is_document_root: root,
        }
    }

    #[test]
    fn key_in_referenced_bib_is_defined() {
        let g = graph(&[("/p/main.tex", &[])]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/refs.bib"), keys(&["knuth1984"]));
        let r = ResolvedCitations::build(&[facts("/p/main.tex", &["/p/refs.bib"], true)], &g, &bib);

        assert!(r.is_defined(Path::new("/p/main.tex"), "knuth1984"));
        // Case-insensitive.
        assert!(r.is_defined(Path::new("/p/main.tex"), "Knuth1984"));
        assert!(!r.is_defined(Path::new("/p/main.tex"), "missing"));
        assert!(r.is_closed(Path::new("/p/main.tex")));
        assert!(r.is_root_component(Path::new("/p/main.tex")));
    }

    #[test]
    fn keys_union_across_included_files() {
        let g = graph(&[
            ("/p/main.tex", &[(IncludeKind::Input, "/p/chap.tex")]),
            ("/p/chap.tex", &[]),
        ]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/a.bib"), keys(&["alpha"]));
        bib.insert(PathBuf::from("/p/b.bib"), keys(&["beta"]));
        let r = ResolvedCitations::build(
            &[
                facts("/p/main.tex", &["/p/a.bib"], true),
                facts("/p/chap.tex", &["/p/b.bib"], false),
            ],
            &g,
            &bib,
        );
        // Both files share one namespace, so both bibs' keys are visible from each.
        assert!(r.is_defined(Path::new("/p/chap.tex"), "alpha"));
        assert!(r.is_defined(Path::new("/p/main.tex"), "beta"));
    }

    #[test]
    fn unanalyzed_bib_opens_the_component() {
        let g = graph(&[("/p/main.tex", &[])]);
        let bib = HashMap::new(); // refs.bib not analyzed
        let r = ResolvedCitations::build(&[facts("/p/main.tex", &["/p/refs.bib"], true)], &g, &bib);
        assert!(!r.is_closed(Path::new("/p/main.tex")));
    }

    #[test]
    fn search_path_alias_uses_the_actual_bibliography() {
        let g = graph(&[("/p/main.tex", &[])]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/shared/refs.bib"), keys(&["knuth1984"]));
        let aliases = HashMap::from([(
            PathBuf::from("/p/refs.bib"),
            PathBuf::from("/shared/refs.bib"),
        )]);

        let r = ResolvedCitations::build_with_aliases(
            &[facts("/p/main.tex", &["/p/refs.bib"], true)],
            &g,
            &bib,
            &aliases,
        );

        assert!(r.is_defined(Path::new("/p/main.tex"), "knuth1984"));
        assert!(r.is_closed(Path::new("/p/main.tex")));
        assert_eq!(
            r.bib_definers(Path::new("/p/main.tex")),
            &[PathBuf::from("/shared/refs.bib")]
        );
    }

    #[test]
    fn dynamic_bib_target_opens_the_component() {
        let g = graph(&[("/p/main.tex", &[])]);
        let r = ResolvedCitations::build(
            &[CiteFileFacts {
                path: PathBuf::from("/p/main.tex"),
                bib_targets: vec![BibTarget::Dynamic],
                manual_keys: Vec::new(),
                nocite_all: false,
                is_document_root: true,
            }],
            &g,
            &HashMap::new(),
        );
        assert!(!r.is_closed(Path::new("/p/main.tex")));
    }

    #[test]
    fn dynamic_tex_include_opens_the_component() {
        let facts_list = vec![FileFacts {
            include_only: Default::default(),
            path: PathBuf::from("/p/main.tex"),
            include_edges: vec![IncludeEdgeKey {
                literal: None,
                build_participation: Some(true),
                kind: IncludeKind::Input,
                target: IncludeTarget::Dynamic,
            }],
        }];
        let g = IncludeGraph::build(&facts_list, None);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/refs.bib"), keys(&["k"]));
        let r = ResolvedCitations::build(&[facts("/p/main.tex", &["/p/refs.bib"], true)], &g, &bib);
        assert!(!r.is_closed(Path::new("/p/main.tex")));
    }

    #[test]
    fn bib_definers_are_namespace_scoped() {
        // Two disjoint projects: main1 → a.bib, main2 → b.bib (no include edge
        // between them). Each file sees only its own component's analyzed bib.
        let g = graph(&[("/p/main1.tex", &[]), ("/p/main2.tex", &[])]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/a.bib"), keys(&["alpha"]));
        bib.insert(PathBuf::from("/p/b.bib"), keys(&["beta"]));
        let r = ResolvedCitations::build(
            &[
                facts("/p/main1.tex", &["/p/a.bib"], true),
                facts("/p/main2.tex", &["/p/b.bib"], true),
            ],
            &g,
            &bib,
        );
        assert_eq!(
            r.bib_definers(Path::new("/p/main1.tex")),
            &[PathBuf::from("/p/a.bib")]
        );
        // b.bib lives in the other component and is not returned for main1.
        assert_eq!(
            r.bib_definers(Path::new("/p/main2.tex")),
            &[PathBuf::from("/p/b.bib")]
        );
        // An unknown file has no namespace, so no definers.
        assert!(r.bib_definers(Path::new("/p/none.tex")).is_empty());
    }

    #[test]
    fn namespace_members_and_bib_citers_scope_to_the_component() {
        // Two disjoint projects share `common.bib`; sharing a bibliography does not
        // merge their include components.
        let g = graph(&[
            ("/p/main1.tex", &[(IncludeKind::Input, "/p/chap1.tex")]),
            ("/p/chap1.tex", &[]),
            ("/p/main2.tex", &[]),
        ]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/common.bib"), keys(&["k"]));
        let r = ResolvedCitations::build(
            &[
                facts("/p/main1.tex", &["/p/common.bib"], true),
                facts("/p/chap1.tex", &[], false),
                facts("/p/main2.tex", &["/p/common.bib"], true),
            ],
            &g,
            &bib,
        );
        // The first project's namespace is its two LaTeX members (the `.bib` is not
        // keyed in `component_of`).
        assert_eq!(
            r.namespace_members(Path::new("/p/chap1.tex")),
            &[Path::new("/p/chap1.tex"), Path::new("/p/main1.tex")]
        );
        assert_eq!(
            r.namespace_members(Path::new("/p/main2.tex")),
            &[Path::new("/p/main2.tex")]
        );
        // The shared bib is cited from both independent documents, so its citers
        // union the LaTeX members of every component referencing it.
        assert_eq!(
            r.bib_citers(Path::new("/p/common.bib")),
            &[
                Path::new("/p/chap1.tex"),
                Path::new("/p/main1.tex"),
                Path::new("/p/main2.tex"),
            ]
        );
        assert!(r.bib_citers(Path::new("/p/unknown.bib")).is_empty());
        assert!(r.namespace_members(Path::new("/p/none.tex")).is_empty());
    }

    #[test]
    fn bib_definers_only_lists_analyzed_bibs() {
        // A resolved bib plus a never-analyzed one: only the analyzed path has a
        // location to jump to, so only it is a definer (and the component is open).
        let g = graph(&[("/p/main.tex", &[])]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/a.bib"), keys(&["alpha"]));
        let r = ResolvedCitations::build(
            &[facts("/p/main.tex", &["/p/a.bib", "/p/missing.bib"], true)],
            &g,
            &bib,
        );
        assert_eq!(
            r.bib_definers(Path::new("/p/main.tex")),
            &[PathBuf::from("/p/a.bib")]
        );
        assert!(!r.is_closed(Path::new("/p/main.tex")));
    }

    #[test]
    fn rootless_and_wildcard_flags() {
        let g = graph(&[("/p/frag.tex", &[])]);
        let mut bib = HashMap::new();
        bib.insert(PathBuf::from("/p/refs.bib"), keys(&["k"]));
        let r =
            ResolvedCitations::build(&[facts("/p/frag.tex", &["/p/refs.bib"], false)], &g, &bib);
        assert!(!r.is_root_component(Path::new("/p/frag.tex")));

        let with_wildcard = ResolvedCitations::build(
            &[CiteFileFacts {
                path: PathBuf::from("/p/main.tex"),
                bib_targets: vec![BibTarget::Path(PathBuf::from("/p/refs.bib"))],
                manual_keys: Vec::new(),
                nocite_all: true,
                is_document_root: true,
            }],
            &graph(&[("/p/main.tex", &[])]),
            &bib,
        );
        assert!(with_wildcard.has_wildcard_nocite(Path::new("/p/main.tex")));
    }
}
