use super::*;
use crate::test_host::TestHost;
use tex_ls_analysis::linter::lint_document;
#[test]
fn refactoring_uses_snapshot_syntax_without_running_lint() {
    let mut db = IncrementalDatabase::default();
    let path = PathBuf::from(fixture_path!("/project/main.tex"));
    let uri = path_to_uri(&path).unwrap();
    let source = r"\begin{tabular}{c} x \\ \end{tabular}";
    let buffer = TextBuffer::new(source, PositionEncoding::Utf16);
    let range = Range::new(Position::new(0, 20), Position::new(0, 20));
    let root = parse_with_declarations(
        source,
        FileKind::Tex.lex_config(),
        &ResolvedDeclarations::default(),
    )
    .syntax();
    let expected = code_action::table_column_actions(&root, &buffer, &uri, range);
    assert!(!expected.is_empty());
    let rules = RuleSelection::resolve(None, &[]).0;
    for tracked in [source, "Different tracked text"] {
        db.apply_change(&path, tracked, None);
        db.clear_query_log();
        let snapshot = db.snapshot();
        let actual = compute_code_actions(
            &snapshot,
            &uri,
            &path,
            FileKind::Tex,
            range,
            Some(&[CodeActionKind::Refactor]),
            &rules,
            PositionEncoding::Utf16,
        );
        let actual: Vec<_> = actual.into_iter().filter(|action| !matches!(action, CodeActionResponse::CodeAction(action) if action.kind.as_ref().is_some_and(|kind| kind.as_str()=="refactor.rewrite.environment"))).collect();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            if tracked == source {
                serde_json::to_value(&expected).unwrap()
            } else {
                serde_json::json!([])
            }
        );
        assert!(
            !db.query_log()
                .iter()
                .any(|q| q.kind == tex_ls_analysis::incremental::QueryKind::LatexLintFindings)
        );
    }
}
#[test]
fn diagnostics_and_code_actions_share_raw_findings() {
    let mut db = IncrementalDatabase::default();
    let path = PathBuf::from(fixture_path!("/project/main.tex"));
    let file = db.apply_change(&path, "\\bf text\n", None);
    let rules = RuleSelection::resolve(None, &[]).0;
    db.clear_query_log();
    let snapshot = db.snapshot();
    let diagnostics = analyze_tex(&snapshot, &path, &rules, PositionEncoding::Utf16).unwrap();
    let findings = lint_findings(&snapshot, &path, FileKind::Tex, &rules).unwrap();
    assert!(!diagnostics.is_empty());
    assert!(findings.iter().any(|d| d.fix.is_some()));
    assert_eq!(findings, snapshot.latex_lint_findings(file));
    let disabled = RuleSelection::resolve(Some(&[]), &[]).0;
    assert!(
        lint_findings(&snapshot, &path, FileKind::Tex, &disabled)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|q| q.kind == tex_ls_analysis::incremental::QueryKind::LatexLintFindings)
            .count(),
        1
    );
}
#[test]
fn diagnostic_resolution_is_demanded_only_for_local_sites() {
    let mut db = IncrementalDatabase::default();
    let main =
        tex_ls_analysis::source::normalize_path(Path::new(fixture_path!("/project/main.tex")));
    let file = db.apply_change(&main, "\\documentclass{article}\nOrdinary prose.\n", None);
    let other = db.apply_change(
        &PathBuf::from(fixture_path!("/project/other.tex")),
        "\\label{other}\n",
        None,
    );
    db.clear_query_log();
    {
        let snapshot = db.snapshot();
        let root = snapshot.parsed_tree(file);
        let model = snapshot.semantic_model(file);
        let lazy = snapshot.latex_lint_findings(file);
        let log = db.query_log();
        assert!(!log.iter().any(|q| matches!(
            q.kind,
            tex_ls_analysis::incremental::QueryKind::ResolvedLabels
                | tex_ls_analysis::incremental::QueryKind::ResolvedCitations
        )));
        assert!(!log.iter().any(|q| q.file == Some(other)
            && q.kind == tex_ls_analysis::incremental::QueryKind::ParsedDocument));
        let (labels, cites) = snapshot.resolve_project();
        let eager = lint_document(
            &main,
            &root,
            model,
            Some(labels),
            Some(cites),
            Some(snapshot.resolve_package_options()),
        );
        assert_eq!(format!("{lazy:?}"), format!("{eager:?}"));
    }
    // Finding sites must demand resolution in a fresh database as well.
    let mut db = IncrementalDatabase::default();
    let file = db.apply_change(
        &main,
        "\\documentclass{article}\n\\ref{missing}\\cite{missing}\\label{a}\n",
        None,
    );
    db.clear_query_log();
    let snapshot = db.snapshot();
    let root = snapshot.parsed_tree(file);
    let model = snapshot.semantic_model(file);
    let lazy = snapshot.latex_lint_findings(file);
    let log = db.query_log();
    assert!(
        log.iter()
            .any(|q| q.kind == tex_ls_analysis::incremental::QueryKind::ResolvedLabels)
    );
    assert!(
        log.iter()
            .any(|q| q.kind == tex_ls_analysis::incremental::QueryKind::ResolvedCitations)
    );
    let (labels, cites) = snapshot.resolve_project();
    let eager = lint_document(
        &main,
        &root,
        model,
        Some(labels),
        Some(cites),
        Some(snapshot.resolve_package_options()),
    );
    assert_eq!(format!("{lazy:?}"), format!("{eager:?}"));
}
fn uri(s: &str) -> Uri {
    s.parse().unwrap()
}
#[test]
fn requested_code_action_kind_admits_descendants_only() {
    assert!(code_action_kind_requested(
        &CodeActionKind::RefactorRewrite,
        None
    ));
    assert!(code_action_kind_requested(
        &CodeActionKind::RefactorRewrite,
        Some(std::slice::from_ref(&CodeActionKind::Refactor))
    ));
    assert!(!code_action_kind_requested(
        &CodeActionKind::RefactorRewrite,
        Some(std::slice::from_ref(&CodeActionKind::QuickFix))
    ));
}
#[test]
fn lint_tags_map_dead_and_deprecated_rules() {
    // A dead label definition dims (Unnecessary).
    assert_eq!(
        lint_diagnostic_tags("unreferenced-label"),
        Some(vec![DiagnosticTag::Unnecessary])
    );
    // Superseded commands/environments strike through (Deprecated).
    for rule in [
        "deprecated-command",
        "obsolete-environment",
        "primitive-command",
    ] {
        assert_eq!(
            lint_diagnostic_tags(rule),
            Some(vec![DiagnosticTag::Deprecated]),
            "rule {rule}"
        );
    }
    // An ordinary lint carries no tag.
    assert_eq!(lint_diagnostic_tags("straight-quotes"), None);
}
#[test]
fn lint_to_lsp_links_each_languages_rule_catalogue() {
    let d = || tex_ls_analysis::linter::Diagnostic {
        rule: "deprecated-command",
        severity: tex_ls_analysis::linter::Severity::Warning,
        path: PathBuf::from("x.tex"),
        start: 0,
        end: 3,
        message: "use bfseries".to_owned(),
        fix: None,
        related: Vec::new(),
    };
    let idx = LineIndex::with_encoding("\\bf x", PositionEncoding::Utf16);

    // LaTeX rules retain their documentation and tags.
    let latex = lint_to_lsp(&idx, d(), true, Path::new("x.tex"));
    assert_eq!(
        latex.code_description.map(|c| c.href.to_string()),
        Some(
            "https://github.com/backmatter/tex-ls/blob/main/docs/reference/linter-rules.md#deprecated-command"
                .to_owned()
        )
    );
    assert_eq!(latex.tags, Some(vec![DiagnosticTag::Deprecated]));

    let mut finding = d();
    finding.rule = "empty-field";
    let bib = lint_to_lsp(&idx, finding, false, Path::new("x.bib"));
    assert_eq!(
        bib.code_description.unwrap().href.as_str(),
        "https://github.com/backmatter/tex-ls/blob/main/docs/reference/bib-linter-rules.md#empty-field"
    );
    assert_eq!(bib.code, Some(Code::String("empty-field".to_owned())));
}
#[test]
fn lint_to_lsp_builds_related_information() {
    // A same-file secondary resolves its range against the current index; a
    // cross-file one is a file-level `0..0` link to the other document.
    let text = "\\label{a}\\label{a}\n";
    let idx = LineIndex::with_encoding(text, PositionEncoding::Utf16);
    let d = tex_ls_analysis::linter::Diagnostic {
        rule: "duplicate-label",
        severity: tex_ls_analysis::linter::Severity::Warning,
        path: PathBuf::from(fixture_path!("/p/main.tex")),
        start: 9,
        end: 18,
        message: "label `a` is defined more than once".to_owned(),
        fix: None,
        related: vec![
            tex_ls_analysis::linter::RelatedInfo {
                path: PathBuf::from(fixture_path!("/p/main.tex")),
                start: 7,
                end: 8,
                message: "first definition of `a`".to_owned(),
            },
            tex_ls_analysis::linter::RelatedInfo {
                path: PathBuf::from(fixture_path!("/p/other.tex")),
                start: 0,
                end: 0,
                message: "other definition of `a`".to_owned(),
            },
        ],
    };
    let lsp = lint_to_lsp(&idx, d, true, Path::new(fixture_path!("/p/main.tex")));
    let related = lsp.related_information.expect("related present");
    assert_eq!(related.len(), 2);

    // Same-file: real range (line 0, cols 7..8), self URI.
    assert_eq!(related[0].message, "first definition of `a`");
    assert_eq!(related[0].location.uri, uri(fixture_uri!("/p/main.tex")));
    assert_eq!(related[0].location.range.start, Position::new(0, 7));
    assert_eq!(related[0].location.range.end, Position::new(0, 8));

    // Cross-file: file-level `0..0` at the other document's start.
    assert_eq!(related[1].message, "other definition of `a`");
    assert_eq!(related[1].location.uri, uri(fixture_uri!("/p/other.tex")));
    assert_eq!(related[1].location.range, Range::default());
}
#[test]
fn uri_to_fs_path_handles_unix_and_windows() {
    #[cfg(not(windows))]
    // Unix: the leading slash is the filesystem root and must be kept.
    assert_eq!(
        uri_to_fs_path(&uri("file:///tmp/dir/main.tex")),
        Some(PathBuf::from("/tmp/dir/main.tex"))
    );
    // Windows: the leading slash before the drive letter is URI syntax only.
    assert_eq!(
        uri_to_fs_path(&uri("file:///C:/Users/me/main.tex")),
        Some(PathBuf::from(if cfg!(windows) {
            "C:/Users/me/main.tex"
        } else {
            "/C:/Users/me/main.tex"
        }))
    );
    // Non-file scheme (unsaved buffer) → no path.
    assert_eq!(uri_to_fs_path(&uri("untitled:Untitled-1")), None);
}
#[test]
fn uri_to_fs_path_spells_separators_natively() {
    // `Path` compares by component, so the assertions above pass either way;
    // this pins the *spelling*, which is what a viewer sees in `%f`.
    let path = uri_to_fs_path(&uri("file:///C:/Users/me/main.tex")).expect("a path");
    let expected = if cfg!(windows) {
        "C:\\Users\\me\\main.tex"
    } else {
        "/C:/Users/me/main.tex"
    };
    assert_eq!(path.display().to_string(), expected);
}
/// A ranged `didChange` content change, spelled the way a client sends it.
fn ranged(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
    lsp_types::TextDocumentContentChangePartial {
        range: Range {
            start: Position::new(start.0, start.1),
            end: Position::new(end.0, end.1),
        },
        text: text.to_owned(),
        ..Default::default()
    }
    .into()
}
/// A range-less content change: the whole-buffer replacement.
fn whole(text: &str) -> TextDocumentContentChangeEvent {
    lsp_types::TextDocumentContentChangeWholeDocument {
        text: text.to_owned(),
    }
    .into()
}
/// Apply `changes` to `old` and assert the reported chain is *exactly* the
/// transform that happened — the one property `parsed_document`'s replay
/// leans on (`reparse_edits` rejects a chain landing anywhere else, so a
/// violation costs a full parse per keystroke and is otherwise invisible).
///
/// Returns the new text and the chain, for the caller's own assertions.
fn assert_chain_round_trips(
    old: &str,
    encoding: PositionEncoding,
    changes: Vec<TextDocumentContentChangeEvent>,
) -> (String, Option<Vec<Edit>>) {
    let mut buffer = Arc::new(TextBuffer::new(old, encoding));
    let edits = apply_content_changes(&mut buffer, changes).unwrap();
    if let Some(edits) = edits.as_deref() {
        assert_eq!(
            tex_ls_parser::parser::try_apply_edits(old, edits).as_deref(),
            Some(buffer.text()),
            "chain {edits:?} does not reproduce the buffer from {old:?}",
        );
    }
    (buffer.text().to_owned(), edits)
}
#[test]
fn apply_content_changes_splices_ranged_edit() {
    // Replace "world" with "there" in "hello world".
    let (text, edits) = assert_chain_round_trips(
        "hello world\n",
        PositionEncoding::Utf16,
        vec![ranged((0, 6), (0, 11), "there")],
    );
    assert_eq!(text, "hello there\n");
    assert_eq!(
        edits,
        Some(vec![Edit {
            range: 6..11,
            insert: "there".to_owned(),
        }])
    );
}
#[test]
fn apply_content_changes_full_replace_on_no_range() {
    let (text, edits) =
        assert_chain_round_trips("old", PositionEncoding::Utf16, vec![whole("new")]);
    assert_eq!(text, "new");
    // A whole-buffer replacement is an unknown transform: a ~100% window the
    // reparse would decline, so the chain degrades rather than describing it.
    assert_eq!(edits, None);
}
#[test]
fn apply_content_changes_reports_an_insert_and_a_delete() {
    let (text, edits) = assert_chain_round_trips(
        "\\section{Hi}\n",
        PositionEncoding::Utf16,
        vec![ranged((0, 10), (0, 10), "z")],
    );
    assert_eq!(text, "\\section{Hzi}\n");
    assert_eq!(edits.unwrap()[0].insert, "z");

    let (text, edits) = assert_chain_round_trips(
        "\\section{Hzi}\n",
        PositionEncoding::Utf16,
        vec![ranged((0, 10), (0, 11), "")],
    );
    assert_eq!(text, "\\section{Hi}\n");
    assert_eq!(
        edits,
        Some(vec![Edit {
            range: 10..11,
            insert: String::new(),
        }])
    );
}
/// Every change after the first is expressed against the text its
/// predecessors produced — the shape `apply_edits` folds, and the reason the
/// chain is a `Vec` rather than a single spanning edit.
#[test]
fn apply_content_changes_chains_a_multi_change_batch() {
    let (text, edits) = assert_chain_round_trips(
        "ab\n",
        PositionEncoding::Utf16,
        vec![
            ranged((0, 1), (0, 1), "XYZ"),
            // Against "aXYZb\n", not against "ab\n".
            ranged((0, 4), (0, 5), "-"),
        ],
    );
    assert_eq!(text, "aXYZ-\n");
    assert_eq!(
        edits,
        Some(vec![
            Edit {
                range: 1..1,
                insert: "XYZ".to_owned(),
            },
            Edit {
                range: 4..5,
                insert: "-".to_owned(),
            },
        ])
    );
}
/// One unknown transform poisons the whole batch: a chain has to describe the
/// entire step from the old text to the new one, and the ranged changes after
/// a full replace are expressed against a text the base never held.
#[test]
fn apply_content_changes_degrades_a_batch_mixing_a_full_replace() {
    let (text, edits) = assert_chain_round_trips(
        "old\n",
        PositionEncoding::Utf16,
        vec![whole("new\n"), ranged((0, 3), (0, 3), "!")],
    );
    assert_eq!(text, "new!\n");
    assert_eq!(edits, None);
}
#[test]
fn apply_content_changes_reports_an_empty_batch_as_an_empty_chain() {
    let (text, edits) = assert_chain_round_trips("x\n", PositionEncoding::Utf16, vec![]);
    assert_eq!(text, "x\n");
    // Staging this appends nothing, which is right: nothing happened.
    assert_eq!(edits, Some(Vec::new()));
}
#[test]
fn invalid_change_rejects_the_entire_batch() {
    let mut buffer = Arc::new(TextBuffer::new("hello\n", PositionEncoding::Utf16));
    let original = buffer.clone();
    let result = apply_content_changes(
        &mut buffer,
        vec![
            ranged((0, 0), (0, 0), "first"),
            ranged((0, 4), (0, 1), "invalid"),
        ],
    );
    assert_eq!(result, Err(ContentChangeError { index: 1 }));
    assert!(Arc::ptr_eq(&buffer, &original));
}
/// CRLF handling must not assume a one-byte line terminator. The `\r` is a line's
/// content as far as byte offsets go, so a column at the line's end lands
/// before it.
#[test]
fn apply_content_changes_reports_offsets_across_crlf() {
    let (text, edits) = assert_chain_round_trips(
        "ab\r\ncd\r\n",
        PositionEncoding::Utf16,
        vec![ranged((1, 1), (1, 1), "X")],
    );
    assert_eq!(text, "ab\r\ncXd\r\n");
    assert_eq!(
        edits,
        Some(vec![Edit {
            range: 5..5,
            insert: "X".to_owned(),
        }])
    );
}
/// `offset_at` is the only thing between the chain and a mid-codepoint slice:
/// a UTF-16 column counts units, and `\alpha` beside a literal astral char is
/// ordinary LaTeX. Checked in both negotiated encodings.
#[test]
fn apply_content_changes_reports_char_boundary_offsets() {
    for encoding in [PositionEncoding::Utf16, PositionEncoding::Utf8] {
        // "a𝕏b": the astral char is 4 bytes, 2 UTF-16 units.
        let column = if encoding == PositionEncoding::Utf16 {
            3
        } else {
            5
        };
        let (text, edits) = assert_chain_round_trips(
            "a𝕏b\n",
            encoding,
            vec![ranged((0, column), (0, column), "!")],
        );
        assert_eq!(text, "a𝕏!b\n", "{encoding:?}");
        assert_eq!(
            edits,
            Some(vec![Edit {
                range: 5..5,
                insert: "!".to_owned(),
            }]),
            "{encoding:?}",
        );
    }
}
/// An edit that lands *on* a `\r\n` is where a patched line table is most
/// likely to go wrong, because the two bytes are one terminator and the edit
/// reads across the seam: deleting the `\n` leaves a bare `\r` that still
/// breaks, and both directions change the line count without either byte
/// moving. Exercise this through the protocol as well as the line-index unit tests.
///
/// Every test in this module is a patch oracle too — tests build in debug, so
/// `with_replacement`'s `debug_assert` rescans on each splice — but only this
/// one puts a CRLF under it.
#[test]
fn apply_content_changes_edits_a_crlf_terminator() {
    // Deleting a whole `\r\n` joins two lines. The pair can only be addressed
    // as a whole: column 5 is the end of the visible line and `(1, 0)` is the
    // start of the next, so there is no position *between* the `\r` and the
    // `\n` — which is what `offset_at` promises and why the seam always moves
    // as a unit from the client's side.
    let (text, edits) = assert_chain_round_trips(
        "alpha\r\nbeta\r\n",
        PositionEncoding::Utf16,
        vec![ranged((0, 5), (1, 0), "")],
    );
    assert_eq!(text, "alphabeta\r\n");
    assert_eq!(
        edits,
        Some(vec![Edit {
            range: 5..7,
            insert: String::new(),
        }])
    );

    // An insert just before the `\r` leaves the terminator whole.
    let (text, _) = assert_chain_round_trips(
        "alpha\r\nbeta\r\n",
        PositionEncoding::Utf16,
        vec![ranged((0, 5), (0, 5), "X")],
    );
    assert_eq!(text, "alphaX\r\nbeta\r\n");

    // An inserted `\r` in the same place does not: it breaks on its own, so
    // the document gains a line without either byte of the original pair
    // moving. This is the shape a table carrying its boundary verdict across
    // an edit gets wrong.
    let (text, _) = assert_chain_round_trips(
        "alpha\r\nbeta\r\n",
        PositionEncoding::Utf16,
        vec![ranged((0, 5), (0, 5), "\r")],
    );
    assert_eq!(text, "alpha\r\r\nbeta\r\n");

    // A second change resolving against the first: line 1 is only reachable
    // if the patched table shifted, so this fails loudly on a stale one.
    let (text, _) = assert_chain_round_trips(
        "alpha\r\nbeta\r\n",
        PositionEncoding::Utf16,
        vec![
            ranged((0, 5), (0, 5), "\r\nmid"),
            ranged((2, 0), (2, 4), "BETA"),
        ],
    );
    assert_eq!(text, "alpha\r\nmid\r\nBETA\r\n");
}
/// An edit beside an astral char that also adds a line: the wide-line flags
/// have to splice *and* the joined lines have to be re-derived, together. The
/// positions in the second change are only meaningful if both happened.
#[test]
fn apply_content_changes_edits_beside_a_wide_char() {
    let (text, _) = assert_chain_round_trips(
        "a𝕏b\nplain\n",
        PositionEncoding::Utf16,
        vec![
            // After `a𝕏` — one UTF-16 unit for `a`, two for the astral char.
            ranged((0, 3), (0, 3), "\nnew"),
            ranged((2, 0), (2, 5), "PLAIN"),
        ],
    );
    assert_eq!(text, "a𝕏\nnewb\nPLAIN\n");
}
/// The byte offset of the first occurrence of `needle` in `text`.
fn offset_of(text: &str, needle: &str) -> usize {
    text.find(needle).expect("needle present")
}
#[test]
fn reference_under_cursor_finds_ref_and_cite() {
    let text = "\\label{a}\n\\ref{a}\n\\cite{k}\n";
    let model = SemanticModel::build(&SyntaxNode::new_root(parse(text).green));

    // Inside `\ref{a}` → the label key `a`.
    let at_ref = offset_of(text, "\\ref{a}") + 5; // on the `a`
    match reference_under_cursor(&model, at_ref) {
        Some(CursorTarget::Labels(names)) => assert_eq!(names, vec![SmolStr::new("a")]),
        other => panic!("expected a label target, got {other:?}"),
    }

    // Inside `\cite{k}` → the cite key `k`.
    let at_cite = offset_of(text, "\\cite{k}") + 6; // on the `k`
    match reference_under_cursor(&model, at_cite) {
        Some(CursorTarget::Citations(names)) => assert_eq!(names, vec![SmolStr::new("k")]),
        other => panic!("expected a citation target, got {other:?}"),
    }

    // On the `\label` definition (not a reference) → nothing to jump *from*.
    let at_label = offset_of(text, "\\label{a}") + 1;
    assert!(reference_under_cursor(&model, at_label).is_none());
}
#[test]
fn reference_under_cursor_selects_one_cref_key() {
    let text = "\\cref{a,b,c}\n";
    let model = SemanticModel::build(&SyntaxNode::new_root(parse(text).green));
    assert!(reference_under_cursor(&model, 2).is_none());
    match reference_under_cursor(&model, 8) {
        Some(CursorTarget::Labels(names)) => assert_eq!(names, vec![SmolStr::new("b")]),
        other => panic!("expected selected b, got {other:?}"),
    }
}

