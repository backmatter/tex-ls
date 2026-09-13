use super::*;

/// Lower a [`SyntaxKind::PARAGRAPH`] under [`WrapMode::Reflow`]: greedily wrap its
/// prose to the line width. Maximal runs of *adjacent* non-whitespace elements
/// glue into one unbreakable *atom* (so `Hello,` and `\emph{x}` never split);
/// inter-word whitespace — or a lone newline, since a paragraph holds no blank
/// lines — is a break opportunity. The run lowers to an [`Ir::fill`], which the
/// printer wraps word-by-word.
///
/// Three things end a line rather than flow into the fill: an explicit `\\` line
/// break (a [`SyntaxKind::LINE_BREAK`] node — the parser groups `\\` with its
/// `*` / `[len]` so the whole unit stays on one line), a `%` comment (which must
/// terminate its line), and a nested *block* (an environment or multi-line group
/// whose IR carries a forced break). Each emits the run-so-far as a fill, then
/// the line breaks; a fresh run continues after. The paragraph's lines are joined
/// by [`Ir::hard_line`].
///
/// A paragraph in a `statementBody` environment is *not* prose and takes
/// [`ReflowKind::Statement`] instead — see [`in_statement_body_env`].
pub(super) fn lower_paragraph_reflow(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    reflow_elements(
        node.children_with_tokens(),
        cx,
        paragraph_reflow_kind(node, cx),
    )
}

/// [`ReflowKind::Statement`] for a paragraph in a `statementBody` environment,
/// [`ReflowKind::Prose`] otherwise. Shared by [`lower_paragraph_reflow`] and the
/// `\begin`-tail splice in [`lower_env_body`], so a header the greedy parser
/// over-attached lays out under the same rule as the body it is spliced into.
pub(super) fn paragraph_reflow_kind(node: &SyntaxNode, cx: LowerCtx<'_>) -> ReflowKind {
    if in_statement_body_env(node, cx) {
        ReflowKind::Statement
    } else {
        ReflowKind::Prose
    }
}

/// Whether `node` is a `.dtx` documentation-layer paragraph. A pure CST-shape
/// fact, like [`is_margin_framed`]: `DOC_MARGIN` exists only under the `.dtx` lexer
/// config, so this is unambiguous and always false elsewhere, and it needs no
/// signature lookup. Two shapes count:
/// - The first content token (skipping leading `WHITESPACE`/`NEWLINE` trivia) is a
///   `DOC_MARGIN` — the margin sits inside the paragraph (the first line of a doc
///   block, or a `% \item` body line opening after the `\begin{…}` break).
/// - The margin *floated out*: when a doc paragraph follows a `%` blank line, its
///   leading `%` is attached as inter-paragraph trivia, so the nearest preceding
///   token (skipping inline whitespace on the same line) is a `DOC_MARGIN`. This is
///   the common multi-paragraph case (see [`margin_floats_into_paragraph`], which
///   drops the floated margin so the reflow re-emits a canonical one).
///
/// A guard-led line (`%<…>`, a `GUARD` token) is *not* doc prose, so guards keep
/// their column-0 pin untouched.
pub(super) fn is_dtx_doc_paragraph(node: &SyntaxNode) -> bool {
    // The paragraph's first content token, descending into child nodes: a
    // paragraph that *opens* with a command (`%<package>\def\x{1}` after a guard)
    // is not doc prose just because a later line carries a margin. Walking only
    // direct child tokens would skip the opening `COMMAND` and read that later
    // margin, wrapping guarded code in a `% ` margin that comments it out.
    let margin_inside = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| !is_collapsible_trivia(t.kind()))
        .is_some_and(|t| t.kind() == SyntaxKind::DOC_MARGIN);
    margin_inside
        || node
            .first_token()
            .is_some_and(|t| margin_precedes_on_line(&t))
}

/// Whether a `.dtx` doc paragraph's *first* content token sits on a margined line —
/// either it is the `DOC_MARGIN` itself, or one precedes it on its line.
///
/// [`is_dtx_doc_paragraph`] is deliberately looser: it accepts a paragraph whose
/// margin appears on any later line, because such a paragraph *is* documentation
/// and its margins must still be respected. But the `DtxProse` reflow re-emits a
/// canonical `% ` on *every* line it produces, so a paragraph whose first line is
/// unmargined would gain a `%` it never had — turning code into a comment
/// (`%<package>\def\x{1}`, where the paragraph opens after a guard). Such a
/// paragraph takes the byte-faithful stream instead.
pub(super) fn dtx_paragraph_starts_margined(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| !is_collapsible_trivia(t.kind()))
        .is_some_and(|t| t.kind() == SyntaxKind::DOC_MARGIN || margin_precedes_on_line(&t))
}

/// Whether `node` is a complete, fully margined `.dtx` documentation block that
/// can be formatted as ordinary virtual LaTeX. The physical `%` prefixes are
/// trivia in the CST, but every generated line must regain one; admitting only a
/// line-owning block makes that prefix scope exact.
pub(super) fn dtx_doc_region(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    if !cx.is_dtx
        || cx.in_dtx_doc_region
        || node.kind() != SyntaxKind::ENVIRONMENT
        || has_verbatim_body(node)
        || node
            .descendants()
            .filter_map(Environment::cast)
            .any(|env| matches!(env.name().as_deref(), Some("verbatim" | "verbatim*")))
        || node.text().to_string().contains("\\begin{verbatim")
        || node
            .descendants()
            .filter(|child| child.kind() == SyntaxKind::BEGIN)
            .any(|begin| {
                begin.descendants_with_tokens().any(|element| {
                    element
                        .into_token()
                        .is_some_and(|token| token.kind() == SyntaxKind::NEWLINE)
                })
            })
        || node.descendants_with_tokens().any(|element| {
            element.into_token().is_some_and(|token| {
                matches!(token.kind(), SyntaxKind::GUARD | SyntaxKind::COMMENT)
            })
        })
        || Environment::cast(node.clone()).is_some_and(|environment| {
            matches!(
                environment.name().as_deref(),
                Some("macrocode" | "macrocode*")
            )
        })
    {
        return false;
    }

    let Some(first) = node.first_token() else {
        return false;
    };
    let first_is_margined =
        first.kind() == SyntaxKind::DOC_MARGIN || line_prefix_is_doc_margin(first.prev_token());
    if !first_is_margined {
        return false;
    }

    // The node must own the rest of its closing line. Otherwise a break inside
    // the region could leave following documentation outside the prefix scope.
    let mut next = node.last_token().and_then(|token| token.next_token());
    while let Some(token) = next {
        match token.kind() {
            SyntaxKind::WHITESPACE => next = token.next_token(),
            SyntaxKind::NEWLINE => break,
            _ => return false,
        }
    }

    // Every continuation line must carry its own physical margin in the source.
    // This excludes mixed doc/code constructs and macrocode bodies without
    // needing a semantic guess about where their layers change.
    let tokens: Vec<SyntaxToken> = node
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .collect();
    tokens.iter().enumerate().all(|(index, token)| {
        token.kind() != SyntaxKind::NEWLINE
            || tokens
                .get(index + 1)
                .is_some_and(|next| next.kind() == SyntaxKind::DOC_MARGIN)
    })
}

