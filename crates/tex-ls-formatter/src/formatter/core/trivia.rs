use super::*;

/// True if `node` directly contains a `NEWLINE` token — **the unsafe
/// lone-newline predicate** (trivia-invariant layout, `formatter.md`).
///
/// Its two surviving readers — the `GROUP` arm's non-[`WrapMode::Reflow`] /
/// doc-margined branch in [`lower_node`] and [`lower_optional`]'s
/// non-`wraps_prose` / doc-margined early return — decide block-vs-inline for
/// a delimited group and are sanctioned **Tier 2**, on this fixed-point
/// argument: the block form ([`lower_bracketed`]) always ends with a newline
/// before its closing delimiter, so its output re-reads as multi-line and
/// takes the block form again, byte-stably (its body renderers —
/// [`ReflowKind::Statement`], the generic stream — carry their own fixed-point
/// contracts); an empty multi-line body collapses to the bare delimiters,
/// which re-read single-line and *stay* on the inline path; and the inline
/// path emits no newline inside the group, so a single-line group re-reads
/// single-line. Every layout either reader can emit re-reads to itself.
///
/// Both readers are reachable only under the Tier-2 wrap modes
/// (`Preserve`/`Stable`/`Sentence`/`Semantic`, modes defined by authored
/// breaks) or behind `doc_margin_opens_line` (a preserved column-0 predicate).
/// Under the default `Reflow`, [`lower_opaque_group`] and [`lower_optional`]
/// decide from width, content, and preserved predicates only and never
/// consult this. **Don't add a reader in a Tier-1 position.**
pub(super) fn spans_multiple_lines(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == SyntaxKind::NEWLINE)
}

/// True if `node` contains a `.dtx` documentation margin or docstrip guard at
/// any depth — a construct continuing across margined or guarded lines. Both
/// tokens are line-oriented column-0 facts (a `%` or `%<…>` recognized at line
/// start only), so any relayout that merges or re-indents their lines silently
/// turns them into ordinary comments on the next parse. Always false outside
/// the `.dtx` lexer mode (only it emits these kinds), which is why `cx.is_dtx`
/// short-circuits it — the same gate [`doc_margin_opens_line`] carries, and not
/// merely an optimization at this size. This is a *match guard* on most of
/// [`lower_node`]'s relayout arms, so it runs for every group, environment, and
/// math node; the walk is `O(subtree)`, and a nested construct re-walks at each
/// level. Ungated it was ~52% of the run on `{{{…}}}` nested 4000 deep, and made
/// lowering quadratic in nesting depth for every file, `.dtx` or not.
pub(super) fn contains_doc_margin(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    if !cx.is_dtx || cx.in_dtx_doc_region {
        return false;
    }
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| matches!(t.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD))
}

/// Whether a forced-break block's interior lines all ride their own column-0
/// margins: the subtree spans at least one newline, every `NEWLINE` token is
/// immediately followed (still inside the node) by a `DOC_MARGIN` or `GUARD`,
/// and no other token embeds a newline (a multi-line `VERB`, a `\`-newline
/// control symbol). Every relayout arm of [`lower_node`] refuses a doc-margined
/// subtree, so such a block lowers through the byte-faithful stream and
/// reproduces its margins exactly when committed raw — only its first line
/// needs the canonical margin re-attached
/// ([`LineBuilder::push_margined_block`]). A node-final `NEWLINE` (nothing
/// follows it inside the node) fails conservatively: the check cannot see what
/// the next line carries.
pub(super) fn block_rides_own_margins(node: &SyntaxNode) -> bool {
    let tokens: Vec<SyntaxToken> = node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .collect();
    let mut saw_newline = false;
    for (i, token) in tokens.iter().enumerate() {
        if !token.text().contains('\n') {
            continue;
        }
        if token.kind() != SyntaxKind::NEWLINE {
            return false;
        }
        saw_newline = true;
        if !tokens
            .get(i + 1)
            .is_some_and(|next| matches!(next.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD))
        {
            return false;
        }
    }
    saw_newline
}

/// True if `node` directly contains a `VERBATIM_BODY` token — i.e. it is a
/// verbatim-like environment whose body must be emitted byte-for-byte.
pub(super) fn has_verbatim_body(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == SyntaxKind::VERBATIM_BODY)
}

