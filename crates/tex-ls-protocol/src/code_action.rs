//! Pure code-action logic: turn fix-carrying linter findings into LSP quick-fixes
//! and build conservative syntax-aware refactorings.
//!
//! The threading side (`super::run_code_action`) re-lints the buffer off a fresh
//! snapshot (like the pull-diagnostics path) and hands the raw findings here. This
//! module is rule-agnostic: any finding whose [`tex_ls_analysis::linter::Diagnostic::fix`] is
//! populated and whose caret overlaps the requested range becomes a `QUICKFIX`.
//! Independently, a cursor inside a statically understood table can receive an
//! atomic `REFACTOR_REWRITE` that appends a column to its preamble and rows.
//!
//! Fully-built actions are returned (no
//! `codeAction/resolve` step), and a fix's byte span maps straight to a `TextEdit`
//! via the shared [`super::byte_range_to_lsp`] — the fix owns *what* to rewrite,
//! never *how* to lay it out (tenet 1).

pub mod load_package;

#[cfg(test)]
use crate::convert::path_to_uri;
use crate::{
    convert::{
        byte_range_to_lsp, byte_range_to_lsp_with_cursor, byte_ranges_to_lsp, file_kind_for,
    },
    diagnostics::{self, lint_to_lsp},
    fix_all,
};
#[cfg(test)]
use lsp_types::Position;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionResponse, Range, TextEdit, Uri, WorkspaceEdit,
};
use rowan::TextSize;
#[cfg(test)]
use std::path::PathBuf;
use std::{collections::HashMap, path::Path};
#[cfg(test)]
use tex_ls_analysis::linter::Severity;
use tex_ls_analysis::{
    incremental::Analysis,
    text::{LineIndex, PositionEncoding, TextBuffer},
};
use tex_ls_parser::{
    ast::{AstNode, Command, Environment, Group, children},
    semantic::SignatureDb,
    syntax::{SyntaxElement, SyntaxKind, SyntaxNode},
};

/// Build the quick-fix actions for the findings overlapping `request_range`.
///
/// A finding is offered when it carries a `fix` and its diagnostic span overlaps the
/// request range (inclusive at the edges, so a zero-width cursor sitting on `\bf`
/// matches). Preferred corrections must be safe, repair the finding and have no
/// competing safe correction.
///
/// A fix's edits are grouped into a `WorkspaceEdit` keyed by URI. Most edits target
/// the buffer under `uri` (the finding's own file, `Edit::path` `None`); an edit
/// with `path: Some(p)` targets file `p`, resolved to its `(Uri, text)` via
/// `resolve`. If any foreign target can't be resolved (its text is not in the
/// snapshot), the whole action is dropped rather than emit a partial cross-file
/// edit.
#[allow(clippy::too_many_arguments)]
pub fn code_actions_for_range(
    findings: &[tex_ls_analysis::linter::Diagnostic],
    text: &TextBuffer,
    uri: &Uri,
    self_path: &Path,
    request_range: Range,
    enc: PositionEncoding,
    latex_rules: bool,
    resolve: &dyn Fn(&Path) -> Option<(Uri, String)>,
) -> Vec<lsp_types::CodeActionResponse> {
    let idx = text.line_index();
    let req_start = idx.offset_at(request_range.start.line, request_range.start.character);
    let req_end = idx.offset_at(request_range.end.line, request_range.end.character);

    let offered: Vec<_> = findings
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            byte_ranges_overlap(d.start, d.end, req_start, req_end)
                && d.fix
                    .as_ref()
                    .is_some_and(|fix| fix_all::valid(fix, text, self_path, resolve))
        })
        .collect();
    let preferred = (offered
        .iter()
        .filter(|(_, d)| {
            d.fix.as_ref().unwrap().applicability
                == tex_ls_analysis::linter::diagnostic::Applicability::Safe
        })
        .count()
        > 1)
    .then(|| fix_all::preferred_all(findings, self_path));
    let edits: Vec<_> = offered
        .iter()
        .flat_map(|(_, d)| d.fix.as_ref().unwrap().edits.iter())
        .collect();
    let mut converted = workspace_edits(&edits, uri, &idx, enc, resolve).into_iter();
    let ranges = byte_ranges_to_lsp(&idx, offered.iter().map(|(_, d)| (d.start, d.end)));
    offered
        .into_iter()
        .zip(ranges)
        .filter_map(|((slot, d), range)| {
            let fix = d.fix.as_ref().unwrap();
            let changes = changes_for_edits(converted.by_ref().take(fix.edits.len()))?;
            Some(CodeActionResponse::CodeAction(CodeAction {
                title: fix.description.clone(),
                kind: Some(CodeActionKind::QuickFix),
                // Link the action to the finding it fixes, so the client can dim it
                // once the diagnostic clears.
                diagnostics: Some(vec![diagnostics::lint_to_lsp_with_range(
                    &idx,
                    d.clone(),
                    latex_rules,
                    self_path,
                    range,
                )]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                is_preferred: Some(preferred.as_ref().map_or_else(
                    || fix_all::preferred(d, findings, self_path),
                    |flags| flags[slot],
                )),
                ..Default::default()
            }))
        })
        .collect()
}

