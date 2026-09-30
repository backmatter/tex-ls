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
fn bibliography_symbols_match_hierarchical_adaptation_in_each_wire_form() {
    for path in [
        Path::new(fixture_path!("/audit/refs.bib")),
        Path::new("untitled:refs.bib"),
    ] {
        for source in [
            "",
            "@string{pub={Éditions}}\r\n@book{clé, title={😀}, publisher=pub}\r\n@misc{broken, note=",
        ] {
            let mut db = IncrementalDatabase::default();
            db.apply_change(path, source, None);
            for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                for hierarchical in [false, true] {
                    let policy = ResponsePolicy::new(&json!({"capabilities":{"textDocument":{
                        "documentSymbol":{"hierarchicalDocumentSymbolSupport":hierarchical,
                            "symbolKind":{"valueSet":[1, 5]}}
                    }}}));
                    let uri = json!(
                        path_to_uri(path).unwrap_or_else(|| path
                            .to_str()
                            .unwrap()
                            .parse()
                            .unwrap())
                    );
                    let snapshot = db.snapshot();
                    let mut expected =
                        serde_json::to_value(compute_bib_symbols(&snapshot, path, enc)).unwrap();
                    policy.response("textDocument/documentSymbol", Some(&uri), &mut expected);
                    let mut actual = serde_json::to_value(compute_bib_symbol_response(
                        &snapshot,
                        path,
                        enc,
                        hierarchical,
                    ))
                    .unwrap();
                    policy.response("textDocument/documentSymbol", Some(&uri), &mut actual);
                    assert_eq!(actual, expected, "{source}, {enc:?}, {hierarchical}");
                }
            }
        }
    }
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
        |_| true,
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

#[test]
fn installed_definitions_are_navigable_without_workspace_lint_or_rename() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, FileInputs, InstalledMetadata, LocationObservation,
        Observation,
    };
    let (mut db, path) = database("\\usepackage{demo}\n\\widget\n");
    let package = path.parent().unwrap().join("texmf/demo.sty");
    db.apply_change(&package, "\\def\\widget{yes}\n$\n", None);
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files([("demo.sty".into(), package.clone())].into()),
        })),
    )
    .unwrap();
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Files(FileInputs {
            locations: ["demo.sty", "demo.dtx"]
                .into_iter()
                .map(|name| {
                    (
                        path.parent().unwrap().join(name),
                        LocationObservation {
                            kind: Observation::Absent,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        }),
    )
    .unwrap();
    let snapshot = db.snapshot();
    let links = compute_goto_definition(
        &snapshot,
        &path,
        Position::new(1, 3),
        PositionEncoding::Utf16,
    );
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target_uri, path_to_uri(&package).unwrap());
    assert_eq!(links[0].target_selection_range.start, Position::new(0, 4));
    let target = tex_ls_analysis::name_refs::NameTarget {
        kind: tex_ls_analysis::name_refs::NameKind::Command,
        name: "widget".into(),
        span: rowan::TextRange::empty(rowan::TextSize::new(0)),
    };
    assert!(snapshot.name_definitions(&path, &target).is_empty());
    let previous = json!([{"uri":path_to_uri(&package).unwrap(),"value":"old"}]);
    let workspace = diagnostic_store::workspace_diagnostics(
        std::slice::from_ref(&snapshot),
        &previous,
        PositionEncoding::Utf16,
        |_| RuleSelection::all(),
        |_| None,
    );
    let reports = workspace["items"].as_array().unwrap();
    let cleared = reports
        .iter()
        .find(|r| r["uri"] == json!(path_to_uri(&package).unwrap()))
        .unwrap();
    assert_eq!(cleared["items"], json!([]));
    assert_eq!(cleared["resultId"], "removed");

    let rules = RuleSelection::all();
    let report = diagnostic_store::document_diagnostics(
        &snapshot,
        &package,
        file_kind_for(&package),
        &rules,
        PositionEncoding::Utf16,
        Some("previous-lint-report"),
    );
    assert_eq!(report["kind"], "full");
    assert_eq!(report["items"], json!([]));
    assert_eq!(
        diagnostic_store::document_diagnostics(
            &snapshot,
            &package,
            file_kind_for(&package),
            &rules,
            PositionEncoding::Utf16,
            report["resultId"].as_str(),
        )["kind"],
        "unchanged"
    );
    assert!(
        diagnostic_store::DiagnosticEntry::capture(
            &snapshot,
            &package,
            file_kind_for(&package),
            &rules,
            PositionEncoding::Utf16,
        )
        .items
        .is_empty()
    );

    drop(snapshot);
    let local = path.parent().unwrap().join("demo.sty");
    db.apply_change(&local, "\\]", None);
    let snapshot = db.snapshot();
    assert!(!snapshot.is_installed_source(&local));
    assert!(
        !diagnostic_store::DiagnosticEntry::capture(
            &snapshot,
            &local,
            file_kind_for(&local),
            &rules,
            PositionEncoding::Utf16,
        )
        .items
        .is_empty()
    );
}

#[test]
fn workspace_exclusions_clear_previous_reports_without_linting() {
    let (db, path) = database("$\n");
    let previous = json!([{"uri":path_to_uri(&path).unwrap(),"value":"old"}]);
    let mut reports = Vec::new();
    diagnostic_store::stream_workspace_diagnostics(
        &[db.snapshot()],
        &previous,
        PositionEncoding::Utf16,
        |_| panic!("excluded file must not be linted"),
        |_| None,
        |_| false,
        |batch| {
            reports.extend(batch);
            true
        },
    );
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["items"], json!([]));
}

