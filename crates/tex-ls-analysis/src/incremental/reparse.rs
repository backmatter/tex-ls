//! Reparse for incremental analysis.
use super::*;

/// The previous parse an incremental reparse splices against, plus the inputs it
/// was produced under.
///
/// All four inputs are carried, not just the text, because a base is only usable
/// for a parse that would have used the same ones: `config` is fixed per file by
/// its extension, `declared` changes when `tex-ls.toml` does, and `ctx` is the
/// scanned context the tree was parsed with (a tier that relexes a fragment must
/// use the same one, or a `\newcommand` the scan found makes the fragment's tokens
/// disagree with the tree's).
///
/// `text` is an `Arc<SourceText>` shared with the tracked input and the live editor
/// buffer, so storing a base costs a refcount bump rather than a copy.
#[derive(Debug, Clone)]
pub struct PrevParse {
    pub text: Arc<SourceText>,
    pub green: rowan::GreenNode,
    pub errors: Vec<SyntaxError>,
    pub ctx: ParseCtx,
    pub config: LexConfig,
    /// The *parse-facing* half of the declarations this base was parsed under
    /// (`parse_declarations`), not the whole block. Storing the whole block
    /// would make `is_current` reject a base over a
    /// `[commands]` edit that provably could not have changed the tree.
    pub declared: ResolvedDeclarations,
}

impl PrevParse {
    /// Whether this base already *is* the parse being asked for — same text, same
    /// inputs. The tree can then be handed back whole.
    pub(super) fn is_current(
        &self,
        text: &Arc<SourceText>,
        config: LexConfig,
        declared: &ResolvedDeclarations,
    ) -> bool {
        self.config == config
            && &self.declared == declared
            // `ptr_eq` is the free half: the language server hands the query the
            // same allocation it wrote. The content compare behind it is what makes
            // the guard correct for an equal-but-distinct text (a disk re-read).
            && (Arc::ptr_eq(&self.text, text) || *self.text == **text)
    }

    /// Borrow this base in the shape the parser's reparse entry points take.
    pub(super) fn as_reparse_base<'a>(
        &'a self,
        declared: &'a ResolvedDeclarations,
    ) -> ReparseBase<'a> {
        ReparseBase::from_parts(
            &self.text,
            &self.green,
            &self.errors,
            &self.ctx,
            self.config,
            declared,
        )
    }
}

/// A BibTeX base shares the source-owned editor cache and staged edit chain.
pub struct PrevBibParse {
    pub(super) text: Arc<SourceText>,
    pub(super) parsed: crate::bib::Parse,
}

/// One file's entry in the [`ReparseCache`].
#[derive(Default, Clone)]
pub struct FileReparseState {
    pub(super) generation: u64,
    pub(super) prev: Option<Arc<PrevParse>>,
    pub(super) bib_prev: Option<Arc<PrevBibParse>>,
    pub(super) pending: Vec<Edit>,
}

/// Reparse bases and pending edit chains, keyed by file.
///
/// Base and chain live under **one** lock because a store must advance both
/// atomically: the store runs on whichever thread demanded the parse while a stage
/// runs on the language server's worker, and a drain that raced a stage would
/// either lose an edit or keep a stale one.
#[derive(Default)]
pub(super) struct ReparseCache {
    pub(super) files: HashMap<SourceInput, FileReparseState>,
}