/// Consume the maximal run of collapsible trivia beginning at `first` and
/// normalize it to a [`Gap`] — the boundary every width-driven lowering takes,
/// and the reason none of them *can* key on a lone newline.
pub(super) fn consume_gap(
    first: &SyntaxToken,
    iter: &mut Peekable<impl Iterator<Item = SyntaxElement>>,
) -> Gap {
    consume_gap_widened(first, iter).narrow()
}

/// The Tier-2 form of [`consume_gap`]: the same run, with the newline count left
/// readable. See [`WideGap`] for who may call this and what they owe.
///
/// The run's newline count is taken over the whole run; the whitespace following
/// the *last* newline is the run's leading indentation, which the printer owns, and
/// whitespace *before* a newline is trailing whitespace — both are dropped by
/// [`Gap::from_run`]. For a run with no newline the whole run is the gap's flat
/// spelling.
pub(super) fn consume_gap_widened(
    first: &SyntaxToken,
    iter: &mut Peekable<impl Iterator<Item = SyntaxElement>>,
) -> WideGap {
    let mut newlines = 0;
    let mut trailing_ws = String::new();
    absorb(first, &mut newlines, &mut trailing_ws);
    loop {
        match iter.peek() {
            Some(SyntaxElement::Token(tok)) if is_collapsible_trivia(tok.kind()) => {}
            _ => break,
        }
        let token = match iter.next() {
            Some(SyntaxElement::Token(tok)) => tok,
            _ => unreachable!("peeked a collapsible trivia token"),
        };
        absorb(&token, &mut newlines, &mut trailing_ws);
    }
    WideGap {
        gap: Gap::from_run(newlines, trailing_ws),
        newlines,
    }
}

/// Consume the maximal run of collapsible trivia in `elements` beginning at
/// `*i`, advancing `*i` past it and returning the number of newlines it spans.
/// The index-based analogue of [`consume_gap_widened`], used by the two reflow
/// drivers, which need to look ahead past the run (the peekable iterator form
/// cannot).
///
/// **Tier 2**, like everything else that reads a newline count: both callers are
/// line-structure-preserving sites carrying their own fixed-point arguments (see
/// [`WideGap`]). Neither needs the flat spelling — a reflow re-derives spacing
/// from the fill — so this returns the count bare rather than a [`WideGap`].
pub(super) fn consume_widened_gap_slice(elements: &[SyntaxElement], i: &mut usize) -> usize {
    let mut newlines = 0;
    while let Some(SyntaxElement::Token(tok)) = elements.get(*i) {
        if !is_collapsible_trivia(tok.kind()) {
            break;
        }
        if tok.kind() == SyntaxKind::NEWLINE {
            newlines += 1;
        }
        *i += 1;
    }
    newlines
}

