use super::*;

/// Lower a delimited group — a brace group `{…}` (`open`/`close` =
/// `L_BRACE`/`R_BRACE`) or an optional-argument group `[…]`
/// (`L_BRACKET`/`R_BRACKET`) — indenting its body one step, exactly like
/// [`lower_environment`] but with token delimiters instead of `BEGIN`/`END`
/// nodes. Under the Tier-2 wrap modes it is called for multi-line groups only
/// (see [`spans_multiple_lines`]); under [`WrapMode::Reflow`] it is the block
/// form a group or optional falls back to when the width-driven paths
/// ([`lower_opaque_group`], [`lower_optional`]) decline — a blank line, a
/// comment, nested block content — where the node may also be single-line
/// (`\baz[{c% x\nd}]`).
///
/// A blank line at an optional body's edge remains a blank line. Unlike opaque
/// brace groups, long optionals can now reach this path structurally, and
/// erasing the edge break would both discard their `\par` and make the next pass
/// select the inline layout.
///
/// Inside a group the parser emits body tokens directly (no `PARAGRAPH`
/// wrapping), so the only `open` token is the first child and the only `close`
/// token is the last — but an `OPTIONAL` body may contain a stray `[` (TeX does
/// not nest `[`), so the opener is captured only once (`open_ir` still `Nil`).
pub(super) fn lower_bracketed(
    node: &SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    cx: LowerCtx<'_>,
    keyval: bool,
) -> Ir {
    let mut open_ir = Ir::Nil;
    let mut close_ir = Ir::Nil;
    let mut body_elements: Vec<SyntaxElement> = Vec::new();
    for element in strip_virtual_dtx_framing(node.children_with_tokens(), cx) {
        match &element {
            SyntaxElement::Token(t) if t.kind() == open && matches!(open_ir, Ir::Nil) => {
                open_ir = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t) if t.kind() == close => {
                close_ir = Ir::verbatim(t.text());
            }
            _ => body_elements.push(element),
        }
    }
    let preserve_blank_edges = open == SyntaxKind::L_BRACKET;
    let leading_blank = preserve_blank_edges
        && body_elements
            .iter()
            .take_while(|element| is_collapsible_trivia_element(element))
            .filter(|element| element.kind() == SyntaxKind::NEWLINE)
            .count()
            >= 2;
    let trailing_blank = preserve_blank_edges
        && body_elements
            .iter()
            .rev()
            .take_while(|element| is_collapsible_trivia_element(element))
            .filter(|element| element.kind() == SyntaxKind::NEWLINE)
            .count()
            >= 2;

    // A comment glued to the open delimiter (`{%`, with no newline between them)
    // must ride on the open-delimiter line. Pushing it to its own indented line
    // would turn the newline the formatter inserts after `{` into real whitespace
    // inside the group, changing `\cmd{%\n}` (an empty group — the `%` eats the
    // source newline) into `\cmd{ }` (a group holding a space). The parser emits
    // leading whitespace/newlines as their own trivia tokens, so the first body
    // element is the comment iff it was glued to the opener.
    let has_leading_comment = body_elements
        .first()
        .and_then(SyntaxElement::as_token)
        .is_some_and(|t| t.kind() == SyntaxKind::COMMENT);
    let open_ir = if has_leading_comment {
        let comment = body_elements.remove(0);
        Ir::concat([open_ir, Ir::verbatim(comment.as_token().unwrap().text())])
    } else {
        open_ir
    };

    // Braces and brackets preserve a glued opening edge: adding a newline
    // would inject a space token. Proven keyval processors strip that space.
    let open_glued = !keyval
        && body_elements
            .first()
            .and_then(SyntaxElement::as_token)
            .is_none_or(|token| !is_collapsible_trivia(token.kind()));

    // Mirror the opener rule at the other edge. If the final body element was
    // glued to a meaningful closer, inserting a line break before that closer
    // would add a space token to the group. This is observable in ordinary text
    // and in a `\def` replacement body. Proven keyval processors are exempt: the
    // signature guarantees that surrounding entry whitespace is insignificant.
    let close_glued = !keyval
        && body_elements.last().is_some_and(|element| {
            element
                .as_token()
                .is_none_or(|token| !is_collapsible_trivia(token.kind()))
        });

    // A brace-group body under reflow is laid out as code-like statements: each
    // source line stays its own logical line, but an over-long one wraps to the
    // width instead of forcing the printer to break the innermost nested prose
    // group (the only soft break a rigid `lower_element_stream` body would expose).
    // Optional `[…]` bodies and the non-reflow modes keep the generic stream.
    let body =
        if matches!(cx.wrap, WrapMode::Reflow | WrapMode::Stable) && open == SyntaxKind::L_BRACE {
            reflow_elements(body_elements.into_iter(), cx, ReflowKind::Statement)
        } else {
            Ir::concat(lower_element_stream(body_elements.into_iter(), cx))
        };
    let body = trim_trailing_break(trim_leading_break(body));

    if matches!(body, Ir::Nil) {
        if leading_blank || trailing_blank {
            // One normalized blank line preserves an otherwise empty long
            // optional's `\par` and keeps this path stable on reparse.
            Ir::concat([open_ir, Ir::empty_line(), close_ir])
        } else if has_leading_comment {
            // `{%\n}`: the comment already rode the open delimiter, so the close
            // must still drop to its own line — collapsing to `{%}` would comment
            // out the closing brace.
            Ir::concat([open_ir, Ir::hard_line(), close_ir])
        } else {
            // Empty multi-line body collapses to the bare delimiters, e.g. `{\n}` → `{}`.
            Ir::concat([open_ir, close_ir])
        }
    } else {
        // A glued opener keeps the first body line on the opener's line; the
        // `Ir::indent` still indents the body's *interior* breaks one step, so
        // only the first line rides the opener (`{\aaa` / `␣␣\bbb`).
        let lead = if leading_blank {
            Ir::empty_line()
        } else if open_glued {
            Ir::Nil
        } else {
            Ir::hard_line()
        };
        let trail = if trailing_blank {
            Ir::empty_line()
        } else if close_glued {
            Ir::Nil
        } else {
            Ir::hard_line()
        };
        Ir::concat([
            open_ir,
            Ir::indent(Ir::concat([lead, body])),
            trail,
            close_ir,
        ])
    }
}

