use super::*;

/// Lower inline `$…$`/`\(…\)` or display `$$…$$`/`\[…\]` math. The delimiter
/// tokens are direct children of the math node and are emitted verbatim; the
/// `MATH` child (the body) is formatted by [`lower_math_body`].
pub(super) fn lower_math(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    Ir::concat(
        strip_virtual_dtx_framing(node.children_with_tokens(), cx)
            .into_iter()
            .map(|el| match el {
                SyntaxElement::Node(n) if n.kind() == SyntaxKind::MATH => lower_math_body(&n, cx),
                SyntaxElement::Node(n) => lower_node(&n, cx),
                SyntaxElement::Token(t) => Ir::verbatim(t.text()),
            }),
    )
}

/// Lower display math (`$$…$$` or `\[…\]`) as a block: the delimiters land on
/// their own lines with the body collapsed by [`lower_math_body`] and indented one
/// level, mirroring [`lower_bracketed`]'s shape. Display math is conceptually its
/// own vertical space, so unlike inline math (`\[ F \]` → `\[F\]`) it never
/// collapses onto a single line. An empty body degenerates to the bare adjacent
/// delimiters (`\[\]`, `$$$$`).
pub(super) fn lower_display_math(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    // Delimiters are one token for `\[`/`\]` but two `DOLLAR` tokens for `$$`, so
    // accumulate every delimiter token on each side of the `MATH` body.
    let mut open = String::new();
    let mut close = String::new();
    let mut body = Ir::Nil;
    let mut body_empty = true;
    let mut seen_body = false;
    let mut open_has_comment = false;
    for element in strip_virtual_dtx_framing(node.children_with_tokens(), cx) {
        match element {
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::MATH => {
                // A `%` trailing the opening delimiter on the same source line
                // rides that line, exactly as an environment's `\begin`-line
                // comment does ([`split_environment`]), and is dropped from the
                // body by identity.
                let lifted = leading_inline_comment(&[SyntaxElement::Node(n.clone())]);
                if let Some(comment) = &lifted {
                    open.push_str(comment.text());
                    open_has_comment = true;
                }
                let elements: Vec<SyntaxElement> = n
                    .children_with_tokens()
                    .filter(|e| !is_lifted_comment(e, lifted.as_ref()))
                    .collect();
                let body_cx = cx.absorbing_trailing_control_newline(&elements);
                body_empty = elements.iter().all(|e| {
                    e.as_token()
                        .is_some_and(|t| is_collapsible_trivia(t.kind()))
                });
                body = trim_trailing_break(lower_display_formula_elements(&elements, body_cx));
                seen_body = true;
            }
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {}
            SyntaxElement::Token(t) if seen_body => close.push_str(t.text()),
            SyntaxElement::Token(t) => open.push_str(t.text()),
            // Unexpected non-MATH node child: defer to generic lowering.
            SyntaxElement::Node(n) => {
                body = lower_node(&n, cx);
                body_empty = false;
                seen_body = true;
            }
        }
    }

    if body_empty {
        if open_has_comment {
            // The lifted comment runs to end of line, so the closing delimiter
            // must not collapse onto it (it would be commented out).
            Ir::concat([Ir::verbatim(open), Ir::hard_line(), Ir::verbatim(close)])
        } else {
            Ir::concat([Ir::verbatim(open), Ir::verbatim(close)])
        }
    } else {
        Ir::concat([
            Ir::verbatim(open),
            Ir::indent(Ir::concat([Ir::hard_line(), body])),
            Ir::hard_line(),
            Ir::verbatim(close),
        ])
    }
}

/// Format a math body (a `MATH` node, or a `{…}` group body in math): collapse
/// internal `WHITESPACE`/`NEWLINE` runs to a single space, drop the runs at the
/// edges (trimming just inside the delimiters), keep `^`/`_` scripts tight, and
/// let a `%` comment force a line break (so a trailing comment never swallows the
/// closing delimiter).
pub(super) fn lower_math_body(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    lower_math_seq(node.children_with_tokens(), cx, MathSpacing::Normal, false)
}

/// Lower a single-formula display-math body per the resolved [`MathWrap`]
/// policy (`LowerCtx::math_wrap`): `Break` routes through the amsmath-style
/// breaker, `SingleLine` through the plain collapsing body (overflowing if too
/// long, like inline math), and `Preserve` keeps authored newlines as hard
/// breaks. `Auto` is resolved away in [`format_root`] and cannot reach here;
/// map it to the breaker defensively rather than panic. Takes the `MATH` node's
/// elements rather than the node itself so a caller can drop the lifted
/// opener-line comment ([`split_environment`]) before the lowering.
pub(super) fn lower_display_formula_elements(elements: &[SyntaxElement], cx: LowerCtx<'_>) -> Ir {
    // A leading `\label{…}` is equation bookkeeping, not part of the formula: give
    // it its own line so the math starts fresh below it, under every wrap policy.
    // Grids (`align`, `\\`) never reach here. The split recurses so the remaining
    // formula lowers under its normal `MathWrap` policy (and a `\label\label` run
    // peels one label per level).
    if let Some((label, rest)) = split_leading_label(elements) {
        return Ir::concat([
            lower_math_element(label, cx, MathSpacing::Normal),
            Ir::hard_line(),
            lower_display_formula_elements(rest, cx),
        ]);
    }
    match cx.math_wrap {
        MathWrap::Auto | MathWrap::Break => lower_display_math_body(elements, cx),
        MathWrap::SingleLine => {
            lower_math_seq(elements.iter().cloned(), cx, MathSpacing::Normal, false)
        }
        MathWrap::Preserve => {
            lower_math_seq(elements.iter().cloned(), cx, MathSpacing::Normal, true)
        }
    }
}

