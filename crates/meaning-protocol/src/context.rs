//! Context operations.
use super::*;

/// `path`'s label namespace — its include-graph component — falling back to just
/// `path` for a standalone or untracked file, so a caller always has something to
/// scan.
pub fn namespace_of<'a>(resolution: &'a ResolvedLabels, path: &'a Path) -> Vec<&'a Path> {
    let members = resolution.namespace_members(path);
    if members.is_empty() {
        vec![path]
    } else {
        members
    }
}

/// The document root of `namespace`: its first member carrying a
/// `\documentclass` or `\begin{document}` ([`project::labels::is_document_root`],
/// via the salsa `file_is_document_root` firewall). `None` when no member is a
/// root — an uncompilable fragment, or a project whose root the server has not
/// loaded (the db seeds one directory at a time, so a child in `chapters/` may
/// never have seen `../main.tex`; that is what `[build] root` is for).
///
/// The single place the "which file did the compiler run on?" question is
/// answered, shared by `.aux` resolution ([`document_aux`]) and PDF resolution
/// (`forward_search::pdf_path`).
///
/// [`project::labels::is_document_root`]: meaning_analysis::project::labels::is_document_root
pub fn root_document_of<'a>(snapshot: &Analysis, namespace: &[&'a Path]) -> Option<&'a Path> {
    namespace.iter().copied().find(|p| {
        snapshot
            .lookup_file(p)
            .is_some_and(|f| snapshot.file_is_document_root(f))
    })
}

/// The merged `.aux` facts for `path`'s label namespace: the aux root is the
/// namespace's document root's directory (where the compiler ran), falling back
/// to `path`'s own; an unknown/untracked file still checks its sibling `.aux`.
/// `None` when the project was never compiled. Shared by label hover (numbers in
/// the preview) and `documentSymbol` (numbers in the outline).
pub fn document_aux(
    snapshot: &Analysis,
    resolution: &ResolvedLabels,
    path: &Path,
    build: &dyn HostServices,
) -> Option<AuxData> {
    let namespace = namespace_of(resolution, path);
    let root_dir = root_document_of(snapshot, &namespace)
        .and_then(Path::parent)
        .or_else(|| path.parent())
        .unwrap_or(Path::new(""));
    build.aux_data(&namespace, root_dir)
}
