use super::*;

/// Lower an `\begin{…} … \end{…}` environment, indenting its body one step. A
/// clean-parse environment is `[BEGIN, body…, END]`: the framing nodes are
/// lowered directly, and the body between them is wrapped in [`Ir::indent`] with
/// a leading [`Ir::hard_line`] (so it starts on its own indented line) and a
/// trailing `hard_line` at the *outer* indent (so `\end` sits flush with
/// `\begin`). All indentation is owned by the printer, so the body's own leading
/// and trailing breaks are trimmed before wrapping — this is what makes
/// re-indentation idempotent. A blank line the author placed against `\begin`/
/// `\end` is preserved as a single blank line (the leading/trailing `hard_line`
/// becomes an [`Ir::empty_line`]); the empty-body case keeps a single break.
///
/// Verbatim-like environments never reach here (their opaque `VERBATIM_BODY`
/// token would be corrupted by reflow); [`lower_node`] routes them to the
/// generic path, which emits the body verbatim.
/// The leading comment-bind run (an own-line `%` run the parser attached as
/// leading children *before* the `BEGIN` node). It is not body: it lowers to its
/// own line(s) above `\begin`, at the environment's own indentation. Returns
/// [`Ir::Nil`] when there is no such run. Shared by every environment lowerer so
/// the bound comment is rendered the same way regardless of body shape.
pub(super) fn lower_environment_leading(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let mut leading: Vec<SyntaxElement> = Vec::new();
    for element in node.children_with_tokens() {
        if matches!(&element, SyntaxElement::Node(c) if c.kind() == SyntaxKind::BEGIN) {
            break;
        }
        leading.push(element);
    }
    if leading.is_empty() {
        Ir::Nil
    } else {
        Ir::concat(lower_element_stream(leading.into_iter(), cx))
    }
}

/// The body-final `\\<newline>` control symbol, ignoring only indentation on
/// the closer's line. The newline is part of this non-trivia token, not a
/// separate [`SyntaxKind::NEWLINE`], so an environment that unconditionally
/// adds its own closing break would otherwise create a blank paragraph.
pub(super) fn trailing_control_newline(body: &[SyntaxElement]) -> Option<SyntaxToken> {
    let first = body.first()?.text_range().start();
    let last = body.last()?;
    let mut token = match last {
        SyntaxElement::Node(node) => node.last_token(),
        SyntaxElement::Token(token) => Some(token.clone()),
    }?;

    loop {
        if token.text_range().start() < first {
            return None;
        }
        match token.kind() {
            SyntaxKind::WHITESPACE => token = token.prev_token()?,
            SyntaxKind::CONTROL_SYMBOL if token.text().ends_with('\n') => return Some(token),
            _ => return None,
        }
    }
}

pub(super) fn split_environment(node: &SyntaxNode, cx: LowerCtx<'_>) -> EnvParts {
    let leading = lower_environment_leading(node, cx);
    let mut begin = Ir::Nil;
    let mut end = Ir::Nil;
    let mut body: Vec<SyntaxElement> = Vec::new();
    let mut tail_len = 0usize;
    let mut seen_begin = false;
    for element in node.children_with_tokens() {
        match &element {
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::BEGIN => {
                seen_begin = true;
                // Content the greedy parser attached past the end of the header
                // leads the body (see [`lower_begin`]). `body` is still empty here
                // — everything before `BEGIN` is `leading` — so extending it now
                // keeps the tail in source order ahead of the real body.
                let parts = lower_begin(child, cx);
                begin = parts.header;
                tail_len = parts.tail.len();
                body.extend(parts.tail);
            }
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::END => {
                end = lower_node(child, cx);
            }
            _ if !seen_begin => {}
            _ => body.push(element),
        }
    }
    let lifted = leading_inline_comment(&body);
    if let Some(comment) = &lifted {
        begin = Ir::concat([begin, Ir::verbatim(comment.text())]);
    }
    let body_header_token = picture_header_token(node, &body);
    if let Some(token) = &body_header_token {
        begin = Ir::concat([begin, lower_loose_token(token, cx)]);
    }
    EnvParts {
        leading,
        begin,
        body,
        end,
        lifted,
        tail_len,
        body_header_token,
    }
}

/// Return the standard `picture` environment's glued `(width,height)` token.
/// TeX gives this environment an unbraced begin argument, so the generic grammar
/// necessarily places it in the body. The name, adjacency, and tuple shape make
/// the relocation text-falsifiable without claiming that arbitrary body text is
/// an environment argument.
pub(super) fn picture_header_token(
    node: &SyntaxNode,
    body: &[SyntaxElement],
) -> Option<SyntaxToken> {
    let environment = Environment::cast(node.clone())?;
    if environment.name().as_deref() != Some("picture") {
        return None;
    }
    let begin = environment.begin()?;
    let paragraph = body.first()?.as_node()?;
    if paragraph.kind() != SyntaxKind::PARAGRAPH {
        return None;
    }
    let token = paragraph.first_token()?;
    (token.kind() == SyntaxKind::WORD
        && begin.syntax().text_range().end() == token.text_range().start()
        && is_picture_tuple(token.text()))
    .then_some(token)
}

pub(super) fn is_picture_tuple(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    let mut groups = 0usize;
    while chars.peek().is_some() {
        if chars.next() != Some('(') {
            return false;
        }
        let mut comma = false;
        let mut content = false;
        loop {
            match chars.next() {
                Some(',') if !comma && content => {
                    comma = true;
                    content = false;
                }
                Some(')') if comma && content => break,
                Some('(' | ')') | None => return false,
                Some(_) => content = true,
            }
        }
        groups += 1;
    }
    matches!(groups, 1 | 2)
}

/// Whether `el` is the [`EnvParts::lifted`] `\begin`-line comment, compared by
/// token identity so the flattening body consumers (list, alignment grid, math
/// formula) drop exactly the token that was lifted and nothing else.
pub(super) fn is_lifted_comment(el: &SyntaxElement, lifted: Option<&SyntaxToken>) -> bool {
    lifted.is_some_and(|l| el.as_token() == Some(l))
}

