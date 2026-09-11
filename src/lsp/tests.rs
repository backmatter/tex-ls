//! Native tests responsibility.
use super::*;

#[test]
fn bibliography_seeding_publishes_external_path_alias() {
    let project = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let main_path = project.path().join("main.tex");
    let actual_bib = external.path().join("shared.bib");
    std::fs::write(&actual_bib, "@article{present, title={Present}}\n").unwrap();

    let mut db = IncrementalDatabase::default();
    db.upsert_file(
        &main_path,
        "\\documentclass{article}\n\\bibliography{shared}\n\\cite{present}\n".to_string(),
    );
    let requested = project.path().join("shared.bib");
    let mut lookups = HashMap::new();
    let project_id = db.project_id();
    let grew = seed_bibliographies_with(&mut db, project_id, &mut lookups, |path, base| {
        assert_eq!(path, requested);
        assert_eq!(base, Some(project.path()));
        Some(actual_bib.clone())
    });

    assert!(grew);
    let citations = db.resolve_citations();
    assert!(citations.is_defined(&main_path, "present"));
    assert!(citations.is_closed(&main_path));
    assert_eq!(citations.bib_definers(&main_path), &[actual_bib]);

    assert!(!seed_bibliographies_with(
        &mut db,
        project_id,
        &mut lookups,
        |_, _| panic!("a published alias must not be looked up again")
    ));
}

fn uri(s: &str) -> Uri {
    s.parse().unwrap()
}

#[test]
fn disabled_ipc_receiver_stays_pending() {
    let (_ipc, rx) = ipc_channel(false, &EditorSettings::default(), Vec::new());
    assert!(matches!(
        rx.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty)
    ));
}

#[test]
fn decide_starts_when_idle() {
    let mut pending = HashMap::new();
    pending.insert(uri("file:///a.tex"), 1);
    assert_eq!(
        decide(None, &pending),
        DispatchAction::Start(uri("file:///a.tex"))
    );
}

#[test]
fn decide_waits_when_idle_and_empty() {
    assert_eq!(decide(None, &HashMap::new()), DispatchAction::Wait);
}

#[test]
fn decide_supersedes_only_on_newer_same_uri() {
    let a = uri("file:///a.tex");
    let mut pending = HashMap::new();
    pending.insert(a.clone(), 5);
    assert_eq!(
        decide(Some((&a, 3)), &pending),
        DispatchAction::SupersedeAndStart(a.clone())
    );
    // Same version (not strictly newer): wait.
    assert_eq!(decide(Some((&a, 5)), &pending), DispatchAction::Wait);
}

#[test]
fn decide_never_cancels_inflight_for_a_different_uri() {
    let a = uri("file:///a.tex");
    let b = uri("file:///b.tex");
    let mut pending = HashMap::new();
    pending.insert(b, 9);
    // A's analyze is in flight; only B is queued → wait, never cancel A.
    assert_eq!(decide(Some((&a, 1)), &pending), DispatchAction::Wait);
}

#[test]
fn editor_settings_namespaced_and_bare() {
    let bare = serde_json::json!({ "lineWidth": 100, "indentWidth": 4 });
    let s = EditorSettings::from_client_value(&bare).unwrap();
    assert_eq!(s.line_width, Some(100));
    assert_eq!(s.indent_width, Some(4));
    let style = s.to_format_style();
    assert_eq!(style.line_width, 100);
    assert_eq!(style.indent_width, 4);

    let namespaced = serde_json::json!({ "meaning": { "lineWidth": 72 } });
    let s = EditorSettings::from_client_value(&namespaced).unwrap();
    assert_eq!(s.line_width, Some(72));
    assert_eq!(s.indent_width, None);
}

#[test]
fn editor_settings_texmf() {
    let value = serde_json::json!({
        "texmf": { "enabled": false, "roots": ["/opt/texmf"], "useKpsewhich": false }
    });
    let s = EditorSettings::from_client_value(&value).unwrap();
    assert!(!s.texmf.config().enabled);
    assert!(!s.texmf.config().use_kpsewhich);
    assert_eq!(s.texmf.config().roots, vec![PathBuf::from("/opt/texmf")]);
    // Omitted entirely: the defaults (enabled, kpsewhich discovery).
    let s = EditorSettings::from_client_value(&serde_json::json!({ "lineWidth": 80 })).unwrap();
    assert!(s.texmf.config().enabled);
    assert!(s.texmf.config().use_kpsewhich);
    assert!(s.texmf.config().roots.is_empty());
}