pub(super) fn environment_begin_has_newline(node: &SyntaxNode) -> bool {
    Environment::cast(node.clone())
        .and_then(|environment| environment.begin())
        .is_some_and(|begin| {
            begin.syntax().descendants_with_tokens().any(|element| {
                element
                    .into_token()
                    .is_some_and(|token| token.kind() == SyntaxKind::NEWLINE)
            })
        })
}

/// Whether the start of the current physical line, walking backward over only
/// source padding, is a documentation margin.
pub(super) fn line_prefix_is_doc_margin(mut token: Option<SyntaxToken>) -> bool {
    while let Some(current) = token {
        match current.kind() {
            SyntaxKind::WHITESPACE => token = current.prev_token(),
            SyntaxKind::DOC_MARGIN => return true,
            _ => return false,
        }
    }
    false
}

/// Whether `margin` is the prefix immediately preceding a virtual documentation
/// region. The region IR re-emits the canonical margin, so this source prefix and
/// its padding must be omitted.
pub(super) fn margin_starts_dtx_doc_region(margin: &SyntaxToken, cx: LowerCtx<'_>) -> bool {
    if margin.kind() != SyntaxKind::DOC_MARGIN || cx.in_dtx_doc_region {
        return false;
    }
    let mut next = margin.next_sibling_or_token();
    while let Some(SyntaxElement::Token(token)) = &next {
        if token.kind() == SyntaxKind::WHITESPACE {
            next = token.next_sibling_or_token();
        } else {
            break;
        }
    }
    let Some(SyntaxElement::Node(node)) = next else {
        return false;
    };
    if dtx_doc_region(&node, cx) {
        return true;
    }
    // A blank doc line ends the preceding paragraph, so the margin can be a
    // root sibling of a paragraph that starts with the virtual environment.
    // The same paragraph may continue with prose after `\end{...}`; that later
    // content does not change ownership of the opener's physical margin.
    if node.kind() != SyntaxKind::PARAGRAPH {
        return false;
    }
    node.children_with_tokens()
        .find(|element| {
            !matches!(
                element,
                SyntaxElement::Token(token) if is_collapsible_trivia(token.kind())
            )
        })
        .and_then(SyntaxElement::into_node)
        .is_some_and(|child| dtx_doc_region(&child, cx))
}

/// Whether `comment` is an ordinary own-line comment kept out of column zero by
/// authored indentation. In `.dtx`, removing that indentation changes even a
/// bare `%` from `COMMENT` to `DOC_MARGIN`, so the enclosing layout unit is
/// opaque.
pub(super) fn is_indented_dtx_comment(comment: &SyntaxToken) -> bool {
    comment.kind() == SyntaxKind::COMMENT
        && comment
            .prev_token()
            .filter(|previous| previous.kind() == SyntaxKind::WHITESPACE)
            .and_then(|whitespace| whitespace.prev_token())
            .is_some_and(|previous| previous.kind() == SyntaxKind::NEWLINE)
}

pub(super) fn contains_indented_dtx_comment(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .any(|comment| is_indented_dtx_comment(&comment))
}

/// Recover the indentation token that sits just outside an opaque node beginning
/// with an indented `.dtx` comment. Generic gap lowering owns that token and would
/// otherwise erase it before the node's verbatim bytes are emitted.
pub(super) fn leading_indented_dtx_comment_padding(node: &SyntaxNode) -> Option<String> {
    let comment = node.first_token()?;
    if !is_indented_dtx_comment(&comment) {
        return None;
    }
    comment
        .prev_token()
        .filter(|previous| previous.kind() == SyntaxKind::WHITESPACE)
        .map(|whitespace| whitespace.text().to_string())
}

pub(super) fn inside_macrocode(node: &SyntaxNode) -> bool {
    node.ancestors()
        .filter_map(Environment::cast)
        .any(|environment| {
            matches!(
                environment.name().as_deref(),
                Some("macrocode" | "macrocode*")
            )
        })
}

/// Whether the physical line `node` starts on opens with a `.dtx` documentation
/// margin or docstrip guard — i.e. everything on it is documentation (or guarded
/// code) that docstrip anchors at column 0.
///
/// Broader than [`margin_precedes_on_line`], which only accepts a margin
/// *immediately* before its token: here the margin may be arbitrarily far back
/// (`% \begin{function}[EXP, pTF]{…}`). A construct that introduces its own line
/// break must consult this, because a break it emits lands on a line the doc layer
/// never margined — turning documentation into live code. [`contains_doc_margin`]
/// cannot see this: the margin sits *outside* the node, before it on the line.
pub(super) fn doc_margin_opens_line(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    if !cx.is_dtx || cx.in_dtx_doc_region {
        return false;
    }
    let mut prev = node.first_token().and_then(|t| t.prev_token());
    while let Some(t) = prev {
        match t.kind() {
            SyntaxKind::NEWLINE => return false,
            SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD => return true,
            _ => prev = t.prev_token(),
        }
    }
    false
}

/// Whether the nearest token before `token`, skipping inline `WHITESPACE` on the
/// same line (stopping at any `NEWLINE` or other token), is a `DOC_MARGIN`: the
/// floated leading margin of a doc paragraph. Mirrors [`is_margin_framed`]'s
/// backward walk.
pub(super) fn margin_precedes_on_line(token: &SyntaxToken) -> bool {
    let mut prev = token.prev_token();
    while let Some(t) = prev {
        match t.kind() {
            SyntaxKind::WHITESPACE => prev = t.prev_token(),
            SyntaxKind::DOC_MARGIN => return true,
            _ => return false,
        }
    }
    false
}

/// Whether `margin` is the floated leading `%` of a reflowable `.dtx` doc
/// paragraph: scanning forward over inline `WHITESPACE` (not a `NEWLINE`), the next
/// sibling is a `PARAGRAPH` that reflows. Such a margin is dropped during reflow
/// because the paragraph's own [`Ir::margin_prefix`] re-emits a canonical `% ` on
/// every line. A `%`-only blank line fails this (its margin is followed by a
/// newline), so it stays a column-0 separator.
pub(super) fn margin_floats_into_paragraph(margin: &SyntaxToken, cx: LowerCtx<'_>) -> bool {
    let mut next = margin.next_sibling_or_token();
    while let Some(SyntaxElement::Token(t)) = &next {
        if t.kind() == SyntaxKind::WHITESPACE {
            next = t.next_sibling_or_token();
        } else {
            break;
        }
    }
    matches!(
        next,
        Some(SyntaxElement::Node(n))
            if n.kind() == SyntaxKind::PARAGRAPH
                && is_dtx_doc_paragraph(&n)
                && dtx_doc_paragraph_reflows_safely(&n, cx)
    )
}

