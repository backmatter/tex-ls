//! Tests for incremental analysis.
use super::*;

#[test]
fn removal_releases_text_after_the_last_snapshot() {
    let mut db = Database::default();
    let text = "\\section{Removed}\n".into_source_text();
    let weak = Arc::downgrade(&text);
    let path = Path::new("/project/removed.tex");
    let file = db.upsert_file(path, text);
    db.parsed_tree(file);
    let read = db.clone();
    db.remove_file(path);
    assert!(db.lookup_file(path).is_none());
    assert!(read.lookup_file(path) == Some(file));
    assert_eq!(read.parsed_tree(file).to_string(), "\\section{Removed}\n");
    assert!(weak.strong_count() > 0);
    drop(read);
    assert_eq!(weak.strong_count(), 0);
}

#[test]
fn removal_preserves_surviving_sources_and_aliases() {
    let mut db = Database::default();
    db.upsert_file(Path::new("/project/a.tex"), "\\label{a}");
    db.upsert_file(Path::new("/project/b.tex"), "\\label{b}");
    db.set_bibliography_alias(Path::new("/project/refs.bib"), Path::new("/texmf/refs.bib"));
    db.remove_file(Path::new("/project/a.tex"));
    let file = db.lookup_file(Path::new("/project/b.tex")).unwrap();
    assert_eq!(db.parsed_tree(file).to_string(), "\\label{b}");
    assert_eq!(
        db.bibliography_alias_in(db.default_project, Path::new("/project/refs.bib")),
        Some(Path::new("/texmf/refs.bib"))
    );
}

#[test]
fn source_declarations_do_not_follow_query_order() {
    let mut db = Database::default();
    let a = Path::new("/a/main.tex");
    let b = Path::new("/b/main.tex");
    let declarations: crate::declarations::Declarations =
        toml::from_str("[environments.code]\nlike = 'lstlisting'\n").unwrap();
    let declared = declarations.resolve().unwrap();
    db.set_source_declarations(a, declared.clone());
    let text = "\\begin{code}$x\\end{code}";
    let fa = db.upsert_file(a, text);
    let fb = db.upsert_file(b, text);
    let expected = crate::parser::parse_with_declarations(text, LexConfig::default(), &declared);
    for _ in 0..3 {
        assert_eq!(db.parsed_tree(fa).green(), expected.syntax().green());
        assert_ne!(db.parsed_tree(fa).green(), db.parsed_tree(fb).green());
    }
}

fn lint_source(db: &mut Database, path: &Path, text: impl IntoSourceText) -> SourceInput {
    let file = db.upsert_file(path, text);
    db.reparse_stage_edits(file, None);
    file
}

#[test]
fn lint_queries_reuse_findings_and_follow_project_changes() {
    let mut db = Database::default();
    let main = PathBuf::from("/project/main.tex");
    let chapter = PathBuf::from("/project/chapter.tex");
    let bib = PathBuf::from("/project/refs.bib");
    let other = PathBuf::from("/project/other.tex");
    let file = lint_source(
        &mut db,
        &main,
        "\\documentclass{article}\n\\input{chapter}\n\\bibliography{refs}\n\\ref{target} \\cite{key}\n",
    );
    lint_source(&mut db, &chapter, "\\label{target}\n");
    let bib_file = lint_source(&mut db, &bib, "@article{key,title={Title}}\n");
    lint_source(&mut db, &other, "Unrelated prose.\n");
    db.clear_query_log();
    let initial = latex_lint_findings(&db, file).clone();
    assert_eq!(&initial, latex_lint_findings(&db, file));
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|q| q.kind == QueryKind::LatexLintFindings)
            .count(),
        1
    );
    lint_source(&mut db, &other, "Changed unrelated prose.\n");
    assert_eq!(&initial, latex_lint_findings(&db, file));
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|q| q.kind == QueryKind::LatexLintFindings)
            .count(),
        1
    );
    // Cross-file labels and citation keys invalidate the same cached findings.
    for (path, text) in [
        (&chapter, "\\label{different}\n"),
        (&bib, "@article{different,title={Title}}\n"),
        (
            &main,
            "\\documentclass{article}\n\\input{chapter}\n\\bibliography{refs}\n\\ref{different} \\cite{different}\n",
        ),
    ] {
        lint_source(&mut db, path, text);
        let actual = latex_lint_findings(&db, file);
        let expected = crate::linter::lint_document(
            &main,
            &parsed_tree_root(&db, file),
            semantic_model(&db, file),
            Some(resolved_labels(&db, db.project(db.default_project))),
            Some(resolved_citations(&db, db.project(db.default_project))),
            Some(resolved_package_options(
                &db,
                db.project(db.default_project),
            )),
        );
        assert_eq!(actual, &expected);
    }
    let findings = bib_lint_findings(&db, bib_file).clone();
    assert_eq!(&findings, bib_lint_findings(&db, bib_file));
    lint_source(&mut db, &bib, "@article{key,title={A},title={B}}\n");
    let expected = crate::bib::linter::lint_document(
        &bib,
        &parsed_bib_tree_root(&db, bib_file),
        bib_semantic_model(&db, bib_file),
    );
    assert_eq!(bib_lint_findings(&db, bib_file), &expected);
    assert_ne!(expected, findings);
    db.remove_file(&chapter);
    assert_eq!(
        latex_lint_findings(&db, file),
        &crate::linter::lint_document(
            &main,
            &parsed_tree_root(&db, file),
            semantic_model(&db, file),
            Some(resolved_labels(&db, db.project(db.default_project))),
            Some(resolved_citations(&db, db.project(db.default_project))),
            Some(resolved_package_options(
                &db,
                db.project(db.default_project)
            ))
        )
    );
}

