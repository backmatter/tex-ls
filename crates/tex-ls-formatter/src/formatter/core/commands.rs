use super::*;

/// Whether `command`'s signature marks any argument the [`lower_command`] path
/// must handle specially — a non-[`Opaque`](ContentKind::Opaque) content kind
/// ([`Prose`](ContentKind::Prose) or a [`TokenList`](ContentKind::TokenList)).
/// The cheap guard that gates the
/// [`lower_command`] path in [`lower_node`]: a command with no such argument (the
/// overwhelming common case) lowers generically, so nothing regresses.
pub(super) fn command_has_managed_arg(command: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| {
            sig.args
                .iter()
                .any(|spec| spec.content != ContentKind::Opaque)
        })
}

pub(super) fn command_has_math_arg(command: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| {
            sig.args
                .iter()
                .any(|spec| spec.domain == ArgumentDomain::Math)
        })
}

/// Whether `command` is an *inline* prose command — one whose prose argument sits
/// in running text (`\footnote`, `\emph`, `\textbf`, …) rather than heading its own
/// line. Such a command is flattened into the surrounding reflow stream (see
/// [`flatten_inline_prose`]) so its body wraps as part of the paragraph and its
/// `{`/`}` glue to the adjacent words, instead of block-breaking the braces onto
/// their own lines ([`lower_prose_group`]).
///
/// Driven by the signature DB's explicit [`CommandSig::inline`] flag, not derived:
/// block-level prose commands that head their own line (`\section`, `\caption`)
/// leave it unset and keep the block treatment.
pub(super) fn command_is_inline_prose(command: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| {
            sig.inline
                && sig
                    .args
                    .iter()
                    .any(|spec| spec.content == ContentKind::Prose)
        })
}

/// Whether `command` is an *inline* command that sits in running text (`\citep`,
/// `\ref`, `\emph`, …), per the signature DB's [`CommandSig::inline`] flag. Paragraph
/// reflow uses this so such a command flows into the fill as an atom even when the
/// author isolated it on its own source line, rather than being preserved as a
/// command-only line (see [`line_is_command_only`]). Broader than
/// [`command_is_inline_prose`], which additionally requires a prose argument.
pub(super) fn command_is_inline(command: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| sig.inline)
}

/// Return the curated grammatical role of an inline citation whose key argument
/// is a token list. The content flag is the structural proof that distinguishes
/// citations from other inline commands such as `\ref` or `\emph`; a local
/// redefinition shadows the built-in signature and therefore withdraws the role.
pub(super) fn command_citation_placement(
    command: &SyntaxNode,
    cx: LowerCtx<'_>,
) -> Option<CitationPlacement> {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .and_then(|sig| {
            (sig.inline
                && sig
                    .args
                    .iter()
                    .any(|spec| spec.content == ContentKind::TokenList))
            .then_some(sig.citation)
            .flatten()
        })
}

/// Whether `command` is a *sectioning* command (`\part` … `\subparagraph`), per the
/// signature DB's [`CommandSig::sectioning`] level. Prose reflow treats such a
/// command as a block-level statement. When the command is a direct child of a
/// prose paragraph, it becomes a paragraph-separated block with one blank line on
/// each side (see [`reflow_elements`]).
///
/// Read from the semantic layer, never from a name list in the formatter (decision
/// #2): sectioning level is exactly the kind of semantics the signature DB owns, and
/// `\section` is only a heading because the DB says so.
pub(super) fn command_is_sectioning(command: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| sig.sectioning.is_some())
}

/// Whether `command` is the canonical label command that may remain attached to a
/// preceding section heading.
pub(super) fn command_is_label(command: &SyntaxNode) -> bool {
    command_name(command).as_deref() == Some("label")
}