#[test]
fn path_to_uri_round_trips_through_uri_to_fs_path() {
    let p = PathBuf::from(fixture_path!("/tmp/my dir/main.tex"));
    let u = path_to_uri(&p).expect("a file path forms a URI");
    // The space is percent-encoded in the URI text…
    assert!(u.as_str().contains("%20"), "got {}", u.as_str());
    // …and decodes back to the original filesystem path.
    assert_eq!(uri_to_fs_path(&u), Some(p));
}
#[test]
fn package_completion_surfaces_installed_set_and_ctan_detail() {
    use tex_ls_analysis::completion::FileArgKind;
    use tex_ls_parser::semantic::signature::SignatureDb;

    // A tree with an installed package that is *not* in the baked CTAN list.
    let tree = tempfile::tempdir().unwrap();
    let sty = tree.path().join("tex/latex/zzlocalpkg/zzlocalpkg.sty");
    std::fs::create_dir_all(sty.parent().unwrap()).unwrap();
    std::fs::write(&sty, "").unwrap();
    let texmf = TestHost::from_roots(&[tree.path().to_path_buf()]).index;

    let sigs = SignatureDb::default();
    let root = SyntaxNode::new_root(parse("").green);
    let model = SemanticModel::build(&root);
    let doc = uri(fixture_uri!("/proj/main.tex"));

    // The installed-set tier surfaces the local install; an empty index does not.
    let installed = package_completion_items(
        &doc,
        "zzlocal",
        FileArgKind::Package,
        &sigs,
        &model,
        &texmf,
        &TestHost::default(),
    );
    assert!(installed.iter().any(|i| i.label == "zzlocalpkg"));
    assert!(
        package_completion_items(
            &doc,
            "zzlocal",
            FileArgKind::Package,
            &sigs,
            &model,
            &TexmfIndex::default(),
            &TestHost::default()
        )
        .is_empty()
    );

    // A baked CTAN name is enriched with its shipped description as `detail`.
    let baked = package_completion_items(
        &doc,
        "amsmath",
        FileArgKind::Package,
        &sigs,
        &model,
        &TexmfIndex::default(),
        &TestHost::default(),
    );
    let amsmath = baked
        .iter()
        .find(|i| i.label == "amsmath")
        .expect("amsmath from the baked list");
    assert!(
        amsmath.detail.as_deref().is_some_and(|d| d.contains("AMS")),
        "detail: {:?}",
        amsmath.detail
    );
}

