use super::*;

/// True if `node` (an `ENVIRONMENT`) names an environment the signature DB marks
/// `align` — an `align`/matrix-family environment whose `&` columns the formatter
/// lays out into a grid (see [`lower_aligned_environment`]).
pub(super) fn is_alignment_env(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    cx.signatures
        .environment_at(node)
        .is_some_and(|sig| sig.align)
}

/// True if `node` (an `ENVIRONMENT`) carries a **top-level `&`** — an alignment tab
/// that is a direct child of the body (or of its single wrapping `PARAGRAPH`), not
/// nested inside a group or sub-environment. A `&` at catcode 4 reading as a column
/// tab is a static CST-shape fact, so an environment the signature DB cannot name
/// (`myaligned`, issue #84) still routes to the `&`-column grid when it is shaped
/// like one. It mirrors the cell boundary [`build_alignment_grid`]/`flatten_alignment_body`
/// use, so the routing decision and the grid it enables agree on what a top-level
/// `&` is; a nested `&` lives in a child node and is correctly invisible. Deliberately
/// keyed on `&` alone (not `\\`): a `\\`-only body is a line stack, not a column
/// alignment, and gridding an arbitrary `\begin{center}a \\ b\end{center}` would
/// reflow it.
pub(super) fn body_has_top_level_ampersand(node: &SyntaxNode) -> bool {
    node.children_with_tokens().any(|el| match el {
        SyntaxElement::Token(t) => t.kind() == SyntaxKind::AMPERSAND,
        SyntaxElement::Node(p) if p.kind() == SyntaxKind::PARAGRAPH => p
            .children_with_tokens()
            .any(|g| g.kind() == SyntaxKind::AMPERSAND),
        _ => false,
    })
}

/// True if `node` (an `ENVIRONMENT`) names an environment the signature DB marks
/// `math` — `equation`, `align`, `gather`, matrix, … The parser wraps such a body
/// in a `MATH` node (it entered math mode); [`lower_math_environment`] lays it out
/// with the math-aware paths.
pub(super) fn is_math_env(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    cx.signatures
        .environment_at(node)
        .is_some_and(|sig| sig.math)
}

/// Lower an `align`/matrix-family environment, laying out its `&` columns into a
/// grid so the ampersands line up. The framing (`\begin`/`\end`, the indented
/// body with leading/trailing `hard_line`) is identical to [`lower_environment`];
/// only the body differs — it is the rendered grid rather than a generic element
/// stream.
///
/// Falls back to [`lower_environment`] whenever the body is not a clean
/// single-paragraph grid (see [`build_alignment_grid`]): a blank-line break, or a
/// cell that cannot collapse to one aligned line (a mid-row comment or a nested
/// block). Comment-only and rule-only lines (`\hline`, `\midrule`, …) are *not* a
/// reason to fall back — they are kept as passthrough lines between rows. The
/// fallback is always available, so an unhandled shape degrades to today's plain
/// indented body, never a panic or corruption.
pub(super) fn lower_aligned_environment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let EnvParts {
        leading,
        begin,
        body,
        end,
        lifted,
        // The `BEGIN` tail (see [`lower_begin`]) rides `body` as ordinary leading
        // elements: this path flattens the body itself, so it needs no splice.
        tail_len: _,
        body_header_token: _,
    } = split_environment(node, cx);

    let body_cx = cx.absorbing_trailing_control_newline(&body);
    let soften_nested_newlines = cx.in_dtx_doc_region && !is_math_env(node, cx);
    let Some(items) = build_alignment_grid(
        &body,
        body_cx,
        false,
        soften_nested_newlines,
        lifted.as_ref(),
    ) else {
        return lower_environment(node, cx);
    };
    if !items.iter().any(|item| matches!(item, GridItem::Row(_))) {
        // A body with no actual rows (empty, `\\`-only, or comment-only) has no
        // grid; let the generic path render it.
        return lower_environment(node, cx);
    }

    let aligns = column_alignments(node, cx).unwrap_or_default();
    let body = render_alignment_rows(&items, &aligns);
    Ir::concat([
        leading,
        begin,
        Ir::indent(Ir::concat([Ir::hard_line(), body])),
        Ir::hard_line(),
        end,
    ])
}

