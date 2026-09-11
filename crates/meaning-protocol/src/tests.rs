use super::*;
use crate::test_host::TestHost;
use meaning_analysis::linter::lint_document;
#[test]
fn refactoring_uses_snapshot_syntax_without_running_lint() {
    let mut db = IncrementalDatabase::default();
    let path = PathBuf::from("/project/main.tex");
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
                .any(|q| q.kind == meaning_analysis::incremental::QueryKind::LatexLintFindings)
        );
    }
}
#[test]
fn diagnostics_and_code_actions_share_raw_findings() {
    let mut db = IncrementalDatabase::default();
    let path = PathBuf::from("/project/main.tex");
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
            .filter(|q| q.kind == meaning_analysis::incremental::QueryKind::LatexLintFindings)
            .count(),
        1
    );
}
#[test]
fn diagnostic_resolution_is_demanded_only_for_local_sites() {
    let mut db = IncrementalDatabase::default();
    let main = PathBuf::from("/project/main.tex");
    let file = db.apply_change(&main, "\\documentclass{article}\nOrdinary prose.\n", None);
    let other = db.apply_change(
        &PathBuf::from("/project/other.tex"),
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
            meaning_analysis::incremental::QueryKind::ResolvedLabels
                | meaning_analysis::incremental::QueryKind::ResolvedCitations
        )));
        assert!(!log.iter().any(|q| q.file == Some(other)
            && q.kind == meaning_analysis::incremental::QueryKind::ParsedDocument));
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
            .any(|q| q.kind == meaning_analysis::incremental::QueryKind::ResolvedLabels)
    );
    assert!(
        log.iter()
            .any(|q| q.kind == meaning_analysis::incremental::QueryKind::ResolvedCitations)
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
fn lint_to_lsp_links_documented_rules_only() {
    let d = || meaning_analysis::linter::Diagnostic {
        rule: "deprecated-command",
        severity: meaning_analysis::linter::Severity::Warning,
        path: PathBuf::from("x.tex"),
        start: 0,
        end: 3,
        message: "use bfseries".to_owned(),
        fix: None,
        related: Vec::new(),
    };
    let idx = LineIndex::with_encoding("\\bf x", PositionEncoding::Utf16);

    // LaTeX arm (link_docs = true): the code deep-links the rule's reference
    // anchor, and the tag still rides along.
    let latex = lint_to_lsp(&idx, d(), true, Path::new("x.tex"));
    assert_eq!(
        latex.code_description.map(|c| c.href.to_string()),
        Some(
            "https://backmatter.github.io/meaning/reference/linter-rules.html#deprecated-command"
                .to_owned()
        )
    );
    assert_eq!(latex.tags, Some(vec![DiagnosticTag::Deprecated]));

    // Bib arm (link_docs = false): the rule id is still the `code`, but with
    // no doc link (bib rules aren't catalogued yet).
    let bib = lint_to_lsp(&idx, d(), false, Path::new("x.tex"));
    assert!(bib.code_description.is_none());
    assert_eq!(
        bib.code,
        Some(Code::String("deprecated-command".to_owned()))
    );
}
#[test]
fn lint_to_lsp_builds_related_information() {
    // A same-file secondary resolves its range against the current index; a
    // cross-file one is a file-level `0..0` link to the other document.
    let text = "\\label{a}\\label{a}\n";
    let idx = LineIndex::with_encoding(text, PositionEncoding::Utf16);
    let d = meaning_analysis::linter::Diagnostic {
        rule: "duplicate-label",
        severity: meaning_analysis::linter::Severity::Warning,
        path: PathBuf::from("/p/main.tex"),
        start: 9,
        end: 18,
        message: "label `a` is defined more than once".to_owned(),
        fix: None,
        related: vec![
            meaning_analysis::linter::RelatedInfo {
                path: PathBuf::from("/p/main.tex"),
                start: 7,
                end: 8,
                message: "first definition of `a`".to_owned(),
            },
            meaning_analysis::linter::RelatedInfo {
                path: PathBuf::from("/p/other.tex"),
                start: 0,
                end: 0,
                message: "other definition of `a`".to_owned(),
            },
        ],
    };
    let lsp = lint_to_lsp(&idx, d, true, Path::new("/p/main.tex"));
    let related = lsp.related_information.expect("related present");
    assert_eq!(related.len(), 2);

    // Same-file: real range (line 0, cols 7..8), self URI.
    assert_eq!(related[0].message, "first definition of `a`");
    assert_eq!(related[0].location.uri, uri("file:///p/main.tex"));
    assert_eq!(related[0].location.range.start, Position::new(0, 7));
    assert_eq!(related[0].location.range.end, Position::new(0, 8));

    // Cross-file: file-level `0..0` at the other document's start.
    assert_eq!(related[1].message, "other definition of `a`");
    assert_eq!(related[1].location.uri, uri("file:///p/other.tex"));
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
            meaning_parser::parser::try_apply_edits(old, edits).as_deref(),
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
/// CRLF is the hazard `TODO.md` records panache losing its whole feature to:
/// nothing here may assume a one-byte line terminator. The `\r` is a line's
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
/// moving. `TODO.md` records panache losing this whole feature on
/// Windows-authored files, so it is worth its own case rather than trusting
/// the unit oracle.
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
fn reference_under_cursor_splits_cref_list() {
    let text = "\\cref{a,b,c}\n";
    let model = SemanticModel::build(&SyntaxNode::new_root(parse(text).green));
    // The whole command shares one range, so every key is returned (per-key
    // sub-ranges are deferred).
    let at = offset_of(text, "\\cref") + 2;
    match reference_under_cursor(&model, at) {
        Some(CursorTarget::Labels(names)) => assert_eq!(
            names,
            vec![SmolStr::new("a"), SmolStr::new("b"), SmolStr::new("c")]
        ),
        other => panic!("expected a label target, got {other:?}"),
    }
}
#[test]
fn path_to_uri_round_trips_through_uri_to_fs_path() {
    let p = PathBuf::from("/tmp/my dir/main.tex");
    let u = path_to_uri(&p).expect("a file path forms a URI");
    // The space is percent-encoded in the URI text…
    assert!(u.as_str().contains("%20"), "got {}", u.as_str());
    // …and decodes back to the original filesystem path.
    assert_eq!(uri_to_fs_path(&u), Some(p));
}
#[test]
fn package_completion_surfaces_installed_set_and_ctan_detail() {
    use meaning_analysis::completion::FileArgKind;
    use meaning_parser::semantic::signature::SignatureDb;

    // A tree with an installed package that is *not* in the baked CTAN list.
    let tree = tempfile::tempdir().unwrap();
    let sty = tree.path().join("tex/latex/zzlocalpkg/zzlocalpkg.sty");
    std::fs::create_dir_all(sty.parent().unwrap()).unwrap();
    std::fs::write(&sty, "").unwrap();
    let texmf = TestHost::from_roots(&[tree.path().to_path_buf()]).index;

    let sigs = SignatureDb::default();
    let root = SyntaxNode::new_root(parse("").green);
    let model = SemanticModel::build(&root);
    let doc = uri("file:///proj/main.tex");

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

use meaning_analysis::incremental::IncrementalDatabase;
