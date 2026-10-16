//! Publication and snapshot reads for host observations.
use super::*;
use crate::external::*;
use crate::source::normalize_path;

impl Database {
    fn location_input(&mut self, project: ProjectId, path: PathBuf) -> LocationInput {
        let input = self.project(project);
        if let Some(location) = input.locations(self).get(&path) {
            return *location;
        }
        let location =
            LocationInput::new(&*self, LocationObservation::default(), Observation::Unknown);
        let mut locations = input.locations(self).clone();
        locations.insert(path, location);
        input
            .set_locations(self)
            .with_durability(salsa::Durability::MEDIUM)
            .to(locations);
        location
    }

    pub(in crate::incremental) fn set_file_resolution_observation(
        &mut self,
        project: ProjectId,
        path: PathBuf,
        value: Observation<PathBuf>,
    ) -> bool {
        let location = self.location_input(project, path);
        if location.file_resolution(self) == &value {
            return false;
        }
        location.set_file_resolution(self).to(value);
        self.revision += 1;
        true
    }
}

impl IncrementalDatabase {
    /// Retire file observations outside a host-proved live scope when history
    /// exceeds twice the scope plus 256 cells. The slack amortizes reconstruction
    /// during target edits while bounding abandoned observations. Include every
    /// current resolution candidate, alias origin/target, directory and pending
    /// read in `keep`; generic observation batches otherwise retain merge semantics.
    /// Reconstruction invalidates file acquisitions from the old storage lifetime.
    pub fn retain_file_observations(
        &mut self,
        project: ProjectId,
        keep: &std::collections::BTreeSet<PathBuf>,
    ) -> Result<bool, ChangeError> {
        let db = &mut self.read.inner;
        if !db.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        let keep: std::collections::BTreeSet<_> =
            keep.iter().map(|path| normalize_path(path)).collect();
        let input = db.project(project);
        let limit = keep.len().saturating_mul(2).saturating_add(256);
        if input.locations(db).len() <= limit {
            return Ok(false);
        }
        let mut catalog = input.locations(db).clone();
        catalog.retain(|path, _| keep.contains(path));
        if catalog.len() == input.locations(db).len() {
            return Ok(false);
        }
        input
            .set_locations(db)
            .with_durability(salsa::Durability::MEDIUM)
            .to(catalog);
        db.rebuild(&Default::default(), None);
        Ok(true)
    }

    /// Publish the complete compiler-artifact scope owned by a host. Unlike
    /// `apply_external_inputs`, this retires observations omitted from the batch.
    /// Old snapshots keep their observations; the writer releases retired cells
    /// and cached payloads by rebuilding storage once when the scope shrinks.
    pub fn replace_compiler_inputs(
        &mut self,
        token: AcquisitionToken,
        artifacts: Vec<(PathBuf, Observation<CompilerArtifact>)>,
    ) -> Result<(), ChangeError> {
        let paths: std::collections::BTreeSet<_> = artifacts
            .iter()
            .map(|(path, _)| normalize_path(path))
            .collect();
        // Use the same generation checks and last-value semantics as a merge.
        // A rejected batch must not retire any previously published observation.
        self.apply_external_inputs(token, ExternalInputs::Compiler(artifacts))?;
        let db = &mut self.read.inner;
        let project = db.project(token.project);
        if project
            .compiler(db)
            .keys()
            .any(|path| !paths.contains(path))
        {
            let mut catalog = project.compiler(db).clone();
            catalog.retain(|path, _| paths.contains(path));
            project.set_compiler(db).to(catalog);
            db.rebuild(&Default::default(), None);
        }
        Ok(())
    }

    /// Reserve a refresh in one independent input category. A later reservation
    /// supersedes earlier work even if that work finishes first.
    pub fn begin_external_refresh(
        &mut self,
        project: ProjectId,
        kind: ExternalInputKind,
    ) -> Result<AcquisitionToken, ChangeError> {
        let db = &mut self.read.inner;
        if !db.projects.contains_key(&project) {
            return Err(ChangeError::ProjectRemoved);
        }
        let generation = db.acquisitions.entry((project, kind)).or_default();
        *generation += 1;
        Ok(AcquisitionToken {
            project,
            kind,
            generation: *generation,
            epoch: db.epoch,
        })
    }