/// Whether `node` (a `PARAGRAPH`) takes the plain prose reflow in [`lower_node`] —
/// the one body path a spliced [`EnvParts::tail_len`] run can join. A `.dtx` doc
/// paragraph re-synthesizes its own `% ` margin per line and an expl3-overlapping
/// one segments into statements; neither admits foreign leading elements, so both
/// keep the concatenated form. Mirrors `lower_node`'s `PARAGRAPH` arms — keep the
/// two in step.
pub(super) fn paragraph_reflows_as_prose(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    cx.wraps_prose()
        && !is_dtx_doc_paragraph(node)
        && !(cx.any_expl3() && cx.overlaps_expl3(node.text_range()))
}

/// Lower [`EnvParts::body`] through the generic element stream, dropping the
/// lifted `\begin`-line comment when one was taken.
///
/// The leading `tail_len` elements are content greedy attachment gave to `BEGIN`
/// past the end of its header (see [`lower_begin`]); where the body opens with a
/// prose paragraph they are *spliced into its reflow* rather than concatenated
/// ahead of it. That is what makes the relocation invisible to the layout: the
/// source lays out identically whether or not the parser happened to pull the
/// group into `BEGIN` — `\begin{center}\n{\bfseries A}\nmore` reflows onto one
/// line, exactly as it does with a word ahead of the group to keep it in the
/// paragraph.
///
/// Concatenating instead would abut the two with *no separator at all*: the
/// paragraph's own leading newline lives inside the node and its reflow trims it,
/// so `{\bfseries A}` and `more` would run together. That is a space TeX typesets,
/// silently deleted — and invisible to every CST oracle, since whitespace is trivia
/// to them and content to TeX.
pub(super) fn lower_env_body(
    body: Vec<SyntaxElement>,
    tail_len: usize,
    lifted: bool,
    body_header_token: Option<&SyntaxToken>,
    cx: LowerCtx<'_>,
) -> Ir {
    if let Some(header_token) = body_header_token
        && let Some(SyntaxElement::Node(paragraph)) = body.first()
    {
        let elements = paragraph
            .children_with_tokens()
            .filter(|element| element.as_token() != Some(header_token));
        let first = if cx.wraps_prose() {
            reflow_elements(elements, cx, paragraph_reflow_kind(paragraph, cx))
        } else {
            Ir::concat(lower_element_stream(elements, cx))
        };
        return Ir::concat(
            std::iter::once(first).chain(lower_element_stream(body[1..].iter().cloned(), cx)),
        );
    }
    if tail_len > 0
        && let Some(SyntaxElement::Node(para)) = body.get(tail_len)
        && para.kind() == SyntaxKind::PARAGRAPH
        && paragraph_reflows_as_prose(para, cx)
    {
        let spliced = reflow_elements(
            body[..tail_len]
                .iter()
                .cloned()
                .chain(para.children_with_tokens()),
            cx,
            paragraph_reflow_kind(para, cx),
        );
        let rest = lower_element_stream(body[tail_len + 1..].iter().cloned(), cx);
        return Ir::concat(std::iter::once(spliced).chain(rest));
    }
    // A named math environment can likewise begin with content greedy attachment
    // left inside `BEGIN`, followed by the parser's ordinary `MATH` body wrapper.
    // The math-grid path flattens those siblings together, but a grid containing a
    // non-final multiline cell must fall back here. Keep the recovered prefix in
    // math mode on that fallback: lowering a comment-bearing group generically
    // would reset binary-operator context after the comment and indent its body a
    // full step rather than hanging it one column past `{`.
    if tail_len > 0
        && body
            .get(tail_len)
            .and_then(SyntaxElement::as_node)
            .is_some_and(|node| node.kind() == SyntaxKind::MATH)
    {
        let prefix = lower_math_seq(
            body[..tail_len].iter().cloned(),
            cx,
            MathSpacing::Normal,
            false,
        );
        let rest = lower_element_stream(body[tail_len..].iter().cloned(), cx);
        return Ir::concat(std::iter::once(prefix).chain(rest));
    }
    if lifted {
        lower_body_dropping_leading_comment(body, cx)
    } else {
        Ir::concat(lower_element_stream(body.into_iter(), cx))
    }
}

pub(super) fn lower_environment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let EnvParts {
        leading,
        begin,
        body,
        end,
        lifted,
        tail_len,
        body_header_token,
    } = split_environment(node, cx);
    let body_cx = cx.absorbing_trailing_control_newline(&body);
    let body = lower_env_body(
        body,
        tail_len,
        lifted.is_some(),
        body_header_token.as_ref(),
        body_cx,
    );
    // Trim the body's own edge breaks (the indenter re-supplies them), but if the
    // author left a blank line touching `\begin`/`\end`, preserve it as a single
    // blank line — LaTeX blank lines are deliberate visual spacing, so we keep one
    // rather than collapse to zero (interior runs already collapse to one).
    let (lead_blank, body) = peel_leading_break(body);
    let (trail_blank, body) = peel_trailing_break(body);
    let lead = if lead_blank {
        Ir::empty_line()
    } else {
        Ir::hard_line()
    };
    let trail = if trail_blank {
        Ir::empty_line()
    } else {
        Ir::hard_line()
    };

    let env = if matches!(body, Ir::Nil) {
        // Empty body: keep `\begin` and `\end` on their own lines (no edge blank).
        Ir::concat([begin, Ir::hard_line(), end])
    } else if environment_no_indent(node, cx) {
        // `document` and friends: lay the body on its own lines, but flush against
        // the surrounding indentation rather than nesting it.
        Ir::concat([begin, lead, body, trail, end])
    } else {
        Ir::concat([begin, Ir::indent(Ir::concat([lead, body])), trail, end])
    };
    Ir::concat([leading, env])
}

