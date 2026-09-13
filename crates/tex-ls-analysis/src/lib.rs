//! Incremental LaTeX analysis. All source and project facts are explicit inputs.
pub mod bib;
pub mod completion;
pub mod external;
pub mod incremental;
pub mod linter;
pub mod project;
pub mod source;
pub mod text;

use tex_ls_formatter::formatter;
use tex_ls_parser::{ast, declarations, directives, parser, semantic, syntax};

pub mod name_refs;

pub mod folding;
pub mod selection;

pub mod navigation;

pub mod hover;

pub mod colors;
