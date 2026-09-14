//! Native acquisition. All I/O finishes before the feature snapshot is captured.
use super::*;
use tex_ls_analysis::external::*;
use tex_ls_analysis::incremental::ProjectId;

pub(super) fn location(path: &Path, list: bool) -> LocationObservation {
    let kind = match std::fs::metadata(path) {
        Ok(meta) => Observation::Present(if meta.is_dir() {
            LocationKind::Directory
        } else {
            LocationKind::File
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Observation::Absent,
        Err(error) => Observation::Error(error.to_string()),
    };
    let directory = if list {
        match std::fs::read_dir(path) {
            Ok(entries) => {
                let mut contents = DirectoryContents {
                    complete: true,
                    ..Default::default()
                };
                for entry in entries {
                    match entry.and_then(|entry| Ok((entry.file_name(), entry.file_type()?))) {
                        Ok((name, kind)) => {
                            contents.entries.insert(
                                name.to_string_lossy().into_owned(),
                                if kind.is_dir() {
                                    LocationKind::Directory
                                } else {
                                    LocationKind::File
                                },
                            );
                        }
                        Err(_) => contents.complete = false,
                    }
                }
                Observation::Present(contents)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Observation::Absent,
            Err(error) => Observation::Error(error.to_string()),
        }
    } else {
        Observation::Unknown
    };
    LocationObservation { kind, directory }
}

#[cfg(test)]
pub(super) fn acquire_files(
    db: &mut IncrementalDatabase,
    project: ProjectId,
    path: &Path,
    completion: Option<(Position, PositionEncoding)>,
    installed: &InstalledPackages,
    refresh_known: bool,
) -> bool {
    let sources = |db: &IncrementalDatabase| {
        let snapshot = db.snapshot_for(project).expect("registered project");
        snapshot
            .tracked_files()
            .into_iter()
            .map(|(path, source)| (path, snapshot.source_version(source)))
            .collect::<Vec<_>>()
    };
    let before = sources(db);
    let snapshot = db.snapshot_for(project).expect("project");
    let (locations, directories) = completion_locations(&snapshot, path, completion);
    drop(snapshot);
    let files = FileInputs {
        locations: locations
            .into_iter()
            .map(|path| {
                let value = location(&path, false);
                (path, value)
            })
            .chain(directories.into_iter().map(|path| {
                let value = location(&path, true);
                (path, value)
            }))
            .collect(),
        ..Default::default()
    };
    let token = db
        .begin_external_refresh(project, ExternalInputKind::Files)
        .expect("registered project");
    db.apply_external_inputs(token, ExternalInputs::Files(files))
        .expect("current acquisition");
    #[cfg(test)]
    let index = Some(installed.index());
    #[cfg(not(test))]
    let index = installed.ready_index();
    if let Some(index) = index
        && db.snapshot_for(project).expect("project").texmf() != index.as_ref()
    {
        let token = db
            .begin_external_refresh(project, ExternalInputKind::Installed)
            .expect("project");
        db.apply_external_inputs(
            token,
            ExternalInputs::Installed(Observation::Present(InstalledMetadata {
                toolchain: "native".into(),
                index: index.as_ref().clone(),
            })),
        )
        .expect("current acquisition");
    }
    acquire_referenced_sources(db, project, refresh_known);
    before != sources(db)
}

/// Refresh reachable local source inputs to a fixed point. Physical identities
/// bound traversal through symlink cycles; logical paths remain analysis inputs.
#[cfg(test)]
fn acquire_referenced_sources(
    db: &mut IncrementalDatabase,
    project: ProjectId,
    refresh_known: bool,
) {
    let mut pending: Vec<_> = db
        .snapshot_for(project)
        .expect("registered project")
        .tracked_files()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let mut scanned = HashSet::new();
    let mut acquired = HashSet::new();
    while let Some(path) = pending.pop() {
        if !scanned.insert(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone())) {
            continue;
        }
        let candidates = {
            let snapshot = db.snapshot_for(project).expect("registered project");
            let Some(file) = snapshot.lookup_file(&path) else {
                continue;
            };
            if !file_kind_or_tex(&path).is_latex() {
                continue;
            }
            snapshot
                .file_references(file)
                .iter()
                .flat_map(|reference| reference.candidates.local.iter().cloned())
                .collect::<Vec<_>>()
        };
        let mut inputs = FileInputs::default();
        for candidate in candidates {
            if !refresh_known
                && db
                    .snapshot_for(project)
                    .expect("registered project")
                    .lookup_file(&candidate)
                    .is_some()
            {
                pending.push(candidate);
                continue;
            }
            if tex_ls_analysis::source::lint_file_kind(&candidate).is_none()
                || !acquired.insert(candidate.clone())
            {
                continue;
            }
            let mut observed = location(&candidate, false);
            match file_acquisition::read_source(&candidate) {
                Ok(text) => {
                    inputs.backing.push((
                        candidate.clone(),
                        Some(Arc::new(tex_ls_analysis::text::SourceText::new(text))),
                    ));
                    pending.push(candidate.clone());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    inputs.backing.push((candidate.clone(), None));
                }
                Err(error) => observed.kind = Observation::Error(error.to_string()),
            }
            inputs.locations.push((candidate, observed));
        }
        if !inputs.locations.is_empty() {
            let token = db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .expect("registered project");
            db.apply_external_inputs(token, ExternalInputs::Files(inputs))
                .expect("current acquisition");
        }
        // Record installed source aliases explicitly after local observations are
        // known. Package metadata remains separate from formatter signatures.
        let resolutions = {
            let snapshot = db.snapshot_for(project).expect("registered project");
            snapshot
                .lookup_file(&path)
                .into_iter()
                .flat_map(|file| snapshot.file_references(file))
                .filter_map(|reference| {
                    let requested = reference.candidates.local.first()?.clone();
                    if !matches!(
                        requested.extension().and_then(|ext| ext.to_str()),
                        Some("tex" | "def" | "lco" | "bib" | "bibtex")
                    ) {
                        return None;
                    }
                    Some((
                        requested,
                        snapshot.resolve_file(&reference.candidates).target,
                    ))
                })
                .collect::<Vec<_>>()
        };
        let mut inputs = FileInputs::default();
        for (requested, target) in resolutions {
            inputs
                .file_resolution
                .push((requested.clone(), target.clone()));
            if let Observation::Present(actual) = target
                && actual != requested
                && acquired.insert(actual.clone())
                && db
                    .snapshot_for(project)
                    .expect("registered project")
                    .lookup_file(&actual)
                    .is_none()
                && let Ok(text) = file_acquisition::read_source(&actual)
            {
                inputs.backing.push((
                    actual.clone(),
                    Some(Arc::new(tex_ls_analysis::text::SourceText::new(text))),
                ));
                inputs
                    .locations
                    .push((actual.clone(), location(&actual, false)));
                pending.push(actual);
            }
        }
        if !inputs.file_resolution.is_empty() {
            let token = db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .expect("registered project");
            db.apply_external_inputs(token, ExternalInputs::Files(inputs))
                .expect("current acquisition");
        }
    }
}

pub(super) fn compiler_members(
    db: &IncrementalDatabase,
    project: ProjectId,
    path: &Path,
    build: &BuildConfig,
) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let members = {
        let snapshot = db.snapshot_for(project).expect("registered project");
        let resolution = snapshot.resolve_labels();
        let mut namespace = namespace_of(resolution, path);
        if let [root] = resolution.candidate_roots(path)
            && !namespace.contains(&root.as_path())
        {
            namespace.insert(0, root.as_path());
        }
        let root_document = build
            .root
            .as_deref()
            .or_else(|| root_document_of(&snapshot, &namespace))
            .unwrap_or(path);
        if !namespace.contains(&root_document) {
            namespace.insert(0, root_document);
        }
        tex_ls_analysis::external::compiler_candidates_for_job(
            &namespace,
            root_document,
            build.aux_dir.as_deref(),
            build.job_name.as_deref(),
        )
    };
    let members: Vec<(PathBuf, Vec<PathBuf>)> = members
        .into_iter()
        .flat_map(|(logical, candidates)| {
            ["aux", "fls", "log"].into_iter().map(move |ext| {
                (
                    logical.with_extension(ext),
                    candidates
                        .iter()
                        .map(|path| path.with_extension(ext))
                        .collect(),
                )
            })
        })
        .collect();
    members
}

#[cfg(test)]
pub(super) fn acquire_compiler(
    db: &mut IncrementalDatabase,
    project: ProjectId,
    path: &Path,
    build: &BuildConfig,
) -> Vec<PathBuf> {
    let members = compiler_members(db, project, path, build);
    let mut watches: Vec<_> = members
        .iter()
        .flat_map(|(_, paths)| paths.iter().cloned())
        .collect();
    let artifacts = acquire_artifacts(members);
    // Include discovered AUX chains and physical aliases; missing candidates must
    // remain watched so a first build is observable too.
    watches.extend(artifacts.iter().filter_map(|(_, value)| match value {
        Observation::Present(artifact) => Some(PathBuf::from(&artifact.identity)),
        _ => None,
    }));
    watches.sort();
    watches.dedup();
    let token = db
        .begin_external_refresh(project, ExternalInputKind::Compiler)
        .expect("registered project");
    db.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))
        .expect("current acquisition");
    watches
}

