//! Incremental analysis inputs, queries, database ownership, and read snapshots.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::text::{IntoSourceText, SourceText};
use salsa::Setter;
use smol_str::SmolStr;

use crate::bib::semantic::Model as BibModel;
use crate::bib::syntax::SyntaxNode as BibSyntaxNode;
use crate::declarations::ResolvedDeclarations;
use crate::parser::{
    Edit, LexConfig, ParseCtx, ReparseBase, SyntaxError, parse_with_declarations_resolved,
    reparse_edits,
};
use crate::project::citations::document_cite_names;
use crate::project::labels::{
    document_glossary_keys, document_label_names, document_ref_names, is_document_root,
};
use crate::project::options::resolved_package_options;
use crate::project::{
    BibTarget, IncludeEdgeKey, PackageEdgeKey, PackageOptionFacts, ProjectMember,
    ResolvedCitations, ResolvedLabels, ResolvedPackageOptions, collect_bib_resource_targets,
    collect_include_edge_keys, collect_package_edge_keys, package_graph, package_option_facts,
    resolved_citations, resolved_labels, workspace_project,
};
use crate::semantic::{
    DocAssociation, SemanticModel, SignatureDb, doc_associations as build_doc_associations,
    scan_definitions,
};
use crate::source::file_kind_or_tex;
use crate::syntax::SyntaxNode;

#[salsa::db]
pub(crate) trait IncrementalDb: salsa::Database {
    fn record_query(&self, entry: QueryLogEntry);
    fn source_input(&self, id: SourceId, project: ProjectId) -> SourceInput;
    fn project_input(&self, id: ProjectId) -> ProjectInput;

    /// The reparse base for `file`, if one is cached.
    ///
    /// This and the four methods below are the **side channel**: mutable state read
    /// from inside an otherwise-pure tracked query. That is sound only because of
    /// the reparse's governing invariant — `parsed_document` returns exactly what
    /// `parse(text)` would whatever this cache holds, so a cold, stale, or evicted
    /// cache costs a full parse and nothing else. Nothing here is a salsa input, and
    /// nothing here may become one: a base that invalidated on write would defeat
    /// the point, and one that did not would be a lie to the dependency graph.
    ///
    /// Default-implemented so a database without a cache simply always full-parses.
    /// That keeps the query total for bare test databases and for any future
    /// non-editor host, without either having to opt out.
    fn reparse_state(&self, _file: SourceInput) -> FileReparseState {
        FileReparseState::default()
    }

    fn reparse_bib_store(
        &self,
        _file: SourceInput,
        _prev: Arc<PrevBibParse>,
        _consumed: usize,
        _generation: u64,
    ) {
    }

    fn reparse_prev(&self, _file: SourceInput) -> Option<Arc<PrevParse>> {
        None
    }

    /// Append `edits` to `file`'s pending chain, or clear it when `None`.
    ///
    /// `None` means "the text changed by a route carrying no edits" — a disk
    /// reload, a whole-buffer replacement, a sweep. Clearing rather than keeping is
    /// what makes the chain self-healing: a chain that no longer describes how the
    /// current text was reached can never describe it again.
    fn reparse_stage_edits(&self, _file: SourceInput, _edits: Option<Vec<Edit>>) {}

    /// Peek at `file`'s pending chain without draining it.
    ///
    /// Peek rather than take, because the query that reads this may be cancelled
    /// before it stores a result; draining here would lose the chain for the retry.
    /// [`reparse_store`](Self::reparse_store) drains the prefix that was consumed.
    fn reparse_pending_edits(&self, _file: SourceInput) -> Vec<Edit> {
        Vec::new()
    }

    /// Install `prev` as `file`'s base and drop the first `consumed` pending edits.
    fn reparse_store(
        &self,
        _file: SourceInput,
        _prev: Arc<PrevParse>,
        _consumed: usize,
        _generation: u64,
    ) {
    }

    /// Drop `file`'s base and chain outright.
    ///
    /// For the case where the buffer the base describes is *gone* rather than
    /// merely changed — a `didClose`, a revert to disk.
    #[cfg(test)]
    fn reparse_evict(&self, _file: SourceInput) {}
}

mod inputs;
pub(crate) use inputs::*;

mod values;
pub(crate) use values::*;

mod reparse;
pub(crate) use reparse::*;

mod queries;
pub(crate) use queries::*;

mod database;
pub(crate) use database::*;

#[cfg(test)]
mod tests;

mod host;
pub use host::*;
pub use queries::FileCiteFacts;
pub use values::{ParseDiagnosticData, QueryKind, QueryLogEntry};