    /// Reject stale acquisition before changing any input. File acquisitions also
    /// belong to a storage lifetime, so removal cannot revive an old membership.
    pub fn apply_external_inputs(
        &mut self,
        token: AcquisitionToken,
        inputs: ExternalInputs,
    ) -> Result<(), ChangeError> {
        let kind = match &inputs {
            ExternalInputs::Files(_) => ExternalInputKind::Files,
            ExternalInputs::Installed(_) => ExternalInputKind::Installed,
            ExternalInputs::Compiler(_) => ExternalInputKind::Compiler,
        };
        let db = &self.read.inner;
        if !db.projects.contains_key(&token.project) {
            return Err(ChangeError::ProjectRemoved);
        }
        let published = db
            .generations
            .get(&token.project)
            .copied()
            .unwrap_or_default();
        let previous = match kind {
            ExternalInputKind::Files => published.files,
            ExternalInputKind::Installed => published.installed,
            ExternalInputKind::Compiler => published.compiler,
        };
        if token.kind != kind
            || db.acquisitions.get(&(token.project, kind)) != Some(&token.generation)
            || token.generation <= previous
            || (kind == ExternalInputKind::Files && token.epoch != db.epoch)
        {
            return Err(ChangeError::StaleRevision);
        }
        match inputs {
            ExternalInputs::Files(files) => {
                // Membership shrink may reconstruct storage. Publish locations
                // against the resulting database, never against old Salsa handles.
                self.set_backing_batch(token.project, files.backing)?;
                let db = &mut self.read.inner;
                // Pre-create every new observation cell with one catalog write.
                // Repeatedly cloning the growing map makes acquisition quadratic.
                let project = db.project(token.project);
                let new_paths: std::collections::BTreeSet<_> = files
                    .locations
                    .iter()
                    .map(|(path, _)| path)
                    .chain(files.file_resolution.iter().map(|(path, _)| path))
                    .map(|path| normalize_path(path))
                    .filter(|path| !project.locations(db).contains_key(path))
                    .collect();
                if !new_paths.is_empty() {
                    let mut catalog = project.locations(db).clone();
                    for path in new_paths {
                        catalog.insert(
                            path,
                            LocationInput::new(
                                &*db,
                                LocationObservation::default(),
                                Observation::Unknown,
                            ),
                        );
                    }
                    project
                        .set_locations(db)
                        .with_durability(salsa::Durability::MEDIUM)
                        .to(catalog);
                }
                for (path, value) in files.locations {
                    let location = db.location_input(token.project, normalize_path(&path));
                    if location.observation(db) != &value {
                        location.set_observation(db).to(value);
                    }
                }
                for (path, value) in files.file_resolution {
                    let value = match value {
                        Observation::Present(path) => Observation::Present(normalize_path(&path)),
                        other => other,
                    };
                    db.set_file_resolution_observation(token.project, normalize_path(&path), value);
                }
            }
            ExternalInputs::Installed(value) => {
                let db = &mut self.read.inner;
                let project = db.project(token.project);
                if project.installed(db).as_ref() != &value {
                    project.set_installed(db).to(Arc::new(value));
                }
            }
            ExternalInputs::Compiler(artifacts) => {
                let db = &mut self.read.inner;
                let project = db.project(token.project);
                let mut catalog = project.compiler(db).clone();
                for (path, value) in artifacts {
                    let path = normalize_path(&path);
                    if let Some(input) = catalog.get(&path) {
                        if input.artifact(db).as_ref() != &value {
                            input.set_artifact(db).to(Arc::new(value));
                        }
                    } else {
                        catalog.insert(path, CompilerInput::new(&*db, Arc::new(value)));
                    }
                }
                if project.compiler(db) != &catalog {
                    project.set_compiler(db).to(catalog);
                }
            }
        }
        let db = &mut self.read.inner;
        let generations = db.generations.entry(token.project).or_default();
        match kind {
            ExternalInputKind::Files => generations.files = token.generation,
            ExternalInputKind::Installed => generations.installed = token.generation,
            ExternalInputKind::Compiler => generations.compiler = token.generation,
        }
        db.revision += 1;
        Ok(())
    }
}