/// Split a `CONDITIONAL_BRANCH`'s elements from the trailing collapsible-trivia
/// run that carries the gap to whatever follows it, classifying that gap as a
/// [`Gap`].
///
/// All inter-segment trivia belongs to the *preceding* branch — the grammar's
/// branch loop consumes it before it reaches the divider — so this is the only
/// place a boundary gap can live.
///
/// Only [`Gap::Glued`] is discriminated by the layout: it is the one case that is
/// not a break opportunity. [`Gap::Comment`] is distinguished purely so that a
/// comment-terminated boundary never lands in `Glued` — the `%` itself is not
/// collapsible trivia, so nothing is peeled behind it, and without its own variant
/// a branch ending `… % note` would read as glued and send the whole construct
/// down the byte-faithful path. It lays out exactly like [`Gap::Space`]; the flat
/// candidate is separately refused by [`collapse_conditional_elements`], which sees
/// the same `%`. Whether the peeled run held a newline is, as everywhere,
/// invisible here.
pub(super) fn split_branch_gap(node: &SyntaxNode) -> (Vec<SyntaxElement>, Gap) {
    let mut elements: Vec<SyntaxElement> = node.children_with_tokens().collect();
    let mut peeled = false;
    while let Some(SyntaxElement::Token(t)) = elements.last() {
        if !is_collapsible_trivia(t.kind()) {
            break;
        }
        peeled = true;
        elements.pop();
    }
    let gap = match elements.last() {
        Some(SyntaxElement::Token(t)) if t.kind() == SyntaxKind::COMMENT => Gap::Comment,
        _ if peeled => Gap::space(),
        _ => Gap::Glued,
    };
    (elements, gap)
}

/// Whether a `CONDITIONAL`'s branch interiors should be reflowed as prose, or
/// `None` when the construct must not be relaid at all.
///
/// Read off the *enclosing* context, since a branch carries no `PARAGRAPH` of its
/// own to answer with (the gate keeps a conditional inside one paragraph, so one
/// never nests in a branch). A conditional whose nearest non-conditional ancestor
/// is a `PARAGRAPH` sits in running text and reflows like it; one inside a `GROUP`
/// or `ARGUMENT` is macro code — a `\def` body — where the enclosing group emits
/// the byte-faithful stream, so the branches do too. Nested conditionals inherit
/// the answer by walking past their parent branch.
///
/// `None` for a `.dtx` documentation paragraph: the broken candidate commits hard
/// lines, and a line committed inside a doc paragraph lands outside its `% `
/// margin. The `contains_doc_margin` guard on the dispatch arm only catches a
/// conditional carrying a margin *itself*, not one riding a margined line whose
/// `DOC_MARGIN` sits before the opener.
pub(super) fn conditional_interior_reflows(node: &SyntaxNode) -> Option<bool> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        match parent.kind() {
            SyntaxKind::CONDITIONAL | SyntaxKind::CONDITIONAL_BRANCH => ancestor = parent.parent(),
            SyntaxKind::PARAGRAPH => return (!is_dtx_doc_paragraph(&parent)).then_some(true),
            _ => return Some(false),
        }
    }
    Some(false)
}

pub(super) fn split_conditional(node: &SyntaxNode) -> Option<ConditionalParts> {
    let mut leading: Vec<SyntaxElement> = Vec::new();
    let mut branches: Vec<SyntaxNode> = Vec::new();
    let mut closer: Option<SyntaxNode> = None;
    for element in node.children_with_tokens() {
        match &element {
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::CONDITIONAL_BRANCH => {
                // The closer is last, positionally (`Conditional::closer`); a branch
                // after one means the walk stopped somewhere this layout cannot model.
                if closer.is_some() {
                    return None;
                }
                branches.push(child.clone());
            }
            SyntaxElement::Node(child)
                if child.kind() == SyntaxKind::COMMAND && !branches.is_empty() =>
            {
                if closer.replace(child.clone()).is_some() {
                    return None;
                }
            }
            _ if branches.is_empty() => leading.push(element),
            // Anything else between the branches would be dropped by the branch
            // walk, so decline the whole construct rather than lose it.
            _ => return None,
        }
    }
    Some(ConditionalParts {
        leading,
        branches,
        closer: closer?,
    })
}

