//! BibTeX completion, links, linting, and outlines over shared parsing and formatting.

pub use tex_ls_formatter::bib::{FormatError, format, format_node, format_with_style};
pub use tex_ls_parser::bib::*;

pub mod completion;
pub mod document_link;
pub mod linter;
pub mod outline;

pub mod render;
