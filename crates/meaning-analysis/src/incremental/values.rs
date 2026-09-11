//! Values for incremental analysis.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    ParsedDocument,
    /// A file's per-file label/reference model ([`semantic_model`]).
    SemanticModel,
    /// A file's scanned `\newcommand`/`\newenvironment`/xparse signatures
    /// ([`document_signatures`]).
    DocumentSignatures,
    /// A `.dtx` file's documentation↔code associations ([`doc_associations`]).
    DocAssociations,
    /// A file's range-free inclusion edges ([`include_edges`]).
    IncludeEdges,
    /// A file's range-free package/class load edges ([`package_edges`]).
    PackageEdges,
    /// A file's sorted, distinct label-name set ([`file_labels`]) — the firewall
    /// the cross-file label resolver consumes.
    FileLabels,
    /// A file's sorted, distinct `\ref`-key set ([`file_refs`]) — the reference
    /// firewall the cross-file label resolver consumes for `unreferenced-label`.
    FileRefs,
    /// A file's sorted, distinct glossary/acronym key set
    /// ([`file_glossary_keys`]) — the firewall glossary key completion consumes.
    FileGlossaryKeys,
    /// Whether a file is a document root ([`file_is_document_root`]).
    FileIsDocumentRoot,
    /// The cross-file inclusion graph ([`crate::project::project_graph`]); a
    /// project-level query, not keyed on a single file.
    ProjectGraph,
    /// The plain, value-comparable project membership derived from
    /// [`ProjectInput`] ([`crate::project::workspace_project`]).
    WorkspaceProject,
    /// The cross-file package-load graph ([`crate::project::package_graph`]); a
    /// project-level query, not keyed on a single file.
    PackageGraph,
    /// A file's merged signature scope — its own definitions plus those of its
    /// transitively loaded local packages ([`scope_signatures`]).
    ScopeSignatures,
    /// The cross-file label resolution ([`crate::project::resolved_labels`]); a
    /// project-level query, not keyed on a single file.
    ResolvedLabels,
    /// A `.bib` file's parse tree ([`parsed_bib_document`]).
    ParsedBibDocument,
    /// A `.bib` file's per-file entry / cite-key / `@string` model
    /// ([`bib_semantic_model`]).
    BibSemanticModel,
    /// A `.bib` file's sorted, distinct cite-key set ([`file_cite_names`]) — the
    /// firewall the cross-file citation resolver consumes.
    FileCiteNames,
    /// A `.tex` file's bibliography-resource targets + `\nocite{*}` flag
    /// ([`file_cite_facts`]) — the per-file citation firewall.
    FileCiteFacts,
    /// The cross-file citation resolution ([`crate::project::resolved_citations`]);
    /// a project-level query, not keyed on a single file.
    ResolvedCitations,
    /// A `.sty` file's statically-declared option surface
    /// ([`file_package_option_facts`]) — the firewall the cross-file
    /// package-option resolver consumes.
    FilePackageOptionFacts,
    /// The cross-file package-option model
    /// ([`crate::project::options::resolved_package_options`]); a project-level
    /// query, not keyed on a single file.
    ResolvedPackageOptions,
    /// Shared raw LaTeX findings, including fixes and project dependencies.
    LatexLintFindings,
    /// Shared raw BibTeX findings, including fixes.
    BibLintFindings,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryLogEntry {
    pub kind: QueryKind,
    /// The per-file query subject, or `None` for project-level queries (none
    /// exist yet; the field is reserved so later items slot in mechanically).
    pub file: Option<SourceId>,
}

pub type ParseDiagnosticData = crate::parser::SyntaxError;
pub type ParsedDocument = Arc<PrevParse>;
pub type ParsedBibDocument = Arc<PrevBibParse>;
