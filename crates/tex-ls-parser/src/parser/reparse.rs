//! Incremental reparse: splice a small edit into the previous green tree instead
//! of re-parsing the whole text.
//!
//! # Contract
//!
//! A successful reparse must produce the same green tree and [`SyntaxError`]
//! vector as a full parse of the edited text. Incremental reparse is only a
//! performance optimization; a failed proof falls back to a full parse.
//!
//! Guards return [`None`] when they cannot prove equivalence. Extend them by
//! adding supported cases or conservative bailouts, never by weakening the oracle.
//!
//! The previous-parse cache cannot affect the query result. A cold, stale, or
//! evicted cache only forces a full parse.
//!
//! # Design
//!
//! The tiers sit strictly **on top of** [`parse_with_declarations_resolved`] and
//! [`crate::parser::lex_with`]. There is no incremental lexer, no token-stream reuse, no restarting
//! the grammar at an offset:
//!
//! - the token tier relexes one leaf in isolation, proves the relex is a
//!   single token of the same kind that joins to its neighbours the same way, and
//!   splices with rowan's [`rowan::SyntaxToken::replace_with`], sharing every green node
//!   off the leaf-to-root path, while rebuilding the ancestor child arrays;
//! - the protected-body tier splices the same way, but proves it differently: a raw
//!   capture cannot be relexed alone, so it relexes the leaf's whole enclosing node
//!   with its delimiters and requires that to reproduce the tree's own tokens;
//! - the math tier reparses the outermost enclosing delimiter-bearing math node,
//!   after the token tier declines a change to the virtual-atom partition;
//! - the region tier re-runs the *ordinary* parser over a substring and splices the
//!   resulting children under `ROOT`, using neighbour-sized boundary parses purely
//!   as proofs that the substring is decoupled from its context.
//!
//! This avoids checkpointing lexer state, prescan indices, or forward shape-gate
//! scans. The math and region tiers decline edits whose effects may escape their
//! fragments.

mod leaf;
mod math;
mod protected;
mod region;
mod token;

use rowan::{GreenNode, TextSize};

use crate::declarations::ResolvedDeclarations;
use crate::parser::core::{Parse, SyntaxError, parse_with_declarations_resolved};
use crate::parser::lexer::{LexConfig, ParseCtx, dtx_has_expl_signal};
use crate::syntax::SyntaxNode;

pub use crate::parser::edit::{Edit, apply_edits, diff_edit, try_apply_edits};

/// Which tier produced a [`Reparsed`]. Surfaced for tests and benchmarks, which
/// assert the tier a scenario reaches — a grammar change that silently downgrades
/// one should fail loudly rather than quietly show up as a slower number.
///
/// Ordered cheapest-first, so a chain can report the most expensive tier any of its
/// steps needed with `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReparseTier {
    /// One leaf token was relexed in isolation and spliced in place.
    Token,
    /// A protected body (`VERBATIM_BODY`, `VERB`) was relexed with its enclosing
    /// node's delimiters and spliced in place.
    Verbatim,
    /// A delimiter-bearing inline, display, or environment math fragment was
    /// reparsed and spliced in place.
    Math,
    /// A run of top-level children was reparsed and spliced under `ROOT`.
    Region,
}

/// A successful incremental reparse: the new whole-file green tree and its errors,
/// both in the *new* text's offsets.
#[derive(Debug, Clone)]
pub struct Reparsed {
    pub green: GreenNode,
    pub errors: Vec<SyntaxError>,
    pub tier: ReparseTier,
    /// Updated source-position facts, or `None` when the coherent base context
    /// remains valid. Callers retaining this result must also retain this context.
    pub ctx: Option<ParseCtx>,
}

impl Reparsed {
    /// Context paired with this result. Unchanged facts are borrowed from the
    /// coherent base supplied by the caller.
    pub fn context<'a>(&'a self, base: &'a ParseCtx) -> &'a ParseCtx {
        self.ctx.as_ref().unwrap_or(base)
    }
}