use tex_ls_analysis::incremental::IncrementalDatabase;

#[test]
fn selected_reference_key_never_resolves_a_sibling() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/p/main.tex"));
    let source = "\\label{one}\n\\label{two}\n\\cref{one,two,missing}";
    db.apply_change(path, source, None);
    let snapshot = db.snapshot();
    let defs = compute_goto_definition(
        &snapshot,
        path,
        Position::new(2, 11),
        PositionEncoding::Utf16,
    );
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].target_selection_range.start.line, 1);
    assert!(
        compute_goto_definition(
            &snapshot,
            path,
            Position::new(2, 17),
            PositionEncoding::Utf16
        )
        .is_empty()
    );
    assert!(
        compute_goto_definition(
            &snapshot,
            path,
            Position::new(2, 2),
            PositionEncoding::Utf16
        )
        .is_empty()
    );
}

#[test]
fn glossary_identity_drives_cross_file_edits_and_highlights() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/p/main.tex"));
    db.apply_change(path, "\\input{defs}\n\\acrshort{cpu}\n\\ac{cpu}", None);
    db.apply_change(
        Path::new(fixture_path!("/p/defs.tex")),
        "\\DeclareAcronym{cpu}{short=CPU,long=processor}",
        None,
    );
    let snapshot = db.snapshot();
    let pos = Position::new(1, 11);
    let enc = PositionEncoding::Utf16;
    assert_eq!(compute_goto_definition(&snapshot, path, pos, enc).len(), 1);
    assert_eq!(compute_references(&snapshot, path, pos, true, enc).len(), 3);
    assert_eq!(
        compute_document_highlight(&snapshot, path, enc, pos).len(),
        2
    );
    assert!(hover::compute_hover(&snapshot, path, enc, pos).is_some());
    let edit = compute_rename(&snapshot, path, pos, "processor", enc).unwrap();
    assert_eq!(
        edit.changes.unwrap().values().map(Vec::len).sum::<usize>(),
        3
    );
}