#[test]
fn unknown_commands_follow_live_edits_and_root_scoped_definitions() {
    let (mut db, path) = database(
        "\\documentclass{article}\n\\input{defs}\n\\begin{document}\n\\tem First\n\\customword\n\\otherword\n\\end{document}\n",
    );
    let dir = path.parent().unwrap();
    db.apply_change(
        &dir.join("defs.tex"),
        "\\newcommand{\\customword}{ok}",
        None,
    );
    db.apply_change(
        &dir.join("other.tex"),
        "\\documentclass{article}\n\\newcommand{\\otherword}{ok}",
        None,
    );
    let messages = |snapshot: &tex_ls_analysis::incremental::Analysis| {
        let file = snapshot.lookup_file(&path).unwrap();
        snapshot
            .latex_lint_findings(file)
            .iter()
            .filter(|d| d.rule == "unknown-command")
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    };
    let initial = messages(&db.snapshot());
    assert!(
        initial
            .iter()
            .any(|m| m.contains("\\tem") && m.contains("\\item")),
        "{initial:?}"
    );
    assert!(
        initial.iter().any(|m| m.contains("\\otherword")),
        "{initial:?}"
    );
    assert!(
        !initial.iter().any(|m| m.contains("\\customword")),
        "{initial:?}"
    );
    db.apply_change(&path, "\\documentclass{article}\n\\input{defs}\n\\begin{document}\n\\item First\n\\customword\n% tex-ls-lint skip unknown-command: generated\n\\otherword\n\\end{document}\n", None);
    assert!(messages(&db.snapshot()).is_empty());
}

#[test]
fn unknown_command_ignore_quick_fixes_apply_and_preserve_line_endings() {
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        for newline in ["\n", "\r\n"] {
            let source = format!(
                "😀 Intro.{newline}\\begin{{enumerate}}{newline}    \\tem test{newline}\\end{{enumerate}}{newline}{newline}\\anotherunknown text{newline}{{\\bf bold}}{newline}"
            );
            let (mut db, path) = database(&source);
            let uri = path_to_uri(&path).unwrap();
            let target = source.find("\\tem").unwrap();
            let buffer = TextBuffer::new(source.as_str(), enc);
            let range = byte_range_to_lsp(&buffer.line_index(), target + 2, target + 2);
            let rules = RuleSelection::resolve(None, &[]).0;
            let actions = compute_code_actions(
                &db.snapshot(),
                &uri,
                &path,
                FileKind::Tex,
                range,
                Some(&[CodeActionKind::QuickFix]),
                &rules,
                enc,
            );
            assert_eq!(actions.len(), 2);
            for response in actions {
                let CodeActionResponse::CodeAction(action) = response else {
                    panic!("literal action");
                };
                assert_eq!(action.is_preferred, Some(false));
                assert_eq!(action.kind, Some(CodeActionKind::QuickFix));
                assert_eq!(
                    serde_json::to_value(&action.diagnostics).unwrap()[0]["code"],
                    "unknown-command"
                );
                let edits = action.edit.unwrap().changes.unwrap().remove(&uri).unwrap();
                assert_eq!(edits.len(), 1);
                let edit = &edits[0];
                assert_eq!(edit.range.start, edit.range.end);
                assert!(edit.new_text.starts_with("    % tex-ls skip"));
                assert!(edit.new_text.ends_with(newline));
                let at = buffer
                    .line_index()
                    .offset_at(edit.range.start.line, edit.range.start.character);
                let mut updated = source.clone();
                updated.insert_str(at, &edit.new_text);
                db.apply_change(&path, updated.as_str(), None);
                let snapshot = db.snapshot();
                let findings = snapshot.latex_lint_findings(snapshot.lookup_file(&path).unwrap());
                let remaining = findings
                    .iter()
                    .filter(|d| d.rule == "unknown-command")
                    .count();
                assert_eq!(
                    remaining,
                    if action.title.ends_with("in this file") {
                        0
                    } else {
                        1
                    }
                );
                assert!(findings.iter().any(|d| d.rule == "deprecated-command"));
                drop(snapshot);
                db.apply_change(&path, source.as_str(), None);
            }
            let fix_all = compute_code_actions(
                &db.snapshot(),
                &uri,
                &path,
                FileKind::Tex,
                range,
                Some(&[CodeActionKind::from("source.fixAll.tex-ls")]),
                &rules,
                enc,
            );
            assert!(
                !serde_json::to_string(&fix_all)
                    .unwrap()
                    .contains("Ignore unknown-command")
            );
            let disabled = RuleSelection::resolve(None, &["unknown-command".into()]).0;
            assert!(
                compute_code_actions(
                    &db.snapshot(),
                    &uri,
                    &path,
                    FileKind::Tex,
                    range,
                    Some(&[CodeActionKind::QuickFix]),
                    &disabled,
                    enc
                )
                .is_empty()
            );
        }
    }
}

