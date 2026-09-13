//! Cross-file inclusion, package, label, citation, and option facts.
//! Every query derives from explicit source inputs and project membership.

// The file is named `auxfile.rs` rather than `aux.rs` because `aux` is a
// reserved device name on Windows and git refuses to check out such a path.
#[path = "project/auxfile.rs"]
pub mod aux;
pub mod citations;
pub mod graph;
pub mod include;
pub mod labels;
pub mod options;
pub mod package;
pub mod texmf;

pub use citations::{CiteFileFacts, ResolvedCitations, document_cite_names};
pub use graph::{
    FileFacts, IncludeGraph, PackageFileFacts, PackageGraph, Project, ProjectMember,
    ResolvedInclude, ResolvedLoad, UnresolvedInclude, UnresolvedLoad,
};
pub use include::{
    BibTarget, IncludeEdge, IncludeEdgeKey, IncludeKind, IncludeTarget,
    collect_bib_resource_targets, collect_include_edge_keys, collect_include_edges,
};
pub use labels::ResolvedLabels;
pub use options::{PackageOptionFacts, ResolvedPackageOptions, package_option_facts};
pub use package::{
    OptionArg, PackageEdge, PackageEdgeKey, PackageKind, PackageTarget, collect_package_edge_keys,
    collect_package_edges, dtx_source_of, load_option_args, resolve_load_target,
};
pub use texmf::TexmfIndex;

pub(crate) use citations::resolved_citations;
pub(crate) use graph::{package_graph, project_graph, workspace_project};
pub(crate) use labels::resolved_labels;

pub(crate) mod root_views;