#[test]
fn bib_string_operations_edit_only_identifiers_and_show_cycles() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/p/refs.bib"));
    let source = "@string{brand = {tex-ls}}\n@string{title = brand # { guide}}\n@book{x,title=title,note={brand}}\n@string{loop = loop}";
    db.apply_change(path, source, None);
    let snapshot = db.snapshot();
    let pos = Position::new(1, 18);
    let enc = PositionEncoding::Utf16;
    assert_eq!(compute_goto_definition(&snapshot, path, pos, enc).len(), 1);
    assert_eq!(compute_references(&snapshot, path, pos, true, enc).len(), 2);
    assert_eq!(
        compute_document_highlight(&snapshot, path, enc, pos).len(),
        2
    );
    let changes = compute_rename(&snapshot, path, pos, "product", enc)
        .unwrap()
        .changes
        .unwrap();
    assert_eq!(changes.values().map(Vec::len).sum::<usize>(), 2);
    let root = snapshot.parsed_bib_tree(snapshot.lookup_file(path).unwrap());
    assert_eq!(bib_strings::expanded(&root, "title"), "tex-ls guide");
    assert!(bib_strings::expanded(&root, "loop").contains("cyclic"));
    assert!(bib_strings::expanded(&root, "unknown").contains("unresolved"));
}