/// Suppression is an explicit user choice, never a preferred correction or Fix All.
/// Check the inserted directive against the parser's actual suppression ranges.
/// Offer up to eight construct choices and one file-wide choice per request.
pub fn unknown_command_ignore_actions(
    snapshot: &Analysis,
    path: &Path,
    uri: &Uri,
    text: &TextBuffer,
    range: Range,
    findings: &[tex_ls_analysis::linter::Diagnostic],
) -> Vec<CodeActionResponse> {
    let idx = text.line_index();
    let start = idx.offset_at(range.start.line, range.start.character);
    let end = idx.offset_at(range.end.line, range.end.character);
    let mut actions = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut positions = idx.position_cursor();
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut construct_attempts = 0;
    let mut file_attempted = false;
    for finding in findings
        .iter()
        .filter(|d| d.rule == "unknown-command" && byte_ranges_overlap(d.start, d.end, start, end))
    {
        if construct_attempts == 8 && file_attempted {
            break;
        }
        snapshot.check_cancelled();
        let (line, _) = positions.position(finding.start);
        let line_start = idx.line_start(line as usize);
        let indent_end = text[line_start..]
            .find(|ch| ch != ' ' && ch != '\t')
            .map_or(text.len(), |at| line_start + at);
        let indent = &text[line_start..indent_end];
        for (verb, scope) in [
            ("skip", "for this construct"),
            ("skip-file", "in this file"),
        ] {
            if (verb == "skip" && construct_attempts == 8)
                || (verb == "skip-file" && file_attempted)
            {
                continue;
            }
            if !seen.insert((line_start, verb)) {
                continue;
            }
            if verb == "skip" {
                construct_attempts += 1;
            } else {
                file_attempted = true;
            }
            snapshot.check_cancelled();
            let insert = format!("{indent}% tex-ls {verb} unknown-command{newline}");
            let mut proposed = text.to_string();
            proposed.insert_str(line_start, &insert);
            let parsed = tex_ls_parser::parser::parse_with_declarations(
                &proposed,
                file_kind_for(path).lex_config(),
                snapshot.declarations_for(path),
            );
            let suppressions =
                tex_ls_analysis::linter::suppression::SuppressionMap::build(&parsed.syntax());
            if !suppressions.is_suppressed(
                finding.rule,
                finding.start + insert.len(),
                finding.end + insert.len(),
            ) {
                continue;
            }
            actions.push(CodeActionResponse::CodeAction(CodeAction {
                title: format!("Ignore unknown-command {scope}"),
                kind: Some(CodeActionKind::QuickFix),
                diagnostics: Some(vec![lint_to_lsp(&idx, finding.clone(), true, path)]),
                is_preferred: Some(false),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: byte_range_to_lsp(&idx, line_start, line_start),
                            new_text: insert,
                        }],
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
    }
    actions
}

/// Offer a structural refactoring that appends one centered column to the table
/// enclosing the request's start position.
///
/// The edit is deliberately conservative: the environment must be a non-redefined
/// built-in table, its preamble and every data row must have a statically known,
/// matching width, and comments inside the body cause the action to be withheld.
/// These gates make the workspace edit atomic—either the preamble and every row
/// acquire the same trailing column, or no action is offered.
pub fn table_column_actions(
    root: &SyntaxNode,
    text: &TextBuffer,
    uri: &Uri,
    request_range: Range,
) -> Vec<lsp_types::CodeActionResponse> {
    let definitions = std::cell::LazyCell::new(|| tex_ls_parser::semantic::scan_definitions(root));
    table_column_actions_with_definitions(root, text, uri, request_range, || &definitions)
}

/// Request source signatures only when a built-in table surrounds the cursor.
pub fn table_column_actions_with_definitions<'a>(
    root: &SyntaxNode,
    text: &TextBuffer,
    uri: &Uri,
    request_range: Range,
    definitions: impl FnOnce() -> &'a SignatureDb,
) -> Vec<lsp_types::CodeActionResponse> {
    let idx = text.line_index();
    let offset = idx.offset_at(request_range.start.line, request_range.start.character);
    let Some((spec_insert, row_inserts)) = table_column_insertions(root, text, offset, definitions)
    else {
        return Vec::new();
    };

    let mut edits = Vec::with_capacity(row_inserts.len() + 1);
    let mut positions = idx.position_cursor();
    edits.push(TextEdit {
        range: byte_range_to_lsp_with_cursor(&mut positions, spec_insert, spec_insert),
        new_text: "c".to_string(),
    });
    edits.extend(row_inserts.into_iter().map(|(at, content)| TextEdit {
        range: byte_range_to_lsp_with_cursor(&mut positions, at, at),
        new_text: content,
    }));

    Some(CodeActionResponse::CodeAction(CodeAction {
        title: "Add column at end".to_string(),
        kind: Some(CodeActionKind::RefactorRewrite),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(uri.clone(), edits)])),
            ..Default::default()
        }),
        ..Default::default()
    }))
    .into_iter()
    .collect()
}

