//! Host project selection from explicit associations and registered roots.
use std::path::{Path, PathBuf};
use tex_ls_analysis::{
    incremental::{IncrementalDatabase, ProjectId},
    source::normalize_path,
};

pub struct ProjectRegistry {
    fallback: ProjectId,
    associations: std::collections::HashMap<PathBuf, ProjectId>,
    roots: Vec<(PathBuf, ProjectId)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSelectionError {
    UnknownProject,
    AmbiguousRoot,
}

impl std::fmt::Display for ProjectSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownProject => "The associated project is not registered",
            Self::AmbiguousRoot => {
                "Multiple projects share this root; specify a project association"
            }
        })
    }
}
impl std::error::Error for ProjectSelectionError {}

impl ProjectRegistry {
    pub fn from_roots(db: &mut IncrementalDatabase, roots: &[PathBuf]) -> Self {
        let mut registry = Self::new(db.project_id());
        let mut roots: Vec<_> = roots.iter().map(|root| normalize_path(root)).collect();
        roots.sort();
        roots.dedup();
        for root in roots {
            let project = db.create_project(db.declarations().clone());
            registry.register(&root, project);
        }
        registry
    }

    pub fn new(fallback: ProjectId) -> Self {
        Self {
            fallback,
            associations: Default::default(),
            roots: Vec::new(),
        }
    }

