//! Shared LSP operations over immutable analysis snapshots and explicit host facts.
pub mod code_action;
pub mod completion_resolve;
pub mod document_link;
pub mod folding;
pub mod host;
pub mod hover;
pub mod name_refs;
pub mod projects;
pub mod selection_range;
pub mod signature_help;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{
    Code, CodeActionKind, CodeActionProvider, CodeActionResponse, CodeDescription, CompletionItem,
    CompletionItemKind, CompletionOptions, Diagnostic, DiagnosticOptions, DiagnosticProvider,
    DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag, DocumentHighlight,
    DocumentHighlightKind, DocumentLink, DocumentLinkOptions, DocumentOnTypeFormattingOptions,
    DocumentSymbol, FoldingRange, FoldingRangeProvider, HoverProvider, InsertTextFormat, Location,
    Position, PositionEncodingKind, Range, RenameOptions, SelectionRange, SelectionRangeProvider,
    ServerCapabilities, SignatureHelpOptions, SymbolKind, TextDocumentContentChangeEvent,
    TextDocumentSync, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit, WorkspaceSymbol,
    WorkspaceSymbolResponse,
};
use rowan::{TextRange, TextSize};
use smol_str::SmolStr;

use meaning_analysis::bib::completion::{
    BibCandidateKind, BibCompletionCandidate, BibCompletionContext, bib_candidates,
    classify_bib_context,
};
use meaning_analysis::bib::format_node as bib_format_node;
use meaning_analysis::bib::outline::{BibOutlineItem, outline as bib_outline};
use meaning_analysis::bib::semantic::Model as BibModel;
use meaning_analysis::completion::{
    CandidateKind, CompletionCandidate, CompletionContext, FileArgKind,
};
use meaning_analysis::incremental::Analysis;
use meaning_analysis::linter::{RuleSelection, Severity};
use meaning_analysis::project::aux::AuxData;
use meaning_analysis::project::texmf::TexmfIndex;
use meaning_analysis::project::{PackageGraph, ResolvedCitations, ResolvedLabels};
use meaning_analysis::source::{FileKind, file_kind_or_tex};
use meaning_analysis::text::{LineIndex, LineTable, PositionEncoding, TextBuffer};
use meaning_formatter::formatter::{
    FormatStyle, SentenceOptions, format_node_range_with_signatures_sentence,
    format_node_with_signatures_sentence,
};
use meaning_parser::ast::{AstNode, Environment};
use meaning_parser::declarations::ResolvedDeclarations;
use meaning_parser::parser::Edit;
#[cfg(test)]
use meaning_parser::parser::{parse, parse_with_declarations};
use meaning_parser::semantic::{
    DefSiteKind, OutlineItem, OutlineSymbol, SemanticModel, SignatureDb, outline,
    scan_definition_sites,
};
use meaning_parser::syntax::{SyntaxKind, SyntaxNode};

pub(crate) use name_refs::{NameKind, NameTarget};

pub use host::HostServices;

#[cfg(test)]
mod test_host;

pub mod capabilities;
pub use capabilities::*;
pub mod convert;
pub use convert::*;
pub mod synchronization;
pub use synchronization::*;
pub mod diagnostics;
pub use diagnostics::*;
pub mod actions;
pub use actions::*;
pub mod formatting;
pub use formatting::*;
pub mod symbols;
pub use symbols::*;
pub mod structure;
pub use structure::*;
pub mod context;
pub use context::*;
pub mod completion;
pub use completion::*;
pub mod navigation;
pub use navigation::*;

#[cfg(test)]
mod tests;