/// Split a display-math body whose first non-trivia atom is a `\label{…}` into
/// that label and the remaining formula elements. Returns `None` when the body
/// does not lead with a label, or when nothing but trivia follows it (a body that
/// is only a label stays on one line rather than gaining a dangling break). Scoped
/// to the single `\label` command by name — a trailing label, or any other
/// bookkeeping command, is deliberately left in place (see the formatter book).
pub(super) fn split_leading_label(
    elements: &[SyntaxElement],
) -> Option<(SyntaxElement, &[SyntaxElement])> {
    let idx = elements
        .iter()
        .position(|e| !is_collapsible_trivia_element(e))?;
    let node = elements[idx].as_node()?;
    if node.kind() != SyntaxKind::COMMAND
        || tex_ls_parser::ast::command_name(node).as_deref() != Some("label")
    {
        return None;
    }
    let rest = &elements[idx + 1..];
    if rest.iter().all(is_collapsible_trivia_element) {
        return None;
    }
    Some((elements[idx].clone(), rest))
}

pub(super) fn math_break_kind(element: &SyntaxElement) -> MathBreakKind {
    let SyntaxElement::Node(node) = element else {
        return match element.as_token().map(|token| token.text()) {
            Some("+" | "-") => MathBreakKind::Additive,
            _ => MathBreakKind::Class,
        };
    };
    if node.kind() == SyntaxKind::SCRIPTED {
        return node
            .children_with_tokens()
            .find(|element| {
                !matches!(
                    element.kind(),
                    SyntaxKind::WHITESPACE
                        | SyntaxKind::NEWLINE
                        | SyntaxKind::SUBSCRIPT
                        | SyntaxKind::SUPERSCRIPT
                )
            })
            .as_ref()
            .map_or(MathBreakKind::Class, math_break_kind);
    }
    if node.kind() != SyntaxKind::COMMAND {
        return MathBreakKind::Class;
    }
    match tex_ls_parser::ast::command_name(node).as_deref() {
        Some("pm" | "mp") => MathBreakKind::Additive,
        Some("cdot") => MathBreakKind::Multiplicative,
        Some("mid") => MathBreakKind::Conditional,
        _ => MathBreakKind::Class,
    }
}