/// Lower a `.dtx` documentation paragraph under [`WrapMode::Reflow`]. When the
/// paragraph is pure running prose ([`dtx_paragraph_reflows`]) the bare prose is
/// reflowed to the line width via [`reflow_elements`] in [`ReflowKind::DtxProse`]
/// mode, which drops each line's `%` margin and re-emits a canonical `% ` margin
/// on every reflowed line (see [`Ir::margin_prefix`]). A complete virtual
/// documentation environment may participate as a self-margin-owning block, so
/// prose on either side keeps reflowing; other paragraphs that contain or sit
/// inside an environment (a `macrocode` block or a `macro`/`environment` doc block)
/// are lowered *preserve-style* so frame margins and item lines round-trip
/// byte-for-byte.
///
/// [`dtx_paragraph_reflows`] is a cheap up-front gate; the exact one is the reflow
/// itself. A forced-break block whose interior lines ride their own margins is
/// committed raw under a canonical first-line margin
/// ([`LineBuilder::push_margined_block`]), and a clean guard line becomes its own
/// column-0 segment ([`collect_guard_line`]), so both reflow with the prose around
/// them. A paragraph whose reflow still commits content *outside* the `% ` margin
/// (a block with an unmargined interior line, a guard line that cannot be
/// isolated — see [`LineBuilder::margin_escaped`]) is re-lowered on the preserve
/// path instead: on an unmargined line a `.dtx` doc comment re-lexes as content,
/// so keeping that layout would break the whitespace-only invariant. The gate
/// reads content only, never [`LowerCtx::wrap`], so `--wrap reflow` on a `.dtx`
/// is exactly as safe as any other mode.
pub(super) fn lower_dtx_doc_paragraph(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    if dtx_doc_paragraph_reflows_safely(node, cx) {
        reflow_elements(node.children_with_tokens(), cx, ReflowKind::DtxProse)
    } else {
        // Margin frames still normalize, but nested inline constructs stay
        // opaque: a width break inside one would create an unmargined line.
        let preserve = LowerCtx {
            preserve_dtx_nested_layout: true,
            ..cx
        };
        Ir::concat(lower_element_stream(node.children_with_tokens(), preserve))
    }
}

/// Whether a `.dtx` documentation paragraph may be reflowed: it is unstructured
/// ([`dtx_paragraph_reflows`]) *and* the reflow keeps every line under the `% `
/// margin ([`LineBuilder::margin_escaped`]). The second half is exact rather than
/// syntactic, so it is answered by running the reflow and throwing the layout away.
///
/// [`margin_floats_into_paragraph`] needs the same answer — a floated leading `%`
/// may only be dropped when the paragraph really does re-emit a canonical margin —
/// so both go through here, memoized per node, and always under the *probing*
/// context so the two callers cannot disagree.
pub(super) fn dtx_doc_paragraph_reflows_safely(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    if !dtx_paragraph_reflows(node, cx) || !dtx_paragraph_starts_margined(node) {
        return false;
    }
    if let Some(&answer) = cx.dtx_reflow_cache.borrow().get(node) {
        return answer;
    }
    // Answer every *nested* doc paragraph first, innermost-first (reverse
    // pre-order visits a node before its ancestors). The probe below lowers this
    // paragraph in full, so an unanswered nested paragraph would start a probe
    // inside a probe — doubling stack depth and duplicating work at every nesting
    // level. Warmed bottom-up, each nested probe is a cache hit instead.
    let nested: Vec<SyntaxNode> = node
        .descendants()
        .filter(|d| d != node && d.kind() == SyntaxKind::PARAGRAPH && is_dtx_doc_paragraph(d))
        .collect();
    for descendant in nested.into_iter().rev() {
        dtx_doc_paragraph_reflows_safely(&descendant, cx);
    }
    let probe = LowerCtx {
        dtx_margin_probe: true,
        ..cx
    };
    let (_, margin_escaped) =
        reflow_elements_checked(node.children_with_tokens(), probe, ReflowKind::DtxProse);
    let answer = !margin_escaped;
    cx.dtx_reflow_cache
        .borrow_mut()
        .insert(node.clone(), answer);
    answer
}

/// Whether a `.dtx` documentation paragraph has only structures that can reflow
/// under its canonical margin. A direct, fully margin-owned environment composes
/// as a self-owning block; an environment hidden inside another child, an unsafe
/// direct environment, or an enclosing environment keeps the paragraph on the
/// byte-faithful preserve path.
pub(super) fn dtx_paragraph_reflows(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    !node
        .ancestors()
        .any(|ancestor| ancestor.kind() == SyntaxKind::ENVIRONMENT)
        && node.children().all(|child| {
            if child.kind() == SyntaxKind::ENVIRONMENT {
                dtx_doc_region(&child, cx)
            } else {
                !child
                    .descendants()
                    .any(|descendant| descendant.kind() == SyntaxKind::ENVIRONMENT)
            }
        })
}