/// A bare [`GlobalState`] with the given editor settings and an empty cache, for
/// exercising [`GlobalState::resolve_settings`].
fn state_with_editor(editor: EditorSettings) -> GlobalState {
    GlobalState {
        documents: HashMap::new(),
        editor_settings: editor,
        config_cache: HashMap::new(),
        config_errors: HashMap::new(),
        config_messages: Vec::new(),
        declarations: HashMap::new(),
        supports_pull_diagnostics: false,
        supports_diagnostic_refresh: false,
        supports_dynamic_watchers: false,
        next_request_id: 1,
        position_encoding: PositionEncoding::Utf16,
        workspace_roots: Vec::new(),
    }
}

/// A `file://` URI for `main.tex` inside `dir`.
fn file_uri_in(dir: &Path) -> Uri {
    // Go through `path_to_uri` so the URI is well-formed on Windows too,
    // where `dir.display()` yields `C:\…` (backslashes, no leading slash).
    path_to_uri(&dir.join("main.tex")).expect("file uri")
}

#[test]
fn resolve_settings_prefers_file_config_over_editor() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("meaning.toml"),
        "[format]\nline-width = 100\nindent-width = 8\n",
    )
    .expect("write config");
    let mut state = state_with_editor(EditorSettings {
        line_width: Some(40),
        indent_width: Some(3),
        ..Default::default()
    });
    let resolved = state.resolve_settings(&file_uri_in(dir.path()));
    assert!(resolved.config_present);
    assert_eq!(resolved.style.line_width, 100);
    assert_eq!(resolved.style.indent_width, 8);
}

#[test]
fn resolve_settings_falls_back_to_editor_without_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_with_editor(EditorSettings {
        line_width: Some(40),
        indent_width: None,
        ..Default::default()
    });
    let resolved = state.resolve_settings(&file_uri_in(dir.path()));
    assert!(!resolved.config_present);
    assert_eq!(resolved.style.line_width, 40);
    // Unset editor knob keeps the built-in default.
    assert_eq!(
        resolved.style.indent_width,
        FormatStyle::default().indent_width
    );
    assert!(resolved.wrap_override.is_none());
}

#[test]
fn resolve_settings_wrap_override_from_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("meaning.toml"),
        "[format]\nwrap = \"preserve\"\n",
    )
    .expect("write config");
    let mut state = state_with_editor(EditorSettings::default());
    let resolved = state.resolve_settings(&file_uri_in(dir.path()));
    assert_eq!(resolved.wrap_override, Some(WrapMode::Preserve));
}

#[test]
fn resolve_settings_stable_wrap_from_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("meaning.toml"),
        "[format]\nline-width = 100\nwrap = \"stable\"\n",
    )
    .expect("write config");
    let mut state = state_with_editor(EditorSettings::default());
    let resolved = state.resolve_settings(&file_uri_in(dir.path()));
    assert_eq!(resolved.wrap_override, Some(WrapMode::Stable));
}

#[test]
fn resolve_settings_applies_lint_selection() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("meaning.toml"),
        "[lint]\nselect = [\"duplicate-label\"]\n",
    )
    .expect("write config");
    let mut state = state_with_editor(EditorSettings::default());
    let rules = state
        .resolve_settings(&file_uri_in(dir.path()))
        .rule_selection();
    assert!(rules.is_active("duplicate-label"));
    assert!(!rules.is_active("deprecated-command"));
    // Parse diagnostics are never filtered out.
    assert!(rules.is_active("parse"));
}

#[test]
fn resolve_settings_builds_exclude_filter_for_sibling_discovery() {
    // The resolved exclude filter is what `Worker::seed_dir` feeds to
    // `collect_lint_files`, so verify it prunes a configured directory while
    // keeping a normal sibling — the whole point of plumbing config into the
    // worker.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("meaning.toml"), "exclude = [\"vendor/\"]\n")
        .expect("write config");
    std::fs::write(dir.path().join("main.tex"), "").expect("write main");
    std::fs::create_dir(dir.path().join("vendor")).expect("mkdir vendor");
    std::fs::write(dir.path().join("vendor").join("lib.tex"), "").expect("write lib");

    let mut state = state_with_editor(EditorSettings::default());
    let resolved = state.resolve_settings(&file_uri_in(dir.path()));

    let files =
        collect_lint_files(&[dir.path().to_path_buf()], &resolved.exclude).expect("collect");
    let names: Vec<_> = files
        .iter()
        .map(|(p, _)| p.strip_prefix(dir.path()).unwrap_or(p).to_path_buf())
        .collect();
    assert!(names.contains(&PathBuf::from("main.tex")));
    assert!(
        !names.iter().any(|p| p.starts_with("vendor")),
        "excluded sibling should be pruned, got {names:?}"
    );
}