static COMPILER_PROBES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn compiler_probes() -> u64 {
    COMPILER_PROBES.load(std::sync::atomic::Ordering::Relaxed)
}

pub(super) fn acquire_artifacts(
    members: Vec<(PathBuf, Vec<PathBuf>)>,
) -> Vec<(PathBuf, Observation<CompilerArtifact>)> {
    let mut artifacts = Vec::new();
    let mut pending: Vec<_> = members.into_iter().rev().collect();
    let mut visited = HashSet::new();
    let mut physical = HashMap::<PathBuf, CompilerArtifact>::new();
    while let Some((logical, candidates)) = pending.pop() {
        if !visited.insert(logical.clone()) {
            continue;
        }
        let mut value = Observation::Absent;
        for candidate in candidates {
            COMPILER_PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match std::fs::metadata(&candidate) {
                Ok(meta) if !meta.is_file() => {
                    value = Observation::Error("Compiler artifact is not a regular file".into());
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    value = Observation::Error(error.to_string());
                    break;
                }
                _ => {}
            }
            match std::fs::read(&candidate) {
                Ok(bytes) => {
                    let actual = candidate.canonicalize().unwrap_or(candidate.clone());
                    if let Some(artifact) = physical.get(&actual) {
                        value = Observation::Present(artifact.clone());
                        break;
                    }
                    let directory = physical
                        .get(&actual.with_extension("fls"))
                        .and_then(|artifact| artifact.recorder.as_ref())
                        .map(|recorder| recorder.directory.as_path())
                        .unwrap_or_else(|| logical.parent().unwrap_or(Path::new("")));
                    let mut artifact = CompilerArtifact::from_text_in(
                        &logical,
                        &String::from_utf8_lossy(&bytes),
                        actual.to_string_lossy().into_owned(),
                        directory,
                    );
                    let parsed = &mut artifact.parsed;
                    let base = actual.parent().unwrap_or(Path::new(""));
                    for input in &mut parsed.inputs {
                        let path = tex_ls_analysis::source::normalize_path(&base.join(&*input));
                        *input = path.to_string_lossy().into_owned();
                        pending.push((path.clone(), vec![path]));
                    }

                    physical.insert(actual, artifact.clone());
                    value = Observation::Present(artifact);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    value = Observation::Error(error.to_string());
                    break;
                }
            }
        }
        artifacts.push((logical, value));
    }
    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;
    use smol_str::SmolStr;
    use tex_ls_analysis::project::aux::*;

    #[test]
    fn import_completion_and_inherited_graphics_use_the_same_search_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("chapters")).unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("chapters/part.tex"), "").unwrap();
        std::fs::write(dir.path().join("assets/picture.png"), "").unwrap();
        let main = dir.path().join("main.tex");
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let installed = InstalledPackages::from(crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        });
        let source = r"\import{chapters/}{pa}";
        db.apply_change(&main, source, None);
        let position = Position::new(0, (source.len() - 1) as u32);
        acquire_files(
            &mut db,
            project,
            &main,
            Some((position, PositionEncoding::Utf16)),
            &installed,
            true,
        );
        let snapshot = db.snapshot();
        let items = compute_completion(
            &snapshot,
            &path_to_uri(&main).unwrap(),
            &main,
            PositionEncoding::Utf16,
            position,
        );
        assert!(items.items.iter().any(|item| item.label == "part.tex"));
        drop(snapshot);
        db.apply_change(
            &main,
            r"\graphicspath{{assets/}}\input{chapters/child}",
            None,
        );
        let child = dir.path().join("chapters/child.tex");
        let source = r"\includegraphics{picture}";
        db.replace_overlay(project, &child, source).unwrap();
        let position = Position::new(0, (source.find("picture").unwrap() + 2) as u32);
        acquire_files(
            &mut db,
            project,
            &child,
            Some((position, PositionEncoding::Utf16)),
            &installed,
            true,
        );
        let snapshot = db.snapshot();
        let items = compute_completion(
            &snapshot,
            &path_to_uri(&child).unwrap(),
            &child,
            PositionEncoding::Utf16,
            position,
        );
        assert!(items.items.iter().any(|item| item.label == "picture.png"));
        let links = snapshot.document_links(snapshot.lookup_file(&child).unwrap());
        assert_eq!(links[0].target, dir.path().join("assets/picture.png"));
    }

    #[test]
    fn installed_source_alias_participates_in_the_root_view() {
        let local = tempfile::tempdir().unwrap();
        let installed_root = tempfile::tempdir().unwrap();
        let target = installed_root.path().join("dependency.tex");
        std::fs::write(&target, r"\label{installed}").unwrap();
        let installed = InstalledPackages::from(crate::texmf::TexmfConfig {
            roots: vec![installed_root.path().to_path_buf()],
            use_kpsewhich: false,
            ..Default::default()
        });
        let main = local.path().join("main.tex");
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        db.apply_change(&main, r"\documentclass{article}\input{dependency}", None);
        acquire_files(&mut db, project, &main, None, &installed, true);
        assert!(db.resolve_labels().is_defined(&main, "installed"));
        assert_eq!(
            db.file_alias(&local.path().join("dependency.tex")),
            Some(target.as_path())
        );
    }

    #[test]
    fn referenced_sources_refresh_recursively_without_overwriting_overlays() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let child = dir.path().join("child.tex");
        let leaf = dir.path().join("leaf.tex");
        std::fs::write(&child, "\\input{leaf}").unwrap();
        std::fs::write(&leaf, "\\input{child}\\label{disk}").unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        db.open_overlay(project, &main, "\\input{child}").unwrap();
        acquire_referenced_sources(&mut db, project, true);
        assert!(db.snapshot().lookup_file(&leaf).is_some());
        db.open_overlay(project, &leaf, "\\label{editor}").unwrap();
        std::fs::remove_file(&leaf).unwrap();
        acquire_referenced_sources(&mut db, project, true);
        let snapshot = db.snapshot();
        assert_eq!(
            snapshot.file_text(snapshot.lookup_file(&leaf).unwrap()),
            "\\label{editor}"
        );
        drop(snapshot);
        db.close_overlay(project, &leaf).unwrap();
        assert!(db.snapshot().lookup_file(&leaf).is_none());
    }

    #[derive(Default)]
    struct Fixture;
    impl Fixture {
        fn data_for(
            &self,
            namespace: &[&Path],
            root: &Path,
            aux_dir: Option<&Path>,
        ) -> Option<AuxData> {
            let mut db = IncrementalDatabase::default();
            let artifacts = acquire_artifacts(tex_ls_analysis::external::compiler_candidates(
                namespace, root, aux_dir,
            ));
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
                .unwrap();
            db.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))
                .unwrap();
            db.snapshot().aux_data(namespace, root)
        }
    }

    fn labels_of(text: &str) -> HashMap<SmolStr, String> {
        parse_aux(text).data.labels
    }

    #[test]
    fn simple_newlabel() {
        let labels = labels_of("\\newlabel{sec:foo}{{1}{1}}\n");
        assert_eq!(labels[&SmolStr::new("sec:foo")], "1");
    }

    #[test]
    fn newlabel_with_caption_and_anchor_groups() {
        let labels = labels_of("\\newlabel{thm:foo}{{1}{1}{Foo}{lemma.1}{}}\n");
        assert_eq!(labels[&SmolStr::new("thm:foo")], "1");
    }

    #[test]
    fn ntheorem_nested_braces_flatten() {
        let labels = labels_of("\\newlabel{thm:test}{{1.{1}}{1}}\n");
        assert_eq!(labels[&SmolStr::new("thm:test")], "1.1");
    }

    #[test]
    fn caption_xref_group_is_skipped_for_the_page_number() {
        // The first group holds a command, not text; the next textual group wins
        // (texlab behavior).
        let labels =
            labels_of("\\newlabel{fig:qux}{{\\caption@xref {fig:qux}{ on input line 15}}{1}}\n");
        assert_eq!(labels[&SmolStr::new("fig:qux")], "1");
    }

    #[test]
    fn empty_groups_are_skipped() {
        let labels = labels_of("\\newlabel{x}{{}{2}}\n");
        assert_eq!(labels[&SmolStr::new("x")], "2");
    }

    #[test]
    fn newlabel_without_number_is_absent() {
        let labels = labels_of("\\newlabel{x}{{}{}}\n\\newlabel{y}{{\\relax}{}}\n");
        assert!(labels.is_empty());
    }

    #[test]
    fn truncated_entry_is_skipped_earlier_entries_survive() {
        let labels = labels_of("\\newlabel{a}{{1}{1}}\n\\newlabel{b}{{2}{1}\n");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[&SmolStr::new("a")], "1");
    }

    #[test]
    fn similarly_named_commands_do_not_match() {
        let parsed = parse_aux("\\newlabelx{a}{{1}{1}}\n\\@inputonce{f.aux}\n");
        assert!(parsed.data.labels.is_empty());
        assert!(parsed.inputs.is_empty());
    }

    #[test]
    fn at_input_targets_collected_in_order() {
        let parsed = parse_aux("\\@input{ch1.aux}\n\\relax\n\\@input{ch2.aux}\n");
        assert_eq!(parsed.inputs, vec!["ch1.aux", "ch2.aux"]);
    }

    #[test]
    fn toc_contentsline_with_numberline() {
        let toc = parse_aux(
            "\\@writefile{toc}{\\contentsline {section}{\\numberline {1.2}Basics}{3}{section.1.2}}\n",
        )
        .data
        .toc;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].level, "section");
        assert_eq!(toc[0].number.as_deref(), Some("1.2"));
        assert_eq!(toc[0].title, "Basics");
    }

    #[test]
    fn starred_contentsline_has_no_number() {
        let toc =
            parse_aux("\\@writefile{toc}{\\contentsline {section}{Preface}{1}{section*.1}}\n")
                .data
                .toc;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].number, None);
        assert_eq!(toc[0].title, "Preface");
    }

    #[test]
    fn title_keeps_macro_source() {
        let toc = parse_aux(
            "\\@writefile{toc}{\\contentsline {section}{\\numberline {1}\\textsc  {Intro}}{1}{section.1}}\n",
        )
        .data
        .toc;
        assert_eq!(toc[0].title, "\\textsc  {Intro}");
    }

    #[test]
    fn non_toc_writefile_streams_are_ignored() {
        let parsed = parse_aux(
            "\\@writefile{lof}{\\contentsline {figure}{\\numberline {1}{\\ignorespaces A chart}}{2}{figure.1}}\n",
        );
        assert!(parsed.data.toc.is_empty());
    }

    #[test]
    fn escaped_braces_do_not_unbalance_groups() {
        let labels = labels_of("\\newlabel{k}{{1}{1}{a \\{b\\} c}{s.1}{}}\n");
        assert_eq!(labels[&SmolStr::new("k")], "1");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn sibling_aux_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "\\documentclass{article}\n");
        write(&dir.path().join("main.aux"), "\\newlabel{sec:a}{{1}{1}}\n");
        let data = Fixture
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(data.labels[&SmolStr::new("sec:a")], "1");
    }

    #[test]
    fn missing_aux_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        assert_eq!(Fixture.data_for(&[&main], dir.path(), None), None);
    }

    #[test]
    fn aux_dir_candidates_flat_and_relative() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let chap = dir.path().join("chapters/one.tex");
        write(&main, "x\n");
        write(&chap, "x\n");
        // latexmk -auxdir layout: root flat, includes under their relative path.
        write(
            &dir.path().join("build/main.aux"),
            "\\newlabel{sec:root}{{1}{1}}\n",
        );
        write(
            &dir.path().join("build/chapters/one.aux"),
            "\\newlabel{sec:one}{{2}{3}}\n",
        );
        let data = Fixture
            .data_for(&[&main, &chap], dir.path(), Some(Path::new("build")))
            .expect("aux found");
        assert_eq!(data.labels[&SmolStr::new("sec:root")], "1");
        assert_eq!(data.labels[&SmolStr::new("sec:one")], "2");
        write(
            &main.with_extension("aux"),
            "\\newlabel{sec:root}{{OLD}{1}}\n",
        );
        let data = Fixture
            .data_for(&[&main, &chap], dir.path(), Some(Path::new("build")))
            .unwrap();
        assert_eq!(data.labels[&SmolStr::new("sec:root")], "1");
        std::fs::remove_file(dir.path().join("build/main.aux")).unwrap();
        assert!(
            Fixture
                .data_for(&[&main], dir.path(), Some(Path::new("build")))
                .is_none()
        );
    }

    #[test]
    fn at_input_chain_is_followed_with_cycles_guarded() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        // main.aux → ch1.aux → main.aux (cycle); ch1 is *not* in the namespace.
        write(
            &dir.path().join("main.aux"),
            "\\newlabel{a}{{1}{1}}\n\\@input{ch1.aux}\n",
        );
        write(
            &dir.path().join("ch1.aux"),
            "\\newlabel{b}{{2}{2}}\n\\@input{main.aux}\n",
        );
        let data = Fixture
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(data.labels.len(), 2);
        assert_eq!(data.labels[&SmolStr::new("b")], "2");
    }

    #[test]
    fn refresh_uses_contents_even_when_size_and_mtime_match() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        let aux = dir.path().join("main.aux");
        write(&aux, "\\newlabel{a}{{1}{1}}\n");
        let cache = Fixture;
        let first = cache
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(first.labels[&SmolStr::new("a")], "1");

        // Preserve metadata while replacing bytes of the same length.
        let old = std::fs::metadata(&aux).unwrap().modified().unwrap();
        write(&aux, "\\newlabel{a}{{7}{1}}\n");
        std::fs::File::options()
            .write(true)
            .open(&aux)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let second = cache
            .data_for(&[&main], dir.path(), None)
            .expect("aux found");
        assert_eq!(second.labels[&SmolStr::new("a")], "7");
    }

    #[test]
    fn deep_aux_graphs_and_path_alias_cycles_are_complete() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x");
        write(&dir.path().join("main.aux"), "\\@input{0.aux}\n");
        for n in 0..40 {
            let next = if n == 39 {
                "./main.aux".to_owned()
            } else {
                format!("{}.aux", n + 1)
            };
            write(
                &dir.path().join(format!("{n}.aux")),
                &format!("\\newlabel{{key{n}}}{{{{{n}}}{{1}}}}\n\\@input{{{next}}}\n"),
            );
        }
        let data = Fixture.data_for(&[&main], dir.path(), None).unwrap();
        assert_eq!(data.labels.len(), 40);
        assert_eq!(data.labels[&SmolStr::new("key39")], "39");
    }

    #[test]
    fn non_tex_members_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let bib = dir.path().join("refs.bib");
        write(&bib, "@article{a}\n");
        write(&dir.path().join("refs.aux"), "\\newlabel{x}{{1}{1}}\n");
        assert_eq!(Fixture.data_for(&[&bib], dir.path(), None), None);
    }

    #[test]
    fn realistic_aux_mix() {
        let text = "\\relax \n\
                    \\providecommand\\hyper@newdestlabel[2]{}\n\
                    \\@writefile{toc}{\\contentsline {section}{\\numberline {1}Intro}{1}{section.1}}\n\
                    \\newlabel{sec:intro}{{1}{1}{Intro}{section.1}{}}\n\
                    \\@writefile{lof}{\\contentsline {figure}{\\numberline {1}{\\ignorespaces X}}{2}{figure.1}}\n\
                    \\newlabel{fig:x}{{1}{2}{X}{figure.1}{}}\n\
                    \\@input{ch1.aux}\n\
                    \\citation{knuth1984}\n\
                    \\gdef \\@abspage@last{3}\n";
        let parsed = parse_aux(text);
        assert_eq!(parsed.data.labels.len(), 2);
        assert_eq!(parsed.data.labels[&SmolStr::new("sec:intro")], "1");
        assert_eq!(parsed.data.labels[&SmolStr::new("fig:x")], "1");
        assert_eq!(parsed.data.toc.len(), 1);
        assert_eq!(parsed.inputs, vec!["ch1.aux"]);
    }
}