impl Analysis {
    pub fn external_generations(&self) -> ExternalGenerations {
        self.inner
            .generations
            .get(&self.project)
            .copied()
            .unwrap_or_default()
    }
    pub fn location_observation(&self, path: &Path) -> LocationObservation {
        self.inner
            .project(self.project)
            .locations(&self.inner)
            .get(&normalize_path(path))
            .map(|input| input.observation(&self.inner).clone())
            .unwrap_or_default()
    }
    /// Resolve availability from supplied source membership and observations.
    /// An incomplete directory listing proves only the entries it contains.
    pub fn location_kind(&self, path: &Path) -> Observation<LocationKind> {
        if self.lookup_file(path).is_some() {
            return Observation::Present(LocationKind::File);
        }
        observed_location_kind(&self.inner, self.inner.project(self.project), path)
    }

    /// Preserve candidate precedence even when an earlier location is unknown.
    /// Acquisition errors stay explicit until the host refreshes that input.
    pub fn resolve_file(&self, candidates: &FileCandidates) -> FileResolution {
        resolve_candidates(&self.inner, self.inner.project(self.project), candidates)
    }

    pub fn installed_metadata(&self) -> &Observation<InstalledMetadata> {
        self.inner.project(self.project).installed(&self.inner)
    }
    pub fn compiler_artifacts(
        &self,
    ) -> impl Iterator<Item = (&Path, &Observation<CompilerArtifact>)> {
        self.inner
            .project(self.project)
            .compiler(&self.inner)
            .iter()
            .map(|(path, input)| (path.as_path(), input.artifact(&self.inner).as_ref()))
    }
    pub fn compiler_artifact(&self, path: &Path) -> Option<&Observation<CompilerArtifact>> {
        self.inner
            .project(self.project)
            .compiler(&self.inner)
            .get(&normalize_path(path))
            .map(|input| input.artifact(&self.inner).as_ref())
    }
}

impl Analysis {
    /// Installed names captured with this read. Unknown metadata contributes no
    /// candidates; callers retain incompleteness independently of this projection.
    pub fn texmf(&self) -> &crate::project::texmf::TexmfIndex {
        static EMPTY: std::sync::LazyLock<crate::project::texmf::TexmfIndex> =
            std::sync::LazyLock::new(Default::default);
        match self.installed_metadata() {
            Observation::Present(metadata) => &metadata.index,
            _ => &EMPTY,
        }
    }

    /// Known directory entries, including supplied source membership. Listing
    /// completeness remains available through `location_observation`.
    pub fn read_dir(&self, path: &Path) -> Vec<(String, bool)> {
        let path = normalize_path(path);
        let mut entries = match self.location_observation(&path).directory {
            Observation::Present(directory) => directory.entries,
            _ => Default::default(),
        };
        for (file, _) in self.tracked_files() {
            let Ok(relative) = file.strip_prefix(&path) else {
                continue;
            };
            let mut components = relative.components();
            if let Some(name) = components.next()
                && let Some(name) = name.as_os_str().to_str()
            {
                entries.insert(
                    name.to_owned(),
                    if components.next().is_some() {
                        LocationKind::Directory
                    } else {
                        LocationKind::File
                    },
                );
            }
        }
        entries
            .into_iter()
            .map(|(name, kind)| (name, kind == LocationKind::Directory))
            .collect()
    }

    /// Merge explicitly supplied last-build artifacts in namespace/source order.
    /// These facts never participate in source definitions or edit authorization.
    pub fn aux_data(
        &self,
        namespace: &[&Path],
        _root: &Path,
    ) -> Option<crate::project::aux::AuxData> {
        let mut result = crate::project::aux::AuxData::default();
        let mut visited = std::collections::HashSet::new();
        let mut identities = std::collections::HashSet::new();
        let mut pending: Vec<_> = namespace
            .iter()
            .rev()
            .map(|path| path.with_extension("aux"))
            .collect();
        while let Some(path) = pending.pop() {
            let path = normalize_path(&path);
            if !visited.insert(path.clone()) {
                continue;
            }
            let Some(Observation::Present(artifact)) = self.compiler_artifact(&path) else {
                continue;
            };
            if !identities.insert(&artifact.identity) {
                continue;
            }
            for (key, number) in &artifact.parsed.data.labels {
                result
                    .labels
                    .entry(key.clone())
                    .or_insert_with(|| number.clone());
            }
            for (key, page) in &artifact.parsed.data.pages {
                result
                    .pages
                    .entry(key.clone())
                    .or_insert_with(|| page.clone());
            }
            for (key, name) in &artifact.parsed.data.names {
                result
                    .names
                    .entry(key.clone())
                    .or_insert_with(|| name.clone());
            }
            result.toc.extend(artifact.parsed.data.toc.iter().cloned());
            if let Some(inputs) = &artifact.resolved_aux_inputs {
                pending.extend(inputs.iter().rev().cloned());
            } else {
                let base = path.parent().unwrap_or(Path::new(""));
                pending.extend(
                    artifact
                        .parsed
                        .inputs
                        .iter()
                        .rev()
                        .map(|target| base.join(target)),
                );
            }
        }
        (!result.labels.is_empty()
            || !result.pages.is_empty()
            || !result.names.is_empty()
            || !result.toc.is_empty())
        .then_some(result)
    }
}

