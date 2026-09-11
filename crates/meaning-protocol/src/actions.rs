//! Actions operations.
use super::*;

#[allow(clippy::too_many_arguments)]
pub fn compute_code_actions(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    kind: FileKind,
    range: Range,
    only: Option<&[CodeActionKind]>,
    rules: &RuleSelection,
    enc: PositionEncoding,
) -> Vec<CodeActionResponse> {
    let Some(file) = snapshot.lookup_file(path) else {
        return Vec::new();
    };
    let buffer = TextBuffer::new(snapshot.source_text(file).clone(), enc);
    let text = &buffer;
    // Only the LaTeX rules are catalogued in the published reference, so the
    // echoed diagnostic on a bib quick-fix carries no doc link (mirrors
    // `analyze_bib`/`lint_to_lsp`).
    let link_docs = !matches!(kind, FileKind::Bib);
    // Resolve a cross-file fix's foreign target to its `(uri, text)` from the
    // snapshot, so a quick-fix can carry edits in files other than this buffer.
    let resolve = |p: &Path| -> Option<(Uri, String)> {
        let file = snapshot.lookup_file(p)?;
        let uri = path_to_uri(p)?;
        Some((uri, snapshot.file_text(file).to_string()))
    };
    let mut actions = if code_action_kind_requested(&CodeActionKind::QuickFix, only) {
        let findings = lint_findings(snapshot, path, kind, rules).unwrap_or_default();
        code_action::code_actions_for_range(
            &findings, text, uri, path, range, enc, link_docs, &resolve,
        )
    } else {
        Vec::new()
    };
    if !matches!(kind, FileKind::Bib)
        && code_action_kind_requested(&CodeActionKind::RefactorRewrite, only)
    {
        let root = snapshot.parsed_tree(file);
        actions.extend(code_action::table_column_actions(&root, text, uri, range));
    }
    actions.retain(|action| match action {
        CodeActionResponse::CodeAction(action) => action
            .kind
            .as_ref()
            .is_none_or(|kind| code_action_kind_requested(kind, only)),
        CodeActionResponse::Command(_) => only.is_none(),
    });
    actions
}

pub fn code_action_kind_requested(kind: &CodeActionKind, only: Option<&[CodeActionKind]>) -> bool {
    only.is_none_or(|requested| {
        requested.iter().any(|parent| {
            kind.as_str() == parent.as_str()
                || kind
                    .as_str()
                    .strip_prefix(parent.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    })
}