/// Whether `command` is a curated *block-level* command (`\usepackage`,
/// `\newcommand`, `\maketitle`, …), per the signature DB's [`CommandSig::block`]
/// flag. Prose reflow treats such a command like a sectioning one — a block-level
/// statement on its own line, whatever trivia the author wrote — except that a
/// block command **glued** to adjacent non-trivia keeps its authored adjacency
/// (see [`reflow_elements`]): breaking there materializes a space token TeX
/// typesets (`\ProcessOptions\relax`), where a heading's own `\par` makes the
/// materialized glue provably inert.
///
/// Read from the semantic layer rather than a formatter-owned name list. The
/// flag is positive and curated-only, so an un-signatured or
/// scanned-definition command is *not* block here and falls back to the residual
/// authored-break rule in [`line_is_command_only`].
///
/// A command whose signature declares a *required* argument must actually carry
/// an attached argument node. A **bare** head — `\newcommand` in
/// `\newcommand\foo{…}`, where the control-word run break leaves every argument
/// unattached — is a shape the attachment model did not capture, and
/// intercepting it is not pass-stable: glued to a forced-break sibling it is
/// stranded at end-of-line by the ride path (glue the engine itself breaks), so
/// the adjacency this gate reads differs between passes
/// (`pgfcomp-version-0-65.sty`). A bare head falls to the residual rule, whose
/// authored-break preservation *is* the fixed point of that stranding.
pub(super) fn command_is_block(command: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(command)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| {
            sig.block
                && (sig.args.iter().all(|arg| !arg.required) || command.children().next().is_some())
        })
}

/// Lower one `STATEMENT` node (a `;`-terminated statement in a curated
/// `statementBody` environment body) under [`WrapMode::Reflow`] — the
/// structural statement lowering, entered from `reflow_elements_checked`'s
/// `STATEMENT` arm.
///
/// The interior reflows under [`ReflowKind::StatementInterior`]: a lone
/// newline is a plain atom boundary the width fill re-decides, the
/// command-only residue is off (a width-owned body must not mint forced
/// breaks), a comment still rides and ends its line, and a forced-break child
/// (a `{label}` holding an environment) commits as its own segment with a
/// glued `;` riding its last line. Gaps additionally consult the TikZ unit
/// model (`semantic::tikz::statement_glue`): a unit-internal gap — an operator
/// and what it connects, `at` and its coordinate, a coordinate and its
/// operation, an operation and its argument — renders as a single space and
/// never breaks, so a width wrap lands only at unit boundaries (idiomatically,
/// before a path operator). The enclosing [`Ir::indent`] then hangs every
/// continuation line after the statement head — width wraps, post-comment
/// lines, and block segments alike — one step under it, so a wrapped
/// `\node[…] at (2,3)` / `{…};` reads as a continuation rather than a sibling.
/// Two leading shapes remain outside that hang: a bound
/// [`SyntaxKind::DOC_COMMENT`], which documents the head, and a maximal run of
/// comment-terminated command-only lines before the eventual TikZ statement.
/// [`statement_leading_comment`] and
/// [`statement_leading_command_comment_prefix`] identify those shapes so their
/// commands and the statement head all remain at the body indentation (issue
/// #158). Once non-command statement content begins, a post-comment tail remains
/// a genuine hanging continuation.
///
/// Fixed point (Tier 1): the hang is *emitted*, never read. Statement extent
/// re-derives from the terminating `;` on every parse, however the emitted
/// layout breaks — an emitted wrap is leading line trivia to the next parse —
/// and the interior reads only width, gluedness, comment presence, and
/// non-trivia token text (the unit model), all preserved or content-derived.
/// A peeled leading comment is re-emitted on its own line immediately above the
/// head, so it rebinds as the same `DOC_COMMENT` on the next parse. A peeled
/// command prefix retains each trailing comment, and therefore each forced line
/// boundary, so the same maximal prefix is recognized on the next pass.
/// So `fmt(fmt(x)) == fmt(x)` holds by structure, where the
/// flush-continuation contract this replaces had to *forbid* the hang.
pub(super) fn lower_statement(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let comment = statement_leading_comment(node, cx);
    let body_cx = LowerCtx {
        omitted_leading_comment: comment
            .as_ref()
            .map(SyntaxNode::text_range)
            .or(cx.omitted_leading_comment),
        ..cx
    };
    let elements: Vec<SyntaxElement> = node.children_with_tokens().collect();
    let prefix_end = statement_leading_command_comment_prefix(&elements, body_cx);

    if comment.is_none() && prefix_end == 0 {
        return Ir::indent(reflow_elements(
            elements.into_iter(),
            cx,
            ReflowKind::StatementInterior,
        ));
    }

    let mut leading = Vec::new();
    if let Some(comment) = comment {
        leading.push(Ir::join(
            Ir::hard_line(),
            comment
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|token| token.kind() == SyntaxKind::COMMENT)
                .map(|token| Ir::verbatim(token.text())),
        ));
    }
    if prefix_end > 0 {
        if !leading.is_empty() {
            leading.push(Ir::hard_line());
        }
        leading.push(reflow_elements(
            elements[..prefix_end].iter().cloned(),
            body_cx,
            ReflowKind::StatementInterior,
        ));
    }
    let body = reflow_elements(
        elements[prefix_end..].iter().cloned(),
        body_cx,
        ReflowKind::StatementInterior,
    );
    Ir::concat([Ir::concat(leading), Ir::hard_line(), Ir::indent(body)])
}