fn table_column_insertions<'a>(
    root: &SyntaxNode,
    text: &str,
    offset: usize,
    definitions: impl FnOnce() -> &'a SignatureDb,
) -> Option<(usize, Vec<(usize, String)>)> {
    let offset = TextSize::try_from(offset).ok()?;
    let definitions = std::cell::LazyCell::new(definitions);
    let env = root
        .descendants()
        .filter_map(Environment::cast)
        .filter(|env| env.syntax().text_range().contains_inclusive(offset))
        .filter(|env| {
            env.name().is_some_and(|name| {
                matches!(name.as_str(), "tabular" | "tabular*" | "array")
                    && definitions.environment(&name).is_none()
            })
        })
        .min_by_key(|env| env.syntax().text_range().len())?;
    let begin = env.begin()?;
    let spec = children::<Group>(begin.syntax()).last()?;
    let columns =
        tex_ls_formatter::formatter::column_count(&spec.inner_source()).filter(|&n| n > 0)?;
    let close = spec.syntax().last_token()?;
    if close.kind() != SyntaxKind::R_BRACE {
        return None;
    }

    let body = table_body(env.syntax())?;
    if body.iter().any(|element| {
        element
            .as_token()
            .is_some_and(|token| token.kind() == SyntaxKind::COMMENT)
    }) {
        return None;
    }

    let mut row = Vec::new();
    let mut inserts = Vec::new();
    for element in body {
        if let SyntaxElement::Node(node) = &element
            && node.kind() == SyntaxKind::LINE_BREAK
        {
            if let Some(insert) =
                row_insertion(&row, columns, node.text_range().start(), text, &definitions)?
            {
                inserts.push(insert);
            }
            row.clear();
        } else {
            row.push(element);
        }
    }
    let end = env.end()?.syntax().text_range().start();
    if let Some(insert) = row_insertion(&row, columns, end, text, &definitions)? {
        inserts.push(insert);
    }

    Some((usize::from(close.text_range().start()), inserts))
}

/// Flatten the single paragraph/math wrapper in a table body. Multiple wrappers
/// mean a blank-line boundary, whose row semantics are not safe to rewrite.
fn table_body(env: &SyntaxNode) -> Option<Vec<SyntaxElement>> {
    let mut body = Vec::new();
    let mut wrappers = 0usize;
    for element in env.children_with_tokens() {
        match element {
            SyntaxElement::Node(node)
                if matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END) => {}
            SyntaxElement::Node(node)
                if matches!(node.kind(), SyntaxKind::PARAGRAPH | SyntaxKind::MATH) =>
            {
                wrappers += 1;
                if wrappers > 1 {
                    return None;
                }
                body.extend(node.children_with_tokens());
            }
            other => body.push(other),
        }
    }
    Some(body)
}

