//! BibTeX completion, links, linting, and outlines over shared parsing and formatting.

pub use meaning_formatter::bib::*;

pub mod completion;
pub mod document_link;
pub mod linter;
pub mod outline;