    /// The most specific registered boundary for bounded native discovery.
    pub fn root_for(&self, path: &Path) -> Option<&Path> {
        self.roots
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(root, _)| root.as_path())
    }

    pub fn register(&mut self, root: &Path, project: ProjectId) {
        let entry = (normalize_path(root), project);
        if !self.roots.contains(&entry) {
            self.roots.push(entry);
        }
    }

    /// Change workspace boundaries, reattaching live overlays before retiring
    /// removed projects. Unchanged roots retain their caches and observations.
    pub fn update_roots(
        &mut self,
        db: &mut IncrementalDatabase,
        roots: &[PathBuf],
        open: &[PathBuf],
    ) {
        let overlays: Vec<_> = open
            .iter()
            .filter_map(|path| {
                let old = self.select(path, None).ok()?;
                let snapshot = db.snapshot_for(old).ok()?;
                let file = snapshot.lookup_file(path)?;
                Some((
                    path.clone(),
                    old,
                    snapshot.source_text(file).clone(),
                    snapshot.declarations_for(path).clone(),
                ))
            })
            .collect();
        let roots: std::collections::BTreeSet<_> =
            roots.iter().map(|root| normalize_path(root)).collect();
        let removed: Vec<_> = self
            .roots
            .iter()
            .filter(|(root, _)| !roots.contains(root))
            .map(|(_, id)| *id)
            .collect();
        self.roots.retain(|(root, _)| roots.contains(root));
        self.associations.retain(|_, id| !removed.contains(id));
        for root in roots {
            if !self.roots.iter().any(|(existing, _)| *existing == root) {
                let project = db.create_project(db.declarations().clone());
                self.register(&root, project);
            }
        }
        for (path, old, text, declarations) in overlays {
            let new = self.select(&path, None).expect("unique workspace roots");
            if old != new {
                db.set_project_source_declarations(new, &path, declarations)
                    .expect("registered project");
                db.replace_overlay(new, &path, text)
                    .expect("registered project");
                db.close_overlay(old, &path)
                    .expect("old project is still live");
            }
        }
        for project in removed {
            db.remove_project(project);
        }
    }

    pub fn rename_files(&mut self, db: &mut IncrementalDatabase, files: &[(PathBuf, PathBuf)]) {
        // Associations belong to sources, including the absence of an explicit
        // association. Capture every destination before clearing either side.
        let associations: std::collections::BTreeMap<_, _> = files
            .iter()
            .map(|(old, new)| {
                (
                    normalize_path(new),
                    self.associations.get(&normalize_path(old)).copied(),
                )
            })
            .collect();
        let projects: std::collections::HashMap<_, _> = files
            .iter()
            .filter_map(|(old, new)| {
                let from = self.select(old, None).ok()?;
                let to = match associations[&normalize_path(new)] {
                    Some(project) => self.select(new, Some(project)),
                    None => self.select_root(new),
                }
                .ok()?;
                Some(((old.as_path(), new.as_path()), (from, to)))
            })
            .collect();
        let declarations: Vec<_> = files
            .iter()
            .filter_map(|(old, new)| {
                let &(from, to) = projects.get(&(old.as_path(), new.as_path()))?;
                let snapshot = db.snapshot_for(from).ok()?;
                snapshot.lookup_file(old)?;
                // Inherited defaults must keep following this project. Across
                // projects retain the effective settings, as before the move.
                let declared = if from == to {
                    snapshot.source_declaration_override(old).cloned()
                } else {
                    Some(snapshot.declarations_for(old).clone())
                };
                Some((new.clone(), to, declared))
            })
            .collect();
        db.rename_sources_between(files, |old, new| projects.get(&(old, new)).copied());
        for (old, _) in files {
            self.associations.remove(&normalize_path(old));
        }
        for (new, project) in associations {
            match project {
                Some(project) => {
                    self.associations.insert(new, project);
                }
                None => {
                    self.associations.remove(&new);
                }
            }
        }
        for (new, project, declarations) in declarations {
            match declarations {
                Some(declarations) => {
                    db.set_project_source_declarations(project, &new, declarations)
                        .expect("registered project");
                }
                None => {
                    // An inherited source can overwrite an explicitly configured
                    // destination. Its old metadata must not survive that move.
                    db.clear_project_source_declarations(project, &new)
                        .expect("registered project");
                }
            }
        }
        use tex_ls_analysis::external::{
            ExternalInputKind, ExternalInputs, FileInputs, LocationKind, LocationObservation,
            Observation,
        };
        for project in self.projects() {
            let locations = files
                .iter()
                .map(|(old, _)| {
                    (
                        old.clone(),
                        LocationObservation {
                            kind: Observation::Absent,
                            directory: Observation::Unknown,
                        },
                    )
                })
                .chain(files.iter().map(|(_, new)| {
                    (
                        new.clone(),
                        LocationObservation {
                            kind: Observation::Present(LocationKind::File),
                            directory: Observation::Unknown,
                        },
                    )
                }))
                .collect();
            let token = db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .expect("registered project");
            db.apply_external_inputs(
                token,
                ExternalInputs::Files(FileInputs {
                    locations,
                    ..Default::default()
                }),
            )
            .expect("current file move");
        }
    }

    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|(root, _)| root.clone()).collect()
    }

    pub fn associate(
        &mut self,
        path: &Path,
        project: ProjectId,
    ) -> Result<(), ProjectSelectionError> {
        self.select(path, Some(project))?;
        self.associations.insert(normalize_path(path), project);
        Ok(())
    }

    pub fn replace(&mut self, old: ProjectId, new: ProjectId) {
        if self.fallback == old {
            self.fallback = new;
        }
        for (_, project) in &mut self.roots {
            if *project == old {
                *project = new;
            }
        }
        self.associations.retain(|_, project| *project != old);
    }

    pub fn projects(&self) -> impl Iterator<Item = ProjectId> + '_ {
        std::iter::once(self.fallback).chain(self.roots.iter().map(|(_, id)| *id))
    }

    pub fn select(
        &self,
        path: &Path,
        association: Option<ProjectId>,
    ) -> Result<ProjectId, ProjectSelectionError> {
        if let Some(project) =
            association.or_else(|| self.associations.get(&normalize_path(path)).copied())
        {
            return self
                .projects()
                .any(|id| id == project)
                .then_some(project)
                .ok_or(ProjectSelectionError::UnknownProject);
        }
        self.select_root(path)
    }

    fn select_root(&self, path: &Path) -> Result<ProjectId, ProjectSelectionError> {
        let path = normalize_path(path);
        let mut selected = None;
        let mut ambiguous = false;
        for (root, project) in &self.roots {
            if !path.starts_with(root) {
                continue;
            }
            let depth = root.components().count();
            match selected {
                Some((previous_depth, previous)) if depth == previous_depth => {
                    ambiguous |= previous != *project;
                }
                Some((previous_depth, _)) if depth < previous_depth => {}
                _ => {
                    selected = Some((depth, *project));
                    ambiguous = false;
                }
            }
        }
        if ambiguous {
            return Err(ProjectSelectionError::AmbiguousRoot);
        }
        Ok(selected.map_or(self.fallback, |(_, project)| project))
    }
}