/// The previous parse a reparse splices against.
///
/// `ctx` is the context the tree was parsed under, from
/// [`parse_with_declarations_resolved`] or a preceding [`Reparsed::context`] —
/// a tier that relexes a fragment must use
/// the same one, or a `\newcommand` the definition scan found makes the fragment's
/// tokens disagree with the tree's. `config` and `declared` are the parse's other
/// two inputs, needed to reproduce it exactly.
#[derive(Debug, Clone, Copy)]
pub struct ReparseBase<'a> {
    pub text: &'a str,
    pub green: &'a GreenNode,
    pub errors: &'a [SyntaxError],
    pub ctx: &'a ParseCtx,
    pub config: LexConfig,
    /// The file-level `.dtx` implicit-expl signal (`%<@@=...>` / `\ProvidesExpl*`)
    /// computed from the full base text.
    ///
    /// The lexer derives this before tokenizing; fragment relexes need the same
    /// regime to be faithful.
    pub implicit_expl: bool,
    pub declared: &'a ResolvedDeclarations,
}

impl<'a> ReparseBase<'a> {
    /// Pair the source with the tree, errors, context, flavor and declarations
    /// returned by the same parse. The caller must keep these parts coherent;
    /// checking arbitrary tree contents would require a full-file traversal.
    pub fn from_parts(
        text: &'a str,
        green: &'a GreenNode,
        errors: &'a [SyntaxError],
        ctx: &'a ParseCtx,
        config: LexConfig,
        declared: &'a ResolvedDeclarations,
    ) -> Self {
        Self {
            text,
            green,
            errors,
            ctx,
            config,
            implicit_expl: implicit_expl_for(text, config),
            declared,
        }
    }

    /// Materialize a red-tree cursor over the base. Cheap (an atomic clone).
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }
}

fn implicit_expl_for(text: &str, config: LexConfig) -> bool {
    config.dtx && dtx_has_expl_signal(text)
}

/// Attempt an incremental reparse of `base` under `edit`, which transforms
/// `base.text` into `new_text`. [`None`] means no tier applied and the caller must
/// do a full parse.
///
/// The caller must supply exactly that text transform, just as it must supply
/// coherent parse parts to [`ReparseBase::from_parts`]. Use [`reparse_edits`] when
/// the transform has not already been established; that entry point validates
/// the entire result text before attempting a splice.
pub fn reparse(base: &ReparseBase<'_>, edit: &Edit, new_text: &str) -> Option<Reparsed> {
    if !spans_its_text(base.green, base.text) || !edit.fits(base.text) {
        return None;
    }
    reparse_one(base, edit, new_text)
}