/// Split a logical-line run into sentences and lay each one flat. Adjacent atoms
/// within a run are always whitespace-separated (a glued no-whitespace span is a
/// single atom), so the inter-atom separator is a single literal space and the
/// boundary detector always sees `has_whitespace_after = true`; the final atom
/// closes the last sentence regardless. A single inserted space keeps every
/// preserved token boundary from re-lexing into a merged token, so the result
/// reparses to the same tokens (idempotent).
pub(super) fn render_sentences(run: Vec<RunAtom>, profile: ResolvedProfile<'_>) -> Ir {
    let n = run.len();
    // Break decisions for the n-1 internal gaps, read before the run is consumed.
    let mut break_after = vec![false; n];
    for i in 0..n.saturating_sub(1) {
        let boundary_at_end = is_sentence_boundary_text(
            &run[i].text,
            Some(run[i + 1].text.as_str()),
            true,
            false,
            profile,
        );
        let boundary_before_glued_citation = run[i]
            .trailing_citation
            .filter(|suffix| suffix.start > 0)
            .is_some_and(|suffix| {
                // A TeX tie is an explicit nonbreaking attachment to the
                // citation, not part of the word whose terminal punctuation
                // determines the sentence boundary.
                let before_citation = run[i].text[..suffix.start]
                    .strip_suffix('~')
                    .unwrap_or(&run[i].text[..suffix.start]);
                is_sentence_boundary_text(
                    before_citation,
                    Some(&run[i].text[suffix.start..]),
                    true,
                    false,
                    profile,
                )
            });
        if !(boundary_at_end || boundary_before_glued_citation) {
            continue;
        }

        // A postpositive citation belongs to the preceding sentence even when an
        // earlier formatter stranded it on the next line. An ambiguous citation
        // follows the author's source layout; textual citations never carry a
        // suffix marker and therefore remain on the following sentence.
        let mut sentence_end = i;
        while run.get(sentence_end + 1).is_some_and(|atom| {
            atom.trailing_citation.is_some_and(|suffix| {
                suffix.start == 0
                    && match suffix.placement {
                        CitationPlacement::Postpositive => true,
                        CitationPlacement::Ambiguous => !atom.preferred_break_before,
                        CitationPlacement::Textual => false,
                    }
            })
        }) {
            sentence_end += 1;
        }
        break_after[sentence_end] = sentence_end + 1 < n;
    }

    let mut sentences: Vec<Ir> = Vec::new();
    let mut current: Vec<Ir> = Vec::new();
    for (i, atom) in run.into_iter().enumerate() {
        if !current.is_empty() {
            current.push(Ir::text(" "));
        }
        current.push(atom.ir);
        if break_after[i] {
            sentences.push(Ir::concat(std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        sentences.push(Ir::concat(current));
    }
    Ir::join(Ir::hard_line(), sentences)
}

pub(super) fn reflow_elements(
    elements: impl Iterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
    kind: ReflowKind,
) -> Ir {
    reflow_elements_checked(elements, cx, kind).0
}

/// [`reflow_elements`], additionally reporting whether the result committed any
/// content outside the `% ` documentation margin (see
/// [`LineBuilder::margin_escaped`]). Only meaningful under
/// [`ReflowKind::DtxProse`]; every other kind always reports `false`.
pub(super) fn reflow_elements_checked(
    elements: impl Iterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
    kind: ReflowKind,
) -> (Ir, bool) {
    // Collected up front so the single-newline arm can look ahead at the next
    // physical line ([`line_is_command_only`]). Inline prose commands (`\footnote`,
    // `\emph`, …) are flattened into the stream so their bodies reflow as running
    // text rather than block-breaking their braces (see [`flatten_inline_prose`]);
    // `STATEMENT` wrappers are spliced out the same way (see
    // [`flatten_statements`]) so their contents reflow as the sibling stream
    // they wrap.
    // Under `Reflow` a statement-body run is lowered *structurally*: `STATEMENT`
    // nodes stay whole and take their own arm below (boundaries from the node,
    // continuations hung — see [`lower_statement`]). Every other path splices
    // the wrappers out and keeps the line-stream behavior.
    let structural = kind == ReflowKind::Statement && cx.wrap == WrapMode::Reflow;
    let elements: Vec<SyntaxElement> = elements.collect();
    let elements = if structural {
        elements
    } else {
        flatten_statements(elements)
    };
    let glue_matched_args = !cx.in_dtx_doc_region
        && !run_carries_doc_margin(&elements, cx)
        && matches!(kind, ReflowKind::Prose | ReflowKind::ProseArg);
    let elements: Vec<SyntaxElement> = flatten_inline_prose(elements, cx, glue_matched_args);

    // Inside one structural statement, gaps consult the TikZ unit model: a
    // unit-internal gap renders as a single space instead of a break
    // opportunity (see [`ReflowKind::StatementInterior`]). Computed over the
    // *flattened* stream so the verdict indices match the loop's.
    let unit_glue: Option<Vec<bool>> =
        (kind == ReflowKind::StatementInterior).then(|| statement_glue(&elements));

    // Under `.dtx` prose reflow each segment is wrapped in a `% ` margin prefix and
    // the per-line `DOC_MARGIN` tokens are dropped; `None` otherwise.
    let margin: Option<&'static str> = (kind == ReflowKind::DtxProse).then_some(DTX_DOC_MARGIN);

    // Sentence/semantic segmentation applies to *prose* runs; a `Statement` run is
    // code (a `\newcommand` body), so it keeps the width fill regardless of mode.
    let render = match cx.wrap {
        WrapMode::Stable if kind != ReflowKind::Statement => RunRender::Stable {
            target: cx.stable_target,
        },
        WrapMode::Sentence | WrapMode::Semantic if kind != ReflowKind::Statement => {
            RunRender::Sentence(cx.profile)
        }
        _ => RunRender::Fill,
    };

    let mut b = LineBuilder::new(margin, render);
    // Whether the current *physical* source line so far consists solely of
    // command(s) (and inline whitespace). Such a line is kept on its own line
    // rather than reflowed into its neighbours (see the single-newline arm). Both
    // reset at every physical-line boundary. This is the *residual* command-line
    // rule: curated block-level commands are intercepted upstream (the
    // block-statement arm below) and never depend on it, so what it decides for
    // is un-signatured and scanned-definition commands — whose block-ness no
    // positive signature property can know — plus block commands glued to
    // adjacent content. That residue reads the lone-newline predicate as
    // sanctioned Tier 2: preservation-only, with the fixed-point argument
    // written on [`line_is_command_only`]. It reaches the count through
    // [`consume_widened_gap_slice`], the widened boundary (see [`WideGap`]).
    let mut line_all_commands = true;
    let mut line_has_content = false;
    // Whether the current physical source line rides a `% ` documentation margin.
    // Only meaningful under `DtxProse`. Initialized `true`: both `DtxProse`
    // callers gate on a margined first line ([`dtx_paragraph_starts_margined`],
    // [`dtx_run_starts_margined`]) — a contract that covers the floated-margin
    // paragraph, whose leading `%` sits *outside* the element stream. Cleared at
    // every newline, re-established by the line's `DOC_MARGIN`.
    let mut line_margined = true;
    // Whether the previous element was a forced-break node committed via
    // `push_segment` (a doc-commented command, an environment, …). A `COMMENT`
    // on the same physical line as such a block — glued directly
    // (`\end{center}%`) or after inline whitespace (`\newcommand{…}{…} % note`)
    // — must ride the block's last line: committing it as its own line changes
    // spacing semantics in the glued case and, because an own-line `%` binds
    // forward as a doc comment on reparse, breaks idempotence (issue #38).
    // `block_gap` records that inline whitespace separated the two, so the
    // riding comment keeps a single space before it.
    let mut prev_was_block = false;
    let mut block_gap = false;
    // Set alongside `prev_was_block` when the committed block *closes* its line: an
    // environment, sectioning command, or curated block command. A trailing `%`
    // still rides (it must never be relocated), but content starts a fresh line.
    // Other forced blocks, such as a doc-commented `\input`, leave their last line
    // open so content from the same source line can ride it.
    let mut prev_block_closes_line = false;

    let mut idx = 0;
    while idx < elements.len() {
        let after_block = std::mem::take(&mut prev_was_block);
        let after_block_gap = std::mem::take(&mut block_gap);
        let after_block_closed = std::mem::take(&mut prev_block_closes_line);
        match &elements[idx] {
            // Whitespace / newline run: a physical-line and atom boundary.
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                let newlines = consume_widened_gap_slice(&elements, &mut idx);
                // A unit-internal gap (the TikZ unit model, statement interiors
                // only): glue the neighbors into one atom with a single space —
                // no break opportunity, no line boundary. Content-derived, so a
                // wrap re-reads to the same units (Tier 1); the model never
                // glues across a comment or a blank line.
                if newlines < 2
                    && let Some(glue) = &unit_glue
                    && glue.get(idx).copied().unwrap_or(false)
                {
                    if after_block {
                        // The unit is riding a block segment's last line (a
                        // doc-commented statement head): keep riding — the next
                        // element's `ride_after_block` gap form restores the
                        // single space, and riding appends flat, so the
                        // no-break promise holds there too.
                        prev_was_block = true;
                        block_gap = true;
                        prev_block_closes_line = after_block_closed;
                    } else {
                        b.push_atom_piece(Ir::verbatim(" "), " ");
                    }
                    continue;
                }
                if newlines >= 2 {
                    // A blank line ends the line and promotes the next separator.
                    b.end_line();
                    b.pending_sep = Ir::empty_line();
                    line_all_commands = true;
                    line_has_content = false;
                    line_margined = false;
                } else if newlines == 1 {
                    // A single source newline. Under `Statement` reflow every source
                    // line is its own logical line, so the break always ends the line
                    // (structural `STATEMENT` nodes never reach this arm — they
                    // commit through their own arm below — so this authored-line
                    // read governs only the fallback content no `;` terminates).
                    // Under `Semantic` an authored soft break is likewise preserved
                    // (sembr keeps the writer's clause breaks). Under `Prose`/`Sentence`
                    // it is normally just an atom boundary the run rejoins, except a
                    // line that is *only* command(s) — on either side of the break — is
                    // kept on its own line: end the line so the break survives instead
                    // of collapsing to a space. This is the residual rule for commands
                    // no positive signature property covers (see `line_all_commands`
                    // above); curated block commands never reach it.
                    // The command-only residue is skipped under `ProseArg` and
                    // `StatementInterior`: a width-owned body must not mint
                    // forced breaks pass 2 can see and pass 1 could not (see
                    // [`ReflowKind`]).
                    let residue_applies =
                        !matches!(kind, ReflowKind::ProseArg | ReflowKind::StatementInterior);
                    let prev_is_command = residue_applies && line_has_content && line_all_commands;
                    let next_is_command =
                        residue_applies && line_is_command_only(&elements, idx, cx);
                    if kind == ReflowKind::Statement
                        || cx.wrap == WrapMode::Semantic
                        || prev_is_command
                        || next_is_command
                    {
                        b.end_line();
                    } else {
                        b.flush_atom();
                        // Stable uses this as a soft layout preference; sentence
                        // mode uses it to distinguish a citation intentionally
                        // starting the next source line from one following the
                        // preceding sentence on the same line. The resulting
                        // boundary reproduces itself on the next pass.
                        b.prefer_next_break();
                    }
                    line_all_commands = true;
                    line_has_content = false;
                    line_margined = false;
                } else {
                    // Pure inline whitespace: an atom boundary within the line.
                    // It stays on the block's physical line, so a comment next
                    // keeps riding the block (with the space restored).
                    b.flush_atom();
                    if after_block {
                        prev_was_block = true;
                        block_gap = true;
                        prev_block_closes_line = after_block_closed;
                    }
                }
                continue;
            }
            // A comment trailing content rides the end of that line, then forces a
            // break. But a comment that *begins* its own physical line stays on its
            // own line: end the current line first so the preceding prose run commits
            // separately, instead of reflowing the bare `%` up into that run.
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMENT => {
                if after_block {
                    // On the block segment's last physical line: ride it instead
                    // of starting a line of its own. A directly-glued comment
                    // (`\end{center}%`) stays glued — the `%` is the
                    // space-suppression idiom — while one separated by inline
                    // whitespace keeps a single space.
                    let comment = if after_block_gap {
                        Ir::concat([Ir::verbatim(" "), Ir::verbatim(token.text())])
                    } else {
                        Ir::verbatim(token.text())
                    };
                    b.append_to_last_line(comment);
                } else if line_has_content {
                    // Trailing content on this physical line: ride its end (never
                    // a fill atom of its own — see `append_trailing_comment`).
                    b.append_trailing_comment(token.text());
                    b.end_line();
                } else {
                    b.end_line();
                    b.push_atom_piece(Ir::verbatim(token.text()), token.text());
                    b.end_line();
                }
                line_all_commands = true;
                line_has_content = false;
            }
            // A `\`-at-end-of-line control symbol (`\` + newline) carries its own
            // newline but nothing after it — kept verbatim for losslessness, it
            // ends the line: emit the part before the break as a flat atom and let
            // the line break supply the newline, so the result reparses to the same
            // token (idempotent) instead of leaving an unbreakable multi-line atom
            // inside the run. Restricted to control symbols: a multi-line `VERB`
            // token (a brace-verbatim argument spanning lines) has real content
            // after its newline and must be emitted whole by the arm below.
            SyntaxElement::Token(token)
                if token.kind() == SyntaxKind::CONTROL_SYMBOL && token.text().contains('\n') =>
            {
                let before = token.text().split_once('\n').map(|(b, _)| b).unwrap_or("");
                if !before.is_empty() {
                    b.push_atom_piece(Ir::verbatim(before), before);
                }
                if !cx.absorbs_control_newline(token) {
                    b.end_line();
                }
                line_all_commands = true;
                line_has_content = false;
            }
            // Under `.dtx` prose reflow, a per-line `%` documentation margin
            // (`DOC_MARGIN`) is dropped: the canonical `% ` margin is re-emitted on
            // every reflowed line by the enclosing [`Ir::margin_prefix`] (see
            // `end_line`), so gluing the source `%` into the run would double it.
            // The single space following it is inter-word whitespace the run
            // re-derives. A `GUARD` is *not* dropped (guards keep their column-0 pin).
            SyntaxElement::Token(token)
                if margin.is_some() && token.kind() == SyntaxKind::DOC_MARGIN =>
            {
                line_margined = true;
            }
            // The enclosing virtual-document region re-emits one canonical
            // margin per generated line. Discard the physical marker and its
            // authored padding before ordinary structural lowering continues.
            SyntaxElement::Token(token)
                if cx.in_dtx_doc_region && token.kind() == SyntaxKind::DOC_MARGIN =>
            {
                while elements.get(idx + 1).is_some_and(|element| {
                    matches!(element, SyntaxElement::Token(next) if next.kind() == SyntaxKind::WHITESPACE)
                }) {
                    idx += 1;
                }
            }
            // A `GUARD` (`%<…>`) pins its whole physical line to column 0. Commit
            // that line as one segment under every reflow kind; otherwise two
            // adjacent guarded commands can join, and the second `%<…>` becomes a
            // trailing comment that swallows its command on the next parse. Under
            // a `.dtx` prose margin the isolated segment is deliberately
            // unmargined—the margin and guard would collide.
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::GUARD => {
                if let Some(line) = collect_guard_line(&elements, &mut idx, cx) {
                    b.end_line();
                    b.push_segment(line);
                    line_all_commands = true;
                    line_has_content = false;
                    continue;
                }
                if margin.is_some() {
                    b.note_margin_escape();
                }
                b.push_atom_piece(lower_loose_token(token, cx), token.text());
                line_has_content = true;
                line_all_commands = false;
            }
            // Any other token (WORD, `~`, `&`, `#`, `^`, `_`, brackets, `\verb`,
            // a bare control symbol) glues onto the current atom — prose content,
            // so this physical line is no longer command-only. A `.dtx` margin/guard
            // (only under the dtx config) pins to column 0 instead of reflowing.
            SyntaxElement::Token(token) => {
                if after_block && !after_block_closed {
                    // Content on the block segment's last physical line
                    // (`\input docstrip.tex`, where the doc comment bound to
                    // `\input` made it a block): ride that line instead of
                    // starting one of its own. Same rule as the `COMMENT` arm
                    // above, and the chain continues so the rest of the line
                    // rides too. A block that *closes* its line (a heading) is
                    // excluded — content after it starts a fresh line.
                    b.append_to_last_line(ride_after_block(
                        lower_loose_token(token, cx),
                        after_block_gap,
                    ));
                    prev_was_block = true;
                } else {
                    b.push_atom_piece(lower_loose_token(token, cx), token.text());
                }
                line_has_content = true;
                line_all_commands = false;
            }
            // A structural statement (`\draw …;` in a curated `statementBody`
            // body): its own logical line, boundaries from the node — never from
            // the trivia around it. Only present when `structural` (every other
            // path spliced the wrapper out up front).
            //
            // A statement opens its own line even when the author *glued* it to
            // what precedes (`…;\draw …`). This is the one sanctioned breach of
            // the glued-divider principle, licensed the way `ContentKind::Keyval`
            // licenses the glued comma split: the curated `statementBody` flag
            // asserts that whitespace between a picture body's statements is
            // insignificant to the package that consumes them, so the inserted
            // break is not a typeset change. The claim is held to the curated
            // standard (`bash scripts/check_typeset_stability.sh` carries a case), and it is
            // empirically idle: glued statement seams are unattested in the
            // pgf and user corpora (~6000 statements).
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::STATEMENT => {
                let ir = lower_statement(child, cx);
                b.end_line();
                b.push_segment(ir);
                line_all_commands = true;
                line_has_content = false;
                // One statement per line: following content starts a fresh line
                // (`prev_block_closes_line`), while a trailing `%` still rides
                // (the `COMMENT` arm's `after_block` path).
                prev_was_block = true;
                prev_block_closes_line = true;
            }
            // An explicit `\\` line break (with its `*` / `[len]`, grouped by the
            // parser into one node) rides the end of the current line, then breaks.
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::LINE_BREAK => {
                b.push_atom_piece(lower_node(child, cx), &child.text().to_string());
                b.end_line();
                line_all_commands = true;
                line_has_content = false;
            }
            // An inline citation list participates in the surrounding paragraph
            // fill at its top-level commas, just as an inline prose argument
            // participates at its inter-word gaps. Keeping the entries at this
            // altitude lets the first key share the preceding prose line and the
            // closing brace share the final key's line with following prose.
            SyntaxElement::Node(child)
                if matches!(cx.wrap, WrapMode::Reflow | WrapMode::Stable)
                    && margin.is_none()
                    && !after_block
                    && child.kind() == SyntaxKind::COMMAND
                    && command_is_inline(child, cx)
                    && inline_token_list_atoms(child, cx).is_some() =>
            {
                let atoms = inline_token_list_atoms(child, cx)
                    .expect("match guard proved the inline token list");
                for (index, atom) in atoms.into_iter().enumerate() {
                    if index > 0 {
                        b.flush_atom();
                    }
                    b.push_atom_piece(atom, "");
                }
                line_has_content = true;
                line_all_commands = false;
                idx += 1;
                continue;
            }
            SyntaxElement::Node(child) => {
                let ir = lower_node(child, cx);
                // A block-level command — sectioning (`\part` … `\subparagraph`) or
                // curated block (`\usepackage`, `\newcommand`, …) — is a block-level
                // statement: it opens a line and closes one, whatever trivia the
                // author wrote around it. At paragraph level, sectioning additionally
                // receives an empty line on both sides, making the structural
                // boundary visible. The old behavior kept the break only when
                // the source had a newline there (via `line_is_command_only` below),
                // which is exactly the lone-newline predicate trivia-invariant layout
                // forbids — `\subsection{X}\nprose` and `\subsection{X} prose` are the
                // same bytes to the next parse, so both must lay out alike. Reading
                // `CommandSig::sectioning`/`CommandSig::block` keeps the rule in the
                // semantic layer instead of a formatter-owned name list.
                //
                // Two gates scope the statement treatment:
                // - Not under `ReflowKind::Statement`, whose Tier-2 contract is the
                //   authored line: a brace-group body (`\AtBeginDocument{\setcounter
                //   {page}{1}}`, a one-line `\newcommand` body) must not be forced
                //   open. Block-statement synthesis lives at prose altitude.
                // - A *block* (unlike a sectioning) command must be trivia-isolated
                //   on both sides: breaking where the author glued
                //   (`\ProcessOptions\relax`) materializes a space token TeX
                //   typesets. A heading splits even glued — its own `\par` discards
                //   the materialized glue — so sectioning keeps the unconditional
                //   form. Gluedness is a predicate the formatter preserves, so both
                //   reads are Tier-safe.
                //
                // Forced-break lowerings (a comment inside the title) fall through to
                // the block path below, which already opens and closes a line — and
                // does so through the `.dtx` margin-aware routes this arm's plain
                // `end_line` pair would bypass.
                let is_sectioning =
                    child.kind() == SyntaxKind::COMMAND && command_is_sectioning(child, cx);
                // A blank line is a real `\par`, so synthesize one only where the
                // parser already identified a top-level prose paragraph. Nested
                // headings still get the block command's hard-line boundaries;
                // inserting `\par` inside a macro argument can make a non-`long`
                // macro invalid, and inside a conditional it can reshape the
                // wrapper between formatting passes.
                let is_section_boundary = is_sectioning
                    && child
                        .parent()
                        .is_some_and(|parent| parent.kind() == SyntaxKind::PARAGRAPH);
                let is_section_label = child.kind() == SyntaxKind::COMMAND
                    && command_is_label(child)
                    && label_follows_sectioning_run(&elements, idx, cx);
                let section_label_closes_line =
                    is_section_label && next_is_separated(&elements, idx);
                let is_block_stmt = kind != ReflowKind::Statement
                    && child.kind() == SyntaxKind::COMMAND
                    && (is_sectioning
                        || (command_is_block(child, cx)
                            && b.atom.is_empty()
                            && next_is_separated(&elements, idx)));
                if is_section_label && !ir.contains_forced_break() {
                    let glued_to_previous_label = idx > 0
                        && matches!(
                            &elements[idx - 1],
                            SyntaxElement::Node(previous)
                                if previous.kind() == SyntaxKind::COMMAND
                                    && command_is_label(previous)
                        );
                    if !glued_to_previous_label {
                        b.end_line();
                    }
                    b.push_atom_piece(ir, &child.text().to_string());
                    if section_label_closes_line {
                        b.end_line();
                        if next_nontrivia_is_label(&elements, idx, cx) {
                            b.pending_sep = Ir::hard_line();
                        } else {
                            b.separate_section();
                        }
                        line_has_content = false;
                        prev_was_block = true;
                        prev_block_closes_line = true;
                    } else {
                        line_has_content = true;
                    }
                    line_all_commands = true;
                    idx += 1;
                    continue;
                }
                if is_block_stmt && !ir.contains_forced_break() {
                    b.end_line();
                    if is_section_boundary {
                        b.separate_section();
                    }
                    b.push_atom_piece(ir, &child.text().to_string());
                    b.end_line();
                    if is_section_boundary && !next_nontrivia_is_label(&elements, idx, cx) {
                        b.separate_section();
                    }
                    line_all_commands = true;
                    line_has_content = false;
                    // Committed as a block, so a `%` still on the heading's physical
                    // line rides it (the `COMMENT` arm's `after_block` path) instead
                    // of being stranded on a line of its own, where it would rebind as
                    // the next construct's `DOC_COMMENT` at the next parse. Content
                    // does *not* ride: closing the line is the whole rule.
                    prev_was_block = true;
                    prev_block_closes_line = true;
                    idx += 1;
                    continue;
                }
                if ir.contains_forced_break() {
                    if is_section_boundary {
                        // A documentation comment belongs to the heading it
                        // introduces, so separate the whole forced-break command
                        // from preceding prose rather than splitting the comment
                        // from its command.
                        b.end_line();
                        b.separate_section();
                    }
                    // A virtual documentation environment already owns every
                    // physical margin through its `Ir::doc_margin` wrapper. It is
                    // therefore a complete segment in `DtxProse`: surrounding
                    // prose may keep reflowing, but the accumulator must neither
                    // add another `% ` nor record the block as a margin escape.
                    let owns_dtx_margin =
                        margin.is_some() && dtx_doc_region(child, cx);
                    // A margin-framed `macrocode` chunk opening its own margined
                    // source line, reachable only through a reflowing expl3 run
                    // ([`dtx_run_reflows_safely`]): committed raw behind its
                    // byte-exact source frame lead, never the canonical `% ` —
                    // docstrip matches the `%    \begin{macrocode}` line literally.
                    let frame_lead = (margin.is_some()
                        && line_margined
                        && !line_has_content
                        && is_margin_framed_macrocode(child))
                    .then(|| dtx_env_line_lead(child))
                    .flatten();
                    let hugs_preceding_prose = margin.is_none()
                        && child.kind() == SyntaxKind::INLINE_MATH
                        && line_has_content
                        && matches!(kind, ReflowKind::Prose | ReflowKind::ProseArg)
                        && matches!(b.render, RunRender::Fill);
                    let rides_preceding_block = after_block
                        && !after_block_gap
                        && !after_block_closed
                        && !is_sectioning;
                    if rides_preceding_block {
                        // A forced-break node can itself be glued to the forced
                        // block before it (`{a%\n}{b%\n}`). Keep their shared
                        // boundary on one physical line just as the token and
                        // ordinary-node arms do below. Splitting it would create
                        // a TeX space token where the source had no gap.
                        b.append_to_last_line(ir);
                    } else if margin.is_none() && (!b.atom.is_empty() || hugs_preceding_prose) {
                        // A directly glued block (`\newcommand\cls@hook{%`) had no
                        // source break opportunity, so it extends the unbreakable
                        // atom in progress. The one spaced admission is inline math
                        // in running prose, whose opening fragment remains inline
                        // through the explicit hugging-fill rule below. Both paths
                        // are skipped under a `.dtx` margin, where a generated line
                        // also needs physical framing.
                        b.push_atom_piece(ir, &child.text().to_string());
                        if hugs_preceding_prose {
                            // Inline math remains inline at its opening edge even
                            // when protected comments force later lines. A hugging
                            // fill measures only the math node's first line here,
                            // moving it to the next line only when that prefix does
                            // not fit beside the preceding prose.
                            b.end_hug_line();
                        } else {
                            b.end_line();
                        }
                    } else if let Some(lead) = frame_lead {
                        b.end_line();
                        b.push_segment(Ir::concat([lead, ir]));
                    } else if owns_dtx_margin {
                        b.end_line();
                        b.push_segment(ir);
                    } else if margin.is_some() && line_margined && block_rides_own_margins(child) {
                        // A block amid `.dtx` doc prose whose interior lines all
                        // carry their own column-0 margins, opening on a margined
                        // line: commit it raw on a fresh line with the canonical
                        // margin re-attached for its first line. Its interior
                        // bytes are untouched, so the layout stays within the
                        // `% ` margin and the surrounding prose keeps reflowing.
                        b.end_line();
                        b.push_margined_block(ir);
                    } else {
                        // A block amid prose: end the current line, then place the
                        // block on its own line(s); a fresh run continues after.
                        // `push_segment` applies no margin, so under `.dtx` prose
                        // reflow a block that does not ride its own interior
                        // margins (or opens on an unmargined line) escapes the
                        // `% ` margin — recorded so the caller abandons the
                        // reflow for this paragraph.
                        b.end_line();
                        b.note_margin_escape();
                        b.push_segment(ir);
                    }
                    line_all_commands = true;
                    line_has_content = false;
                    prev_was_block = true;
                    // Environments and display math are complete structural blocks,
                    // so prose after their closers starts a fresh line whether the
                    // source gap was a space or newline. Restrict display math to
                    // genuine prose: the `Statement` path also lowers opaque group
                    // bodies, where breaking a glued suffix would add a meaningful
                    // space token. A block-level statement whose lowering forced a
                    // break (a `%` bound to it as a `DOC_COMMENT`, a comment inside a
                    // title, a multi-line `\title` body) closes its line for the same
                    // reason. Everything else that lands here — an un-signatured
                    // command, a glued block command, `\input`'s bare filename shape
                    // — leaves the line open so following content can ride it (the
                    // `after_block` paths).
                    let display_math_closes_prose = child.kind() == SyntaxKind::DISPLAY_MATH
                        && matches!(kind, ReflowKind::Prose | ReflowKind::ProseArg);
                    prev_block_closes_line = is_block_stmt
                        || section_label_closes_line
                        || child.kind() == SyntaxKind::ENVIRONMENT
                        || display_math_closes_prose;
                    if is_section_boundary && !next_nontrivia_is_label(&elements, idx, cx) {
                        b.separate_section();
                    } else if section_label_closes_line {
                        if next_nontrivia_is_label(&elements, idx, cx) {
                            b.pending_sep = Ir::hard_line();
                        } else {
                            b.separate_section();
                        }
                    }
                } else {
                    // A block-level `COMMAND` keeps the line command-only; an inline
                    // command (`\citep`, `\ref`, …) is running-text content, as is any
                    // other inline node (math, an inline group), and disqualifies it.
                    if after_block && !after_block_closed {
                        // Same as the token arm: content still on the previous
                        // block's last physical line rides it.
                        b.append_to_last_line(ride_after_block(ir, after_block_gap));
                        prev_was_block = true;
                    } else if child.kind() == SyntaxKind::COMMAND
                        && let Some(placement) = command_citation_placement(child, cx)
                        && placement != CitationPlacement::Textual
                    {
                        b.push_trailing_citation(ir, &child.text().to_string(), placement);
                    } else {
                        b.push_atom_piece(ir, &child.text().to_string());
                    }
                    line_has_content = true;
                    line_all_commands &=
                        child.kind() == SyntaxKind::COMMAND && !command_is_inline(child, cx);
                }
            }
        }
        idx += 1;
    }
    let escaped = b.margin_escaped;
    (b.finish(), escaped)
}

/// Collect the guard-led physical line starting at `elements[*idx]` (a `GUARD`,
/// always at column 0) as one byte-faithful, single-line segment, advancing
/// `*idx` past the line's content; the terminating newline run is left for the
/// caller's trivia arm. Trailing inline whitespace before that newline is
/// skipped (the formatter never emits trailing whitespace — a trivia-only
/// change). Returns `None` with `*idx` untouched when the line cannot be
/// isolated — an element on it spans a newline, or its lowering carries a
/// forced break — in which case the caller records a margin escape.
pub(super) fn collect_guard_line(
    elements: &[SyntaxElement],
    idx: &mut usize,
    cx: LowerCtx<'_>,
) -> Option<Ir> {
    let mut end = *idx;
    while let Some(element) = elements.get(end) {
        let spans_lines = match element {
            SyntaxElement::Token(t) => t.text().contains('\n'),
            SyntaxElement::Node(n) => n.text().contains_char('\n'),
        };
        if spans_lines {
            if matches!(element, SyntaxElement::Token(t) if is_collapsible_trivia(t.kind())) {
                break;
            }
            return None;
        }
        end += 1;
    }
    let mut last = end;
    while last > *idx
        && matches!(&elements[last - 1], SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()))
    {
        last -= 1;
    }
    let ir = Ir::concat(lower_element_stream(
        elements[*idx..last].iter().cloned(),
        cx,
    ));
    if ir.contains_forced_break() {
        return None;
    }
    *idx = end;
    Some(ir)
}

/// Lower a `PARAGRAPH` that overlaps an expl3 region. The paragraph is split at the
/// `\ExplSyntaxOn`/`Off` toggles into maximal in-region and out-of-region runs;
/// each in-region run lays out as expl3 code ([`lower_expl_code`]), each out-of-region
/// run keeps the ordinary prose/stream treatment. The common case — a whole
/// paragraph inside a region (a `.sty`/`.dtx` body, or a blank-line-separated
/// `\ExplSyntaxOn…Off` block) — is a single in-region run. Runs are joined by a hard
/// line break (a region boundary always begins a fresh line).
/// Prefix `ir` with the single space that separated it from the block segment it
/// rides, when the source had one. Directly-glued content (`\end{center}%`) stays
/// glued — the missing space is the space-suppression idiom.
pub(super) fn ride_after_block(ir: Ir, gap: bool) -> Ir {
    if gap {
        Ir::concat([Ir::verbatim(" "), ir])
    } else {
        ir
    }
}

/// Whether an element run carries a `.dtx` documentation margin or docstrip guard
/// anywhere inside it. Both pin to column 0, so a run containing one is never
/// *generic*-prose-reflowed (see [`lower_expl_paragraph`]); a margined run may
/// still reflow under the `% ` margin when [`dtx_run_reflows_safely`] holds.
/// Always false outside the `.dtx` lexer config, where neither token kind exists.
pub(super) fn run_carries_doc_margin(run: &[SyntaxElement], cx: LowerCtx<'_>) -> bool {
    if !cx.is_dtx {
        return false;
    }
    run.iter().any(|element| match element {
        SyntaxElement::Token(t) => {
            matches!(t.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD)
        }
        SyntaxElement::Node(n) => contains_doc_margin(n, cx),
    })
}

/// Slice analogue of [`dtx_paragraph_starts_margined`] for an out-of-region expl3
/// run: the run's first non-trivia token (descending into nodes) must be a
/// `DOC_MARGIN`. Deliberately without the [`margin_precedes_on_line`] backward
/// walk — `prev_token` would cross the run boundary into the previous run's
/// byte-faithful output, and a margin owned there must not count (the reflow
/// would re-emit a second `% ` onto the same physical line).
pub(super) fn dtx_run_starts_margined(run: &[SyntaxElement]) -> bool {
    for element in run {
        let first = match element {
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => continue,
            SyntaxElement::Token(t) => Some(t.clone()),
            SyntaxElement::Node(n) => n
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| !is_collapsible_trivia(t.kind())),
        };
        if let Some(token) = first {
            return token.kind() == SyntaxKind::DOC_MARGIN;
        }
    }
    false
}