/// The maximal statement prefix made of comment-terminated command-only lines.
/// The trailing comment is the structural boundary: it forces the line break
/// and preserves this positional classification across passes without
/// consulting authored newline shape. Non-command content before a comment ends
/// the prefix, so a genuine path tail such as `\draw (0,0) % note` remains
/// hanging-indented. This is only a layout gate; it makes no claim about what an
/// arbitrary macro expands to.
pub(super) fn statement_leading_command_comment_prefix(
    elements: &[SyntaxElement],
    cx: LowerCtx<'_>,
) -> usize {
    let mut prefix_end = 0;
    let mut saw_command = false;
    for (idx, element) in elements.iter().enumerate() {
        match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::WHITESPACE => {}
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::NEWLINE => {
                if saw_command {
                    return prefix_end;
                }
            }
            SyntaxElement::Node(node)
                if node.kind() == SyntaxKind::COMMAND
                    && !cx.suppressed(node.text_range())
                    && !(cx.is_dtx && contains_indented_dtx_comment(node)) =>
            {
                saw_command = true;
            }
            SyntaxElement::Token(token)
                if token.kind() == SyntaxKind::COMMENT
                    && saw_command
                    && !cx.suppressed(token.text_range()) =>
            {
                prefix_end = idx + 1;
                saw_command = false;
            }
            _ => return prefix_end,
        }
    }
    prefix_end
}

/// The bound own-line comment at the front of a structural statement, if its
/// enclosing construct is available to ordinary lowering. The parser keeps the
/// comment inside the command or conditional it documents, and the statement
/// correctly owns that construct; this positional read distinguishes that
/// leading annotation from a genuine mid-statement comment. Its own-line status
/// is a preserved predicate: [`lower_statement`] re-emits the separating hard
/// line that makes it bind forward again.
pub(super) fn statement_leading_comment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Option<SyntaxNode> {
    let construct = node.first_child()?;
    if cx.suppressed(construct.text_range())
        || cx.is_dtx && contains_indented_dtx_comment(&construct)
    {
        return None;
    }
    let comment = construct
        .first_child()
        .filter(|child| child.kind() == SyntaxKind::DOC_COMMENT)
        .filter(|comment| comment.text_range().start() == node.text_range().start())?;
    (!cx.suppressed(comment.text_range())).then_some(comment)
}

/// Pre-pass over a paragraph element stream: splice each `STATEMENT` wrapper's
/// children into the stream, restoring the sibling layout a pre-statement parse
/// produced. Taken by every path that lays a statement-body paragraph out as a
/// *line stream* — the non-`Reflow` prose modes and the `Preserve` paragraph arm
/// — so the wrapper changes no bytes there; only the structural `Reflow`
/// lowering ([`lower_statement`]) reads the node itself.
pub(super) fn flatten_statements(elements: Vec<SyntaxElement>) -> Vec<SyntaxElement> {
    if !elements.iter().any(|e| {
        e.as_node()
            .is_some_and(|n| n.kind() == SyntaxKind::STATEMENT)
    }) {
        return elements;
    }
    let mut out = Vec::with_capacity(elements.len());
    for element in elements {
        match &element {
            SyntaxElement::Node(node) if node.kind() == SyntaxKind::STATEMENT => {
                out.extend(node.children_with_tokens());
            }
            _ => out.push(element),
        }
    }
    out
}