/// [`reparse`] for a chain of edits, each expressed against the text its
/// predecessors produced — the shape an LSP `didChange` batch arrives in.
///
/// Replaying the chain is not the same as collapsing it: a diff of scattered edits
/// spans everything between them, which a cost guard declines outright, while the
/// chain splices each edit on its own.
pub fn reparse_edits(base: &ReparseBase<'_>, edits: &[Edit], new_text: &str) -> Option<Reparsed> {
    if edits.is_empty() || !spans_its_text(base.green, base.text) {
        return None;
    }

    // A single edit already has its resulting buffer. Prove that it describes
    // exactly that buffer before using it directly, avoiding two whole-file
    // allocations even when every tier will decline. This is the same transform
    // check as the replay below; the tier ladder and finish oracle are unchanged.
    if let [edit] = edits {
        if !edit.fits(base.text) {
            return None;
        }
        let insert_end = edit.range.start.checked_add(edit.insert.len())?;
        if new_text.get(..edit.range.start) != Some(&base.text[..edit.range.start])
            || new_text.get(edit.range.start..insert_end) != Some(edit.insert.as_str())
            || new_text.get(insert_end..) != Some(&base.text[edit.range.end..])
        {
            return None;
        }
        return reparse_one(base, edit, new_text);
    }

    // Compose touching edits without copying unrelated source text. Feed the
    // result through the same transform validation and tier proofs as one edit.
    // If a combined span has no local proof, retain individual replay below.
    if let Some(edit) = super::edit::coalesce_touching_edits(base.text, edits)
        && let Some(result) = reparse_edits(base, std::slice::from_ref(&edit), new_text)
    {
        return Some(result);
    }

    // Verify the chain describes exactly the transform claimed, then replay it. The
    // fold is deliberately *not* hoisted ahead of the splices as a pre-check: it
    // costs the same order as the work it would guard, and each step below already
    // validates against the text its predecessors produced.
    let mut text = base.text.to_string();
    let mut green = base.green.clone();
    let mut errors = base.errors.to_vec();
    let mut tier: Option<ReparseTier> = None;
    let mut ctx: Option<ParseCtx> = None;

    for edit in edits {
        if !edit.fits(&text) {
            return None;
        }
        let next = edit.apply(&text);
        let step = {
            let step_base = ReparseBase::from_parts(
                &text,
                &green,
                &errors,
                ctx.as_ref().unwrap_or(base.ctx),
                base.config,
                base.declared,
            );
            reparse_one(&step_base, edit, &next)?
        };
        if let Some(updated) = step.ctx {
            ctx = Some(updated);
        }
        text = next;
        green = step.green;
        errors = step.errors;
        tier = Some(tier.map_or(step.tier, |t| t.max(step.tier)));
    }

    // A stale chain can apply cleanly and still land somewhere other than the
    // buffer the caller is asking about. Reject it rather than answer for the wrong
    // text.
    if text != new_text {
        return None;
    }

    Some(Reparsed {
        green,
        errors,
        tier: tier?,
        ctx,
    })
}