#[test]
fn file_argument_roles_share_highlighting_navigation_and_filename_translation() {
    let cases = [
        ("documentclass", "", "article", "article.cls", 7),
        ("usepackage", "[draft]", "amsmath", "amsmath.sty", 7),
        (
            "usetheme",
            "[numbering=fraction]",
            "metropolis",
            "beamerthememetropolis.sty",
            7,
        ),
        ("usecolortheme", "", "dove", "beamercolorthemedove.sty", 7),
        ("usefonttheme", "", "serif", "beamerfontthemeserif.sty", 7),
        (
            "useinnertheme",
            "",
            "circles",
            "beamerinnerthemecircles.sty",
            7,
        ),
        ("useoutertheme", "", "split", "beamerouterthemesplit.sty", 7),
        (
            "usetikzlibrary",
            "",
            "arrows.meta",
            "tikzlibraryarrows.meta.code.tex",
            7,
        ),
        ("usepgflibrary", "", "fpu", "pgflibraryfpu.code.tex", 7),
        ("bibliographystyle", "", "plain", "plain.bst", 7),
        (
            "RequireBibliographyStyle",
            "",
            "authoryear",
            "authoryear.bbx",
            7,
        ),
        ("RequireCitationStyle", "", "numeric", "numeric.cbx", 7),
        (
            "DeclareLanguageMapping",
            "{english}",
            "english-apa",
            "english-apa.lbx",
            7,
        ),
        ("input", "", "chapter", "chapter.tex", 5),
        ("include", "", "chapter", "chapter.tex", 5),
        ("bibliography", "", "references", "references.bib", 5),
        ("addbibresource", "", "references.bib", "references.bib", 5),
        ("includegraphics", "[width=3cm]", "figure", "figure.pdf", 5),
        (
            "import",
            "{sections/}",
            "chapter",
            "sections/chapter.tex",
            5,
        ),
    ];
    for (command, options, name, filename, kind) in cases {
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let source = format!("😀 \\{command}{options}{{ {name} }}\r\n");
            let (mut db, path) = database(&source);
            let target = path.parent().unwrap().join(filename);
            db.apply_change(&target, "% fixture\n", None);
            let snapshot = db.snapshot();
            let file = snapshot.lookup_file(&path).unwrap();
            let references = snapshot.file_references(file);
            assert_eq!(references.len(), 1, "{command}");
            let reference = &references[0];
            assert_eq!(
                &source[usize::from(reference.range.start())..usize::from(reference.range.end())],
                name
            );
            let buffer = TextBuffer::new(source.as_str(), enc);
            let idx = buffer.line_index();
            let range = lsp_range(&idx, reference.range);
            let links = compute_goto_definition(&snapshot, &path, range.start, enc);
            assert_eq!(links.len(), 1, "{command}: {references:?}");
            assert_eq!(
                links[0].target_uri,
                path_to_uri(&target).unwrap(),
                "{command}"
            );
            assert_eq!(links[0].origin_selection_range, Some(range));
            let tokens = semantic_tokens::compute(&snapshot, &path, enc, None);
            let (mut line, mut column) = (0, 0);
            assert!(
                tokens["data"]
                    .as_array()
                    .unwrap()
                    .as_chunks::<5>()
                    .0
                    .iter()
                    .any(|token| {
                        let delta = token[0].as_u64().unwrap() as u32;
                        line += delta;
                        column =
                            if delta == 0 { column } else { 0 } + token[1].as_u64().unwrap() as u32;
                        Position::new(line, column) == range.start && token[3] == kind
                    }),
                "{command}: {tokens}"
            );
        }
    }
}

#[test]
fn named_module_lists_ignore_dynamic_arguments_and_complete_logical_names() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
    };
    let source = "\\usetheme{ metropolis, Madrid }\n\\usetheme{\\dynamic}\n\\section{metropolis}\n";
    let (mut db, path) = database(source);
    let first = path.parent().unwrap().join("beamerthememetropolis.sty");
    db.apply_change(&first, "% local\n", None);
    let installed = path.parent().unwrap().join("texmf/beamerthemeMadrid.sty");
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files(
                [
                    ("beamerthemeMadrid.sty".into(), installed),
                    (
                        "beamercolorthemeMadrid.sty".into(),
                        path.parent().unwrap().join("texmf/color.sty"),
                    ),
                ]
                .into(),
            ),
        })),
    )
    .unwrap();
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(&path).unwrap();
    let references = snapshot.file_references(file);
    assert_eq!(references.len(), 2);
    assert_eq!(
        references[0].candidates.installed.as_ref().unwrap().0,
        "beamerthememetropolis"
    );
    assert_eq!(
        references[1].candidates.installed.as_ref().unwrap().0,
        "beamerthemeMadrid"
    );
    let items = compute_tex_completion(
        &snapshot,
        &path_to_uri(&path).unwrap(),
        &path,
        source.find("metropolis").unwrap() + 2,
    );
    let names: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(names.contains(&"metropolis"), "{names:?}");
    assert!(names.contains(&"Madrid"), "{names:?}");
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("beamertheme") || name.starts_with("beamercolortheme"))
    );
}

