//! The BibTeX side of the formatter: `meaning-parser`'s bib pipeline
//! re-exported at the old paths, plus the `.bib` formatter itself.

pub use meaning_parser::bib::*;

pub mod formatter;

pub use formatter::{FormatError, format, format_node, format_with_style};