#[test]
fn resolve_settings_without_config_excludes_nothing() {
    // The editor-fallback path keeps the historical unfiltered walk: a
    // `vendor/` sibling is still discovered when no `meaning.toml` governs.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("main.tex"), "").expect("write main");
    std::fs::create_dir(dir.path().join("vendor")).expect("mkdir vendor");
    std::fs::write(dir.path().join("vendor").join("lib.tex"), "").expect("write lib");

    let mut state = state_with_editor(EditorSettings::default());
    let resolved = state.resolve_settings(&file_uri_in(dir.path()));
    assert!(!resolved.config_present);

    let files =
        collect_lint_files(&[dir.path().to_path_buf()], &resolved.exclude).expect("collect");
    let names: Vec<_> = files
        .iter()
        .map(|(p, _)| p.strip_prefix(dir.path()).unwrap_or(p).to_path_buf())
        .collect();
    assert!(names.contains(&PathBuf::from("main.tex")));
    assert!(names.contains(&PathBuf::from("vendor/lib.tex")));
}

#[test]
fn resolve_settings_caches_by_anchor_until_cleared() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_with_editor(EditorSettings {
        line_width: Some(40),
        indent_width: None,
        ..Default::default()
    });
    let uri = file_uri_in(dir.path());
    assert_eq!(state.resolve_settings(&uri).style.line_width, 40);
    // A later editor change is masked by the cache until it is cleared (the
    // `didChangeConfiguration` handler clears it).
    state.editor_settings.line_width = Some(72);
    assert_eq!(state.resolve_settings(&uri).style.line_width, 40);
    state.config_cache.clear();
    assert_eq!(state.resolve_settings(&uri).style.line_width, 72);
}

#[test]
fn resolve_settings_detects_nearer_config_creation_and_deletion() {
    let repo = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(repo.path().join(".git")).expect("create git boundary");
    std::fs::write(
        repo.path().join("meaning.toml"),
        "[format]\nline-width = 60\n",
    )
    .expect("write root config");
    let nested = repo.path().join("chapters");
    std::fs::create_dir(&nested).expect("create nested dir");
    let uri = file_uri_in(&nested);
    let mut state = state_with_editor(EditorSettings::default());

    assert_eq!(state.resolve_settings(&uri).style.line_width, 60);

    let nearer = nested.join("meaning.toml");
    std::fs::write(&nearer, "[format]\nline-width = 40\n").expect("write nearer config");
    assert_eq!(state.resolve_settings(&uri).style.line_width, 40);

    std::fs::remove_file(nearer).expect("remove nearer config");
    assert_eq!(state.resolve_settings(&uri).style.line_width, 60);
}

#[test]
fn invalid_configuration_keeps_last_good_settings_until_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meaning.toml");
    let uri = file_uri_in(dir.path());
    let mut state = state_with_editor(EditorSettings::default());
    std::fs::write(
        &path,
        "[format]\nline-width = 61\n[environments.code]\nlike = 'lstlisting'\n",
    )
    .unwrap();
    let valid = state.resolve_settings(&uri);
    std::fs::write(&path, "[format]\nline-width = 0\n").unwrap();
    state.invalidate_settings();
    let retained = state.resolve_settings(&uri);
    assert_eq!(retained.style, valid.style);
    assert_eq!(retained.declarations, valid.declarations);
    assert_eq!(state.config_messages.len(), 1);
    state.resolve_settings(&uri);
    assert_eq!(
        state.config_messages.len(),
        1,
        "unchanged errors are reported once"
    );
    std::fs::write(&path, "[format]\nline-width = 73\n").unwrap();
    assert_eq!(state.resolve_settings(&uri).style.line_width, 73);
    assert!(state.config_errors.is_empty());
}

#[test]
fn invalid_editor_settings_report_an_error() {
    assert!(EditorSettings::from_client_value(&serde_json::json!({"lineWidth": 0})).is_err());
    assert!(EditorSettings::from_client_value(&serde_json::json!({"lineWidth": "wide"})).is_err());
}

#[test]
fn resolve_settings_untitled_uses_editor_fallback_uncached() {
    let mut state = state_with_editor(EditorSettings {
        line_width: Some(55),
        indent_width: None,
        ..Default::default()
    });
    let resolved = state.resolve_settings(&uri("untitled:Untitled-1"));
    assert!(!resolved.config_present);
    assert_eq!(resolved.style.line_width, 55);
    // A non-file buffer never joins the anchor-dir cache.
    assert!(state.config_cache.is_empty());
}
