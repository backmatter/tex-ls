//! Native compiler artifact acquisition and physical/logical provenance.
//! One batch owns payload/parse caches and AUX traversal; it returns observations.
use crate::config::BuildConfig;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
#[cfg(test)]
use tex_ls_analysis::external::{ExternalInputKind, ExternalInputs};
use tex_ls_analysis::{
    external::{CompilerArtifact, Observation},
    incremental::{IncrementalDatabase, ProjectId},
};
use tex_ls_protocol::{
    context::{namespace_of, root_document_of},
    convert::path_to_uri,
};

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
fn acquire_compiler(
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
    let acquired = acquire_artifacts_with_candidates(members);
    // Include exact physical aliases and missing AUX children so first builds
    // remain observable without interpreting opaque artifact identities.
    watches.extend(acquired.candidates.values().flatten().cloned());
    watches.sort();
    watches.dedup();
    let token = db
        .begin_external_refresh(project, ExternalInputKind::Compiler)
        .expect("registered project");
    db.apply_external_inputs(token, ExternalInputs::Compiler(acquired.artifacts))
        .expect("current acquisition");
    watches
}

static COMPILER_PROBES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static COMPILER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(in crate::lsp) fn compiler_probes() -> u64 {
    COMPILER_PROBES.load(std::sync::atomic::Ordering::Relaxed)
}

pub(super) struct AcquiredArtifacts {
    pub artifacts: Vec<(PathBuf, Observation<CompilerArtifact>)>,
    pub candidates: HashMap<PathBuf, Vec<PathBuf>>,
}

#[cfg(test)]
fn acquire_artifacts(
    members: Vec<(PathBuf, Vec<PathBuf>)>,
) -> Vec<(PathBuf, Observation<CompilerArtifact>)> {
    acquire_artifacts_with_candidates(members).artifacts
}

