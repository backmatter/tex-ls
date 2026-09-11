//! In-memory host for the same language computations as the native LSP server.
//!
//! The caller owns transport, file discovery, persistence, and scheduling. A
//! session processes writes in order and answers against the resulting revision.

use meaning_analysis::incremental::ProjectId;
use meaning_protocol::projects::{ProjectRegistry, workspace_roots};
use meaning_protocol::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

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
    initialized: bool,
    documents: HashMap<Uri, Document>,
    encoding: PositionEncoding,
    host: crate::manifest::ManifestHost,
    settings: crate::settings::ResolvedSettings,
    source_settings: HashMap<PathBuf, crate::settings::ResolvedSettings>,
}

impl Default for Session {
    fn default() -> Self {
        let db = IncrementalDatabase::default();
        Self {
            projects: ProjectRegistry::new(db.project_id()),
            initialized: false,
            db,
            documents: HashMap::new(),
            encoding: PositionEncoding::Utf16,
            host: crate::manifest::ManifestHost::default(),
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
        text: Option<Arc<meaning_analysis::text::SourceText>>,
    ) -> Result<(), String> {
        let project = self
            .projects
            .select(path, None)
            .map_err(|error| error.to_string())?;
        if text.is_some() {
            self.host.files.insert(crate::manifest::normalize(path));
        } else {
            self.host.files.remove(&crate::manifest::normalize(path));
        }
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
            let path = crate::manifest::normalize(path);
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
        let path = crate::manifest::normalize(path);
        self.source_settings.remove(&path);
        let project = self
            .projects
            .select(&path, None)
            .expect("unique workspace roots");
        self.db
            .clear_project_source_declarations(project, &path)
            .expect("registered project");
    }

    /// Handle LSP method parameters without stdio, threads, or a JSON-RPC loop.
    /// Positions use UTF-16, matching browsers and LSP's default encoding.
    pub fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "meaning/updateSettings" => {
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
                if self.initialized || !self.documents.is_empty() {
                    return Err("Session is already initialized".into());
                }
                self.projects =
                    ProjectRegistry::from_roots(&mut self.db, &workspace_roots(&params));
                self.initialized = true;
                let capabilities = server_capabilities(self.encoding, true);
                return Ok(json!({"capabilities": capabilities}));
            }
            "initialized" | "shutdown" | "exit" => return Ok(Value::Null),
            "meaning/registerFile" => {
                let uri: Uri = decode(params.get("uri").cloned().ok_or("Missing uri")?)?;
                let path = uri_to_fs_path(&uri).ok_or("Expected a file URI")?;
                self.host.files.insert(crate::manifest::normalize(&path));
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
                self.host.overlays.insert(crate::manifest::normalize(&path));
                let text = Arc::new(TextBuffer::new(item.text, self.encoding));
                if let Some(settings) = self.source_settings.get(&crate::manifest::normalize(&path))
                {
                    self.db
                        .set_project_source_declarations(
                            project,
                            &path,
                            settings.declarations.clone(),
                        )
                        .expect("registered project");
                }
                self.db
                    .open_overlay(project, &path, text.text_arc())
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
                if let Some(path) = uri_to_fs_path(&close.text_document.uri) {
                    self.host
                        .overlays
                        .remove(&crate::manifest::normalize(&path));
                }
                if let Some(document) = self.documents.remove(&close.text_document.uri) {
                    self.db
                        .close_overlay(document.project, &document.path)
                        .expect("registered project");
                }
                return Ok(Value::Null);
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
            .get(&crate::manifest::normalize(path))
            .unwrap_or(&self.settings);
        let position =
            || decode::<Position>(params.get("position").cloned().ok_or("Missing position")?);
        let build = &self.host;

        match method {
            "textDocument/completion" => encode(CompletionList {
                apply_kind: None,
                item_defaults: None,
                is_incomplete: true,
                items: compute_completion(
                    &snapshot,
                    &uri,
                    path,
                    self.encoding,
                    position()?,
                    &self.host,
                ),
            }),
            "textDocument/hover" => encode(hover::compute_hover(
                &snapshot,
                path,
                self.encoding,
                position()?,
                build,
            )),
            "textDocument/signatureHelp" => encode(signature_help::compute_signature_help(
                &snapshot,
                path,
                position()?,
                self.encoding,
            )),
            "textDocument/definition" => encode(compute_goto_definition(
                &snapshot,
                path,
                position()?,
                &self.host,
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
                compute_symbols(&snapshot, path, self.encoding, build)
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
            "textDocument/diagnostic" => encode(diagnostic_report(
                compute_diagnostics(&snapshot, path, kind, &settings.rules, self.encoding),
                params.get("previousResultId").and_then(Value::as_str),
            )),
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
            "textDocument/documentLink" => encode(compute_document_link(
                &snapshot,
                path,
                self.encoding,
                kind,
                &self.host,
            )),
            "textDocument/formatting" => encode(
                compute_format(
                    &snapshot,
                    path,
                    self.encoding,
                    settings.format.style,
                    kind,
                    settings.format.sentence_options(),
                )
                .into_iter()
                .collect::<Vec<_>>(),
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
use meaning_analysis::{
    incremental::IncrementalDatabase,
    source::FileKind,
    text::{PositionEncoding, TextBuffer},
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
