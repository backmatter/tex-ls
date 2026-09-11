//! Incremental LaTeX analysis. All source and project facts are explicit inputs.
pub mod bib;
pub mod completion;
pub mod external;
pub mod incremental;
pub mod linter;
pub mod project;
pub mod source;
pub mod text;

use meaning_formatter::formatter;
use meaning_parser::{ast, declarations, directives, parser, semantic, syntax};