#[test]
fn format_definitions_cover_kernel_symbols_aliases_and_environment_delimiters() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, InputNeed, InstalledMetadata, Observation,
    };
    let source =
        "\\documentclass{article}\n\\begin{demo}\n\\[x\\]\n\\alias\n\\end{demo}\n\\undefined\n";
    let (mut db, path) = database(source);
    let kernel = path.parent().unwrap().join("texmf/latex.ltx");
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files([("latex.ltx".into(), kernel.clone())].into()),
        })),
    )
    .unwrap();
    assert!(
        !db.snapshot()
            .file_discovery_needs(&path)
            .contains(&InputNeed::Source(kernel.clone()))
    );
    let definitions = "\\def\\documentclass#1{}\n\\protected\\def\\begin#1{}\n\\def\\end#1{}\n\\DeclareRobustCommand\\[{open}\n\\def\\]{close}\n\\let\\alias=\\documentclass\n\\def\\demo{}\n\\def\\enddemo{}\n";
    db.apply_change(&kernel, definitions, None);
    let snapshot = db.snapshot();
    assert!(
        !snapshot
            .file_discovery_needs(&path)
            .contains(&InputNeed::Source(kernel.clone()))
    );
    for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        for (line, col, expected) in [
            (0, 2, 0),
            (1, 2, 1),
            (1, 8, 6),
            (2, 1, 3),
            (2, 4, 4),
            (3, 2, 5),
            (4, 2, 2),
            (4, 6, 6),
        ] {
            let links =
                compute_goto_definition(&snapshot, &path, Position::new(line, col), encoding);
            assert_eq!(links.len(), 1, "{line}:{col}");
            assert_eq!(links[0].target_uri, path_to_uri(&kernel).unwrap());
            assert_eq!(
                links[0].target_selection_range.start.line, expected,
                "{line}:{col}"
            );
        }
        assert!(
            compute_goto_definition(&snapshot, &path, Position::new(5, 2), encoding).is_empty()
        );
    }
    let target = tex_ls_analysis::name_refs::NameTarget {
        kind: tex_ls_analysis::name_refs::NameKind::Command,
        name: "documentclass".into(),
        span: rowan::TextRange::empty(rowan::TextSize::new(0)),
    };
    assert!(
        snapshot.name_definitions(&path, &target).is_empty(),
        "kernel navigation must not authorize rename"
    );
    drop(snapshot);
    db.apply_change(
        &path,
        "\\def\\documentclass#1{}\n\\documentclass{article}\n",
        None,
    );
    let links = compute_goto_definition(
        &db.snapshot(),
        &path,
        Position::new(1, 2),
        PositionEncoding::Utf16,
    );
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target_uri, path_to_uri(&path).unwrap());
}

#[test]
fn command_rename_rejects_installed_names_without_known_signatures() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
    };
    let (mut db, path) = database("\\newcommand{\\custom}{value}\n\\custom\n");
    let kernel = path.parent().unwrap().join("texmf/latex.ltx");
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files([("latex.ltx".into(), kernel.clone())].into()),
        })),
    )
    .unwrap();
    db.apply_change(&kernel, "\\let\\installed=\\unresolved\n", None);
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(&path).unwrap();
    assert!(snapshot.editor_symbols(file).commands.contains("installed"));
    assert!(
        snapshot
            .editor_signatures(file)
            .command("installed")
            .is_none()
    );
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        assert!(compute_rename(&snapshot, &path, Position::new(1, 3), "unusedname", enc).is_some());
        assert!(
            compute_rename(&snapshot, &path, Position::new(1, 3), "installed", enc).is_none(),
            "rename must not replace an installed definition with unknown arity"
        );
    }
}

#[test]
fn environment_rename_rejects_installed_names_without_known_signatures() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
    };
    let (mut db, path) =
        database("\\newenvironment{available}{}{}\n\\begin{available}x\\end{available}\n");
    let kernel = path.parent().unwrap().join("texmf/latex.ltx");
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files([("latex.ltx".into(), kernel.clone())].into()),
        })),
    )
    .unwrap();
    db.apply_change(&kernel, "\\NewDocumentEnvironment{taken}{s}{}{}\n", None);
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(&path).unwrap();
    assert!(snapshot.editor_symbols(file).environments.contains("taken"));
    assert!(
        snapshot
            .editor_signatures(file)
            .environment("taken")
            .is_none()
    );
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        assert!(compute_rename(&snapshot, &path, Position::new(1, 8), "unusedname", enc).is_some());
        assert!(compute_rename(&snapshot, &path, Position::new(1, 8), "taken", enc).is_none());
    }
}

#[test]
fn diagnostic_identity_tracks_acquired_format_definitions() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, InstalledMetadata, Observation,
    };
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let (mut db, path) = database("\\kernelcustom\n\\section{Hello}\n");
        let kernel = path.parent().unwrap().join("texmf/latex.ltx");
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                toolchain: "test".into(),
                index: TexmfIndex::from_files([("latex.ltx".into(), kernel.clone())].into()),
            })),
        )
        .unwrap();
        let report = |db: &IncrementalDatabase, previous: Option<&str>| {
            diagnostic_store::document_diagnostics(
                &db.snapshot(),
                &path,
                FileKind::Tex,
                &RuleSelection::all(),
                enc,
                previous,
            )
        };
        let before = report(&db, None);
        assert!(
            before["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "unknown-command")
        );
        db.apply_change(&kernel, "\\newcommand{\\kernelcustom}{}\n", None);
        let acquired = report(&db, before["resultId"].as_str());
        assert_eq!(acquired["kind"], "full");
        assert_eq!(acquired["items"], json!([]));
        assert_eq!(acquired, report(&db, None));
        assert_eq!(
            report(&db, acquired["resultId"].as_str())["kind"],
            "unchanged"
        );
        db.apply_change(&kernel, "\\newcommand{\\otherkernelcommand}{}\n", None);
        let changed = report(&db, acquired["resultId"].as_str());
        assert_eq!(changed["kind"], "full");
        assert_eq!(changed["items"], before["items"]);
        assert_eq!(changed, report(&db, None));
    }
}