pub(super) fn acquire_artifacts_with_candidates(
    members: Vec<(PathBuf, Vec<PathBuf>)>,
) -> AcquiredArtifacts {
    let mut artifacts = Vec::new();
    let mut watched_candidates = HashMap::new();
    let mut pending: Vec<_> = members.into_iter().rev().collect();
    let mut visited = HashSet::new();
    let mut payloads = HashMap::<PathBuf, Vec<u8>>::new();
    let mut parsed =
        HashMap::<(PathBuf, PathBuf, Option<std::ffi::OsString>), CompilerArtifact>::new();
    let mut expanded_aux = HashSet::new();
    while let Some((logical, candidates)) = pending.pop() {
        if !visited.insert(logical.clone()) {
            continue;
        }
        let mut watches = candidates.clone();
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
            let actual = candidate.canonicalize().unwrap_or(candidate.clone());
            watches.push(actual.clone());
            let fallback = logical.parent().unwrap_or(Path::new(""));
            let recorder_key = (
                actual.with_extension("fls"),
                fallback.to_owned(),
                Some(std::ffi::OsString::from("fls")),
            );
            let directory = parsed
                .get(&recorder_key)
                .and_then(|artifact| artifact.recorder.as_ref())
                .map(|recorder| recorder.directory.as_path())
                .unwrap_or(fallback)
                .to_owned();
            let key = (
                actual.clone(),
                directory.clone(),
                logical.extension().map(std::ffi::OsStr::to_os_string),
            );
            if let Some(artifact) = parsed.get(&key) {
                value = Observation::Present(artifact.clone());
                break;
            }
            let identity = match actual.to_str() {
                Some(path) => path.to_owned(),
                None => match path_to_uri(&actual) {
                    Some(uri) => uri.to_string(),
                    None => {
                        value = Observation::Error(
                            "Cannot represent compiler artifact identity as a file URI".into(),
                        );
                        break;
                    }
                },
            };
            if let std::collections::hash_map::Entry::Vacant(entry) = payloads.entry(actual.clone())
            {
                #[cfg(test)]
                COMPILER_READS.with(|reads| reads.set(reads.get() + 1));
                match std::fs::read(&candidate) {
                    Ok(bytes) => {
                        entry.insert(bytes);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        value = Observation::Error(error.to_string());
                        break;
                    }
                }
            }
            // Bytes have physical identity; parsed relative paths also depend on
            // the supplied directory and the logical artifact's extension.
            let bytes = &payloads[&actual];
            let mut artifact = CompilerArtifact::from_text_in(
                &logical,
                &String::from_utf8_lossy(bytes),
                identity,
                &directory,
            );
            let base = actual.parent().unwrap_or(Path::new(""));
            let inputs: Vec<_> = artifact
                .parsed
                .inputs
                .iter()
                .map(|input| tex_ls_analysis::source::normalize_path(&base.join(input)))
                .collect();
            if !inputs.is_empty() && expanded_aux.insert(actual) {
                for input in &inputs {
                    pending.push((input.clone(), vec![input.clone()]));
                }
            }
            artifact.resolved_aux_inputs = Some(inputs);
            parsed.insert(key, artifact.clone());
            value = Observation::Present(artifact);
            break;
        }
        watches.sort();
        watches.dedup();
        watched_candidates.insert(logical.clone(), watches);
        artifacts.push((logical, value));
    }
    AcquiredArtifacts {
        artifacts,
        candidates: watched_candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smol_str::SmolStr;
    use tex_ls_analysis::project::aux::{AuxData, parse_aux};

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
    fn caption_xref_does_not_turn_the_page_into_a_reference_number() {
        let data =
            parse_aux("\\newlabel{fig:qux}{{\\caption@xref {fig:qux}{ on input line 15}}{1}}\n")
                .data;
        assert!(!data.labels.contains_key("fig:qux"));
        assert_eq!(data.pages[&SmolStr::new("fig:qux")], "1");
    }

    #[test]
    fn empty_number_keeps_a_page_without_inventing_a_reference_number() {
        let data = parse_aux("\\newlabel{x}{{}{2}}\n").data;
        assert!(!data.labels.contains_key("x"));
        assert_eq!(data.pages[&SmolStr::new("x")], "2");
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

    #[cfg(unix)]
    #[test]
    fn physical_payload_log_alias_does_not_suppress_later_aux_children() {
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("physical.bin");
        write(&actual, "\\newlabel{main}{{1}{1}}\\@input{child.aux}");
        let child = dir.path().canonicalize().unwrap().join("child.aux");
        write(&child, "\\newlabel{child}{{2}{1}}");
        let log = dir.path().join("main.log");
        let aux = dir.path().join("main.aux");
        for alias in [&log, &aux] {
            std::os::unix::fs::symlink(&actual, alias).unwrap();
        }
        let before = COMPILER_READS.with(std::cell::Cell::get);
        let artifacts = acquire_artifacts(vec![
            (log.clone(), vec![log.clone()]),
            (aux.clone(), vec![aux.clone()]),
        ]);
        assert_eq!(artifacts.len(), 3, "one child is acquired exactly once");
        assert_eq!(
            COMPILER_READS.with(std::cell::Cell::get) - before,
            2,
            "one physical parent read plus one child read"
        );
        let present = |path: &Path| match &artifacts.iter().find(|(key, _)| key == path).unwrap().1
        {
            Observation::Present(artifact) => artifact,
            _ => panic!("artifact present"),
        };
        assert!(present(&log).parsed.data.labels.is_empty());
        assert_eq!(present(&aux).parsed.data.labels[&SmolStr::new("main")], "1");
        assert_eq!(
            present(&aux).resolved_aux_inputs.as_deref(),
            Some(std::slice::from_ref(&child))
        );
        assert_eq!(
            present(&child).parsed.data.labels[&SmolStr::new("child")],
            "2"
        );
    }

    #[cfg(unix)]
    #[test]
    fn physical_recorder_aliases_preserve_logical_fallback_directories() {
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual.fls");
        write(&actual, "INPUT relative.tex\n");
        let first = dir.path().join("first/main.fls");
        let second = dir.path().join("second/main.fls");
        for alias in [&first, &second] {
            std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&actual, alias).unwrap();
        }
        let artifacts = acquire_artifacts(vec![
            (first.clone(), vec![first.clone()]),
            (second.clone(), vec![second.clone()]),
        ]);
        for (logical, value) in artifacts {
            let Observation::Present(artifact) = value else {
                panic!("artifact present");
            };
            let recorder = artifact.recorder.unwrap();
            assert_eq!(recorder.directory, logical.parent().unwrap());
            assert_eq!(
                recorder.inputs,
                vec![logical.parent().unwrap().join("relative.tex")]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn physical_compiler_aliases_read_payload_once_per_acquisition() {
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual.aux");
        write(&actual, "\\newlabel{main}{{1}{1}}");
        let first = dir.path().join("first.aux");
        let second = dir.path().join("second.aux");
        for alias in [&first, &second] {
            std::os::unix::fs::symlink(&actual, alias).unwrap();
        }
        let before = COMPILER_READS.with(std::cell::Cell::get);
        let artifacts = acquire_artifacts(vec![
            (first.clone(), vec![first]),
            (second.clone(), vec![second]),
        ]);
        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            COMPILER_READS.with(std::cell::Cell::get) - before,
            1,
            "physical identity reuse must avoid a repeated payload read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_aux_symlink_cycles_have_bounded_logical_expansion() {
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("main.aux");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        write(&actual, "\\newlabel{main}{{1}{1}}\\@input{sub/main.aux}");
        std::os::unix::fs::symlink(&actual, dir.path().join("sub/main.aux")).unwrap();
        let artifacts = acquire_artifacts(vec![(actual.clone(), vec![actual])]);
        assert!(
            artifacts.len() <= 3,
            "a physical cycle must not create ever-deeper logical aliases"
        );
        assert_eq!(
            artifacts
                .iter()
                .filter(|(_, value)| matches!(value, Observation::Present(_)))
                .count(),
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn compiler_watches_preserve_native_physical_alias_paths() {
        let dir = tempfile::tempdir().unwrap();
        for (index, name) in crate::test_native_paths::native_names(
            dir.path(),
            "physical-λ.aux",
            b"physical\xff.aux",
        )
        .into_iter()
        .enumerate()
        {
            let main = dir.path().join(format!("main-{index}.tex"));
            let actual = dir.path().join(name);
            write(&actual, "\\newlabel{key}{{1}{1}}");
            std::os::unix::fs::symlink(&actual, main.with_extension("aux")).unwrap();
            let mut db = IncrementalDatabase::default();
            db.apply_change(&main, "\\label{key}", None);
            let project = db.project_id();
            let watches = acquire_compiler(&mut db, project, &main, &BuildConfig::default());
            assert!(
                watches.contains(&actual.canonicalize().unwrap()),
                "the physical build artifact remains observable"
            );
            if actual.to_str().is_none() {
                assert!(!watches.contains(&PathBuf::from(actual.to_string_lossy().into_owned())));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn distinct_native_aux_paths_do_not_share_compiler_identity() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let mut names = vec![(
            std::ffi::OsString::from("part-λ.tex"),
            std::ffi::OsString::from("part-μ.tex"),
        )];
        if crate::test_native_paths::supports_non_utf8_names(dir.path()) {
            names.push((
                std::ffi::OsString::from_vec(b"part\xff.tex".to_vec()),
                std::ffi::OsString::from_vec(b"part\xfe.tex".to_vec()),
            ));
        }
        for (first, second) in names {
            let first = dir.path().join(first);
            let second = dir.path().join(second);
            write(&first.with_extension("aux"), "\\newlabel{first}{{1}{1}}");
            write(&second.with_extension("aux"), "\\newlabel{second}{{2}{1}}");
            let mut db = IncrementalDatabase::default();
            db.apply_change(&first, "text", None);
            db.apply_change(&second, "text", None);
            let artifacts = acquire_artifacts(vec![
                (
                    first.with_extension("aux"),
                    vec![first.with_extension("aux")],
                ),
                (
                    second.with_extension("aux"),
                    vec![second.with_extension("aux")],
                ),
            ]);
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
                .unwrap();
            db.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))
                .unwrap();
            let snapshot = db.snapshot();
            let data = snapshot.aux_data(&[&first, &second], dir.path()).unwrap();
            assert_eq!(
                data.labels.len(),
                2,
                "distinct physical artifacts must not deduplicate lossily"
            );
            assert_eq!(data.labels["first"], "1");
            assert_eq!(data.labels["second"], "2");
        }
    }

    #[cfg(unix)]
    #[test]
    fn sibling_logical_aux_aliases_keep_separate_physical_child_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.tex");
        let second = dir.path().join("second.tex");
        for (source, number) in [(&first, "1"), (&second, "2")] {
            let physical = dir.path().join(format!("physical-{number}"));
            std::fs::create_dir(&physical).unwrap();
            let actual = physical.join("main.aux");
            write(&actual, "\\@input{child.aux}");
            write(
                &physical.join("child.aux"),
                &format!("\\newlabel{{child}}{{{{{number}}}{{1}}}}"),
            );
            std::os::unix::fs::symlink(actual, source.with_extension("aux")).unwrap();
        }
        let mut db = IncrementalDatabase::default();
        db.apply_change(&first, "text", None);
        db.apply_change(&second, "text", None);
        let artifacts = acquire_artifacts(vec![
            (
                first.with_extension("aux"),
                vec![first.with_extension("aux")],
            ),
            (
                second.with_extension("aux"),
                vec![second.with_extension("aux")],
            ),
        ]);
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))
            .unwrap();
        let snapshot = db.snapshot();
        for (source, expected) in [(&first, "1"), (&second, "2")] {
            let data = snapshot.aux_data(&[source], dir.path()).unwrap();
            assert_eq!(data.labels.get("child").map(String::as_str), Some(expected));
        }
    }

    #[cfg(unix)]
    #[test]
    fn nested_aux_chains_preserve_native_directories_and_logical_aliases() {
        let dir = tempfile::tempdir().unwrap();
        for (index, name) in
            crate::test_native_paths::native_names(dir.path(), "physical-λ", b"physical\xff")
                .into_iter()
                .enumerate()
        {
            let physical = dir.path().join(name);
            std::fs::create_dir(&physical).unwrap();
            let physical_main = physical.join("main.aux");
            write(
                &physical_main,
                "\\newlabel{main}{{1}{1}}\\@input{child.aux}",
            );
            write(&physical.join("child.aux"), "\\newlabel{child}{{2}{1}}");
            let first = dir.path().join(format!("first-{index}/main.tex"));
            let second = dir.path().join(format!("second-{index}/main.tex"));
            for main in [&first, &second] {
                std::fs::create_dir_all(main.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&physical_main, main.with_extension("aux")).unwrap();
            }
            let mut db = IncrementalDatabase::default();
            db.apply_change(&first, "text", None);
            db.apply_change(&second, "text", None);
            let artifacts = acquire_artifacts(vec![
                (
                    first.with_extension("aux"),
                    vec![first.with_extension("aux")],
                ),
                (
                    second.with_extension("aux"),
                    vec![second.with_extension("aux")],
                ),
            ]);
            let token = db
                .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
                .unwrap();
            db.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))
                .unwrap();
            let snapshot = db.snapshot();
            for main in [&first, &second] {
                let data = snapshot.aux_data(&[main], dir.path()).unwrap();
                assert_eq!(
                    data.labels.get("child").map(String::as_str),
                    Some("2"),
                    "nested physical AUX facts remain reachable for each alias"
                );
            }
        }
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
    fn texe_output_is_found_for_an_entry_below_its_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("src/main.tex");
        write(&main, "x\n");
        write(
            &dir.path().join("src/main.aux"),
            "\\newlabel{sec:one}{{STALE}{1}}\n",
        );
        write(
            &dir.path().join(".texe/build/output/main.aux"),
            "\\newlabel{sec:one}{{2}{1}}\n",
        );
        assert_eq!(
            Fixture
                .data_for(&[&main], &dir.path().join("src"), None)
                .unwrap()
                .labels[&SmolStr::new("sec:one")],
            "2"
        );
    }

    #[test]
    fn shipout_and_texe_output_directories_are_found_without_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.tex");
        write(&main, "x\n");
        write(
            &dir.path().join("main.aux"),
            "\\newlabel{sec:one}{{STALE}{1}}\n",
        );
        assert_eq!(
            Fixture.data_for(&[&main], dir.path(), None).unwrap().labels[&SmolStr::new("sec:one")],
            "STALE"
        );
        write(
            &dir.path().join(".texe/build/output/main.aux"),
            "\\newlabel{sec:one}{{2}{1}}\n",
        );
        assert_eq!(
            Fixture.data_for(&[&main], dir.path(), None).unwrap().labels[&SmolStr::new("sec:one")],
            "2"
        );
        write(
            &dir.path().join(".shipout/main.aux"),
            "\\newlabel{sec:one}{{3}{1}}\n",
        );
        assert_eq!(
            Fixture.data_for(&[&main], dir.path(), None).unwrap().labels[&SmolStr::new("sec:one")],
            "3"
        );
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