fn row_insertion(
    row: &[SyntaxElement],
    columns: usize,
    boundary: TextSize,
    text: &str,
    definitions: &tex_ls_parser::semantic::SignatureDb,
) -> Option<Option<(usize, String)>> {
    let structural: Vec<&SyntaxElement> = row
        .iter()
        .filter(|element| !is_table_trivia(element) && !is_rule_command(element, definitions))
        .collect();
    if structural.is_empty() {
        return Some(None);
    }

    let mut cell = Vec::new();
    let mut used = 0usize;
    for element in &structural {
        if element.kind() == SyntaxKind::AMPERSAND {
            used = used.checked_add(table_cell_span(&cell)?)?;
            cell.clear();
        } else {
            cell.push(*element);
        }
    }
    used = used.checked_add(table_cell_span(&cell)?)?;
    if used != columns {
        return None;
    }

    let anchor = structural.last()?.text_range().end();
    let gap = text.get(usize::from(anchor)..usize::from(boundary))?;
    if !gap.chars().all(char::is_whitespace) {
        return None;
    }
    let content = if gap.is_empty() { " & " } else { " &" };
    Some(Some((usize::from(anchor), content.to_string())))
}

fn table_cell_span(cell: &[&SyntaxElement]) -> Option<usize> {
    let commands: Vec<Command> = cell
        .iter()
        .filter_map(|element| element.as_node())
        .filter_map(|node| Command::cast(node.clone()))
        .filter(|command| command.name().as_deref() == Some("multicolumn"))
        .collect();
    if commands.is_empty() {
        return Some(1);
    }
    if cell.len() != 1 || commands.len() != 1 {
        return None;
    }
    commands[0]
        .nth_group_text(0)?
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|&span| span > 0)
}

fn is_table_trivia(element: &SyntaxElement) -> bool {
    element
        .as_token()
        .is_some_and(|token| matches!(token.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE))
}

fn is_rule_command(
    element: &SyntaxElement,
    definitions: &tex_ls_parser::semantic::SignatureDb,
) -> bool {
    let Some(command) = element
        .as_node()
        .and_then(|node| Command::cast(node.clone()))
    else {
        return false;
    };
    command.name().is_some_and(|name| {
        definitions.command(&name).is_none()
            && tex_ls_parser::semantic::signature::builtin()
                .command(&name)
                .is_some_and(|signature| signature.rule)
    })
}

/// Group a fix's edits into a `WorkspaceEdit`'s per-URI `TextEdit` lists.
///
/// `None` edits map through `self_idx` onto `self_uri` (the finding's own file);
/// `Some(p)` edits are converted through a `LineIndex` built from `p`'s resolved
/// text. Returns `None` — dropping the whole action — if any foreign file fails to
/// resolve, so a cross-file fix is never offered half-applied.
pub(crate) fn workspace_changes(
    fix: &tex_ls_analysis::linter::Fix,
    self_uri: &Uri,
    self_idx: &LineIndex,
    enc: PositionEncoding,
    resolve: &dyn Fn(&Path) -> Option<(Uri, String)>,
) -> Option<HashMap<Uri, Vec<TextEdit>>> {
    let edits: Vec<_> = fix.edits.iter().collect();
    changes_for_edits(workspace_edits(&edits, self_uri, self_idx, enc, resolve).into_iter())
}

fn workspace_edits(
    edits: &[&tex_ls_analysis::linter::diagnostic::Edit],
    self_uri: &Uri,
    self_idx: &LineIndex,
    enc: PositionEncoding,
    resolve: &dyn Fn(&Path) -> Option<(Uri, String)>,
) -> Vec<Option<(Uri, TextEdit)>> {
    let mut groups: HashMap<Option<&Path>, Vec<usize>> = HashMap::new();
    for (slot, edit) in edits.iter().enumerate() {
        groups.entry(edit.path.as_deref()).or_default().push(slot);
    }
    let mut converted = vec![None; edits.len()];
    for (path, mut slots) in groups {
        if !slots
            .windows(2)
            .all(|pair| edits[pair[0]].start <= edits[pair[1]].start)
        {
            slots.sort_unstable_by_key(|&slot| edits[slot].start);
        }
        let mut convert = |uri: &Uri, idx: &LineIndex| {
            let mut cursor = idx.position_cursor();
            for &slot in &slots {
                let edit = edits[slot];
                converted[slot] = Some((
                    uri.clone(),
                    TextEdit {
                        range: byte_range_to_lsp_with_cursor(&mut cursor, edit.start, edit.end),
                        new_text: edit.content.clone(),
                    },
                ));
            }
        };
        if let Some(path) = path {
            let Some((uri, text)) = resolve(path) else {
                continue;
            };
            let idx = LineIndex::with_encoding(&text, enc);
            convert(&uri, &idx);
        } else {
            convert(self_uri, self_idx);
        }
    }
    converted
}