#[test]
fn linked_editing_and_pair_actions_preserve_the_body() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/p/main.tex"));
    let uri = path_to_uri(path).unwrap();
    let source = "\\begin{center}\nbody % keep\n\\end{center}";
    db.apply_change(path, source, None);
    let snapshot = db.snapshot();
    let enc = PositionEncoding::Utf16;
    assert_eq!(
        compute_linked_editing(&snapshot, path, enc, Position::new(0, 9))
            .unwrap()
            .ranges
            .len(),
        2
    );
    assert!(compute_linked_editing(&snapshot, path, enc, Position::new(1, 1)).is_none());
    let actions = compute_code_actions(
        &snapshot,
        &uri,
        path,
        FileKind::Tex,
        Range::new(Position::new(1, 1), Position::new(1, 1)),
        Some(&[CodeActionKind::Refactor]),
        &RuleSelection::resolve(None, &[]).0,
        enc,
    );
    assert!(actions.iter().any(|action| matches!(action,CodeActionResponse::CodeAction(action) if action.kind.as_ref().is_some_and(|kind|kind.as_str()=="refactor.rewrite.environment") && action.edit.as_ref().unwrap().changes.as_ref().unwrap().values().all(|edits| edits.len()==2))));
}

#[test]
fn environment_refactoring_accepts_its_exact_requested_kind() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/p/main.tex"));
    let uri = path_to_uri(path).unwrap();
    let source = "\\begin{center}\nbody % keep\n\\end{center}";
    db.apply_change(path, source, None);
    let enc = PositionEncoding::Utf16;
    let environment_kind = CodeActionKind::from("refactor.rewrite.environment");
    let actions = compute_code_actions(
        &db.snapshot(),
        &uri,
        path,
        FileKind::Tex,
        Range::new(Position::new(1, 1), Position::new(1, 1)),
        Some(std::slice::from_ref(&environment_kind)),
        &RuleSelection::resolve(None, &[]).0,
        enc,
    );
    assert!(!actions.is_empty());
    for action in actions {
        let CodeActionResponse::CodeAction(action) = action else {
            panic!("expected a literal environment action");
        };
        assert_eq!(action.kind, Some(environment_kind.clone()));
        let changes = action.edit.unwrap().changes.unwrap();
        assert_eq!(changes.len(), 1);
        let edits = &changes[&uri];
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].new_text, edits[1].new_text);
        let idx = LineIndex::new(source);
        let mut rewritten = source.to_owned();
        for edit in edits.iter().rev() {
            let start = idx.offset_at(edit.range.start.line, edit.range.start.character);
            let end = idx.offset_at(edit.range.end.line, edit.range.end.character);
            assert_eq!(&source[start..end], "center");
            rewritten.replace_range(start..end, &edit.new_text);
        }
        assert_eq!(
            rewritten,
            format!(
                "\\begin{{{0}}}\nbody % keep\n\\end{{{0}}}",
                edits[0].new_text
            )
        );
    }
    assert!(
        !db.query_log().iter().any(|query| {
            query.kind == tex_ls_analysis::incremental::QueryKind::LatexLintFindings
        })
    );
}

#[test]
fn shared_child_refuses_rename_and_inspection_exposes_roots() {
    let mut db = IncrementalDatabase::default();
    db.apply_change(
        Path::new(fixture_path!("/views/a.tex")),
        r"\documentclass{article}\input{shared}\label{a}",
        None,
    );
    db.apply_change(
        Path::new(fixture_path!("/views/b.tex")),
        r"\documentclass{article}\input{shared}\label{b}",
        None,
    );
    let child = Path::new(fixture_path!("/views/shared.tex"));
    db.apply_change(child, r"\label{shared}", None);
    let snapshot = db.snapshot();
    assert!(
        compute_prepare_rename(
            &snapshot,
            child,
            PositionEncoding::Utf16,
            Position::new(0, 9)
        )
        .is_none()
    );
    assert!(
        compute_rename(
            &snapshot,
            child,
            Position::new(0, 9),
            "other",
            PositionEncoding::Utf16
        )
        .is_none()
    );
    let result = projects::inspect(&snapshot, child);
    assert_eq!(result["ambiguous"], true);
    assert_eq!(
        result["candidateRoots"],
        serde_json::json!([
            tex_ls_analysis::source::normalize_path(Path::new(fixture_path!("/views/a.tex"))),
            tex_ls_analysis::source::normalize_path(Path::new(fixture_path!("/views/b.tex")))
        ])
    );
    let a = projects::inspect(&snapshot, Path::new(fixture_path!("/views/a.tex")));
    assert_eq!(
        a["members"],
        serde_json::json!([
            tex_ls_analysis::source::normalize_path(Path::new(fixture_path!("/views/a.tex"))),
            tex_ls_analysis::source::normalize_path(Path::new(fixture_path!("/views/shared.tex")))
        ])
    );
}

#[test]
fn citation_rename_from_source_refuses_a_bibliography_shared_by_other_roots() {
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        for shared in [false, true] {
            let mut db = IncrementalDatabase::default();
            let a = Path::new(fixture_path!("/rename/a.tex"));
            let bib = Path::new(fixture_path!("/rename/refs.bib"));
            let source = "\\documentclass{article}\n\\bibliography{refs}\n\\cite{old}";
            db.apply_change(a, source, None);
            if shared {
                db.apply_change(Path::new(fixture_path!("/rename/b.tex")), source, None);
            }
            db.apply_change(bib, "@misc{old,title={Title}}", None);
            let snapshot = db.snapshot();
            let position = Position::new(2, 7);
            assert_eq!(
                compute_prepare_rename(&snapshot, a, enc, position).is_some(),
                !shared
            );
            let edit = compute_rename(&snapshot, a, position, "replacement", enc);
            assert_eq!(edit.is_some(), !shared);
            // Starting at the bibliography can prove and update every citing root.
            let edit =
                compute_rename(&snapshot, bib, Position::new(0, 7), "replacement", enc).unwrap();
            let changes = edit.changes.unwrap();
            assert_eq!(changes.len(), if shared { 3 } else { 2 });
            assert!(
                changes
                    .values()
                    .all(|edits| edits.len() == 1 && edits[0].new_text == "replacement")
            );
        }
    }
}

