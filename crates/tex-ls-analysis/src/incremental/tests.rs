//! Tests for incremental analysis.
use super::*;

#[test]
fn ordered_alias_lookup_preserves_resolution_and_invalidates_removed_aliases() {
    use crate::external::{
        ExternalInputKind, ExternalInputs, FileCandidates, FileInputs, Observation,
    };
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let main = Path::new("/p/main.tex");
    let target = Path::new("/p/actual.tex");
    let file = db.apply_change(
        main,
        "\\documentclass{article}\\input{alias0000}\\input{alias0500}\\input{alias0999}",
        None,
    );
    db.apply_change(target, "\\newcommand{\\aliasedcommand}{Printed}", None);
    let paths: Vec<_> = (0..1_000)
        .map(|index| PathBuf::from(format!("/p/alias{index:04}.tex")))
        .collect();
    let token = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Files(FileInputs {
            file_resolution: paths
                .iter()
                .rev()
                .map(|path| (path.clone(), Observation::Present(target.into())))
                .collect(),
            ..Default::default()
        }),
    )
    .unwrap();
    {
        let snapshot = db.snapshot();
        for path in &paths {
            assert_eq!(snapshot.file_alias(path), Some(target));
            assert_eq!(
                snapshot
                    .resolve_file(&FileCandidates {
                        local: vec![path.clone()],
                        installed: None
                    })
                    .target,
                Observation::Present(path.clone())
            );
        }
        for missing in ["/p/a.tex", "/p/alias0500a.tex", "/p/z.tex"] {
            assert!(
                snapshot
                    .resolve_file(&FileCandidates {
                        local: vec![missing.into()],
                        installed: None
                    })
                    .target
                    == Observation::Unknown
            );
        }
        assert!(
            snapshot
                .editor_symbols(file)
                .commands
                .contains("aliasedcommand")
        );
        assert!(
            snapshot
                .command_source_files(main)
                .iter()
                .any(|path| path == target)
        );
    }
    let token = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Files(FileInputs {
            file_resolution: paths
                .iter()
                .map(|path| (path.clone(), Observation::Absent))
                .collect(),
            ..Default::default()
        }),
    )
    .unwrap();
    let snapshot = db.snapshot();
    assert!(snapshot.file_alias(&paths[500]).is_none());
    assert_eq!(
        snapshot
            .resolve_file(&FileCandidates {
                local: vec![paths[500].clone()],
                installed: None
            })
            .target,
        Observation::Unknown
    );
    assert!(
        !snapshot
            .editor_symbols(file)
            .commands
            .contains("aliasedcommand")
    );
    assert!(
        !snapshot
            .command_source_files(main)
            .iter()
            .any(|path| path == target)
    );
}

#[test]
fn acquisition_batches_preserve_shared_overlays_removals_aliases_and_last_values() {
    use crate::external::{
        ExternalInputKind, ExternalInputs, FileInputs, LocationKind, LocationObservation,
        Observation,
    };
    let mut db = IncrementalDatabase::default();
    let first = db.project_id();
    let second = db.create_project(ResolvedDeclarations::default());
    let shared = Path::new("/p/shared.tex");
    let removed = Path::new("/p/removed.tex");
    db.set_backing(first, shared, Some("Old backing".into_source_text()))
        .unwrap();
    db.set_backing(second, shared, Some("Old backing".into_source_text()))
        .unwrap();
    db.set_backing(first, removed, Some("Removed backing".into_source_text()))
        .unwrap();
    db.open_overlay(second, shared, "Unsaved overlay").unwrap();
    let token = db
        .begin_external_refresh(first, ExternalInputKind::Files)
        .unwrap();
    db.apply_external_inputs(
        token,
        ExternalInputs::Files(FileInputs {
            backing: vec![
                (shared.into(), Some("New backing".into_source_text())),
                (removed.into(), None),
                ("/p/new.tex".into(), Some("First value".into_source_text())),
                ("/p/./new.tex".into(), Some("Last value".into_source_text())),
            ],
            locations: vec![(
                "/p/./new.tex".into(),
                LocationObservation {
                    kind: Observation::Present(LocationKind::File),
                    ..Default::default()
                },
            )],
            file_resolution: vec![(
                "/p/alias.tex".into(),
                Observation::Present("/p/./new.tex".into()),
            )],
        }),
    )
    .unwrap();
    let snapshot = db.snapshot_for(first).unwrap();
    assert_eq!(
        snapshot
            .project_members()
            .iter()
            .map(|member| member.path.as_path())
            .collect::<Vec<_>>(),
        [Path::new("/p/new.tex"), shared]
    );
    assert!(snapshot.lookup_file(removed).is_none());
    assert_eq!(
        snapshot.file_text(snapshot.lookup_file(shared).unwrap()),
        "Unsaved overlay"
    );
    assert_eq!(
        snapshot.file_text(snapshot.lookup_file(Path::new("/p/new.tex")).unwrap()),
        "Last value"
    );
    assert_eq!(
        snapshot.file_alias(Path::new("/p/alias.tex")),
        Some(Path::new("/p/new.tex"))
    );
    assert_eq!(
        snapshot.location_observation(Path::new("/p/new.tex")).kind,
        Observation::Present(LocationKind::File)
    );
    drop(snapshot);
    db.close_overlay(second, shared).unwrap();
    for project in [first, second] {
        let snapshot = db.snapshot_for(project).unwrap();
        assert_eq!(
            snapshot.file_text(snapshot.lookup_file(shared).unwrap()),
            "New backing"
        );
    }
    // An addition without a storage rebuild must publish membership too.
    db.set_backing_batch(
        first,
        [("/p/extra.tex".into(), Some("Extra".into_source_text()))],
    )
    .unwrap();
    let snapshot = db.snapshot_for(first).unwrap();
    assert_eq!(snapshot.project_members().len(), 3);
    assert!(snapshot.lookup_file(Path::new("/p/extra.tex")).is_some());
}