/// Lower a named **math** environment (`equation`, `align`, `gather`, matrix, …),
/// whose body the parser wrapped in a `MATH` node. Two layouts, chosen by the body's
/// shape:
///
/// - **Grid** (a top-level `&` or `\\`): `align`/matrix column-and-row grids, and
///   `gather`/`multline` row stacks (a single column). Reuses [`build_alignment_grid`]
///   in `math` mode, so cells get role-aware math spacing.
/// - **Single formula** (neither): `equation`/`displaymath`. Routes the `MATH` body
///   through [`lower_display_math_body`], the relation-aware amsmath-style breaker,
///   so a too-long formula breaks at its top-level relations/operators.
///
/// Framing (leading, `\begin` header, indented body, `\end`) mirrors
/// [`lower_display_math`] and [`lower_aligned_environment`]. If the body is not a
/// `MATH` node — which only happens if the formatter's `math` signature view diverges
/// from the parser's built-in one — it falls back to [`lower_environment`] rather than
/// mislaying the body.
pub(super) fn lower_math_environment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let EnvParts {
        leading,
        begin,
        body: body_elements,
        end,
        lifted,
        // The `BEGIN` tail (see [`lower_begin`]) rides `body` as ordinary leading
        // elements: this path flattens the body itself, so it needs no splice.
        tail_len: _,
        body_header_token: _,
    } = split_environment(node, cx);
    let body_cx = cx.absorbing_trailing_control_newline(&body_elements);

    let Some(math_node) = body_elements
        .iter()
        .filter_map(|e| e.as_node())
        .find(|n| n.kind() == SyntaxKind::MATH)
    else {
        // The parser did not enter math mode for this environment (its built-in
        // signature is not `math`); render it generically.
        return lower_environment(node, cx);
    };

    // A top-level `&` or `\\` inside the `MATH` body means a grid; otherwise it is a
    // single formula.
    let is_grid = math_node
        .children_with_tokens()
        .any(|e| matches!(e.kind(), SyntaxKind::AMPERSAND | SyntaxKind::LINE_BREAK));

    let body = if is_grid {
        match build_alignment_grid(&body_elements, body_cx, true, false, lifted.as_ref()) {
            Some(items) if items.iter().any(|item| matches!(item, GridItem::Row(_))) => {
                let aligns = column_alignments(node, cx).unwrap_or_default();
                render_alignment_rows(&items, &aligns)
            }
            // A grid we cannot lay out on aligned rows (a mid-row comment, a blank
            // line, a nested block that is not the last cell of its row): fall back
            // to the generic environment lowering, exactly as
            // [`lower_aligned_environment`] does. A *trailing* nested block keeps
            // the grid — it renders as a hanging block cell (see [`Cell::block`]).
            _ => return lower_environment(node, cx),
        }
    } else {
        // Flatten the whole body, not only the parser's `MATH` child. Greedy
        // `\begin` attachment can leave a detached leading group in `body`
        // before that node (`\begin{equation}\n{\foo} : T`); selecting only
        // `math_node` would silently delete the group. The siblings are math
        // content too, and the display-formula lowerer handles them directly.
        let mut elements: Vec<SyntaxElement> = Vec::new();
        for element in &body_elements {
            if element.as_node() == Some(math_node) {
                elements.extend(math_node.children_with_tokens());
            } else {
                elements.push(element.clone());
            }
        }
        // Drop the lifted `\begin`-line comment (the `MATH` node's first token)
        // before the formula lowering, which would otherwise re-emit it.
        elements.retain(|e| !is_lifted_comment(e, lifted.as_ref()));
        if elements.iter().all(|e| {
            e.as_token()
                .is_some_and(|t| is_collapsible_trivia(t.kind()))
        }) {
            // Empty body (possibly only after the lift): `\begin` and `\end` on
            // adjacent lines, as [`lower_environment`]'s empty-body branch does,
            // rather than framing an empty indented line.
            return Ir::concat([leading, begin, Ir::hard_line(), end]);
        }
        trim_trailing_break(lower_display_formula_elements(&elements, body_cx))
    };

    Ir::concat([
        leading,
        begin,
        Ir::indent(Ir::concat([Ir::hard_line(), body])),
        Ir::hard_line(),
        end,
    ])
}