/// The same definition knowledge must reach all editor features.
fn assert_editor_command(db: &IncrementalDatabase, path: &Path, name: &str, arguments: usize) {
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(path).unwrap();
    let source = snapshot.file_text(file);
    let offset = source.rfind(&format!("\\{name}")).unwrap() + 2;
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let idx = snapshot.file_line_index(file, enc);
        let (line, character) = idx.position(offset);
        let position = Position::new(line, character);
        assert!(
            !compute_goto_definition(&snapshot, path, position, enc).is_empty(),
            "definition {name} {enc:?} {position:?} {:?}",
            snapshot.definition_sites(file)
        );
        assert!(
            hover::compute_hover(&snapshot, path, enc, position).is_some(),
            "hover {name}"
        );
        let list = compute_completion(&snapshot, &path_to_uri(path).unwrap(), path, enc, position);
        assert!(
            list.items.iter().any(|item| item.label == name),
            "completion {name}"
        );
        if arguments > 0 {
            let at = offset + source[offset..].find('{').unwrap() + 1;
            let (line, character) = idx.position(at);
            let help = signature_help::compute_signature_help(
                &snapshot,
                path,
                Position::new(line, character),
                enc,
            )
            .unwrap_or_else(|| panic!("signature {name}"));
            assert_eq!(
                help.signatures[0].parameters.as_ref().unwrap().len(),
                arguments,
                "{name}"
            );
        }
        let report = diagnostic_store::document_diagnostics(
            &snapshot,
            path,
            file_kind_for(path),
            &RuleSelection::all(),
            enc,
            None,
        );
        assert!(
            !report["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "unknown-command"),
            "{name}: {report}"
        );
    }
}

#[test]
fn editor_declaration_families_agree_across_features() {
    for (declaration, name, arguments) in [
        (
            r"\NewExpandableDocumentCommand{\audiexpand}{m}{#1}",
            "audiexpand",
            1,
        ),
        (
            r"\newcommand{\original}[1]{#1}\NewCommandCopy{\audicopy}{\original}\renewcommand{\original}[2]{#1#2}",
            "audicopy",
            1,
        ),
        (r"\let\audialias=\textbf", "audialias", 1),
        (r"\newif\ifaudiflag", "audiflagtrue", 0),
        (r"\newif\ifaudiflag", "audiflagfalse", 0),
        (
            r"\ExplSyntaxOn\cs_new:cpn { audiliteral } #1 {#1}\ExplSyntaxOff",
            "audiliteral",
            1,
        ),
        (
            r"\ExplSyntaxOn\cs_new:Npn \audinamed #1 {#1}\ExplSyntaxOff",
            "audinamed",
            1,
        ),
    ] {
        let source = format!("% 😀\n{declaration}\n\\{name}{{value}}\n");
        let (db, path) = database(&source);
        assert_editor_command(&db, &path, name, arguments);
    }
}

#[test]
fn generated_literal_declarations_reach_editor_features() {
    for (declaration, name, arguments) in [
        (
            r"\usepackage{etoolbox}\csdef{audicsname}#1{#1}",
            "audicsname",
            1,
        ),
        (
            r"\usepackage{etoolbox}\csletcs{audicsalias}{textbf}",
            "audicsalias",
            1,
        ),
        (r"\newcounter{audicount}", "theaudicount", 0),
        (
            r"\ExplSyntaxOn\cs_new:Nn \audi_base:nn {#1#2}\cs_generate_variant:Nn \audi_base:nn {Ve}",
            "audi_base:Ve",
            2,
        ),
        (
            r"\ExplSyntaxOn\prg_new_conditional:Npnn \audi_if:n #1 {p,T,F,TF} {\prg_return_true:}",
            "audi_if:nTF",
            3,
        ),
    ] {
        let source = format!("% 😀\n{declaration}\n\\{name}{{value}}\n");
        let (db, path) = database(&source);
        assert_editor_command(&db, &path, name, arguments);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            assert!(
                compute_rename(&db.snapshot(), &path, Position::new(2, 3), "other", enc).is_none(),
                "generated-name rename: {name}"
            );
        }
    }
    let (db, path) = database(
        "\\ExplSyntaxOn\n\\cs_new:Npn \\audi_base:nn #1#2 {#1#2}\n\\cs_generate_variant:Nn \\audi_base:nn {Ve}\n\\audi_base:nn {one}{two}\n\\audi_base:Ve \\l_tmpa_tl {two}",
    );
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let snapshot = db.snapshot();
        assert!(compute_rename(&snapshot, &path, Position::new(3, 3), "other", enc).is_none());
        assert!(!compute_goto_definition(&snapshot, &path, Position::new(3, 3), enc).is_empty());
    }
}

#[test]
fn ambiguous_shared_sources_offer_each_definition_without_authorizing_rename() {
    let (mut db, path) = database("\\audishared{value}\n");
    let first = path.parent().unwrap().join("first.tex");
    let second = path.parent().unwrap().join("second.tex");
    let shared_name = path.file_name().unwrap().to_str().unwrap();
    db.apply_change(
        &first,
        format!("\\newcommand{{\\audishared}}[1]{{first #1}}\n\\input{{{shared_name}}}"),
        None,
    );
    db.apply_change(
        &second,
        format!("\\newcommand{{\\audishared}}[1]{{second #1}}\n\\input{{{shared_name}}}"),
        None,
    );
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let snapshot = db.snapshot();
        let definitions = compute_goto_definition(&snapshot, &path, Position::new(0, 3), enc);
        assert_eq!(definitions.len(), 2, "{definitions:?}");
        assert!(compute_rename(&snapshot, &path, Position::new(0, 3), "other", enc).is_none());
    }
    db.apply_change(
        &path,
        "\\newcommand{\\audishared}[1]{shared #1}\n\\audishared{value}\n",
        None,
    );
    let snapshot = db.snapshot();
    let definitions = compute_goto_definition(
        &snapshot,
        &path,
        Position::new(1, 3),
        PositionEncoding::Utf16,
    );
    assert_eq!(
        definitions.len(),
        3,
        "a shared declaration must preserve root ambiguity: {definitions:?}"
    );
    assert!(
        compute_rename(
            &snapshot,
            &path,
            Position::new(1, 3),
            "other",
            PositionEncoding::Utf16
        )
        .is_none()
    );
}

