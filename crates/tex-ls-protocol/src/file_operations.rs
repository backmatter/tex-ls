//! Standard file operations over captured, root-scoped file observations.
use super::*;
use tex_ls_analysis::{external::command_directories, source::normalize_path};

/// Refuse a batch when an affected spelling cannot be interpreted uniquely.
/// Edits address pre-move URIs, as required by workspace/willRenameFiles.
pub fn will_rename(
    snapshots: &[Analysis],
    files: &[(PathBuf, PathBuf)],
    encoding: PositionEncoding,
) -> Option<WorkspaceEdit> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for snapshot in snapshots {
        if files
            .iter()
            .any(|(old, new)| old != new && snapshot.lookup_file(new).is_some())
        {
            return None;
        }
        for (path, source) in snapshot.tracked_files() {
            if !file_kind_or_tex(&path).is_latex() {
                continue;
            }
            let context = snapshot.file_path_context(source);
            let text = snapshot.file_text(source);
            let tree = snapshot.parsed_tree(source);
            let mut references = snapshot.document_links(source);
            // includeonly is a build selector, not a dependency or clickable link.
            // Its literal spellings must nevertheless follow the selected files.
            for command in tree.descendants().filter(|node| {
                node.kind() == SyntaxKind::COMMAND
                    && tex_ls_parser::ast::command_name(node).as_deref() == Some("includeonly")
                    && !node
                        .ancestors()
                        .skip(1)
                        .any(|parent| parent.kind() == SyntaxKind::COMMAND)
            }) {
                let affects_root = snapshot
                    .resolve_labels()
                    .candidate_roots(&path)
                    .iter()
                    .map(PathBuf::as_path)
                    .chain(std::iter::once(path.as_path()))
                    .any(|root| {
                        files.iter().any(|(old, _)| {
                            snapshot
                                .resolve_labels()
                                .namespace_members(root)
                                .contains(&old.as_path())
                        })
                    });
                if context.is_none() && affects_root {
                    return None;
                }
                let Some((range, raw)) = tex_ls_parser::ast::nth_group_inner(&command, 0) else {
                    if affects_root {
                        return None;
                    }
                    continue;
                };
                let Some(context) = context else {
                    continue;
                };
                for (name, range) in tex_ls_analysis::external::links::comma_spans(&raw, range) {
                    let candidates = tex_ls_analysis::external::FileCandidates::new(
                        name,
                        &["tex"],
                        false,
                        Some(&context.root_directory),
                    );
                    if let tex_ls_analysis::external::Observation::Present(target) =
                        snapshot.resolve_file(&candidates).target
                    {
                        references
                            .push(tex_ls_analysis::external::links::LinkTarget { range, target });
                    }
                }
            }
            if references.is_empty() {
                continue;
            }
            // Moving a compilation root or import source changes outgoing bases.
            // Such a move needs a complete post-move interpretation first.
            if files
                .iter()
                .any(|(old, new)| old == &path && old.parent() != new.parent())
            {
                return None;
            }
            for reference in references {
                let Some((_, destination)) = files.iter().find(|(old, _)| old == &reference.target)
                else {
                    continue;
                };
                let context = context?;
                if snapshot.resolve_labels().candidate_roots(&path).len() > 1 {
                    return None;
                }
                let raw =
                    &text[usize::from(reference.range.start())..usize::from(reference.range.end())];
                let command = tree
                    .descendants()
                    .filter(|node| {
                        node.kind() == SyntaxKind::COMMAND
                            && node.text_range().contains_range(reference.range)
                    })
                    .min_by_key(|node| node.text_range().len())?;
                let mut spellings = Vec::new();
                for base in &context.bases {
                    let directories = if tex_ls_parser::ast::command_name(&command).as_deref()
                        == Some("includeonly")
                    {
                        vec![context.root_directory.clone()]
                    } else {
                        command_directories(
                            &tree,
                            &command,
                            Some(base),
                            Some(&context.root_directory),
                            &context.graphics,
                        )
                    };
                    for directory in directories {
                        let requested = normalize_path(&directory.join(raw));
                        let omitted = Path::new(raw).extension().is_none();
                        let expanded = if omitted {
                            requested.with_extension(reference.target.extension()?)
                        } else {
                            requested
                        };
                        if expanded != reference.target {
                            continue;
                        }
                        let mut spelling = if Path::new(raw).is_absolute() {
                            destination.clone()
                        } else {
                            relative_path(destination, &directory)?
                        };
                        if omitted
                            && destination.extension() == reference.target.extension()
                            && spelling
                                .file_stem()
                                .is_some_and(|stem| !stem.to_string_lossy().contains('.'))
                        {
                            spelling.set_extension("");
                        }
                        let spelling = spelling.to_str()?.replace('\\', "/");
                        let list = tex_ls_parser::ast::command_name(&command)
                            .and_then(|name| tex_ls_parser::semantic::roles::file_role(&name))
                            .is_none_or(|role| role.list);
                        if !tex_ls_analysis::external::is_literal_file_name(&spelling, list) {
                            return None;
                        }
                        spellings.push(spelling);
                    }
                }
                spellings.sort();
                spellings.dedup();
                if spellings.len() != 1 {
                    return None;
                }
                let mut spelling = spellings.pop()?;
                if spelling.chars().any(char::is_whitespace)
                    && command
                        .children()
                        .any(|node| node.kind() == SyntaxKind::BARE_ARGUMENT)
                {
                    spelling = format!("{{{spelling}}}");
                }
                let edit = TextEdit {
                    range: lsp_range(&LineIndex::with_encoding(text, encoding), reference.range),
                    new_text: spelling,
                };
                let entries = changes.entry(path_to_uri(&path)?).or_default();
                if entries
                    .iter()
                    .any(|other| other.range == edit.range && other.new_text != edit.new_text)
                {
                    return None;
                }
                if !entries.contains(&edit) {
                    entries.push(edit);
                }
            }
        }
    }
    for edits in changes.values_mut() {
        edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
    }
    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