#[test]
fn graph_filters_and_root_graphics_reuse_cached_ownership_after_edits() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new("/project/main.tex");
    let source = r"\includeonly{a}\meaning\includeonly{b}\graphicspath{{images/}}\meaning\graphicspath{{fake/}}\meaning\input{printed}\include{a}\include{b}";
    let file = db.apply_change(path, source, None);
    db.apply_change(
        Path::new("/project/a.tex"),
        r"A\includegraphics{plot}",
        None,
    );
    db.apply_change(
        Path::new("/project/b.tex"),
        r"B\includegraphics{plot}",
        None,
    );
    let check = |db: &IncrementalDatabase, directory: &str, included: &str| {
        let snapshot = db.snapshot();
        snapshot.file_modes(file);
        let view = snapshot.project_graph().interpret(path);
        for name in ["a", "b"] {
            let target = PathBuf::from(format!("/project/{name}.tex"));
            assert_eq!(
                view.edges
                    .iter()
                    .find(|edge| edge.target.as_ref() == Some(&target))
                    .unwrap()
                    .build_participation,
                Some(name == included)
            );
        }
        let child = snapshot
            .lookup_file(Path::new(&format!("/project/{included}.tex")))
            .unwrap();
        let references = snapshot.file_references(child);
        assert_eq!(references.len(), 1);
        assert!(
            references[0]
                .candidates
                .local
                .contains(&PathBuf::from(format!("/project/{directory}/plot.pdf")))
        );
    };
    db.clear_query_log();
    check(&db, "images", "a");
    for kind in [QueryKind::DefinitionSites, QueryKind::FileOperands] {
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == kind && entry.file == Some(file))
                .count(),
            1
        );
    }
    db.clear_query_log();
    check(&db, "images", "a");
    assert!(db.query_log().is_empty());
    db.apply_change(
        path,
        format!(r"\renewcommand{{\meaning}}[1]{{#1}}{source}"),
        None,
    );
    check(&db, "fake", "b");
    for kind in [QueryKind::DefinitionSites, QueryKind::FileOperands] {
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == kind && entry.file == Some(file))
                .count(),
            1
        );
    }
}

#[test]
fn inherited_graphics_are_shared_and_prose_edits_reuse_child_contexts() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new("/project/main.tex");
    let mut source = "\\documentclass{article}\n\\graphicspath{{images/}}\n".to_string();
    let mut children = Vec::new();
    for index in 0..32 {
        source.push_str(&format!("\\input{{chapter{index}}}\n"));
        children.push(db.apply_change(
            &PathBuf::from(format!("/project/chapter{index}.tex")),
            "\\includegraphics{figure.png}\n",
            None,
        ));
    }
    db.apply_change(path, source.as_str(), None);
    db.clear_query_log();
    let check = |db: &IncrementalDatabase, directory: &str| {
        let snapshot = db.snapshot();
        for &child in &children {
            let references = snapshot.file_references(child);
            assert_eq!(references.len(), 1);
            assert!(
                references[0]
                    .candidates
                    .local
                    .contains(&PathBuf::from(format!("/project/{directory}/figure.png")))
            );
        }
        assert_eq!(
            snapshot
                .query_log()
                .iter()
                .filter(|entry| entry.kind == QueryKind::RootGraphics)
                .count(),
            1
        );
    };
    check(&db, "images");
    db.clear_query_log();
    db.apply_change(path, format!("Prose.\n{source}"), None);
    check(&db, "images");
    assert!(
        db.snapshot()
            .query_log()
            .iter()
            .all(|entry| entry.kind != QueryKind::FileReferences)
    );
    db.clear_query_log();
    db.apply_change(path, source.replace("images/", "changed/"), None);
    check(&db, "changed");
    assert_eq!(
        db.snapshot()
            .query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileReferences)
            .count(),
        children.len()
    );
}