/// Pre-pass over a reflow element stream: replace each *inline* prose command
/// ([`command_is_inline_prose`]) with its surface tokens, splicing its prose
/// argument's body directly into the stream. The body's inter-word whitespace then
/// becomes break opportunities in the surrounding paragraph fill, and the prose
/// `{`/`}` glue onto the adjacent words — so an inline footnote wraps as running
/// text instead of exploding into a block. Non-prose arguments and the control
/// word are kept verbatim; nested inline prose commands are expanded recursively.
/// `glue_matched_args` is enabled only in ordinary prose and prose-argument
/// reflow. A code-like statement or preserve-mode stream keeps its
/// argument-boundary trivia because flattening an inner command must not make an
/// opaque parent group newly flat on the next pass. Virtual `.dtx` margin prose
/// and other margin-carrying streams also keep it: the corpus gate shows that
/// deleting the boundary there is not yet fixed-point stable.
pub(super) fn flatten_inline_prose(
    elements: Vec<SyntaxElement>,
    cx: LowerCtx<'_>,
    glue_matched_args: bool,
) -> Vec<SyntaxElement> {
    let mut out = Vec::new();
    for element in elements {
        match &element {
            SyntaxElement::Node(node)
                if node.kind() == SyntaxKind::COMMAND && command_is_inline_prose(node, cx) =>
            {
                expand_inline_prose(node, cx, glue_matched_args, &mut out);
            }
            _ => out.push(element),
        }
    }
    out
}

