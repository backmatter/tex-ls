//! Native language-server transport, settings, scheduling, and external integrations.

mod forward_search;
mod services;
mod task_pool;
use services::NativeServices;

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, never, select, unbounded};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    ApplyWorkspaceEditParams, CodeActionKind, CodeActionParams, CompletionItem, CompletionList,
    CompletionParams, CompletionResponse, DefinitionParams, Diagnostic,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentDiagnosticParams, DocumentDiagnosticReport,
    DocumentDiagnosticReportProgress, DocumentFormattingParams, DocumentHighlightParams,
    DocumentLinkParams, DocumentOnTypeFormattingParams, DocumentRangeFormattingParams,
    DocumentSymbolParams, DocumentSymbolResponse, ExecuteCommandParams, FileChangeType,
    FileSystemWatcher, FoldingRangeParams, GlobPattern, HoverParams, Position, PrepareRenameResult,
    PublishDiagnosticsParams, Range, ReferenceParams, Registration, RegistrationParams,
    RelatedFullDocumentDiagnosticReport, RenameParams, SelectionRangeParams, ShowDocumentParams,
    SignatureHelpParams, TextDocumentPositionParams, Uri, WorkspaceEdit, WorkspaceSymbolParams,
};
use lsp_types::{
    ApplyWorkspaceEditRequest, CodeActionRequest, CompletionRequest, CompletionResolveRequest,
    DefinitionRequest, DiagnosticRefreshRequest, DocumentDiagnosticRequest,
    DocumentFormattingRequest, DocumentHighlightRequest, DocumentLinkRequest,
    DocumentOnTypeFormattingRequest, DocumentRangeFormattingRequest, DocumentSymbolRequest,
    ExecuteCommandRequest, FoldingRangeRequest, HoverRequest, PrepareRenameRequest,
    ReferencesRequest, RegistrationRequest, RenameRequest, Request as _, SelectionRangeRequest,
    ShowDocumentRequest, SignatureHelpRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    DidChangeConfigurationNotification, DidChangeTextDocumentNotification,
    DidChangeWatchedFilesNotification, DidCloseTextDocumentNotification,
    DidOpenTextDocumentNotification, Notification as _, PublishDiagnosticsNotification,
};
use serde::Deserialize;

use crate::config::BuildConfig;
use crate::declarations::ResolvedDeclarations;
use crate::file_discovery::{ExcludeFilter, collect_lint_files};
use crate::formatter::sentence::SentenceLanguage;
use crate::formatter::{FormatStyle, SentenceOptions, WrapMode};
use crate::incremental::{Analysis, IncrementalDatabase};
use crate::linter::RuleSelection;
use crate::parser::Edit;
use crate::project::BibTarget;
use crate::texmf::InstalledPackages;
use crate::text::{IntoSourceText, PositionEncoding, TextBuffer};
use forward_search::{ForwardSearchRequest, ForwardSearchStatus};
use meaning_analysis::source::{FileKind, file_kind_or_tex};
use meaning_protocol::*;

use task_pool::{Spawner, TaskPool, read_pool_size};

type DynError = Box<dyn std::error::Error + Sync + Send>;

pub fn run() -> Result<(), DynError> {
    let (connection, io_threads) = Connection::stdio();
    serve(connection)?;
    io_threads.join()?;
    Ok(())
}

pub fn serve(connection: Connection) -> Result<(), DynError> {
    let (initialize_id, init_params) = connection.initialize_start()?;
    let encoding = negotiate_position_encoding(&init_params);
    let (supports_pull_diagnostics, _) = client_diagnostic_support(&init_params);
    let capabilities =
        serde_json::to_value(native_capabilities(encoding, supports_pull_diagnostics))?;
    connection.initialize_finish(
        initialize_id,
        serde_json::json!({ "capabilities": capabilities }),
    )?;
    main_loop(connection, init_params, encoding)
}

mod capabilities;
use capabilities::*;
mod state;
use state::*;
mod settings;
use settings::*;
mod messages;
use messages::*;
mod inverse_search;
use inverse_search::*;
mod event_loop;
use event_loop::*;
mod synchronization;
use synchronization::*;
mod requests;
use requests::*;
mod worker;
use worker::*;
mod responses;
use responses::*;
#[cfg(test)]
mod tests;