/// Follow explicit parent hints without deriving parser declarations from disk.
#[cfg(test)]
pub(super) fn acquire_parents(
    db: &mut IncrementalDatabase,
    project: ProjectId,
    path: &Path,
    configured: Option<&Path>,
) -> bool {
    let before = db
        .snapshot_for(project)
        .expect("project")
        .project_members()
        .len();
    let mut pending = vec![path.to_owned()];
    if let Some(root) = configured {
        pending.push(root.to_owned());
    }
    let mut seen = HashSet::new();
    while let Some(path) = pending.pop() {
        if seen.len() >= 64 || !seen.insert(tex_ls_analysis::source::normalize_path(&path)) {
            continue;
        }
        if db
            .snapshot_for(project)
            .expect("project")
            .lookup_file(&path)
            .is_none()
            && let Ok(text) = file_acquisition::read_source(&path)
        {
            db.set_backing(project, &path, Some(text.into_source_text()))
                .expect("project");
        }
        let snapshot = db.snapshot_for(project).expect("project");
        if let Some(file) = snapshot.lookup_file(&path) {
            for parent in
                tex_ls_analysis::external::explicit_root_hints(snapshot.file_text(file), &path)
            {
                pending.push(parent);
            }
        }
    }
    acquire_referenced_sources(db, project, false);
    before
        != db
            .snapshot_for(project)
            .expect("project")
            .project_members()
            .len()
}