/// Lower one CST element into the semantic atoms that its source surface
/// contains. Structural nodes stay indivisible; a coalesced `WORD` is sliced at
/// Unicode scalar boundaries. Consecutive relation scalars and a colon run
/// followed by `=` remain one surface atom, so authored compound spellings such
/// as `<=`, `:=`, and `::=` are not separated.
pub(super) fn lower_math_atoms(
    el: SyntaxElement,
    cx: LowerCtx<'_>,
    spacing: MathSpacing,
) -> Vec<MathSurfaceAtom> {
    let atoms: Vec<_> = math_atoms(&el).collect();
    let break_kind = math_break_kind(&el);
    let SyntaxElement::Token(token) = &el else {
        let starts_control_word_letter = element_starts_control_word_letter(&el);
        let ends_control_word = element_ends_control_word(&el);
        let starts_with_equals = element_starts_with_token_text(&el, "=");
        let atom = atoms
            .into_iter()
            .next()
            .expect("a structural math element has one semantic atom");
        let control_word_operator =
            ends_control_word && matches!(atom.class, MathClass::Bin | MathClass::Rel);
        return vec![MathSurfaceAtom {
            ir: lower_math_element(el, cx, spacing),
            class: atom.class,
            break_kind,
            delimiter: atom.delimiter,
            colon_relation_prefix: false,
            starts_equals_relation: atom.class == MathClass::Rel && starts_with_equals,
            spaced_slash: false,
            slash: false,
            control_word_operator,
            starts_control_word_letter,
            ends_control_word,
            postfix_left_limit: false,
        }];
    };
    if token.kind() != SyntaxKind::WORD {
        let starts_control_word_letter = token
            .text()
            .chars()
            .next()
            .is_some_and(is_control_word_letter);
        let ends_control_word = token.kind() == SyntaxKind::CONTROL_WORD;
        let atom = atoms
            .into_iter()
            .next()
            .expect("a math token has one semantic atom");
        let control_word_operator =
            ends_control_word && matches!(atom.class, MathClass::Bin | MathClass::Rel);
        return vec![MathSurfaceAtom {
            ir: lower_math_element(el, cx, spacing),
            class: atom.class,
            break_kind,
            delimiter: atom.delimiter,
            colon_relation_prefix: false,
            starts_equals_relation: false,
            spaced_slash: false,
            slash: false,
            control_word_operator,
            starts_control_word_letter,
            ends_control_word,
            postfix_left_limit: false,
        }];
    }

    let token_start = token.text_range().start();
    let mut surface: Vec<MathSurfaceAtom> = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let start = usize::from(atom.range.start() - token_start);
        let end = usize::from(atom.range.end() - token_start);
        let text = &token.text()[start..end];
        if let Some(previous) = surface.last_mut() {
            let extends_colon_prefix =
                previous.colon_relation_prefix && atom.class == MathClass::Punct && text == ":";
            let completes_colon_relation =
                previous.colon_relation_prefix && atom.class == MathClass::Rel && text == "=";
            if previous.class == MathClass::Rel && atom.class == MathClass::Rel
                || extends_colon_prefix
                || completes_colon_relation
            {
                previous.ir = Ir::concat([previous.ir.clone(), Ir::verbatim(text)]);
                if completes_colon_relation {
                    previous.class = MathClass::Rel;
                    previous.colon_relation_prefix = false;
                }
                continue;
            }
        }
        surface.push(MathSurfaceAtom {
            ir: Ir::verbatim(text),
            class: atom.class,
            break_kind: match text {
                "+" | "-" => MathBreakKind::Additive,
                _ => MathBreakKind::Class,
            },
            delimiter: atom.delimiter,
            colon_relation_prefix: atom.class == MathClass::Punct && text == ":",
            starts_equals_relation: false,
            spaced_slash: atom.class == MathClass::Ord
                && text == "/"
                && (start == 0
                    && token
                        .prev_token()
                        .is_some_and(|token| is_collapsible_trivia(token.kind()))
                    || end == token.text().len()
                        && token
                            .next_token()
                            .is_some_and(|token| is_collapsible_trivia(token.kind()))),
            slash: atom.class == MathClass::Ord && text == "/",
            control_word_operator: false,
            starts_control_word_letter: text.chars().next().is_some_and(is_control_word_letter),
            ends_control_word: false,
            postfix_left_limit: text == "-",
        });
    }
    let following_token_closes = || {
        let mut next = token.next_token();
        while next
            .as_ref()
            .is_some_and(|token| is_collapsible_trivia(token.kind()))
        {
            next = next.and_then(|token| token.next_token());
        }
        next.and_then(|token| math_atoms(&SyntaxElement::Token(token)).next())
            .is_some_and(|atom| atom.delimiter == Some(DelimiterRole::Close))
    };
    for index in 0..surface.len() {
        let followed_by_closer = surface
            .get(index + 1)
            .is_some_and(|next| next.delimiter == Some(DelimiterRole::Close))
            || index + 1 == surface.len() && following_token_closes();
        surface[index].postfix_left_limit &= followed_by_closer;
    }
    // Anticipate the gap the sequencer will add before a following operator;
    // otherwise that gap makes only the next pass recognize the slash as spaced.
    for index in 0..surface.len() {
        if !surface[index].slash || surface[index].spaced_slash {
            continue;
        }
        let next_operator = surface.get(index + 1).is_some_and(|next| {
            spacing == MathSpacing::Normal && matches!(next.class, MathClass::Bin | MathClass::Rel)
        }) || index + 1 == surface.len()
            && token.next_token().is_some_and(|next| {
                let class = math_atoms(&SyntaxElement::Token(next.clone()))
                    .next()
                    .map(|atom| atom.class);
                class.is_some_and(|class| {
                    matches!(class, MathClass::Bin | MathClass::Rel)
                        && (spacing == MathSpacing::Normal
                            || next.kind() == SyntaxKind::CONTROL_WORD)
                })
            });
        surface[index].spaced_slash = next_operator;
    }
    surface
}

pub(super) fn is_control_word_letter(character: char) -> bool {
    character.is_alphabetic()
}

pub(super) fn element_starts_control_word_letter(element: &SyntaxElement) -> bool {
    let first = match element {
        SyntaxElement::Node(node) => node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| !is_collapsible_trivia(token.kind())),
        SyntaxElement::Token(token) => Some(token.clone()),
    };
    first
        .as_ref()
        .and_then(|token| token.text().chars().next())
        .is_some_and(is_control_word_letter)
}

pub(super) fn element_starts_with_token_text(element: &SyntaxElement, expected: &str) -> bool {
    match element {
        SyntaxElement::Node(node) => node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| !is_collapsible_trivia(token.kind())),
        SyntaxElement::Token(token) => Some(token.clone()),
    }
    .is_some_and(|token| token.text() == expected)
}

pub(super) fn element_ends_control_word(element: &SyntaxElement) -> bool {
    let last = match element {
        SyntaxElement::Node(node) => node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| !is_collapsible_trivia(token.kind()))
            .last(),
        SyntaxElement::Token(token) => Some(token.clone()),
    };
    last.is_some_and(|token| token.kind() == SyntaxKind::CONTROL_WORD)
}