/// Lower an `\if… … \else … \or … \fi` conditional **all-or-nothing**: flat when
/// the whole construct fits, else *every* divider opens a line.
///
/// The construct's extent is what makes this decidable, and it is why the node
/// exists. A per-divider rule at this layer has no coherent form: fired only
/// across a gap the author already wrote it *is* the lone-newline read; fired
/// unconditionally it manufactures a space token at the ~22% of glued sites, which
/// TeX contributes to the horizontal list; fired only where the author broke, it is
/// lopsided — one divider broken and its sibling not, decided by where the author
/// happened to glue.
///
/// Two things the node deliberately does **not** buy. There is no body indent and
/// no head/body split: the `\if` *test*'s extent is not statically resolvable
/// (`\ifnum\radius>5` scans ⟨number⟩⟨rel⟩⟨number⟩ by TeX's own scanner), so the
/// environment-shaped layout the corpus files are written in is out of reach even
/// with the node. And a construct with **any glued divider** takes the byte-faithful
/// path instead of the group: breaking one divider but not its glued sibling is the
/// lopsided form, and breaking the glued one is the typeset change, so the only
/// coherent option left is to relayout none of them. Those keep their authored
/// line structure, which is a fixed point (a hard line re-reads as a newline, glue
/// re-reads as glue) and never materializes a space.
///
/// The decision is offered to the printer as two whole candidates
/// ([`Ir::conditional_group_all_lines`]) rather than as one [`Ir::group`] of
/// `Ir::Line`s, and that is load-bearing. A group's break state is saturated from
/// whatever forced breaks its subtree carries, and a branch *interior* carries one
/// for every authored line the command-only-line rule
/// ([`line_is_command_only`]) keeps — so a group would decide the dividers from
/// the interior's authored newlines, which is precisely the predicate that must
/// not decide them. The flat candidate is collapsed from *content* alone
/// ([`collapse_conditional_elements`]), so its width — and therefore the choice
/// between the two — is a function of non-trivia content and the config only.
/// When no flat candidate exists (a `%` comment, a nested block), the broken form
/// is unconditional, which is a content fact and fair to read.
///
/// A branch *interior* is lowered the way the construct's **enclosing context**
/// would lower the same elements ([`conditional_interior_reflows`]). That is what
/// "as anywhere else" has to mean, and it cannot be read off the branch itself: the
/// gate keeps a `CONDITIONAL` inside one paragraph, so no `PARAGRAPH` node ever
/// nests in a branch to carry the prose lowering the way an environment body's
/// does. In prose the branch therefore reflows — its words wrap and its inter-word
/// spacing normalizes, exactly as they would outside the construct — while in a
/// `\def` body it takes the byte-faithful stream, because that is what the
/// enclosing `GROUP` does. Feeding macro code to the prose reflow is not merely
/// cosmetic: `pagesel.sty`'s `\ifx\\#2\\%` has the parser's `LINE_BREAK` node in an
/// `\ifx` operand slot, and the reflow's "a `\\` ends its line" rule oscillates on
/// it pass over pass.
///
/// The whole relayout is confined to the modes that lay prose out at all
/// ([`LowerCtx::wraps_prose`]). [`WrapMode::Preserve`] promises authored line
/// breaks are untouched, and the all-or-nothing choice would rejoin a conditional
/// the author spread over lines — so that mode takes the byte-faithful stream.
/// The other three rebuild every prose line from runs already, so the choice is
/// theirs to make.
pub(super) fn lower_conditional(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let generic = || Ir::concat(lower_element_stream(node.children_with_tokens(), cx));
    if !cx.wraps_prose() {
        return generic();
    }
    let Some(reflow_interior) = conditional_interior_reflows(node) else {
        return generic();
    };
    let Some(ConditionalParts {
        leading,
        branches,
        closer,
    }) = split_conditional(node)
    else {
        return generic();
    };

    let split: Vec<(Vec<SyntaxElement>, Gap)> = branches.iter().map(split_branch_gap).collect();
    if split.iter().any(|(_, gap)| matches!(gap, Gap::Glued)) {
        return generic();
    }

    // The bound `DOC_COMMENT` run, if any. Lowered outside the candidates (as
    // `lower_environment` does with its own leading run): it is not part of the
    // construct's width, and it must survive whichever candidate the printer picks.
    let leading = if leading.is_empty() {
        Ir::Nil
    } else {
        Ir::concat(lower_element_stream(leading.into_iter(), cx))
    };

    let closer_ir = lower_node(&closer, cx);
    let mut broken = Vec::with_capacity(split.len() * 2 + 1);
    for (elements, _) in &split {
        broken.push(if reflow_interior {
            reflow_elements(elements.iter().cloned(), cx, ReflowKind::Prose)
        } else {
            Ir::concat(lower_element_stream(elements.iter().cloned(), cx))
        });
        broken.push(Ir::hard_line());
    }
    broken.push(closer_ir.clone());
    let broken = Ir::concat(broken);

    let group = match collapse_conditional(&split, &closer_ir, cx) {
        Some(flat) => Ir::conditional_group_all_lines([flat, broken]),
        None => broken,
    };
    Ir::concat([leading, group])
}

/// The one-line candidate for [`lower_conditional`]: every branch collapsed to a
/// single line, dividers separated by one space.
///
/// `None` — no flat form exists, so the construct is unconditionally broken — when
/// any branch holds a `%` comment (which must end its line) or force-break content
/// (a nested environment, display math, `\\`). Both are *content* facts, so keying
/// the layout on them is sound; an authored newline is not, and collapses to a
/// space here exactly as it does in [`collapse_arg_group`].
pub(super) fn collapse_conditional(
    split: &[(Vec<SyntaxElement>, Gap)],
    closer: &Ir,
    cx: LowerCtx<'_>,
) -> Option<Ir> {
    if closer.contains_forced_break() {
        return None;
    }
    let mut parts = Vec::new();
    for (elements, _) in split {
        parts.extend(collapse_conditional_elements(elements, cx)?);
        parts.push(Ir::verbatim(" "));
    }
    parts.push(closer.clone());
    Some(Ir::concat(parts))
}

/// Collapse one branch's elements to a single line, or `None` if it cannot be
/// collapsed. Mirrors [`collapse_arg_group`]'s body loop, without the delimiter
/// handling a branch has no equivalent of.
pub(super) fn collapse_conditional_elements(
    elements: &[SyntaxElement],
    cx: LowerCtx<'_>,
) -> Option<Vec<Ir>> {
    let mut out = Vec::new();
    let mut iter = elements.iter().cloned().peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                let gap = consume_gap(&t, &mut iter);
                if gap == Gap::Blank {
                    return None; // a blank-line `\par` (the gate should preclude it)
                }
                out.push(Ir::verbatim(gap.flat()));
            }
            // A `%` comment must terminate its line, so there is no flat form.
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => return None,
            SyntaxElement::Token(t) => out.push(Ir::verbatim(t.text())),
            SyntaxElement::Node(child) => {
                let ir = lower_node(&child, cx);
                if ir.contains_forced_break() {
                    return None; // nested block content: keep the broken form
                }
                out.push(ir);
            }
        }
    }
    Some(out)
}

/// Whether the environment is *margin-framed*: a `.dtx` documentation margin
/// (`DOC_MARGIN`) or docstrip guard (`GUARD`) sits immediately before its `\begin`
/// on the same physical line — `%␣␣␣␣\begin{macrocode}`, a documentation-layer
/// `% \begin{itemize}`. The `\begin`/`\end` are docstrip *frame lines* anchored at
/// column 0, so the body must not be indented (indenting would push the frame
/// margins off column 0 and split the closing `%␣␣␣␣\end{…}` frame — the corruption
/// this fixes). A pure CST-shape fact: it walks back over inline whitespace from
/// `\begin` and asks only "is the previous token a margin/guard on this line", with
/// no signature lookup, and covers `macrocode` and prose-layer environments
/// uniformly. `DOC_MARGIN`/
/// `GUARD` exist only under the `.dtx` config, so this is always false elsewhere.
pub(super) fn is_margin_framed(node: &SyntaxNode) -> bool {
    let Some(begin) = Environment::cast(node.clone()).and_then(|e| e.begin()) else {
        return false;
    };
    // A frame header occupies its physical line. If body content follows the
    // `\begin` inline, the generic stream must keep it behind the existing `%`;
    // the framed layout would insert a break and turn it into live package code.
    if begin
        .syntax()
        .last_token()
        .and_then(|token| token.next_token())
        .is_some_and(|token| token.kind() != SyntaxKind::NEWLINE)
    {
        return false;
    }
    let mut tok = begin.syntax().first_token().and_then(|t| t.prev_token());
    while let Some(t) = tok {
        match t.kind() {
            SyntaxKind::WHITESPACE => tok = t.prev_token(),
            SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD => return true,
            _ => return false,
        }
    }
    false
}

