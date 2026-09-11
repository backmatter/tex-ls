//! Internal-path shim: a project's declarations
//! ([`meaning_parser::declarations`]), re-exported so an embedder holding only
//! this crate can build the value
//! [`formatter::format_with_declarations_sentence`](crate::formatter::format_with_declarations_sentence)
//! takes without naming the parser crate.

pub use meaning_parser::declarations::*;