/// The [`MathRole`] of a top-level math atom. `prev_class` is the TeX class of the
/// preceding atom and `prev_opener` whether it ended with an opening delimiter: a
/// `+`/`-` (or any binary operator) with no operand to its left — either the first
/// atom, one after a binary, large operator, relation, opener, or punctuation —
/// is unary, so it glues to its operand and is *not* a break point, degrading to
/// an [`MathRole::Operand`]. A structurally proven postfix left-limit sign gets
/// its own tight role. The full class is needed here because [`MathRole`]
/// deliberately collapses operators and punctuation into `Operand`.
pub(super) fn math_atom_role(
    class: MathClass,
    prev_class: MathClass,
    prev_opener: bool,
    postfix_left_limit: bool,
) -> MathRole {
    if postfix_left_limit {
        return MathRole::PostfixLeftLimit;
    }
    let raw = match class {
        MathClass::Bin => MathRole::Binary,
        MathClass::Rel => MathRole::Relation,
        _ => MathRole::Operand,
    };
    if raw == MathRole::Binary
        && (matches!(
            prev_class,
            MathClass::Op | MathClass::Bin | MathClass::Rel | MathClass::Punct
        ) || prev_opener)
    {
        MathRole::Operand
    } else {
        raw
    }
}

/// Collect the top-level atoms of a display-math `MATH` body as [`MathPiece`]s,
/// collapsing trivia runs exactly as [`lower_math_seq`] does. Returns `None` —
/// signalling the caller to take the plain non-breaking path — when the body
/// holds a comment or explicit line break (either forces its own break, which
/// does not compose with the operator-break layout), or has fewer than two atoms
/// (nothing to break).
pub(super) fn collect_math_pieces(
    elements: &[SyntaxElement],
    cx: LowerCtx<'_>,
) -> Option<Vec<MathPiece>> {
    let mut pieces: Vec<MathPiece> = Vec::new();
    // Start as a non-operand so a leading `+`/`-` (no left operand) reads as unary
    // and glues to its operand rather than becoming a break point — e.g. `-x`.
    let mut prev_class = MathClass::Rel;
    let mut prev_opener = false;
    let mut bracket_depth = 0_i32;
    let mut conditional_at_top_level = false;
    let mut pending_space = false;
    let mut iter = strip_virtual_dtx_framing(elements.iter().cloned(), cx)
        .into_iter()
        .peekable();
    while let Some(el) = iter.next() {
        match el {
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                consume_gap(&t, &mut iter);
                if !pieces.is_empty() {
                    pending_space = true;
                }
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => return None,
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::LINE_BREAK => return None,
            other => {
                for atom in lower_math_atoms(other, cx, MathSpacing::Normal) {
                    let role = math_atom_role(
                        atom.class,
                        prev_class,
                        prev_opener,
                        atom.postfix_left_limit,
                    );
                    let completes_colon_relation = !pending_space
                        && pieces
                            .last()
                            .is_some_and(|piece| piece.colon_relation_prefix)
                        && atom.starts_equals_relation;
                    if completes_colon_relation {
                        let previous = pieces.last_mut().expect("checked above");
                        previous.ir = Ir::concat([previous.ir.clone(), atom.ir]);
                        previous.role = MathRole::Relation;
                        previous.colon_relation_prefix = false;
                        previous.bracket_delta += match atom.delimiter {
                            Some(DelimiterRole::Open) => 1,
                            Some(DelimiterRole::Close) => -1,
                            Some(DelimiterRole::Fence) | None => 0,
                        };
                        prev_class = MathClass::Rel;
                        prev_opener = atom.delimiter == Some(DelimiterRole::Open);
                        continue;
                    }
                    prev_class = atom.class;
                    prev_opener = atom.delimiter == Some(DelimiterRole::Open);
                    let bracket_delta = match atom.delimiter {
                        Some(DelimiterRole::Open) => 1,
                        Some(DelimiterRole::Close) => -1,
                        Some(DelimiterRole::Fence) | None => 0,
                    };
                    pieces.push(MathPiece {
                        ir: atom.ir,
                        role,
                        break_before: !matches!(
                            atom.break_kind,
                            MathBreakKind::Multiplicative | MathBreakKind::Conditional
                        ) && !(role == MathRole::Relation
                            && conditional_at_top_level),
                        colon_relation_prefix: atom.colon_relation_prefix,
                        spaced_slash: atom.spaced_slash,
                        slash: atom.slash,
                        space_before: pending_space,
                        bracket_delta,
                    });
                    if atom.break_kind == MathBreakKind::Conditional && bracket_depth == 0 {
                        conditional_at_top_level = true;
                    }
                    bracket_depth += bracket_delta;
                    pending_space = false;
                }
            }
        }
    }
    // The display breaker computes every effective role up front, so it can make
    // operator-created slash gaps symmetric before building either layout.
    for index in 0..pieces.len() {
        if pieces[index].slash
            && (index > 0 && !pieces[index - 1].role.is_operand_like()
                || pieces
                    .get(index + 1)
                    .is_some_and(|next| !next.role.is_operand_like()))
        {
            pieces[index].spaced_slash = true;
        }
    }
    (pieces.len() >= 2).then_some(pieces)
}

