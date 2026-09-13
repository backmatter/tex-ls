//! Shared LSP operations over immutable analysis snapshots and explicit host facts.

#[cfg(test)]
#[macro_use]
#[path = "../../../tests/support/paths.rs"]
mod test_paths;
pub mod code_action;
mod completion_context;
mod completion_edits;
pub mod completion_rank;
pub mod completion_resolve;
pub mod document_link;
pub mod file_operations;
pub mod folding;
pub mod hover;
pub use tex_ls_analysis::name_refs;
pub mod hints;
pub mod presentation;
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

use tex_ls_analysis::bib::completion::{
    BibCandidateKind, BibCompletionCandidate, BibCompletionContext, bib_candidates,
    classify_bib_context,
};
use tex_ls_analysis::bib::format_node as bib_format_node;
use tex_ls_analysis::bib::outline::{BibOutlineItem, outline as bib_outline};
use tex_ls_analysis::bib::semantic::Model as BibModel;
use tex_ls_analysis::completion::{
    CandidateKind, CompletionCandidate, CompletionContext, FileArgKind,
};
use tex_ls_analysis::incremental::Analysis;
use tex_ls_analysis::linter::{RuleSelection, Severity};
use tex_ls_analysis::project::aux::AuxData;
use tex_ls_analysis::project::texmf::TexmfIndex;
use tex_ls_analysis::project::{ResolvedCitations, ResolvedLabels};
use tex_ls_analysis::source::{FileKind, file_kind_or_tex};
use tex_ls_analysis::text::{LineIndex, LineTable, PositionEncoding, TextBuffer};
use tex_ls_formatter::formatter::{
    FormatStyle, SentenceOptions, format_node_range_with_signatures_sentence,
    format_node_with_signatures_sentence,
};
use tex_ls_parser::ast::{AstNode, Environment};
use tex_ls_parser::declarations::ResolvedDeclarations;
use tex_ls_parser::parser::Edit;
#[cfg(test)]
use tex_ls_parser::parser::{parse, parse_with_declarations};
use tex_ls_parser::semantic::{
    DefSiteKind, OutlineItem, OutlineSymbol, SemanticModel, SignatureDb,
};
use tex_ls_parser::syntax::{SyntaxKind, SyntaxNode};

pub(crate) use name_refs::{NameKind, NameTarget};

#[cfg(test)]
mod test_host;

pub mod response_policy;
pub use response_policy::ResponsePolicy;
pub mod capabilities;
pub use capabilities::*;
pub mod convert;
pub use convert::*;
pub mod synchronization;
pub use synchronization::*;
pub mod diagnostic_store;
pub mod diagnostics;
pub use diagnostics::*;
pub mod actions;
mod fix_all;
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
pub mod bib_strings;
pub mod glossary;
pub mod navigation;
pub use navigation::*;

#[cfg(test)]
mod tests;

pub mod lifecycle;

pub mod workspace_edits;

pub mod semantic_tokens;

pub mod source_cards;

#[cfg(test)]
mod audit_tests;

pub mod colors;