pub(super) fn completion_locations(
    snapshot: &Analysis,
    path: &Path,
    completion: Option<(Position, PositionEncoding)>,
) -> (
    std::collections::BTreeSet<PathBuf>,
    std::collections::BTreeSet<PathBuf>,
) {
    let mut locations = std::collections::BTreeSet::new();
    let mut directories = std::collections::BTreeSet::new();
    if let Some(file) = snapshot.lookup_file(path)
        && file_kind_or_tex(path).is_latex()
    {
        let root = snapshot.parsed_tree(file);
        for reference in snapshot.file_references(file) {
            locations.extend(reference.candidates.local.iter().cloned());
        }
        if let Some((position, encoding)) = completion {
            let offset = snapshot
                .file_line_index(file, encoding)
                .offset_at(position.line, position.character);
            let context = tex_ls_analysis::completion::classify_context_with_declarations(
                &root,
                offset,
                snapshot.declarations_for(path),
            );
            use tex_ls_analysis::completion::CompletionContext;
            let prefix = match &context {
                CompletionContext::FilePath { prefix, .. }
                | CompletionContext::PackageName { prefix, .. } => Some(prefix),
                _ => None,
            };
            if let Some(prefix) = prefix {
                let part = prefix
                    .rfind('/')
                    .map(|slash| &prefix[..=slash])
                    .unwrap_or("");
                for directory in snapshot
                    .file_path_context(file)
                    .into_iter()
                    .flat_map(|context| {
                        context.bases.iter().flat_map(|base| {
                            completion_directories(
                                &root,
                                offset,
                                Some(base),
                                Some(&context.root_directory),
                                &context.graphics,
                            )
                        })
                    })
                {
                    directories.insert(directory.join(part));
                }
            }
        }
    }
    (locations, directories)
}

