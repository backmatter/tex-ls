use super::*;
use serde_json::json;
use tex_ls_analysis::incremental::IncrementalDatabase;
fn database(text: &str) -> (IncrementalDatabase, PathBuf) {
    let mut db = IncrementalDatabase::default();
    let path = PathBuf::from(fixture_path!("/audit/main.tex"));
    db.apply_change(&path, text, None);
    (db, path)
}

#[test]
fn bib_rule_links_match_quick_fixes_and_document_workspace_reports() {
    let path = PathBuf::from(fixture_path!("/diagnostic-help/refs.bib"));
    let mut db = IncrementalDatabase::default();
    db.apply_change(&path, "@misc{key, title={Title}, note={}}\n", None);
    let snapshot = db.snapshot();
    let rules = RuleSelection::all();
    let uri = path_to_uri(&path).unwrap();
    let report = diagnostic_store::document_diagnostics(
        &snapshot,
        &path,
        FileKind::Bib,
        &rules,
        PositionEncoding::Utf16,
        None,
    );
    let diagnostic = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["code"] == "empty-field")
        .unwrap();
    assert_eq!(
        diagnostic["codeDescription"]["href"],
        "https://github.com/backmatter/tex-ls/blob/main/docs/reference/bib-linter-rules.md#empty-field"
    );
    let actions = serde_json::to_value(compute_code_actions(
        &snapshot,
        &uri,
        &path,
        FileKind::Bib,
        Range::new(Position::new(0, 0), Position::new(1, 0)),
        None,
        &rules,
        PositionEncoding::Utf16,
    ))
    .unwrap();
    let echoed = actions
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|action| action.pointer("/diagnostics/0"))
        .find(|item| item["code"] == "empty-field")
        .unwrap();
    assert_eq!(echoed, diagnostic);
    let workspace = diagnostic_store::workspace_diagnostics(
        &[snapshot],
        &json!([]),
        PositionEncoding::Utf16,
        |_| rules.clone(),
        |_| None,
    );
    assert_eq!(workspace["items"][0]["items"], report["items"]);
}