/// Whether an opaque brace group directly contains an environment glued to
/// sibling content.
///
/// With no signature proving the group's whitespace semantics, the glued seam
/// says that partially expanding the environment is unsafe: it would insert a
/// space where the source had none and leave the rest of the argument half
/// formatted. Preserve the whole group instead. A comment or any authored gap
/// already supplies a safe boundary and therefore does not trigger this gate.
/// The predicate reads only CST adjacency, so the verbatim result is its own
/// fixed point.
pub(super) fn opaque_group_has_glued_environment_sibling(node: &SyntaxNode) -> bool {
    let elements: Vec<SyntaxElement> = node.children_with_tokens().collect();
    elements.iter().enumerate().any(|(index, element)| {
        let SyntaxElement::Node(child) = element else {
            return false;
        };
        if child.kind() != SyntaxKind::ENVIRONMENT {
            return false;
        }

        let left = index
            .checked_sub(1)
            .and_then(|previous| elements.get(previous));
        glued_group_content(left, SyntaxKind::L_BRACE)
            || glued_group_content(elements.get(index + 1), SyntaxKind::R_BRACE)
    })
}

pub(super) fn glued_group_content(element: Option<&SyntaxElement>, delimiter: SyntaxKind) -> bool {
    match element {
        Some(SyntaxElement::Node(_)) => true,
        Some(SyntaxElement::Token(token)) => token.kind() != delimiter && !is_trivia(token.kind()),
        None => false,
    }
}