fn relative_path(path: &Path, base: &Path) -> Option<PathBuf> {
    let path: Vec<_> = path.components().collect();
    let base: Vec<_> = base.components().collect();
    if path.first() != base.first() {
        return None;
    }
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut result = PathBuf::new();
    for _ in common..base.len() {
        result.push("..");
    }
    for component in &path[common..] {
        result.push(component.as_os_str());
    }
    Some(result)
}

pub fn decode_files(params: &serde_json::Value) -> Option<Vec<(PathBuf, PathBuf)>> {
    params
        .get("files")?
        .as_array()?
        .iter()
        .map(|file| {
            Some((
                uri_to_fs_path(&serde_json::from_value::<Uri>(file.get("oldUri")?.clone()).ok()?)?,
                uri_to_fs_path(&serde_json::from_value::<Uri>(file.get("newUri")?.clone()).ok()?)?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn renames_preserve_bare_filenames_and_includeonly_build_participation() {
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let path = Path::new(fixture_path!("/files/main.tex"));
            for source in [
                "\\documentclass{article}\n😀 \\input chapter after\n",
                "\\documentclass{article}\n\\includeonly{ other, chapter }\n\\include{chapter}\n",
            ] {
                let old = Path::new(fixture_path!("/files/chapter.tex"));
                let new = Path::new(fixture_path!("/files/new chapter.tex"));
                let mut db = IncrementalDatabase::default();
                db.apply_change(path, source, None);
                db.apply_change(old, "Chapter", None);
                let edit = will_rename(&[db.snapshot()], &[(old.into(), new.into())], enc).unwrap();
                let changes = edit.changes.unwrap();
                let idx = LineIndex::with_encoding(source, enc);
                let mut applied = source.to_owned();
                for edit in changes[&path_to_uri(path).unwrap()].iter().rev() {
                    let start = idx.offset_at(edit.range.start.line, edit.range.start.character);
                    let end = idx.offset_at(edit.range.end.line, edit.range.end.character);
                    applied.replace_range(start..end, &edit.new_text);
                }
                let expected = if source.contains("includeonly") {
                    source.replace("chapter", "new chapter")
                } else {
                    source.replace("chapter", "{new chapter}")
                };
                assert_eq!(applied, expected);
                db.rename_source(old, new);
                db.apply_change(path, applied.as_str(), None);
                let snapshot = db.snapshot();
                let file = snapshot.lookup_file(path).unwrap();
                assert!(
                    snapshot
                        .document_links(file)
                        .iter()
                        .any(|link| link.target == new)
                );
                if source.contains("includeonly") {
                    assert_eq!(
                        snapshot
                            .semantic_model(file)
                            .include_only()
                            .participates("new chapter"),
                        Some(true)
                    );
                }
            }
        }
    }

    #[test]
    fn a_shared_includeonly_selector_prevents_partial_file_rename() {
        let mut db = IncrementalDatabase::default();
        db.apply_change(
            Path::new(fixture_path!("/files/a.tex")),
            "\\documentclass{article}\\input{filter}\\include{chapter}",
            None,
        );
        db.apply_change(
            Path::new(fixture_path!("/files/b.tex")),
            "\\documentclass{article}\\input{filter}",
            None,
        );
        db.apply_change(
            Path::new(fixture_path!("/files/filter.tex")),
            "\\includeonly{chapter}",
            None,
        );
        db.apply_change(
            Path::new(fixture_path!("/files/chapter.tex")),
            "Chapter",
            None,
        );
        assert!(
            will_rename(
                &[db.snapshot()],
                &[(
                    fixture_path!("/files/chapter.tex").into(),
                    fixture_path!("/files/renamed.tex").into()
                )],
                PositionEncoding::Utf16
            )
            .is_none()
        );
    }

    #[test]
    fn dynamic_includeonly_refuses_an_unproved_file_rename() {
        let mut db = IncrementalDatabase::default();
        db.apply_change(
            Path::new(fixture_path!("/files/main.tex")),
            "\\documentclass{article}\\includeonly{\\chapters}\\include{chapter}",
            None,
        );
        db.apply_change(
            Path::new(fixture_path!("/files/chapter.tex")),
            "Chapter",
            None,
        );
        assert!(
            will_rename(
                &[db.snapshot()],
                &[(
                    fixture_path!("/files/chapter.tex").into(),
                    fixture_path!("/files/renamed.tex").into()
                )],
                PositionEncoding::Utf16
            )
            .is_none()
        );
    }
    #[test]
    fn exact_spelling_and_overlay_lifecycle() {
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let root = Path::new(fixture_path!("/files/main.tex"));
        let old = Path::new(fixture_path!("/files/chapter.tex"));
        let new = Path::new(fixture_path!("/files/parts/new.tex"));
        db.apply_change(root, "\\documentclass{article}\n😀\\input{chapter}\n", None);
        db.apply_change(old, "backing", None);
        db.open_overlay(project, old, "unsaved").unwrap();
        let result = will_rename(
            &[db.snapshot()],
            &[(old.into(), new.into())],
            PositionEncoding::Utf16,
        )
        .unwrap();
        let edits = &result.changes.unwrap()[&path_to_uri(root).unwrap()];
        assert_eq!(edits[0].new_text, "parts/new");
        assert_eq!(edits[0].range.start.character, 9);
        db.rename_source(old, new);
        assert!(db.lookup_file(old).is_none());
        assert_eq!(db.file_text(db.lookup_file(new).unwrap()), "unsaved");
        db.close_overlay(project, new).unwrap();
        assert_eq!(db.file_text(db.lookup_file(new).unwrap()), "backing");
        let other = Path::new(fixture_path!("/files/other.tex"));
        db.open_overlay(project, other, "other unsaved").unwrap();
        db.rename_sources(
            &[(new.into(), other.into()), (other.into(), new.into())],
            |_| Some(project),
        );
        assert_eq!(db.file_text(db.lookup_file(new).unwrap()), "other unsaved");
        assert_eq!(db.file_text(db.lookup_file(other).unwrap()), "backing");
    }
}