#[test]
fn glossary_rename_refuses_shared_definitions_and_shared_uses() {
    for shared in [false, true] {
        for definition_in_shared in [false, true] {
            let mut db = IncrementalDatabase::default();
            let a = Path::new(fixture_path!("/rename/a.tex"));
            let definition = "\\newacronym{cpu}{CPU}{processor}";
            let use_site = "\\gls{cpu}";
            let local = if definition_in_shared {
                use_site
            } else {
                definition
            };
            let common = if definition_in_shared {
                definition
            } else {
                use_site
            };
            let source = format!("\\documentclass{{article}}\n\\input{{shared}}\n{local}");
            db.apply_change(a, source.as_str(), None);
            if shared {
                db.apply_change(
                    Path::new(fixture_path!("/rename/b.tex")),
                    source.as_str(),
                    None,
                );
            }
            db.apply_change(Path::new(fixture_path!("/rename/shared.tex")), common, None);
            let snapshot = db.snapshot();
            let position = Position::new(2, if definition_in_shared { 6 } else { 13 });
            assert_eq!(
                compute_prepare_rename(&snapshot, a, PositionEncoding::Utf16, position).is_some(),
                !shared
            );
            let edit = compute_rename(
                &snapshot,
                a,
                position,
                "replacement",
                PositionEncoding::Utf16,
            );
            assert_eq!(edit.is_some(), !shared);
            if let Some(edit) = edit {
                let changes = edit.changes.unwrap();
                assert_eq!(changes.len(), 2);
                assert!(
                    changes
                        .values()
                        .all(|edits| edits.len() == 1 && edits[0].new_text == "replacement")
                );
            }
        }
    }
}

#[test]
fn shared_package_rename_refuses_to_edit_only_one_loading_root() {
    let mut db = IncrementalDatabase::default();
    for path in [fixture_path!("/views/a.tex"), fixture_path!("/views/b.tex")] {
        db.apply_change(
            Path::new(path),
            r"\documentclass{article}\usepackage{shared}\foo",
            None,
        );
    }
    let package = Path::new(fixture_path!("/views/shared.sty"));
    db.apply_change(package, r"\newcommand{\foo}{x}", None);
    let snapshot = db.snapshot();
    assert!(
        compute_prepare_rename(
            &snapshot,
            package,
            PositionEncoding::Utf16,
            Position::new(0, 14)
        )
        .is_none()
    );
    assert!(
        compute_rename(
            &snapshot,
            Path::new(fixture_path!("/views/a.tex")),
            Position::new(0, 45),
            "bar",
            PositionEncoding::Utf16
        )
        .is_none()
    );
}

#[test]
fn manual_bibliography_identity_and_exact_definition_links() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/manual/main.tex"));
    let text =
        "\\documentclass{article}\n\\bibitem[Display]{manual:key} Body\n😀\\cite{manual:key}\n";
    db.apply_change(path, text, None);
    let position = Position {
        line: 2,
        character: 10,
    };
    let links = compute_goto_definition(&db, path, position, PositionEncoding::Utf16);
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].target_selection_range.start,
        Position {
            line: 1,
            character: 18
        }
    );
    assert_eq!(links[0].target_selection_range.end.character, 28);
    assert_eq!(links[0].origin_selection_range.unwrap().start.character, 8);
    assert_eq!(
        compute_references(&db, path, position, true, PositionEncoding::Utf16).len(),
        2
    );
    assert_eq!(
        compute_document_highlight(&db, path, PositionEncoding::Utf16, position).len(),
        2
    );
    let edits = compute_rename(&db, path, position, "changed", PositionEncoding::Utf16)
        .unwrap()
        .changes
        .unwrap();
    assert_eq!(edits[&path_to_uri(path).unwrap()].len(), 2);
    let mut fallback = serde_json::to_value(&links).unwrap();
    ResponsePolicy::default().response("textDocument/definition", None, &mut fallback);
    assert_eq!(
        fallback[0]["range"],
        serde_json::to_value(links[0].target_selection_range).unwrap()
    );
    let mut rich = serde_json::to_value(&links).unwrap();
    ResponsePolicy::new(
        &serde_json::json!({"capabilities":{"textDocument":{"definition":{"linkSupport":true}}}}),
    )
    .response("textDocument/definition", None, &mut rich);
    assert!(rich[0].get("targetRange").is_some());
    db.apply_change(
        path,
        "\\documentclass{article}\\bibitem{a}A\\bibitem{b}B\\cite{a}",
        None,
    );
    assert!(
        compute_rename(
            &db,
            path,
            Position {
                line: 0,
                character: 38
            },
            "b",
            PositionEncoding::Utf16
        )
        .is_none()
    );
}

#[test]
fn bibliography_structural_editing_and_rendering_in_both_encodings() {
    for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let mut db = IncrementalDatabase::default();
        let path = Path::new(fixture_path!("/bib/edit.bib"));
        let source = "@string{pub = {Press}}\n\n@book{key,\n title={😀 A title},\n author={Garc\\'ia, Jos\\'e},\n date={2024-2-9},\n publisher=pub,\n doi={10.1/example}\n}\n\n@comment{ Keep  bytes }\n@misc{untouched,title={  preserve  }}\n";
        db.apply_change(path, source, None);
        let position = |needle: &str| {
            let idx = LineIndex::with_encoding(source, encoding);
            let (line, character) = idx.position(source.find(needle).unwrap() + 2);
            Position { line, character }
        };
        let symbols = compute_bib_symbols(&db, path, encoding);
        assert_eq!(symbols[0].name, "pub");
        assert_eq!(symbols[1].kind, SymbolKind::Class);
        assert_eq!(symbols[1].children.as_ref().unwrap()[0].name, "title");
        let WorkspaceSymbolResponse::WorkspaceSymbolList(workspace) =
            compute_projects_workspace_symbols(
                &[db.snapshot()],
                "",
                encoding,
                &|_| Default::default(),
                None,
            )
        else {
            panic!()
        };
        assert_eq!(workspace.len(), 3, "fields do not flood workspace search");
        let selections =
            compute_selection_range(&db, path, encoding, FileKind::Bib, &[position("title}")]);
        let mut parent = selections[0].parent.as_deref();
        let mut depth = 0;
        while let Some(selection) = parent {
            depth += 1;
            parent = selection.parent.as_deref();
        }
        assert!(depth >= 3);
        let folds = compute_folding(&db, path, encoding, FileKind::Bib);
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].end_line, 8);
        let mut folds = serde_json::to_value(folds).unwrap();
        ResponsePolicy::new(&serde_json::json!({"capabilities":{"textDocument":{"foldingRange":{"lineFoldingOnly":true}}}})).response("textDocument/foldingRange", None, &mut folds);
        assert_eq!(folds[0]["endLine"], 7);
        let at = position("title=");
        let edits = compute_range_format(
            &db,
            path,
            encoding,
            FormatStyle::default(),
            FileKind::Bib,
            Range::new(at, at),
            SentenceOptions::default(),
        )
        .unwrap();
        assert_eq!(edits.len(), 1);
        let idx = LineIndex::with_encoding(source, encoding);
        let edit = &edits[0];
        let start = idx.offset_at(edit.range.start.line, edit.range.start.character);
        let end = idx.offset_at(edit.range.end.line, edit.range.end.character);
        let mut actual = source.to_owned();
        actual.replace_range(start..end, &edit.new_text);
        assert!(
            actual.ends_with("@comment{ Keep  bytes }\n@misc{untouched,title={  preserve  }}\n")
        );
        assert!(tex_ls_analysis::bib::parse(&actual).errors.is_empty());
        let field_hover = hover::compute_hover(&db, path, encoding, position("title=")).unwrap();
        assert!(
            serde_json::to_string(&field_hover)
                .unwrap()
                .contains("capitalization")
        );
        let card = hover::compute_hover(&db, path, encoding, position("key,")).unwrap();
        let mut plaintext = serde_json::to_value(card).unwrap();
        ResponsePolicy::default().response("textDocument/hover", None, &mut plaintext);
        let value = plaintext["contents"]["value"].as_str().unwrap();
        assert!(value.contains("José García"), "{value}");
        assert!(value.contains("2024-02-09"));
        assert!(value.contains("https://doi.org/10.1/example"));
        assert_eq!(compute_bib_document_link(&db, path, encoding).len(), 1);
        db.apply_change(path, actual.as_str(), None);
        assert!(
            compute_range_format(
                &db,
                path,
                encoding,
                FormatStyle::default(),
                FileKind::Bib,
                Range::new(at, at),
                SentenceOptions::default()
            )
            .unwrap()
            .is_empty()
        );
    }
}