#[test]
fn lint_query_observes_declared_alias_changes() {
    let mut db = Database::default();
    let path = PathBuf::from("/project/main.tex");
    let file = lint_source(
        &mut db,
        &path,
        "\\documentclass{article}\n\\myref{missing}\n",
    );
    let before = latex_lint_findings(&db, file).clone();
    let declarations =
        toml::from_str::<crate::declarations::Declarations>("[commands.myref]\nlike = 'ref'\n")
            .unwrap()
            .resolve()
            .unwrap();
    db.set_declarations(declarations);
    let after = latex_lint_findings(&db, file);
    assert_ne!(&before, after);
    assert!(after.iter().any(|d| d.rule == "undefined-ref"));
    db.set_declarations(ResolvedDeclarations::default());
    assert_eq!(&before, latex_lint_findings(&db, file));
}

#[test]
fn bib_cache_observes_patches_replacements_and_eviction() {
    let mut db = Database::default();
    let path = PathBuf::from("/project/references.bib");
    let mut text = "@article{probe,ti}\n".to_owned();
    let file = db.upsert_file(&path, text.as_str());
    db.reparse_stage_edits(file, None);
    db.parsed_bib_tree(file);
    for (insert, known, evict) in [
        ("author", true, false),
        ("title", false, false),
        ("year", true, true),
        ("😀", true, false),
    ] {
        let at = text.find(',').unwrap() + 1;
        let end = text.find('}').unwrap();
        let edit = Edit {
            range: at..end,
            insert: insert.into(),
        };
        text = edit.apply(&text);
        db.upsert_file(&path, text.as_str());
        db.reparse_stage_edits(file, known.then_some(vec![edit]));
        if evict {
            db.reparse_evict(file);
        }
        let actual = parsed_bib_document(&db, file);
        let expected = crate::bib::parse(&text);
        assert_eq!(actual.parsed.green, expected.green);
        assert_eq!(actual.parsed.errors, expected.errors);
        assert!(db.reparse_pending_edits(file).is_empty());
    }
}

#[test]
fn bib_cache_coalesces_sequential_edits_and_rejects_stale_chains() {
    let mut db = Database::default();
    let path = PathBuf::from("/project/references.bib");
    let original = "@article{probe,ti}\n";
    let file = db.upsert_file(&path, original);
    db.parsed_bib_tree(file);
    let at = original.find("ti}").unwrap();
    let edits = vec![
        Edit {
            range: at..at + 2,
            insert: "a".into(),
        },
        Edit {
            range: at + 1..at + 1,
            insert: "uthor".into(),
        },
    ];
    let text = crate::parser::apply_edits(original, &edits);
    db.upsert_file(&path, text.as_str());
    db.reparse_stage_edits(file, Some(edits));
    let actual = parsed_bib_document(&db, file);
    let full = crate::bib::parse(&text);
    assert_eq!(actual.parsed.green, full.green);
    assert_eq!(actual.parsed.errors, full.errors);
    assert!(db.reparse_pending_edits(file).is_empty());
    // A valid edit sequence for a different revision must not leak its tree.
    let stale = vec![Edit {
        range: at..at + 6,
        insert: "year".into(),
    }];
    let current = "@article{probe,title={😀}}\n";
    db.upsert_file(&path, current);
    db.reparse_stage_edits(file, Some(stale));
    let actual = parsed_bib_document(&db, file);
    let full = crate::bib::parse(current);
    assert_eq!(actual.parsed.green, full.green);
    assert_eq!(actual.parsed.errors, full.errors);
}

