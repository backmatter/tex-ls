//! Native acquisition. All I/O finishes before the feature snapshot is captured.
#[cfg(test)]
use super::file_acquisition;
#[cfg(test)]
use crate::texmf::InstalledPackages;
use lsp_types::Position;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::{collections::HashSet, sync::Arc};
use tex_ls_analysis::{
    external::{
        DirectoryContents, LocationKind, LocationObservation, Observation, completion_directories,
    },
    incremental::Analysis,
    source::file_kind_or_tex,
    text::PositionEncoding,
};
#[cfg(test)]
use tex_ls_analysis::{
    external::{ExternalInputKind, ExternalInputs, FileInputs, InstalledMetadata},
    incremental::{IncrementalDatabase, ProjectId},
    text::IntoSourceText,
};

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
                    match entry.and_then(|entry| {
                        let Ok(name) = entry.file_name().into_string() else {
                            // Completion edits cannot name non-UTF8 entries.
                            return Ok(None);
                        };
                        let kind = entry.file_type()?;
                        let directory = if kind.is_symlink() {
                            // Match location probes, which follow symbolic links.
                            // Only links need another metadata lookup.
                            std::fs::metadata(entry.path())?.is_dir()
                        } else {
                            kind.is_dir()
                        };
                        Ok(Some((name, directory)))
                    }) {
                        Ok(Some((name, kind))) => {
                            contents.entries.insert(
                                name,
                                if kind {
                                    LocationKind::Directory
                                } else {
                                    LocationKind::File
                                },
                            );
                        }
                        Ok(None) => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_protocol::{compute_completion, convert::path_to_uri};

    #[cfg(unix)]
    #[test]
    fn unicode_native_entries_remain_available_in_directory_listings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("directory💡")).unwrap();
        std::fs::write(dir.path().join("source💡.tex"), "text").unwrap();
        std::os::unix::fs::symlink(dir.path().join("directory💡"), dir.path().join("link💡"))
            .unwrap();
        let Observation::Present(contents) = location(dir.path(), true).directory else {
            panic!("directory listing is available");
        };
        assert!(contents.complete);
        assert_eq!(contents.entries.len(), 3);
        assert_eq!(
            contents.entries.get("source💡.tex"),
            Some(&LocationKind::File)
        );
        for name in ["directory💡", "link💡"] {
            assert_eq!(contents.entries.get(name), Some(&LocationKind::Directory));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unrepresentable_broken_symlink_does_not_invalidate_utf8_listing() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        if !crate::test_native_paths::supports_non_utf8_names(dir.path()) {
            return;
        }
        let link = dir
            .path()
            .join(std::ffi::OsString::from_vec(b"bad\xff".to_vec()));
        std::os::unix::fs::symlink(dir.path().join("missing"), link).unwrap();
        std::fs::write(dir.path().join("valid.tex"), "text").unwrap();
        let Observation::Present(contents) = location(dir.path(), true).directory else {
            panic!("directory listing is available");
        };
        assert!(
            contents.complete,
            "unrepresentable entries do not affect the UTF8 listing"
        );
        assert_eq!(
            contents
                .entries
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["valid.tex"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_completion_skips_unrepresentable_native_names() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        if !crate::test_native_paths::supports_non_utf8_names(dir.path()) {
            return;
        }
        for name in [b"invalid\xff.tex".to_vec(), b"valid\xfe.tex".to_vec()] {
            std::fs::create_dir(dir.path().join(std::ffi::OsString::from_vec(name))).unwrap();
        }
        let valid = "valid\u{fffd}.tex";
        std::fs::write(dir.path().join(valid), "text").unwrap();
        let Observation::Present(contents) = location(dir.path(), true).directory else {
            panic!("directory listing is available");
        };
        assert!(contents.complete);
        assert_eq!(
            contents.entries.len(),
            1,
            "lossy names must not become completion targets"
        );
        assert_eq!(contents.entries.get(valid), Some(&LocationKind::File));
    }

    #[cfg(unix)]
    #[test]
    fn path_completion_offers_symbolic_links_to_directories() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let path = dir.path().join("main.tex");
        let source = r"\input{li}";
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let project = db.project_id();
        let installed = InstalledPackages::from(crate::texmf::TexmfConfig {
            enabled: false,
            ..Default::default()
        });
        let position = Position::new(0, (source.len() - 1) as u32);
        acquire_files(
            &mut db,
            project,
            &path,
            Some((position, PositionEncoding::Utf16)),
            &installed,
            false,
        );
        let snapshot = db.snapshot();
        let items = compute_completion(
            &snapshot,
            &path_to_uri(&path).unwrap(),
            &path,
            PositionEncoding::Utf16,
            position,
        );
        let item = items
            .items
            .iter()
            .find(|item| item.label == "linked")
            .expect("a linked directory is a path completion candidate");
        assert_eq!(item.kind, Some(lsp_types::CompletionItemKind::Folder));
        let Some(lsp_types::CompletionItemTextEdit::TextEdit(edit)) = &item.text_edit else {
            panic!("directory completion supplies a text edit");
        };
        assert_eq!(edit.new_text, "linked/");
    }

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
}