/// Source-cached restrictions include inactive text. A tree-equivalent edit
/// must also leave their occurrences unchanged before reusing that context.
fn cached_guard_signals_unchanged(base: &ReparseBase<'_>, edit: &Edit, new_text: &str) -> bool {
    use super::embedded::{FULL_REPARSE_SIGNALS, LOCAL_REPARSE_SIGNALS};

    const fn longest(signals: &[&str]) -> usize {
        let mut longest = 0;
        let mut index = 0;
        while index < signals.len() {
            let length = signals[index].len();
            if length > longest {
                longest = length;
            }
            index += 1;
        }
        longest
    }
    const RADIUS: usize = {
        let full = longest(FULL_REPARSE_SIGNALS);
        let local = longest(LOCAL_REPARSE_SIGNALS);
        if full > local { full } else { local }
    };
    let radius = RADIUS;
    let mut start = edit.range.start.saturating_sub(radius);
    while !base.text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = edit.range.end.saturating_add(radius).min(base.text.len());
    while !base.text.is_char_boundary(end) {
        end += 1;
    }
    let Some(old) = base.text.get(start..end) else {
        return false;
    };
    let Some(new_end) = new_text.len().checked_sub(base.text.len() - end) else {
        return false;
    };
    let Some(new) = new_text.get(start..new_end) else {
        return false;
    };
    const fn byte_mask(text: &str) -> [u64; 4] {
        let mut mask = [0; 4];
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index] as usize;
            mask[byte / 64] |= 1 << (byte % 64);
            index += 1;
        }
        mask
    }
    const SIGNALS: [(&str, [u64; 4]); FULL_REPARSE_SIGNALS.len() + LOCAL_REPARSE_SIGNALS.len()] = {
        let mut signals = [("", [0; 4]); FULL_REPARSE_SIGNALS.len() + LOCAL_REPARSE_SIGNALS.len()];
        let mut index = 0;
        while index < signals.len() {
            let signal = if index < FULL_REPARSE_SIGNALS.len() {
                FULL_REPARSE_SIGNALS[index]
            } else {
                LOCAL_REPARSE_SIGNALS[index - FULL_REPARSE_SIGNALS.len()]
            };
            signals[index] = (signal, byte_mask(signal));
            index += 1;
        }
        signals
    };
    // Every byte of a literal must occur in the window. A missing byte proves
    // zero occurrences in both versions; all other signals keep exact counts.
    let mut present = [0u64; 4];
    for byte in old.bytes().chain(new.bytes()) {
        let byte = usize::from(byte);
        present[byte / 64] |= 1 << (byte % 64);
    }
    if SIGNALS
        .iter()
        .filter(|(_, required)| {
            required
                .iter()
                .zip(present)
                .all(|(required, present)| required & present == *required)
        })
        .any(|(signal, _)| old.match_indices(signal).count() != new.match_indices(signal).count())
    {
        return false;
    }

    // A raw capture may contain an inactive shortverb declaration whose operand
    // affects the conservative source cache. Do not claim operand invariance
    // from a body edit; ordinary ASCII WORD proofs already reject
    // potentially active letter-shortverb sources.
    // Every such declaration is also a full-parse signal. Its absence from the
    // coherent base source rules out an inactive operand without a tree lookup.
    if !base.ctx.reparse_guard.full {
        return true;
    }
    let root = base.syntax();
    let offset = TextSize::try_from(edit.range.start).ok();
    let Some(offset) = offset else { return false };
    if let Some(leaf) = crate::syntax::token_at_offset(&root, offset).right_biased()
        && matches!(
            leaf.kind(),
            crate::syntax::SyntaxKind::VERB
                | crate::syntax::SyntaxKind::VERBATIM_BODY
                | crate::syntax::SyntaxKind::COMMENT
        )
        && [
            "\\DefineShortVerb",
            "\\MakeShortVerb",
            "\\DeleteShortVerb",
            "\\UndefineShortVerb",
        ]
        .iter()
        .any(|signal| leaf.text().contains(signal))
    {
        return false;
    }
    true
}