pub fn workspace_roots(init_params: &serde_json::Value) -> Vec<PathBuf> {
    let folders = init_params
        .get("workspaceFolders")
        .and_then(serde_json::Value::as_array)
        .map(|folders| {
            folders
                .iter()
                .filter_map(|folder| folder.get("uri"))
                .filter_map(serde_json::Value::as_str)
                .filter_map(|uri| uri.parse::<lsp_types::Uri>().ok())
                .filter_map(|uri| crate::uri_to_fs_path(&uri))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !folders.is_empty() {
        return folders;
    }
    init_params
        .get("rootUri")
        .and_then(serde_json::Value::as_str)
        .and_then(|uri| uri.parse::<lsp_types::Uri>().ok())
        .and_then(|uri| crate::uri_to_fs_path(&uri))
        .into_iter()
        .collect()
}

/// Explain the current source-only root interpretation without performing IO.
/// UTF-8 paths retain their native spelling. Other absolute native paths use
/// percent-encoded file URIs; paths that cannot form a URI return an error object.
pub fn inspect(
    snapshot: &tex_ls_analysis::incremental::Analysis,
    path: &Path,
) -> serde_json::Value {
    inspect_value(snapshot, path).unwrap_or_else(
        |error| serde_json::json!({"error": error, "uri": crate::path_to_uri(path)}),
    )
}

fn inspection_path(path: &Path) -> Result<std::borrow::Cow<'_, str>, &'static str> {
    if let Some(path) = path.to_str() {
        return Ok(std::borrow::Cow::Borrowed(path));
    }
    crate::path_to_uri(path)
        .map(|uri| std::borrow::Cow::Owned(uri.to_string()))
        .ok_or("Cannot represent this native path as a file URI")
}

fn inspection_paths<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<Vec<std::borrow::Cow<'a, str>>, &'static str> {
    paths.into_iter().map(inspection_path).collect()
}

fn inspect_value(
    snapshot: &tex_ls_analysis::incremental::Analysis,
    path: &Path,
) -> Result<serde_json::Value, &'static str> {
    let labels = snapshot.resolve_labels();
    let candidates = labels.candidate_roots(path);
    let root = if let [root] = candidates {
        root.as_path()
    } else {
        path
    };
    let view = snapshot.project_graph().interpret(root);
    let edge_value = |edge: &tex_ls_analysis::project::graph::RootEdge| {
        Ok::<_, &'static str>(serde_json::json!({
            "from": inspection_path(&edge.from)?, "to": edge.target.as_deref().map(inspection_path).transpose()?,
            "kind": format!("{:?}", edge.kind), "buildParticipation": edge.build_participation,
        }))
    };
    let edges = view
        .edges
        .iter()
        .filter(|edge| edge.resolved)
        .map(edge_value)
        .collect::<Result<Vec<_>, _>>()?;
    let unresolved = view
        .edges
        .iter()
        .filter(|edge| !edge.resolved)
        .map(edge_value)
        .collect::<Result<Vec<_>, _>>()?;
    let hints = snapshot
        .lookup_file(path)
        .map(|file| tex_ls_analysis::external::explicit_root_hints(snapshot.file_text(file), path))
        .unwrap_or_default();
    let directories = view
        .contexts
        .get(path)
        .map(|paths| inspection_paths(paths.iter().map(PathBuf::as_path)))
        .transpose()?;
    let compiler = snapshot
        .compiler_artifacts()
        .filter_map(|(path, value)| {
            if let tex_ls_analysis::external::Observation::Present(artifact) = value {
                artifact.recorder.as_ref().map(|recorder| {
                Ok::<_, &'static str>(serde_json::json!({
                    "artifact": inspection_path(path)?, "identity": artifact.identity,
                    "recorder": {
                        "directory": inspection_path(&recorder.directory)?,
                        "inputs": inspection_paths(recorder.inputs.iter().map(PathBuf::as_path))?,
                        "outputs": inspection_paths(recorder.outputs.iter().map(PathBuf::as_path))?,
                    }
                }))
            })
            } else {
                None
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({
        "path": inspection_path(path)?,
        "candidateRoots": inspection_paths(candidates.iter().map(PathBuf::as_path))?,
        "rootHints": inspection_paths(hints.iter().map(PathBuf::as_path))?,
        "ambiguous": !labels.has_unique_root(path),
        "members": inspection_paths(labels.namespace_members(path))?,
        "searchDirectories": directories,
        "closed": view.is_closed(), "edges": edges, "unresolved": unresolved,
        "compilerObservations": compiler,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[cfg(unix)]
    #[test]
    fn inspect_reports_relative_non_utf8_path_error_without_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"main\xff.tex".to_vec()));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, "text", None);
        let result = inspect(&db.snapshot(), &path);
        assert_eq!(
            result["error"],
            "Cannot represent this native path as a file URI"
        );
        assert!(result["uri"].is_null());
    }

    #[test]
    fn inspect_keeps_dynamic_unresolved_target_null() {
        let path = Path::new("/project/main.tex");
        let mut db = IncrementalDatabase::default();
        db.apply_change(path, "\\input{\\dynamic}", None);
        let result = inspect(&db.snapshot(), path);
        assert_eq!(result["path"], path.to_str().unwrap());
        assert_eq!(result["unresolved"].as_array().unwrap().len(), 1);
        assert!(result["unresolved"][0]["to"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn inspect_preserves_non_utf8_path_identity_as_file_uris() {
        use std::os::unix::ffi::OsStringExt;
        use tex_ls_analysis::external::{
            CompilerArtifact, ExternalInputKind, ExternalInputs, Observation, compiler::Recorder,
        };
        let mut db = IncrementalDatabase::default();
        let path =
            Path::new("/project").join(std::ffi::OsString::from_vec(b"main\xff.tex".to_vec()));
        let child = Path::new("/project/child.tex");
        db.apply_change(&path, "\\input{child}", None);
        db.apply_change(child, "text", None);
        let artifact =
            Path::new("/project").join(std::ffi::OsString::from_vec(b"main\xfe.fls".to_vec()));
        let mut value = CompilerArtifact::from_text(&artifact, "", "native".into());
        value.recorder = Some(Recorder {
            directory: path.parent().unwrap().to_owned(),
            inputs: vec![path.clone(), child.to_owned()],
            outputs: vec![artifact.clone()],
        });
        let token = db
            .begin_external_refresh(db.project_id(), ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Compiler(vec![(artifact.clone(), Observation::Present(value))]),
        )
        .unwrap();
        let result = inspect(&db.snapshot(), &path);
        let uri = crate::path_to_uri(&path).unwrap().to_string();
        assert_eq!(result["path"], uri);
        assert!(
            result["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member == &uri)
        );
        assert!(
            result["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member == child.to_str().unwrap())
        );
        let compiler = &result["compilerObservations"][0];
        assert_eq!(
            compiler["artifact"],
            crate::path_to_uri(&artifact).unwrap().to_string()
        );
        assert_eq!(compiler["recorder"]["inputs"][0], uri);
        assert_eq!(compiler["recorder"]["inputs"][1], child.to_str().unwrap());
        assert_eq!(
            compiler["recorder"]["outputs"][0],
            crate::path_to_uri(&artifact).unwrap().to_string()
        );
    }

    #[test]
    fn renamed_inherited_source_follows_later_project_declarations() {
        let quote = serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
            serde_json::json!({"environments":{"custom":{"like":"quote"}}}),
        )
        .unwrap()
        .resolve()
        .unwrap();
        let math = serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
            serde_json::json!({"environments":{"custom":{"like":"equation"}}}),
        )
        .unwrap()
        .resolve()
        .unwrap();
        let root = PathBuf::from(fixture_path!("/project"));
        let old = root.join("main.tex");
        let new = root.join("renamed.tex");
        let source = "\\begin{custom}\nx_i\n\\end{custom}\n";
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[root]);
        let project = registry.select(&old, None).unwrap();
        db.set_project_declarations(project, quote.clone()).unwrap();
        db.replace_overlay(project, &old, source).unwrap();
        registry.rename_files(&mut db, &[(old.clone(), new.clone())]);
        assert_eq!(registry.select(&new, None).unwrap(), project);
        db.set_project_declarations(project, math.clone()).unwrap();
        let snapshot = db.snapshot_for(project).unwrap();
        assert!(snapshot.lookup_file(&old).is_none());
        let file = snapshot.lookup_file(&new).unwrap();
        assert_eq!(snapshot.file_text(file), source);
        let full = tex_ls_parser::parser::parse_with_declarations_resolved(
            source,
            tex_ls_parser::parser::LexConfig::default(),
            &math.parse_tier(),
        )
        .0;
        assert_eq!(snapshot.parsed_tree(file).green(), &*full.green);
        assert!(full.errors.is_empty());
        assert!(snapshot.parse_diagnostics(file).is_empty());
        assert_eq!(snapshot.declarations_for(&new), &math);
    }

    #[test]
    fn renamed_explicit_source_override_survives_project_reconfiguration() {
        let math = serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
            serde_json::json!({"environments":{"custom":{"like":"equation"}}}),
        )
        .unwrap()
        .resolve()
        .unwrap();
        let quote = serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
            serde_json::json!({"environments":{"custom":{"like":"quote"}}}),
        )
        .unwrap()
        .resolve()
        .unwrap();
        let root = PathBuf::from(fixture_path!("/project"));
        let old = root.join("main.tex");
        let new = root.join("renamed.tex");
        let source = "\\begin{custom}\nx_i\n\\end{custom}\n";
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[root]);
        let project = registry.select(&old, None).unwrap();
        // Equal values still have distinct ownership: this explicit setting
        // must survive a later project-default change.
        db.set_project_declarations(project, math.clone()).unwrap();
        db.set_project_source_declarations(project, &old, math.clone())
            .unwrap();
        db.replace_overlay(project, &old, source).unwrap();
        registry.rename_files(&mut db, &[(old, new.clone())]);
        db.set_project_declarations(project, quote).unwrap();
        let snapshot = db.snapshot_for(project).unwrap();
        let file = snapshot.lookup_file(&new).unwrap();
        let full = tex_ls_parser::parser::parse_with_declarations_resolved(
            source,
            tex_ls_parser::parser::LexConfig::default(),
            &math.parse_tier(),
        )
        .0;
        assert_eq!(snapshot.parsed_tree(file).green(), &*full.green);
        assert!(full.errors.is_empty());
        assert!(snapshot.parse_diagnostics(file).is_empty());
        assert_eq!(snapshot.declarations_for(&new), &math);
    }

    #[test]
    fn inherited_rename_overwrites_destination_override_without_freezing_defaults() {
        let declarations = |like: &str| {
            serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
                serde_json::json!({"environments":{"custom":{"like":like}}}),
            )
            .unwrap()
            .resolve()
            .unwrap()
        };
        let quote = declarations("quote");
        let math = declarations("equation");
        let root = PathBuf::from(fixture_path!("/project"));
        let old = root.join("main.tex");
        let new = root.join("destination.tex");
        let source = "Inherited.\n\\begin{custom}\nx_i\n\\end{custom}\n";
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[root]);
        let project = registry.select(&old, None).unwrap();
        db.set_project_declarations(project, quote.clone()).unwrap();
        db.replace_overlay(project, &old, source).unwrap();
        db.set_project_source_declarations(project, &new, math.clone())
            .unwrap();
        db.replace_overlay(project, &new, "Destination.").unwrap();
        registry.rename_files(&mut db, &[(old, new.clone())]);
        {
            let snapshot = db.snapshot_for(project).unwrap();
            assert_eq!(snapshot.declarations_for(&new), &quote);
            assert_eq!(
                snapshot.file_text(snapshot.lookup_file(&new).unwrap()),
                source
            );
        }
        db.set_project_declarations(project, math.clone()).unwrap();
        let snapshot = db.snapshot_for(project).unwrap();
        let file = snapshot.lookup_file(&new).unwrap();
        let full = tex_ls_parser::parser::parse_with_declarations_resolved(
            source,
            tex_ls_parser::parser::LexConfig::default(),
            &math.parse_tier(),
        )
        .0;
        assert_eq!(snapshot.parsed_tree(file).green(), &*full.green);
        assert!(full.errors.is_empty());
        assert!(snapshot.parse_diagnostics(file).is_empty());
        assert_eq!(snapshot.declarations_for(&new), &math);
    }

    #[test]
    fn declaration_provenance_follows_source_through_same_project_swap() {
        let declarations = |like: &str| {
            serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
                serde_json::json!({"environments":{"custom":{"like":like}}}),
            )
            .unwrap()
            .resolve()
            .unwrap()
        };
        let math = declarations("equation");
        let quote = declarations("quote");
        let root = PathBuf::from(fixture_path!("/project"));
        let a = root.join("a.tex");
        let b = root.join("b.tex");
        let inherited = "Inherited.\n\\begin{custom}\nx_i\n\\end{custom}\n";
        let explicit = "Explicit.\n\\begin{custom}\ny_i\n\\end{custom}\n";
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[root]);
        let project = registry.select(&a, None).unwrap();
        db.set_project_declarations(project, math.clone()).unwrap();
        db.replace_overlay(project, &a, inherited).unwrap();
        db.set_project_source_declarations(project, &b, math.clone())
            .unwrap();
        db.replace_overlay(project, &b, explicit).unwrap();
        registry.rename_files(&mut db, &[(a.clone(), b.clone()), (b.clone(), a.clone())]);
        db.set_project_declarations(project, quote.clone()).unwrap();
        let snapshot = db.snapshot_for(project).unwrap();
        for (path, source, declared) in [(&a, explicit, &math), (&b, inherited, &quote)] {
            let file = snapshot.lookup_file(path).unwrap();
            assert_eq!(snapshot.file_text(file), source);
            let full = tex_ls_parser::parser::parse_with_declarations_resolved(
                source,
                tex_ls_parser::parser::LexConfig::default(),
                &declared.parse_tier(),
            )
            .0;
            assert_eq!(snapshot.parsed_tree(file).green(), &*full.green);
            assert!(full.errors.is_empty());
            assert!(snapshot.parse_diagnostics(file).is_empty());
            assert_eq!(snapshot.declarations_for(path), declared);
        }
    }

    #[test]
    fn unassociated_cross_root_rename_retains_effective_declarations() {
        let declarations = |like: &str| {
            serde_json::from_value::<tex_ls_parser::declarations::Declarations>(
                serde_json::json!({"environments":{"custom":{"like":like}}}),
            )
            .unwrap()
            .resolve()
            .unwrap()
        };
        let quote = declarations("quote");
        let a = PathBuf::from(fixture_path!("/a"));
        let b = PathBuf::from(fixture_path!("/b"));
        let old = a.join("main.tex");
        let new = b.join("renamed.tex");
        let source = "\\begin{custom}\nx_i\n\\end{custom}\n";
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[a, b]);
        let from = registry.select(&old, None).unwrap();
        let to = registry.select(&new, None).unwrap();
        db.set_project_declarations(from, quote.clone()).unwrap();
        db.set_project_declarations(to, declarations("equation"))
            .unwrap();
        db.replace_overlay(from, &old, source).unwrap();
        registry.rename_files(&mut db, &[(old, new.clone())]);
        assert_eq!(registry.select(&new, None).unwrap(), to);
        let snapshot = db.snapshot_for(to).unwrap();
        let file = snapshot.lookup_file(&new).unwrap();
        assert_eq!(snapshot.file_text(file), source);
        assert_eq!(snapshot.declarations_for(&new), &quote);
        let full = tex_ls_parser::parser::parse_with_declarations_resolved(
            source,
            tex_ls_parser::parser::LexConfig::default(),
            &quote.parse_tier(),
        )
        .0;
        assert_eq!(snapshot.parsed_tree(file).green(), &*full.green);
        assert!(full.errors.is_empty());
        assert!(snapshot.parse_diagnostics(file).is_empty());
    }

    #[test]
    fn associated_source_cross_root_rename_keeps_selected_membership() {
        let a = PathBuf::from(fixture_path!("/a"));
        let b = PathBuf::from(fixture_path!("/b"));
        let old = a.join("main.tex");
        let new = b.join("renamed.tex");
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[a, b]);
        let project = registry.select(&old, None).unwrap();
        let destination_root = registry.select(&new, None).unwrap();
        assert_ne!(project, destination_root);
        registry.associate(&old, project).unwrap();
        db.replace_overlay(project, &old, "Unsaved source.\n")
            .unwrap();
        registry.rename_files(&mut db, &[(old.clone(), new.clone())]);
        assert_eq!(registry.select(&new, None).unwrap(), project);
        let snapshot = db.snapshot_for(project).unwrap();
        assert!(snapshot.lookup_file(&old).is_none());
        let file = snapshot
            .lookup_file(&new)
            .expect("selected project owns the moved source");
        assert_eq!(snapshot.file_text(file), "Unsaved source.\n");
    }

    #[test]
    fn associated_sources_keep_memberships_through_cross_root_swap() {
        let a_root = PathBuf::from(fixture_path!("/a"));
        let b_root = PathBuf::from(fixture_path!("/b"));
        let a = a_root.join("main.tex");
        let b = b_root.join("main.tex");
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[a_root, b_root]);
        let a_project = registry.select(&a, None).unwrap();
        let b_project = registry.select(&b, None).unwrap();
        registry.associate(&a, a_project).unwrap();
        registry.associate(&b, b_project).unwrap();
        db.replace_overlay(a_project, &a, "First unsaved source.\n")
            .unwrap();
        db.replace_overlay(b_project, &b, "Second unsaved source.\n")
            .unwrap();
        registry.rename_files(&mut db, &[(a.clone(), b.clone()), (b.clone(), a.clone())]);
        for (path, project, text) in [
            (&b, a_project, "First unsaved source.\n"),
            (&a, b_project, "Second unsaved source.\n"),
        ] {
            assert_eq!(registry.select(path, None).unwrap(), project);
            let snapshot = db.snapshot_for(project).unwrap();
            let file = snapshot
                .lookup_file(path)
                .expect("carried association owns the moved source");
            assert_eq!(snapshot.file_text(file), text);
        }
    }

    #[test]
    fn inherited_source_overwrite_retires_destination_association() {
        let a = PathBuf::from(fixture_path!("/a"));
        let b = PathBuf::from(fixture_path!("/b"));
        let old = a.join("main.tex");
        let new = b.join("destination.tex");
        let mut db = IncrementalDatabase::default();
        let mut registry = ProjectRegistry::from_roots(&mut db, &[a, b.clone()]);
        let old_project = registry.select(&old, None).unwrap();
        let destination_root = registry.select(&b, None).unwrap();
        registry.associate(&new, old_project).unwrap();
        db.replace_overlay(old_project, &old, "Inherited source.\n")
            .unwrap();
        db.replace_overlay(old_project, &new, "Associated destination.\n")
            .unwrap();
        registry.rename_files(&mut db, &[(old, new.clone())]);
        assert_eq!(registry.select(&new, None).unwrap(), destination_root);
        let snapshot = db.snapshot_for(destination_root).unwrap();
        let file = snapshot.lookup_file(&new).unwrap();
        assert_eq!(snapshot.file_text(file), "Inherited source.\n");
    }

    #[test]
    fn folder_replacement_preserves_unsaved_overlay() {
        let mut db = IncrementalDatabase::default();
        let root = PathBuf::from(fixture_path!("/project"));
        let path = root.join("main.tex");
        let mut registry = ProjectRegistry::from_roots(&mut db, std::slice::from_ref(&root));
        let old = registry.select(&path, None).unwrap();
        db.set_backing(
            old,
            &path,
            Some(std::sync::Arc::new(tex_ls_analysis::text::SourceText::new(
                "disk".into(),
            ))),
        )
        .unwrap();
        db.replace_overlay(old, &path, "unsaved").unwrap();
        registry.update_roots(&mut db, &[], std::slice::from_ref(&path));
        let new = registry.select(&path, None).unwrap();
        assert_ne!(old, new);
        let snapshot = db.snapshot_for(new).unwrap();
        assert_eq!(
            snapshot.file_text(snapshot.lookup_file(&path).unwrap()),
            "unsaved"
        );
        assert!(db.snapshot_for(old).is_err());
        drop(snapshot);
        registry.update_roots(&mut db, &[root], std::slice::from_ref(&path));
        let new = registry.select(&path, None).unwrap();
        let snapshot = db.snapshot_for(new).unwrap();
        assert_eq!(
            snapshot.file_text(snapshot.lookup_file(&path).unwrap()),
            "unsaved"
        );
    }

    #[test]
    fn roots_are_specific_and_associations_take_precedence() {
        let mut db = IncrementalDatabase::default();
        let outer = db.create_project(Default::default());
        let inner = db.create_project(Default::default());
        let mut registry = ProjectRegistry::new(db.project_id());
        registry.register(Path::new(fixture_path!("/work")), outer);
        registry.register(Path::new(fixture_path!("/work/nested")), inner);
        let path = Path::new(fixture_path!("/work/./nested/main.tex"));
        assert_eq!(registry.select(path, None), Ok(inner));
        assert_eq!(registry.select(path, Some(outer)), Ok(outer));
        assert_eq!(
            registry.select(Path::new(fixture_path!("/workbook/a.tex")), None),
            Ok(db.project_id())
        );
        registry.register(Path::new(fixture_path!("/work/nested")), outer);
        assert_eq!(
            registry.select(path, None),
            Err(ProjectSelectionError::AmbiguousRoot)
        );
        assert_eq!(registry.select(path, Some(inner)), Ok(inner));
        let unknown = db.create_project(Default::default());
        assert_eq!(
            registry.select(path, Some(unknown)),
            Err(ProjectSelectionError::UnknownProject)
        );
    }
}