#[test]
fn command_rename_rejects_literal_alias_sources_but_updates_control_sequence_sources() {
    for (alias, blocked) in [
        (r"\csletcs{audialias}{audibase}", true),
        (r"\letcs\audialias{audibase}", true),
        (r"\letcs{\audialias}{audibase}", true),
        (
            r"\ExplSyntaxOn\cs_set_eq:Nc \audialias {audibase}\ExplSyntaxOff",
            true,
        ),
        (
            r"\ExplSyntaxOn\cs_new_eq:cc {audialias}{audibase}\ExplSyntaxOff",
            true,
        ),
        (
            r"\ExplSyntaxOn\cs_gset_eq:Nc \audialias {audibase}\ExplSyntaxOff",
            true,
        ),
        (r"\let\audialias=\audibase", false),
        (r"\cslet{audialias}{\audibase}", false),
        (r"\NewCommandCopy{\audialias}{\audibase}", false),
        (
            r"\ExplSyntaxOn\cs_set_eq:NN \audialias \audibase\ExplSyntaxOff",
            false,
        ),
        (
            r"\ExplSyntaxOn\cs_set_eq:cN {audialias} \audibase\ExplSyntaxOff",
            false,
        ),
    ] {
        let (mut db, path) = database(
            "% 😀\n\\newcommand{\\audibase}[1]{#1}\n\\input{aliases}\n\\audibase{value}\n",
        );
        let aliases = path.parent().unwrap().join("aliases.tex");
        db.apply_change(&aliases, alias, None);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let snapshot = db.snapshot();
            let renamed = compute_rename(&snapshot, &path, Position::new(3, 3), "other", enc);
            assert_eq!(renamed.is_none(), blocked, "{alias}: {renamed:?}");
            assert!(
                !compute_goto_definition(&snapshot, &path, Position::new(3, 3), enc).is_empty(),
                "{alias}"
            );
            if let Some(edit) = renamed {
                assert!(
                    edit.changes
                        .unwrap()
                        .contains_key(&path_to_uri(&aliases).unwrap()),
                    "alias source must be updated: {alias}"
                );
            }
        }
        // The cached restriction must clear when a literal reference is removed.
        db.apply_change(&aliases, "% removed\n", None);
        assert!(
            compute_rename(
                &db.snapshot(),
                &path,
                Position::new(3, 3),
                "other",
                PositionEncoding::Utf16
            )
            .is_some(),
            "{alias}"
        );
    }
}

#[test]
fn generated_and_direct_declarations_cannot_authorize_an_incomplete_command_rename() {
    for (generated, name) in [
        (r"\newcounter{auditcounter}", "theauditcounter"),
        (r"\newif\ifauditflag", "auditflagtrue"),
        (r"\csdef{audiname}{original}", "audiname"),
        (
            r"\ExplSyntaxOn\cs_new:cpn {audiname} {original}\ExplSyntaxOff",
            "audiname",
        ),
    ] {
        let source = format!(
            "\\input{{generated}}\n\\{name}\n\\renewcommand{{\\{name}}}{{X}}\n😀 \\{name}\n"
        );
        let (mut db, path) = database(&source);
        let dependency = path.parent().unwrap().join("generated.tex");
        db.apply_change(&dependency, generated, None);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let snapshot = db.snapshot();
            let file = snapshot.lookup_file(&path).unwrap();
            let index = snapshot.file_line_index(file, enc);
            let offset = source.rfind(&format!("\\{name}")).unwrap() + 2;
            let (line, character) = index.position(offset);
            let position = Position::new(line, character);
            assert!(
                compute_rename(&snapshot, &path, position, "renamedcounter", enc).is_none(),
                "{generated}"
            );
            assert!(
                !compute_goto_definition(&snapshot, &path, position, enc).is_empty(),
                "{generated}"
            );
        }
        // Replacing the generated declaration with a direct one must clear
        // the cached restriction and allow every occurrence to be updated.
        db.apply_change(
            &dependency,
            format!("\\newcommand{{\\{name}}}{{original}}"),
            None,
        );
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let snapshot = db.snapshot();
            let file = snapshot.lookup_file(&path).unwrap();
            let index = snapshot.file_line_index(file, enc);
            let offset = source.rfind(&format!("\\{name}")).unwrap() + 2;
            let (line, character) = index.position(offset);
            let edit = compute_rename(
                &snapshot,
                &path,
                Position::new(line, character),
                "renamedcounter",
                enc,
            )
            .unwrap();
            let changes = edit.changes.unwrap();
            assert_eq!(
                changes[&path_to_uri(&path).unwrap()].len(),
                3,
                "{generated}"
            );
            assert_eq!(
                changes[&path_to_uri(&dependency).unwrap()].len(),
                1,
                "{generated}"
            );
        }
    }
}