/// Run analogue of [`dtx_doc_paragraph_reflows_safely`] for a doc-margined
/// out-of-region expl3 run: opening on a margined line
/// ([`dtx_run_starts_margined`]), unstructured, and the speculative reflow keeps
/// every line under the `% ` margin. Three deliberate differences from the
/// paragraph gate. "Unstructured" *admits* a margin-framed `macrocode` chunk as
/// a direct run element — such a run is exactly how a doc-layer paragraph
/// overlaps an expl3 region (the chunk bodies are the region; the doc lines
/// around them are the out-of-region rest), and the chunk commits raw behind
/// its byte-exact source frame lead ([`dtx_env_line_lead`]), so docstrip's
/// literal `%    \begin{macrocode}` match survives. Any other environment in
/// the run — or around it, like [`dtx_paragraph_reflows`]'s ancestor half
/// (prose inside a `% \begin{macro}` doc block keeps its authored `%    `
/// margins; reflowing structured doc content stays out of scope) — still
/// declines. And unmemoized: nothing asks twice for a run
/// ([`margin_floats_into_paragraph`] consults only `PARAGRAPH` nodes), so each
/// run is probed at most once.
pub(super) fn dtx_run_reflows_safely(run: &[SyntaxElement], cx: LowerCtx<'_>) -> bool {
    if !dtx_run_starts_margined(run) {
        return false;
    }
    let structured = run.iter().any(|element| match element {
        SyntaxElement::Node(n) if is_margin_framed_macrocode(n) => false,
        SyntaxElement::Node(n) => n.descendants().any(|d| d.kind() == SyntaxKind::ENVIRONMENT),
        SyntaxElement::Token(_) => false,
    }) || run
        .first()
        .and_then(SyntaxElement::parent)
        .is_some_and(|p| p.ancestors().any(|a| a.kind() == SyntaxKind::ENVIRONMENT));
    if structured {
        return false;
    }
    let probe = LowerCtx {
        dtx_margin_probe: true,
        ..cx
    };
    !reflow_elements_checked(run.iter().cloned(), probe, ReflowKind::DtxProse).1
}

