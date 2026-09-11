//! Backing contents and overlays are published through the same source writer.
use super::*;
use crate::source::normalize_path;
use database::SourceLayers;
use std::collections::HashSet;

impl IncrementalDatabase {
    /// Supply or remove backing contents for a project member. An open overlay
    /// remains authoritative until its last owner closes it.
    pub fn set_backing(
        &mut self,
        project: ProjectId,
        path: &Path,
        text: Option<Arc<SourceText>>,
    ) -> Result<Option<SourceId>, ChangeError> {
        self.set_backing_batch(project, [(path.to_path_buf(), text)])?;
        Ok(self.read.inner.files.get(&normalize_path(path)).copied())
    }

    /// Publish backing observations together, rebuilding storage at most once
    /// when the batch removes project memberships. The last value for a path wins.
    pub fn set_backing_batch(
        &mut self,
        project: ProjectId,
        updates: impl IntoIterator<Item = (PathBuf, Option<Arc<SourceText>>)>,
    ) -> Result<(), ChangeError> {
        self.require_project(project)?;
        let updates: std::collections::BTreeMap<_, _> = updates
            .into_iter()
            .map(|(path, text)| (normalize_path(&path), text))
            .collect();
        let mut removed = HashSet::new();
        for (path, text) in updates {
            let mut layers = self.layers_for(&path);
            if let Some(text) = text {
                layers.backing = Some(text);
                layers.backing_projects.insert(project);
            } else {
                layers.backing_projects.remove(&project);
                if layers.backing_projects.is_empty() {
                    layers.backing = None;
                }
            }
            self.stage_layers(&path, layers, &mut removed);
        }
        if !removed.is_empty() {
            self.read.inner.rebuild(&removed, None);
        }
        Ok(())
    }

    /// Open an editor overlay, sharing its effective bytes across project
    /// instances. This operation does not turn the overlay into backing data.
    pub fn open_overlay(
        &mut self,
        project: ProjectId,
        path: &Path,
        text: impl IntoSourceText,
    ) -> Result<SourceId, ChangeError> {
        self.require_project(project)?;
        let mut layers = self.layers_for(path);
        layers.overlay = Some(text.into_source_text());
        layers.overlay_projects.insert(project);
        Ok(self
            .publish_layers(path, layers)
            .expect("open overlay has contents"))
    }

    /// Close this project's overlay. Restore backing contents when supplied;
    /// otherwise remove memberships no longer held by another layer.
    pub fn close_overlay(
        &mut self,
        project: ProjectId,
        path: &Path,
    ) -> Result<Option<SourceId>, ChangeError> {
        self.require_project(project)?;
        let mut layers = self.layers_for(path);
        if !layers.overlay_projects.remove(&project) {
            return Ok(self
                .read
                .inner
                .lookup_file_in(project, path)
                .map(|file| *file.identity(&self.read.inner)));
        }
        if layers.overlay_projects.is_empty() {
            layers.overlay = None;
        }
        Ok(self.publish_layers(path, layers))
    }

    fn require_project(&self, project: ProjectId) -> Result<(), ChangeError> {
        self.read
            .inner
            .projects
            .contains_key(&project)
            .then_some(())
            .ok_or(ChangeError::ProjectRemoved)
    }

    fn layers_for(&self, path: &Path) -> SourceLayers {
        let db = &self.read.inner;
        let Some(source) = db.files.get(&normalize_path(path)) else {
            return SourceLayers::default();
        };
        db.source_layers.get(source).cloned().unwrap_or_else(|| {
            let instances = &db.identities[source];
            let file = instances.values().next().expect("live source");
            // Inputs supplied through the plain writer are explicit project
            // contents. Preserve them as backing when an overlay is first opened.
            SourceLayers {
                backing: Some(file.text(db).clone()),
                backing_projects: instances.keys().copied().collect(),
                ..SourceLayers::default()
            }
        })
    }

    fn publish_layers(&mut self, path: &Path, layers: SourceLayers) -> Option<SourceId> {
        let mut removed = HashSet::new();
        self.stage_layers(path, layers, &mut removed);
        if !removed.is_empty() {
            self.read.inner.rebuild(&removed, None);
        }
        self.read.inner.files.get(&normalize_path(path)).copied()
    }

    fn stage_layers(
        &mut self,
        path: &Path,
        layers: SourceLayers,
        removed: &mut HashSet<(ProjectId, PathBuf)>,
    ) {
        let path = normalize_path(path);
        let db = &mut self.read.inner;
        let old_source = db.files.get(&path).copied();
        if old_source.is_some_and(|source| db.source_layers.get(&source) == Some(&layers)) {
            return;
        }
        let retained: HashSet<_> = layers
            .backing_projects
            .union(&layers.overlay_projects)
            .copied()
            .collect();
        removed.extend(
            old_source
                .into_iter()
                .flat_map(|source| db.identities[&source].keys())
                .filter(|project| !retained.contains(project))
                .map(|project| (*project, path.clone())),
        );
        let changed = layers.effective().is_some_and(|text| {
            old_source
                .and_then(|source| db.identities[&source].values().next())
                .is_none_or(|file| file.text(db) != text)
        });
        if let Some(text) = layers.effective() {
            let mut projects: Vec<_> = retained.into_iter().collect();
            projects.sort_unstable();
            for project in projects {
                db.upsert_file_in(project, &path, text.clone());
            }
            let source = db.files[&path];
            if changed {
                db.stage_source_edits(source, None);
            }
            Arc::make_mut(&mut db.source_layers).insert(source, layers);
            db.revision += 1;
        } else if let Some(source) = old_source {
            Arc::make_mut(&mut db.source_layers).remove(&source);
        }
    }
}