/// The tier ladder for one already-validated edit, cheapest first.
///
/// Each tier lands here as an `.or_else` and returns through [`finish`], so none
/// can skip the length check or the oracle.
fn reparse_one(base: &ReparseBase<'_>, edit: &Edit, new_text: &str) -> Option<Reparsed> {
    // Width-changing edits before qualified definitions move their cached ends.
    // Same-width ordinary WORD edits can retain those positions, but must not
    // alter any definition body or source-sensitive cached signal.
    let before_qualified_profile = [
        base.ctx.verbatim_comment_start,
        base.ctx.release_header_start,
    ]
    .into_iter()
    .flatten()
    .any(|start| edit.range.start < start);
    if before_qualified_profile && edit.insert.len() != edit.range.len() {
        return None;
    }
    // Recovery consumed the rest of the file; any edit may change that boundary.
    if base
        .errors
        .iter()
        .any(|error| error.message == crate::NESTING_LIMIT_ERROR)
    {
        return None;
    }
    if !cached_guard_signals_unchanged(base, edit, new_text) {
        return None;
    }
    if before_qualified_profile {
        return token::reparse_ordinary_word(base, edit, new_text);
    }
    if base.ctx.reader_overrides_after_or_at(edit.range.start)
        || base.ctx.verbatim_comment_start.is_some()
        || base.ctx.release_header_start.is_some()
    {
        // Only these leaf proofs establish unchanged source definitions and
        // incoming lexer state. Rebase later reader positions after proving the
        // splice; other fragment/state proofs still decline here.
        let token = if base.ctx.reparse_guard.full {
            token::reparse_ordinary_word(base, edit, new_text)
        } else {
            token::reparse_letters(base, edit, new_text)
        };
        let mut result =
            token.or_else(|| protected::reparse_pure_builtin_capture(base, edit, new_text))?;
        if edit.insert.len() != edit.range.len()
            && base.ctx.reader_overrides_after_or_at(edit.range.start)
        {
            result.ctx = Some(base.ctx.after_leaf_edit(
                edit.range.start,
                edit.range.end,
                edit.insert.len(),
            )?);
            #[cfg(debug_assertions)]
            {
                let (_, full_ctx) =
                    parse_with_declarations_resolved(new_text, base.config, base.declared);
                debug_assert_eq!(
                    result.context(base.ctx),
                    &full_ctx,
                    "rebased context diverged from full parse"
                );
            }
        }
        return Some(result);
    }
    if base.ctx.reparse_guard.full {
        return token::reparse_ordinary_word(base, edit, new_text)
            .or_else(|| protected::reparse_pure_builtin_capture(base, edit, new_text));
    }
    if base.ctx.reparse_guard.local_modes {
        // Alphabetic WORD edits cannot toggle lexical state. The token tier
        // retains its ancestry, text-read, relex and neighbour proofs. Opaque
        // captures have their own old/new sequence proof. Math admits only
        // ASCII letters here, so no newly state-sensitive punctuation can enter
        // a fragment that already proved it parses faithfully in isolation.
        return token::reparse_letters(base, edit, new_text)
            .or_else(|| protected::reparse_protected(base, edit, new_text))
            .or_else(|| {
                edit.insert
                    .bytes()
                    .chain(base.text[edit.range.clone()].bytes())
                    .all(|byte| byte.is_ascii_alphabetic())
                    .then(|| math::reparse_math(base, edit, new_text))
                    .flatten()
            });
    }
    token::reparse_token(base, edit, new_text)
        .or_else(|| protected::reparse_protected(base, edit, new_text))
        .or_else(|| math::reparse_math(base, edit, new_text))
        .or_else(|| region::reparse_region(base, edit, new_text))
}

/// The single exit for every tier.
///
/// Routing all of them through one function is deliberate: a tier cannot return a
/// result without paying the every-build length check and the debug oracle, so
/// "did the new tier remember to verify?" is not a question a reviewer has to ask.
///
/// The length check is the release-build backstop. The oracle below is
/// `debug_assertions`-only because it costs a full parse, which would defeat the
/// point in the build that ships — but that is also the build whose formatter
/// rewrites the user's file, so *something* must hold there. A tree that does not
/// span exactly its text is the cheap, `O(1)`, always-affordable half of the
/// invariant, and it catches the whole class of offset-arithmetic bugs a splice can
/// have. It *falls back* rather than panicking, per the refusal-first contract.
fn finish(
    green: GreenNode,
    errors: Vec<SyntaxError>,
    tier: ReparseTier,
    base: &ReparseBase<'_>,
    new_text: &str,
) -> Option<Reparsed> {
    if !spans_its_text(&green, new_text) {
        return None;
    }
    let out = Reparsed {
        green,
        errors,
        tier,
        ctx: None,
    };
    assert_matches_full_parse(&out, base, new_text);
    Some(out)
}

/// Whether `green` spans exactly `text`. `O(1)` — rowan stores the width.
fn spans_its_text(green: &GreenNode, text: &str) -> bool {
    usize::from(green.text_len()) == text.len()
}

/// Render every node and token in preorder as `KIND@range "text"`.
///
/// Equal fingerprints mean byte-identical trees, and an unequal pair names the
/// first place they diverge, which a `GreenNode` inequality does not. Public (and
/// hidden) so the in-crate assert and the external harness share one definition of
/// "identical" and can never drift apart.
#[doc(hidden)]
pub fn fingerprint(node: &SyntaxNode) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for element in node.descendants_with_tokens() {
        match element {
            rowan::NodeOrToken::Node(n) => {
                let _ = writeln!(out, "{:?}@{:?}", n.kind(), n.text_range());
            }
            rowan::NodeOrToken::Token(t) => {
                let _ = writeln!(out, "{:?}@{:?} {:?}", t.kind(), t.text_range(), t.text());
            }
        }
    }
    out
}

