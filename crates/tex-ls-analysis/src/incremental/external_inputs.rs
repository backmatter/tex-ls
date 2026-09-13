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
        let path = normalize_path(path);
        if self.lookup_file(&path).is_some() {
            return Observation::Present(LocationKind::File);
        }
        let observation = self.location_observation(&path);
        if observation.kind != Observation::Unknown {
            return observation.kind;
        }
        let Some(parent) = path.parent() else {
            return Observation::Unknown;
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Observation::Unknown;
        };
        match self.location_observation(parent).directory {
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

    /// Preserve candidate precedence even when an earlier location is unknown.
    /// Acquisition errors stay explicit until the host refreshes that input.
    pub fn resolve_file(&self, candidates: &FileCandidates) -> FileResolution {
        let mut needs = Vec::new();
        for path in &candidates.local {
            match self.location_kind(path) {
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
            Some((stem, extensions)) => match self.installed_metadata() {
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
            if let Some(name) = components.next() {
                entries.insert(
                    name.as_os_str().to_string_lossy().into_owned(),
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
            result.toc.extend(artifact.parsed.data.toc.iter().cloned());
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
        (!result.labels.is_empty() || !result.toc.is_empty()).then_some(result)
    }
}

impl Analysis {
    /// Literal target acquisition requests. Hosts deduplicate acquisition by
    /// generation; negative and failed observations do not cause query retries.
    pub fn file_discovery_needs(&self, path: &Path) -> Vec<InputNeed> {
        let Some(file) = self.lookup_file(path) else {
            return Vec::new();
        };
        if path.extension().is_some_and(|ext| ext == "bib") {
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
        for reference in self.file_references(file) {
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