#[test]
fn delimited_primitive_definitions_keep_navigation_without_inventing_argument_slots() {
    for target in [r"\section", r"\audiprimitive", r"\%", r"\\"] {
        let name = target.strip_prefix('\\').unwrap();
        let source = format!("\\def{target}#1;{{#1}}\n{target}{{value}}\n");
        let (db, path) = database(&source);
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        assert!(snapshot.editor_symbols(file).commands.contains(name));
        assert!(
            snapshot.editor_signatures(file).command(name).is_none(),
            "{target}"
        );
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let position = Position::new(1, 1);
            assert!(
                !compute_goto_definition(&snapshot, &path, position, enc).is_empty(),
                "{target}"
            );
            assert!(
                hover::compute_hover(&snapshot, &path, enc, position).is_some(),
                "{target}"
            );
            if target == r"\section" {
                assert!(
                    signature_help::compute_signature_help(
                        &snapshot,
                        &path,
                        Position::new(1, 10),
                        enc
                    )
                    .is_none()
                );
            }
        }
    }
}

#[test]
fn editor_dependencies_reach_all_features_without_changing_formatter_inputs() {
    for loader in [r"\input{partial}", r"\InputIfFileExists{partial.tex}{}{}"] {
        let (mut db, path) = database(&format!("{loader}\n\\audifromfile{{value}}\n"));
        let partial = path.parent().unwrap().join("partial.tex");
        db.apply_change(&partial, "\\newcommand{\\audifromfile}[1]{#1}\n", None);
        assert_editor_command(&db, &path, "audifromfile", 1);
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        assert!(
            snapshot
                .scope_signatures(file)
                .command("audifromfile")
                .is_none(),
            "editor acquisition must not broaden formatter inputs"
        );
        drop(snapshot);
        db.apply_change(&partial, "% removed\n", None);
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        assert!(
            !snapshot
                .editor_symbols(file)
                .commands
                .contains("audifromfile")
        );
    }
}

#[test]
fn literal_theorem_environments_and_dynamic_names_have_distinct_behavior() {
    for declaration in [
        r"\newtheorem{audithm}{Theorem}",
        r"\newtheorem*{audithm}{Theorem}",
        r"\declaretheorem[name=Theorem]{audithm}",
    ] {
        let (db, path) = database(&format!(
            "{declaration}\n\\begin{{audithm}}[A title]Text\\end{{audithm}}\n"
        ));
        let snapshot = db.snapshot();
        assert!(
            !compute_goto_definition(
                &snapshot,
                &path,
                Position::new(1, 9),
                PositionEncoding::Utf16
            )
            .is_empty()
        );
        let list = compute_completion(
            &snapshot,
            &path_to_uri(&path).unwrap(),
            &path,
            PositionEncoding::Utf16,
            Position::new(1, 9),
        );
        assert!(list.items.iter().any(|item| item.label == "audithm"));
    }
    let (db, path) = database(
        "\\ExplSyntaxOn\\cs_new:cpn {\\dynamic} #1 {#1}\\ExplSyntaxOff\n\\dynamic{value}\n",
    );
    assert!(
        compute_goto_definition(
            &db.snapshot(),
            &path,
            Position::new(1, 3),
            PositionEncoding::Utf16
        )
        .is_empty()
    );
    // Generated condition names and literal c-names are navigable, but a simple
    // control-word rename cannot safely update their declarations and siblings.
    for source in [
        "\\newif\\ifaudiflag\n\\ifaudiflag x\\fi",
        "\\ExplSyntaxOn\\cs_new:cpn {audiliteral} #1 {#1}\\ExplSyntaxOff\n\\audiliteral{x}",
    ] {
        let (db, path) = database(source);
        let edits = compute_rename(
            &db.snapshot(),
            &path,
            Position::new(1, 3),
            "other",
            PositionEncoding::Utf16,
        );
        assert!(edits.is_none(), "unsafe rename: {edits:?}");
    }
}

#[test]
fn document_extensions_share_unknown_command_diagnostics() {
    for extension in ["tex", "ltx", "tikz"] {
        let mut db = IncrementalDatabase::default();
        let path = PathBuf::from(fixture_path!("/audit/main.tex")).with_extension(extension);
        db.apply_change(&path, "\\auditypo\n", None);
        let report = diagnostic_store::document_diagnostics(
            &db.snapshot(),
            &path,
            file_kind_for(&path),
            &RuleSelection::all(),
            PositionEncoding::Utf16,
            None,
        );
        assert!(
            report["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "unknown-command"),
            "{extension}"
        );
    }
}

#[test]
fn comment_continued_bibliography_styles_keep_links_and_token_fragments() {
    let source = "😀 \\usepackage[sty% key\r\n le={num% 😀 style\r\n eric}]{biblatex}\r\n";
    let (mut db, path) = database(source);
    let targets: Vec<_> = ["numeric.bbx", "numeric.cbx"]
        .into_iter()
        .map(|name| path.parent().unwrap().join(name))
        .collect();
    for target in &targets {
        db.apply_change(target, "% style\n", None);
    }
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        let idx = snapshot.file_line_index(file, enc);
        let tokens = semantic_tokens::compute(&snapshot, &path, enc, None);
        let mut spans = Vec::new();
        let (mut line, mut column) = (0, 0);
        for token in tokens["data"].as_array().unwrap().as_chunks::<5>().0 {
            let delta = token[0].as_u64().unwrap() as u32;
            line += delta;
            column = if delta == 0 { column } else { 0 } + token[1].as_u64().unwrap() as u32;
            if token[3] == 7 {
                spans.push(Range::new(
                    Position::new(line, column),
                    Position::new(line, column + token[2].as_u64().unwrap() as u32),
                ));
            }
        }
        for fragment in ["num", "eric"] {
            let start = source.find(fragment).unwrap();
            let (line, column) = idx.position(start);
            let (end_line, end_column) = idx.position(start + fragment.len());
            let range = Range::new(
                Position::new(line, column),
                Position::new(end_line, end_column),
            );
            let links = compute_goto_definition(&snapshot, &path, range.start, enc);
            assert_eq!(links.len(), 2, "{fragment}");
            for target in &targets {
                assert!(links.iter().any(|link| {
                    link.target_uri == path_to_uri(target).unwrap()
                        && link.origin_selection_range == Some(range)
                }));
            }
            assert!(spans.contains(&range), "{fragment}: {spans:?}");
            let completion = compute_completion(
                &snapshot,
                &path_to_uri(&path).unwrap(),
                &path,
                enc,
                Position::new(line, column + 1),
            );
            assert!(completion.items.iter().all(|item| item.label != "numeric"));
        }
    }
}