/// Expand one inline prose command into `out` (see [`flatten_inline_prose`]): the
/// control word and any non-prose argument are emitted verbatim, while each prose
/// argument is spliced delimiter-and-body via [`splice_prose_group`]. At
/// prose-reflow altitude, collapsible trivia before a matched argument slot is
/// dropped: TeX's undelimited argument scanner already ignores it, and retaining
/// it would let an authored space versus newline choose the command's layout. A
/// comment remains a barrier because the line ending it consumes cannot be
/// removed. Slot matching mirrors [`lower_command`] so an omitted optional does
/// not misalign positions.
pub(super) fn expand_inline_prose(
    node: &SyntaxNode,
    cx: LowerCtx<'_>,
    glue_matched_args: bool,
    out: &mut Vec<SyntaxElement>,
) {
    let Some(sig) = command_name(node).and_then(|name| cx.signatures.command(&name)) else {
        out.push(SyntaxElement::Node(node.clone()));
        return;
    };
    let mut slot = 0usize;
    let mut pending_trivia = Vec::new();
    for child in node.children_with_tokens() {
        if is_collapsible_trivia_element(&child) {
            pending_trivia.push(child);
            continue;
        }
        match child {
            SyntaxElement::Node(group)
                if matches!(group.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL) =>
            {
                let is_bracket = group.kind() == SyntaxKind::OPTIONAL;
                let kind = if is_bracket {
                    ArgKind::Bracket
                } else {
                    ArgKind::Brace
                };
                let spec = match_arg_slot(&sig.args, &mut slot, kind);
                let follows_comment = out.last().is_some_and(
                    |element| matches!(element, SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT),
                );
                if spec.is_none() || follows_comment || !glue_matched_args {
                    out.append(&mut pending_trivia);
                } else {
                    pending_trivia.clear();
                }
                let prose = spec.is_some_and(|spec| spec.content == ContentKind::Prose);
                if prose {
                    let (open, close) = if is_bracket {
                        (SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET)
                    } else {
                        (SyntaxKind::L_BRACE, SyntaxKind::R_BRACE)
                    };
                    splice_prose_group(&group, open, close, cx, glue_matched_args, out);
                } else {
                    out.push(SyntaxElement::Node(group));
                }
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::VERB => {
                out.append(&mut pending_trivia);
                if token.text().starts_with('{') {
                    match_verbatim_arg_slot(&sig.args, &mut slot);
                }
                out.push(SyntaxElement::Token(token));
            }
            other => {
                out.append(&mut pending_trivia);
                out.push(other);
            }
        }
    }
    out.append(&mut pending_trivia);
}

/// Splice a prose group's delimiters and body into `out` (see
/// [`flatten_inline_prose`]). The group's own `open`/`close` tokens are emitted
/// around the body; the body's leading and trailing whitespace is dropped so the
/// delimiters glue tight to the first and last words, and nested inline prose
/// commands inside the body are expanded recursively.
///
/// The delimiter kinds are the node's *own* pair — `{`/`}` for a `GROUP`, `[`/`]`
/// for an `OPTIONAL` — never "any closer". A bracket is ordinary prose content
/// inside a brace group (`\emph{a [b] c}`), and matching it as a delimiter dropped
/// it from the body: the `open` arm is guarded by `open.is_none()`, but a `close`
/// arm matching both kinds is overwritten by the group's real closer, so the `]`
/// vanished from the output entirely — the whitespace-only invariant broken at
/// default settings. Kind-matching is sufficient without also demanding the *last*
/// such token: the formatter only runs on clean parses, where a `GROUP` holds
/// exactly one `R_BRACE` (a second would have closed it) and the parser ends an
/// `OPTIONAL` at its first `]`.
pub(super) fn splice_prose_group(
    group: &SyntaxNode,
    open_kind: SyntaxKind,
    close_kind: SyntaxKind,
    cx: LowerCtx<'_>,
    glue_matched_args: bool,
    out: &mut Vec<SyntaxElement>,
) {
    let mut open: Option<SyntaxElement> = None;
    let mut close: Option<SyntaxElement> = None;
    let mut body: Vec<SyntaxElement> = Vec::new();
    for element in group.children_with_tokens() {
        match &element {
            SyntaxElement::Token(t) if t.kind() == open_kind && open.is_none() => {
                open = Some(element);
            }
            SyntaxElement::Token(t) if t.kind() == close_kind => {
                close = Some(element);
            }
            _ => body.push(element),
        }
    }
    while body.first().is_some_and(is_collapsible_trivia_element) {
        body.remove(0);
    }
    while body.last().is_some_and(is_collapsible_trivia_element) {
        body.pop();
    }
    if let Some(open) = open {
        out.push(open);
    }
    out.extend(flatten_inline_prose(body, cx, glue_matched_args));
    if let Some(close) = close {
        out.push(close);
    }
}

/// True when `element` is a collapsible-trivia token (whitespace/newline), the
/// boundary whitespace [`splice_prose_group`] trims so a prose delimiter glues to
/// its body.
pub(super) fn is_collapsible_trivia_element(element: &SyntaxElement) -> bool {
    matches!(element, SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()))
}