/// Split a trailing closing-frame margin run off `body`, returning it (the docstrip
/// `\end` frame's `%␣␣␣␣` prefix) so the caller can ride it onto the `\end` line at
/// column 0 instead of leaving it as body tail with a break before `\end` (which
/// would split the frame). The frame is the maximal trailing run of inline
/// `WHITESPACE` / `DOC_MARGIN` / `GUARD` tokens, and only counts as a frame when it
/// actually contains a margin/guard; the `NEWLINE` before it stays in `body` as the
/// trailing break that becomes the frame line's leading break. Returns `None` when
/// `\end` has no preceding margin on its own line (e.g. a prose-layer `\end{…}`
/// authored flush against content), so the caller falls back to the plain
/// no-indent shape.
pub(super) fn split_closing_frame(body: &mut Vec<SyntaxElement>) -> Option<Vec<SyntaxElement>> {
    let mut boundary = body.len();
    let mut has_margin = false;
    while boundary > 0 {
        match &body[boundary - 1] {
            SyntaxElement::Token(t)
                if matches!(t.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD) =>
            {
                has_margin = true;
                boundary -= 1;
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::WHITESPACE => boundary -= 1,
            _ => break,
        }
    }
    has_margin.then(|| body.split_off(boundary))
}

/// Lower a *margin-framed* environment (see [`is_margin_framed`]): a `.dtx`
/// docstrip frame whose `\begin`/`\end` sit on column-0 margin lines. Unlike
/// [`lower_environment`] this never indents the body (the frames are not a real
/// indentation scope) and it pulls the closing `%␣␣␣␣` frame back onto the `\end`
/// line so the terminator stays a single byte-faithful frame line. The body is
/// still lowered as ordinary content — for `macrocode` that is real code whose
/// interior groups/environments indent relative to their column-0 base; for a
/// prose-layer environment it is margin lines, each pinned to column 0 by
/// [`Ir::column_zero`].
pub(super) fn lower_margin_framed_environment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let EnvParts {
        leading,
        begin,
        mut body,
        end,
        lifted,
        tail_len,
        body_header_token: _,
    } = split_environment(node, cx);

    // Pull the `%␣␣␣␣` that frames `\end` onto the `\end` line; what remains is the
    // real body.
    let frame = split_closing_frame(&mut body);
    let frame_ir = frame
        .map(|f| Ir::concat(lower_element_stream(f.into_iter(), cx)))
        .filter(|ir| !matches!(ir, Ir::Nil));

    let body_cx = cx.absorbing_trailing_control_newline(&body);
    let body = lower_env_body(body, tail_len, lifted.is_some(), None, body_cx);
    let (lead_blank, body) = peel_leading_break(body);
    let (trail_blank, body) = peel_trailing_break(body);
    let lead = if lead_blank {
        Ir::empty_line()
    } else {
        Ir::hard_line()
    };
    // The break that separates the body (or `\begin`, for an empty body) from the
    // `\end` frame line.
    let close_break = if trail_blank {
        Ir::empty_line()
    } else {
        Ir::hard_line()
    };

    let env = match (matches!(body, Ir::Nil), frame_ir) {
        // Empty body, framed close: `\begin` then the `%␣␣␣␣\end` frame line.
        (true, Some(frame_ir)) => Ir::concat([begin, close_break, frame_ir, end]),
        // Empty body, no frame: `\begin` and `\end` on their own lines.
        (true, None) => Ir::concat([begin, Ir::hard_line(), end]),
        // Body then the `%␣␣␣␣\end` frame line at column 0.
        (false, Some(frame_ir)) => Ir::concat([begin, lead, body, close_break, frame_ir, end]),
        // Body but no closing margin: behave like a no-indent environment.
        (false, None) => Ir::concat([begin, lead, body, close_break, end]),
    };
    Ir::concat([leading, env])
}

/// The `%` comment that trails the `\begin{…}` header on the *same* source line —
/// only inline whitespace, never a newline, separates the header from it. Such a
/// comment is the space-suppression idiom and belongs on the header line; a
/// comment the author placed on its own line (a newline intervenes) returns
/// `None` and stays in the body. Scans the body in source order, descending into
/// the first node (the body's leading paragraph holds the comment as its first
/// token): inline whitespace is skipped, a comment matches, and anything else —
/// a newline or real content — ends the scan.
pub(super) fn leading_inline_comment(body_elements: &[SyntaxElement]) -> Option<SyntaxToken> {
    for element in body_elements {
        match element {
            SyntaxElement::Token(token) => match token.kind() {
                SyntaxKind::WHITESPACE => continue,
                SyntaxKind::COMMENT => return Some(token.clone()),
                _ => return None,
            },
            SyntaxElement::Node(node) => {
                for token in node
                    .descendants_with_tokens()
                    .filter_map(|e| e.into_token())
                {
                    match token.kind() {
                        SyntaxKind::WHITESPACE => continue,
                        SyntaxKind::COMMENT => return Some(token),
                        _ => return None,
                    }
                }
            }
        }
    }
    None
}

/// Lower an environment body whose leading inline comment has already been lifted
/// onto the `\begin` header by [`lower_environment`]. The comment is dropped from
/// the body to avoid emitting it twice: a bare comment token is skipped outright,
/// and the leading paragraph is re-lowered with its leading whitespace-and-comment
/// run stripped (see [`lower_node_dropping_leading_comment`]). Everything after
/// the comment lowers through the normal stream path.
pub(super) fn lower_body_dropping_leading_comment(
    body_elements: Vec<SyntaxElement>,
    cx: LowerCtx<'_>,
) -> Ir {
    let mut out: Vec<Ir> = Vec::new();
    let mut iter = body_elements.into_iter();
    for element in iter.by_ref() {
        match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::WHITESPACE => continue,
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMENT => break,
            SyntaxElement::Node(node) => {
                out.push(lower_node_dropping_leading_comment(&node, cx));
                break;
            }
            // Unreachable given `leading_inline_comment` matched, but stay lossless.
            SyntaxElement::Token(token) => {
                out.push(lower_loose_token(&token, cx));
                break;
            }
        }
    }
    out.extend(lower_element_stream(iter, cx));
    Ir::concat(out)
}