/// Lower a brace [`SyntaxKind::GROUP`] under [`WrapMode::Reflow`]: a
/// width-driven fill over its body, so block-vs-inline is decided by width,
/// content, and preserved predicates — never by whether the author happened to
/// break the line ([`spans_multiple_lines`], the unsafe lone-newline
/// predicate; see the trivia-invariant-layout section of `formatter.md`).
///
/// The flat rendering is byte-identical to the generic inline path except that
/// a lone-newline run renders as one space — the newline ↔ space exchange that
/// is TeX-identical. Break opportunities are exactly the perturbation-eligible
/// gaps ([`crate::formatter::perturb`]): a lone-newline run and a single-space
/// gap, both of which render `" "` flat, so `fmt(perturbed) == fmt(original)`
/// holds by construction. Any other gap spelling (`a␣␣b`, a tab) glues
/// verbatim into its atom, and a glued junction never gains a break — breaking
/// where the author glued would inject a space token TeX typesets (the same
/// rationale as [`lower_bracketed`]'s `open_glued`). Edge padding rides the
/// flat rendering and vanishes broken, where the delimiter's own newline
/// supplies the space token; an empty body keeps its padding flat (`{ }` and
/// `{\n}` both render `{ }` — deleting it would delete a space token).
/// One Tier-2 boundary is preserved rather than width-driven: an authored newline
/// immediately after a [`SyntaxKind::LINE_BREAK`] remains a hard source line in a
/// structurally plain, command-only text group. A same-line space remains available
/// to the fill, and inline, command/parameter-like, or glued successors remain
/// untouched; the narrow shape avoids treating `\\` in opaque macro code as a
/// semantic row break.
///
/// A body the fill cannot own takes today's indented block form
/// ([`lower_bracketed`]) instead, keyed on preserved predicates and content
/// only: an *interior* blank line, a direct `%` comment (which must end its
/// line; the glued `{%` case included), a token embedding a newline (a
/// multi-line brace `\verb`), or a child whose IR carries a forced break —
/// nested block content, itself decided by preserved predicates and content
/// under this policy, so the read stays Tier-1-clean. A blank run at the
/// body's *edge* does not decline: the block form trims it away
/// ([`trim_leading_break`]/[`trim_trailing_break`]), so edge-blank presence is
/// not a predicate the block form preserves — it erases to padding here,
/// matching the deletion the block form already performed.
///
/// Known residuals, argued safe by catcode rather than by oracle: a
/// multi-space run stays authored (`{a  b}` and `{a  \nb}` differ, but neither
/// is an eligible perturbation and TeX collapses a catcode-10 run to one space
/// token); a lone newline beside a `\verb` is erased though the oracle
/// excludes VERB-adjacent gaps (a complete `VERB` token carries its
/// delimiters); and an `\obeylines` body joins — unresolvable macro semantics are
/// out of scope, as it is for paragraph reflow generally.
pub(super) fn lower_opaque_group(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    /// Resolve the gap read but not yet committed: a `" "` flat gap is a break
    /// opportunity (the fill's own separator renders it), anything else glues
    /// verbatim into the atom in progress.
    fn commit_gap(atoms: &mut Vec<Ir>, atom: &mut Vec<Ir>, pending: &mut Option<(String, bool)>) {
        if let Some((gap, _)) = pending.take() {
            if gap == " " {
                if !atom.is_empty() {
                    atoms.push(Ir::concat(std::mem::take(atom)));
                }
            } else {
                atom.push(Ir::verbatim(gap));
            }
        }
    }
    let block = || lower_bracketed(node, SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, cx, false);
    if plain_opaque_block_has_authored_rows(node, cx) {
        return block();
    }
    let mut open = Ir::Nil;
    let mut close = Ir::Nil;
    let mut lead: Option<String> = None;
    let mut atoms: Vec<Ir> = Vec::new();
    let mut atom: Vec<Ir> = Vec::new();
    // The flat spelling of the gap read but not yet committed, and whether it
    // was a blank-line run. Only an *interior* blank line declines — the block
    // form trims a blank at the body's edge away (`trim_leading_break` /
    // `trim_trailing_break`), so declining on one would key on a predicate the
    // emitter then destroys: pass 2 would see no blank and flatten (the
    // latexindent `poly-switch-blank-line` family). An edge blank erases to
    // padding, exactly the deletion the block form already performed.
    let mut pending: Option<(String, bool)> = None;
    let mut iter = alignment_cell_elements(node.children_with_tokens(), cx)
        .into_iter()
        .peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Token(t)
                if t.kind() == SyntaxKind::L_BRACE && matches!(open, Ir::Nil) =>
            {
                open = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::R_BRACE => {
                close = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t)
                if cx.in_dtx_doc_region && t.kind() == SyntaxKind::DOC_MARGIN =>
            {
                // The virtual region owns both the physical margin and its source
                // padding. Retaining the padding makes a wrapped group add its
                // indentation again on every pass.
                while matches!(
                    iter.peek(),
                    Some(SyntaxElement::Token(next)) if next.kind() == SyntaxKind::WHITESPACE
                ) {
                    iter.next();
                }
            }
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                let gap = consume_gap(&t, &mut iter);
                let blank = gap == Gap::Blank;
                let flat = gap.flat().to_string();
                if atoms.is_empty() && atom.is_empty() {
                    lead = Some(flat);
                } else {
                    pending = Some((flat, blank));
                }
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => return block(),
            SyntaxElement::Token(t) if t.text().contains('\n') => return block(),
            SyntaxElement::Token(t) => {
                if matches!(pending, Some((_, true))) {
                    return block(); // an interior blank line: preserved predicate
                }
                commit_gap(&mut atoms, &mut atom, &mut pending);
                atom.push(lower_loose_token(&t, cx));
            }
            SyntaxElement::Node(child) => {
                let ir = lower_node(&child, cx);
                if ir.contains_forced_break() {
                    return block(); // nested block content
                }
                if matches!(pending, Some((_, true))) {
                    return block(); // an interior blank line: preserved predicate
                }
                commit_gap(&mut atoms, &mut atom, &mut pending);
                atom.push(ir);
            }
        }
    }
    let trail: Option<String> = pending.take().map(|(gap, _)| gap);
    if !atom.is_empty() {
        atoms.push(Ir::concat(atom));
    }
    if atoms.is_empty() {
        // `{}` / `{ }` / `{\n}`: nothing to lay out; the padding survives flat.
        let lead = lead.map(Ir::verbatim).unwrap_or(Ir::Nil);
        return Ir::concat([open, lead, close]);
    }
    // An edge gap joins the vanish-when-broken protocol only when its flat
    // spelling is `" "` — the one spelling a break reproduces (the broken
    // form's newline re-reads as a lone-newline gap, whose flat is `" "`), and
    // the same criterion the interior break opportunities use. Any other
    // spelling (`{0    }`) must ride verbatim and never break: vanishing it
    // would hand pass 2 a `" "` gap where pass 1 measured four spaces, and the
    // layout oscillates (pgf's `\pgfpoint@oncoil{0    }` coil tables).
    let mut parts: Vec<Ir> = vec![open];
    let mut inner: Vec<Ir> = Vec::new();
    match lead.as_deref() {
        None => {}
        Some(" ") => {
            inner.push(Ir::soft_line());
            inner.push(Ir::if_break(Ir::verbatim(" "), Ir::Nil));
        }
        Some(other) => parts.push(Ir::verbatim(other)),
    }
    inner.push(Ir::fill(atoms));
    parts.push(Ir::indent(Ir::concat(inner)));
    match trail.as_deref() {
        None => {}
        Some(" ") => {
            parts.push(Ir::if_break(Ir::verbatim(" "), Ir::Nil));
            parts.push(Ir::soft_line());
        }
        Some(other) => parts.push(Ir::verbatim(other)),
    }
    parts.push(close);
    Ir::group(Ir::concat(parts))
}

