//! Native CLI and language-server host.
#[path = "auxfile.rs"]
pub mod aux;
pub mod bibliography;
pub mod cli;
pub mod config;
pub mod file_discovery;
pub mod ipc;
pub mod lsp;
pub mod texmf;
use meaning_analysis::{bib, incremental, linter, project, text};
use meaning_formatter::formatter;
use meaning_parser::{declarations, parser, semantic, syntax};

pub mod format;
pub mod package_source;