/// Re-lower `node` with its leading whitespace-and-comment run dropped, using the
/// same dispatch [`lower_node`] would (reflow for a `PARAGRAPH` under
/// [`WrapMode::Reflow`], the generic stream otherwise). Used by
/// [`lower_body_dropping_leading_comment`] to strip a comment lifted onto the
/// `\begin` header.
pub(super) fn lower_node_dropping_leading_comment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let mut children: Vec<SyntaxElement> = node.children_with_tokens().collect();
    let mut i = 0;
    while matches!(
        children.get(i).and_then(|c| c.as_token()).map(|t| t.kind()),
        Some(SyntaxKind::WHITESPACE)
    ) {
        i += 1;
    }
    if matches!(
        children.get(i).and_then(|c| c.as_token()).map(|t| t.kind()),
        Some(SyntaxKind::COMMENT)
    ) {
        children.drain(..=i);
    }
    if node.kind() == SyntaxKind::PARAGRAPH && cx.wraps_prose() {
        reflow_elements(children.into_iter(), cx, ReflowKind::Prose)
    } else {
        Ir::concat(lower_element_stream(children.into_iter(), cx))
    }
}

/// Whether the environment's body should be left at the surrounding indentation
/// level rather than nested one step in (the `noIndent` signature flag — see
/// [`tex_ls_parser::semantic::signature::EnvironmentSig::no_indent`]). The canonical case
/// is `document`, whose body conventionally sits flush against the margin.
pub(super) fn environment_no_indent(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    cx.signatures
        .environment_at(node)
        .is_some_and(|sig| sig.no_indent)
}

/// Whether `node` — a `PARAGRAPH`, or an environment body about to be reflowed as
/// one — sits in the body of an environment the signature DB marks
/// `statementBody`: the TikZ/pgf picture family, whose content is a sequence of
/// `;`-terminated path statements rather than running prose
/// ([`tex_ls_parser::semantic::signature::EnvironmentSig::statement_body`]).
///
/// A prose fill is wrong for such a body in a way width alone cannot express: it
/// runs `\draw …;` and `\node …;` onto one line, and it breaks a `\foreach`
/// header away from its loop variables (issue #114). [`ReflowKind::Statement`]
/// routes the body to the statement layout: under [`WrapMode::Reflow`] the
/// parser's `STATEMENT` nodes are lowered structurally with hung continuations
/// ([`lower_statement`], Tier 1), and content no `;` terminates keeps the
/// authored-line fallback — the same posture a code-like brace-group body takes,
/// with the Tier-2 flush-continuation fixed point carrying over unchanged.
///
/// The **nearest** environment ancestor decides, never any of them. An `itemize`
/// or a `tabular` inside a `\node`'s label holds ordinary prose and must still
/// reflow, though a `tikzpicture` encloses it.
pub(super) fn in_statement_body_env(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    node.ancestors()
        .skip(1)
        .find(|ancestor| ancestor.kind() == SyntaxKind::ENVIRONMENT)
        .and_then(|env| cx.signatures.environment_at(&env))
        .is_some_and(|sig| sig.statement_body)
}