/// Whether an opaque group is the plain text argument of a command that alone
/// occupies its paragraph, with every `\\` followed by an authored newline and
/// flanked by words. This is the conservative shape in which source rows are
/// useful structure rather than plausible macro parameter text.
pub(super) fn plain_opaque_block_has_authored_rows(group: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    if cx.in_dtx_doc_region {
        return false;
    }
    let Some(command) = group
        .parent()
        .filter(|node| node.kind() == SyntaxKind::COMMAND)
    else {
        return false;
    };
    let Some(paragraph) = command
        .parent()
        .filter(|node| node.kind() == SyntaxKind::PARAGRAPH)
    else {
        return false;
    };
    if is_dtx_doc_paragraph(&paragraph) {
        return false;
    }
    let mut content = paragraph
        .children_with_tokens()
        .filter(|element| !is_collapsible_trivia_element(element));
    if !content
        .next()
        .is_some_and(|element| element.as_node() == Some(&command))
        || content.next().is_some()
    {
        return false;
    }
    let elements: Vec<SyntaxElement> = group.children_with_tokens().collect();
    if !elements.iter().all(|element| match element {
        SyntaxElement::Token(token) => matches!(
            token.kind(),
            SyntaxKind::L_BRACE
                | SyntaxKind::R_BRACE
                | SyntaxKind::WORD
                | SyntaxKind::WHITESPACE
                | SyntaxKind::NEWLINE
        ),
        SyntaxElement::Node(node) => node.kind() == SyntaxKind::LINE_BREAK,
    }) {
        return false;
    }
    let mut saw_line_break = false;
    for (index, element) in elements.iter().enumerate() {
        if !matches!(element, SyntaxElement::Node(node) if node.kind() == SyntaxKind::LINE_BREAK) {
            continue;
        }
        saw_line_break = true;
        let left_is_word = elements[..index]
            .iter()
            .rev()
            .find(|element| !is_collapsible_trivia_element(element))
            .is_some_and(
                |element| matches!(element, SyntaxElement::Token(t) if t.kind() == SyntaxKind::WORD),
            );
        let mut newlines = 0usize;
        let right = elements[index + 1..].iter().find(|element| {
            if let SyntaxElement::Token(token) = element
                && is_collapsible_trivia(token.kind())
            {
                newlines += usize::from(token.kind() == SyntaxKind::NEWLINE);
                false
            } else {
                true
            }
        });
        let right_is_word = right.is_some_and(
            |element| matches!(element, SyntaxElement::Token(t) if t.kind() == SyntaxKind::WORD),
        );
        if !left_is_word || newlines != 1 || !right_is_word {
            return false;
        }
    }
    saw_line_break
}

