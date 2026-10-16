//! Shared LSP operations over immutable analysis snapshots and explicit host facts.

#[cfg(test)]
#[macro_use]
#[path = "../../../tests/support/paths.rs"]
mod test_paths;
pub mod code_action;
// Public compatibility paths retain the completion subsystem's owned modules.
pub use completion::{rank as completion_rank, resolve as completion_resolve};
pub mod document_link;
pub mod file_operations;
pub mod folding;
pub mod hover;
pub use tex_ls_analysis::name_refs;
pub mod hints;
pub mod presentation;
pub mod projects;
pub mod selection_range;
pub mod signature_help;

#[cfg(test)]
mod test_host;

pub mod response_policy;
pub use response_policy::ResponsePolicy;
pub mod capabilities;
pub use capabilities::*;
pub mod convert;
pub use convert::*;
pub mod synchronization;
pub use synchronization::*;
pub mod diagnostic_store;
pub mod diagnostics;
pub use diagnostics::*;
pub mod actions;
mod fix_all;
pub use actions::*;
pub mod formatting;
pub use formatting::*;
pub mod symbols;
pub use symbols::*;
pub mod structure;
pub use structure::*;
pub mod context;
pub use context::*;
pub mod completion;
pub use completion::{
    bib_candidate_to_item, candidate_to_item, cite_completion_items, compute_bib_completion,
    compute_completion, compute_tex_completion, file_completion_items, file_stem,
    glossary_completion_items, has_extension, package_completion_items,
};
pub mod bib_strings;
pub mod glossary;
pub mod navigation;
pub use navigation::*;

#[cfg(test)]
mod tests;

pub mod lifecycle;

pub mod workspace_edits;

pub mod semantic_token_store;
pub mod semantic_tokens;

mod command_docs;
pub mod source_cards;

#[cfg(test)]
mod audit_tests;

pub mod colors;