#[test]
fn source_modes_are_reused_and_invalidate_when_source_changes_the_domain() {
    use tex_ls_parser::semantic::mode::Mode;
    let mut db = IncrementalDatabase::default();
    let path = Path::new("/project/modes.tex");
    let source = r"$x + \text{printed}$";
    let file = db.apply_change(path, source, None);
    let at = source.find("printed").unwrap();
    assert_eq!(db.snapshot().file_modes(file).mode_at(at), Mode::Text);
    db.clear_query_log();
    for _ in 0..3 {
        assert_eq!(db.snapshot().file_modes(file).mode_at(at), Mode::Text);
    }
    assert!(
        db.snapshot()
            .query_log()
            .iter()
            .all(|entry| entry.kind != QueryKind::FileModes)
    );
    let changed = r"\renewcommand{\text}[1]{#1}$x + \text{printed}$";
    db.apply_change(path, changed, None);
    assert_eq!(
        db.snapshot()
            .file_modes(file)
            .mode_at(changed.find("printed").unwrap()),
        Mode::Unknown
    );
    assert_eq!(
        db.snapshot()
            .query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileModes)
            .count(),
        1
    );
}

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
    db.set_file_alias(Path::new("/project/refs.bib"), Path::new("/texmf/refs.bib"));
    db.remove_file(Path::new("/project/a.tex"));
    let file = db.lookup_file(Path::new("/project/b.tex")).unwrap();
    assert_eq!(db.parsed_tree(file).to_string(), "\\label{b}");
    assert_eq!(
        db.file_alias_in(db.default_project, Path::new("/project/refs.bib")),
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
fn unknown_commands_use_package_loads_from_the_selected_root() {
    let mut db = Database::default();
    let first = lint_source(
        &mut db,
        Path::new("/project/one.tex"),
        "\\documentclass{article}\n\\input{preamble}\n\\input{shared}\n\\ce{H2O}\n",
    );
    let second = lint_source(
        &mut db,
        Path::new("/project/two.tex"),
        "\\documentclass{article}\n\\input{shared}\n\\ce{H2O}\n",
    );
    lint_source(
        &mut db,
        Path::new("/project/preamble.tex"),
        "\\usepackage{mhchem}\n",
    );
    lint_source(&mut db, Path::new("/project/shared.tex"), "Text\n");
    let unknown = |file| {
        latex_lint_findings(&db, file)
            .iter()
            .filter(|finding| finding.rule == "unknown-command")
            .count()
    };
    assert_eq!(unknown(first), 0);
    assert_eq!(unknown(second), 1);
}

#[test]
fn unknown_commands_follow_transitive_local_package_loads() {
    let mut db = Database::default();
    let document = lint_source(
        &mut db,
        Path::new("/project/main.tex"),
        "\\documentclass{article}\n\\usepackage{local}\n\\SI{1}{m}\n",
    );
    lint_source(
        &mut db,
        Path::new("/project/local.sty"),
        "\\RequirePackage{siunitx}\n",
    );
    assert!(
        latex_lint_findings(&db, document)
            .iter()
            .all(|finding| finding.rule != "unknown-command")
    );
    lint_source(&mut db, Path::new("/project/local.sty"), "% no package\n");
    assert!(
        latex_lint_findings(&db, document)
            .iter()
            .any(|finding| finding.rule == "unknown-command")
    );
}

#[test]
fn editor_symbols_follow_shared_sources_in_the_selected_root() {
    let mut db = Database::default();
    let first = lint_source(
        &mut db,
        Path::new("/project/one/main.tex"),
        "\\documentclass{article}\n\\input{../shared}\n\\fromleaf\n",
    );
    let second = lint_source(
        &mut db,
        Path::new("/project/two/main.tex"),
        "\\documentclass{article}\n\\input{../shared}\n\\fromleaf\n",
    );
    lint_source(&mut db, Path::new("/project/shared.tex"), "\\input{leaf}\n");
    for (path, source) in [
        ("/project/one/leaf.tex", "\\newcommand{\\fromleaf}[1]{#1}\n"),
        (
            "/project/two/leaf.tex",
            "\\newcommand{\\fromleaf}[2]{#1#2}\n",
        ),
    ] {
        lint_source(&mut db, Path::new(path), source);
    }
    for (file, arity) in [(first, 1), (second, 2)] {
        assert_eq!(
            editor_symbols(&db, file)
                .signatures
                .command("fromleaf")
                .map(|sig| sig.args.len()),
            Some(arity)
        );
        assert!(
            latex_lint_findings(&db, file)
                .iter()
                .all(|finding| finding.rule != "unknown-command")
        );
    }
}

#[test]
fn displayed_files_do_not_define_document_commands() {
    let mut db = Database::default();
    let document = lint_source(
        &mut db,
        Path::new("/project/main.tex"),
        "\\lstinputlisting{listing.tex}\n\\includegraphics{picture.tex}\n\\bibliographystyle{plain}\n\\input{executed}\n\\fromlisting \\frompicture \\frombibstyle \\frominput\n",
    );
    for (name, command) in [
        ("listing.tex", "fromlisting"),
        ("picture.tex", "frompicture"),
        ("plain.bst", "frombibstyle"),
        ("executed.tex", "frominput"),
    ] {
        lint_source(
            &mut db,
            &Path::new("/project").join(name),
            format!("\\newcommand{{\\{command}}}{{}}\n"),
        );
    }
    let symbols = editor_symbols(&db, document);
    assert!(!symbols.commands.contains("fromlisting"));
    assert!(!symbols.commands.contains("frompicture"));
    assert!(!symbols.commands.contains("frombibstyle"));
    assert!(symbols.commands.contains("frominput"));
    let unknown: Vec<_> = latex_lint_findings(&db, document)
        .iter()
        .filter(|finding| finding.rule == "unknown-command")
        .map(|finding| finding.message.as_str())
        .collect();
    assert!(
        unknown
            .iter()
            .any(|message| message.contains("fromlisting"))
    );
    assert!(
        unknown
            .iter()
            .any(|message| message.contains("frompicture"))
    );
    assert!(
        unknown
            .iter()
            .any(|message| message.contains("frombibstyle"))
    );
    assert!(!unknown.iter().any(|message| message.contains("frominput")));
}

#[test]
fn acquisition_observes_all_shared_source_contexts_without_selecting_a_link() {
    let mut db = IncrementalDatabase::default();
    for (path, source) in [
        (
            "/project/one/main.tex",
            "\\documentclass{article}\n\\input{../shared}\n",
        ),
        (
            "/project/two/main.tex",
            "\\documentclass{article}\n\\input{../shared}\n",
        ),
        ("/project/shared.tex", "\\input{leaf}\n"),
    ] {
        db.apply_change(Path::new(path), source, None);
    }
    let shared = Path::new("/project/shared.tex");
    {
        let snapshot = db.snapshot();
        let file = snapshot.lookup_file(shared).unwrap();
        assert!(snapshot.file_references(file).is_empty());
        let needs = snapshot.file_discovery_needs(shared);
        for path in ["/project/one/leaf.tex", "/project/two/leaf.tex"] {
            assert!(needs.contains(&crate::external::InputNeed::Location(path.into())));
        }
    }
    db.apply_change(shared, "\\input{replacement}\n", None);
    let snapshot = db.snapshot();
    let needs = snapshot.file_discovery_needs(shared);
    for path in [
        "/project/one/replacement.tex",
        "/project/two/replacement.tex",
    ] {
        assert!(needs.contains(&crate::external::InputNeed::Location(path.into())));
    }
    assert!(!needs.contains(&crate::external::InputNeed::Location(
        "/project/one/leaf.tex".into()
    )));
}

#[test]
fn prose_edits_reuse_editor_symbols_but_declaration_order_stays_current() {
    let mut db = Database::default();
    let path = Path::new("/project/main.tex");
    let source = "\\documentclass{article}\n\\newcommand{\\original}[1]{#1}\n\\input{copy}\n\\renewcommand{\\original}[2]{#1#2}\n";
    let file = lint_source(&mut db, path, source);
    lint_source(
        &mut db,
        Path::new("/project/copy.tex"),
        "\\NewCommandCopy{\\copied}{\\original}\n",
    );
    lint_source(
        &mut db,
        Path::new("/project/article.cls"),
        "\\newcommand{\\fromclass}[1]{#1}\n",
    );
    assert!(editor_symbols(&db, file).commands.contains("fromclass"));
    assert_eq!(
        editor_symbols(&db, file)
            .signatures
            .command("copied")
            .unwrap()
            .args
            .len(),
        1
    );
    db.clear_query_log();
    lint_source(&mut db, path, format!("Some prose.\n{source}"));
    assert_eq!(
        editor_symbols(&db, file)
            .signatures
            .command("copied")
            .unwrap()
            .args
            .len(),
        1
    );
    assert!(
        !db.query_log()
            .iter()
            .any(|entry| entry.kind == QueryKind::EditorSymbols)
    );
    assert!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::EditorEvents)
            .all(|entry| entry.file == Some(*file.identity(&db)))
    );

    // Moving the include across a declaration must invalidate the ordered facts.
    let reordered = "\\documentclass{article}\n\\newcommand{\\original}[1]{#1}\n\\renewcommand{\\original}[2]{#1#2}\n\\input{copy}\n";
    lint_source(&mut db, path, reordered);
    assert_eq!(
        editor_symbols(&db, file)
            .signatures
            .command("copied")
            .unwrap()
            .args
            .len(),
        2
    );
    let mut fresh = Database::default();
    let fresh_file = lint_source(&mut fresh, path, reordered);
    lint_source(
        &mut fresh,
        Path::new("/project/copy.tex"),
        "\\NewCommandCopy{\\copied}{\\original}\n",
    );
    lint_source(
        &mut fresh,
        Path::new("/project/article.cls"),
        "\\newcommand{\\fromclass}[1]{#1}\n",
    );
    assert_eq!(
        editor_symbols(&db, file),
        editor_symbols(&fresh, fresh_file)
    );
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
    // This complete root now cites `different`; `key` is unused.
    let project = crate::bib::linter::project::ProjectFacts {
        complete: true,
        used: ["different".to_owned()].into_iter().collect(),
        ..Default::default()
    };
    let expected = crate::bib::linter::lint_document_with_project(
        &bib,
        &parsed_bib_tree_root(&db, bib_file),
        bib_semantic_model(&db, bib_file),
        Some(&project),
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
        ctx: Arc::new(ctx),
        config,
        declared: Arc::new(declared),
    }
}

