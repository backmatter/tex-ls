//! Native CLI and language-server host.
pub mod bibliography;
pub mod cli;
pub mod config;
pub mod file_discovery;
pub mod ipc;
pub mod lsp;
mod native_path;
pub mod texmf;
use tex_ls_analysis::{bib, incremental, linter, project, text};
use tex_ls_formatter::formatter;
use tex_ls_parser::{declarations, parser, semantic, syntax};

pub mod format;
pub mod package_source;

#[cfg(all(test, unix))]
#[path = "../tests/support/native_paths.rs"]
mod test_native_paths;
