//! Inputs for incremental analysis.
use super::*;

#[salsa::input]
pub struct SourceInput {
    /// The path this file was tracked under. Set once at creation and never
    /// mutated, so path-keyed queries (which later items will add) don't re-run
    /// on a text edit. In-memory files (see [`Database::add_file`])
    /// get a unique synthetic path so they never collide.
    ///
    /// Constructed at [`Durability::HIGH`](salsa::Durability::HIGH) (it is never
    /// `set_`), while `text` keeps the `LOW` default because it mutates per
    /// keystroke. Salsa's per-field revision tracking already keeps a path-only
    /// query from re-running on a text edit; the `HIGH` marking adds the coarse
    /// global short-circuit that starts to matter once a genuinely
    /// rarely-changing input (config, package metadata) is promoted into salsa —
    /// such inputs must likewise be constructed `HIGH`/`MEDIUM`, or every
    /// keystroke's `LOW` write would invalidate them.
    #[returns(ref)]
    pub path: PathBuf,
    /// The file's current text, as a shared handle rather than a `String`.
    ///
    /// A language-server keystroke moves this text through several hands — the
    /// live buffer, the worker job, the salsa cell, every in-flight read job —
    /// and each of them only ever reads it. `Arc<SourceText>` makes all but the first
    /// of those a refcount bump, and gives the staleness guards
    /// ([`text_is_current`](Database::text_is_current)) a pointer
    /// comparison in front of the `O(N)` content compare.
    #[returns(ref)]
    pub text: Arc<SourceText>,
    #[returns(ref)]
    pub declarations: ResolvedDeclarations,
    pub identity: SourceId,
    pub revision: u64,
    pub project: ProjectId,
}

/// One project's explicit membership, default declarations, and bibliography aliases.
///
/// Paths live on the immutable [`SourceInput`] inputs, so the file handles are
/// sufficient to derive the complete, canonically ordered [`ProjectMember`]
/// snapshot. Membership changes much less often than buffer text and is always
/// written at [`Durability::MEDIUM`](salsa::Durability::MEDIUM), preserving the
/// per-file firewalls across ordinary edits.
#[salsa::input]
pub struct ProjectInput {
    pub identity: ProjectId,
    #[returns(ref)]
    pub files: Vec<SourceInput>,
    #[returns(ref)]
    pub declarations: ResolvedDeclarations,
    #[returns(ref)]
    pub locations: std::collections::BTreeMap<PathBuf, LocationInput>,
    #[returns(ref)]
    pub installed: Arc<crate::external::Observation<crate::external::InstalledMetadata>>,
    #[returns(ref)]
    pub compiler: std::collections::BTreeMap<PathBuf, CompilerInput>,
}

#[salsa::input]
pub struct LocationInput {
    #[returns(ref)]
    pub observation: crate::external::LocationObservation,
    #[returns(ref)]
    pub file_resolution: crate::external::Observation<PathBuf>,
}

#[salsa::input]
pub struct CompilerInput {
    #[returns(ref)]
    pub artifact: Arc<crate::external::Observation<crate::external::CompilerArtifact>>,
}

/// Equality projections keep semantic-only settings out of parse dependencies.
#[salsa::tracked(returns(ref))]
pub(super) fn parse_declarations_of(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> ResolvedDeclarations {
    file.declarations(db).parse_tier()
}

#[salsa::tracked(returns(ref))]
pub(super) fn semantic_declarations_of(
    db: &dyn IncrementalDb,
    file: SourceInput,
) -> ResolvedDeclarations {
    file.declarations(db).semantic_tier()
}

/// Positive aliases are an equality projection of explicit observations. Errors
/// and unknown candidates cannot accidentally become successful resolutions.
#[salsa::tracked(returns(ref))]
pub(crate) fn file_aliases(
    db: &dyn IncrementalDb,
    project: ProjectInput,
) -> Vec<(PathBuf, PathBuf)> {
    project
        .locations(db)
        .iter()
        .filter_map(|(path, input)| match input.file_resolution(db) {
            crate::external::Observation::Present(actual) => Some((path.clone(), actual.clone())),
            _ => None,
        })
        .collect()
}
