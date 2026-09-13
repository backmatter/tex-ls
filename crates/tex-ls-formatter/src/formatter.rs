//! The formatter: parse → lower CST to a Wadler/Prettier `Ir` → print.
//!
//! The layout engine (`ir`, `printer`, `style`, `context`) selects line breaks
//! and indentation. The LaTeX-specific lowering lives in [`core`].

pub(crate) mod colspec;
pub(crate) mod context;
pub mod core;
pub(crate) mod ir;
pub mod perturb;
pub(crate) mod printer;
pub mod sentence;
pub mod style;

pub use colspec::column_count;
pub use core::{
    FormatError, declared_scope, format, format_node, format_node_range_with_signatures,
    format_node_range_with_signatures_sentence, format_node_with_signatures,
    format_node_with_signatures_sentence, format_with_declarations_sentence, format_with_style,
    format_with_style_flavored, format_with_style_flavored_sentence,
    format_with_style_flavored_with_signatures,
};
pub use sentence::SentenceOptions;
pub use style::{FormatStyle, ItemIndent, LineEnding, MathWrap, WrapMode};