/// Whether `node` is a margin-framed `macrocode`/`macrocode*` chunk — the one
/// environment [`dtx_run_reflows_safely`] admits inside a reflowing run. The
/// curated name set matches [`intersect_macrocode_bodies`]: these are the chunks
/// whose bodies *are* the expl3 regions, so a doc paragraph cannot overlap a
/// region without containing one.
pub(super) fn is_margin_framed_macrocode(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::ENVIRONMENT
        && Environment::cast(node.clone())
            .is_some_and(|e| matches!(e.name().as_deref(), Some("macrocode" | "macrocode*")))
        && is_margin_framed(node)
}

/// Whether an element is the explicit toggle that closes an expl3 region.
pub(super) fn is_expl_syntax_off_command(element: &SyntaxElement) -> bool {
    let SyntaxElement::Node(command) = element else {
        return false;
    };
    command.kind() == SyntaxKind::COMMAND
        && command.first_token().is_some_and(|token| {
            token.kind() == SyntaxKind::CONTROL_WORD
                && expl_toggle(token.text()) == Some(ExplToggle::Off)
        })
}

/// The byte-exact line lead of a margin-framed block opening a fresh source
/// line: its preceding siblings walked backward are optional inline
/// `WHITESPACE` then the line's `DOC_MARGIN`. Returns the lead re-lowered as
/// column-0 IR (`%` pinned, whitespace verbatim), or `None` when the block is
/// not led by a directly-preceding margin. Used to commit a `macrocode` chunk
/// raw during `DtxProse` reflow with its source frame margin — docstrip
/// recognizes the literal `%    \begin{macrocode}` line, so the canonical `% `
/// re-emitted for prose must never replace a frame lead.
pub(super) fn dtx_env_line_lead(node: &SyntaxNode) -> Option<Ir> {
    let mut ws: Option<String> = None;
    let mut prev = node.prev_sibling_or_token();
    if let Some(SyntaxElement::Token(t)) = &prev
        && t.kind() == SyntaxKind::WHITESPACE
        && !t.text().contains('\n')
    {
        ws = Some(t.text().to_string());
        prev = t.prev_sibling_or_token();
    }
    match prev {
        Some(SyntaxElement::Token(t)) if t.kind() == SyntaxKind::DOC_MARGIN => {
            let margin = Ir::column_zero(t.text());
            Some(match ws {
                Some(ws) => Ir::concat([margin, Ir::verbatim(ws)]),
                None => margin,
            })
        }
        _ => None,
    }
}