/// Whether the physical source line beginning at `start` in `elements` consists
/// solely of non-inline command(s) and inline whitespace — the unit
/// [`reflow_elements`]'s *residual* rule keeps on its own line rather than
/// reflowing into its neighbours. The line runs until the next newline, comment,
/// or end of the stream; any non-trivia element that is not such a command (a
/// word, a control symbol, a group, math, a `\\`, a block, or an *inline* command
/// like `\citep`/`\ref` — see [`command_is_inline`]) disqualifies it. A line with
/// no command (e.g. an empty or comment-only line) is not a command line.
///
/// Residual: a curated block command ([`command_is_block`]) is intercepted by the
/// block-statement arm and gets its own line regardless of this test, so the
/// authored-break preservation decided here matters only for un-signatured and
/// scanned-definition commands (block-ness undecidable without semantics) and for
/// block commands glued to adjacent content.
///
/// That read of the lone-newline predicate is sanctioned **Tier 2**, on this
/// fixed-point argument: the rule is preservation-only — its entire effect is to
/// harden a gap that already holds a newline, never to write a break where none
/// was or to move content across one. So a newline in the output is either (a) a
/// break this rule kept, which re-reads as the same command-only line at the same
/// gap (nothing moves non-trivia across a hard line end, so command-only-ness is
/// itself preserved) and is kept again in place; or (b) a break the fill emitted,
/// which the next pass may *harden* when the printed line it opened or closed
/// happens to be command-only — a width wrap stranding an un-signatured command
/// alone. Hardening a break the greedy fill already chose is layout-neutral:
/// filling is first-fit, so the lines before the hardened break refill unchanged
/// and the fill after it restarts from column 0 exactly as the soft break did
/// (`reflow_command_stranded_by_width` pins this corner; the Tier-2 render modes'
/// own contracts cover their breaks the same way). Strict trivia invariance is
/// deliberately not claimed — preserving the authored break *is* the rule — so
/// `--checks trivia-strict` still reports these shapes
/// (`trivia_strict_check_fires_where_an_authored_break_is_preserved`), and the
/// [`Gap`] normalization carries this read as a [`WideGap`].
/// Retiring the rule outright would glue every authored `\mymacro`-on-its-own-line
/// into the fill — a policy change, not a fix.
///
/// A `CONDITIONAL` reaches this as a single non-`COMMAND` element and so
/// disqualifies its line, which is correct: [`lower_conditional`] owns where its
/// dividers fall, and the divider commands never appear in a reflow stream of
/// their own.
pub(super) fn line_is_command_only(
    elements: &[SyntaxElement],
    start: usize,
    cx: LowerCtx<'_>,
) -> bool {
    let mut saw_command = false;
    for element in &elements[start..] {
        match element {
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::NEWLINE => break,
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => break,
            SyntaxElement::Token(t) if cx.is_dtx && t.kind() == SyntaxKind::DOC_MARGIN => {
                continue;
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::WHITESPACE => continue,
            SyntaxElement::Node(n)
                if n.kind() == SyntaxKind::COMMAND && !command_is_inline(n, cx) =>
            {
                saw_command = true
            }
            _ => return false,
        }
    }
    saw_command
}

/// Whether the element after `idx` leaves a break opportunity: the stream ends, or
/// the next element is collapsible trivia (whitespace, a newline) or a `COMMENT`
/// (a glued `%` is the line-continuation idiom and rides the committed line via
/// the `after_block` path). Anything else is glued to `elements[idx]`, and the
/// engine's rule is that adjacent non-whitespace elements form one unbreakable
/// atom — splitting there would materialize a space token TeX typesets. Keyed on
/// adjacency alone, a predicate the formatter preserves (it never converts
/// glued↔spaced), so the block-statement gate may read it.
pub(super) fn next_is_separated(elements: &[SyntaxElement], idx: usize) -> bool {
    match elements.get(idx + 1) {
        None => true,
        Some(SyntaxElement::Token(t)) => matches!(
            t.kind(),
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
        ),
        Some(SyntaxElement::Node(_)) => false,
    }
}

/// Whether the next non-framing element in the paragraph is a `\label` command.
/// A `.dtx` margin is physical line framing, so a heading and label separated by
/// one remains structurally adjacent.
pub(super) fn next_nontrivia_is_label(
    elements: &[SyntaxElement],
    idx: usize,
    cx: LowerCtx<'_>,
) -> bool {
    elements[idx + 1..]
        .iter()
        .find(|element| !is_section_label_framing(element, cx))
        .is_some_and(|element| {
            matches!(element, SyntaxElement::Node(node) if node.kind() == SyntaxKind::COMMAND && command_is_label(node))
        })
}

pub(super) fn is_section_label_framing(element: &SyntaxElement, cx: LowerCtx<'_>) -> bool {
    is_collapsible_trivia_element(element)
        || matches!(element, SyntaxElement::Token(token) if cx.is_dtx && token.kind() == SyntaxKind::DOC_MARGIN)
}

/// Whether `elements[idx]` is in a label run immediately after a sectioning
/// command. The paragraph boundary supplies the outer structural gate; scanning
/// only within this flattened sibling stream cannot attach a label across
/// intervening prose, comments, or another construct. Glued seams are admitted
/// because the sectioning rule itself puts the heading and first label on separate
/// lines; accepting that source shape on pass one is required for idempotence.
pub(super) fn label_follows_sectioning_run(
    elements: &[SyntaxElement],
    idx: usize,
    cx: LowerCtx<'_>,
) -> bool {
    for element in elements[..idx].iter().rev() {
        match element {
            element if is_section_label_framing(element, cx) => {}
            SyntaxElement::Node(node)
                if node.kind() == SyntaxKind::COMMAND && command_is_label(node) => {}
            SyntaxElement::Node(node)
                if node.kind() == SyntaxKind::COMMAND && command_is_sectioning(node, cx) =>
            {
                return node
                    .parent()
                    .is_some_and(|parent| parent.kind() == SyntaxKind::PARAGRAPH);
            }
            _ => return false,
        }
    }
    false
}

pub(super) fn absorb(tok: &SyntaxToken, newlines: &mut usize, trailing_ws: &mut String) {
    if tok.kind() == SyntaxKind::NEWLINE {
        *newlines += 1;
        trailing_ws.clear();
    } else {
        trailing_ws.push_str(tok.text());
    }
}

/// Map a trivia run to a single IR primitive: no newline → the inline whitespace
/// (a genuine inter-word space) kept verbatim; one newline → a [`Ir::hard_line`];
/// two or more → a single [`Ir::empty_line`] (one blank line). A non-math grid
/// cell softens the one-newline case to [`Ir::line`] so continuation lines join
/// even when parser attachment nests the trivia inside a command. Whitespace
/// that followed the last newline is *indentation*, which the printer owns and
/// recreates, so it is dropped by [`Gap::from_run`] — keeping it would
/// double-indent on reformat.
///
/// This is normally the byte-faithful stream's boundary, and the reason it takes a
/// [`WideGap`]: reproducing the author's line structure is its entire contract, so
/// it reads the newline count by definition. **Tier 2**, with the trivial
/// fixed-point argument — a `hard_line` re-reads as one newline, an `empty_line`
/// re-reads as two, and verbatim whitespace re-reads as itself. The alignment-cell
/// exception inherits the grid's existing continuation rule: it emits the cell on
/// one line, which reparses without the softened newline and remains on that same
/// flat path; a blank line is never softened.
pub(super) fn classify_trivia(gap: WideGap, soften_newline: bool) -> Ir {
    match gap.newlines {
        0 => Ir::verbatim(gap.gap.flat()),
        1 if soften_newline => Ir::line(),
        1 => Ir::hard_line(),
        _ => Ir::empty_line(),
    }
}

/// A break the indenter supplies itself and so trims from a body edge: a forced
/// line break, an inline whitespace chunk (indentation), or [`Ir::Nil`]. A
/// `VERBATIM_BODY` (force-break verbatim, or non-blank text) is never trimmable,
/// so protected content survives.
pub(super) fn is_trimmable_break(ir: &Ir) -> bool {
    match ir {
        Ir::HardLine | Ir::EmptyLine | Ir::Nil => true,
        Ir::Verbatim { text, force_break } => {
            !force_break && text.chars().all(|c| c == ' ' || c == '\t')
        }
        _ => false,
    }
}

/// Drop leading break/indentation IR from `ir`, reporting whether the trimmed-away
/// break carried a blank line (an [`Ir::empty_line`]). Recurses into a leading
/// `Concat` (the body's first break is often buried inside the first paragraph).
/// [`lower_environment`] uses the blank flag to re-supply one blank line at the
/// body's leading edge; callers that only want the trim use [`trim_leading_break`].
pub(super) fn peel_leading_break(ir: Ir) -> (bool, Ir) {
    if is_trimmable_break(&ir) {
        return (matches!(ir, Ir::EmptyLine), Ir::Nil);
    }
    match ir {
        Ir::Concat(items) => {
            let mut v: Vec<Ir> = items.iter().cloned().collect();
            let mut blank = false;
            while !v.is_empty() {
                let (b, head) = peel_leading_break(v.remove(0));
                blank |= b;
                if matches!(head, Ir::Nil) {
                    continue;
                }
                v.insert(0, head);
                break;
            }
            (blank, Ir::concat(v))
        }
        other => (false, other),
    }
}

/// Mirror of [`peel_leading_break`] for the trailing edge.
pub(super) fn peel_trailing_break(ir: Ir) -> (bool, Ir) {
    if is_trimmable_break(&ir) {
        return (matches!(ir, Ir::EmptyLine), Ir::Nil);
    }
    match ir {
        Ir::Concat(items) => {
            let mut v: Vec<Ir> = items.iter().cloned().collect();
            let mut blank = false;
            while let Some(last) = v.pop() {
                let (b, tail) = peel_trailing_break(last);
                blank |= b;
                if matches!(tail, Ir::Nil) {
                    continue;
                }
                v.push(tail);
                break;
            }
            (blank, Ir::concat(v))
        }
        other => (false, other),
    }
}

/// Drop leading break/indentation IR from `ir`, discarding the blank flag (see
/// [`peel_leading_break`]).
pub(super) fn trim_leading_break(ir: Ir) -> Ir {
    peel_leading_break(ir).1
}

/// Drop trailing break/indentation IR from `ir`, discarding the blank flag (see
/// [`peel_trailing_break`]).
pub(super) fn trim_trailing_break(ir: Ir) -> Ir {
    peel_trailing_break(ir).1
}