/// The fast path's predicate. It has to be all three inputs, not just the text:
/// the same bytes parse differently under a different `tex-ls.toml` or a
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

#[test]
fn successful_token_reparses_share_immutable_context_and_declarations() {
    let environments: serde_json::Map<String, serde_json::Value> = (0..2048)
        .map(|index| {
            (
                format!("declared{index}"),
                serde_json::json!({"like": "quote"}),
            )
        })
        .collect();
    let declared = serde_json::from_value::<crate::declarations::Declarations>(
        serde_json::json!({"environments": environments}),
    )
    .unwrap()
    .resolve()
    .unwrap()
    .parse_tier();
    let mut db = Database::default();
    db.set_declarations(declared.clone());
    let path = Path::new("/project/shared-context.tex");
    let mut text = "alphabet beta\n".to_owned();
    let file = db.upsert_file(path, text.as_str());
    let initial = parsed_document(&db, file).clone();
    let initial_green = initial.green.clone();
    let initial_errors = initial.errors.clone();
    let mut previous = initial.clone();
    for insert in ["X", "Y", "Z"] {
        let edit = Edit {
            range: 2..2,
            insert: insert.into(),
        };
        let changed = edit.apply(&text);
        let previous_base = previous.as_reparse_base(&previous.declared);
        let proved = crate::parser::reparse(&previous_base, &edit, &changed)
            .expect("this known ASCII word edit must splice");
        assert_eq!(proved.tier, crate::parser::ReparseTier::Token);
        let (full, full_ctx) =
            parse_with_declarations_resolved(&changed, LexConfig::default(), &declared);
        assert_eq!(proved.green, full.green);
        assert_eq!(proved.errors, full.errors);
        db.upsert_file(path, changed.as_str());
        db.reparse_stage_edits(file, Some(vec![edit]));
        let actual = parsed_document(&db, file).clone();
        assert_eq!(actual.green, full.green);
        assert_eq!(actual.errors, full.errors);
        let initial_base = initial.as_reparse_base(&initial.declared);
        let actual_base = actual.as_reparse_base(&actual.declared);
        assert_eq!(actual_base.ctx, &full_ctx);
        assert_eq!(actual_base.declared, &declared);
        assert!(
            std::ptr::eq(initial_base.ctx, actual_base.ctx),
            "a successful splice must share the immutable full-file context"
        );
        assert!(
            std::ptr::eq(initial_base.declared, actual_base.declared),
            "a successful splice must share its unchanged declarations"
        );
        assert_eq!(initial.green, initial_green);
        assert_eq!(initial.errors, initial_errors);
        assert_eq!(initial_base.text, "alphabet beta\n");
        assert_eq!(
            initial_base.declared.as_db().environment_names().count(),
            2048
        );
        text = changed;
        previous = actual;
    }
}