/// Build a base for `text` the way [`parsed_document`]'s full-parse branch does.
fn base_for(text: &str) -> PrevParse {
    let text = text.into_source_text();
    let declared = ResolvedDeclarations::default();
    let config = LexConfig::default();
    let (parse, ctx) = parse_with_declarations_resolved(&text, config, &declared);
    PrevParse {
        text,
        green: parse.green,
        errors: parse.errors,
        ctx,
        config,
        declared,
    }
}

/// The fast path's predicate. It has to be all three inputs, not just the text:
/// the same bytes parse differently under a different `meaning.toml` or a
/// different file flavor, so a text-only check would hand back a stale tree the
/// oracle never sees (this path does not splice, so nothing verifies it).
#[test]
fn a_base_is_current_only_for_the_inputs_it_was_parsed_under() {
    let base = base_for("\\section{Hi}\n");
    let declared = ResolvedDeclarations::default();
    let same = "\\section{Hi}\n".into_source_text();
    let other = "\\section{Ho}\n".into_source_text();

    // Equal but distinct allocations still count: a disk re-read is a fresh one.
    assert!(base.is_current(&same, LexConfig::default(), &declared));
    assert!(base.is_current(&base.text.clone(), LexConfig::default(), &declared));

    assert!(!base.is_current(&other, LexConfig::default(), &declared));
    assert!(
        !base.is_current(
            &same,
            LexConfig {
                flavor: crate::parser::LatexFlavor::Package,
                dtx: false,
            },
            &declared,
        ),
        "a `.sty` reads `@` as a letter, so the same bytes are a different parse"
    );

    // Only the parse-facing half is ever compared, so declaring a command
    // leaves the base usable. A base rejected here costs a full reparse on the
    // next keystroke, for an edit that cannot have changed the tree.
    let commands_only =
        toml::from_str::<crate::declarations::Declarations>("[commands.myref]\nlike = 'cref'\n")
            .expect("declarations deserialize")
            .resolve()
            .expect("declarations resolve");
    assert!(base.is_current(&same, LexConfig::default(), &commands_only.parse_tier()));
}

/// Reparse state is retained until its source is removed.
#[test]
fn reparse_bases_follow_source_lifetime() {
    let mut db = Database::default();
    let paths: Vec<_> = (0..200)
        .map(|n| PathBuf::from(format!("file{n}.tex")))
        .collect();
    for path in &paths {
        let file = db.upsert_file(path, "text\n".to_owned());
        db.reparse_store(
            file,
            Arc::new(base_for("text\n")),
            0,
            db.reparse_state(file).generation,
        );
    }
    assert_eq!(db.reparse_cache_len(), paths.len());
    for path in &paths {
        db.remove_file(path);
    }
    assert_eq!(db.reparse_cache_len(), 0);
}

/// A store from before another stage cannot replace its base or compacted hint.
#[test]
fn a_superseded_store_preserves_the_newer_transform() {
    let mut db = Database::default();
    let file = db.upsert_file(Path::new("a.tex"), "x\n".to_owned());
    db.parsed_tree(file);

    db.reparse_stage_edits(
        file,
        Some(vec![Edit {
            range: 0..0,
            insert: "a".to_string(),
        }]),
    );
    let peeked = db.reparse_pending_edits(file).len();
    let generation = db.reparse_state(file).generation;
    // The race: another stage arrives before the store.
    db.reparse_stage_edits(
        file,
        Some(vec![Edit {
            range: 0..0,
            insert: "b".to_string(),
        }]),
    );

    db.reparse_store(file, Arc::new(base_for("ax\n")), peeked, generation);
    let left = db.reparse_pending_edits(file);
    assert_eq!(left.len(), 1, "the late stage must survive");
    assert_eq!(left[0].insert, "ba");
    assert_eq!(&**db.reparse_prev(file).unwrap().text, "x\n");
}