/// Lower a [`SyntaxKind::OPTIONAL`] argument group, or `None` to leave it on the
/// generic inline path. The bracket entry point to [`lower_segmented_group`].
pub(super) fn lower_optional(node: &SyntaxNode, cx: LowerCtx<'_>, keyval: bool) -> Option<Ir> {
    lower_segmented_group(
        node,
        SyntaxKind::L_BRACKET,
        SyntaxKind::R_BRACKET,
        cx,
        keyval,
    )
}

/// Lower a grid environment's column preamble as one all-or-nothing group.
///
/// The final declared brace argument of an environment marked `align` is the
/// column preamble. Whitespace between its top-level syntax elements separates
/// independently readable specifications (`l`, `S[…]`, `p{…}`), while brackets
/// and brace groups stay sealed inside their owning spec. If the flat preamble
/// overflows, every such boundary breaks; a partial fill is harder to scan than
/// either the compact or fully exploded form.
///
/// A comment, blank line, or nested forced break declines this layout and leaves
/// the generic group path in charge. The decision reads normalized [`Gap`]s, so a
/// source space and source newline converge on the same width-driven shape.
pub(super) fn lower_column_spec_group(node: &SyntaxNode, cx: LowerCtx<'_>) -> Option<Ir> {
    if !cx.wraps_prose()
        || (!cx.in_dtx_doc_region
            && (contains_doc_margin(node, cx) || doc_margin_opens_line(node, cx)))
    {
        return None;
    }

    let mut open = Ir::Nil;
    let mut close = Ir::Nil;
    let mut entries: Vec<Ir> = Vec::new();
    let mut current: Vec<Ir> = Vec::new();
    let mut pending_gap: Option<Gap> = None;
    let mut bracket_depth = 0usize;
    let mut iter = strip_virtual_dtx_framing(node.children_with_tokens(), cx)
        .into_iter()
        .peekable();

    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Token(token)
                if token.kind() == SyntaxKind::L_BRACE && matches!(open, Ir::Nil) =>
            {
                open = Ir::verbatim(token.text());
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::R_BRACE => {
                close = Ir::verbatim(token.text());
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::L_BRACKET => {
                if let Some(gap) = pending_gap.take() {
                    current.push(Ir::verbatim(gap.flat()));
                }
                bracket_depth += 1;
                current.push(lower_loose_token(&token, cx));
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::R_BRACKET => {
                bracket_depth = bracket_depth.saturating_sub(1);
                current.push(lower_loose_token(&token, cx));
            }
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                let gap = consume_gap(&token, &mut iter);
                if gap == Gap::Blank {
                    return None;
                }
                if bracket_depth == 0 {
                    pending_gap = Some(gap);
                } else {
                    current.push(Ir::verbatim(gap.flat()));
                }
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMENT => return None,
            SyntaxElement::Token(token) => {
                if let Some(gap) = pending_gap.take()
                    && !current.is_empty()
                {
                    entries.push(Ir::concat(std::mem::take(&mut current)));
                    entries.push(gap.separator());
                }
                current.push(lower_loose_token(&token, cx));
            }
            SyntaxElement::Node(child) => {
                let ir = lower_node(&child, cx);
                if ir.contains_forced_break() {
                    return None;
                }
                if let Some(gap) = pending_gap.take() {
                    current.push(Ir::verbatim(gap.flat()));
                }
                current.push(ir);
            }
        }
    }
    entries.push(Ir::concat(current));
    while entries.last().is_some_and(is_segment_separator) {
        entries.pop();
    }
    let splits = entries.iter().filter(|ir| is_segment_separator(ir)).count();
    if splits == 0 {
        return None;
    }

    Some(Ir::group(Ir::concat([
        open,
        Ir::indent(Ir::concat([Ir::soft_line(), Ir::concat(entries)])),
        Ir::soft_line(),
        close,
    ])))
}

/// Lower a delimited argument group as a comma-segmented Wadler group, or `None` to
/// leave it on the generic inline path.
///
/// The body is a plain Wadler group over its top-level comma-separated entries: flat
/// when it fits the width, one key per line when it does not. An unproven textual
/// optional retains the collapsed gap, so `\foo[a=1,\nb=2]` formats as
/// `\foo[a=1, b=2]` (issue #47). A proven keyval instead canonicalizes every flat
/// separator without a space (`a=1,b=2`); either way, a source line break inside
/// `[…]` is incidental. An over-long bracket *expands* instead of silently
/// overflowing, and the choice no longer depends on where the author broke the line
/// (`spans_multiple_lines` was the unsafe lone-newline predicate; see the
/// trivia-invariant-layout section of `formatter.md`). The fit decision is this
/// group's rest-aware measurement, so trailing same-line content (`]{c}`) counts
/// toward it.
///
/// `keyval` reports that the signature DB proved this argument a `key=value` list
/// (see [`ContentKind::Keyval`]), which additionally licenses splitting a comma the
/// author *glued*. Without it only gaps the author already wrote are break
/// opportunities, so a textual optional (`\item[red,green]`, a `\newcommand`
/// default) can never gain a space that would be typeset.
///
/// `{…}` reaches here only *through* that proof. A mandatory group is the ordinary
/// home of typeset text, so the generic opaque lowering owns it by default; the
/// keyval-family setters (`\pgfkeys`, `\tikzset`, `\lstset`, …) opt in through the
/// curated signature DB, and there segmenting at commas is the whole point — the
/// alternative is reflowing a key list as prose, which wraps mid-key.
///
/// A body that is not safely segmentable — a blank line, a `%` comment, nested
/// block content — takes the indented block form ([`lower_bracketed`])
/// unconditionally, so both spellings of the same content land on it (the
/// choice reads content and preserved predicates, never a lone newline). With
/// no split point at all the group collapses to one atom rather than
/// uselessly detonating into `[\n!htb\n]`. Inert under [`WrapMode::Preserve`]
/// and the other non-prose-wrapping modes, which keep the pre-existing block
/// layout.
pub(super) fn lower_segmented_group(
    node: &SyntaxNode,
    open_kind: SyntaxKind,
    close_kind: SyntaxKind,
    cx: LowerCtx<'_>,
    keyval: bool,
) -> Option<Ir> {
    // A captured xparse `v` argument is same-line by construction. Breaking a
    // preceding optional makes the next parse lose the VERB capture and exposes
    // raw name bytes as ordinary LaTeX syntax.
    if node.next_sibling().is_some_and(|sibling| {
        sibling.kind() == SyntaxKind::GROUP
            && sibling
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .any(|token| token.kind() == SyntaxKind::VERB)
    }) {
        return None;
    }
    // A `[…]` continuing across `.dtx` doc-margined lines keeps its authored
    // margins: `lower_node` already gates on this, but the signature-aware callers
    // reach here directly, and relaying such a body would move content off its `%`.
    //
    // The second gate is the one that bites: a bracket *on* a doc line
    // (`% \begin{function}[EXP, pTF]{…}`) holds no margin token of its own, so
    // `contains_doc_margin` says nothing, yet every line a break here creates would
    // land unmargined — silently promoting documentation to live code. The old
    // lowering was safe by accident (it only ever broke an already-multi-line
    // bracket); a width-driven group has to say so.
    if !cx.wraps_prose()
        || (!cx.in_dtx_doc_region
            && (contains_doc_margin(node, cx) || doc_margin_opens_line(node, cx)))
    {
        // Tier-2 residue: under a mode that does not wrap prose (or on a `.dtx`
        // doc line) the pre-existing behaviour is kept byte for byte — block
        // form when the author broke the line, generic inline path otherwise.
        // Fixed-point argument on [`spans_multiple_lines`].
        return spans_multiple_lines(node)
            .then(|| lower_bracketed(node, open_kind, close_kind, cx, keyval));
    }
    let Some(segments) = segment_delimited_body(node, open_kind, close_kind, cx, keyval) else {
        // Not safely segmentable: blank line, comment, or a child carrying a
        // forced break. The first two put a NEWLINE directly in the node, but
        // the third occurs in single-line spellings too (`\baz[{c\n\nd}]`), so
        // the block form applies unconditionally — both spellings of the same
        // content take it, keyed on content and preserved predicates alone.
        return Some(lower_bracketed(node, open_kind, close_kind, cx, keyval));
    };
    let GroupSegments {
        open,
        mut parts,
        close,
        splits,
    } = segments;
    // Padding at the body's edges rides the flat rendering but must vanish when the
    // delimiters take their own lines, or the first key lands at indent + 1.
    let lead = peel_padding(&mut parts, Edge::Leading);
    let trail = peel_padding(&mut parts, Edge::Trailing);
    let body = Ir::concat(parts);
    if splits == 0 {
        // Nothing to break at: emit the collapsed atom and let it overflow. A
        // breakable group here would push `[!htb]` onto three lines to no gain.
        return Some(Ir::concat([open, lead, body, trail, close]));
    }
    Some(Ir::group(Ir::concat([
        open,
        Ir::indent(Ir::concat([
            Ir::soft_line(),
            Ir::if_break(lead, Ir::Nil),
            body,
        ])),
        Ir::if_break(trail, Ir::Nil),
        Ir::soft_line(),
        close,
    ])))
}

/// Remove and return the whitespace padding at one end of a segmented `[…]` body
/// (see [`is_trimmable_break`]). A [`Ir::Nil`] result means there was none.
pub(super) fn peel_padding(parts: &mut Vec<Ir>, edge: Edge) -> Ir {
    let mut padding = Ir::Nil;
    loop {
        let at = match edge {
            Edge::Leading => 0,
            Edge::Trailing => parts.len().saturating_sub(1),
        };
        match parts.get(at) {
            Some(ir) if is_trimmable_break(ir) => {
                let taken = parts.remove(at);
                if matches!(padding, Ir::Nil) {
                    padding = taken;
                }
            }
            _ => return padding,
        }
    }
}

/// Segment an `OPTIONAL` or `GROUP` body at its top-level commas, or `None` when the
/// body is not safely segmentable — the same three bail conditions as
/// [`collapse_arg_group`]: a blank-line `\par`, a `%` comment (which must end its
/// line), or nested content carrying a forced break.
///
/// A comma is a split point only at bracket depth 0. The parser closes an
/// `OPTIONAL` at its first `]`, so a stray `[` inside the body (TeX does not nest
/// `[`) opens a region that never closes — everything after it stays glued, which
/// is the conservative reading: `\foo[a=[1,2]` must not break at the `1,2`. A `{…}`
/// body needs no matching rule for braces: the parser gives every nested brace group
/// its own `GROUP` node, so the only `L_BRACE`/`R_BRACE` *tokens* here are this
/// body's own delimiters, and a nested comma arrives already sealed inside a child.
pub(super) fn segment_delimited_body(
    node: &SyntaxNode,
    open_kind: SyntaxKind,
    close_kind: SyntaxKind,
    cx: LowerCtx<'_>,
    normalize_commas: bool,
) -> Option<GroupSegments> {
    let mut open = Ir::Nil;
    let mut close = Ir::Nil;
    let mut parts: Vec<Ir> = Vec::new();
    let mut splits = 0usize;
    let mut depth = 0usize;
    // Whether the last content token was a `WORD` ending in `,` at depth 0, so the
    // *next* gap is a break opportunity.
    let mut open_entry = false;
    // Whether the entry currently being accumulated holds any content yet — what
    // tells [`push_entry_word`] a leading comma closes a real entry rather than an
    // empty one.
    let mut entry_open = false;
    let mut iter = strip_virtual_dtx_framing(node.children_with_tokens(), cx)
        .into_iter()
        .peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Token(t) if t.kind() == open_kind && matches!(open, Ir::Nil) => {
                open = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t) if t.kind() == close_kind => {
                close = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::L_BRACKET => {
                depth += 1;
                open_entry = false;
                entry_open = true;
                parts.push(Ir::verbatim(t.text()));
            }
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                let gap = consume_gap(&t, &mut iter);
                if gap == Gap::Blank {
                    return None; // a blank-line `\par`: keep the block form
                }
                if open_entry && depth == 0 {
                    parts.push(if normalize_commas {
                        tight_line()
                    } else {
                        gap.separator()
                    });
                    splits += 1;
                    entry_open = false;
                } else {
                    // Not a split point: the gap rides at its flat spelling, which
                    // collapses a lone newline to a single space and keeps pure
                    // inline whitespace verbatim, matching the generic lowering.
                    parts.push(Ir::verbatim(gap.flat()));
                }
                open_entry = false;
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => return None,
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::WORD && depth == 0 => {
                splits += push_entry_word(t.text(), normalize_commas, &mut parts, entry_open);
                // A word ending in `,` closes its entry and opens an empty one.
                open_entry = t.text().ends_with(',');
                entry_open = !open_entry;
            }
            SyntaxElement::Token(t) => {
                parts.push(lower_loose_token(&t, cx));
                open_entry = false;
                entry_open = true;
            }
            SyntaxElement::Node(child) => {
                let ir = lower_node(&child, cx);
                if ir.contains_forced_break() {
                    return None; // nested block content: keep the block form
                }
                parts.push(ir);
                open_entry = false;
                entry_open = true;
            }
        }
    }
    // A trailing separator (`[a, b, ]`) would put the closing `]` two lines down.
    // Drop it — but an `Ir::Line` replaced authored whitespace above, and an
    // optional is textual, so that space token must survive as trailing padding
    // (`[a, ]` and `[a,\n]` both keep it); a normalized tight separator stood
    // for nothing and restores nothing.
    let mut dropped_gap = false;
    while parts.last().is_some_and(is_segment_separator) {
        dropped_gap |= matches!(parts.last(), Some(Ir::Line));
        parts.pop();
        splits = splits.saturating_sub(1);
    }
    if dropped_gap {
        parts.push(Ir::verbatim(" "));
    }
    Some(GroupSegments {
        open,
        parts,
        close,
        splits,
    })
}