#[test]
fn parsed_document_retains_rebased_reader_context_across_staged_edits() {
    let mut db = Database::default();
    let path = Path::new("/project/rebased-readers.tex");
    let mut text = "alphabet beta\n\\renewcommand{\\meaning}[1]{#1}\nAfterword.\n".to_owned();
    let declared = ResolvedDeclarations::default();
    let file = db.upsert_file(path, text.as_str());
    let initial = parsed_document(&db, file).clone();
    let retained = initial.ctx.clone();
    let mut previous = initial.clone();
    for (at, remove, insert) in [(2, 0, "XY"), (2, 1, ""), (2, 1, "Q")] {
        let edit = Edit {
            range: at..at + remove,
            insert: insert.into(),
        };
        let changed = edit.apply(&text);
        let proved = crate::parser::reparse(&previous.as_reparse_base(&declared), &edit, &changed)
            .expect("known word with rebased later reader must splice");
        assert_eq!(proved.tier, crate::parser::ReparseTier::Token);
        let (full, full_ctx) =
            parse_with_declarations_resolved(&changed, LexConfig::default(), &declared);
        assert!(full.errors.is_empty());
        assert_eq!(full.syntax().to_string(), changed);
        assert_eq!(proved.green, full.green);
        assert_eq!(proved.errors, full.errors);
        assert_eq!(proved.context(&previous.ctx), &full_ctx);
        db.upsert_file(path, changed.as_str());
        db.reparse_stage_edits(file, Some(vec![edit]));
        let actual = parsed_document(&db, file).clone();
        assert_eq!(actual.green, full.green);
        assert_eq!(actual.errors, full.errors);
        assert_eq!(actual.ctx.as_ref(), &full_ctx);
        assert!(Arc::ptr_eq(&initial.declared, &actual.declared));
        let (_, original_ctx) = parse_with_declarations_resolved(
            "alphabet beta\n\\renewcommand{\\meaning}[1]{#1}\nAfterword.\n",
            LexConfig::default(),
            &declared,
        );
        assert_eq!(retained.as_ref(), &original_ctx);
        previous = actual;
        text = changed;
    }
}

#[test]
fn staged_prose_edit_does_not_reuse_a_previous_declaration_context() {
    let mut db = Database::default();
    let path = Path::new("/project/main.tex");
    let original = "Before.\n\\begin{custom}\nx_i\n\\end{custom}\n";
    let file = db.upsert_file(path, original);
    db.parsed_tree(file);
    let edit = Edit {
        range: 1..2,
        insert: "a".into(),
    };
    let changed = edit.apply(original);
    db.upsert_file(path, changed.as_str());
    db.reparse_stage_edits(file, Some(vec![edit]));
    let declared = toml::from_str::<crate::declarations::Declarations>(
        "[environments.custom]\nlike = 'equation'\n",
    )
    .unwrap()
    .resolve()
    .unwrap();
    db.set_declarations(declared.clone());
    let actual = parsed_document(&db, file);
    let expected =
        parse_with_declarations_resolved(&changed, LexConfig::default(), &declared.parse_tier()).0;
    assert_eq!(actual.green, expected.green);
    assert_eq!(actual.errors, expected.errors);
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

#[test]
fn feature_extractions_share_current_ranges_and_preserve_stable_project_facts() {
    let path = Path::new("/project/main.tex");
    let original = "\\documentclass{article}\n\\newcommand{\\foo}{x}\n\\section{Intro}\\label{intro}\\ref{intro}\\foo\n";
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(path, original);
    db.clear_query_log();
    for _ in 0..3 {
        let snapshot = db.snapshot();
        assert_eq!(snapshot.definition_sites(file).len(), 1);
        assert_eq!(snapshot.name_occurrences(file).commands("foo").len(), 2);
        assert!(!snapshot.file_outline(file).is_empty());
        snapshot.resolve_project();
    }
    for kind in [
        QueryKind::DefinitionSites,
        QueryKind::FileOperands,
        QueryKind::NameOccurrences,
        QueryKind::Outline,
        QueryKind::IncludeComponents,
    ] {
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == kind)
                .count(),
            1,
            "query should be shared"
        );
    }
    let shifted = format!("A paragraph.\n\n{original}");
    db.clear_query_log();
    db.upsert_file(path, shifted.clone());
    let mut fresh = IncrementalDatabase::default();
    let fresh_file = fresh.upsert_file(path, shifted);
    let snapshot = db.snapshot();
    let fresh_snapshot = fresh.snapshot();
    assert_eq!(
        snapshot.definition_sites(file),
        fresh_snapshot.definition_sites(fresh_file)
    );
    assert_eq!(
        snapshot.name_occurrences(file),
        fresh_snapshot.name_occurrences(fresh_file)
    );
    assert_eq!(
        snapshot.file_outline(file),
        fresh_snapshot.file_outline(fresh_file)
    );
    assert_eq!(snapshot.resolve_labels(), fresh_snapshot.resolve_labels());
    assert_eq!(
        snapshot.resolve_citations(),
        fresh_snapshot.resolve_citations()
    );
    assert!(!db.query_log().iter().any(|entry| matches!(
        entry.kind,
        QueryKind::IncludeComponents | QueryKind::ResolvedLabels | QueryKind::ResolvedCitations
    )));
}

#[test]
fn attachment_replacement_and_close_invalidate_edits_over_identical_backing() {
    let mut db = IncrementalDatabase::default();
    let project = db.project_id();
    let path = Path::new("/project/attachment.tex");
    db.set_backing(project, path, Some("Text".into_source_text()))
        .unwrap();
    let source = db.replace_overlay(project, path, "Text").unwrap();
    let old = db.source_version(source).unwrap();
    db.replace_overlay(project, path, "Text").unwrap();
    assert_ne!(old, db.source_version(source).unwrap());
    let reopened = db.source_version(source).unwrap();
    db.close_overlay(project, path).unwrap();
    assert_ne!(reopened, db.source_version(source).unwrap());
    assert_eq!(db.file_text(source), "Text");
}

#[test]
fn changing_one_namespace_reuses_the_other_components_facts() {
    let mut db = IncrementalDatabase::default();
    let left = Path::new("/project/left.tex");
    let right = Path::new("/project/right.tex");
    db.upsert_file(
        left,
        "\\documentclass{article}\\label{left}\\bibliography{left}",
    );
    let right_id = db.upsert_file(
        right,
        "\\documentclass{article}\\label{right}\\bibliography{right}",
    );
    db.upsert_file(Path::new("/project/left.bib"), "@book{first,title={First}}");
    db.upsert_file(
        Path::new("/project/right.bib"),
        "@book{other,title={Other}}",
    );
    db.snapshot().resolve_project();
    db.clear_query_log();
    db.upsert_file(
        left,
        "\\documentclass{article}\\label{changed}\\bibliography{left}",
    );
    db.upsert_file(
        Path::new("/project/left.bib"),
        "@book{second,title={Second}}",
    );
    let snapshot = db.snapshot();
    let (labels, citations) = snapshot.resolve_project();
    assert!(labels.is_defined(left, "changed"));
    assert!(citations.is_defined(left, "second"));
    assert!(labels.is_defined(right, "right"));
    assert!(citations.is_defined(right, "other"));
    assert!(
        !db.query_log()
            .iter()
            .any(|entry| entry.file == Some(right_id)
                && matches!(
                    entry.kind,
                    QueryKind::ComponentLabels | QueryKind::ComponentCitations
                ))
    );
}