/// Lower a display-math `MATH` body, additionally letting a too-long body *break*
/// before its eligible top-level binary/relation operators (amsmath style). The
/// layout is two-level: equation-chain *relations* align in a single column (a
/// chain of `=` reads as a stack, the second `=` under the first), and a breakable
/// *binary* operator hangs one relation-width deeper, under the first term of its
/// right-hand side (a `+`-chain tucks under the first summand). Multiplicative and
/// conditional operators use the narrower policy in [`math_break_kind`]. The
/// left-hand side and the first relation stay flat on the opening line. The whole
/// body is one [`Ir::group`], so it stays on a single line whenever it fits —
/// degrading to [`lower_math_body`] otherwise. Each segment's right-hand side is
/// its own nested group: breaking the body at its relations does not also break a
/// segment at its binary operators unless that segment overflows its own line. If
/// the LHS-derived relation column would make a continuation overflow, the
/// printer breaks before the first relation and hangs the relation stack at the
/// display body's base indent instead.
pub(super) fn lower_display_math_body(elements: &[SyntaxElement], cx: LowerCtx<'_>) -> Ir {
    let Some(pieces) = collect_math_pieces(elements, cx) else {
        return lower_math_seq(elements.iter().cloned(), cx, MathSpacing::Normal, false);
    };

    let flat_width = |ir: &Ir| {
        Printer::new(FormatStyle::default())
            .print_flat(ir)
            .chars()
            .count()
    };

    // Bracket depth entering each atom (running sum of the preceding atoms'
    // deltas). A top-level break/relation is one seen at depth 0; operators
    // inside a parenthesized subexpression are structurally interior.
    let depth_before: Vec<i32> = {
        let mut acc = 0;
        let mut v = Vec::with_capacity(pieces.len());
        for p in &pieces {
            v.push(acc);
            acc += p.bracket_delta;
        }
        v
    };

    // Non-breaking separator between atoms `k-1` and `k`, mirroring
    // [`lower_math_seq`]: a space around any operator (either side) or across an
    // authored gap, nothing between two operands authored tight.
    let space_sep = |k: usize| -> Ir {
        if pieces[k].spaced_slash || pieces[k - 1].spaced_slash {
            Ir::verbatim(" ")
        } else if pieces[k].role == MathRole::PostfixLeftLimit
            || pieces[k - 1].role == MathRole::PostfixLeftLimit
        {
            Ir::Nil
        } else if pieces[k].role != MathRole::Operand
            || pieces[k - 1].role != MathRole::Operand
            || pieces[k].space_before
        {
            Ir::text(" ")
        } else {
            Ir::Nil
        }
    };
    // Whether a break may be inserted before atom `k`: an eligible top-level
    // binary operator with an operand to its left (a genuine infix `+`, not a
    // unary sign, a multiplicative join, or one nested in parentheses).
    let breakable = |k: usize| -> bool {
        pieces[k].break_before
            && pieces[k].role == MathRole::Binary
            && pieces[k - 1].role == MathRole::Operand
            && depth_before[k] == 0
    };
    // The first eligible top-level relation (a relation nested in parentheses or
    // protected by a conditional bar does not anchor or split a segment).
    let is_anchor = |k: usize| {
        pieces[k].break_before && pieces[k].role == MathRole::Relation && depth_before[k] == 0
    };

    // With no eligible top-level relation, continuation lines hang at the base
    // indent: the body breaks before each eligible top-level binary operator.
    let Some(anchor) = (0..pieces.len()).find(|&k| is_anchor(k)) else {
        let mut parts: Vec<Ir> = Vec::with_capacity(pieces.len() * 2);
        for (i, piece) in pieces.iter().enumerate() {
            if i > 0 {
                parts.push(if breakable(i) {
                    Ir::line()
                } else {
                    space_sep(i)
                });
            }
            parts.push(piece.ir.clone());
        }
        return Ir::group(Ir::concat(parts));
    };

    let mut lhs: Vec<Ir> = Vec::new();
    // Left-hand side, flat on the opening line.
    for (i, piece) in pieces[..anchor].iter().enumerate() {
        if i > 0 {
            lhs.push(space_sep(i));
        }
        lhs.push(piece.ir.clone());
    }
    // A multi-line left-hand side (a nested matrix/`aligned`/`cases` environment)
    // has no meaningful flat width — using it as the relation column would push
    // every hanging line dozens of columns right. Anchor the relations at the
    // base indent instead, and break before the first relation so the segment's
    // hanging indent still corresponds to real columns.
    let lhs_multiline = pieces[..anchor]
        .iter()
        .any(|p| p.ir.contains_forced_break());
    // The relation column: the left-hand side sits flat on the opening line, and
    // the first relation follows one space later. Every top-level relation aligns
    // here.
    let rel_col = if anchor == 0 || lhs_multiline {
        0
    } else {
        flat_width(&Ir::concat(lhs.clone())) + 1
    };

    let build_relation_layout = |relation_indent: usize, break_before_first: bool| {
        let mut parts = lhs.clone();
        // Each relation opens a segment running to the next relation. In the
        // aligned layout, the first relation stays beside the LHS and later
        // relations return to `relation_indent`. The base-indent fallback also
        // breaks before the first relation, so an over-deep LHS cannot dictate
        // the width of every continuation line.
        let mut i = anchor;
        let mut first_segment = true;
        while i < pieces.len() {
            if first_segment {
                if lhs_multiline {
                    parts.push(Ir::hard_line());
                } else if break_before_first {
                    parts.push(Ir::line());
                } else if anchor > 0 {
                    parts.push(space_sep(anchor));
                }
            } else {
                parts.push(Ir::line());
            }
            parts.push(pieces[i].ir.clone());
            let relw = flat_width(&pieces[i].ir);

            let start = i + 1;
            let mut j = start;
            while j < pieces.len() && !is_anchor(j) {
                j += 1;
            }
            let mut rhs: Vec<Ir> = Vec::with_capacity((j - start) * 2);
            for (offset, piece) in pieces[start..j].iter().enumerate() {
                let k = start + offset;
                rhs.push(if breakable(k) {
                    Ir::line()
                } else {
                    space_sep(k)
                });
                rhs.push(piece.ir.clone());
            }
            parts.push(Ir::group(Ir::align(relw + 1, Ir::concat(rhs))));

            first_segment = false;
            i = j;
        }

        Ir::align(relation_indent, Ir::concat(parts))
    };

    let body = if rel_col == 0 {
        build_relation_layout(0, lhs_multiline)
    } else {
        Ir::bounded_align(
            build_relation_layout(rel_col, false),
            build_relation_layout(0, true),
        )
    };
    Ir::group(body)
}