/// Split an inline command's token-list argument into paragraph-fill atoms.
/// Everything before the first entry (the command head and any optional arguments)
/// stays on that atom; everything after the final entry (the closing delimiter and
/// any attached suffix) stays on the last. Returns `None` when there is no useful
/// top-level comma split or the body carries a preserved predicate that forbids
/// segmentation.
pub(super) fn inline_token_list_atoms(node: &SyntaxNode, cx: LowerCtx<'_>) -> Option<Vec<Ir>> {
    let sig = command_name(node).and_then(|name| cx.signatures.command(&name))?;
    let mut slot = 0usize;
    let mut found = false;
    let mut atoms: Vec<Vec<Ir>> = vec![Vec::new()];

    for element in alignment_cell_elements(node.children_with_tokens(), cx) {
        match element {
            SyntaxElement::Node(group)
                if matches!(group.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL) =>
            {
                let is_bracket = group.kind() == SyntaxKind::OPTIONAL;
                let (open_kind, close_kind, kind) = if is_bracket {
                    (
                        SyntaxKind::L_BRACKET,
                        SyntaxKind::R_BRACKET,
                        ArgKind::Bracket,
                    )
                } else {
                    (SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, ArgKind::Brace)
                };
                let spec = match_arg_slot(&sig.args, &mut slot, kind);
                if spec.is_some_and(|spec| spec.content == ContentKind::TokenList) {
                    let GroupSegments {
                        open,
                        mut parts,
                        close,
                        splits,
                    } = segment_delimited_body(&group, open_kind, close_kind, cx, true)?;
                    if splits == 0 {
                        return None;
                    }
                    let _ = peel_padding(&mut parts, Edge::Leading);
                    let _ = peel_padding(&mut parts, Edge::Trailing);
                    atoms.last_mut().unwrap().push(open);
                    for part in parts {
                        if is_segment_separator(&part) {
                            atoms.push(Vec::new());
                        } else {
                            atoms.last_mut().unwrap().push(part);
                        }
                    }
                    atoms.last_mut().unwrap().push(close);
                    found = true;
                } else {
                    atoms.last_mut().unwrap().push(lower_node(&group, cx));
                }
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::VERB => {
                if token.text().starts_with('{') {
                    match_verbatim_arg_slot(&sig.args, &mut slot);
                }
                atoms
                    .last_mut()
                    .unwrap()
                    .push(lower_loose_token(&token, cx));
            }
            SyntaxElement::Node(child) => atoms.last_mut().unwrap().push(lower_node(&child, cx)),
            SyntaxElement::Token(token) => {
                atoms
                    .last_mut()
                    .unwrap()
                    .push(lower_loose_token(&token, cx));
            }
        }
    }

    found.then(|| atoms.into_iter().map(Ir::concat).collect())
}

/// Lower a `COMMAND` whose signature marks an argument's content kind (see
/// [`command_has_managed_arg`], which gates this path). Each attached `{…}`/`[…]`
/// group is matched to its signature slot — kind-aware, so an omitted optional does
/// not misalign positions (`\section{Title}` binds the `{title}` slot, not a
/// leading `[short]`) — and a group filling a prose slot is reflowed via
/// [`lower_prose_group`]. Everything else (non-prose slots, groups past the declared
/// arity that the greedy parser over-attached, trivia) lowers exactly as the generic
/// path would.
pub(super) fn lower_command(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    lower_command_with_math_spacing(node, cx, MathSpacing::Normal)
}

pub(super) fn lower_command_with_math_spacing(
    node: &SyntaxNode,
    cx: LowerCtx<'_>,
    math_spacing: MathSpacing,
) -> Ir {
    let Some(sig) = command_name(node).and_then(|name| cx.signatures.command(&name)) else {
        // Defensive: the guard already proved a prose signature exists.
        return Ir::concat(lower_element_stream(node.children_with_tokens(), cx));
    };
    let math_only = sig
        .args
        .iter()
        .any(|spec| spec.domain == ArgumentDomain::Math);

    let mut out: Vec<Ir> = Vec::new();
    let mut slot = 0usize;
    let mut iter = alignment_cell_elements(node.children_with_tokens(), cx)
        .into_iter()
        .peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Node(child)
                if matches!(child.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL) =>
            {
                let is_bracket = child.kind() == SyntaxKind::OPTIONAL;
                let (open, close) = if is_bracket {
                    (SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET)
                } else {
                    (SyntaxKind::L_BRACE, SyntaxKind::R_BRACE)
                };
                let kind = if is_bracket {
                    ArgKind::Bracket
                } else {
                    ArgKind::Brace
                };
                let spec = match_arg_slot(&sig.args, &mut slot, kind);
                if math_only {
                    if spec.is_some_and(|spec| spec.domain == ArgumentDomain::Math) {
                        out.push(lower_math_argument_group(&child, cx, math_spacing));
                    } else {
                        out.push(Ir::verbatim(child.text().to_string()));
                    }
                    continue;
                }
                match spec.map(|s| s.content) {
                    Some(ContentKind::Prose) => {
                        out.push(lower_prose_group(&child, open, close, cx));
                    }
                    Some(ContentKind::TokenList) => {
                        // Outside a paragraph fill, keep a token list as one inline
                        // atom. The paragraph path exposes its top-level entries as
                        // fill atoms before reaching this lowering.
                        out.push(
                            collapse_arg_group(&child, open, close, cx)
                                .unwrap_or_else(|| lower_node(&child, cx)),
                        );
                    }
                    // A proven `key=value` list: its processor strips spaces around
                    // entries, so the layout may also break at a comma the author
                    // glued (see [`ContentKind::Keyval`]). A `{…}` reaches this only
                    // through the curated tier — the setters (`\pgfkeys`, `\tikzset`)
                    // whose whole mandatory argument is the key list.
                    Some(ContentKind::Keyval) => {
                        out.push(
                            lower_segmented_group(&child, open, close, cx, true)
                                .unwrap_or_else(|| lower_node(&child, cx)),
                        );
                    }
                    _ => out.push(lower_node(&child, cx)),
                }
            }
            SyntaxElement::Node(child) if math_only => {
                out.push(Ir::verbatim(child.text().to_string()))
            }
            SyntaxElement::Node(child) => out.push(lower_node(&child, cx)),
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::VERB => {
                if token.text().starts_with('{') {
                    match_verbatim_arg_slot(&sig.args, &mut slot);
                }
                out.push(lower_loose_token(&token, cx));
            }
            SyntaxElement::Token(token) if math_only => out.push(Ir::verbatim(token.text())),
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                out.push(classify_trivia(
                    consume_gap_widened(&token, &mut iter),
                    cx.in_alignment_cell,
                ));
            }
            SyntaxElement::Token(token) => out.push(lower_loose_token(&token, cx)),
        }
    }
    Ir::concat(out)
}

