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

    pub(in crate::incremental) fn set_bibliography_observation(
        &mut self,
        project: ProjectId,
        path: PathBuf,
        value: Observation<PathBuf>,
    ) -> bool {
        let location = self.location_input(project, path);
        if location.bibliography(self) == &value {
            return false;
        }
        location.set_bibliography(self).to(value);
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
                for (path, value) in files.bibliography {
                    let value = match value {
                        Observation::Present(path) => Observation::Present(normalize_path(&path)),
                        other => other,
                    };
                    db.set_bibliography_observation(token.project, normalize_path(&path), value);
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
    pub fn compiler_artifact(&self, path: &Path) -> Option<&Observation<CompilerArtifact>> {
        self.inner
            .project(self.project)
            .compiler(&self.inner)
            .get(&normalize_path(path))
            .map(|input| input.artifact(&self.inner).as_ref())
    }
}