/// The shared math-atom sequencer (see [`lower_math_body`]). Ordinary math spacing
/// is *role-aware*: a single space is placed around every binary/relation operator
/// (`a+b` → `a + b`, `x=-b` → `x = -b`), reusing [`math_atom_role`]'s unary
/// detection so a `+`/`-` with no left operand stays glued to its operand
/// (`-x`, `2^{-5}`). Script-size lists suppress padding around punctuation
/// operators recursively, but retain spaces around control-word operators such as
/// `\in`; function application remains tight to its opener (`\Gamma(x)`). Both
/// modes preserve a fully glued slash but symmetrize a gap on either side, and
/// retain any separator required to avoid merging a control word with a following
/// letter. Plain operand juxtaposition keeps its authored spacing (a gap collapses
/// to one space, no gap stays tight). A `%`
/// comment forces a hard line break, and an authored own-line comment remains
/// own-line under every wrap mode so its association cannot change. A trailing
/// break (a comment at the body's end) is emitted rather than trimmed so the
/// caller's closing delimiter lands on its own line, while a trailing space is
/// dropped.
///
/// With `preserve_newlines` ([`MathWrap::Preserve`]) a trivia run spanning at
/// least one newline becomes a hard break instead of a space — the author's
/// line structure survives while in-line spacing is still normalized. A blank
/// run (≥2 newlines, invalid inside math anyway) also collapses to a single
/// break, and an edge run is still trimmed (the caller's delimiters own their
/// lines).
pub(super) fn lower_math_seq(
    elements: impl Iterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
    spacing: MathSpacing,
    preserve_newlines: bool,
) -> Ir {
    let mut out: Vec<Ir> = Vec::new();
    let mut started = false;
    // Start as a non-operand so a leading `+`/`-` reads as unary (see
    // [`collect_math_pieces`]).
    let mut prev_role = MathRole::Relation;
    let mut prev_class = MathClass::Rel;
    let mut prev_opener = false; // the previous atom ended with an opening delimiter
    let mut prev_delimiter_edge = false;
    let mut prev_spaced_slash = false;
    let mut prev_control_word_operator = false;
    let mut prev_ends_control_word = false;
    let mut prev_colon_relation_prefix = false;
    let mut prev_colon_needs_left_space = false;
    let mut prev_atom_ir_index = 0;
    let mut pending_space = false; // authored whitespace since the last atom
    let mut pending_break = false; // a comment forced a hard line break
    let mut pending_newline = false; // a preserved authored line break
    let mut pending_comment_own_line = false; // the next comment must retain its association
    let mut iter = strip_virtual_dtx_framing(elements, cx)
        .into_iter()
        .peekable();
    while let Some(el) = iter.next() {
        match el {
            // Tier 2 under `preserve_newlines` ([`MathWrap::Preserve`]) only: that
            // mode's contract is the author's line structure, and reproducing a
            // break as a break is preservation-only, hence its own fixed point.
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                let gap = consume_gap_widened(&t, &mut iter);
                if started {
                    pending_space = true;
                    pending_newline = preserve_newlines && gap.newlines > 0;
                    pending_comment_own_line = gap.newlines > 0;
                }
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => {
                if pending_break || pending_newline || pending_comment_own_line {
                    out.push(Ir::hard_line());
                } else if pending_space {
                    out.push(Ir::verbatim(" "));
                }
                out.push(Ir::verbatim(t.text()));
                started = true;
                pending_space = false;
                pending_newline = false;
                pending_comment_own_line = false;
                pending_break = true;
            }
            other => {
                // A top-level `\\` (a `LINE_BREAK` node) ends its line: emit it, then
                // force a hard break before the next atom. This is how a row stack
                // (`\[ a \\ b \]`, or an aligned body that fell back off the grid)
                // keeps each row on its own line. A `&` in a cell never reaches here,
                // so cells are unaffected.
                let is_line_break = matches!(
                    &other,
                    SyntaxElement::Node(n) if n.kind() == SyntaxKind::LINE_BREAK
                );
                for atom in lower_math_atoms(other, cx, spacing) {
                    let completes_colon_relation = prev_colon_relation_prefix
                        && atom.starts_equals_relation
                        && !pending_space
                        && !pending_break
                        && !pending_newline;
                    if completes_colon_relation && prev_colon_needs_left_space {
                        out[prev_atom_ir_index] =
                            Ir::concat([Ir::verbatim(" "), out[prev_atom_ir_index].clone()]);
                    }
                    let role = math_atom_role(
                        atom.class,
                        prev_class,
                        prev_opener,
                        atom.postfix_left_limit,
                    );
                    let spaced_slash = atom.spaced_slash
                        || atom.slash
                            && (spacing == MathSpacing::Normal && !prev_role.is_operand_like()
                                || spacing == MathSpacing::Script && prev_control_word_operator);
                    let touches_spaced_slash = prev_spaced_slash || spaced_slash;
                    let delimiter_edge = matches!(
                        atom.delimiter,
                        Some(DelimiterRole::Open | DelimiterRole::Close)
                    );
                    let touches_delimiter = prev_delimiter_edge || delimiter_edge;
                    let spaced_operands = role == MathRole::Operand
                        && prev_role == MathRole::Operand
                        && pending_space
                        && !touches_delimiter;
                    let script_operator_spacing =
                        atom.control_word_operator || prev_control_word_operator;
                    let normal_spacing = spacing == MathSpacing::Normal
                        && role != MathRole::PostfixLeftLimit
                        && prev_role != MathRole::PostfixLeftLimit
                        && (role != MathRole::Operand
                            || prev_role != MathRole::Operand
                            || pending_space);
                    let separator_start = out.len();
                    if !started {
                        // no separator before the first atom
                    } else if pending_break || pending_newline {
                        out.push(Ir::hard_line());
                    } else if completes_colon_relation {
                        // The preceding colon and this scripted equals form one
                        // relation atom, so their boundary stays tight.
                    } else if prev_ends_control_word && atom.starts_control_word_letter {
                        // Tight script spacing must not merge `\in A` into the
                        // distinct control word `\inA`.
                        out.push(Ir::verbatim(" "));
                    } else if touches_spaced_slash
                        || normal_spacing
                        || spacing == MathSpacing::Script
                            && (script_operator_spacing || spaced_operands)
                    {
                        // Space around a binary/relation operator (either side), or a
                        // collapsed authored gap between ordinary operands. Script-size
                        // lists suppress incidental gaps around operators.
                        out.push(Ir::verbatim(" "));
                    }
                    prev_opener = atom.delimiter == Some(DelimiterRole::Open);
                    prev_delimiter_edge = delimiter_edge;
                    prev_spaced_slash = spaced_slash;
                    prev_control_word_operator = atom.control_word_operator;
                    prev_ends_control_word = atom.ends_control_word;
                    prev_colon_relation_prefix = atom.colon_relation_prefix;
                    prev_colon_needs_left_space = atom.colon_relation_prefix
                        && spacing == MathSpacing::Normal
                        && started
                        && separator_start == out.len();
                    prev_atom_ir_index = out.len();
                    out.push(atom.ir);
                    started = true;
                    pending_space = false;
                    pending_newline = false;
                    pending_comment_own_line = false;
                    pending_break = is_line_break;
                    prev_role = role;
                    prev_class = atom.class;
                }
            }
        }
    }
    if pending_break {
        out.push(Ir::hard_line());
    }
    Ir::concat(out)
}