#[cfg(test)]
mod audit_acquisition_tests {
    use super::*;
    #[test]
    fn child_open_loads_nested_parent_hints_and_guards_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let child = dir.path().join("chapters/ch.tex");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(
            &main,
            "\\documentclass{article}\\label{parent}\\input{chapters/ch}",
        )
        .unwrap();
        std::fs::write(&child, "% !TeX root = ../main.tex\n\\ref{parent}").unwrap();
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        db.apply_change(&child, std::fs::read_to_string(&child).unwrap(), None);
        assert!(acquire_parents(&mut db, project, &child, None));
        assert!(db.resolve_labels().is_defined(&child, "parent"));
        std::fs::write(&main, "% !TeX root = chapters/ch.tex\n").unwrap();
        assert!(!acquire_parents(&mut db, project, &child, None));
    }
    #[test]
    fn job_name_maps_artifacts_without_renaming_child_aux() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        let child = dir.path().join("child.tex");
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(
            dir.path().join("out/thesis.aux"),
            "\\newlabel{main}{{7}{1}}\n\\@input{child.aux}",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("out/child.aux"),
            "\\newlabel{child}{{8}{1}}",
        )
        .unwrap();
        let mut db = IncrementalDatabase::default();
        db.apply_change(
            &main,
            "\\documentclass{article}\\input{child}\\label{main}",
            None,
        );
        db.apply_change(&child, "\\label{child}", None);
        let project = db.project_id();
        acquire_compiler(
            &mut db,
            project,
            &child,
            &BuildConfig {
                aux_dir: Some("out".into()),
                job_name: Some("thesis".into()),
                ..Default::default()
            },
        );
        let aux = db.aux_data(&[&main, &child], dir.path()).unwrap();
        assert_eq!(aux.labels["main"], "7");
        assert_eq!(aux.labels["child"], "8");
    }
}
