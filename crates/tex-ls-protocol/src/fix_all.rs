//! Conservative composition of independently safe, atomic lint fixes.
use super::*;
use tex_ls_analysis::linter::diagnostic::{Applicability, Fix};

fn conflicts(
    a: &tex_ls_analysis::linter::diagnostic::Edit,
    b: &tex_ls_analysis::linter::diagnostic::Edit,
    origin: &Path,
) -> bool {
    a.path.as_deref().unwrap_or(origin) == b.path.as_deref().unwrap_or(origin)
        && a.start <= b.end
        && b.start <= a.end
}

pub fn valid(
    fix: &Fix,
    text: &str,
    origin: &Path,
    resolve: &dyn Fn(&Path) -> Option<(Uri, String)>,
) -> bool {
    if fix.edits.is_empty() {
        return false;
    }
    for (i, edit) in fix.edits.iter().enumerate() {
        let foreign;
        let source = if let Some(path) = &edit.path
            && path != origin
        {
            let Some((_, source)) = resolve(path) else {
                return false;
            };
            foreign = source;
            foreign.as_str()
        } else {
            text
        };
        if edit.start > edit.end
            || !source.is_char_boundary(edit.start)
            || !source.is_char_boundary(edit.end)
        {
            return false;
        }
        if fix.edits[..i]
            .iter()
            .any(|other| conflicts(edit, other, origin))
        {
            return false;
        }
    }
    true
}

/// A preferred correction must repair its own finding and have no competing safe
/// correction. Safety alone does not make a remote or ambiguous edit preferred.
pub fn preferred(
    finding: &tex_ls_analysis::linter::Diagnostic,
    findings: &[tex_ls_analysis::linter::Diagnostic],
    origin: &Path,
) -> bool {
    let Some(fix) = &finding.fix else {
        return false;
    };
    fix.applicability == Applicability::Safe
        && fix.edits.iter().any(|edit| {
            edit.path.as_deref().is_none_or(|path| path == origin)
                && edit.start <= finding.end
                && finding.start <= edit.end
        })
        && !findings
            .iter()
            .filter(|other| !std::ptr::eq(*other, finding))
            .filter_map(|other| other.fix.as_ref())
            .any(|other| {
                other.applicability == Applicability::Safe
                    && other.edits != fix.edits
                    && other
                        .edits
                        .iter()
                        .any(|a| fix.edits.iter().any(|b| conflicts(a, b, origin)))
            })
}

type EditKey<'a> = (Option<&'a Path>, usize, usize, &'a str);

fn edit_key(fix: &Fix) -> Vec<EditKey<'_>> {
    fix.edits
        .iter()
        .map(|edit| {
            (
                edit.path.as_deref(),
                edit.start,
                edit.end,
                edit.content.as_str(),
            )
        })
        .collect()
}

/// Compare competing safe edits once when a request offers many corrections.
/// Invalid competitors still count, matching the single-finding policy.
pub fn preferred_all(findings: &[tex_ls_analysis::linter::Diagnostic], origin: &Path) -> Vec<bool> {
    let mut unique = HashMap::new();
    let mut fixes = Vec::new();
    let mut groups = Vec::with_capacity(findings.len());
    for finding in findings {
        let group = finding
            .fix
            .as_ref()
            .filter(|fix| fix.applicability == Applicability::Safe)
            .map(|fix| {
                *unique.entry(edit_key(fix)).or_insert_with(|| {
                    fixes.push(fix);
                    fixes.len() - 1
                })
            });
        groups.push(group);
    }
    let rejected = conflicting_fixes(&fixes, origin);
    findings
        .iter()
        .zip(groups)
        .map(|(finding, group)| {
            group.is_some_and(|group| {
                !rejected.contains(&group)
                    && finding.fix.as_ref().unwrap().edits.iter().any(|edit| {
                        edit.path.as_deref().is_none_or(|path| path == origin)
                            && edit.start <= finding.end
                            && finding.start <= edit.end
                    })
            })
        })
        .collect()
}

fn conflicting_fixes(fixes: &[&Fix], origin: &Path) -> HashSet<usize> {
    let mut spans = Vec::new();
    for (index, fix) in fixes.iter().enumerate() {
        for edit in &fix.edits {
            spans.push((
                edit.path.as_deref().unwrap_or(origin),
                edit.start,
                edit.end,
                index,
            ));
        }
    }
    spans.sort();
    let mut rejected = HashSet::new();
    let mut active = Vec::new();
    for (path, start, end, index) in spans {
        active.retain(|&(old_path, _, old_end, _)| old_path == path && old_end >= start);
        for &(_, old_start, _, other) in &active {
            if old_start <= end && other != index {
                rejected.insert(index);
                rejected.insert(other);
            }
        }
        active.push((path, start, end, index));
    }
    rejected
}