/// Split an alignment environment body into a sequence of grid items (rows and
/// passthrough lines), or `None` to signal the caller should fall back to the
/// generic environment lowering.
///
/// Rows are delimited by *top-level* `\\` ([`SyntaxKind::LINE_BREAK`]) nodes and
/// cells by top-level `&` ([`SyntaxKind::AMPERSAND`]) tokens; a `&` nested inside a
/// group or sub-environment lives in a child node, never a direct body child, so
/// it is correctly invisible here. Each cell's elements lower through the generic
/// [`lower_element_stream`] and render *flat* (so inline math/groups normalize as
/// they do elsewhere), trimmed of surrounding space.
///
/// **Comments and rule lines.** A physical line between rows that is made up solely
/// of comments and/or horizontal-rule commands (`\hline`, `\midrule`, …) is kept as
/// a [`GridItem::Passthrough`] line, not a cell. A comment at the end of a row's
/// last physical line — directly after the row's `\\`, or trailing the final row —
/// is attached as the row's `trailing_comment`; comment-only lines after it are
/// passthrough lines like any other. A comment in the *middle* of a row (with more
/// cells after it) cannot sit on an aligned line — its text runs to end of line,
/// commenting out the rest — so it returns `None` and falls back.
///
/// Returns `None` when [`flatten_alignment_body`] rejects the body (a blank-line
/// break), when a cell carries a forced break that cannot collapse to one line (a
/// nested block, or a blank line inside the cell — a lone continuation newline is
/// joined, not a fallback), or on a mid-row comment. Exception: in a *math* grid, a
/// cell whose forced break comes from a nested block environment (`aligned`,
/// `cases`, a matrix) becomes a multi-line *block* cell ([`Cell::block`]) instead of
/// a fallback, provided it is the last cell of its row — the grid survives and the
/// nested environment's lines hang at its `\begin{…}` column.
///
/// `math` is `true` for math grids (`align`, `pmatrix`, …) whose body the parser
/// wrapped in a `MATH` node: the flattener descends that node and each cell lowers
/// through the role-aware math sequencer ([`lower_math_seq`]) so operator spacing
/// and tight scripts apply. It is `false` for a non-math grid (`tabular`),
/// where the body is a prose block and cells lower through [`lower_element_stream`]
/// exactly as before. `soften_nested_newlines` is restricted to non-math cells in
/// fully owned virtual `.dtx` regions: it makes a margin-framed continuation inside
/// a parser-attached command behave like the same continuation after margin
/// normalization, while ordinary tables and math-grid fallbacks retain their
/// existing forced-break gates.
pub(super) fn build_alignment_grid(
    body_elements: &[SyntaxElement],
    cx: LowerCtx<'_>,
    math: bool,
    soften_nested_newlines: bool,
    lifted: Option<&SyntaxToken>,
) -> Option<Vec<GridItem>> {
    let mut inline = flatten_alignment_body(body_elements, cx, math)?;
    // The `\begin`-line trailing comment was lifted onto the header
    // ([`split_environment`]); drop it here so it is not re-emitted as a
    // passthrough line of its own.
    inline.retain(|e| !is_lifted_comment(e, lifted));
    let printer = Printer::new(FormatStyle::default());
    let cell_cx = LowerCtx {
        in_alignment_cell: soften_nested_newlines,
        ..cx
    };

    /// Render the accumulated cell elements flat and trimmed, pushing the result
    /// onto `cells`. Returns `None` on a cell that cannot collapse to one line.
    ///
    /// Collapsible trivia at the cell's edges is dropped first: the structural
    /// newline after `\begin`/each `\\` and the indentation before the next cell
    /// are *boundary* whitespace, not cell content; left in, the leading newline
    /// would lower to a forced break (an [`Ir::hard_line`]) and spuriously trip the
    /// fallback. A lone newline *inside* a cell is a continuation line; it lowers to
    /// a top-level [`Ir::HardLine`], which we collapse to a space so the cell stays
    /// on one aligned row. A blank line (`\par`, an [`Ir::EmptyLine`]) in a cell, or
    /// a forced break nested inside a child block (`\begin{cases}…`), is *not*
    /// collapsed and still (correctly) falls back — it cannot sit on one aligned row.
    fn finish_cell(
        cell: &mut Vec<SyntaxElement>,
        cells: &mut Vec<Cell>,
        printer: &Printer,
        cx: LowerCtx<'_>,
        math: bool,
    ) -> Option<()> {
        let is_edge_trivia = |e: &SyntaxElement| {
            e.as_token()
                .is_some_and(|t| is_collapsible_trivia(t.kind()))
        };
        while cell.first().is_some_and(&is_edge_trivia) {
            cell.remove(0);
        }
        while cell.last().is_some_and(&is_edge_trivia) {
            cell.pop();
        }
        // Read the `\multicolumn` span/alignment before the cell is drained below.
        let (span, align) = detect_multicolumn(cell);
        // A comment in a cell is handled by the caller (passthrough / trailing /
        // fallback) and never reaches here in a handled case; this guard keeps the
        // fallback safe if one ever slips through an unmodeled path.
        if cell.iter().any(|e| {
            e.as_token()
                .is_some_and(|t| t.kind() == SyntaxKind::COMMENT)
        }) {
            return None;
        }
        // A lone interior newline classifies to a top-level `Ir::HardLine`
        // (`classify_trivia`); collapse it to a space so a continuation line joins
        // onto its aligned row. A blank line inside a cell is an `Ir::EmptyLine`
        // (untouched here), and a nested block's breaks live inside a child `Ir`, so
        // both keep tripping `contains_forced_break` below and fall back.
        //
        // A math cell lowers through the role-aware sequencer, which already collapses
        // an interior whitespace/newline run to a single space (so a continuation line
        // joins) and applies operator spacing; a blank line still surfaces as a forced
        // break and (correctly) falls back below, but a nested block environment's
        // break yields a *block* cell instead (see [`Cell::block`]) so the grid
        // survives a `\begin{aligned}…`/`\begin{cases}…`/matrix cell.
        //
        // Block eligibility is read off the elements before they are drained. The
        // anchor is the first *node* child whose own lowering cannot stay flat —
        // a nested block environment, possibly wrapped in `\left…\right` or a
        // group. Only node children can carry a forced break here (comments
        // bailed above, a `\\` never lands inside a cell), so any break in the
        // full cell IR below is that node's structured layout, safe to hang at
        // its column. A verbatim-bodied environment anywhere in the cell's
        // subtree, or a blank line of the cell's own, still falls back.
        let first_block = if math {
            cell.iter().position(|e| {
                e.as_node().is_some()
                    && lower_math_element(e.clone(), cx, MathSpacing::Normal)
                        .contains_forced_break()
            })
        } else {
            None
        };
        let block_eligible = first_block.is_some()
            && !cell.iter().filter_map(|e| e.as_node()).any(|n| {
                n.descendants()
                    .any(|d| d.kind() == SyntaxKind::ENVIRONMENT && has_verbatim_body(&d))
            })
            && !cell_has_blank_line(cell);
        // The hang offset anchors a block cell's continuation lines at the
        // breaking node's start column: the flat width of the cell content before
        // it, plus the one joining space the sequencer places before it (a
        // relation/operator prefix like `= ` always gets one; the tight operand
        // juxtaposition `2\begin{…}` would not, costing one cosmetic column in
        // that unwritten shape). Computed before the drain below.
        let hang = match first_block.filter(|_| block_eligible) {
            None | Some(0) => 0,
            Some(i) => {
                let prefix =
                    lower_math_seq(cell[..i].iter().cloned(), cx, MathSpacing::Normal, false);
                let width = printer.print_flat(&prefix).trim().chars().count();
                if width == 0 { 0 } else { width + 1 }
            }
        };
        let ir = if math {
            lower_math_seq(cell.drain(..), cx, MathSpacing::Normal, false)
        } else {
            let joined = lower_element_stream(cell.drain(..), cx)
                .into_iter()
                .map(|ir| {
                    if matches!(ir, Ir::HardLine) {
                        Ir::line()
                    } else {
                        ir
                    }
                })
                .collect::<Vec<_>>();
            Ir::concat(joined)
        };
        if ir.contains_forced_break() {
            if !block_eligible {
                return None;
            }
            cells.push(Cell {
                text: String::new(),
                span,
                align,
                block: Some(BlockCell { hang, ir }),
            });
            return Some(());
        }
        cells.push(Cell {
            text: printer.print_flat(&ir).trim().to_string(),
            span,
            align,
            block: None,
        });
        Some(())
    }

    /// Inspect a not-yet-drained cell for a lone `\multicolumn{n}{spec}{body}`,
    /// returning its column span and the alignment from its `{spec}` (`(1, None)`
    /// for any ordinary cell). The greedy parser attaches all three `{…}` groups to
    /// the `\multicolumn` `COMMAND`, so the cell is a single command node; a
    /// non-integer span or unparsable spec degrades to span 1 / no override.
    fn detect_multicolumn(cell: &[SyntaxElement]) -> (usize, Option<ColAlign>) {
        let mut content = cell.iter().filter(|e| {
            !e.as_token()
                .is_some_and(|t| is_collapsible_trivia(t.kind()))
        });
        let Some(first) = content.next() else {
            return (1, None);
        };
        if content.next().is_some() {
            return (1, None);
        }
        let Some(node) = first.as_node() else {
            return (1, None);
        };
        if node.kind() != SyntaxKind::COMMAND
            || command_name(node).as_deref() != Some("multicolumn")
        {
            return (1, None);
        }
        let span = tex_ls_parser::ast::nth_group_text(node, 0)
            .and_then(|t| t.trim().parse::<usize>().ok())
            .filter(|&n| n >= 1);
        let align = tex_ls_parser::ast::nth_group(node, 1)
            .map(|g| tex_ls_parser::ast::group_inner_source(&g))
            .and_then(|s| colspec::parse_column_spec(&s))
            .and_then(|v| v.first().copied());
        (span.unwrap_or(1), align)
    }

    let mut items: Vec<GridItem> = Vec::new();
    let mut cells: Vec<Cell> = Vec::new();
    let mut cell: Vec<SyntaxElement> = Vec::new();

    let mut idx = 0;
    while idx < inline.len() {
        // A row boundary: no committed cells and the current cell holds only
        // boundary trivia. Only here can a non-row (passthrough / trailing-comment)
        // line begin.
        let at_boundary = cells.is_empty() && cell_is_blank(&cell);
        if at_boundary
            && is_comment_or_rule_start(&inline[idx], cx)
            && let Some(line) = non_row_line(&inline, idx, &printer, cx)
        {
            // A comment on its own line (a newline separates it from the previous
            // grid token), or any non-row line with no row yet before it, is a
            // passthrough between rows.
            let own_line = cell_has_newline(&cell);
            let prev_is_row = matches!(items.last(), Some(GridItem::Row(_)));
            if own_line || !prev_is_row {
                items.push(GridItem::Passthrough(line.text));
                cell.clear();
                idx = line.next;
                continue;
            }
            // Not on its own line: it directly follows the previous row's `\\`.
            if line.has_rule {
                // The `\\ \hline` form — a rule sharing the physical line with the
                // preceding row's `\\`. Normalize it onto its own passthrough line
                // (idempotent: on re-parse it reads as an own-line rule).
                items.push(GridItem::Passthrough(line.text));
            } else if let Some(GridItem::Row(row)) = items.last_mut() {
                // A pure comment there trails that row.
                row.trailing_comment = Some(line.text);
            }
            cell.clear();
            idx = line.next;
            continue;
        }

        match &inline[idx] {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::AMPERSAND => {
                finish_cell(&mut cell, &mut cells, &printer, cell_cx, math)?;
                // A block cell may only end its row: a `&` after one would need
                // the next cell to align past the block's last line, which the
                // grid cannot lay out — fall back.
                if cells.last().is_some_and(|c| c.block.is_some()) {
                    return None;
                }
            }
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::LINE_BREAK => {
                finish_cell(&mut cell, &mut cells, &printer, cell_cx, math)?;
                let line_break = printer
                    .print_flat(&lower_node(child, cx))
                    .trim()
                    .to_string();
                items.push(GridItem::Row(AlignRow {
                    cells: std::mem::take(&mut cells),
                    line_break: Some(line_break),
                    trailing_comment: None,
                }));
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMENT => {
                // A comment that is *not* at a boundary trails cell content. It is
                // clean only when nothing more of the row can follow: trivia and
                // own-line comments (each a passthrough line, handled by the
                // boundary branch above on later iterations) may remain; anything
                // else would be commented out by joining onto this line — fall
                // back.
                if !rest_is_trivia_and_comment_lines(&inline, idx + 1) {
                    return None;
                }
                let text = token.text().trim_end().to_string();
                finish_cell(&mut cell, &mut cells, &printer, cell_cx, math)?;
                items.push(GridItem::Row(AlignRow {
                    cells: std::mem::take(&mut cells),
                    line_break: None,
                    trailing_comment: Some(text),
                }));
            }
            _ => cell.push(inline[idx].clone()),
        }
        idx += 1;
    }

    // The final segment (content after the last `\\` or trailing comment). Drop it
    // when it is a single empty cell — the "body ended in `\\`" (or in a
    // trailing-comment row) case — so the trailing break stays on the prior row
    // without adding a blank line; otherwise it is a real last row.
    finish_cell(&mut cell, &mut cells, &printer, cell_cx, math)?;
    let final_is_empty = cells.len() == 1
        && cells[0].text.is_empty()
        && cells[0].block.is_none()
        && cells[0].span == 1;
    if !final_is_empty {
        items.push(GridItem::Row(AlignRow {
            cells,
            line_break: None,
            trailing_comment: None,
        }));
    }

    Some(items)
}

/// Try to read a *non-row* line — one made up solely of comments, horizontal-rule
/// commands (`\hline`, `\midrule`, …), and inline whitespace — starting at `start`
/// (which the caller guarantees is a comment or rule command). Returns `None` when
/// the line contains anything else (a cell, a `&`, a `\\`), so the caller treats it
/// as ordinary cell content. The rendered text is the line flattened and trimmed
/// (comments verbatim), exactly as cells and `\\` are rendered.
pub(super) fn non_row_line(
    inline: &[SyntaxElement],
    start: usize,
    printer: &Printer,
    cx: LowerCtx<'_>,
) -> Option<NonRowLine> {
    let mut i = start;
    let mut content_end = start;
    let mut has_rule = false;
    let mut has_comment = false;
    while i < inline.len() {
        match &inline[i] {
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::NEWLINE => break,
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::WHITESPACE => {}
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => {
                // A comment runs to end of line, so it is the line's last content.
                has_comment = true;
                i += 1;
                content_end = i;
                break;
            }
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::COMMAND && is_rule_command(n, cx) => {
                has_rule = true;
                i += 1;
                content_end = i;
                continue;
            }
            // A bare rule control word — the peeled prefix of an over-attaching rule
            // command ([`rule_overattaches_cell`]), whose trailing `{…}` cell now
            // follows as its own element.
            SyntaxElement::Token(t) if token_is_rule_word(t, cx) => {
                has_rule = true;
                i += 1;
                content_end = i;
                continue;
            }
            // The booktabs `\cmidrule(lr){2-3}` paren trim spec. `(lr)` is generic
            // catcode-12 text (a `WORD`) that breaks the greedy argument attach, so
            // it and the following detached `{2-3}` range group arrive as loose
            // siblings after the rule command. Recognizing them as part of the rule
            // line is a layout decision (the rule-line concept is the formatter's).
            SyntaxElement::Token(t) if has_rule && is_paren_trim_word(t) => {
                i += 1;
                content_end = i;
                continue;
            }
            SyntaxElement::Node(n) if has_rule && n.kind() == SyntaxKind::GROUP => {
                i += 1;
                content_end = i;
                continue;
            }
            _ => return None,
        }
        i += 1;
    }
    if !(has_rule || has_comment) {
        return None;
    }
    // Resume past the line's terminating newline (and any trailing whitespace).
    let mut next = content_end;
    while next < inline.len() {
        match &inline[next] {
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::WHITESPACE => next += 1,
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::NEWLINE => {
                next += 1;
                break;
            }
            _ => break,
        }
    }
    let ir = Ir::concat(lower_element_stream(
        inline[start..content_end].iter().cloned(),
        cx,
    ));
    let text = printer.print_flat(&ir).trim().to_string();
    Some(NonRowLine {
        text,
        next,
        has_rule,
    })
}

/// Whether `element` begins a candidate non-row line — a comment, or a command the
/// signature DB flags as a horizontal rule (`\hline`, `\midrule`, …).
pub(super) fn is_comment_or_rule_start(element: &SyntaxElement, cx: LowerCtx<'_>) -> bool {
    match element {
        SyntaxElement::Token(t) => t.kind() == SyntaxKind::COMMENT || token_is_rule_word(t, cx),
        SyntaxElement::Node(n) => n.kind() == SyntaxKind::COMMAND && is_rule_command(n, cx),
    }
}

/// Whether `token` is a bare control word naming a horizontal-rule command per the
/// signature DB — the peeled prefix of an over-attaching rule command (see
/// [`rule_overattaches_cell`]), recognized as a rule the same way an intact rule
/// `COMMAND` node is ([`is_rule_command`]).
pub(super) fn token_is_rule_word(token: &SyntaxToken, cx: LowerCtx<'_>) -> bool {
    token.kind() == SyntaxKind::CONTROL_WORD
        && cx
            .signatures
            .command(token.text().trim_start_matches('\\'))
            .is_some_and(|sig| sig.rule)
}

/// Whether `token` is a booktabs `\cmidrule` paren trim spec — a `WORD` of the form
/// `(l)`, `(r)`, `(lr)`, or `(rl)` (catcode-12 text the lexer globs into one token).
/// `pub` because the linter's rule-span gate (`in_rule_span_argument`, in the
/// `tex-ls` crate) recognizes the same shape; single-sourced so the two never
/// drift.
pub fn is_paren_trim_word(token: &SyntaxToken) -> bool {
    if token.kind() != SyntaxKind::WORD {
        return false;
    }
    let t = token.text();
    t.len() >= 3
        && t.starts_with('(')
        && t.ends_with(')')
        && t[1..t.len() - 1].chars().all(|c| c == 'l' || c == 'r')
}

/// Whether `node` (a `COMMAND`) is a horizontal-rule command per the signature DB.
pub(super) fn is_rule_command(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    command_name(node)
        .and_then(|name| cx.signatures.command(&name))
        .is_some_and(|sig| sig.rule)
}

/// Whether the accumulated cell holds only collapsible trivia (no real content) —
/// i.e. the parser is at a grid boundary.
pub(super) fn cell_is_blank(cell: &[SyntaxElement]) -> bool {
    cell.iter().all(|e| {
        e.as_token()
            .is_some_and(|t| is_collapsible_trivia(t.kind()))
    })
}

/// Whether the cell's own top-level trivia contains a blank line (two newlines
/// with only inline whitespace between them, the `\par` boundary). A blank line
/// inside a *child* node (a nested environment's body) is that node's own
/// business and is not seen here.
pub(super) fn cell_has_blank_line(cell: &[SyntaxElement]) -> bool {
    let mut run_newlines = 0;
    for e in cell {
        match e.as_token().map(SyntaxToken::kind) {
            Some(SyntaxKind::NEWLINE) => {
                run_newlines += 1;
                if run_newlines >= 2 {
                    return true;
                }
            }
            Some(SyntaxKind::WHITESPACE) => {}
            _ => run_newlines = 0,
        }
    }
    false
}

/// Whether the boundary trivia accumulated since the last grid token includes a
/// newline — i.e. a following comment sits on its *own* physical line rather than
/// trailing the previous row's `\\`.
pub(super) fn cell_has_newline(cell: &[SyntaxElement]) -> bool {
    cell.iter().any(|e| {
        e.as_token()
            .is_some_and(|t| t.kind() == SyntaxKind::NEWLINE)
    })
}

/// Whether everything from `from` onward is collapsible trivia or `%` comments
/// that each start their own physical line — nothing more of the *row* remains,
/// so a comment at the current position is a clean trailing comment, and any
/// following comment-only lines render as passthrough lines. A comment that does
/// not sit on its own line, or any non-trivia element, disqualifies the rest
/// (joining it onto the row's line would comment it out).
pub(super) fn rest_is_trivia_and_comment_lines(inline: &[SyntaxElement], from: usize) -> bool {
    let mut own_line = false;
    for element in &inline[from..] {
        let Some(token) = element.as_token() else {
            return false;
        };
        match token.kind() {
            SyntaxKind::NEWLINE => own_line = true,
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::COMMENT if own_line => own_line = false,
            _ => return false,
        }
    }
    true
}

/// Flatten an alignment environment's body into a single stream of inline
/// elements, descending one level into the lone body wrapper node (where the `&`
/// and `\\` separators live) — a `PARAGRAPH` for a prose grid (`tabular`), or a
/// `MATH` node for a `math` grid (`align`/matrix, parsed in math mode). Trivia
/// outside the wrapper is dropped (it is just the body's own leading/trailing break,
/// which the indenter re-supplies).
///
/// Returns `None` when the body holds more than one wrapper node — a blank-line
/// break, which the single grid does not model — so the caller falls back.
///
/// A rule command that the greedy parser saddled with the next line's first cell
/// as a bogus `{…}` argument ([`rule_overattaches_cell`]) is expanded into its own
/// children, so the rule lands on its own passthrough line and the `{…}` is handed
/// back to the grid as cell content.
pub(super) fn flatten_alignment_body(
    body_elements: &[SyntaxElement],
    cx: LowerCtx<'_>,
    math: bool,
) -> Option<Vec<SyntaxElement>> {
    let wrapper = if math {
        SyntaxKind::MATH
    } else {
        SyntaxKind::PARAGRAPH
    };
    let mut inline: Vec<SyntaxElement> = Vec::new();
    let mut paragraphs = 0;
    for element in strip_virtual_dtx_framing(body_elements.iter().cloned(), cx) {
        match element {
            SyntaxElement::Node(child) if child.kind() == wrapper => {
                paragraphs += 1;
                if paragraphs > 1 {
                    return None;
                }
                extend_alignment_elements(&mut inline, child.children_with_tokens(), cx);
            }
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {}
            other => push_alignment_element(&mut inline, other, cx),
        }
    }
    Some(inline)
}

/// Extend a flattened grid stream through the virtual-document framing adapter.
/// The adapter is applied at every level the grid flattener descends, so a source
/// margin and its padding cannot become cell content merely because the parser
/// attached that physical line inside a wrapper or an over-attaching rule node.
pub(super) fn extend_alignment_elements(
    inline: &mut Vec<SyntaxElement>,
    elements: impl IntoIterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
) {
    for element in strip_virtual_dtx_framing(elements, cx) {
        push_alignment_element(inline, element, cx);
    }
}

/// Push one flattened-body element, expanding an over-attaching rule command
/// ([`rule_overattaches_cell`]) into its children so its trailing `{…}` cell is
/// exposed to the grid rather than glued to the rule.
pub(super) fn push_alignment_element(
    inline: &mut Vec<SyntaxElement>,
    element: SyntaxElement,
    cx: LowerCtx<'_>,
) {
    if let SyntaxElement::Node(node) = &element
        && rule_overattaches_cell(node, cx)
    {
        extend_alignment_elements(inline, node.children_with_tokens(), cx);
    } else {
        inline.push(element);
    }
}

/// Whether `node` is a horizontal-rule `COMMAND` (`\midrule`, `\toprule`, …) onto
/// which the greedy parser attached the next line's first
/// cell as a spurious `{…}` argument. Booktabs rules take at most an optional
/// `[width]`, never a mandatory brace argument, so a leading `{…}` is never a real
/// argument — it is cell content the arity refinement peels back off.
///
/// Restricted to a *leading* `{…}` (no real argument consumed first): the rare
/// `\toprule[2pt]{…}` shape keeps the generic fallback rather than being split.
pub(super) fn rule_overattaches_cell(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    if node.kind() != SyntaxKind::COMMAND || !is_rule_command(node, cx) {
        return false;
    }
    let Some(first_arg) = node
        .children()
        .find(|child| matches!(child.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL))
    else {
        return false;
    };
    if first_arg.kind() != SyntaxKind::GROUP {
        return false;
    }
    // A leading `{…}` is a real argument only when the signature's first slot is a
    // mandatory brace argument (`\cline{2-3}`, `\specialrule{…}`).
    let first_slot_is_brace = command_name(node)
        .and_then(|name| cx.signatures.command(&name))
        .and_then(|sig| sig.args.first())
        .is_some_and(|arg| arg.kind == ArgKind::Brace);
    !first_slot_is_brace
}

/// The per-column alignments declared by a `tabular`/`array` environment's column
/// specification (`\begin{tabular}{lcr}` → `[Left, Center, Right]`), or `None` to
/// signal the caller to fall back to all-left.
///
/// Only environments the signature DB marks `align` carry a user column spec; a math
/// grid's `\begin` has no argument `GROUP` at all (the environment name is a separate
/// `NAME_GROUP`), so those return `None` and fall through. The spec is the *last*
/// `{…}` `GROUP` on the `\begin` — uniform across `tabular`'s `{spec}`, `array`'s
/// `{spec}`, and `tabular*`'s `{width}[pos]{spec}` (the `[pos]` is an `OPTIONAL`, not
/// a `GROUP`). The raw inner source is read via [`Group::inner_source`] rather than
/// `nth_group_text`, which would bail on the nested `{…}` of a `p{3cm}` column.
pub(super) fn column_alignments(env: &SyntaxNode, cx: LowerCtx<'_>) -> Option<Vec<ColAlign>> {
    let begin = Environment::cast(env.clone())?.begin()?;
    if !cx
        .signatures
        .environment_at(begin.syntax())
        .is_some_and(|sig| sig.align)
    {
        return None;
    }
    let spec = tex_ls_parser::ast::children::<Group>(begin.syntax()).last()?;
    colspec::parse_column_spec(&spec.inner_source())
}

/// Render the grid to IR: align each cell within its column to the column's declared
/// alignment (`aligns`, empty = all-left), join cells with `" & "`, append the row's
/// `\\` and any trailing comment, and join all items with [`Ir::hard_line`]. A row is
/// one [`Ir::text`] (no newline; cells are flat), which the caller indents one step.
/// Rows terminated by `\\` pad to the full grid width, aligning their terminators;
/// unterminated rows omit that final padding so they never carry trailing whitespace.
/// [`GridItem::Passthrough`] lines (comments, `\hline`/`\midrule`, …) are emitted
/// verbatim between rows and never counted toward column widths.
///
/// A `\multicolumn{n}{…}{…}` cell spans `n` columns: it never defines a single
/// column's width, and its rendered field is the sum of the spanned column widths
/// plus the `" & "` separators it absorbs. The spanned columns keep their
/// data-derived widths (the `\multicolumn`'s *source* markup is usually wider than a
/// few narrow columns; growing them to fit would balloon the data rows), so when the
/// markup exceeds its span it simply overflows that one row — matching how such a row
/// is written by hand. When the span is instead *wider* than the markup, the cell is
/// aligned within it per the `\multicolumn`'s own `{spec}`.
pub(super) fn render_alignment_rows(items: &[GridItem], aligns: &[ColAlign]) -> Ir {
    const SEP: &str = " & ";

    // Column width = the max char-count over every *span-1* cell in that column
    // (including last cells, so a long final cell still widens the column above it).
    // Char count matches the printer's own column metric. Spanning cells and
    // passthrough lines do not participate here.
    let mut col_widths: Vec<usize> = Vec::new();
    let mut widen = |c: usize, width: usize| {
        while c >= col_widths.len() {
            col_widths.push(0);
        }
        if width > col_widths[c] {
            col_widths[c] = width;
        }
    };
    for item in items {
        let GridItem::Row(row) = item else { continue };
        let mut c = 0;
        for cell in &row.cells {
            if cell.span == 1 && cell.block.is_none() {
                widen(c, cell.text.chars().count());
            }
            c += cell.span;
        }
    }

    // The combined field width of the `span` columns starting at `c`: their widths
    // plus the `" & "` separators the spanning cell absorbs.
    let field_width = |c: usize, span: usize| -> usize {
        let sum: usize = (c..c + span)
            .map(|i| col_widths.get(i).copied().unwrap_or(0))
            .sum();
        sum + SEP.len() * (span - 1)
    };
    let grid_width =
        col_widths.iter().sum::<usize>() + SEP.len() * col_widths.len().saturating_sub(1);
    let grid_width = grid_width.saturating_sub(usize::from(
        col_widths.len() > 1 && col_widths.first() == Some(&0),
    ));

    let lines = items.iter().map(|item| {
        let row = match item {
            GridItem::Passthrough(text) => {
                // A passthrough spans physical lines when a comment-only line
                // binds into a rule command as its `DOC_COMMENT` (issue #49):
                // flat-printing keeps the doc comment's break as a raw newline,
                // which a single `Ir::text` would emit without re-applying the
                // grid indent. Split so every physical line is indented.
                return Ir::join(
                    Ir::hard_line(),
                    text.lines().map(|line| Ir::text(line.trim().to_string())),
                );
            }
            GridItem::Row(row) => row,
        };
        let mut line = String::new();
        // A block cell (always the row's last, see [`Cell::block`]): its later
        // lines hang at the nested environment's start column, so its IR goes
        // inside an [`Ir::align`] whose width is the flat prefix already on the
        // line plus the cell's own hang offset. It takes no padding and no
        // width — like a spanning cell, it overflows.
        let mut block: Option<Ir> = None;
        let last = row.cells.len().saturating_sub(1);
        let mut c = 0;
        for (idx, cell) in row.cells.iter().enumerate() {
            if idx > 0 {
                // A row never opens with the separator's leading space: when
                // everything before this `&` is empty (an `aligned`/`split` body
                // whose rows all start at `&`, so the leading column's width is
                // 0), there is nothing to separate and the `&` is the line's
                // first character. A *padded* empty cell (its column is nonzero
                // elsewhere) has already pushed its pad, keeping the `&` aligned.
                line.push_str(if line.is_empty() { "& " } else { SEP });
            }
            if let Some(block_cell) = &cell.block {
                block = Some(Ir::align(
                    line.chars().count() + block_cell.hang,
                    block_cell.ir.clone(),
                ));
                break;
            }
            let field = field_width(c, cell.span);
            let text_width = cell.text.chars().count();
            let pad = field.saturating_sub(text_width);
            // A `\multicolumn`'s own `{spec}` overrides the column alignment.
            let align = cell
                .align
                .unwrap_or_else(|| aligns.get(c).copied().unwrap_or(ColAlign::Left));
            let (leading, trailing) = match align {
                ColAlign::Left => (0, pad),
                ColAlign::Right => (pad, 0),
                ColAlign::Center => (pad / 2, pad - pad / 2),
            };
            // The last cell never carries trailing whitespace (leading pad is fine).
            let trailing = if idx == last { 0 } else { trailing };
            line.push_str(&" ".repeat(leading));
            line.push_str(&cell.text);
            line.push_str(&" ".repeat(trailing));
            c += cell.span;
        }
        let mut tail = String::new();
        if let Some(line_break) = &row.line_break {
            if block.is_none() {
                line.push_str(&" ".repeat(grid_width.saturating_sub(line.chars().count())));
            }
            tail.push(' ');
            tail.push_str(line_break);
        }
        // The trailing comment always follows the `\\` so the break is never
        // commented out.
        if let Some(comment) = &row.trailing_comment {
            tail.push(' ');
            tail.push_str(comment);
        }
        match block {
            Some(block) => Ir::concat([Ir::text(line), block, Ir::text(tail)]),
            None => {
                line.push_str(&tail);
                Ir::text(line)
            }
        }
    });
    Ir::join(Ir::hard_line(), lines)
}