#[test]
fn label_context_cache_is_lazy_shared_across_candidates_and_invalidates_with_ownership() {
    use tex_ls_parser::semantic::LabelContext;
    let mut db = IncrementalDatabase::default();
    let origin = Path::new("/project/context-main.tex");
    let child = Path::new("/project/context-child.tex");
    let mut origin_source = String::from(
        r"\input{context-child}\section{Live}\meaning\section{Copied}\label{ownership}",
    );
    for index in 0..64 {
        origin_source.push_str(&format!(
            "\n\\section{{Origin {index}}}\\label{{origin-{index}}}"
        ));
    }
    let child_source = (0..256)
        .map(|index| format!("\n\\section{{Child {index}}}\\label{{child-{index}}}"))
        .collect::<String>();
    let origin_file = db.apply_change(origin, origin_source.as_str(), None);
    let child_file = db.apply_change(child, child_source.as_str(), None);
    {
        let snapshot = db.snapshot();
        snapshot.semantic_model(origin_file);
        snapshot.file_outline(origin_file);
        snapshot.file_modes(origin_file);
        assert!(
            snapshot
                .query_log()
                .iter()
                .all(|entry| entry.kind != QueryKind::FileLabelContexts)
        );
    }
    db.clear_query_log();
    {
        let snapshot = db.snapshot();
        for (prefix, count) in [("origin", 64), ("child", 256)] {
            for index in 0..count {
                let expected = if prefix == "origin" {
                    format!("Origin {index}")
                } else {
                    format!("Child {index}")
                };
                assert_eq!(
                    snapshot.label_context(origin, &format!("{prefix}-{index}")),
                    Some(LabelContext::Section { title: expected })
                );
            }
        }
        for file in [origin_file, child_file] {
            assert_eq!(
                snapshot
                    .query_log()
                    .iter()
                    .filter(|entry| entry.kind == QueryKind::FileLabelContexts
                        && entry.file == Some(file))
                    .count(),
                1
            );
        }
        assert_eq!(
            snapshot.label_context(origin, "ownership"),
            Some(LabelContext::Section {
                title: "Live".into()
            })
        );
    }
    db.clear_query_log();
    for index in 0..256 {
        assert!(
            db.snapshot()
                .label_context(origin, &format!("child-{index}"))
                .is_some()
        );
    }
    assert!(
        db.query_log()
            .iter()
            .all(|entry| entry.kind != QueryKind::FileLabelContexts)
    );
    db.apply_change(
        origin,
        format!(r"\renewcommand{{\meaning}}[1]{{#1}}{origin_source}"),
        None,
    );
    assert_eq!(
        db.snapshot().label_context(origin, "ownership"),
        Some(LabelContext::Section {
            title: "Copied".into()
        })
    );
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileLabelContexts)
            .count(),
        1
    );
    assert_eq!(
        db.query_log()
            .iter()
            .find(|entry| entry.kind == QueryKind::FileLabelContexts)
            .unwrap()
            .file,
        Some(origin_file)
    );
}

#[test]
fn label_context_cache_keeps_first_duplicate_and_none_falls_through_to_other_definers() {
    use tex_ls_parser::semantic::LabelContext;
    let mut db = IncrementalDatabase::default();
    let origin = Path::new("/project/duplicate-main.tex");
    let child = Path::new("/project/duplicate-child.tex");
    let file = db.apply_change(origin, r"\input{duplicate-child}\label{none-first}\section{Origin}\label{none-first}\label{first-title}\section{Later}\label{first-title}", None);
    db.apply_change(child, r"\section{Child}\label{none-first}", None);
    let snapshot = db.snapshot();
    assert_eq!(
        snapshot.file_label_contexts(file).get("none-first"),
        Some(None)
    );
    assert_eq!(
        snapshot.label_context(origin, "none-first"),
        Some(LabelContext::Section {
            title: "Child".into()
        })
    );
    assert_eq!(
        snapshot.label_context(origin, "first-title"),
        Some(LabelContext::Section {
            title: "Origin".into()
        })
    );
    assert_eq!(snapshot.label_context(origin, "absent"), None);
}

#[test]
fn label_context_cache_stores_long_titles_once_per_source_context() {
    let mut db = IncrementalDatabase::default();
    let path = Path::new("/project/shared-titles.tex");
    let heading = "Heading ".repeat(4096);
    let caption = "Caption ".repeat(4096);
    let mut source = format!("\\section{{{heading}}}");
    for index in 0..256 {
        source.push_str(&format!("\\label{{section-{index}}}"));
    }
    source.push_str(&format!("\\begin{{figure}}\\caption{{{caption}}}"));
    for index in 0..256 {
        source.push_str(&format!("\\label{{float-{index}}}"));
    }
    source.push_str(r"\end{figure}");
    let file = db.apply_change(path, source, None);
    let snapshot = db.snapshot();
    let cache = snapshot.file_label_contexts(file);
    assert_eq!(cache.labels.len(), 512);
    assert_eq!(cache.contexts.len(), 2);
    assert_eq!(cache.get("section-0"), cache.get("section-255"));
    assert_eq!(cache.get("float-0"), cache.get("float-255"));
    let stored_text_bytes = cache
        .contexts
        .values()
        .map(|context| match context {
            tex_ls_parser::semantic::LabelContext::Section { title } => title.len(),
            tex_ls_parser::semantic::LabelContext::Float { caption, .. } => {
                caption.as_ref().map_or(0, String::len)
            }
            _ => 0,
        })
        .sum::<usize>();
    assert_eq!(
        stored_text_bytes,
        heading.trim().len() + caption.trim().len()
    );
}