pub fn action(
    findings: &[tex_ls_analysis::linter::Diagnostic],
    text: &TextBuffer,
    uri: &Uri,
    origin: &Path,
    enc: PositionEncoding,
    resolve: &dyn Fn(&Path) -> Option<(Uri, String)>,
) -> Option<lsp_types::CodeActionResponse> {
    let mut candidates: Vec<&Fix> = Vec::new();
    let mut seen = HashSet::new();
    for fix in findings.iter().filter_map(|finding| finding.fix.as_ref()) {
        if fix.applicability == Applicability::Safe
            && valid(fix, text, origin, resolve)
            && seen.insert(edit_key(fix))
        {
            candidates.push(fix);
        }
    }
    let rejected = conflicting_fixes(&candidates, origin);
    let edits = candidates
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !rejected.contains(index))
        .flat_map(|(_, fix)| fix.edits.clone())
        .collect::<Vec<_>>();
    if edits.is_empty() {
        return None;
    }
    let fix = Fix::safe_edits(edits, "Fix all safe tex-ls issues");
    let changes = code_action::workspace_changes(&fix, uri, &text.line_index(), enc, resolve)?;
    Some(lsp_types::CodeActionResponse::CodeAction(
        lsp_types::CodeAction {
            title: fix.description,
            kind: Some(CodeActionKind::from("source.fixAll.tex-ls")),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            ..Default::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::linter::Diagnostic;
    use tex_ls_analysis::linter::diagnostic::Edit;
    fn finding(fix: Fix) -> Diagnostic {
        Diagnostic {
            path: PathBuf::new(),
            rule: "deprecated-command",
            message: "Issue".into(),
            severity: Severity::Warning,
            start: 0,
            end: 2,
            fix: Some(fix),
            related: Vec::new(),
        }
    }

    #[test]
    fn batched_preferences_match_pairwise_policy_for_every_competitor_subset() {
        let origin = Path::new(fixture_path!("/p/a.tex"));
        let other = PathBuf::from(fixture_path!("/p/b.tex"));
        let fixes = [
            Fix::safe(0, 2, "a", "first"),
            Fix::safe(0, 2, "a", "duplicate with different title"),
            Fix::safe(2, 2, "b", "touching"),
            Fix::safe(3, 4, "c", "separate"),
            Fix::safe_edits(
                vec![Edit::new(8, 10, "x"), Edit::in_file(other, 0, 2, "y")],
                "atomic",
            ),
            Fix::safe_edits(
                vec![Edit::in_file(origin.into(), 0, 2, "a")],
                "explicit origin",
            ),
            Fix::unsafe_(0, 10, "u", "unsafe competitor"),
            Fix::safe(5, 1, "r", "reversed invalid competitor"),
        ];
        for mask in 0..1 << fixes.len() {
            let findings: Vec<_> = fixes
                .iter()
                .enumerate()
                .filter(|(slot, _)| mask & (1 << slot) != 0)
                .map(|(_, fix)| finding(fix.clone()))
                .collect();
            for findings in [findings.clone(), findings.into_iter().rev().collect()] {
                let expected: Vec<_> = findings
                    .iter()
                    .map(|finding| preferred(finding, &findings, origin))
                    .collect();
                assert_eq!(preferred_all(&findings, origin), expected);
            }
        }
    }
    #[test]
    fn conflicting_atomic_fixes_are_all_omitted_with_unicode_bounds_checked() {
        let text = TextBuffer::new("😀 ab cd ef", PositionEncoding::Utf16);
        let uri: Uri = fixture_uri!("/p/a.tex").parse().unwrap();
        let path = Path::new(fixture_path!("/p/a.tex"));
        let foreign = Path::new(fixture_path!("/p/other.tex"));
        let resolve = |p: &Path| (p == foreign).then(|| (path_to_uri(p).unwrap(), "remote".into()));
        let findings = vec![
            finding(Fix::safe_edits(
                vec![
                    Edit::new(5, 7, "A"),
                    Edit::in_file(foreign.into(), 0, 2, "R"),
                ],
                "atomic",
            )),
            finding(Fix::safe(6, 7, "B", "conflicting")),
            finding(Fix::unsafe_(8, 10, "C", "unsafe")),
            finding(Fix::safe(1, 2, "X", "split unicode")),
            finding(Fix::safe(11, 13, "E", "independent")),
        ];
        let action = action(
            &findings,
            &text,
            &uri,
            path,
            PositionEncoding::Utf16,
            &resolve,
        )
        .unwrap();
        let value = serde_json::to_value(action).unwrap();
        let changes = value["edit"]["changes"].as_object().unwrap();
        assert_eq!(changes.len(), 1);
        let edits = changes[uri.as_str()].as_array().unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["newText"], "E");
        assert_eq!(edits[0]["range"]["start"]["character"], 9);
        assert!(!preferred(&findings[0], &findings, path));
    }
}