/// Assert the governing invariant on a result about to be returned.
///
/// **Every failure here is an incremental-parser bug whose fix is a new
/// bail-to-full-parse condition, never a relaxation of this assert.** If a tier
/// produces a tree a full parse would not, the tier does not understand the
/// construct it just spliced, and the honest repair is to stop claiming it does.
#[cfg(debug_assertions)]
fn assert_matches_full_parse(result: &Reparsed, base: &ReparseBase<'_>, new_text: &str) {
    let full = full_parse(base, new_text);
    debug_assert_eq!(
        fingerprint(&SyntaxNode::new_root(result.green.clone())),
        fingerprint(&full.syntax()),
        "reparse ({:?}) produced a different tree than a full parse",
        result.tier,
    );
    debug_assert_eq!(
        result.errors, full.errors,
        "reparse ({:?}) produced different errors than a full parse",
        result.tier,
    );
}

#[cfg(not(debug_assertions))]
fn assert_matches_full_parse(_: &Reparsed, _: &ReparseBase<'_>, _: &str) {}

/// The full parse a reparse must agree with, under the base's own inputs.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn full_parse(base: &ReparseBase<'_>, text: &str) -> Parse {
    parse_with_declarations_resolved(text, base.config, base.declared).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::lexer::LatexFlavor;

    fn base_of(text: &str) -> (Parse, ParseCtx, ResolvedDeclarations) {
        let declared = ResolvedDeclarations::default();
        let (parse, ctx) = parse_with_declarations_resolved(text, LatexFlavor::Document, &declared);
        (parse, ctx, declared)
    }

    fn with_base<R>(text: &str, f: impl FnOnce(&ReparseBase<'_>) -> R) -> R {
        let (parse, ctx, declared) = base_of(text);
        f(&ReparseBase::from_parts(
            text,
            &parse.green,
            &parse.errors,
            &ctx,
            LatexFlavor::Document.into(),
            &declared,
        ))
    }

    fn edit(range: std::ops::Range<usize>, insert: &str) -> Edit {
        Edit {
            range,
            insert: insert.to_string(),
        }
    }

    #[test]
    fn an_edit_outside_a_plain_leaf_falls_back() {
        with_base("\\section{Hi}\n\nbody text\n", |base| {
            let e = edit(8..8, "x");
            assert!(reparse(base, &e, &e.apply(base.text)).is_none());
            let e = edit(7..10, "zz");
            assert!(reparse(base, &e, &e.apply(base.text)).is_none());
        });
    }

    #[test]
    fn an_edit_that_does_not_fit_the_base_is_refused() {
        with_base("abc\n", |base| {
            assert!(reparse(base, &edit(90..99, "x"), "abc\n").is_none());
            assert!(reparse(base, &edit(1..1, "x"), "abc\n").is_none());
        });
        with_base("α\n", |base| {
            assert!(reparse(base, &edit(1..1, "x"), "αx\n").is_none());
        });
    }

    #[test]
    fn an_empty_chain_is_refused() {
        with_base("abc\n", |base| {
            assert!(reparse_edits(base, &[], "abc\n").is_none());
        });
    }

    #[test]
    fn a_chain_that_lands_elsewhere_is_refused() {
        with_base("abc\n", |base| {
            assert!(reparse_edits(base, &[edit(0..0, "x")], "totally different").is_none());
        });
    }

    #[test]
    fn local_batches_preserve_full_parse_and_reject_stale_results() {
        with_base("\\sec\n", |base| {
            let edits = [edit(1..4, "p"), edit(2..2, "ro")];
            let next = super::super::apply_edits(base.text, &edits);
            let result = reparse_edits(base, &edits, &next).unwrap();
            let (full, _, _) = base_of(&next);
            assert_eq!(result.tier, ReparseTier::Token);
            assert_eq!(result.green, full.green);
            assert_eq!(result.errors, full.errors);
            assert!(reparse_edits(base, &edits, "\\different\n").is_none());
        });
    }

    #[test]
    fn spans_its_text_measures_the_green_width() {
        with_base("\\section{Hi}\n", |base| {
            assert!(spans_its_text(base.green, base.text));
            assert!(!spans_its_text(base.green, "\\section{Hi}"));
            assert!(!spans_its_text(base.green, "\\section{Hi}\n\n"));
        });
    }

    #[test]
    fn finish_refuses_a_tree_that_does_not_span_its_text() {
        with_base("\\section{Hi}\n", |base| {
            let out = finish(
                base.green.clone(),
                base.errors.to_vec(),
                ReparseTier::Token,
                base,
                "\\section{Hi}\n\n",
            );
            assert!(out.is_none());
        });
    }

    #[test]
    fn finish_accepts_an_identity_splice() {
        with_base("\\section{Hi}\n\nbody\n", |base| {
            let out = finish(
                base.green.clone(),
                base.errors.to_vec(),
                ReparseTier::Token,
                base,
                base.text,
            );
            let out = out.expect("an identity splice matches a full parse");
            assert_eq!(out.tier, ReparseTier::Token);
            assert_eq!(&out.green, base.green);
        });
    }

    #[test]
    fn tiers_order_cheapest_first() {
        assert!(ReparseTier::Token < ReparseTier::Verbatim);
        assert!(ReparseTier::Verbatim < ReparseTier::Math);
        assert!(ReparseTier::Math < ReparseTier::Region);
    }

    #[test]
    fn fingerprint_separates_trees_that_differ_only_in_token_text() {
        let a = crate::parser::parse("\\a{b}");
        let b = crate::parser::parse("\\a{c}");
        assert_ne!(fingerprint(&a.syntax()), fingerprint(&b.syntax()));
    }

    #[test]
    fn fingerprint_agrees_with_itself_across_equal_parses() {
        let a = crate::parser::parse("\\section{Hi}\n\nbody $x^2$ % c\n");
        let b = crate::parser::parse("\\section{Hi}\n\nbody $x^2$ % c\n");
        assert_eq!(fingerprint(&a.syntax()), fingerprint(&b.syntax()));
    }

    #[cfg(debug_assertions)]
    mod oracle_self_tests {
        use super::*;

        #[test]
        #[should_panic(expected = "different tree")]
        fn the_oracle_rejects_a_wrong_tree() {
            with_base("\\section{Hi}\n", |base| {
                let wrong = crate::parser::parse("\\section{Ho}\n");
                let _ = finish(
                    wrong.green,
                    base.errors.to_vec(),
                    ReparseTier::Token,
                    base,
                    base.text,
                );
            });
        }

        #[test]
        #[should_panic(expected = "different errors")]
        fn the_oracle_rejects_a_perturbed_error_vector() {
            with_base("\\section{Hi}\n", |base| {
                let mut errors = base.errors.to_vec();
                errors.push(SyntaxError {
                    message: "invented".to_string(),
                    start: 0,
                    end: 1,
                });
                let _ = finish(
                    base.green.clone(),
                    errors,
                    ReparseTier::Token,
                    base,
                    base.text,
                );
            });
        }

        #[test]
        #[should_panic(expected = "different errors")]
        fn the_oracle_rejects_an_error_that_moved() {
            let text = "\\begin{itemize}\n";
            with_base(text, |base| {
                assert!(
                    !base.errors.is_empty(),
                    "this fixture exists to carry an error"
                );
                let mut errors = base.errors.to_vec();
                errors[0].start += 1;
                let _ = finish(
                    base.green.clone(),
                    errors,
                    ReparseTier::Token,
                    base,
                    base.text,
                );
            });
        }
    }
}