#[test]
fn bibliography_diagnostics_never_parse_fields_as_a_tex_document() {
    use tex_ls_analysis::incremental::QueryKind;
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/bib-only/refs.bib"));
    db.apply_change(
        path,
        "@misc{key, title={\\documentclass{article} in a title}}\n",
        None,
    );
    db.clear_query_log();
    let snapshot = db.snapshot();
    let report = diagnostic_store::document_diagnostics(
        &snapshot,
        path,
        FileKind::Bib,
        &RuleSelection::all(),
        PositionEncoding::Utf16,
        None,
    );
    assert_eq!(report["kind"], "full");
    assert!(snapshot.resolve_labels().candidate_roots(path).is_empty());
    assert!(
        snapshot
            .query_log()
            .iter()
            .all(|entry| entry.kind != QueryKind::ParsedDocument),
        "a bibliography must only have a BibTeX parse"
    );
}
#[test]
fn semantic_tokens_respect_encoding_protection_and_negotiated_legend() {
    let source =
        "😀 \\textbf{x} \\label{kéy}\n\\begin{verbatim}\n\\textbf{hidden}\n\\end{verbatim}\n";
    let (db, path) = database(source);
    for (encoding, column) in [(PositionEncoding::Utf8, 5), (PositionEncoding::Utf16, 3)] {
        let result = semantic_tokens::compute(&db.snapshot(), &path, encoding, None);
        assert_eq!(result["data"][1], column);
        let chunks: Vec<_> = result["data"]
            .as_array()
            .unwrap()
            .as_chunks::<5>()
            .0
            .iter()
            .collect();
        let mut line = 0;
        for chunk in chunks {
            line += chunk[0].as_u64().unwrap();
            assert_ne!(line, 2, "protected body");
        }
        let policy = ResponsePolicy::new(
            &json!({"capabilities":{"textDocument":{"semanticTokens":{"tokenTypes":["variable"],"formats":["relative"],"requests":{"full":true,"range":true}}}}}),
        );
        let init = policy.initialize_result(capabilities::server_capabilities(encoding, false));
        assert_eq!(
            init["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"],
            json!(["variable"])
        );
        let mut result = result;
        policy.response("textDocument/semanticTokens/full", None, &mut result);
        assert_eq!(result["data"].as_array().unwrap().len(), 5);
        assert_eq!(result["data"][3], 0);
    }
}
#[test]
fn option_edit_replaces_only_selected_multiword_key() {
    let source =
        "\\begin{tikzpicture}\\draw[line width=1pt,draw=red] (0,0)--(1,1);\\end{tikzpicture}";
    let (db, path) = database(source);
    let offset = source.find("width").unwrap() + 1;
    let snapshot = db.snapshot();
    let idx = snapshot.file_line_index(
        snapshot.lookup_file(&path).unwrap(),
        PositionEncoding::Utf16,
    );
    let (line, character) = idx.position(offset);
    let result = compute_completion(
        &snapshot,
        &path_to_uri(&path).unwrap(),
        &path,
        PositionEncoding::Utf16,
        Position { line, character },
    );
    let value = serde_json::to_value(result).unwrap();
    let item = value["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "line width")
        .expect("key completion");
    assert_eq!(item["textEdit"]["newText"], "line width");
    let edit: TextEdit = serde_json::from_value(item["textEdit"].clone()).unwrap();
    assert_eq!(
        &source[idx.offset_at(edit.range.start.line, edit.range.start.character)
            ..idx.offset_at(edit.range.end.line, edit.range.end.character)],
        "line width"
    );
}
#[test]
fn unchanged_diagnostics_skip_lint_and_independent_root_edits() {
    let (mut db, path) = database("\\documentclass{article}\\ref{missing}\n");
    let other = Path::new(fixture_path!("/audit/other.tex"));
    db.apply_change(other, "\\documentclass{article}other", None);
    let first = diagnostic_store::document_diagnostics(
        &db.snapshot(),
        &path,
        FileKind::Tex,
        &RuleSelection::all(),
        PositionEncoding::Utf16,
        None,
    );
    db.apply_change(other, "\\documentclass{article}changed", None);
    let second = diagnostic_store::document_diagnostics(
        &db.snapshot(),
        &path,
        FileKind::Tex,
        &RuleSelection::all(),
        PositionEncoding::Utf16,
        first["resultId"].as_str(),
    );
    assert_eq!(second["kind"], "unchanged");
    let changed_rules = diagnostic_store::document_diagnostics(
        &db.snapshot(),
        &path,
        FileKind::Tex,
        &RuleSelection::resolve(Some(&[]), &[]).0,
        PositionEncoding::Utf16,
        first["resultId"].as_str(),
    );
    assert_eq!(changed_rules["kind"], "full");
}
#[test]
fn diagnostic_stream_stops_after_first_batch() {
    let mut db = IncrementalDatabase::default();
    for n in 0..130 {
        db.apply_change(
            &Path::new(fixture_path!("/audit")).join(format!("{n:03}.tex")),
            "text",
            None,
        );
    }
    let mut count = 0;
    diagnostic_store::stream_workspace_diagnostics(
        &[db.snapshot()],
        &json!([]),
        PositionEncoding::Utf16,
        |_| RuleSelection::all(),
        |_| None,
        |batch| {
            count += batch.len();
            false
        },
    );
    assert_eq!(count, 64);
}
#[test]
fn workspace_search_is_bounded_and_exact_names_win() {
    let (db, path) = database(
        &(0..400)
            .map(|n| format!("\\section{{Section {n}}}\n"))
            .collect::<String>(),
    );
    let result = symbols::compute_projects_workspace_symbols(
        &[db.snapshot()],
        "",
        PositionEncoding::Utf16,
        &|_| Default::default(),
        Some(&path),
    );
    assert_eq!(
        serde_json::to_value(result)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        symbols::WORKSPACE_SYMBOL_LIMIT
    );
    let result = symbols::compute_projects_workspace_symbols(
        &[db.snapshot()],
        "Section 399",
        PositionEncoding::Utf16,
        &|_| Default::default(),
        Some(&path),
    );
    assert_eq!(
        serde_json::to_value(result).unwrap()[0]["name"],
        "Section 399"
    );
}
#[test]
fn completion_defaults_preserve_edits_and_minimal_fallback() {
    let range = json!({"start":{"line":0,"character":1},"end":{"line":0,"character":3}});
    let result = json!({"isIncomplete":false,"items":[{"label":"a","textEdit":{"range":range,"newText":"alpha"}},{"label":"b","textEdit":{"range":range,"newText":"beta"}}]});
    let mut rich = result.clone();
    let policy = ResponsePolicy::new(
        &json!({"capabilities":{"textDocument":{"completion":{"completionList":{"itemDefaults":["editRange"]}}}}}),
    );
    policy.response("textDocument/completion", None, &mut rich);
    assert_eq!(rich["itemDefaults"]["editRange"], range);
    assert_eq!(rich["items"][0]["textEditText"], "alpha");
    let mut minimal = result;
    ResponsePolicy::default().response("textDocument/completion", None, &mut minimal);
    assert!(minimal.get("itemDefaults").is_none());
    assert_eq!(minimal["items"][0]["textEdit"]["newText"], "alpha");
}
#[test]
fn source_cards_keep_dynamic_tex_and_stop_at_next_item() {
    let (db, path) = database(
        "\\begin{thebibliography}{9}\n\\bibitem{one} Jane Doe. \\emph{Title}. 2026.\n\\bibitem{two} Other title.\n\\end{thebibliography}\n\\newacronym{cpu}{CPU}{Central Processing Unit}\n",
    );
    let card = source_cards::manual(&db.snapshot(), &path, "one").unwrap();
    assert!(card.contains("Jane Doe"));
    assert!(card.contains("\\emph{Title}"));
    assert!(!card.contains("Other title"));
    let card = source_cards::glossary(&db.snapshot(), &path, "cpu").unwrap();
    assert!(card.contains("Central Processing Unit"));
}

#[test]
fn request_cancellation_is_checked_at_query_boundaries() {
    let (db, path) = database("\\section{title}");
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let snapshot = db.snapshot().with_cancellation(flag.clone());
    let file = snapshot.lookup_file(&path).unwrap();
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| snapshot.file_outline(file)))
            .is_err()
    );
}
#[test]
fn multiple_ranges_merge_overlapping_expanded_blocks() {
    let source = "\\begin{itemize}\n\\item  first\n\\item second\n\\end{itemize}\n";
    let (db, path) = database(source);
    let ranges = [
        Range::new(Position::new(1, 0), Position::new(2, 1)),
        Range::new(Position::new(2, 0), Position::new(3, 0)),
    ];
    let actual = formatting::compute_ranges_format(
        &db.snapshot(),
        &path,
        PositionEncoding::Utf16,
        FormatStyle::default(),
        FileKind::Tex,
        &ranges,
        SentenceOptions::default(),
    );
    let expected = formatting::compute_range_format(
        &db.snapshot(),
        &path,
        PositionEncoding::Utf16,
        FormatStyle::default(),
        FileKind::Tex,
        ranges[0],
        SentenceOptions::default(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn declared_option_schema_preserves_parse_shape_and_replaces_unicode_suffix() {
    let source = "\\custom[α key=one]";
    let (mut db, path) = database(source);
    let before = db
        .snapshot()
        .parsed_tree(db.snapshot().lookup_file(&path).unwrap())
        .green()
        .to_owned();
    let mut declarations = ResolvedDeclarations::default();
    declarations.options.insert(
        "custom".into(),
        [("α key".into(), vec!["one".into(), "two".into()])].into(),
    );
    db.set_declarations(declarations);
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(&path).unwrap();
    assert_eq!(snapshot.parsed_tree(file).green().to_owned(), before);
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let idx = snapshot.file_line_index(file, enc);
        for (offset, expected, selected) in [
            (source.find("key").unwrap(), "α key", "α key"),
            (source.find("one").unwrap() + 1, "one", "one"),
        ] {
            let (line, character) = idx.position(offset);
            let list = compute_completion(
                &snapshot,
                &path_to_uri(&path).unwrap(),
                &path,
                enc,
                Position::new(line, character),
            );
            let value = serde_json::to_value(list).unwrap();
            let item = value["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == expected)
                .expect("declared option");
            let edit: TextEdit = serde_json::from_value(item["textEdit"].clone()).unwrap();
            assert_eq!(
                &source[idx.offset_at(edit.range.start.line, edit.range.start.character)
                    ..idx.offset_at(edit.range.end.line, edit.range.end.character)],
                selected
            );
        }
    }
}

#[test]
fn package_options_use_the_resolved_source_and_source_commands_override_curated_keys() {
    let source = "\\usepackage[loc]{theme}";
    let (mut db, path) = database(source);
    db.apply_change(
        Path::new(fixture_path!("/audit/theme.sty")),
        "\\DeclareOption{local}{}",
        None,
    );
    db.apply_change(
        Path::new(fixture_path!("/audit/unrelated/theme.sty")),
        "\\DeclareOption{localWrong}{}",
        None,
    );
    let list = compute_completion(
        &db.snapshot(),
        &path_to_uri(&path).unwrap(),
        &path,
        PositionEncoding::Utf16,
        Position::new(0, 15),
    );
    assert!(list.items.iter().any(|item| item.label == "local"));
    assert!(!list.items.iter().any(|item| item.label == "localWrong"));
    let source = "\\newcommand{\\includegraphics}[1][]{custom}\\includegraphics[wid]";
    db.apply_change(&path, source, None);
    let list = compute_completion(
        &db.snapshot(),
        &path_to_uri(&path).unwrap(),
        &path,
        PositionEncoding::Utf16,
        Position::new(0, (source.len() - 1) as u32),
    );
    assert!(!list.items.iter().any(|item| item.label == "width"));
}

#[test]
fn shared_child_diagnostic_identity_tracks_each_root_log() {
    use tex_ls_analysis::external::{
        CompilerArtifact, ExternalInputKind, ExternalInputs, Observation,
    };
    let (mut db, first) = database("\\documentclass{article}\\input{child}");
    let second = Path::new(fixture_path!("/audit/second.tex"));
    let child = Path::new(fixture_path!("/audit/child.tex"));
    db.apply_change(second, "\\documentclass{article}\\input{child}", None);
    db.apply_change(child, "text\n", None);
    assert_eq!(
        db.snapshot().resolve_labels().candidate_roots(child).len(),
        2
    );
    let report = |db: &IncrementalDatabase, previous: Option<&str>| {
        diagnostic_store::document_diagnostics(
            &db.snapshot(),
            child,
            FileKind::Tex,
            &RuleSelection::all(),
            PositionEncoding::Utf16,
            previous,
        )
    };
    let before = report(&db, None);
    let log = first.with_extension("log");
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Compiler(vec![(
            log.clone(),
            Observation::Present(CompilerArtifact::from_text(
                &log,
                "(./child.tex\n! Undefined control sequence.\nl.1 text\n)\n",
                "log".into(),
            )),
        )]),
    )
    .unwrap();
    let fresh = report(&db, None);
    assert_ne!(before["items"], fresh["items"]);
    assert_eq!(report(&db, before["resultId"].as_str()), fresh);
}

#[test]
fn manual_cards_use_unicode_key_identity_and_refuse_duplicate_definitions() {
    let (mut db, path) = database(
        "\\begin{thebibliography}{9}\n\\bibitem{Écho} Unicode title.\n\\end{thebibliography}\n",
    );
    assert!(source_cards::manual(&db.snapshot(), &path, "écho").is_some());
    db.apply_change(&path, "\\begin{thebibliography}{9}\n\\bibitem{same} First.\n\\bibitem{same} Second.\n\\end{thebibliography}\n", None);
    assert!(source_cards::manual(&db.snapshot(), &path, "same").is_none());
}