/// Lower a `\begin{name}` node into the header line and the body content the
/// greedy parser over-attached to it.
///
/// **The header ends at the last element glued to it.** Two rules decide where
/// that is. Groups matching the environment's *declared* argument slots are glued
/// to `\begin{name}` whatever the author wrote between them, so
/// `\begin{tabular}\n{cc}` renders as a single `\begin{tabular}{cc}` header.
/// Positional matching skips omitted optional slots. Once the supplied groups
/// exhaust the signature — and from the `{name}` group onwards for an environment
/// with no declared arguments — the header continues only while each boundary is
/// [`Gap::Glued`]; at the first gap it stops, and everything from there is body.
///
/// The slots come from the [`Signatures`] overlay (`cx.signatures`): a document's
/// own `\newenvironment{thm}[1]…` is honored just like a built-in `tabular`, with
/// the scanned definition shadowing a built-in of the same name. A delimiter
/// mismatch against a pending required slot invalidates the positional claim and
/// demotes the rest of the header to ordinary glue boundaries; this keeps an
/// incomplete curated signature from reclassifying source on the next parse.
///
/// Attachment past the declared slots is not an argument claim, so it must not be
/// rendered as one: leaving it in the header
/// stranded it at the `\begin` column, one level short of the body it belongs to
/// (`\begin{center}\n{\bfseries A heading}`). Gluing it up instead would dress body
/// content as an argument, so body is the honest destination.
///
/// Trivia-invariant: only `Glued`-versus-not is read, which the normalized [`Gap`]
/// boundary preserves — a lone newline, a space, and a blank line all fall in the
/// same bucket, so the unsafe predicate never reaches this decision. It is a fixed
/// point in both directions: a glued tail re-parses glued, and a tail sent to the
/// body re-parses separated.
///
/// A `%` that trailed the header on its own source line stays on it (own-line-ness
/// is a preserved predicate, and relocating a trailing comment rebinds it as the
/// next construct's `DOC_COMMENT`); one the author gave its own
/// line travels to the body with the rest of the tail, which is where it already
/// was. But a header comment *with* a declared arity keeps every argument in the
/// header, because both available moves are wrong there: gluing an argument across
/// the `%` would comment it out, and sending it to the body would take a `tabular`'s
/// colspec away from the grid. A mandatory argument after a trailing comment is
/// emitted on an indented continuation line; an optional must retain the generic
/// path, since inserting whitespace before `[…]` can change whether TeX recognizes
/// it. A `.dtx` doc margin or guard is likewise preserved wholesale — both must
/// open their own line.
///
/// Each declared argument is also matched to its signature slot ([`match_arg_slot`],
/// mirroring [`lower_command`]) so a [`ContentKind::Keyval`] argument reaches the
/// delimiter-appropriate segmented layout: `[…]` for `axis`/`tikzpicture`, and
/// `{…}` for tabularray's inner specification. The argument is nested one step
/// beneath the environment header, so a multiline argument's closing delimiter
/// aligns with the surrounding environment body and its content sits one level
/// deeper. Its first line stays attached to `\begin{…}`, and every other content
/// kind lowers exactly as the generic path would.
pub(super) fn lower_begin(begin: &SyntaxNode, cx: LowerCtx<'_>) -> BeginParts {
    let sig = cx.signatures.environment_at(begin);
    let mut has_comment = false;
    let mut has_margin = false;
    for token in begin
        .children_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind() {
            SyntaxKind::COMMENT => has_comment = true,
            SyntaxKind::DOC_MARGIN if !cx.in_dtx_doc_region => has_margin = true,
            SyntaxKind::GUARD => has_margin = true,
            _ => {}
        }
    }
    if has_margin {
        return BeginParts {
            header: lower_node(begin, cx),
            tail: Vec::new(),
        };
    }

    let args = sig.as_ref().map(|sig| &*sig.args).unwrap_or(&[]);
    if has_comment && !args.is_empty() {
        return lower_commented_begin(begin, cx, args);
    }
    let elements: Vec<SyntaxElement> = begin.children_with_tokens().collect();
    let mut head: Vec<Ir> = Vec::new();
    let mut slot = 0usize;
    let mut signature_matches = true;
    let mut i = 0usize;
    while let Some(element) = elements.get(i) {
        match element {
            SyntaxElement::Token(token)
                if cx.in_dtx_doc_region && token.kind() == SyntaxKind::DOC_MARGIN =>
            {
                // The region wrapper owns the canonical margin. Its source
                // padding is not a gap in the virtual LaTeX header.
                i += 1;
                while matches!(
                    elements.get(i),
                    Some(SyntaxElement::Token(next)) if next.kind() == SyntaxKind::WHITESPACE
                ) {
                    i += 1;
                }
            }
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                // Measure the run in place rather than consuming it: when it turns
                // out to be the split point it must travel to the body, so that
                // `leading_inline_comment` sees trivia — not a bare `%` — first and
                // declines to lift an own-line comment back onto the header.
                let (mut end, mut newlines, mut flat) = (i, 0usize, String::new());
                while let Some(SyntaxElement::Token(token)) = elements.get(end) {
                    if !is_collapsible_trivia(token.kind()) {
                        break;
                    }
                    newlines += usize::from(token.kind() == SyntaxKind::NEWLINE);
                    flat.push_str(token.text());
                    end += 1;
                }
                // A following group that matches the next signature slot glues
                // to `\begin{name}`, so the run is dropped. Match on a copy:
                // the group arm commits the slot only once it consumes the node.
                let mut next_slot = slot;
                let declared_arg_follows = signature_matches
                    && elements
                        .get(end)
                        .and_then(attached_arg_kind)
                        .and_then(|kind| match_arg_slot(args, &mut next_slot, kind))
                        .is_some();
                if declared_arg_follows {
                    i = end;
                    continue;
                }
                // A `%` authored on the header line rides it.
                let trails_comment = newlines == 0
                    && matches!(
                        elements.get(end),
                        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::COMMENT
                    );
                if trails_comment {
                    head.push(Ir::verbatim(flat));
                    i = end;
                    continue;
                }
                // Not glued: the header ends here and the rest is body.
                return BeginParts {
                    header: Ir::concat(head),
                    tail: elements[i..].to_vec(),
                };
            }
            SyntaxElement::Node(child)
                if matches!(child.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL) =>
            {
                let kind = attached_arg_kind(element).expect("group or optional argument");
                let spec = signature_matches
                    .then(|| match_arg_slot(args, &mut slot, kind))
                    .flatten();
                // A delimiter mismatch against a pending required slot means the
                // signature is incomplete for this source shape. Demote the rest
                // of the header to ordinary glue boundaries instead of making a
                // later group look declared by skipping over the mismatch.
                if spec.is_none() && slot < args.len() {
                    signature_matches = false;
                }
                let keyval = spec.is_some_and(|spec| spec.content == ContentKind::Keyval);
                let colspec = !keyval
                    && child.kind() == SyntaxKind::GROUP
                    && spec.is_some()
                    && slot == args.len()
                    && sig.as_ref().is_some_and(|sig| sig.align);
                let segmented = if keyval {
                    Some(match child.kind() {
                        SyntaxKind::OPTIONAL => lower_optional(child, cx, true),
                        SyntaxKind::GROUP => lower_segmented_group(
                            child,
                            SyntaxKind::L_BRACE,
                            SyntaxKind::R_BRACE,
                            cx,
                            true,
                        ),
                        _ => unreachable!("argument kind checked above"),
                    })
                } else if colspec {
                    Some(lower_column_spec_group(child, cx))
                } else {
                    None
                };
                let argument = segmented.flatten().unwrap_or_else(|| lower_node(child, cx));
                head.push(if spec.is_some() {
                    Ir::indent(argument)
                } else {
                    argument
                });
                i += 1;
            }
            // The `\begin` control word, the `{name}` group, and anything the
            // author glued past the declared arity stay on the header line.
            SyntaxElement::Node(child) => {
                head.push(lower_node(child, cx));
                i += 1;
            }
            SyntaxElement::Token(token) => {
                head.push(lower_loose_token(token, cx));
                i += 1;
            }
        }
    }
    BeginParts {
        header: Ir::concat(head),
        tail: Vec::new(),
    }
}

/// The positional signature delimiter represented by an attached syntax node.
pub(super) fn attached_arg_kind(element: &SyntaxElement) -> Option<ArgKind> {
    match element.as_node()?.kind() {
        SyntaxKind::GROUP => Some(ArgKind::Brace),
        SyntaxKind::OPTIONAL => Some(ArgKind::Bracket),
        _ => None,
    }
}