#[test]
fn bibliography_tex_completion_is_value_specific_and_keeps_macros() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/bib/completion.bib"));
    let uri = path_to_uri(path).unwrap();
    let source = "@book{k,title={\\alpobsolete}}";
    db.apply_change(path, source, None);
    let items = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, 19),
    );
    let item = items
        .items
        .iter()
        .find(|item| item.label == "alpha")
        .unwrap();
    let lsp_types::CompletionItemTextEdit::TextEdit(edit) = item.text_edit.as_ref().unwrap() else {
        panic!()
    };
    let mut actual = source.to_owned();
    actual.replace_range(
        edit.range.start.character as usize..edit.range.end.character as usize,
        &edit.new_text,
    );
    assert_eq!(actual, "@book{k,title={\\alpha}}");
    db.apply_change(path, "@string{pub={Press}}\n@book{k,publisher=pu}", None);
    assert!(
        compute_bib_completion(&db, path, 41)
            .iter()
            .any(|item| item.label == "pub")
    );
    db.apply_change(path, "@book{k,url={\\alp}}", None);
    assert!(compute_bib_completion(&db, path, 17).is_empty());
}

#[test]
fn ranked_citations_replace_author_queries_and_preserve_resolve_edits() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/rank/main.tex"));
    let source = "\\documentclass{article}\\bibliography{refs}\\cite{jane doe}";
    let bibliography = format!(
        "@book{{target,title={{{}}},author={{Doe, Jane}}}}",
        "Long title ".repeat(40)
    );
    db.apply_change(path, source, None);
    db.apply_change(
        Path::new(fixture_path!("/rank/refs.bib")),
        bibliography.as_str(),
        None,
    );
    let list = compute_completion(
        &db,
        &path_to_uri(path).unwrap(),
        path,
        PositionEncoding::Utf16,
        Position::new(0, (source.len() - 1) as u32),
    );
    assert!(!list.is_incomplete);
    assert_eq!(list.items.len(), 1);
    let item = &list.items[0];
    assert!(item.filter_text.as_ref().unwrap().len() > 128);
    let lsp_types::CompletionItemTextEdit::TextEdit(edit) = item.text_edit.as_ref().unwrap() else {
        panic!()
    };
    let mut actual = source.to_owned();
    actual.replace_range(
        edit.range.start.character as usize..edit.range.end.character as usize,
        &edit.new_text,
    );
    assert!(actual.ends_with("\\cite{target}"), "{actual}");
    let resolved = completion_resolve::resolve(&db, item.clone());
    assert_eq!(resolved.text_edit, item.text_edit);
    assert_eq!(resolved.filter_text, item.filter_text);
    assert_eq!(resolved.sort_text, item.sort_text);
}

#[test]
fn completion_uses_unresolved_labels_matching_ends_and_observed_names() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/rank/main.tex"));
    let uri = path_to_uri(path).unwrap();
    let source = "\\ref{sec:missing}\\label{sec:}";
    db.apply_change(path, source, None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, (source.len() - 1) as u32),
    );
    assert_eq!(list.items[0].label, "sec:missing");
    let source = "\\begin{itemize}body\\end{itemize}";
    db.apply_change(path, source, None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, (source.rfind("itemize").unwrap() + 3) as u32),
    );
    assert_eq!(list.items[0].label, "itemize");
    assert_eq!(list.items[0].preselect, Some(true));
    let mut plain = serde_json::to_value(&list).unwrap();
    ResponsePolicy::default().response("textDocument/completion", None, &mut plain);
    assert!(plain["items"][0].get("preselect").is_none());
    let source = "\\mysteryObserved{body}\\verb|\\mysteryProtected|\n\\mystery";
    db.apply_change(path, source, None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(1, 8),
    );
    assert!(
        list.items
            .iter()
            .any(|item| item.label == "mysteryObserved")
    );
    assert!(
        !list
            .items
            .iter()
            .any(|item| item.label == "mysteryProtected")
    );
}

#[test]
fn completion_relevance_uses_loaded_packages_and_glyphs_without_signature_changes() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/rank/main.tex"));
    let source = "\\usepackage{amsmath}\\newcommand{\\myCommand}{body}\n\\";
    db.apply_change(path, source, None);
    let uri = path_to_uri(path).unwrap();
    let mut candidates = compute_tex_completion(&db, &uri, path, source.len());
    let (_, relevance, _) = completion_context::prepare(&db, path, source.len(), &mut candidates);
    assert_eq!(relevance["numberwithin"], 0);
    assert_eq!(relevance["myCommand"], 0);
    let unrelated = tex_ls_parser::semantic::signature::cwl()
        .command_names()
        .find(|name| {
            tex_ls_parser::semantic::signature::builtin()
                .command(name)
                .is_none()
                && !tex_ls_parser::semantic::signature::cwl()
                    .command_packages(name)
                    .contains(&"amsmath")
        })
        .unwrap();
    assert_eq!(relevance[unrelated], 2);
    let source = "\\alp";
    db.apply_change(path, source, None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, 4),
    );
    let alpha = list
        .items
        .iter()
        .find(|item| item.label == "alpha")
        .unwrap();
    assert!(alpha.detail.as_deref().unwrap().contains('α'));
    let resolved = completion_resolve::resolve(&db, alpha.clone());
    assert!(resolved.detail.as_deref().unwrap().contains('α'));
    assert_eq!(resolved.text_edit, alpha.text_edit);
}

#[test]
fn structural_command_snippets_only_fill_an_empty_tail() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/rank/main.tex"));
    let uri = path_to_uri(path).unwrap();
    db.apply_change(path, "\\be", None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, 3),
    );
    let begin = list
        .items
        .iter()
        .find(|item| item.label == "begin")
        .unwrap();
    assert_eq!(begin.insert_text_format, Some(InsertTextFormat::Snippet));
    assert!(
        serde_json::to_value(begin).unwrap()["textEdit"]["newText"]
            .as_str()
            .unwrap()
            .contains("${1:environment}")
    );
    let mut plain = serde_json::to_value(begin).unwrap();
    ResponsePolicy::default().response("completionItem/resolve", None, &mut plain);
    assert_eq!(plain["textEdit"]["newText"], "begin");
    db.apply_change(path, "\\begin{itemize}body\\end{itemize}", None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, 3),
    );
    assert!(
        list.items
            .iter()
            .find(|item| item.label == "begin")
            .unwrap()
            .insert_text_format
            != Some(InsertTextFormat::Snippet)
    );
    db.apply_change(path, "\\", None);
    let list = compute_completion(
        &db,
        &uri,
        path,
        PositionEncoding::Utf16,
        Position::new(0, 1),
    );
    for name in ["(", "["] {
        let item = list.items.iter().find(|item| item.label == name).unwrap();
        assert_eq!(item.insert_text_format, Some(InsertTextFormat::Snippet));
        assert!(
            serde_json::to_value(item).unwrap()["textEdit"]["newText"]
                .as_str()
                .unwrap()
                .contains("$0")
        );
    }
}