/// Lower a prose argument group: like [`lower_bracketed`], but the body is reflowed
/// to the line width ([`reflow_elements`]) and the whole thing is wrapped in a soft
/// [`Ir::group`] so it stays on one line when it fits (`\footnote{short}`) and
/// breaks the delimiters onto their own lines, indenting and word-wrapping the body,
/// when it does not. Empty bodies collapse to the bare delimiters.
///
/// A `%` comment at either edge of the body takes [`lower_bracketed`]'s two
/// guards, for the same reasons — the soft group is the only thing that makes
/// them look different here:
///
/// - A comment **glued to the open delimiter** rides the opener's line. Pushing
///   it to its own indented line would turn the newline the formatter writes
///   after `{` into a real space token inside the group, changing `\caption{%\n}`
///   (empty — the `%` eats the source newline) into `\caption{ }`.
/// - A comment the body **ends** with forces the group open, so the close
///   delimiter takes its own line. Flat, the group renders `\caption{x%}` and the
///   `%` comments the closing brace out — a content deletion, and one the
///   whitespace-only oracle sees only as a comment growing a `}`.
///
/// Both bite exactly when the whole body reflows to a *single* line: any second
/// line puts a hard separator between them, which already forces the group.
pub(super) fn lower_prose_group(
    node: &SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    cx: LowerCtx<'_>,
) -> Ir {
    let mut open_ir = Ir::Nil;
    let mut close_ir = Ir::Nil;
    let mut body_elements: Vec<SyntaxElement> = Vec::new();
    for element in alignment_cell_elements(node.children_with_tokens(), cx) {
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

    // The parser emits leading whitespace/newlines as their own trivia tokens, so
    // the first body element is a `COMMENT` iff it was glued to the opener.
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
    let has_trailing_comment = body_ends_with_comment(node, close);

    let body = reflow_elements(body_elements.into_iter(), cx, ReflowKind::ProseArg);
    if matches!(body, Ir::Nil) {
        if has_leading_comment {
            // `\caption{%\n}`: the comment already rode the open delimiter, so
            // the close must still drop to its own line.
            Ir::concat([open_ir, Ir::hard_line(), close_ir])
        } else {
            Ir::concat([open_ir, close_ir])
        }
    } else {
        let brk: fn() -> Ir = if has_leading_comment || has_trailing_comment {
            Ir::hard_line
        } else {
            Ir::soft_line
        };
        Ir::group(Ir::concat([
            open_ir,
            Ir::indent(Ir::concat([brk(), body])),
            brk(),
            close_ir,
        ]))
    }
}

/// Whether the last content token of `node`'s body — the `close` delimiter and
/// any trailing collapsible trivia skipped — is a `%` comment, i.e. whatever the
/// body lowers to ends a line and nothing may follow it there.
///
/// Read at any depth, because a comment nested in the last child is still the
/// last thing emitted *unless* that child's own lowering already put a break
/// after it, which is exactly what the delimiter-bearing lowerings do
/// (`\caption{\emph{a%\n}}` ends on `\emph`'s `}`, not on the comment).
pub(super) fn body_ends_with_comment(node: &SyntaxNode, close: SyntaxKind) -> bool {
    let mut token = node.last_token();
    while let Some(t) = token {
        if !node.text_range().contains_range(t.text_range()) {
            return false; // walked out of the group (an unclosed body)
        }
        match t.kind() {
            SyntaxKind::COMMENT => return true,
            k if k == close || is_collapsible_trivia(k) => token = t.prev_token(),
            _ => return false,
        }
    }
    false
}

/// Collapse a signature-marked [`ContentKind::TokenList`] to a single inline atom.
/// This is the fallback outside paragraph reflow and for lists that cannot expose
/// safe comma boundaries there. Interior newlines collapse to spaces, so a citation
/// list written across lines (`\citep{\n  a,\n  b\n}`) formats identically to its
/// one-line form (`\citep{a, b}`).
///
/// Returns `None` — the caller falls back to the generic form ([`lower_node`]) — when
/// the group is *not* safely collapsible: it holds a blank-line paragraph break, a `%`
/// comment (which must end its line), or force-break content (a nested environment,
/// display math, `\\`). Those keep the indented multi-line block form. Mirrors
/// [`lower_bracketed`]'s delimiter handling and edge-break trimming.
pub(super) fn collapse_arg_group(
    node: &SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    cx: LowerCtx<'_>,
) -> Option<Ir> {
    let mut open_ir = Ir::Nil;
    let mut close_ir = Ir::Nil;
    let mut body: Vec<Ir> = Vec::new();
    let mut iter = node.children_with_tokens().peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Token(t) if t.kind() == open && matches!(open_ir, Ir::Nil) => {
                open_ir = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t) if t.kind() == close => {
                close_ir = Ir::verbatim(t.text());
            }
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                let gap = consume_gap(&t, &mut iter);
                if gap == Gap::Blank {
                    return None; // a blank-line `\par`: keep the block form
                }
                // The gap's flat spelling: a lone newline collapses to a single
                // space, pure inline whitespace stays verbatim, matching the
                // one-line generic lowering.
                body.push(Ir::verbatim(gap.flat()));
            }
            // A `%` comment must terminate its line, so the group cannot collapse.
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => return None,
            SyntaxElement::Token(t) => body.push(Ir::verbatim(t.text())),
            SyntaxElement::Node(child) => {
                let ir = lower_node(&child, cx);
                if ir.contains_forced_break() {
                    return None; // nested block content: keep the block form
                }
                body.push(ir);
            }
        }
    }
    let body = trim_trailing_break(trim_leading_break(Ir::concat(body)));
    Some(Ir::concat([open_ir, body, close_ir]))
}