#[test]
fn bibliography_option_file_roles_share_links_completion_and_highlighting() {
    let source = "\\usepackage[style={numeric},citestyle=authoryear]{biblatex}\n";
    let (mut db, path) = database(source);
    for filename in ["numeric.bbx", "numeric.cbx", "authoryear.cbx"] {
        db.apply_change(&path.parent().unwrap().join(filename), "% style\n", None);
    }
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        let idx = snapshot.file_line_index(file, enc);
        for (name, expected) in [("numeric", 2), ("authoryear", 1)] {
            let offset = source.find(name).unwrap() + 2;
            let (line, character) = idx.position(offset);
            let links =
                compute_goto_definition(&snapshot, &path, Position::new(line, character), enc);
            assert_eq!(links.len(), expected, "{name}");
            let list = compute_completion(
                &snapshot,
                &path_to_uri(&path).unwrap(),
                &path,
                enc,
                Position::new(line, character),
            );
            let item = list
                .items
                .iter()
                .find(|item| item.label == name)
                .unwrap_or_else(|| panic!("style completion {name}"));
            let json = serde_json::to_value(item).unwrap();
            let edit: TextEdit = serde_json::from_value(json["textEdit"].clone()).unwrap();
            assert_eq!(edit.new_text, name);
            assert_eq!(
                edit.range.start.character as usize,
                source.find(name).unwrap()
            );
            assert_eq!(
                edit.range.end.character as usize,
                source.find(name).unwrap() + name.len()
            );
        }
    }
    db.apply_change(&path, "\\usepackage[style=\\dynamic]{biblatex}\n", None);
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(&path).unwrap();
    assert!(!snapshot.file_references(file).iter().any(|reference| {
        reference
            .candidates
            .installed
            .as_ref()
            .is_some_and(|(name, _)| name.contains("dynamic"))
    }));
}

#[test]
fn installed_and_kernel_definitions_reach_all_editor_features() {
    use tex_ls_analysis::external::{
        ExternalInputKind, ExternalInputs, FileInputs, InstalledMetadata, LocationObservation,
        Observation,
    };
    let (mut db, path) =
        database("\\usepackage{audipkg}\n\\audikernel{value}\n\\audipackage{one}{two}\n");
    let kernel = path.parent().unwrap().join("texmf/latex.ltx");
    let package = path.parent().unwrap().join("texmf/audipkg.sty");
    db.apply_change(&kernel, "\\newcommand{\\audikernel}[1]{#1}\n", None);
    db.apply_change(&package, "\\newcommand{\\audipackage}[2]{#1#2}\n", None);
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Installed)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Installed(Observation::Present(InstalledMetadata {
            toolchain: "test".into(),
            index: TexmfIndex::from_files(
                [
                    ("latex.ltx".into(), kernel),
                    ("audipkg.sty".into(), package),
                ]
                .into(),
            ),
        })),
    )
    .unwrap();
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Files(FileInputs {
            locations: ["audipkg.sty", "audipkg.dtx"]
                .into_iter()
                .map(|name| {
                    (
                        path.parent().unwrap().join(name),
                        LocationObservation {
                            kind: Observation::Absent,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_editor_command(&db, &path, "audikernel", 1);
    assert_editor_command(&db, &path, "audipackage", 2);
    let snapshot = db.snapshot();
    let file = snapshot.lookup_file(&path).unwrap();
    assert!(
        snapshot
            .scope_signatures(file)
            .command("audikernel")
            .is_none()
    );
    assert!(
        snapshot
            .scope_signatures(file)
            .command("audipackage")
            .is_none()
    );
}

#[test]
fn alias_signatures_follow_source_order_and_do_not_guess_forward_targets() {
    for source in [
        "\\let\\audiforward=\\later\n\\newcommand{\\later}[1]{#1}\n\\audiforward{x}\n",
        "\\NewCommandCopy{\\audiforward}{\\later}\n\\input{later}\n\\audiforward{x}\n",
    ] {
        let (mut db, path) = database(source);
        db.apply_change(
            &path.parent().unwrap().join("later.tex"),
            "\\newcommand{\\later}[1]{#1}\n",
            None,
        );
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(&path).unwrap();
        assert!(
            snapshot
                .editor_symbols(file)
                .commands
                .contains("audiforward")
        );
        assert!(
            snapshot
                .editor_signatures(file)
                .command("audiforward")
                .is_none()
        );
    }
    let (mut db, path) =
        database("\\input{later}\n\\NewCommandCopy{\\audicopy}{\\later}\n\\audicopy{x}\n");
    db.apply_change(
        &path.parent().unwrap().join("later.tex"),
        "\\newcommand{\\later}[1]{#1}\n",
        None,
    );
    assert_editor_command(&db, &path, "audicopy", 1);
}
