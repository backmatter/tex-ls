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
        let declarations: Vec<_> = files
            .iter()
            .filter_map(|(old, new)| {
                let from = self.select(old, None).ok()?;
                let to = self.select(new, None).ok()?;
                let snapshot = db.snapshot_for(from).ok()?;
                snapshot.lookup_file(old)?;
                Some((new.clone(), to, snapshot.declarations_for(old).clone()))
            })
            .collect();
        db.rename_sources(files, |path| self.select(path, None).ok());
        let associations: Vec<_> = files
            .iter()
            .filter_map(|(old, new)| {
                self.associations
                    .remove(old)
                    .map(|project| (new.clone(), project))
            })
            .collect();
        for (new, project, declarations) in declarations {
            db.set_project_source_declarations(project, &new, declarations)
                .expect("registered project");
        }
        self.associations.extend(associations);
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
pub fn inspect(
    snapshot: &tex_ls_analysis::incremental::Analysis,
    path: &Path,
) -> serde_json::Value {
    let labels = snapshot.resolve_labels();
    let candidates = labels.candidate_roots(path);
    let root = if let [root] = candidates {
        root.as_path()
    } else {
        path
    };
    let view = snapshot.project_graph().interpret(root);
    let edge_value = |edge: &tex_ls_analysis::project::graph::RootEdge| {
        serde_json::json!({
            "from": edge.from, "to": edge.target, "kind": format!("{:?}", edge.kind),
            "buildParticipation": edge.build_participation,
        })
    };
    let edges: Vec<_> = view
        .edges
        .iter()
        .filter(|edge| edge.resolved)
        .map(edge_value)
        .collect();
    let unresolved: Vec<_> = view
        .edges
        .iter()
        .filter(|edge| !edge.resolved)
        .map(edge_value)
        .collect();
    serde_json::json!({"path": path, "candidateRoots": candidates,
        "rootHints": snapshot.lookup_file(path).map(|file| tex_ls_analysis::external::explicit_root_hints(snapshot.file_text(file), path)).unwrap_or_default(),

        "ambiguous": !labels.has_unique_root(path), "members": labels.namespace_members(path),
        "searchDirectories": view.contexts.get(path),
        "closed": view.is_closed(), "edges": edges, "unresolved": unresolved,
        "compilerObservations": snapshot.compiler_artifacts().filter_map(|(path, value)| {
            if let tex_ls_analysis::external::Observation::Present(artifact) = value {
                artifact.recorder.as_ref().map(|recorder| serde_json::json!({"artifact": path, "identity":artifact.identity, "recorder":recorder}))
            } else { None }
        }).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

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