/// Lower a commented header slice after removing collapsible gaps before declared
/// arguments, as the ordinary [`lower_begin`] path does. A gap after a comment is
/// never removed because the comment consumes the rest of its line; suppressed
/// gaps likewise remain byte-exact. Virtual `.dtx` margin streams preserve every
/// boundary because an inner rewrite can otherwise change the enclosing margin
/// region's parse shape on the next pass. Declared argument nodes receive the
/// same header-relative nesting as the ordinary [`lower_begin`] path.
pub(super) fn lower_commented_header_stream(
    elements: &[SyntaxElement],
    declared: &[bool],
    mut previous_was_comment: bool,
    cx: LowerCtx<'_>,
) -> Ir {
    if cx.in_dtx_doc_region {
        return Ir::concat(lower_element_stream(elements.iter().cloned(), cx));
    }

    let mut filtered = Vec::with_capacity(elements.len());
    let mut i = 0usize;
    while i < elements.len() {
        let SyntaxElement::Token(token) = &elements[i] else {
            previous_was_comment = false;
            filtered.push((elements[i].clone(), declared[i]));
            i += 1;
            continue;
        };
        if !is_collapsible_trivia(token.kind()) {
            previous_was_comment = token.kind() == SyntaxKind::COMMENT;
            filtered.push((elements[i].clone(), declared[i]));
            i += 1;
            continue;
        }

        let start = i;
        while matches!(
            elements.get(i),
            Some(SyntaxElement::Token(token)) if is_collapsible_trivia(token.kind())
        ) {
            i += 1;
        }
        let followed_by_declared = declared.get(i).copied().unwrap_or(false);
        let suppressed = elements[start..i].iter().any(|element| {
            element
                .as_token()
                .is_some_and(|token| cx.suppressed(token.text_range()))
        });
        if followed_by_declared && !previous_was_comment && !suppressed {
            continue;
        }
        filtered.extend(
            elements[start..i]
                .iter()
                .cloned()
                .zip(declared[start..i].iter().copied()),
        );
    }

    let mut lowered = Vec::new();
    let mut generic = Vec::new();
    for (element, is_declared) in filtered {
        if !is_declared {
            generic.push(element);
            continue;
        }

        lowered.extend(lower_element_stream(generic.drain(..), cx));
        let node = element
            .into_node()
            .expect("declared environment argument must be a syntax node");
        lowered.push(Ir::indent(lower_node(&node, cx)));
    }
    lowered.extend(lower_element_stream(generic.into_iter(), cx));
    Ir::concat(lowered)
}

/// Lower a declared `\begin` header containing a comment without letting the
/// comment detach or consume a later argument. Argument matching mirrors the
/// ordinary [`lower_begin`] path: omitted optional slots are skipped, a pending
/// required-slot mismatch demotes the remaining groups, and content past the
/// completed header becomes [`BeginParts::tail`] for ordinary body reflow.
///
/// When a comment trails one argument and the next matched argument is mandatory,
/// that group is a structural header continuation and receives one indent. The
/// brace gate matters: TeX skips whitespace while scanning a mandatory argument,
/// whereas inserting indentation before an optional `[…]` can change argument
/// recognition.
pub(super) fn lower_commented_begin(
    begin: &SyntaxNode,
    cx: LowerCtx<'_>,
    args: &[ArgSpec],
) -> BeginParts {
    let elements: Vec<SyntaxElement> = begin.children_with_tokens().collect();
    let mut declared = vec![false; elements.len()];
    let mut required_brace = vec![false; elements.len()];
    let mut slot = 0usize;
    let mut signature_matches = true;

    for (i, element) in elements.iter().enumerate() {
        let Some(kind) = attached_arg_kind(element) else {
            continue;
        };
        let spec = signature_matches
            .then(|| match_arg_slot(args, &mut slot, kind))
            .flatten();
        if spec.is_none() && slot < args.len() {
            signature_matches = false;
        }
        if let Some(spec) = spec {
            declared[i] = true;
            required_brace[i] = spec.required && kind == ArgKind::Brace;
        }
    }

    let split = elements
        .iter()
        .enumerate()
        .find_map(|(i, element)| {
            let SyntaxElement::Token(token) = element else {
                return None;
            };
            if !is_collapsible_trivia(token.kind()) {
                return None;
            }

            let mut end = i;
            let mut newlines = 0usize;
            while let Some(SyntaxElement::Token(token)) = elements.get(end) {
                if !is_collapsible_trivia(token.kind()) {
                    break;
                }
                newlines += usize::from(token.kind() == SyntaxKind::NEWLINE);
                end += 1;
            }
            if declared.get(end).copied().unwrap_or(false)
                || (newlines == 0
                    && matches!(
                        elements.get(end),
                        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::COMMENT
                    ))
            {
                None
            } else {
                Some(i)
            }
        })
        .unwrap_or(elements.len());

    let header_elements = &elements[..split];
    let header = header_elements
        .iter()
        .enumerate()
        .find_map(|(i, element)| {
            let SyntaxElement::Token(comment) = element else {
                return None;
            };
            if comment.kind() != SyntaxKind::COMMENT {
                return None;
            }

            let trails_argument =
                header_elements[..i]
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(index, previous)| match previous {
                        SyntaxElement::Token(token) if token.kind() == SyntaxKind::WHITESPACE => {
                            None
                        }
                        SyntaxElement::Node(_) => Some(declared[index]),
                        _ => Some(false),
                    })
                    == Some(true);
            if !trails_argument {
                return None;
            }

            let mut next = i + 1;
            let mut has_newline = false;
            while let Some(SyntaxElement::Token(token)) = header_elements.get(next) {
                if !is_collapsible_trivia(token.kind()) {
                    break;
                }
                has_newline |= token.kind() == SyntaxKind::NEWLINE;
                next += 1;
            }
            if !has_newline || !required_brace.get(next).copied().unwrap_or(false) {
                return None;
            }

            let prefix =
                lower_commented_header_stream(&header_elements[..=i], &declared[..=i], false, cx);
            let continuation = lower_commented_header_stream(
                &header_elements[i + 1..],
                &declared[i + 1..split],
                true,
                cx,
            );
            Some(Ir::concat([prefix, Ir::indent(continuation)]))
        })
        .unwrap_or_else(|| {
            lower_commented_header_stream(header_elements, &declared[..split], false, cx)
        });

    BeginParts {
        header,
        tail: elements[split..].to_vec(),
    }
}