#[test]
fn bibliography_targets_are_lazy_shared_and_refresh_source_ownership() {
    let path = Path::new("/project/bibliography-targets.tex");
    let source = r"\bibliography{first, second}\addbibresource{third.bib}\meaning\addbibresource{printed}\let\copy\addbibresource{copied}\newcommand{\wrap}[1]{\addbibresource{protected}}\bibitem{manual}\nocite{*}";
    let edited = format!(r"\renewcommand{{\addbibresource}}[1]{{}}{source}");
    let mut db = Database::default();
    let file = db.upsert_file(path, source);
    let identity = *file.identity(&db);
    for (text, names) in [
        // Discovery conservatively retains literal resources in replacement
        // bodies, matching the existing full citation facts.
        (
            source,
            vec!["first.bib", "second.bib", "third.bib", "protected.bib"],
        ),
        (edited.as_str(), vec!["first.bib", "second.bib"]),
    ] {
        db.upsert_file(path, text);
        db.clear_query_log();
        let targets = queries::file_bibliography_targets(&db, file).clone();
        assert_eq!(
            targets,
            names
                .into_iter()
                .map(|name| BibTarget::Path(name.into()))
                .collect::<Vec<_>>()
        );
        assert_eq!(targets, *queries::file_bibliography_targets(&db, file));
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == QueryKind::FileBibliographyTargets
                    && entry.file == Some(identity))
                .count(),
            1,
            "cold resource discovery executes once per source revision"
        );
        assert!(
            db.query_log().iter().all(|entry| !matches!(
                entry.kind,
                QueryKind::SemanticModel | QueryKind::ResolvedLabels | QueryKind::ComponentLabels
            )),
            "resource discovery does not materialize citation or label semantics"
        );
        db.clear_query_log();
        assert_eq!(targets, *queries::file_bibliography_targets(&db, file));
        assert!(
            db.query_log().is_empty(),
            "warm resource discovery reuses its memo"
        );
        let facts = queries::file_cite_facts(&db, file);
        assert_eq!(targets, facts.bib_targets);
        assert_eq!(facts.manual_keys, vec![SmolStr::new("manual")]);
        assert!(facts.nocite_all);
        let mut fresh = Database::default();
        let fresh_file = fresh.upsert_file(path, text);
        assert_eq!(
            targets,
            *queries::file_bibliography_targets(&fresh, fresh_file)
        );
        assert_eq!(facts, queries::file_cite_facts(&fresh, fresh_file));
    }
}

#[test]
fn file_role_consumers_share_one_context_per_revision_and_refresh_loader_overrides() {
    let path = Path::new("/project/role-cache.tex");
    let source = r"\documentclass{article}\graphicspath{{images/}}\input{chapter}\usepackage{array}\addbibresource{refs.bib}\includegraphics{plot}";
    let overridden = format!(
        r"\renewcommand{{\input}}[1]{{}}\renewcommand{{\usepackage}}[1]{{}}\renewcommand{{\addbibresource}}[1]{{}}\renewcommand{{\graphicspath}}[1]{{}}{source}"
    );
    let mut db = Database::default();
    let file = db.upsert_file(path, source);
    let identity = *file.identity(&db);
    let check = |db: &Database, text: &str, overridden: bool| {
        let includes = queries::include_edges(db, file);
        let packages = queries::package_edges(db, file);
        let citations = queries::file_cite_facts(db, file);
        let references = queries::file_references(db, file);
        let graphics = queries::root_graphics(db, file);
        assert_eq!(includes.len(), usize::from(!overridden));
        assert_eq!(packages.len(), if overridden { 1 } else { 2 });
        assert_eq!(citations.bib_targets.len(), usize::from(!overridden));
        let expected_graphics = if overridden {
            Vec::new()
        } else {
            vec![PathBuf::from("/project/images")]
        };
        assert_eq!(graphics, &expected_graphics);
        let image = references
            .iter()
            .find(|reference| {
                reference
                    .candidates
                    .local
                    .iter()
                    .any(|p| p.ends_with("plot.pdf"))
            })
            .expect("live graphics call remains a file reference");
        assert_eq!(
            image
                .candidates
                .local
                .contains(&PathBuf::from("/project/images/plot.pdf")),
            !overridden
        );
        assert_eq!(references.len(), if overridden { 2 } else { 5 });
        // A full fresh analysis is the oracle for extraction, including exact
        // current spans and candidate ordering after the declarations change.
        let mut fresh = Database::default();
        let fresh_file = fresh.upsert_file(path, text);
        assert_eq!(includes, queries::include_edges(&fresh, fresh_file));
        assert_eq!(packages, queries::package_edges(&fresh, fresh_file));
        assert_eq!(citations, queries::file_cite_facts(&fresh, fresh_file));
        assert_eq!(references, queries::file_references(&fresh, fresh_file));
        assert_eq!(graphics, queries::root_graphics(&fresh, fresh_file));
    };
    db.clear_query_log();
    check(&db, source, false);
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileRoles && entry.file == Some(identity))
            .count(),
        1,
        "all five extraction paths share one source-role index"
    );
    db.clear_query_log();
    check(&db, source, false);
    assert!(
        db.query_log().is_empty(),
        "unchanged extractions remain cached"
    );
    assert!(db.upsert_file(path, overridden.as_str()) == file);
    check(&db, &overridden, true);
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::FileRoles && entry.file == Some(identity))
            .count(),
        1,
        "a source loader override rebuilds the shared index once"
    );
}