/// Present a specialized lowerer with the virtual document's LaTeX stream rather
/// than the physical `.dtx` framing stored in the lossless CST. A documentation
/// margin and its following padding belong to the region wrapper; every other
/// element, including the preceding newline, remains available to the layout.
pub(super) fn strip_virtual_dtx_framing(
    elements: impl IntoIterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
) -> Vec<SyntaxElement> {
    if !cx.in_dtx_doc_region {
        return elements.into_iter().collect();
    }

    let mut stripped = Vec::new();
    let mut after_margin = false;
    for element in elements {
        match &element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::DOC_MARGIN => {
                after_margin = true;
            }
            SyntaxElement::Token(token)
                if after_margin && token.kind() == SyntaxKind::WHITESPACE => {}
            _ => {
                after_margin = false;
                stripped.push(element);
            }
        }
    }
    stripped
}

/// Present a nested non-math alignment-cell lowerer with the same virtual stream
/// as the grid itself. Outside that narrow context, retain the node's ordinary
/// stream so enabling virtual grids does not alter unrelated document lowering.
pub(super) fn alignment_cell_elements(
    elements: impl IntoIterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
) -> Vec<SyntaxElement> {
    if cx.in_alignment_cell {
        strip_virtual_dtx_framing(elements, cx)
    } else {
        elements.into_iter().collect()
    }
}