impl Analysis {
    /// The preloaded LaTeX format has source definitions even though documents
    /// do not explicitly input it. The host's selected TEXMF index owns its path.
    pub fn format_source_paths(&self) -> Vec<PathBuf> {
        match self.installed_metadata() {
            Observation::Present(metadata) => metadata
                .index
                .by_name
                .get("latex.ltx")
                .cloned()
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Literal target acquisition requests. Hosts deduplicate acquisition by
    /// generation; negative and failed observations do not cause query retries.
    pub fn file_discovery_needs(&self, path: &Path) -> Vec<InputNeed> {
        let Some(file) = self.lookup_file(path) else {
            return Vec::new();
        };
        if path.extension().is_some_and(|ext| ext == "bib") {
            return Vec::new();
        }
        // The preloaded format is consulted for literal definitions on demand.
        // Its bootstrap inputs are not document dependencies; following every
        // conditional input there would load much of the installation for one
        // navigation request.
        if matches!(
            self.installed_metadata(),
            Observation::Present(metadata)
                if metadata.index.by_name.get("latex.ltx").is_some_and(|format| format == path)
        ) {
            return Vec::new();
        }
        let mut needs = Vec::new();
        for root in crate::external::explicit_root_hints(self.file_text(file), path) {
            if self.lookup_file(&root).is_none()
                && self
                    .file_alias(&root)
                    .and_then(|actual| self.lookup_file(actual))
                    .is_none()
            {
                match self.location_observation(&root).kind {
                    Observation::Unknown => needs.push(InputNeed::Location(root)),
                    Observation::Present(_) => needs.push(InputNeed::Source(root)),
                    _ => {}
                }
            }
        }
        for reference in self.file_discovery_references(file) {
            let mut resolution = self.resolve_file(&reference.candidates);
            if let Observation::Present(target) = &resolution.target
                && crate::source::lint_file_kind(target).is_some()
                && self.lookup_file(target).is_none()
                && self
                    .file_alias(target)
                    .and_then(|actual| self.lookup_file(actual))
                    .is_none()
            {
                resolution.needs.push(InputNeed::Source(target.clone()));
            }
            for need in resolution.needs {
                if !needs.contains(&need) {
                    needs.push(need);
                }
            }
        }
        needs
    }
}

pub(super) fn observed_location_kind(
    db: &dyn IncrementalDb,
    project: ProjectInput,
    path: &Path,
) -> Observation<LocationKind> {
    let observe = |path: &Path| {
        project
            .locations(db)
            .get(path)
            .map(|input| input.observation(db).clone())
            .unwrap_or_default()
    };

    let path = normalize_path(path);
    let actual = super::inputs::file_alias_target(db, project, &path).unwrap_or(&path);
    if crate::project::root_views::project_sources(db, project).contains_key(actual) {
        return Observation::Present(LocationKind::File);
    }
    let observation = observe(&path);
    if observation.kind != Observation::Unknown {
        return observation.kind;
    }
    let Some(parent) = path.parent() else {
        return Observation::Unknown;
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Observation::Unknown;
    };
    match observe(parent).directory {
        Observation::Present(directory) => match directory.entries.get(name) {
            Some(kind) => Observation::Present(*kind),
            None if directory.complete => Observation::Absent,
            None => Observation::Unknown,
        },
        Observation::Absent => Observation::Absent,
        Observation::Error(error) => Observation::Error(error),
        Observation::Unknown => Observation::Unknown,
    }
}

pub(in crate::incremental) fn resolve_candidates(
    db: &dyn IncrementalDb,
    project: ProjectInput,
    candidates: &FileCandidates,
) -> FileResolution {
    let mut needs = Vec::new();
    for path in &candidates.local {
        match observed_location_kind(db, project, path) {
            Observation::Present(LocationKind::File) => {
                return FileResolution {
                    target: if needs.is_empty() {
                        Observation::Present(path.clone())
                    } else {
                        Observation::Unknown
                    },
                    needs,
                };
            }
            Observation::Present(LocationKind::Directory) | Observation::Absent => {}
            Observation::Unknown => needs.push(InputNeed::Location(path.clone())),
            Observation::Error(error) => {
                return FileResolution {
                    target: Observation::Error(error),
                    needs,
                };
            }
        }
    }
    if !needs.is_empty() {
        return FileResolution {
            target: Observation::Unknown,
            needs,
        };
    }
    let target = match &candidates.installed {
        None => Observation::Absent,
        Some((stem, extensions)) => match project.installed(db).as_ref() {
            Observation::Present(metadata) => {
                let extensions: Vec<_> = extensions.iter().map(String::as_str).collect();
                metadata
                    .index
                    .resolve(stem, &extensions)
                    .map(|path| Observation::Present(path.to_path_buf()))
                    .unwrap_or(Observation::Absent)
            }
            Observation::Absent => Observation::Absent,
            Observation::Error(error) => Observation::Error(error.clone()),
            Observation::Unknown => {
                needs.push(InputNeed::Installed);
                Observation::Unknown
            }
        },
    };
    FileResolution { target, needs }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn directory_memberships_preserve_bytes_without_lossy_completion_names() {
        use std::os::unix::ffi::OsStringExt;
        let mut db = IncrementalDatabase::default();
        let root = Path::new("/project");
        let invalid = root.join(std::ffi::OsString::from_vec(b"bad\xff.tex".to_vec()));
        let invalid_directory = root.join(std::ffi::OsString::from_vec(b"dir\xfe".to_vec()));
        let child = invalid_directory.join("child.tex");
        let valid = root.join("valid\u{fffd}.tex");
        for path in [&invalid, &child, &valid] {
            db.apply_change(path, "text", None);
        }
        let snapshot = db.snapshot();
        assert!(snapshot.lookup_file(&invalid).is_some());
        assert!(snapshot.lookup_file(&child).is_some());
        assert_eq!(
            snapshot.read_dir(root),
            vec![("valid\u{fffd}.tex".into(), false)]
        );
    }

    #[test]
    fn file_observation_retirement_preserves_live_facts_and_bounds_history() {
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let source = Path::new("/p/main.tex");
        db.apply_change(source, "$alphabet$", None);
        let source_id = db.snapshot().lookup_file(source).unwrap();
        let version = db.snapshot().source_version(source_id);
        let absent = PathBuf::from("/p/missing.bib");
        let failed = PathBuf::from("/p/failed.bib");
        let alias = PathBuf::from("/p/link.bib");
        let actual = PathBuf::from("/installed/library.bib");
        let keep = [
            source.to_owned(),
            absent.clone(),
            failed.clone(),
            alias.clone(),
            actual.clone(),
        ]
        .into_iter()
        .collect();
        let rebuilds = db.read.inner.rebuild_count;
        let mut retirements = 0;
        for generation in 0..1024 {
            let old_target = PathBuf::from(format!("/p/old-{generation}.bib"));
            let token = db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .unwrap();
            db.apply_external_inputs(
                token,
                ExternalInputs::Files(FileInputs {
                    locations: vec![
                        (
                            old_target.clone(),
                            LocationObservation {
                                kind: Observation::Absent,
                                ..Default::default()
                            },
                        ),
                        (
                            absent.clone(),
                            LocationObservation {
                                kind: Observation::Absent,
                                ..Default::default()
                            },
                        ),
                        (
                            failed.clone(),
                            LocationObservation {
                                kind: Observation::Error("permission denied".into()),
                                ..Default::default()
                            },
                        ),
                    ],
                    file_resolution: vec![
                        (old_target, Observation::Absent),
                        (alias.clone(), Observation::Present(actual.clone())),
                    ],
                    ..Default::default()
                }),
            )
            .unwrap();
            let stale = db
                .begin_external_refresh(project, ExternalInputKind::Files)
                .unwrap();
            if db.retain_file_observations(project, &keep).unwrap() {
                retirements += 1;
                assert_eq!(
                    db.apply_external_inputs(stale, ExternalInputs::Files(Default::default())),
                    Err(ChangeError::StaleRevision)
                );
            }
            let catalog = db.read.inner.project(project).locations(&db.read.inner);
            assert!(catalog.len() <= keep.len() * 2 + 256);
            let snapshot = db.snapshot();
            assert_eq!(snapshot.source_version(source_id), version);
            assert_eq!(snapshot.location_kind(&absent), Observation::Absent);
            assert_eq!(
                snapshot.location_kind(&failed),
                Observation::Error("permission denied".into())
            );
            assert_eq!(snapshot.file_alias(&alias), Some(actual.as_path()));
        }
        assert!(
            retirements > 0 && retirements < 8,
            "reconstruction must be amortized"
        );
        assert_eq!(db.read.inner.rebuild_count, rebuilds + retirements);
        assert_eq!(
            db.retain_file_observations(ProjectId::new(), &keep),
            Err(ChangeError::ProjectRemoved)
        );
    }

    #[test]
    fn authoritative_compiler_scope_retires_payloads_without_changing_merge_semantics() {
        use crate::external::{CompilerArtifact, ExternalInputKind, ExternalInputs, Observation};
        let mut db = IncrementalDatabase::default();
        let project = db.project_id();
        let source = Path::new("/p/main.tex");
        db.apply_change(source, "\\label{live}", None);
        let keep = Path::new("/p/main.aux");
        let retired = Path::new("/p/retired.aux");
        let artifact = |path: &Path, number: &str| {
            Observation::Present(CompilerArtifact::from_text(
                path,
                &format!("\\newlabel{{live}}{{{{{number}}}{{1}}}}"),
                path.display().to_string(),
            ))
        };
        let token = db
            .begin_external_refresh(project, ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(
            token,
            ExternalInputs::Compiler(vec![
                (keep.into(), artifact(keep, "1")),
                (retired.into(), artifact(retired, "2")),
            ]),
        )
        .unwrap();
        let old = db.snapshot();
        let input = db.read.inner.project(project).compiler(&db.read.inner)[retired];
        let payload = Arc::downgrade(input.artifact(&db.read.inner));
        assert!(old.compiler_artifact(retired).is_some());
        // Salsa publication waits for readers. Release this snapshot before a
        // writer update; the live writer still owns the retired payload.
        drop(old);
        assert!(payload.upgrade().is_some());
        let stale = db
            .begin_external_refresh(project, ExternalInputKind::Compiler)
            .unwrap();
        let fresh = db
            .begin_external_refresh(project, ExternalInputKind::Compiler)
            .unwrap();
        let rebuilds = db.read.inner.rebuild_count;
        assert!(db.replace_compiler_inputs(stale, vec![]).is_err());
        assert!(db.snapshot().compiler_artifact(retired).is_some());
        db.replace_compiler_inputs(fresh, vec![("/p/./main.aux".into(), artifact(keep, "3"))])
            .unwrap();
        assert_eq!(db.read.inner.rebuild_count, rebuilds + 1);
        assert!(db.snapshot().compiler_artifact(retired).is_none());
        assert!(
            payload.upgrade().is_none(),
            "retired Salsa cells must release their payloads"
        );
        assert!(db.snapshot().lookup_file(source).is_some());
        let fresh = db
            .begin_external_refresh(project, ExternalInputKind::Compiler)
            .unwrap();
        db.replace_compiler_inputs(fresh, vec![(keep.into(), artifact(keep, "3"))])
            .unwrap();
        assert_eq!(db.read.inner.rebuild_count, rebuilds + 1);
        let fresh = db
            .begin_external_refresh(project, ExternalInputKind::Compiler)
            .unwrap();
        db.apply_external_inputs(
            fresh,
            ExternalInputs::Compiler(vec![(retired.into(), Observation::Absent)]),
        )
        .unwrap();
        assert!(db.snapshot().compiler_artifact(keep).is_some());
        let fresh = db
            .begin_external_refresh(project, ExternalInputKind::Compiler)
            .unwrap();
        db.replace_compiler_inputs(fresh, vec![]).unwrap();
        assert_eq!(db.snapshot().compiler_artifacts().count(), 0);
        assert_eq!(db.read.inner.rebuild_count, rebuilds + 2);
    }
}