/// Lower one math atom (a non-trivia element of a math body).
pub(super) fn lower_math_element(el: SyntaxElement, cx: LowerCtx<'_>, spacing: MathSpacing) -> Ir {
    match el {
        SyntaxElement::Node(n) => match n.kind() {
            SyntaxKind::SCRIPTED => lower_scripted(&n, cx, spacing),
            SyntaxKind::SUBSCRIPT | SyntaxKind::SUPERSCRIPT => lower_script(&n, cx),
            SyntaxKind::GROUP => lower_math_group(&n, cx, spacing),
            SyntaxKind::LEFT_RIGHT => lower_left_right(&n, cx, spacing),
            // Only signature-proven math slots recurse. A scanned redefinition
            // shadows the built-in with unknown domains and restores the
            // whole-command preservation fallback.
            SyntaxKind::COMMAND if command_has_math_arg(&n, cx) => {
                lower_command_with_math_spacing(&n, cx, spacing)
            }
            SyntaxKind::COMMAND => Ir::verbatim(n.text().to_string()),
            // A block environment is an indivisible math atom whose continuation
            // lines hang from its rendered start column. Generic indentation alone
            // would instead return to the enclosing math body's base indentation.
            SyntaxKind::ENVIRONMENT => Ir::align_current(lower_node(&n, cx)),
            // Anything unexpected: defer to generic lowering.
            _ => lower_node(&n, cx),
        },
        SyntaxElement::Token(t) => lower_loose_token(&t, cx),
    }
}

/// Lower a `{…}` math group: keep the braces, format the body in math mode. The
/// body sits one column past the `{` ([`Ir::align`]), so a multi-line body (a
/// nested block environment) hangs at its own start column — its `\end{…}` under
/// its `\begin{…}` — instead of at the `{`. A single-line body is unaffected.
pub(super) fn lower_math_group(node: &SyntaxNode, cx: LowerCtx<'_>, spacing: MathSpacing) -> Ir {
    let inner = node
        .children_with_tokens()
        .filter(|el| !matches!(el.kind(), SyntaxKind::L_BRACE | SyntaxKind::R_BRACE));
    Ir::concat([
        Ir::verbatim("{"),
        Ir::align(1, lower_math_seq(inner, cx, spacing, false)),
        Ir::verbatim("}"),
    ])
}