#[test]
fn operand_facts_are_shared_across_consumers_and_match_fresh_analysis_after_edits() {
    let path = Path::new("/project/operand-cache.tex");
    let source = r"\includeonly{a}\meaning\includeonly{b}\include{a}\meaning\newcommand{\fake}[1]{#1}\section{Live}\label{live}\meaning\ref{printed}\ref{live}";
    let mut db = IncrementalDatabase::default();
    let file = db.apply_change(path, source, None);
    let check = |snapshot: &Analysis, source: &str, expected_refs: usize, expected_fake: bool| {
        let root = snapshot.parsed_tree(file);
        let full = crate::parser::parse(source).syntax();
        assert_eq!(root.green(), full.green());
        assert_eq!(
            snapshot.semantic_model(file),
            &tex_ls_parser::semantic::SemanticModel::build(&full)
        );
        assert_eq!(snapshot.semantic_model(file).refs().len(), expected_refs);
        assert_eq!(
            snapshot.document_signatures(file),
            &tex_ls_parser::semantic::scan_definitions(&full)
        );
        assert_eq!(
            snapshot.document_signatures(file).command("fake").is_some(),
            expected_fake
        );
        assert_eq!(
            snapshot.file_modes(file),
            &tex_ls_parser::semantic::mode::ModeIndex::build(&full)
        );
        assert_eq!(
            snapshot.file_outline(file),
            tex_ls_parser::semantic::outline(&full)
        );
        assert_eq!(
            snapshot.doc_associations(file),
            tex_ls_parser::semantic::doc_associations(&full)
        );
        snapshot.file_label_contexts(file);
        snapshot.file_references(file);
        assert_eq!(
            snapshot.label_context(path, "live"),
            Some(tex_ls_parser::semantic::LabelContext::Section {
                title: "Live".into()
            })
        );
    };
    db.clear_query_log();
    check(&db.snapshot(), source, 1, false);
    for kind in [QueryKind::FileOperands, QueryKind::DefinitionSites] {
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == kind && entry.file == Some(file))
                .count(),
            1
        );
    }
    db.clear_query_log();
    check(&db.snapshot(), source, 1, false);
    assert!(db.query_log().iter().all(|entry| !matches!(
        entry.kind,
        QueryKind::FileOperands | QueryKind::DefinitionSites
    )));
    let edited = format!(r"\renewcommand{{\meaning}}[1]{{#1}}{source}");
    db.apply_change(path, edited.as_str(), None);
    check(&db.snapshot(), &edited, 2, true);
    for kind in [QueryKind::FileOperands, QueryKind::DefinitionSites] {
        assert_eq!(
            db.query_log()
                .iter()
                .filter(|entry| entry.kind == kind && entry.file == Some(file))
                .count(),
            1
        );
    }
}

#[test]
fn empty_definition_cache_preserves_copied_file_roles_and_nested_live_uses() {
    let path = Path::new("/project/empty-sites.tex");
    let source = r"\meaning\input{fake \input{nested}}\ifx a\input{compared}\fi\input{real}";
    let mut db = IncrementalDatabase::default();
    let file = db.apply_change(path, source, None);
    let snapshot = db.snapshot();
    assert!(snapshot.definition_sites(file).is_empty());
    let root = snapshot.parsed_tree(file);
    let roles = crate::external::links::FileRoleContext::from_definition_sites_and_operands(
        &root,
        snapshot.definition_sites(file),
        snapshot.file_operand_contexts(file),
    );
    let heads = root
        .descendants()
        .filter(|node| crate::ast::command_name(node).as_deref() == Some("input"))
        .map(|node| roles.allows(&node))
        .collect::<Vec<_>>();
    assert_eq!(heads, vec![false, true, false, true]);
}

#[test]
fn manual_bib_body_ends_share_queries_and_preserve_extent_boundaries() {
    use crate::syntax::SyntaxKind;
    use rowan::TextSize;
    let path = Path::new("/project/manual.tex");
    let source = "\\begin{thebibliography}{9}\n\\bibitem[One]{Écho} First.\n\\bibitem{Two}\\bibitem{Three} Third.\n\\end{thebibliography}\n\\bibitem{Tail} EOF body";
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(path, source);
    let old_ends = |snapshot: &Analysis| {
        let mut ends: Vec<_> = snapshot
            .semantic_model(file)
            .bibitems()
            .iter()
            .map(|item| item.range.start())
            .chain(
                snapshot
                    .parsed_tree(file)
                    .descendants()
                    .filter(|node| {
                        node.kind() == SyntaxKind::END
                            && crate::ast::environment_name(node).as_deref()
                                == Some("thebibliography")
                    })
                    .map(|node| node.text_range().start()),
            )
            .collect();
        ends.sort();
        ends
    };
    db.clear_query_log();
    for _ in 0..3 {
        let snapshot = db.snapshot();
        assert_eq!(snapshot.manual_bib_body_ends(file), old_ends(&snapshot));
        let ends = snapshot.manual_bib_body_ends(file);
        let text = snapshot.file_text(file);
        let bodies: Vec<_> = snapshot
            .semantic_model(file)
            .bibitems()
            .iter()
            .map(|item| {
                let start = item.range.end();
                let end = ends
                    .get(ends.partition_point(|end| *end < start))
                    .copied()
                    .unwrap_or_else(|| TextSize::new(text.len() as u32));
                &text[usize::from(start)..usize::from(end)]
            })
            .collect();
        assert_eq!(bodies, [" First.\n", "", " Third.\n", " EOF body"]);
    }
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::ManualBibBodyEnds)
            .count(),
        1
    );
    db.upsert_file(Path::new("/project/unrelated.tex"), "unrelated");
    assert_eq!(
        db.snapshot().manual_bib_body_ends(file),
        old_ends(&db.snapshot())
    );
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::ManualBibBodyEnds)
            .count(),
        1
    );
    db.upsert_file(path, format!("prefix\n\n{source}"));
    let snapshot = db.snapshot();
    assert_eq!(snapshot.manual_bib_body_ends(file), old_ends(&snapshot));
    assert_eq!(
        db.query_log()
            .iter()
            .filter(|entry| entry.kind == QueryKind::ManualBibBodyEnds)
            .count(),
        2
    );
}
