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
    // Echoed diagnostics link to the same language-specific catalogue as reports.
    let latex_rules = !matches!(kind, FileKind::Bib);
    // Resolve a cross-file fix's foreign target to its `(uri, text)` from the
    // snapshot, so a quick-fix can carry edits in files other than this buffer.
    let resolve = |p: &Path| -> Option<(Uri, String)> {
        if !snapshot.resolve_labels().has_unique_root(p) {
            return None;
        }
        let file = snapshot.lookup_file(p)?;
        let uri = path_to_uri(p)?;
        Some((uri, snapshot.file_text(file).to_string()))
    };
    let findings = if code_action_kind_requested(&CodeActionKind::QuickFix, only)
        || code_action_kind_requested(&CodeActionKind::from("source.fixAll.tex-ls"), only)
    {
        lint_findings(snapshot, path, kind, rules).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut actions = if code_action_kind_requested(&CodeActionKind::QuickFix, only) {
        code_action::code_actions_for_range(
            &findings,
            text,
            uri,
            path,
            range,
            enc,
            latex_rules,
            &resolve,
        )
    } else {
        Vec::new()
    };
    if code_action_kind_requested(&CodeActionKind::from("source.fixAll.tex-ls"), only)
        && let Some(action) = fix_all::action(&findings, text, uri, path, enc, &resolve)
    {
        actions.push(action);
    }
    let environment_kind = CodeActionKind::from("refactor.rewrite.environment");
    let tables_requested = code_action_kind_requested(&CodeActionKind::RefactorRewrite, only);
    let environments_requested = code_action_kind_requested(&environment_kind, only);
    if !matches!(kind, FileKind::Bib) && (tables_requested || environments_requested) {
        let root = snapshot.parsed_tree(file);
        if tables_requested {
            actions.extend(code_action::table_column_actions(&root, text, uri, range));
        }
        let idx = buffer.line_index();
        let offset = idx.offset_at(range.start.line, range.start.character);
        if environments_requested
            && let Some((old, ranges)) = proved_environment_pair(&root, offset)
        {
            let scope = snapshot.scope_signatures(file);
            if let Some((original, _)) = tex_ls_analysis::hover::lookup_environment(scope, &old) {
                let mut names: Vec<_> = scope
                    .environment_names()
                    .chain(tex_ls_parser::semantic::signature::builtin().environment_names())
                    .filter(|name| *name != old)
                    .collect();
                names.sort_unstable();
                names.dedup();
                for name in names
                    .into_iter()
                    .filter(|name| {
                        tex_ls_analysis::hover::lookup_environment(scope, name).is_some_and(
                            |(sig, _)| {
                                sig.args == original.args
                                    && sig.verbatim_body == original.verbatim_body
                                    && sig.math == original.math
                                    && sig.code == original.code
                            },
                        )
                    })
                    .take(20)
                {
                    let mut changes = HashMap::new();
                    for &range in &ranges {
                        push_edit(&mut changes, uri, &idx, range, name);
                    }
                    actions.push(CodeActionResponse::CodeAction(lsp_types::CodeAction {
                        title: format!("Change environment to {name}"),
                        kind: Some(environment_kind.clone()),
                        edit: finalize_rename(changes),
                        ..Default::default()
                    }));
                }
            }
        }
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