#[test]
fn fix_all_applies_safe_edits_without_formatting_and_preserves_unsafe_changes() {
    for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let mut db = IncrementalDatabase::default();
        let path = Path::new(fixture_path!("/fix/main.tex"));
        let uri = path_to_uri(path).unwrap();
        let source = "😀 $x^{2}$ and $y_{3}$. {\\bf bold}\n";
        db.apply_change(path, source, None);
        let rules = RuleSelection::all();
        let actions = compute_code_actions(
            &db,
            &uri,
            path,
            FileKind::Tex,
            Range::default(),
            Some(&[CodeActionKind::from("source.fixAll.tex-ls")]),
            &rules,
            encoding,
        );
        assert_eq!(actions.len(), 1);
        let CodeActionResponse::CodeAction(action) = &actions[0] else {
            panic!("literal action");
        };
        let edits = &action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri];
        let idx = LineIndex::with_encoding(source, encoding);
        let mut edits = edits
            .iter()
            .map(|edit| {
                (
                    idx.offset_at(edit.range.start.line, edit.range.start.character),
                    idx.offset_at(edit.range.end.line, edit.range.end.character),
                    edit.new_text.as_str(),
                )
            })
            .collect::<Vec<_>>();
        edits.sort();
        let mut output = source.to_owned();
        for (start, end, text) in edits.into_iter().rev() {
            output.replace_range(start..end, text);
        }
        assert_eq!(output, "😀 $x^2$ and $y_3$. {\\bf bold}\n");
        db.apply_change(path, output, None);
        assert!(
            compute_code_actions(
                &db,
                &uri,
                path,
                FileKind::Tex,
                Range::default(),
                Some(&[CodeActionKind::from("source.fixAll.tex-ls")]),
                &rules,
                encoding
            )
            .is_empty()
        );
    }
}

#[test]
fn outline_visibility_and_enriched_unicode_workspace_search() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/outline/main.tex"));
    let other = Path::new(fixture_path!("/outline/other.tex"));
    let source = "\\documentclass{article}\n\\section{Résumé Methods}\n\\begin{equation}x=1\\label{eq:one}\\end{equation}\n\\[y=2\\]\n\\begin{enumerate}\n\\item First\\label{item:a}\ncontinued\n\\item Second\n\\end{enumerate}\n% first\n% second\n\\begin{custom}Body\\end{custom}\n";
    db.apply_change(path, source, None);
    db.apply_change(other, "\\documentclass{article}\\section{Résumé}", None);
    fn kinds(items: &[DocumentSymbol]) -> Vec<SymbolKind> {
        items
            .iter()
            .flat_map(|item| {
                std::iter::once(item.kind)
                    .chain(kinds(item.children.as_deref().unwrap_or_default()))
            })
            .collect()
    }
    let defaults = presentation::OutlineOptions::default();
    let symbols = compute_symbols(&db, path, PositionEncoding::Utf16, &defaults);
    assert_eq!(
        kinds(&symbols)
            .iter()
            .filter(|kind| **kind == SymbolKind::Number)
            .count(),
        1
    );
    assert!(!kinds(&symbols).contains(&SymbolKind::EnumMember));
    let options = presentation::OutlineOptions {
        items: true,
        unlabelled_equations: true,
        environment_names: [("custom".into(), "Custom display".into())].into(),
        ..defaults
    };
    let symbols = compute_symbols(&db, path, PositionEncoding::Utf16, &options);
    assert_eq!(
        kinds(&symbols)
            .iter()
            .filter(|kind| **kind == SymbolKind::Number)
            .count(),
        2
    );
    assert_eq!(
        kinds(&symbols)
            .iter()
            .filter(|kind| **kind == SymbolKind::EnumMember)
            .count(),
        2
    );
    assert!(
        serde_json::to_string(&symbols)
            .unwrap()
            .contains("Custom display")
    );
    let search = |query| {
        serde_json::to_value(compute_projects_workspace_symbols(
            &[db.snapshot()],
            query,
            PositionEncoding::Utf16,
            &|_| options.clone(),
            Some(path),
        ))
        .unwrap()
    };
    let matches = search("RÉSUMÉ methods");
    assert!(matches.as_array().unwrap().len() > 1);
    assert_eq!(matches[0]["name"], "Résumé Methods");
    assert!(
        matches
            .as_array()
            .unwrap()
            .iter()
            .all(|symbol| symbol["location"]["uri"] == fixture_uri!("/outline/main.tex"))
    );
    assert_eq!(search("RÉSUMÉ nonexistent"), serde_json::json!([]));
    assert_eq!(search("résumé")[0]["name"], "Résumé");
    let folds = compute_folding(&db, path, PositionEncoding::Utf16, FileKind::Tex);
    assert!(
        folds
            .iter()
            .any(|fold| fold.kind == Some(lsp_types::FoldingRangeKind::Comment))
    );
    assert!(
        folds
            .iter()
            .any(|fold| fold.start_line == 5 && fold.end_line == 6)
    );
}

#[test]
fn hints_use_root_aux_range_toggles_and_unicode_truncation() {
    use tex_ls_analysis::external::*;
    let mut db = IncrementalDatabase::default();
    let path = Path::new(fixture_path!("/hints/main.tex"));
    let source = "😀 \\label{eq:a}\n\\ref{eq:a} \\ref{missing}\n";
    db.apply_change(path, source, None);
    let range = Range::new(Position::new(0, 0), Position::new(3, 0));
    assert_eq!(
        hints::compute(
            &db,
            path,
            range,
            PositionEncoding::Utf16,
            &Default::default()
        ),
        serde_json::json!([])
    );
    let token = db
        .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Compiler(vec![(
            path.with_extension("aux"),
            Observation::Present(CompilerArtifact::from_text(
                &path.with_extension("aux"),
                "\\newlabel{eq:a}{{αβγδ}{1}}\n\\newlabel{missing}{{8}{1}}",
                "build".into(),
            )),
        )]),
    )
    .unwrap();
    for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
        let options = presentation::HintOptions {
            definitions: false,
            max_length: 3,
            ..Default::default()
        };
        let hints = hints::compute(&db, path, range, enc, &options);
        assert_eq!(hints.as_array().unwrap().len(), 1);
        assert_eq!(hints[0]["label"], "αβ…");
        assert_eq!(hints[0]["position"]["line"], 1);
        assert!(hints[0]["tooltip"].as_str().unwrap().contains("Last build"));
        let limited = Range::new(Position::new(0, 0), Position::new(0, 1));
        assert_eq!(
            hints::compute(&db, path, limited, enc, &Default::default()),
            serde_json::json!([])
        );
    }
}
