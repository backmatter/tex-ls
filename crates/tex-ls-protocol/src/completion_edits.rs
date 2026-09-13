//! Exact replacement spans and source-aware structural insertion.
use super::*;

pub fn attach(
    snapshot: &Analysis,
    path: &Path,
    offset: usize,
    enc: PositionEncoding,
    items: &mut [CompletionItem],
) {
    let Some(file) = snapshot.lookup_file(path) else {
        return;
    };
    let source = snapshot.file_text(file);
    let bib_command = if file_kind_for(path) == FileKind::Bib {
        tex_ls_analysis::bib::completion::tex_command_completion(
            &snapshot.parsed_bib_tree(file),
            offset,
        )
    } else {
        None
    };
    let context = if file_kind_for(path) == FileKind::Bib {
        bib_command
            .as_ref()
            .map(|(_, prefix)| CompletionContext::CommandName {
                prefix: prefix.clone(),
            })
    } else {
        Some(
            tex_ls_analysis::completion::classify_context_with_declarations(
                &snapshot.parsed_tree(file),
                offset,
                snapshot.declarations_for(path),
            ),
        )
    };
    let command = matches!(context, Some(CompletionContext::CommandName { .. }));
    let key = matches!(
        context,
        Some(
            CompletionContext::CitationKey { .. }
                | CompletionContext::LabelRef { .. }
                | CompletionContext::LabelDefinition { .. }
        )
    );
    let environment = matches!(context, Some(CompletionContext::EnvironmentName { .. }));
    let file_path = matches!(context, Some(CompletionContext::FilePath { .. }));
    let role = if file_path {
        let root = snapshot.parsed_tree(file);
        root.descendants()
            .filter(|node| {
                node.kind() == SyntaxKind::COMMAND
                    && node
                        .text_range()
                        .contains_inclusive(TextSize::new(offset as u32))
            })
            .min_by_key(|node| node.text_range().len())
            .and_then(|node| tex_ls_parser::ast::command_name(&node))
            .and_then(|name| tex_ls_parser::semantic::roles::file_role(&name))
    } else {
        None
    };
    let allowed = |ch: char| {
        if command {
            ch.is_alphabetic() || "@_:".contains(ch)
        } else if key {
            !",{}[]\\\n\r".contains(ch)
        } else if environment {
            ch.is_alphanumeric() || "@_:*-.".contains(ch)
        } else if file_path {
            !"/{}[]\n\r\\".contains(ch) && !(ch == ',' && role.is_some_and(|role| role.list))
        } else {
            !ch.is_whitespace() && !",{}[]\\\"=#@".contains(ch)
        }
    };
    // Bare filenames have no brace delimiter. Their CST range separates the
    // filename from the command head and from following prose/trivia.
    let bounds = if file_path {
        snapshot.parsed_tree(file).descendants().find_map(|node| {
            (node.kind() == SyntaxKind::BARE_ARGUMENT
                && node
                    .text_range()
                    .contains_inclusive(TextSize::from(offset as u32)))
            .then(|| {
                (
                    usize::from(node.text_range().start()),
                    usize::from(node.text_range().end()),
                )
            })
        })
    } else {
        None
    };
    let (lower, upper) = bounds.unwrap_or((0, source.len()));
    let start = source[lower..offset]
        .char_indices()
        .rev()
        .find(|(_, ch)| !allowed(*ch))
        .map_or(lower, |(index, ch)| lower + index + ch.len_utf8());
    let end = source[offset..upper]
        .char_indices()
        .find(|(_, ch)| !allowed(*ch))
        .map_or(upper, |(index, _)| offset + index);
    let start = start + source[start..offset].len() - source[start..offset].trim_start().len();
    let end = offset + source[offset..end].trim_end().len();
    let (start, end) = bib_command.map_or((start, end), |(range, _)| {
        (usize::from(range.start()), usize::from(range.end()))
    });
    let (start, end) = match &context {
        Some(CompletionContext::Option { range, .. }) => {
            (usize::from(range.start()), usize::from(range.end()))
        }
        _ => (start, end),
    };
    let index = snapshot.file_line_index(file, enc);
    let range = lsp_range(
        &index,
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
    );
    let begin = matches!(
        context,
        Some(CompletionContext::EnvironmentName { closing: false, .. })
    );
    let suffix = &source[end..];
    // A skeleton is appropriate only at the empty tail of an unfinished begin.
    // Reuse an existing final brace as the end-name's closing brace.
    let skeleton = begin
        && (suffix.is_empty()
            || suffix
                .strip_prefix('}')
                .is_some_and(|rest| rest.trim().is_empty()));
    for item in items {
        let mut edit_range = range;
        let mut text = item
            .insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone());
        if file_path {
            if item.kind == Some(CompletionItemKind::Folder) {
                text.push('/');
            } else if let Some(role) = role {
                use tex_ls_parser::semantic::roles::FileRoleKind;
                let extension = match role.kind {
                    FileRoleKind::Source(_) => Some(".tex"),
                    FileRoleKind::Bibliography if role.list => Some(".bib"),
                    _ => None,
                };
                if let Some(stem) = extension.and_then(|extension| text.strip_suffix(extension))
                    && !stem.contains('.')
                    && !source[start..end].contains('.')
                {
                    text = stem.into();
                }
            }
        }
        if skeleton {
            let name = item
                .label
                .replace('\\', "\\\\")
                .replace('$', "\\$")
                .replace('}', "\\}");
            text = format!("{name}}}\n\t$0\n\\\\end{{{name}");
            if !suffix.starts_with('}') {
                text.push('}');
            }
            item.insert_text_format = Some(InsertTextFormat::Snippet);
        }
        if let Some((lower, upper)) = bounds
            && text.chars().any(char::is_whitespace)
        {
            // A space terminates bare input. Group the entire filename, including
            // path segments outside this completion edit, without consuming prose.
            text = format!(
                "{{{}{}{}}}",
                &source[lower..start],
                text,
                &source[end..upper]
            );
            edit_range = byte_range_to_lsp(&index, lower, upper);
        }
        item.insert_text = None;
        item.text_edit = Some(
            TextEdit {
                range: edit_range,
                new_text: text,
            }
            .into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tex_ls_analysis::incremental::IncrementalDatabase;

    fn item(marked: &str, label: &str, enc: PositionEncoding, rich: bool) -> (String, Value) {
        let cursor = marked.find('|').unwrap();
        let source = marked.replacen('|', "", 1);
        let path = Path::new(fixture_path!("/completion/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, source.as_str(), None);
        let index = LineIndex::with_encoding(&source, enc);
        let position = lsp_range(&index, TextRange::empty(TextSize::new(cursor as u32))).start;
        let snapshot = db.snapshot();
        let items =
            compute_completion(&snapshot, &path_to_uri(path).unwrap(), path, enc, position).items;
        let found = items
            .into_iter()
            .find(|item| item.label == label)
            .expect("candidate");
        let mut value = serde_json::to_value(found).unwrap();
        let policy = ResponsePolicy::new(
            &json!({"capabilities":{"textDocument":{"completion":{"completionItem":{
                "snippetSupport":rich, "insertReplaceSupport":rich
            }}}}}),
        );
        policy.response("completionItem/resolve", None, &mut value);
        (source, value)
    }

    fn apply(source: &str, item: &Value, choice: &str, enc: PositionEncoding) -> String {
        let edit = &item["textEdit"];
        let range: Range = serde_json::from_value(
            edit.get(choice)
                .or_else(|| edit.get("range"))
                .unwrap()
                .clone(),
        )
        .unwrap();
        let index = LineIndex::with_encoding(source, enc);
        let start = index.offset_at(range.start.line, range.start.character);
        let end = index.offset_at(range.end.line, range.end.character);
        let mut result = source.to_owned();
        let mut text = edit["newText"].as_str().unwrap().to_owned();
        if item["insertTextFormat"] == 2 {
            text = text.replace("$0", "").replace("\\\\", "\\");
        }
        result.replace_range(start..end, &text);
        result
    }

    #[test]
    fn path_segments_and_bib_identifiers_replace_suffixes_without_touching_delimiters() {
        for (path, marked, label, expected) in [
            (
                fixture_path!("/completion/main.tex"),
                r"\input{chap/pa|rt-old.tex}",
                "part-new.tex",
                r"\input{chap/part-new.tex}",
            ),
            (
                fixture_path!("/completion/main.tex"),
                r"\input{pa|rt-old}",
                "part-new.tex",
                r"\input{part-new}",
            ),
            (
                fixture_path!("/completion/main.tex"),
                r"\includegraphics{plot,pa|rt-old.png}",
                "plot,part-new.png",
                r"\includegraphics{plot,part-new.png}",
            ),
            (
                fixture_path!("/completion/refs.bib"),
                r"@article{key, title=br|oken}",
                "brand",
                r"@article{key, title=brand}",
            ),
        ] {
            let offset = marked.find('|').unwrap();
            let source = marked.replace('|', "");
            let path = Path::new(path);
            let mut db = IncrementalDatabase::default();
            db.apply_change(path, source.as_str(), None);
            let mut items = vec![CompletionItem {
                label: label.into(),
                kind: Some(CompletionItemKind::File),
                ..Default::default()
            }];
            attach(
                &db.snapshot(),
                path,
                offset,
                PositionEncoding::Utf16,
                &mut items,
            );
            let item = serde_json::to_value(&items[0]).unwrap();
            assert_eq!(
                apply(&source, &item, "replace", PositionEncoding::Utf16),
                expected
            );
        }
    }

    #[test]
    fn bare_input_edits_preserve_the_head_and_following_text() {
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            for rich in [false, true] {
                for (marked, expected) in [
                    ("😀 \\input cha|", "😀 \\input chapter"),
                    ("\\input |chapter-old after", "\\input chapter after"),
                    ("\\input cha|pter-old after", "\\input chapter after"),
                    ("\\input chapter|\nnext", "\\input chapter\nnext"),
                    (
                        "\\input% keep\n cha|pter-old next",
                        "\\input% keep\n chapter next",
                    ),
                    (
                        "\\input parts/cha|pter-old.tex next",
                        "\\input parts/chapter.tex next",
                    ),
                ] {
                    let offset = marked.find('|').unwrap();
                    let source = marked.replace('|', "");
                    let path = Path::new(fixture_path!("/completion/main.tex"));
                    let mut db = IncrementalDatabase::default();
                    db.apply_change(path, source.as_str(), None);
                    let mut items = vec![CompletionItem {
                        label: "chapter.tex".into(),
                        kind: Some(CompletionItemKind::File),
                        ..Default::default()
                    }];
                    attach(&db.snapshot(), path, offset, enc, &mut items);
                    let mut value = serde_json::to_value(&items[0]).unwrap();
                    ResponsePolicy::new(&json!({"capabilities":{"textDocument":{"completion":{"completionItem":{"insertReplaceSupport":rich}}}}}))
                        .response("completionItem/resolve", None, &mut value);
                    for choice in ["insert", "replace"] {
                        assert_eq!(apply(&source, &value, choice, enc), expected, "{marked}");
                    }
                }
            }
        }
    }

    #[test]
    fn file_completion_does_not_insert_tex_syntax_as_a_filename() {
        let path = Path::new(fixture_path!("/completion/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, "\\input{bad}", None);
        for name in ["bad%comment.tex", "bad#parameter.tex", "bad\"quote.tex"] {
            db.apply_change(
                &Path::new(fixture_path!("/completion")).join(name),
                "Text",
                None,
            );
        }
        let result = compute_completion(
            &db.snapshot(),
            &path_to_uri(path).unwrap(),
            path,
            PositionEncoding::Utf16,
            Position::new(0, 10),
        );
        assert!(result.items.is_empty(), "{:?}", result.items);
    }

    #[test]
    fn bib_accent_completion_replaces_the_existing_control_symbol() {
        let source = r#"@article{key, title={\"{O}resund}}"#;
        let offset = source.find('\\').unwrap() + 1;
        let path = Path::new(fixture_path!("/completion/refs.bib"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, source, None);
        let snapshot = db.snapshot();
        let items = compute_completion(
            &snapshot,
            &path_to_uri(path).unwrap(),
            path,
            PositionEncoding::Utf16,
            Position::new(0, offset as u32),
        );
        let item = items
            .items
            .iter()
            .find(|item| item.label == "'")
            .expect("accent candidate");
        assert_eq!(
            apply(
                source,
                &serde_json::to_value(item).unwrap(),
                "replace",
                PositionEncoding::Utf16
            ),
            r"@article{key, title={\'{O}resund}}"
        );
    }

    #[test]
    fn bare_input_completion_groups_filenames_with_spaces() {
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            for marked in [
                "😀 \\input ne|w after",
                "\\input parts/ne|w.tex after",
                "\\input ne|w/child.tex\n",
            ] {
                let offset = marked.find('|').unwrap();
                let source = marked.replace('|', "");
                let path = Path::new(fixture_path!("/completion/main.tex"));
                let mut db = IncrementalDatabase::default();
                db.apply_change(path, source.as_str(), None);
                let mut items = vec![CompletionItem {
                    label: "new chapter.tex".into(),
                    kind: Some(CompletionItemKind::File),
                    ..Default::default()
                }];
                attach(&db.snapshot(), path, offset, enc, &mut items);
                for rich in [false, true] {
                    let mut value = serde_json::to_value(&items[0]).unwrap();
                    let policy = ResponsePolicy::new(
                        &json!({"capabilities":{"textDocument":{"completion":{"completionItem":{"insertReplaceSupport":rich}}}}}),
                    );
                    policy.response("completionItem/resolve", None, &mut value);
                    for choice in ["insert", "replace"] {
                        let applied = apply(&source, &value, choice, enc);
                        let expected = if source.contains("parts/") {
                            "\\input {parts/new chapter.tex} after"
                        } else if source.contains("/child") {
                            "\\input {new chapter/child.tex}\n"
                        } else {
                            "😀 \\input {new chapter} after"
                        };
                        assert_eq!(applied, expected);
                        let tree =
                            SyntaxNode::new_root(tex_ls_parser::parser::parse(&applied).green);
                        assert_eq!(tree.text().to_string(), applied);
                        let command = tree
                            .descendants()
                            .find(|node| {
                                tex_ls_parser::ast::command_name(node).as_deref() == Some("input")
                            })
                            .unwrap();
                        assert!(
                            tex_ls_parser::ast::nth_group_inner(&command, 0)
                                .unwrap()
                                .1
                                .contains("new chapter")
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn completion_resolve_keeps_eager_edits_unchanged() {
        let path = Path::new(fixture_path!("/completion/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, r"\sectobsolete", None);
        let snapshot = db.snapshot();
        let item = compute_completion(
            &snapshot,
            &path_to_uri(path).unwrap(),
            path,
            PositionEncoding::Utf16,
            Position::new(0, 5),
        )
        .items
        .into_iter()
        .find(|item| item.label == "section")
        .unwrap();
        let resolved = completion_resolve::resolve(&snapshot, item.clone());
        assert_eq!(item.text_edit, resolved.text_edit);
        assert_eq!(item.insert_text, resolved.insert_text);
        assert_eq!(item.filter_text, resolved.filter_text);
        assert_eq!(item.sort_text, resolved.sort_text);
    }

    #[test]
    fn full_key_and_command_edits_work_in_both_encodings_and_acceptance_modes() {
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            for rich in [false, true] {
                let (source, value) = item(
                    "😀 \\label{sec:new-key} \\ref{sec:n|ow-key}",
                    "sec:new-key",
                    enc,
                    rich,
                );
                for choice in ["insert", "replace"] {
                    assert_eq!(
                        apply(&source, &value, choice, enc),
                        "😀 \\label{sec:new-key} \\ref{sec:new-key}"
                    );
                }
                let (source, value) = item("😀 \\sec|tionobsolete{Title}", "section", enc, rich);
                assert_eq!(
                    apply(&source, &value, "replace", enc),
                    "😀 \\section{Title}"
                );
            }
        }
    }

    #[test]
    fn environment_insertion_preserves_existing_structure_and_negotiates_skeletons() {
        for marked in [
            "\\begin{ite|mize}\nbody\n\\end{itemize}",
            "\\begin{ite|mize}% keep\ntext",
        ] {
            let (source, value) = item(marked, "itemize", PositionEncoding::Utf16, true);
            assert_ne!(value["insertTextFormat"], 2);
            assert_eq!(
                apply(&source, &value, "replace", PositionEncoding::Utf16),
                marked.replace('|', "")
            );
        }
        for marked in ["\\begin{ite|", "\\begin{ite|}"] {
            let (source, value) = item(marked, "itemize", PositionEncoding::Utf16, true);
            assert_eq!(
                apply(&source, &value, "replace", PositionEncoding::Utf16),
                "\\begin{itemize}\n\t\n\\end{itemize}"
            );
            let (source, plain) = item(marked, "itemize", PositionEncoding::Utf16, false);
            let expected = if source.ends_with('}') {
                "\\begin{itemize}"
            } else {
                "\\begin{itemize"
            };
            assert_eq!(
                apply(&source, &plain, "replace", PositionEncoding::Utf16),
                expected
            );
        }
    }
}