/// Lower a signature-proven math argument while retaining its authored brace or
/// bracket delimiters. Only the body enters recursive math lowering.
pub(super) fn lower_math_argument_group(
    node: &SyntaxNode,
    cx: LowerCtx<'_>,
    spacing: MathSpacing,
) -> Ir {
    let (open_kind, close_kind, open, close) = match node.kind() {
        SyntaxKind::GROUP => (SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, "{", "}"),
        SyntaxKind::OPTIONAL => (SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET, "[", "]"),
        _ => return Ir::verbatim(node.text().to_string()),
    };
    let inner = node.children_with_tokens().filter(
        |element| !matches!(element.kind(), kind if kind == open_kind || kind == close_kind),
    );
    Ir::concat([
        Ir::verbatim(open),
        Ir::align(1, lower_math_seq(inner, cx, spacing, false)),
        Ir::verbatim(close),
    ])
}

/// Lower a `\left( … \right)` pair: the `\left`/`\right` control words and their
/// delimiter tokens are emitted verbatim, the inner `MATH` body is trimmed and
/// collapsed by [`lower_math_body`], and the trivia the parser kept between a
/// delimiter command and its delimiter (for losslessness) is dropped.
///
/// A non-empty body is set off by one space just inside each delimiter, so
/// `\left (  x + y  \right )` becomes `\left( x + y \right)`. That spacing is also
/// what keeps a control-word delimiter from gluing onto the body (`\left\langle x`
/// stays two tokens, never `\left\langlex`). An empty body stays tight
/// (`\left.\right.`).
///
/// The body sits just inside the opening delimiter, and its [`Ir::align`] width is
/// that flat opening width — so a multi-line body (a nested block environment)
/// hangs at its own start column, its `\end{…}` under its `\begin{…}` rather than
/// under the `\left`. A single-line body is unaffected.
pub(super) fn lower_left_right(node: &SyntaxNode, cx: LowerCtx<'_>, spacing: MathSpacing) -> Ir {
    let mut parts: Vec<Ir> = Vec::new();
    // The flat width of the opening run (`\left(`, `\left\langle`, …): every
    // delimiter token seen before the body.
    let mut open_width = 0usize;
    let mut seen_body = false;
    for el in node.children_with_tokens() {
        match el {
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {}
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::MATH => {
                seen_body = true;
                if !math_body_is_empty(&n) {
                    parts.push(Ir::align(
                        open_width + 1,
                        Ir::concat([
                            Ir::verbatim(" "),
                            lower_math_seq(n.children_with_tokens(), cx, spacing, false),
                            Ir::verbatim(" "),
                        ]),
                    ));
                }
            }
            SyntaxElement::Token(t) => {
                if !seen_body {
                    open_width += t.text().chars().count();
                }
                parts.push(Ir::verbatim(t.text()));
            }
            SyntaxElement::Node(n) => parts.push(lower_node(&n, cx)),
        }
    }
    Ir::concat(parts)
}

/// Whether a math body has no visible content (only whitespace/newlines), so a
/// `\left( … \right)` around it should not gain inner spaces.
pub(super) fn math_body_is_empty(node: &SyntaxNode) -> bool {
    node.text().to_string().trim().is_empty()
}

/// Lower a `SCRIPTED` atom: the base then its `^`/`_` scripts, all tight (the
/// trivia the parser kept inside the node for losslessness is dropped here).
pub(super) fn lower_scripted(node: &SyntaxNode, cx: LowerCtx<'_>, spacing: MathSpacing) -> Ir {
    Ir::concat(
        strip_virtual_dtx_framing(node.children_with_tokens(), cx)
            .into_iter()
            .filter_map(|el| match el {
                SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => None,
                SyntaxElement::Node(n)
                    if matches!(n.kind(), SyntaxKind::SUBSCRIPT | SyntaxKind::SUPERSCRIPT) =>
                {
                    Some(lower_script(&n, cx))
                }
                other => Some(lower_math_element(other, cx, spacing)),
            }),
    )
}

/// Lower a `SUBSCRIPT`/`SUPERSCRIPT`: the `_`/`^` glued tightly to its argument.
/// Braces are kept verbatim — dropping redundant single-token braces (`x^{2}` ->
/// `x^2`) is a *content* rewrite, not layout, so it lives in the linter's
/// `redundant-script-braces` autofix, keeping this layout engine whitespace-only.
pub(super) fn lower_script(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    Ir::concat(
        strip_virtual_dtx_framing(node.children_with_tokens(), cx)
            .into_iter()
            .filter_map(|el| match el {
                SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => None,
                SyntaxElement::Token(t)
                    if matches!(t.kind(), SyntaxKind::CARET | SyntaxKind::UNDERSCORE) =>
                {
                    Some(Ir::verbatim(t.text()))
                }
                other => Some(lower_math_element(other, cx, MathSpacing::Script)),
            }),
    )
}
