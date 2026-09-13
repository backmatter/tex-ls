//! In-memory host for the same language computations as the native LSP server.
//!
//! The caller owns transport, file discovery, persistence, and scheduling. A
//! session processes writes in order and answers against the resulting revision.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tex_ls_analysis::external::*;
use tex_ls_analysis::incremental::ProjectId;
use tex_ls_analysis::source::normalize_path;
use tex_ls_protocol::projects::{ProjectRegistry, workspace_roots};
use tex_ls_protocol::*;

struct Document {
    path: PathBuf,
    text: Arc<TextBuffer>,
    version: i32,
    project: ProjectId,
}

/// A project whose documents are supplied by the embedding application.
pub struct Session {
    db: IncrementalDatabase,
    projects: ProjectRegistry,
    lifecycle: lifecycle::Lifecycle,
    documents: HashMap<Uri, Document>,
    encoding: PositionEncoding,
    active_path: Option<PathBuf>,
    policy: ResponsePolicy,
    acquisitions: HashMap<u64, (ProjectId, ExternalInputKind, AcquisitionToken)>,
    next_acquisition: u64,
    settings: crate::settings::ResolvedSettings,
    source_settings: HashMap<PathBuf, crate::settings::ResolvedSettings>,
}

impl Default for Session {
    fn default() -> Self {
        let db = IncrementalDatabase::default();
        Self {
            projects: ProjectRegistry::new(db.project_id()),
            lifecycle: lifecycle::Lifecycle::default(),
            db,
            documents: HashMap::new(),
            encoding: PositionEncoding::Utf16,
            active_path: None,
            policy: ResponsePolicy::default(),
            acquisitions: HashMap::new(),
            next_acquisition: 0,
            settings: crate::settings::ResolvedSettings::default(),
            source_settings: HashMap::new(),
        }
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn encode(value: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

impl Session {
    /// Supply backing contents independently of an open editor overlay.
    pub fn set_backing(
        &mut self,
        path: &std::path::Path,
        text: Option<Arc<tex_ls_analysis::text::SourceText>>,
    ) -> Result<(), String> {
        let project = self
            .projects
            .select(path, None)
            .map_err(|error| error.to_string())?;
        self.db
            .set_backing(project, path, text)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Replace project defaults or one source's override after validating every
    /// subsystem. Invalid settings leave the complete previous value installed.
    pub fn configure(
        &mut self,
        path: Option<&std::path::Path>,
        settings: crate::settings::Settings,
    ) -> Result<(), crate::settings::SettingsError> {
        let resolved = settings.resolve()?;
        if let Some(path) = path {
            let path = normalize_path(path);
            let project = self.projects.select(&path, None).map_err(|error| {
                crate::settings::SettingsError {
                    field: "project".into(),
                    message: error.to_string(),
                }
            })?;
            self.db
                .set_project_source_declarations(project, &path, resolved.declarations.clone())
                .expect("registered project");
            self.source_settings.insert(path, resolved);
        } else {
            for project in self.projects.projects() {
                self.db
                    .set_project_declarations(project, resolved.declarations.clone())
                    .expect("registered project");
            }
            self.settings = resolved;
        }
        Ok(())
    }

    pub fn clear_source_settings(&mut self, path: &std::path::Path) {
        let path = normalize_path(path);
        self.source_settings.remove(&path);
        let project = self
            .projects
            .select(&path, None)
            .expect("unique workspace roots");
        self.db
            .clear_project_source_declarations(project, &path)
            .expect("registered project");
    }

    /// Run a synchronous read with source preconditions for its caller's editor
    /// transaction. Preconditions belong to this Session instance; a worker
    /// restart must discard results returned by the previous instance.
    pub fn dispatch_with_context(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.lifecycle.require_running()?;
        let preconditions = self.source_preconditions();
        let result = self.dispatch(method, params)?;
        Ok(json!({"result": result, "preconditions": preconditions}))
    }

    fn source_preconditions(&self) -> Value {
        let mut values = Vec::new();
        let mut contexts = Vec::new();
        for project in self.projects.projects() {
            let snapshot = self.db.snapshot_for(project).expect("registered project");
            contexts
                .push(json!({"epoch":snapshot.epoch(),"revision":snapshot.revision().revision}));
            for (path, source) in snapshot.tracked_files() {
                let Some(uri) = path_to_uri(&path) else {
                    continue;
                };
                let version = snapshot.source_version(source).expect("live source");
                values.push(json!({"uri": uri, "revision": version.revision,
                    "version": self.documents.get(&uri).map(|document| document.version)}));
            }
        }
        values.sort_by_cached_key(Value::to_string);
        contexts.sort_by_cached_key(Value::to_string);
        json!({"sources":values,"contexts":contexts})
    }

    /// Handle LSP method parameters without stdio, threads, or a JSON-RPC loop.
    /// Positions use UTF-16, matching browsers and LSP's default encoding.
    pub fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let uri = params.pointer("/textDocument/uri").cloned();
        if let Some(uri) = uri
            .as_ref()
            .and_then(|value| serde_json::from_value::<Uri>(value.clone()).ok())
            .and_then(|uri| uri_to_fs_path(&uri))
        {
            self.active_path = Some(uri);
        }
        let mut result = self.dispatch_raw(method, params)?;
        if self
            .policy
            .supports("/workspace/workspaceEdit/documentChanges")
        {
            workspace_edits::attach_versions(
                &mut result,
                &self
                    .documents
                    .iter()
                    .map(|(uri, document)| (uri.as_str().to_owned(), document.version))
                    .collect(),
            );
        }
        self.policy.response(method, uri.as_ref(), &mut result);
        Ok(result)
    }

    fn dispatch_raw(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if method != "initialize" && method != "exit" {
            self.lifecycle.require_running()?;
        }
        match method {
            "tex-ls/replaceProject" => {
                let uri: Uri = decode(params.get("uri").cloned().ok_or("Missing uri")?)?;
                let path = uri_to_fs_path(&uri).ok_or("Expected a file URI")?;
                let old = self
                    .projects
                    .select(&path, None)
                    .map_err(|error| error.to_string())?;
                self.source_settings
                    .retain(|path, _| self.projects.select(path, None) != Ok(old));
                self.documents.retain(|_, document| document.project != old);
                self.acquisitions
                    .retain(|_, (project, _, _)| *project != old);
                let was_default = old == self.db.project_id();
                self.db.remove_project(old);
                let new = if was_default {
                    self.db.project_id()
                } else {
                    self.db.create_project(self.settings.declarations.clone())
                };
                self.db
                    .set_project_declarations(new, self.settings.declarations.clone())
                    .expect("new project");
                self.projects.replace(old, new);
                return Ok(Value::Null);
            }
            "tex-ls/associateSource" => {
                let uri: Uri = decode(params.get("uri").cloned().ok_or("Missing uri")?)?;
                let project_uri: Uri = decode(
                    params
                        .get("projectUri")
                        .cloned()
                        .ok_or("Missing projectUri")?,
                )?;
                let path = uri_to_fs_path(&uri).ok_or("Expected a file URI")?;
                let project_path =
                    uri_to_fs_path(&project_uri).ok_or("Expected a project file URI")?;
                let project = self
                    .projects
                    .select(&project_path, None)
                    .map_err(|error| error.to_string())?;
                self.projects
                    .associate(&path, project)
                    .map_err(|error| error.to_string())?;
                if let Some(document) = self.documents.get_mut(&uri)
                    && document.project != project
                {
                    if let Some(settings) = self.source_settings.get(&normalize_path(&path)) {
                        self.db
                            .set_project_source_declarations(
                                project,
                                &path,
                                settings.declarations.clone(),
                            )
                            .expect("registered project");
                    }
                    self.db
                        .open_overlay(project, &path, document.text.text_arc())
                        .expect("registered project");
                    self.db
                        .close_overlay(document.project, &path)
                        .expect("old project");
                    document.project = project;
                }
                return Ok(Value::Null);
            }
            "tex-ls/checkPreconditions" => {
                let expected = params.get("preconditions").ok_or("Missing preconditions")?;
                return Ok(json!({"current": expected == &self.source_preconditions()}));
            }
            "tex-ls/discoveryNeeds" => {
                let uri: Uri = decode(params.get("uri").cloned().ok_or("Missing uri")?)?;
                let path = uri_to_fs_path(&uri).ok_or("Expected a file URI")?;
                let project = self
                    .projects
                    .select(&path, None)
                    .map_err(|error| error.to_string())?;
                let snapshot = self
                    .db
                    .snapshot_for(project)
                    .map_err(|error| error.to_string())?;
                return encode(snapshot.file_discovery_needs(&path));
            }
            "tex-ls/beginExternalRefresh" => {
                let uri: Uri = decode(params.get("uri").cloned().ok_or("Missing uri")?)?;
                let path = uri_to_fs_path(&uri).ok_or("Expected a file URI")?;
                let project = self
                    .projects
                    .select(&path, None)
                    .map_err(|error| error.to_string())?;
                let kind = match params.get("kind").and_then(Value::as_str) {
                    Some("files") => ExternalInputKind::Files,
                    Some("installed") => ExternalInputKind::Installed,
                    Some("compiler") => ExternalInputKind::Compiler,
                    _ => return Err("Expected files, installed, or compiler kind".into()),
                };
                let token = self
                    .db
                    .begin_external_refresh(project, kind)
                    .map_err(|error| error.to_string())?;
                self.acquisitions
                    .retain(|_, (owner, category, _)| *owner != project || *category != kind);
                self.next_acquisition += 1;
                self.acquisitions
                    .insert(self.next_acquisition, (project, kind, token));
                return Ok(json!({"token": self.next_acquisition}));
            }
            "tex-ls/applyExternalInputs" => {
                let id = params
                    .get("token")
                    .and_then(Value::as_u64)
                    .ok_or("Missing acquisition token")?;
                let batch: crate::external_inputs::InputBatch =
                    decode(params.get("inputs").cloned().ok_or("Missing inputs")?)?;
                let (_, _, token) = self
                    .acquisitions
                    .remove(&id)
                    .ok_or("Acquisition is stale or already completed")?;
                self.db
                    .apply_external_inputs(token, batch.into_inputs())
                    .map_err(|error| error.to_string())?;
                return Ok(json!({"applied": true}));
            }
            "tex-ls/updateSettings" => {
                let path = params
                    .get("uri")
                    .map(|value| {
                        let uri: Uri = decode(value.clone())?;
                        uri_to_fs_path(&uri).ok_or_else(|| "Expected a file URI".to_owned())
                    })
                    .transpose()?;
                if params.get("settings").is_some_and(Value::is_null) {
                    if let Some(path) = path.as_deref() {
                        self.clear_source_settings(path);
                    } else {
                        self.configure(None, crate::settings::Settings::default())
                            .expect("valid defaults");
                    }
                    return Ok(json!({"applied": true}));
                }
                let settings = match decode(
                    params.get("settings").cloned().ok_or("Missing settings")?,
                ) {
                    Ok(settings) => settings,
                    Err(message) => {
                        return Ok(
                            json!({"applied": false, "error": {"field": "settings", "message": message}}),
                        );
                    }
                };
                return Ok(match self.configure(path.as_deref(), settings) {
                    Ok(()) => json!({"applied": true}),
                    Err(error) => json!({"applied": false, "error": error}),
                });
            }
            "initialize" => {
                self.lifecycle.initialize()?;
                self.projects =
                    ProjectRegistry::from_roots(&mut self.db, &workspace_roots(&params));
                self.encoding = capabilities::negotiate_position_encoding(&params);
                self.policy = ResponsePolicy::new(&params);
                let capabilities =
                    server_capabilities(self.encoding, self.policy.pull_diagnostics());
                return Ok(self.policy.initialize_result(capabilities));
            }
            "initialized" => return Ok(Value::Null),
            "shutdown" => {
                self.lifecycle.shutdown()?;
                return Ok(Value::Null);
            }
            "exit" => {
                *self = Self::default();
                self.lifecycle.exit();
                return Ok(Value::Null);
            }
            "tex-ls/registerFile" => {
                let uri: Uri = decode(params.get("uri").cloned().ok_or("Missing uri")?)?;
                let path = uri_to_fs_path(&uri).ok_or("Expected a file URI")?;
                let project = self
                    .projects
                    .select(&path, None)
                    .map_err(|error| error.to_string())?;
                let token = self
                    .db
                    .begin_external_refresh(project, ExternalInputKind::Files)
                    .map_err(|error| error.to_string())?;
                self.db
                    .apply_external_inputs(
                        token,
                        ExternalInputs::Files(FileInputs {
                            locations: vec![(
                                path,
                                LocationObservation {
                                    kind: Observation::Present(LocationKind::File),
                                    directory: Observation::Unknown,
                                },
                            )],
                            ..Default::default()
                        }),
                    )
                    .map_err(|error| error.to_string())?;
                return Ok(Value::Null);
            }
            "textDocument/didOpen" => {
                let item = decode::<DidOpenTextDocumentParams>(params)?.text_document;
                let path =
                    uri_to_fs_path(&item.uri).ok_or("Embedded documents require a file URI")?;
                let project = self
                    .projects
                    .select(&path, None)
                    .map_err(|error| error.to_string())?;
                let text = Arc::new(TextBuffer::new(item.text, self.encoding));
                if let Some(settings) = self.source_settings.get(&normalize_path(&path)) {
                    self.db
                        .set_project_source_declarations(
                            project,
                            &path,
                            settings.declarations.clone(),
                        )
                        .expect("registered project");
                }
                self.db
                    .replace_overlay(project, &path, text.text_arc())
                    .expect("registered project");
                self.documents.insert(
                    item.uri,
                    Document {
                        path,
                        text,
                        version: item.version,
                        project,
                    },
                );
                return Ok(Value::Null);
            }
            "textDocument/didChange" => {
                let change = decode::<DidChangeTextDocumentParams>(params)?;
                let document = self
                    .documents
                    .get_mut(&change.text_document.text_document_identifier.uri)
                    .ok_or("Document is not open")?;
                if change.text_document.version <= document.version {
                    return Err("Document versions must increase".to_owned());
                }
                let edits = apply_content_changes(&mut document.text, change.content_changes)
                    .map_err(|error| error.to_string())?;
                self.db
                    .apply_project_change(
                        document.project,
                        &document.path,
                        document.text.text_arc(),
                        edits,
                    )
                    .expect("registered project");
                document.version = change.text_document.version;
                return Ok(Value::Null);
            }
            "textDocument/didClose" => {
                let close = decode::<DidCloseTextDocumentParams>(params)?;
                if let Some(document) = self.documents.remove(&close.text_document.uri) {
                    self.db
                        .close_overlay(document.project, &document.path)
                        .expect("registered project");
                }
                return Ok(Value::Null);
            }
            "workspace/didChangeWorkspaceFolders" => {
                let params: lsp_types::DidChangeWorkspaceFoldersParams = decode(params)?;
                let removed: Vec<_> = params
                    .event
                    .removed
                    .iter()
                    .filter_map(|folder| uri_to_fs_path(&folder.uri))
                    .collect();
                let mut roots = self.projects.roots();
                roots.retain(|root| !removed.contains(root));
                roots.extend(
                    params
                        .event
                        .added
                        .iter()
                        .filter_map(|folder| uri_to_fs_path(&folder.uri)),
                );
                let open: Vec<_> = self
                    .documents
                    .values()
                    .map(|document| document.path.clone())
                    .collect();
                self.projects.update_roots(&mut self.db, &roots, &open);
                for document in self.documents.values_mut() {
                    document.project = self
                        .projects
                        .select(&document.path, None)
                        .map_err(|error| error.to_string())?;
                }
                return Ok(Value::Null);
            }
            "workspace/executeCommand" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or("Missing command")?;
                if command != "tex-ls.inspectProject" {
                    return Err(format!("Unknown command: {command}"));
                }
                let uri: Uri = decode(
                    params
                        .pointer("/arguments/0/uri")
                        .cloned()
                        .ok_or("Missing argument uri")?,
                )?;
                let path = uri_to_fs_path(&uri).ok_or("Expected file URI")?;
                let project = self
                    .projects
                    .select(&path, None)
                    .map_err(|error| error.to_string())?;
                return Ok(tex_ls_protocol::projects::inspect(
                    &self
                        .db
                        .snapshot_for(project)
                        .map_err(|error| error.to_string())?,
                    &path,
                ));
            }
            "workspace/willRenameFiles" => {
                let files = tex_ls_protocol::file_operations::decode_files(&params)
                    .ok_or("Invalid rename files")?;
                let snapshots: Vec<_> = self
                    .projects
                    .projects()
                    .map(|project| self.db.snapshot_for(project).expect("registered project"))
                    .collect();
                return encode(tex_ls_protocol::file_operations::will_rename(
                    &snapshots,
                    &files,
                    self.encoding,
                ));
            }
            "workspace/didRenameFiles" => {
                let files = tex_ls_protocol::file_operations::decode_files(&params)
                    .ok_or("Invalid rename files")?;
                self.projects.rename_files(&mut self.db, &files);
                let moved: Vec<_> = files
                    .iter()
                    .filter_map(|(old, new)| {
                        let old_uri = path_to_uri(old)?;
                        let new_uri = path_to_uri(new)?;
                        Some((new.clone(), new_uri, self.documents.remove(&old_uri)?))
                    })
                    .collect();
                let settings: Vec<_> = files
                    .iter()
                    .filter_map(|(old, new)| {
                        self.source_settings
                            .remove(old)
                            .map(|settings| (new.clone(), settings))
                    })
                    .collect();
                self.source_settings.extend(settings);
                for (new, uri, mut document) in moved {
                    document.project = self
                        .projects
                        .select(&new, None)
                        .map_err(|error| error.to_string())?;
                    document.path = new;
                    self.documents.insert(uri, document);
                }
                return Ok(Value::Null);
            }
            "workspace/diagnostic" => {
                let params: lsp_types::WorkspaceDiagnosticParams = decode(params)?;
                return Ok(diagnostic_store::workspace_diagnostics(
                    &self
                        .projects
                        .projects()
                        .map(|project| self.db.snapshot_for(project).expect("registered project"))
                        .collect::<Vec<_>>(),
                    &encode(params.previous_result_ids)?,
                    self.encoding,
                    |path| {
                        self.source_settings
                            .get(path)
                            .unwrap_or(&self.settings)
                            .rules
                            .clone()
                    },
                    |path| {
                        path_to_uri(path)
                            .and_then(|uri| self.documents.get(&uri).map(|doc| doc.version))
                    },
                ));
            }
            "workspace/symbol" => {
                return encode(compute_projects_workspace_symbols(
                    &self
                        .projects
                        .projects()
                        .map(|project| self.db.snapshot_for(project).expect("registered project"))
                        .collect::<Vec<_>>(),
                    params.get("query").and_then(Value::as_str).unwrap_or(""),
                    self.encoding,
                    &|path| {
                        self.source_settings
                            .get(path)
                            .unwrap_or(&self.settings)
                            .outline
                            .clone()
                    },
                    self.active_path.as_deref(),
                ));
            }
            "completionItem/resolve" => {
                let item = decode(params)?;
                let project = completion_resolve::source_path(&item)
                    .map(|path| self.projects.select(&path, None))
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .unwrap_or(self.db.project_id());
                return encode(completion_resolve::resolve(
                    &self.db.snapshot_for(project).expect("registered project"),
                    item,
                ));
            }
            _ => {}
        }

        let uri: Uri = decode(
            params
                .get("textDocument")
                .and_then(|d| d.get("uri"))
                .cloned()
                .ok_or("Missing textDocument.uri")?,
        )?;
        let document = self.documents.get(&uri).ok_or("Document is not open")?;
        let snapshot = self
            .db
            .snapshot_for(document.project)
            .expect("registered project");
        let path = &document.path;
        let kind = file_kind_for(path);
        let settings = self
            .source_settings
            .get(&normalize_path(path))
            .unwrap_or(&self.settings);
        let position =
            || decode::<Position>(params.get("position").cloned().ok_or("Missing position")?);

        match method {
            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/range" => {
                let range = if method.ends_with("/range") {
                    Some(decode(
                        params.get("range").cloned().ok_or("Missing range")?,
                    )?)
                } else {
                    None
                };
                Ok(tex_ls_protocol::semantic_tokens::compute(
                    &snapshot,
                    path,
                    self.encoding,
                    range,
                ))
            }
            "textDocument/completion" => encode(compute_completion(
                &snapshot,
                &uri,
                path,
                self.encoding,
                position()?,
            )),
            "textDocument/hover" => encode(hover::compute_hover(
                &snapshot,
                path,
                self.encoding,
                position()?,
            )),
            "textDocument/signatureHelp" => encode(signature_help::compute_signature_help(
                &snapshot,
                path,
                position()?,
                self.encoding,
            )),
            "textDocument/linkedEditingRange" => encode(compute_linked_editing(
                &snapshot,
                path,
                self.encoding,
                position()?,
            )),
            "textDocument/definition" => encode(compute_goto_definition(
                &snapshot,
                path,
                position()?,
                self.encoding,
            )),
            "textDocument/references" => encode(compute_references(
                &snapshot,
                path,
                position()?,
                params
                    .pointer("/context/includeDeclaration")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                self.encoding,
            )),
            "textDocument/documentHighlight" => encode(compute_document_highlight(
                &snapshot,
                path,
                self.encoding,
                position()?,
            )),
            "textDocument/prepareRename" => encode(
                compute_prepare_rename(&snapshot, path, self.encoding, position()?).map(
                    |(range, placeholder)| {
                        PrepareRenameResult::PrepareRenamePlaceholder(
                            lsp_types::PrepareRenamePlaceholder { range, placeholder },
                        )
                    },
                ),
            ),
            "textDocument/rename" => encode(compute_rename(
                &snapshot,
                path,
                position()?,
                params
                    .get("newName")
                    .and_then(Value::as_str)
                    .ok_or("Missing newName")?,
                self.encoding,
            )),
            "textDocument/documentSymbol" => encode(if kind == FileKind::Bib {
                compute_bib_symbols(&snapshot, path, self.encoding)
            } else {
                compute_symbols(&snapshot, path, self.encoding, &settings.outline)
            }),
            "textDocument/codeAction" => {
                let request: CodeActionParams = decode(params)?;
                encode(compute_code_actions(
                    &snapshot,
                    &uri,
                    path,
                    kind,
                    request.range,
                    request.context.only.as_deref(),
                    &settings.rules,
                    self.encoding,
                ))
            }
            "textDocument/diagnostic" => Ok(diagnostic_store::document_diagnostics(
                &snapshot,
                path,
                kind,
                &settings.rules,
                self.encoding,
                params.get("previousResultId").and_then(Value::as_str),
            )),
            "textDocument/inlayHint" => Ok(hints::compute(
                &snapshot,
                path,
                decode(params.get("range").cloned().ok_or("Missing range")?)?,
                self.encoding,
                &settings.inlay_hints,
            )),
            "textDocument/documentColor" => {
                let _: lsp_types::DocumentColorParams = decode(params.clone())?;
                encode(tex_ls_protocol::colors::document_colors(
                    &snapshot,
                    path,
                    self.encoding,
                ))
            }
            "textDocument/colorPresentation" => {
                let params: lsp_types::ColorPresentationParams = decode(params.clone())?;
                encode(tex_ls_protocol::colors::color_presentations(
                    &snapshot,
                    path,
                    self.encoding,
                    params.range,
                    params.color,
                ))
            }
            "textDocument/foldingRange" => {
                encode(compute_folding(&snapshot, path, self.encoding, kind))
            }
            "textDocument/selectionRange" => encode(compute_selection_range(
                &snapshot,
                path,
                self.encoding,
                kind,
                &decode::<Vec<Position>>(
                    params
                        .get("positions")
                        .cloned()
                        .ok_or("Missing positions")?,
                )?,
            )),
            "textDocument/documentLink" => {
                encode(compute_document_link(&snapshot, path, self.encoding, kind))
            }
            "textDocument/formatting" => encode(
                compute_format(
                    &snapshot,
                    path,
                    self.encoding,
                    settings.format.style,
                    kind,
                    settings.format.sentence_options(),
                )
                .unwrap_or_default(),
            ),
            "textDocument/rangesFormatting" => encode(
                tex_ls_protocol::formatting::compute_ranges_format(
                    &snapshot,
                    path,
                    self.encoding,
                    settings.format.style,
                    kind,
                    &decode::<Vec<Range>>(params.get("ranges").cloned().ok_or("Missing ranges")?)?,
                    settings.format.sentence_options(),
                )
                .unwrap_or_default(),
            ),
            "textDocument/rangeFormatting" => encode(
                compute_range_format(
                    &snapshot,
                    path,
                    self.encoding,
                    settings.format.style,
                    kind,
                    decode(params.get("range").cloned().ok_or("Missing range")?)?,
                    settings.format.sentence_options(),
                )
                .unwrap_or_default(),
            ),
            "textDocument/onTypeFormatting" => encode(
                compute_on_type_format(
                    &snapshot,
                    path,
                    self.encoding,
                    settings.format.style,
                    kind,
                    position()?,
                    settings.format.sentence_options(),
                )
                .unwrap_or_default(),
            ),
            _ => Err(format!("Unsupported embedded method: {method}")),
        }
    }
}

use lsp_types::*;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tex_ls_analysis::{
    incremental::IncrementalDatabase,
    source::FileKind,
    text::{PositionEncoding, TextBuffer},
};
