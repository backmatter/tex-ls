//! BibTeX formatting. Parsing and semantic data live in `tex-ls-parser`.

pub mod formatter;

pub use formatter::{FormatError, format, format_node, format_with_style};
