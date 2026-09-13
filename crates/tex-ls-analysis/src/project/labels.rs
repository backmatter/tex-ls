//! Cross-file label resolution: union the per-file label definitions across the
//! inclusion graph so a `\ref` can be resolved against the whole document, and
//! a key defined in two files of one document can be flagged as a duplicate.
//!
//! Layered like [`crate::project::graph`]: [`ResolvedLabels::build`] is the
//! **pure** algorithm (no salsa, no disk), and `crate::project::resolved_labels`
//! is a thin tracked wrapper. The CLI calls the pure builder directly (one-shot,
//! no salsa); the language server (eventually) uses the query. Both feed the same
//! data into the linter, so results match.
//!
//! Namespaces follow directed document roots. Shared includes participate in
//! separate views; a shared child's default view never merges its parent roots.
use crate::incremental::ProjectInput;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use smol_str::SmolStr;

use crate::ast::{command_name, environment_name};
use crate::incremental::{
    IncrementalDb, QueryKind, QueryLogEntry, file_is_document_root, file_labels, file_refs,
};
use crate::project::graph::IncludeGraph;
use crate::semantic::SemanticModel;
use crate::syntax::{SyntaxKind, SyntaxNode};

/// The distinct label names defined in `model`, sorted and deduped—the per-file
/// label input to [`ResolvedLabels::build`]. Shared by the CLI
/// (one-shot, non-salsa) and the `crate::incremental::file_labels` firewall so
/// both feed identical data into the resolver.
pub fn document_label_names(model: &SemanticModel) -> Vec<SmolStr> {
    let mut names: Vec<SmolStr> = model
        .labels()
        .iter()
        .map(|label| label.name.clone())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The distinct `\ref`-family key names *used* in `model`, sorted and deduped —
/// the per-file reference input to [`ResolvedLabels::build`], the mirror image of
/// [`document_label_names`]. A `\cref{a,b}` contributes both `a` and `b` (the
/// model already splits key lists). Feeds the cross-file `unreferenced-label`
/// lint, which asks whether a label definition is targeted *anywhere* in the
/// namespace.
pub fn document_ref_names(model: &SemanticModel) -> Vec<SmolStr> {
    let mut names: Vec<SmolStr> = model.refs().iter().map(|r| r.name.clone()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The distinct glossary/acronym keys defined in `model`
/// (`\newglossaryentry`/`\newacronym`/…), sorted and deduped — the per-file input
/// to the `crate::incremental::file_glossary_keys` firewall, the glossary
/// analog of [`document_label_names`].
pub fn document_glossary_keys(model: &SemanticModel) -> Vec<SmolStr> {
    let mut keys: Vec<SmolStr> = model
        .glossary_defs()
        .iter()
        .map(|def| def.key.clone())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Whether `root` carries a `\documentclass` or a `\begin{document}` — the
/// document-root signal gating the `undefined-ref` lint (see [`ResolvedLabels`]).
/// Shared by the CLI and the `crate::incremental::file_is_document_root`
/// firewall.
pub fn is_document_root(root: &SyntaxNode) -> bool {
    root.descendants().any(|node| match node.kind() {
        SyntaxKind::COMMAND => command_name(&node).as_deref() == Some("documentclass"),
        // The `document` environment's name lives on its `\begin{document}`.
        SyntaxKind::BEGIN => environment_name(&node).as_deref() == Some("document"),
        _ => false,
    })
}

/// One directed document namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Component {
    /// Label name → the files in this component that define it, sorted & deduped.
    defs: HashMap<SmolStr, Vec<PathBuf>>,
    /// Every `\ref`-family key *used* by any file in the component. Membership
    /// only (order-free), so a `HashSet`: `unreferenced-label` asks "is this
    /// label referenced somewhere in the namespace?", the mirror of `defs`.
    refs: HashSet<SmolStr>,
    /// Whether every include in the component resolves to an analyzed member: no
    /// dynamic and no external (out-of-set) targets. Only then is "defined
    /// nowhere" trustworthy enough to drive `undefined-ref`.
    closed: bool,
    /// Whether any member is a document root (`\documentclass` /
    /// `\begin{document}`). `undefined-ref` fires only inside a rooted namespace.
    rooted: bool,
}

/// The resolved cross-file label model over a set of analyzed files.
///
/// Built by [`ResolvedLabels::build`], with value equality for query reuse.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResolvedLabels {
    /// File path → index into [`components`](Self::components).
    component_of: HashMap<PathBuf, usize>,
    scopes: Vec<Vec<PathBuf>>,
    roots: HashMap<PathBuf, Vec<PathBuf>>,
    components: Vec<Arc<Component>>,
}

impl ResolvedLabels {
    /// Resolve labels for `files` — each a `(path, distinct sorted label names,
    /// distinct sorted `\ref` key names, is_document_root)` tuple — partitioned by
    /// the inclusion `graph`.
    ///
    /// Pure and deterministic: components are assigned in sorted-path order and
    /// every definer list is sorted, so the output never depends on `HashMap`
    /// iteration order. (The per-component reference set is queried by membership
    /// only, so its iteration order never reaches the output.)
    pub fn build(
        files: &[(PathBuf, Vec<SmolStr>, Vec<SmolStr>, bool)],
        graph: &IncludeGraph,
    ) -> Self {
        let partition = super::root_views::RootViews::build(
            files.iter().map(|(path, _, _, _)| path.as_path()),
            graph,
            files
                .iter()
                .filter(|(_, _, _, root)| *root)
                .map(|(path, _, _, _)| path.as_path()),
        );
        Self::build_in_components(files, graph, &partition)
    }

    fn build_in_components(
        files: &[(PathBuf, Vec<SmolStr>, Vec<SmolStr>, bool)],
        _graph: &IncludeGraph,
        partition: &super::root_views::RootViews,
    ) -> Self {
        let component_of = partition.component_of.clone();
        let mut components: Vec<Component> = (0..partition.count)
            .map(|_| Component {
                closed: true,
                ..Component::default()
            })
            .collect();

        // Index definitions, references, and the rooted flag per component.
        for (path, names, refs, is_root) in files {
            for (id, scope) in partition.scopes.iter().enumerate() {
                if !scope.contains(path) {
                    continue;
                }
                let comp = &mut components[id];
                comp.rooted |= *is_root;
                for name in names {
                    comp.defs
                        .entry(name.clone())
                        .or_default()
                        .push(path.clone());
                }
                comp.refs.extend(refs.iter().cloned());
            }
        }

        // An unresolved include (dynamic or out-of-set) opens its component: the
        // real label universe may be larger than what we analyzed.
        for (id, closed) in partition.closed.iter().enumerate() {
            components[id].closed &= closed;
        }

        // Canonicalize definer lists (a file appears at most once per name —
        // `file_labels` is already deduped — but distinct files arrive unordered).
        for comp in &mut components {
            for definers in comp.defs.values_mut() {
                definers.sort_unstable();
                definers.dedup();
            }
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

    /// Files in `file`'s namespace that define `name`, sorted. Empty when `file`
    /// is unknown or `name` is undefined in its component. Includes `file` itself
    /// when it defines `name`; callers wanting *other* definers filter it out.
    pub fn definers(&self, file: &Path, name: &str) -> &[PathBuf] {
        self.component_of
            .get(file)
            .and_then(|&id| self.components[id].defs.get(name))
            .map_or(&[], Vec::as_slice)
    }

    /// Distinct label names in this file's include-connected namespace.
    /// Order is unspecified; callers choose their presentation order.
    pub fn label_names<'a>(&'a self, file: &Path) -> impl Iterator<Item = &'a str> + 'a {
        self.component_of
            .get(file)
            .map(|&id| &self.components[id])
            .into_iter()
            .flat_map(|component| component.defs.keys().map(SmolStr::as_str))
    }

    /// Whether `name` is defined anywhere in `file`'s namespace.
    pub fn is_defined(&self, file: &Path, name: &str) -> bool {
        !self.definers(file, name).is_empty()
    }

    /// Whether `name` is targeted by a `\ref`-family command anywhere in `file`'s
    /// namespace. The mirror of [`is_defined`](Self::is_defined): `undefined-ref`
    /// asks whether a *reference* has a definition, `unreferenced-label` asks
    /// whether a *definition* has a reference. Both are trustworthy only over a
    /// closed, rooted namespace (see [`is_closed`](Self::is_closed) /
    /// [`is_root_component`](Self::is_root_component)).
    pub fn is_referenced(&self, file: &Path, name: &str) -> bool {
        self.component_of
            .get(file)
            .is_some_and(|&id| self.components[id].refs.contains(name))
    }

    /// All member files sharing `file`'s namespace (its connected component),
    /// sorted; empty when `file` is unknown. Includes `file` itself. Unlike
    /// [`definers`](Self::definers) (which files *define* a name) this is every
    /// file in the namespace — the search set for find-references, which must scan
    /// each member for `\ref` use sites.
    pub fn namespace_members(&self, file: &Path) -> Vec<&Path> {
        let Some(&id) = self.component_of.get(file) else {
            return Vec::new();
        };
        self.scopes[id].iter().map(PathBuf::as_path).collect()
    }

    /// Whether `file`'s namespace is closed — every include resolves to an
    /// analyzed member. Gates `undefined-ref` (an open namespace may define the
    /// key in a file we never saw).
    pub fn is_closed(&self, file: &Path) -> bool {
        self.component_of
            .get(file)
            .is_some_and(|&id| self.components[id].closed)
    }

    /// Whether `file`'s namespace contains a document root. Gates `undefined-ref`
    /// so a bare fragment opened standalone is never flagged.
    pub fn is_root_component(&self, file: &Path) -> bool {
        self.component_of
            .get(file)
            .is_some_and(|&id| self.components[id].rooted)
    }
}

/// The cross-file label resolution for `project`, built from the per-file
/// [`file_labels`] firewall and the [`crate::project::project_graph`].
///
/// Range-free input projections and value equality preserve unchanged answers.
#[salsa::tracked(returns(ref))]
pub(crate) fn resolved_labels(db: &dyn IncrementalDb, context: ProjectInput) -> ResolvedLabels {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ResolvedLabels,
        file: None,
    });

    ResolvedLabels {
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
            .map(|representative| component_labels(db, representative).clone())
            .collect(),
    }
}

#[salsa::tracked(returns(ref))]
fn component_labels(
    db: &dyn IncrementalDb,
    representative: crate::incremental::SourceInput,
) -> Arc<Component> {
    db.record_query(QueryLogEntry {
        kind: QueryKind::ComponentLabels,
        file: Some(*representative.identity(db)),
    });
    let mut component = Component {
        closed: *super::root_views::closed(db, representative),
        ..Default::default()
    };
    for &member in super::root_views::members(db, representative) {
        component.rooted |= *file_is_document_root(db, member);
        for name in file_labels(db, member) {
            component
                .defs
                .entry(name.clone())
                .or_default()
                .push(member.path(db).clone());
        }
        component.refs.extend(file_refs(db, member).iter().cloned());
    }
    Arc::new(component)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::graph::FileFacts;
    use crate::project::include::{IncludeEdgeKey, IncludeKind, IncludeTarget};

    /// Build an `IncludeGraph` from `(path, [(kind, target)])` tuples.
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

    fn names(list: &[&str]) -> Vec<SmolStr> {
        list.iter().map(SmolStr::new).collect()
    }

    #[test]
    fn shared_include_keeps_independent_root_views() {
        let g = graph(&[
            ("/p/a.tex", &[(IncludeKind::Input, "/p/shared.tex")]),
            ("/p/b.tex", &[(IncludeKind::Input, "/p/shared.tex")]),
            ("/p/shared.tex", &[]),
        ]);
        let r = ResolvedLabels::build(
            &[
                ("/p/a.tex".into(), names(&["same", "a"]), names(&[]), true),
                ("/p/b.tex".into(), names(&["same", "b"]), names(&[]), true),
                (
                    "/p/shared.tex".into(),
                    names(&["shared"]),
                    names(&[]),
                    false,
                ),
            ],
            &g,
        );
        assert_eq!(
            r.definers(Path::new("/p/a.tex"), "same"),
            &[PathBuf::from("/p/a.tex")]
        );
        assert!(!r.is_defined(Path::new("/p/a.tex"), "b"));
        assert!(r.is_defined(Path::new("/p/a.tex"), "shared"));
        assert!(r.is_defined(Path::new("/p/b.tex"), "shared"));
        assert_eq!(
            r.candidate_roots(Path::new("/p/shared.tex")),
            &[PathBuf::from("/p/a.tex"), PathBuf::from("/p/b.tex")]
        );
        assert!(!r.has_unique_root(Path::new("/p/shared.tex")));
        assert!(!r.is_defined(Path::new("/p/shared.tex"), "same"));
    }

    #[test]
    fn lone_file_is_its_own_component() {
        let g = graph(&[("/p/a.tex", &[])]);
        let r = ResolvedLabels::build(
            &[(PathBuf::from("/p/a.tex"), names(&["x"]), names(&[]), false)],
            &g,
        );
        assert!(r.is_defined(Path::new("/p/a.tex"), "x"));
        assert!(!r.is_defined(Path::new("/p/a.tex"), "y"));
        // No other file defines `x`.
        assert_eq!(
            r.definers(Path::new("/p/a.tex"), "x"),
            &[PathBuf::from("/p/a.tex")]
        );
    }

    #[test]
    fn input_chain_shares_one_namespace() {
        let g = graph(&[
            ("/p/main.tex", &[(IncludeKind::Input, "/p/chap.tex")]),
            ("/p/chap.tex", &[]),
        ]);
        let r = ResolvedLabels::build(
            &[
                (
                    PathBuf::from("/p/main.tex"),
                    names(&[]),
                    names(&["a"]),
                    true,
                ),
                (
                    PathBuf::from("/p/chap.tex"),
                    names(&["a"]),
                    names(&[]),
                    false,
                ),
            ],
            &g,
        );
        // A label in the chapter is visible from the main file's namespace.
        assert!(r.is_defined(Path::new("/p/main.tex"), "a"));
        assert!(r.is_root_component(Path::new("/p/chap.tex")));
        assert!(r.is_closed(Path::new("/p/main.tex")));
        // The chapter's `\label{a}` is referenced cross-file (from main), visible
        // when the whole namespace is queried from either member.
        assert!(r.is_referenced(Path::new("/p/chap.tex"), "a"));
        assert!(!r.is_referenced(Path::new("/p/chap.tex"), "b"));
    }

    #[test]
    fn diamond_merges_all_four() {
        let g = graph(&[
            (
                "/p/main.tex",
                &[
                    (IncludeKind::Input, "/p/a.tex"),
                    (IncludeKind::Input, "/p/b.tex"),
                ],
            ),
            ("/p/a.tex", &[(IncludeKind::Input, "/p/shared.tex")]),
            ("/p/b.tex", &[(IncludeKind::Input, "/p/shared.tex")]),
            ("/p/shared.tex", &[]),
        ]);
        let r = ResolvedLabels::build(
            &[
                (PathBuf::from("/p/main.tex"), names(&[]), names(&[]), true),
                (PathBuf::from("/p/a.tex"), names(&["k"]), names(&[]), false),
                (PathBuf::from("/p/b.tex"), names(&["k"]), names(&[]), false),
                (
                    PathBuf::from("/p/shared.tex"),
                    names(&[]),
                    names(&[]),
                    false,
                ),
            ],
            &g,
        );
        // `k` defined in both a and b → both are cross-file definers, sorted.
        assert_eq!(
            r.definers(Path::new("/p/a.tex"), "k"),
            &[PathBuf::from("/p/a.tex"), PathBuf::from("/p/b.tex")]
        );
        // The whole diamond is one namespace: every member is a reference-search
        // target, regardless of whether it defines anything.
        assert_eq!(
            r.namespace_members(Path::new("/p/shared.tex")),
            &[
                Path::new("/p/a.tex"),
                Path::new("/p/b.tex"),
                Path::new("/p/main.tex"),
                Path::new("/p/shared.tex"),
            ]
        );
    }

    #[test]
    fn namespace_members_isolates_independent_documents() {
        let g = graph(&[("/p/one.tex", &[]), ("/p/two.tex", &[])]);
        let r = ResolvedLabels::build(
            &[
                (PathBuf::from("/p/one.tex"), names(&["x"]), names(&[]), true),
                (PathBuf::from("/p/two.tex"), names(&["x"]), names(&[]), true),
            ],
            &g,
        );
        assert_eq!(
            r.namespace_members(Path::new("/p/one.tex")),
            &[Path::new("/p/one.tex")]
        );
        assert!(r.namespace_members(Path::new("/p/missing.tex")).is_empty());
    }

    #[test]
    fn independent_documents_do_not_share_labels() {
        let g = graph(&[("/p/one.tex", &[]), ("/p/two.tex", &[])]);
        let r = ResolvedLabels::build(
            &[
                (
                    PathBuf::from("/p/one.tex"),
                    names(&["intro"]),
                    names(&[]),
                    true,
                ),
                (
                    PathBuf::from("/p/two.tex"),
                    names(&["intro"]),
                    names(&[]),
                    true,
                ),
            ],
            &g,
        );
        // Same key in two unrelated docs is NOT a cross-file duplicate.
        assert_eq!(
            r.definers(Path::new("/p/one.tex"), "intro"),
            &[PathBuf::from("/p/one.tex")]
        );
        assert_eq!(
            r.definers(Path::new("/p/two.tex"), "intro"),
            &[PathBuf::from("/p/two.tex")]
        );
    }

    #[test]
    fn cycle_is_one_component() {
        let g = graph(&[
            ("/p/a.tex", &[(IncludeKind::Input, "/p/b.tex")]),
            ("/p/b.tex", &[(IncludeKind::Input, "/p/a.tex")]),
        ]);
        let r = ResolvedLabels::build(
            &[
                (PathBuf::from("/p/a.tex"), names(&["x"]), names(&[]), false),
                (PathBuf::from("/p/b.tex"), names(&[]), names(&[]), false),
            ],
            &g,
        );
        assert!(r.is_defined(Path::new("/p/b.tex"), "x"));
    }

    #[test]
    fn dynamic_include_opens_the_component() {
        let g = {
            let facts = vec![FileFacts {
                include_only: Default::default(),
                path: PathBuf::from("/p/main.tex"),
                include_edges: vec![IncludeEdgeKey {
                    literal: None,
                    build_participation: Some(true),
                    kind: IncludeKind::Input,
                    target: IncludeTarget::Dynamic,
                }],
            }];
            IncludeGraph::build(&facts, None)
        };
        let r = ResolvedLabels::build(
            &[(PathBuf::from("/p/main.tex"), names(&[]), names(&[]), true)],
            &g,
        );
        assert!(!r.is_closed(Path::new("/p/main.tex")));
    }

    #[test]
    fn external_include_opens_the_component() {
        // `/p/missing.tex` is not an analyzed member → unresolved → open.
        let g = graph(&[("/p/main.tex", &[(IncludeKind::Input, "/p/missing.tex")])]);
        let r = ResolvedLabels::build(
            &[(PathBuf::from("/p/main.tex"), names(&[]), names(&[]), true)],
            &g,
        );
        assert!(!r.is_closed(Path::new("/p/main.tex")));
    }

    #[test]
    fn rootless_component_reports_no_root() {
        let g = graph(&[("/p/frag.tex", &[])]);
        let r = ResolvedLabels::build(
            &[(
                PathBuf::from("/p/frag.tex"),
                names(&["x"]),
                names(&[]),
                false,
            )],
            &g,
        );
        assert!(!r.is_root_component(Path::new("/p/frag.tex")));
        assert!(r.is_closed(Path::new("/p/frag.tex")));
    }

    #[test]
    fn is_referenced_tracks_the_component_reference_union() {
        // One namespace: `a` is defined and referenced (in-file), `b` is defined
        // but never referenced anywhere, `c` is referenced but undefined.
        let g = graph(&[("/p/a.tex", &[])]);
        let r = ResolvedLabels::build(
            &[(
                PathBuf::from("/p/a.tex"),
                names(&["a", "b"]),
                names(&["a", "c"]),
                true,
            )],
            &g,
        );
        assert!(r.is_referenced(Path::new("/p/a.tex"), "a"));
        assert!(!r.is_referenced(Path::new("/p/a.tex"), "b"));
        // A referenced-but-undefined key still reads as referenced (that is
        // `undefined-ref`'s concern, not this method's).
        assert!(r.is_referenced(Path::new("/p/a.tex"), "c"));
        // An unknown file has an empty reference set.
        assert!(!r.is_referenced(Path::new("/p/missing.tex"), "a"));
    }
}