fn changes_for_edits(
    edits: impl Iterator<Item = Option<(Uri, TextEdit)>>,
) -> Option<HashMap<Uri, Vec<TextEdit>>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    let mut complete = true;
    for edit in edits {
        if let Some((uri, edit)) = edit {
            changes.entry(uri).or_default().push(edit);
        } else {
            complete = false;
        }
    }
    complete.then_some(changes)
}

/// Whether two byte ranges overlap, inclusive at the edges (a zero-width cursor at a
/// range boundary counts as touching it).
fn byte_ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start <= b_end && b_start <= a_end
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_refactoring_probe_without_a_table_does_not_request_signatures() {
        let source = "ordinary prose";
        let root = tex_ls_parser::parser::parse(source).syntax();
        assert!(
            table_column_actions_with_definitions(
                &root,
                &buf(source),
                &uri(),
                Range::new(Position::new(0, 3), Position::new(0, 3)),
                || panic!("a prose action request must not scan definitions"),
            )
            .is_empty()
        );
    }

    use super::*;
    use tex_ls_analysis::linter::check_document;
    use tex_ls_parser::parser::LexConfig;

    #[test]
    fn batched_fix_edits_keep_interleaved_targets_and_original_order() {
        use tex_ls_analysis::linter::diagnostic::{Edit, Fix};
        let source = "é😀 text\r\n𝕏 tail";
        let foreign = PathBuf::from("other.tex");
        let foreign_uri: Uri = fixture_uri!("/other.tex").parse().unwrap();
        let fix = Fix::safe_edits(
            vec![
                Edit::new(13, 17, "late"),
                Edit::in_file(foreign.clone(), 7, 11, "remote"),
                Edit::new(0, 6, "first"),
                Edit::in_file(foreign.clone(), 0, 6, "again"),
                Edit::new(13, 17, "duplicate"),
                Edit::new(usize::MAX, usize::MAX, "clamped"),
            ],
            "batch",
        );
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let idx = LineIndex::with_encoding(source, enc);
            let calls = std::cell::Cell::new(0);
            let resolve = |path: &Path| {
                assert_eq!(path, foreign);
                calls.set(calls.get() + 1);
                Some((foreign_uri.clone(), source.to_owned()))
            };
            let mut expected: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
            for edit in &fix.edits {
                let uri = if edit.path.is_some() {
                    &foreign_uri
                } else {
                    &uri()
                };
                expected.entry(uri.clone()).or_default().push(TextEdit {
                    range: byte_range_to_lsp(&idx, edit.start, edit.end),
                    new_text: edit.content.clone(),
                });
            }
            assert_eq!(
                workspace_changes(&fix, &uri(), &idx, enc, &resolve),
                Some(expected)
            );
            assert_eq!(calls.get(), 1);
            assert!(workspace_changes(&fix, &uri(), &idx, enc, &|_| None).is_none());
        }
    }

    #[test]
    fn an_unresolved_action_does_not_shift_later_batched_corrections() {
        use tex_ls_analysis::linter::diagnostic::{Diagnostic, Edit, Fix};
        let source = "😀 abc def gh";
        let path = Path::new(fixture_path!("/actions/main.tex"));
        let uri = path_to_uri(path).unwrap();
        let fixes = [
            Fix::safe(5, 6, "A", "first"),
            Fix::safe_edits(
                vec![
                    Edit::new(9, 10, "D"),
                    Edit::in_file(path.into(), 0, 0, "prefix"),
                ],
                "unresolved",
            ),
            Fix::safe(13, 14, "G", "last"),
        ];
        let findings: Vec<_> = fixes
            .into_iter()
            .map(|fix| Diagnostic {
                path: path.into(),
                rule: "synthetic",
                severity: Severity::Warning,
                message: "issue".into(),
                start: fix.edits[0].start,
                end: fix.edits[0].end,
                fix: Some(fix),
                related: Vec::new(),
            })
            .collect();
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let buffer = TextBuffer::new(source, enc);
            let idx = buffer.line_index();
            let (line, character) = idx.position(source.len());
            let range = Range::new(Position::default(), Position::new(line, character));
            let actions =
                code_actions_for_range(&findings, &buffer, &uri, path, range, enc, true, &|_| None);
            assert_eq!(actions.len(), 2);
            for (action, finding) in actions.iter().zip([&findings[0], &findings[2]]) {
                let CodeActionResponse::CodeAction(action) = action else {
                    panic!("literal correction")
                };
                let fix = finding.fix.as_ref().unwrap();
                assert_eq!(action.title, fix.description);
                assert_eq!(
                    action.diagnostics.as_ref().unwrap()[0],
                    lint_to_lsp(&idx, finding.clone(), true, path)
                );
                assert_eq!(
                    action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri][0],
                    TextEdit {
                        range: byte_range_to_lsp(&idx, fix.edits[0].start, fix.edits[0].end),
                        new_text: fix.edits[0].content.clone(),
                    }
                );
            }
        }
    }

    fn uri() -> Uri {
        fixture_uri!("/x.tex").parse().unwrap()
    }

    fn full_range(text: &str) -> Range {
        let idx = LineIndex::new(text);
        let (el, ec) = idx.position(text.len());
        Range {
            start: Position::new(0, 0),
            end: Position::new(el, ec),
        }
    }

    /// Lint `src` as a `.tex` document and return the raw findings.
    fn findings(src: &str) -> Vec<tex_ls_analysis::linter::Diagnostic> {
        check_document(
            std::path::Path::new("x.tex"),
            &buf(src),
            LexConfig::default(),
            &tex_ls_parser::declarations::ResolvedDeclarations::default(),
        )
    }

    /// The document buffer the entry point takes, over a test source.
    fn buf(src: &str) -> TextBuffer {
        TextBuffer::new(src, PositionEncoding::Utf16)
    }

    /// A resolver that knows no foreign files — the single-file default.
    fn no_resolve(_: &Path) -> Option<(Uri, String)> {
        None
    }

    #[test]
    fn offers_quickfix_for_deprecated_command_in_range() {
        let src = "{\\bf hi}\n";
        let actions = code_actions_for_range(
            &findings(src),
            &buf(src),
            &uri(),
            std::path::Path::new("x.tex"),
            full_range(src),
            PositionEncoding::Utf16,
            true,
            &no_resolve,
        );
        let CodeActionResponse::CodeAction(action) = actions
            .iter()
            .find(
                |a| matches!(a, CodeActionResponse::CodeAction(a) if a.title.contains("bfseries")),
            )
            .expect("a `\\bf` → `\\bfseries` quick-fix")
        else {
            unreachable!()
        };
        assert_eq!(action.kind, Some(CodeActionKind::QuickFix));
        assert_eq!(action.is_preferred, Some(false));
        let edits = action
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .and_then(|c| c.get(&uri()))
            .expect("a single-file edit");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "\\bfseries");
        // The edit replaces just `\bf` (line 0, chars 1..4).
        assert_eq!(edits[0].range.start, Position::new(0, 1));
        assert_eq!(edits[0].range.end, Position::new(0, 4));
    }

    #[test]
    fn one_quickfix_repairs_a_whole_quotation() {
        // A straight-quote pair is one finding carrying both edits, so the editor
        // offers a single action -- with the caret on either quote -- that rewrites
        // the quotation in one step, instead of one action per quote.
        let src = "He said \"hi\" now\n";
        for caret in [9, 11] {
            let cursor = Range {
                start: Position::new(0, caret),
                end: Position::new(0, caret),
            };
            let actions = code_actions_for_range(
                &findings(src),
                &buf(src),
                &uri(),
                std::path::Path::new("x.tex"),
                cursor,
                PositionEncoding::Utf16,
                true,
                &no_resolve,
            );
            let quotes: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    CodeActionResponse::CodeAction(a) if a.title.contains("straight quotes") => {
                        Some(a)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(quotes.len(), 1, "caret {caret}: {actions:?}");
            let edits = quotes[0]
                .edit
                .as_ref()
                .and_then(|e| e.changes.as_ref())
                .and_then(|c| c.get(&uri()))
                .expect("a single-file edit");
            assert_eq!(edits.len(), 2);
            assert_eq!(edits[0].new_text, "``");
            assert_eq!(edits[0].range.start, Position::new(0, 8));
            assert_eq!(edits[1].new_text, "''");
            assert_eq!(edits[1].range.start, Position::new(0, 11));
            // Direction is inferred, so the fix stays unsafe and never preferred.
            assert_eq!(quotes[0].is_preferred, Some(false));
        }
    }

    #[test]
    fn empty_when_range_misses_the_finding() {
        let src = "ok\n{\\bf hi}\n";
        // A zero-width cursor on line 0 (the `ok` prose), nowhere near `\bf`.
        let cursor = Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        };
        let actions = code_actions_for_range(
            &findings(src),
            &buf(src),
            &uri(),
            std::path::Path::new("x.tex"),
            cursor,
            PositionEncoding::Utf16,
            true,
            &no_resolve,
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn surfaces_dollar_display_math_fix() {
        let src = "$$x = y$$\n";
        let actions = code_actions_for_range(
            &findings(src),
            &buf(src),
            &uri(),
            std::path::Path::new("x.tex"),
            full_range(src),
            PositionEncoding::Utf16,
            true,
            &no_resolve,
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            CodeActionResponse::CodeAction(a) if a.title.contains("\\[")
        )));
    }

    /// A synthetic cross-file fix (an `Edit::in_file` into another buffer) becomes
    /// a single `WorkspaceEdit` spanning both URIs when the foreign file resolves.
    #[test]
    fn cross_file_fix_spans_two_uris() {
        use tex_ls_analysis::linter::{Diagnostic, Edit, Fix, Severity};

        let other_uri: Uri = fixture_uri!("/other.tex").parse().unwrap();
        let other_text = "\\ref{x}\n";
        let resolve = |p: &Path| -> Option<(Uri, String)> {
            (p == Path::new("other.tex")).then(|| (other_uri.clone(), other_text.to_string()))
        };

        // Own-file finding on `\label{x}` at 7..8, plus a cross-file edit renaming
        // the `\ref{x}` key (5..6) in other.tex.
        let src = "\\label{x}\n";
        let d = Diagnostic {
            rule: "synthetic",
            severity: Severity::Warning,
            path: std::path::PathBuf::from("x.tex"),
            start: 7,
            end: 8,
            message: "rename x".into(),
            fix: Some(Fix::safe_edits(
                vec![
                    Edit::new(7, 8, "y"),
                    Edit::in_file(PathBuf::from("other.tex"), 5, 6, "y"),
                ],
                "rename x to y",
            )),
            related: Vec::new(),
        };

        let actions = code_actions_for_range(
            std::slice::from_ref(&d),
            &buf(src),
            &uri(),
            std::path::Path::new("x.tex"),
            full_range(src),
            PositionEncoding::Utf16,
            true,
            &resolve,
        );
        let CodeActionResponse::CodeAction(action) = actions
            .iter()
            .find(|a| matches!(a, CodeActionResponse::CodeAction(a) if a.title == "rename x to y"))
            .expect("the cross-file quick-fix")
        else {
            unreachable!()
        };
        let changes = action
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .expect("workspace changes");
        assert_eq!(changes.len(), 2, "one entry per touched file");
        assert_eq!(changes[&uri()][0].new_text, "y");
        assert_eq!(changes[&other_uri][0].new_text, "y");
        // The foreign edit's range is computed from other.tex, not the buffer.
        assert_eq!(changes[&other_uri][0].range.start, Position::new(0, 5));
    }

    /// If a cross-file fix's foreign target can't be resolved, the whole action is
    /// dropped — never a half-applied edit.
    #[test]
    fn cross_file_fix_dropped_when_target_unresolved() {
        use tex_ls_analysis::linter::{Diagnostic, Edit, Fix, Severity};

        let src = "\\label{x}\n";
        let d = Diagnostic {
            rule: "synthetic",
            severity: Severity::Warning,
            path: std::path::PathBuf::from("x.tex"),
            start: 7,
            end: 8,
            message: "rename x".into(),
            fix: Some(Fix::safe_edits(
                vec![
                    Edit::new(7, 8, "y"),
                    Edit::in_file(PathBuf::from("gone.tex"), 5, 6, "y"),
                ],
                "rename x to y",
            )),
            related: Vec::new(),
        };
        let actions = code_actions_for_range(
            std::slice::from_ref(&d),
            &buf(src),
            &uri(),
            std::path::Path::new("x.tex"),
            full_range(src),
            PositionEncoding::Utf16,
            true,
            &no_resolve,
        );
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                CodeActionResponse::CodeAction(a) if a.title == "rename x to y"
            )),
            "unresolved cross-file fix must not be offered"
        );
    }

    fn apply_table_action(src: &str, line: u32, character: u32) -> Option<(CodeAction, String)> {
        let root = SyntaxNode::new_root(tex_ls_parser::parser::parse(src).green);
        let buffer = buf(src);
        let cursor = Range::new(
            Position::new(line, character),
            Position::new(line, character),
        );
        let mut actions = table_column_actions(&root, &buffer, &uri(), cursor);
        let CodeActionResponse::CodeAction(action) = actions.pop()? else {
            return None;
        };
        let edits = action.edit.as_ref()?.changes.as_ref()?.get(&uri())?;
        let idx = buffer.line_index();
        let mut byte_edits: Vec<_> = edits
            .iter()
            .map(|edit| {
                let start = idx.offset_at(edit.range.start.line, edit.range.start.character);
                let end = idx.offset_at(edit.range.end.line, edit.range.end.character);
                (start, end, edit.new_text.as_str())
            })
            .collect();
        byte_edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
        let mut output = src.to_string();
        for (start, end, replacement) in byte_edits {
            output.replace_range(start..end, replacement);
        }
        Some((action, output))
    }

    #[test]
    fn adds_a_trailing_column_to_every_table_row() {
        let src = "\\begin{tabular}{lr}\n  a & b \\\\\n  c & d \\\\\n\\end{tabular}\n";
        let (action, output) = apply_table_action(src, 1, 3).expect("table refactor");
        assert_eq!(action.title, "Add column at end");
        assert_eq!(action.kind, Some(CodeActionKind::RefactorRewrite));
        assert_eq!(
            output,
            "\\begin{tabular}{lrc}\n  a & b & \\\\\n  c & d & \\\\\n\\end{tabular}\n"
        );
    }

    #[test]
    fn adds_a_column_to_an_unterminated_final_row() {
        let src = "\\begin{tabular}{cc}\n  a & b\n\\end{tabular}\n";
        let (_, output) = apply_table_action(src, 1, 3).expect("table refactor");
        assert_eq!(output, "\\begin{tabular}{ccc}\n  a & b &\n\\end{tabular}\n");
    }

    #[test]
    fn declines_short_rows_and_dynamic_spans() {
        let short = "\\begin{tabular}{ccc}\n  a & b \\\\\n\\end{tabular}\n";
        assert!(apply_table_action(short, 1, 3).is_none());

        let dynamic = "\\begin{tabular}{cc}\n  \\multicolumn{\\n}{c}{a} \\\\\n\\end{tabular}\n";
        assert!(apply_table_action(dynamic, 1, 3).is_none());
    }

    #[test]
    fn handles_static_multicolumn_rows_and_rule_lines() {
        let src = "\\begin{tabular}{cc}\n  \\toprule\n  \\multicolumn{2}{c}{heading} \\\\\n  a & b \\\\\n  \\bottomrule\n\\end{tabular}\n";
        let (_, output) = apply_table_action(src, 3, 3).expect("table refactor");
        assert_eq!(
            output,
            "\\begin{tabular}{ccc}\n  \\toprule\n  \\multicolumn{2}{c}{heading} & \\\\\n  a & b & \\\\\n  \\bottomrule\n\\end{tabular}\n"
        );
    }

    #[test]
    fn declines_custom_preambles_redefinitions_and_outside_cursors() {
        let custom = "\\begin{tabular}{XX}\n  a & b \\\\\n\\end{tabular}\n";
        assert!(apply_table_action(custom, 1, 3).is_none());

        let redefined =
            "\\renewenvironment{tabular}[1]{}{}\n\\begin{tabular}{c}\n  a \\\\\n\\end{tabular}\n";
        assert!(apply_table_action(redefined, 2, 3).is_none());

        let outside = "prose\n\\begin{tabular}{c}\n  a \\\\\n\\end{tabular}\n";
        assert!(apply_table_action(outside, 0, 2).is_none());
    }

    #[test]
    fn covers_tabular_star_and_array_but_declines_commented_tables() {
        let tabular_star = "\\begin{tabular*}{4cm}{c}\n  a \\\\\n\\end{tabular*}\n";
        let (_, output) = apply_table_action(tabular_star, 1, 3).expect("tabular* refactor");
        assert_eq!(
            output,
            "\\begin{tabular*}{4cm}{cc}\n  a & \\\\\n\\end{tabular*}\n"
        );

        let array = "\\begin{array}{c}\n  a \\\\\n\\end{array}\n";
        let (_, output) = apply_table_action(array, 1, 3).expect("array refactor");
        assert_eq!(output, "\\begin{array}{cc}\n  a & \\\\\n\\end{array}\n");

        let commented = "\\begin{tabular}{c}\n  a \\\\ % why\n  b \\\\\n\\end{tabular}\n";
        assert!(apply_table_action(commented, 1, 3).is_none());
    }
}