/// Push one body `WORD` onto `parts`, cutting it at each *interior* comma when
/// `normalize_commas` licenses it, and return how many separators were emitted. The comma
/// stays on the piece it terminates (`xmin=-5,` / `xmax=5,`), since it belongs to
/// the key before it.
///
/// A comma whose entry holds nothing at all is an empty entry (a doubled `,`), not
/// a key: it never earns a line of its own and rides along on the next piece
/// instead. That emptiness is *not* a property of this word alone — the lexer ends
/// a `WORD` at every control sequence, so a key list routinely hands us a word that
/// opens with the comma closing the previous token's entry
/// (`width=` `\figurewidth` `,xmin=-5,…`). Hence `entry_open`: whether the caller
/// has already emitted content for the entry this word continues. Every proven
/// whitespace-insensitive separator uses [`tight_line`]: it is empty when flat and
/// an ordinary line break when broken. On the next pass that newline reaches the
/// matching gap arm above and normalizes to the same conditional separator, keeping
/// width decisions at a fixed point (issue #121). The caller's `Keyval` or
/// `TokenList` proof is what licenses removing otherwise-significant whitespace.
pub(super) fn push_entry_word(
    text: &str,
    normalize_commas: bool,
    parts: &mut Vec<Ir>,
    entry_open: bool,
) -> usize {
    if !normalize_commas || !text.contains(',') {
        parts.push(Ir::verbatim(text));
        return 0;
    }
    let mut splits = 0usize;
    let mut start = 0usize;
    let mut pushed = 0usize;
    let mut has_content = entry_open;
    for (i, _) in text.match_indices(',') {
        let piece = &text[start..=i];
        if piece.len() == 1 && !has_content {
            continue; // an empty entry: let the comma ride on the next piece
        }
        if pushed > 0 {
            parts.push(tight_line());
            splits += 1;
        }
        parts.push(Ir::verbatim(piece));
        pushed += 1;
        start = i + 1;
        has_content = false;
    }
    if start < text.len() {
        if pushed > 0 {
            parts.push(tight_line());
            splits += 1;
        }
        parts.push(Ir::verbatim(&text[start..]));
    }
    splits
}

/// A comma-list separator that is glued in flat mode and breaks like an ordinary
/// [`Ir::Line`] in break mode. Its conservative fit width preserves the surrounding
/// layout's established break decisions while its rendering owns the no-space
/// keyval style.
pub(super) fn tight_line() -> Ir {
    Ir::tight_line()
}

/// Whether `ir` is a separator emitted by [`segment_delimited_body`].
pub(super) fn is_segment_separator(ir: &Ir) -> bool {
    matches!(ir, Ir::Line | Ir::TightLine | Ir::SoftLine)
}
