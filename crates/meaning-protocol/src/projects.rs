//! Host project selection from explicit associations and registered roots.
use meaning_analysis::{
    incremental::{IncrementalDatabase, ProjectId},
    source::normalize_path,
};
use std::path::{Path, PathBuf};

pub struct ProjectRegistry {
    fallback: ProjectId,
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
            roots: Vec::new(),
        }
    }

    pub fn register(&mut self, root: &Path, project: ProjectId) {
        let entry = (normalize_path(root), project);
        if !self.roots.contains(&entry) {
            self.roots.push(entry);
        }
    }

    pub fn projects(&self) -> impl Iterator<Item = ProjectId> + '_ {
        std::iter::once(self.fallback).chain(self.roots.iter().map(|(_, id)| *id))
    }

    pub fn select(
        &self,
        path: &Path,
        association: Option<ProjectId>,
    ) -> Result<ProjectId, ProjectSelectionError> {
        if let Some(project) = association {
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

#[cfg(test)]
mod tests {
    use super::*;
    use meaning_analysis::incremental::IncrementalDatabase;

    #[test]
    fn roots_are_specific_and_associations_take_precedence() {
        let mut db = IncrementalDatabase::default();
        let outer = db.create_project(Default::default());
        let inner = db.create_project(Default::default());
        let mut registry = ProjectRegistry::new(db.project_id());
        registry.register(Path::new("/work"), outer);
        registry.register(Path::new("/work/nested"), inner);
        let path = Path::new("/work/./nested/main.tex");
        assert_eq!(registry.select(path, None), Ok(inner));
        assert_eq!(registry.select(path, Some(outer)), Ok(outer));
        assert_eq!(
            registry.select(Path::new("/workbook/a.tex"), None),
            Ok(db.project_id())
        );
        registry.register(Path::new("/work/nested"), outer);
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
