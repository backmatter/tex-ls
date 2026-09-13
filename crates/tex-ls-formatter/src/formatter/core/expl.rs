use super::*;

pub(super) fn lower_expl_paragraph(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let elements: Vec<SyntaxElement> = node.children_with_tokens().collect();
    let mut segments: Vec<Ir> = Vec::new();
    let mut seps: Vec<Ir> = Vec::new();
    let mut i = 0;
    while i < elements.len() {
        // Trivia straddling a run boundary feeds the join separator (one break,
        // or a preserved blank line), never the run itself: the separator
        // already begins the fresh line, so a leading newline inside the run
        // would double it — and each pass would grow a blank line.
        let mut boundary_newlines = 0;
        if !segments.is_empty() {
            while i < elements.len() {
                let SyntaxElement::Token(t) = &elements[i] else {
                    break;
                };
                if !is_collapsible_trivia(t.kind()) {
                    break;
                }
                boundary_newlines += t.text().matches('\n').count();
                i += 1;
            }
            if i >= elements.len() {
                break;
            }
        }
        let in_region = cx.in_expl3_region(elements[i].text_range().start());
        let start = i;
        while i < elements.len()
            && cx.in_expl3_region(elements[i].text_range().start()) == in_region
        {
            i += 1;
        }
        // The region ends at the `\ExplSyntaxOff` token, but a comment after
        // inline whitespace still belongs to that physical line. Lend the
        // comment to the expl3 run for layout only; the parser's region remains
        // catcode-exact, while `lower_expl_code` keeps the comment trailing and
        // prevents it from rebinding to the next command on the following pass.
        if in_region
            && elements[start..i]
                .iter()
                .rev()
                .find(|element| !is_collapsible_trivia_element(element))
                .is_some_and(is_expl_syntax_off_command)
        {
            let mut comment = i;
            while let Some(SyntaxElement::Token(token)) = elements.get(comment)
                && token.kind() == SyntaxKind::WHITESPACE
                && !token.text().contains(['\r', '\n'])
            {
                comment += 1;
            }
            if matches!(
                elements.get(comment),
                Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::COMMENT
            ) {
                i = comment + 1;
            }
        }
        // The trailing half of the boundary-trivia rule above: trivia at the end
        // of a run also feeds the separator. Left inside the run, a preserved
        // run-final newline would stack with the hard-line separator into a blank
        // line the next pass keeps growing. (Skipped for an all-trivia run, so
        // `i` always advances.)
        if i < elements.len() {
            let mut end = i;
            while end > start && is_collapsible_trivia_element(&elements[end - 1]) {
                end -= 1;
            }
            if end > start {
                i = end;
            }
        }
        let run = &elements[start..i];
        let guarded_text = (!in_region).then(|| {
            let text = run.iter().map(ToString::to_string).collect::<String>();
            text_is_fully_guarded(&text).then_some(text)
        });
        let ir = if let Some(text) = guarded_text.flatten() {
            // Region subtraction deliberately hands fully guarded `.dtx` lines
            // to the non-expl3 side. They remain byte-faithful there: generic
            // lowering would still collapse gaps when a guard is nested inside
            // a command/group rather than exposed as a direct paragraph token.
            // The leading boundary newline belongs to the separator that opened
            // this run, so reproduce from the first guard-bearing element.
            Ir::verbatim(text.trim_start_matches(['\r', '\n']))
        } else if in_region {
            lower_expl_code(run.iter().cloned(), cx, Statements::Structural)
        } else if cx.wraps_prose() && !run_carries_doc_margin(run, cx) {
            reflow_elements(run.iter().cloned(), cx, ReflowKind::Prose)
        } else if cx.wraps_prose() && dtx_run_reflows_safely(run, cx) {
            // `.dtx` documentation prose between expl3 regions, riding `% `
            // margins: reflow it under the margin like a doc paragraph. Gated
            // exactly like [`lower_dtx_doc_paragraph`] — margined first line, no
            // environments, and the speculative escape probe — so a run the
            // reflow cannot keep under the margin still falls through below.
            reflow_elements(run.iter().cloned(), cx, ReflowKind::DtxProse)
        } else {
            // Either a non-wrapping mode, or `.dtx` documentation-layer text
            // between expl3 regions that the gate above declined: it rides
            // margin-framed `macrocode` frames or unmargined lines that generic
            // prose reflow would relocate off column 0 — a semantics change that
            // leaves the next pass unparseable — so it takes the byte-faithful
            // stream in every wrap mode.
            Ir::concat(lower_element_stream(run.iter().cloned(), cx))
        };
        if !matches!(ir, Ir::Nil) {
            if !segments.is_empty() {
                seps.push(if boundary_newlines >= 2 {
                    Ir::empty_line()
                } else {
                    Ir::hard_line()
                });
            }
            segments.push(ir);
        }
    }
    let mut result = Vec::with_capacity(segments.len().saturating_mul(2));
    for (n, seg) in segments.into_iter().enumerate() {
        if n > 0 {
            result.push(seps[n - 1].clone());
        }
        result.push(seg);
    }
    Ir::concat(result)
}

/// Whether every nonempty physical line in `text` begins with a docstrip guard.
/// Guards are recognized only at column zero, so the spelling is the structural
/// fact the `.dtx` lexer exposes as `GUARD` before any formatter pass can move it.
pub(super) fn text_is_fully_guarded(text: &str) -> bool {
    let mut content_lines = text.lines().filter(|line| !line.is_empty());
    content_lines
        .next()
        .is_some_and(|line| line.starts_with("%<"))
        && content_lines.all(|line| line.starts_with("%<"))
}

/// Lower expl3 statements with width-driven fills. Calls with derivable arity
/// use sticky argument layout; unknown runs use a hugging fill. Comments, guards
/// and blank lines retain their boundaries. Single newlines are ordinary trivia.
pub(super) fn lower_expl_code(
    elements: impl Iterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
    statements: Statements,
) -> Ir {
    let elements: Vec<SyntaxElement> = elements.collect();
    // The statement-boundary map (structural mode only): computed over the very
    // element vector lowered below, so indices align by construction.
    let map = (statements == Statements::Structural).then(|| segment_expl_statements(&elements));
    let mut lines: Vec<Ir> = Vec::new();
    let mut seps: Vec<Ir> = Vec::new();
    let mut pending_sep = Ir::hard_line();

    // The current logical line is built as a Wadler *fill*: `atom` accumulates the
    // glued pieces of the atom in progress; `parts` is the alternating
    // `[atom, sep, atom, …]` the printer fills greedily. `sep_before_next` is the
    // separator to emit before the next atom — `Line` for an inter-token space
    // (flat: one space, keeping the token boundary), `SoftLine` after a `~` (flat:
    // nothing, since the `~` is itself the space).
    let mut atom: Vec<Ir> = Vec::new();
    let mut parts: Vec<Ir> = Vec::new();
    let mut sep_before_next: Option<Ir> = None;

    /// Commit the glued atom in progress as one fill atom, prefixing the pending
    /// separator when it is not the first atom of the line.
    fn flush_atom(atom: &mut Vec<Ir>, parts: &mut Vec<Ir>, sep_before_next: &mut Option<Ir>) {
        if atom.is_empty() {
            return;
        }
        if !parts.is_empty() {
            parts.push(sep_before_next.take().unwrap_or(Ir::Line));
        }
        parts.push(Ir::concat(atom.drain(..)));
        *sep_before_next = None;
    }

    /// The fill a line's `[atom, sep, atom, …]` parts commit as: sticky for a
    /// structural statement, hugging for a fallback (or junk-glued) one — see
    /// `commit_line`. Every early line commit must build its head with this,
    /// not a bare [`Ir::Fill`]: a statement that ends one atom earlier on the
    /// next pass hands the same atoms to a different arm, and the two arms have
    /// to measure them the same way (`xo-place.dtx`).
    fn line_fill(mut parts: Vec<Ir>, sticky: bool) -> Ir {
        if parts.len() == 1 {
            return parts.drain(..).next().unwrap();
        }
        if sticky {
            Ir::StickyFill(parts.into())
        } else {
            Ir::HugFill(parts.into())
        }
    }

    /// Commit the in-progress line (if any) as the next logical line, recording the
    /// pending line separator before it and resetting line state (`sticky` resets
    /// to `true`, the structural default).
    fn commit_line(
        atom: &mut Vec<Ir>,
        parts: &mut Vec<Ir>,
        sep_before_next: &mut Option<Ir>,
        lines: &mut Vec<Ir>,
        seps: &mut Vec<Ir>,
        pending_sep: &mut Ir,
        sticky: &mut bool,
    ) {
        flush_atom(atom, parts, sep_before_next);
        if !parts.is_empty() {
            // Known calls cascade their hanging arguments. Unknown runs pack
            // greedily and measure a multiline atom by its first line.
            let line = line_fill(std::mem::take(parts), *sticky);
            seps.push(std::mem::replace(pending_sep, Ir::hard_line()));
            lines.push(line);
        }
        parts.clear();
        *sep_before_next = None;
        *sticky = true;
    }

    // True right after a multi-line block was pushed as its own line, surviving
    // an inline (newline-free) whitespace run: a trailing comment there rides
    // the block's closing line (`}%`, the macro-code continuation idiom).
    // Stranding it would mint a fresh *own-line* comment, which the next parse
    // binds leading into the following command — a different
    // shape, so a different layout: idempotence would break.
    let mut after_block = false;
    // Whether the line in progress commits as a sticky fill (structural
    // statements) or a plain greedy fill (fallback/junk-glued statements) —
    // see `commit_line`. Any fallback-marked element makes its line greedy.
    let mut line_sticky = true;
    let mut idx = 0;
    while idx < elements.len() {
        if let Some(m) = map.as_ref()
            && (m.is_fallback(idx) || m.is_glued(idx))
        {
            line_sticky = false;
        }
        match &elements[idx] {
            // Insignificant whitespace: a gap the boundary map marks ends the
            // logical line, a blank line promotes the next line separator, and
            // any other run is a single (breakable) space before the next atom.
            // The run's newline count is read only for the blank-line promotion
            // and the `after_block` clear — both preserved predicates — never
            // for the boundary itself (trivia-invariant layout).
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                let run_start = idx;
                let newlines = consume_widened_gap_slice(&elements, &mut idx);
                let boundary = map
                    .as_ref()
                    .is_some_and(|m| run_start > 0 && m.boundary_after(run_start - 1));
                if boundary {
                    after_block = false;
                    commit_line(
                        &mut atom,
                        &mut parts,
                        &mut sep_before_next,
                        &mut lines,
                        &mut seps,
                        &mut pending_sep,
                        &mut line_sticky,
                    );
                    if newlines >= 2 {
                        pending_sep = Ir::empty_line();
                    }
                } else {
                    // A trailing comment glues only to a directly-abutting `}`,
                    // never across a line break (own-line-ness is preserved).
                    if newlines >= 1 {
                        after_block = false;
                    }
                    flush_atom(&mut atom, &mut parts, &mut sep_before_next);
                    // Keep tie breaks and unbreakable gaps in glued statements.
                    if sep_before_next.is_none() {
                        // Keep a call and its single-token operands together. Braced
                        // arguments provide the hanging break opportunities.
                        let bare_arg_glue = map.is_none() && elements.get(idx).is_some_and(|el| {
                            !matches!(
                                el,
                                SyntaxElement::Node(n)
                                    if matches!(n.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL)
                            ) && !matches!(
                                el,
                                SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT
                            )
                        });
                        sep_before_next = Some(
                            if bare_arg_glue || map.as_ref().is_some_and(|m| m.is_glued(idx)) {
                                Ir::verbatim(" ")
                            } else {
                                Ir::Line
                            },
                        );
                    }
                }
                continue;
            }
            // `~`: a literal space. Glue it to the end of the current atom, then
            // close the atom with a soft break (flat: nothing; broken: newline).
            // A tie directly before a recognized head mid-fallback-statement
            // must not break either (`xo-or.dtx`'s `=~ \exp_not:c {…}` trace
            // lines), so that gap renders as nothing (`Nil`, the soft break's
            // flat form).
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::TILDE => {
                after_block = false;
                atom.push(Ir::verbatim(token.text()));
                flush_atom(&mut atom, &mut parts, &mut sep_before_next);
                let next = elements[idx + 1..]
                    .iter()
                    .position(|el| {
                        !matches!(el, SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()))
                    })
                    .map(|off| idx + 1 + off);
                sep_before_next = Some(
                    if next.is_some_and(|n| map.as_ref().is_some_and(|m| m.is_glued(n))) {
                        Ir::Nil
                    } else {
                        Ir::SoftLine
                    },
                );
            }
            // A comment ends its line (it must terminate the source line). One
            // trailing a multi-line block glues onto the block's closing line
            // (see `after_block` above), spaced when the source spaced it. One
            // trailing code rides the committed line *outside* its width fill
            // — zero-width, rustfmt-style: the line may overflow, but prose
            // length never re-breaks code, and relocating the comment would
            // rebind it as the next statement's leading doc comment on the
            // second pass, changing its attachment. An own-line
            // comment stays its own line.
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMENT => {
                if after_block {
                    let block = lines.pop().expect("after_block implies a pushed line");
                    let spaced = sep_before_next.take().is_some();
                    lines.push(Ir::concat(if spaced {
                        vec![block, Ir::verbatim(" "), Ir::verbatim(token.text())]
                    } else {
                        vec![block, Ir::verbatim(token.text())]
                    }));
                    after_block = false;
                } else if atom.is_empty() && parts.is_empty() {
                    atom.push(Ir::verbatim(token.text()));
                    commit_line(
                        &mut atom,
                        &mut parts,
                        &mut sep_before_next,
                        &mut lines,
                        &mut seps,
                        &mut pending_sep,
                        &mut line_sticky,
                    );
                } else {
                    // A non-empty `atom` means the comment directly abuts it
                    // (any trivia would have flushed the atom); an empty one
                    // means source whitespace preceded, kept as one space.
                    let spaced = atom.is_empty();
                    commit_line(
                        &mut atom,
                        &mut parts,
                        &mut sep_before_next,
                        &mut lines,
                        &mut seps,
                        &mut pending_sep,
                        &mut line_sticky,
                    );
                    let line = lines
                        .pop()
                        .expect("trailing comment follows committed code");
                    let comment = if spaced {
                        format!(" {}", token.text())
                    } else {
                        token.text().to_string()
                    };
                    lines.push(Ir::concat(vec![line, Ir::zero_width(comment)]));
                }
            }
            // A docstrip guard (`%<…>`) is recognized only at column 0, so it must
            // lead its output line. Under `Statements::Ignore` (a command's attached
            // arguments) source newlines are catcode-9 whitespace, so without this a
            // guard between two arguments packs onto the previous line as a trailing
            // `%<…>` comment — losing its guard semantics and re-lexing on the next
            // parse as an ordinary comment that swallows the following argument's
            // braces (issue #78, l3backend-basics.dtx's per-backend `.def` list).
            // Commit the line in progress so the guard opens a fresh one, where
            // `lower_loose_token` pins it to column 0; the following code stays on
            // the guard's line via the ordinary inter-token fill.
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::GUARD => {
                after_block = false;
                commit_line(
                    &mut atom,
                    &mut parts,
                    &mut sep_before_next,
                    &mut lines,
                    &mut seps,
                    &mut pending_sep,
                    &mut line_sticky,
                );
                atom.push(lower_loose_token(token, cx));
            }
            SyntaxElement::Token(token) => {
                after_block = false;
                atom.push(lower_loose_token(token, cx));
            }
            // A command with a bound leading `DOC_COMMENT` (an
            // own-line comment binds forward). Rendered as an opaque block it
            // would strand a blank line after the comment (the comment's own
            // newline stacking with the block separator) and split the
            // statement — a shape the next parse reads differently, breaking
            // idempotence. Instead each comment line commits as its own line
            // and the command's remaining children continue the statement.
            SyntaxElement::Node(child)
                if child.kind() == SyntaxKind::COMMAND
                    && child
                        .first_child()
                        .is_some_and(|c| c.kind() == SyntaxKind::DOC_COMMENT) =>
            {
                after_block = false;
                commit_line(
                    &mut atom,
                    &mut parts,
                    &mut sep_before_next,
                    &mut lines,
                    &mut seps,
                    &mut pending_sep,
                    &mut line_sticky,
                );
                let mut rest: Vec<SyntaxElement> = Vec::new();
                for el in child.children_with_tokens() {
                    match &el {
                        SyntaxElement::Node(n) if n.kind() == SyntaxKind::DOC_COMMENT => {
                            for t in n.children_with_tokens() {
                                if let SyntaxElement::Token(t) = t
                                    && t.kind() == SyntaxKind::COMMENT
                                {
                                    seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                                    lines.push(Ir::verbatim(t.text()));
                                }
                            }
                        }
                        _ => rest.push(el.clone()),
                    }
                }
                let ir = lower_expl_code(rest.into_iter(), cx, Statements::Ignore);
                if !matches!(ir, Ir::Nil) {
                    atom.push(ir);
                }
            }
            SyntaxElement::Node(child) => {
                after_block = false;
                // A junk-bearing *glued* statement bypasses every layout
                // special: its nodes accumulate as plain atoms joined by the
                // hard separators the trivia arm supplies, so the authored
                // line shape survives verbatim (see [`StatementMap::is_glued`]
                // — a conditional explosion or head-hug here would commit a
                // line mid-statement and strand the junk on a fresh line the
                // next pass segments differently).
                let in_glued = map.as_ref().is_some_and(|m| m.is_glued(idx));
                // R2 ("everything divided up using spaces"): an expl3 function's
                // brace argument written flush against its head
                // (`\clist_count:n{#1}`) gets the house style's space. Synthesized
                // *here*, by flushing the atom exactly as a source gap would, so
                // every branch below sees the state the spaced spelling produces —
                // hang, conditional explosion, trailing-hang candidates — with no
                // second spelling to special-case. Inserting the space is a trivia
                // edit and catcode-safe: in-region source spaces are catcode 9, so
                // the token stream is unchanged (a real space is `~`, catcode 10).
                // Junk-glued statements are exempt: their authored line shape is
                // load-bearing (see above).
                if !in_glued
                    && !atom.is_empty()
                    && child.kind() == SyntaxKind::GROUP
                    && expl_arg_takes_leading_space(child)
                {
                    flush_atom(&mut atom, &mut parts, &mut sep_before_next);
                    sep_before_next = Some(Ir::Line);
                }
                // A *statement-leading* expl3 conditional (`\…:nTF {c} {T} {F}`,
                // nothing on the logical line before it) explodes structurally: the
                // head on its own line, then each `T`/`F` branch on its own line at
                // +6 (R4/R5), regardless of whether it would fit inline. Keyed on the
                // command name's argspec suffix ([`expl3::conditional_branches`]); a
                // conditional used mid-line as a value (`,key = \…:nTF …`, atom or
                // parts non-empty) is not statement-leading and stays on the
                // width-driven head-hug path (issue #71).
                //
                // [`expl_conditional_at`] covers the branches wherever greedy
                // attachment put them — on the head, or on a sibling an `N`/`V` slot
                // handed them to — so the explosion does not depend on an accident of
                // the surrounding tokens, and the unit may span several siblings
                // (hence the `last`-driven resume rather than `idx += 1`).
                if !in_glued
                    && parts.is_empty()
                    && atom.is_empty()
                    && child.kind() == SyntaxKind::COMMAND
                    && let Some((cond_ir, last)) = expl_conditional_at(&elements, idx, cx)
                {
                    seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                    lines.push(cond_ir);
                    after_block = true;
                    idx = last + 1;
                    continue;
                }
                // A *trailing* expl3 conditional — one used mid-line as a value, with
                // head atoms before it on the line and only trivia after it in the
                // statement — is width-conditional. It stays flat on the line when the
                // whole statement fits (issue #71's `,key = \…:nTF {c} {T} {F}` shape),
                // but when head and conditional together overflow, the head drops to
                // its own line, which makes the conditional *statement-leading* on the
                // next parse so it then explodes unconditionally (R4). Committing head
                // and conditional together as one `group(IfBreak { flat, broken })`
                // — measured by the group's *flat* width (head included, so neither
                // fooled by a branch that detonates internally nor evaluated apart
                // from its head) — makes the passes agree: it fits => `head cond` on
                // one line; it overflows => head on its own line then the R4
                // explosion, which re-parses statement-leading and re-explodes to the
                // identical bytes (idempotency failure, `lthooks.dtx`, issue #96). The
                // `!(…)` guard leaves the statement-leading position to the block above.
                // Suppressed in a *fallback* statement too: a conditional name
                // mid-way through an unrecognized line is data being spliced
                // (`xtemplate`'s `cs_ \str_if_eq:nnT {#1} { global } { g }
                // set:Npn` name assembly), not a call — and whether it sits
                // trailing depends on where the line's junk ends, which is not
                // pass-invariant.
                //
                // **Head-attached branches only** — deliberately, unlike the
                // statement-leading arm above. Mid-statement the conditional is not
                // the head of its own unit; the enclosing segmentation already
                // decided it is an *argument* being passed as a token
                // (`\@@_patch_check:NNnn \cs_if_exist:NTF #1 { undef } {…}`, where
                // `{ undef }` is `\@@_patch_check:NNnn`'s third argument, and
                // `\exp_not:N \…:nTF`). Re-scanning it as a head would resolve
                // "branches" belonging to the outer call and explode them — a misread
                // in every one of the eight latex2e/latex3 sites it reached.
                // [`lower_expl_conditional`] reads only the node's own greedily
                // attached children, so it cannot make that mistake.
                let in_fallback = map.as_ref().is_some_and(|m| m.is_fallback(idx));
                let statement_leading = parts.is_empty() && atom.is_empty();
                if !in_glued
                    && !in_fallback
                    && !statement_leading
                    && child.kind() == SyntaxKind::COMMAND
                    && is_trailing_in_statement(&elements, idx, map.as_ref())
                    && let Some(exploded) = command_name(child)
                        .and_then(|name| expl3::conditional_branches(&name))
                        .and_then(|n| lower_expl_conditional(child, cx, n))
                {
                    // The head↔conditional separator: a space when trivia flushed the
                    // atom (`… \…:nTF`), nothing when the conditional directly abuts
                    // the atom in progress (`…\…:nTF`, no space). `flush_atom`'s own
                    // `sep_before_next` handles the *internal* head joins.
                    let sep = if atom.is_empty() {
                        sep_before_next.take().unwrap_or(Ir::Line)
                    } else {
                        Ir::Nil
                    };
                    flush_atom(&mut atom, &mut parts, &mut sep_before_next);
                    let head = if parts.len() == 1 {
                        parts.drain(..).next().unwrap()
                    } else {
                        Ir::StickyFill(std::mem::take(&mut parts).into())
                    };
                    let flat = Ir::concat(vec![head.clone(), sep, lower_node(child, cx)]);
                    let broken = Ir::concat(vec![head, Ir::hard_line(), exploded]);
                    seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                    lines.push(Ir::group(Ir::if_break(flat, broken)));
                    after_block = true;
                    idx += 1;
                    continue;
                }
                // For a trailing group with multiple commands, measure the whole
                // head and body. Choose a flat form, a hanging inline body, or a
                // hanging block according to width. Other argument shapes use the
                // ordinary group layout below.
                if child.kind() == SyntaxKind::GROUP
                    && atom.is_empty()
                    && !parts.is_empty()
                    && !expl_group_forces_break(child)
                    && expl_group_body_is_multi_atom(child)
                    && is_trailing_in_statement(&elements, idx, map.as_ref())
                    && !statement_has_preceding_group(&elements, idx, map.as_ref())
                    && !head_command_has_grouped_sibling_arg(child)
                    && let ExplGroupPieces::Pieces {
                        open_ir,
                        body,
                        close_ir,
                        spaced,
                        forced: false,
                    } = expl_group_pieces(child, SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, cx)
                    // Forced breaks cannot participate in an inline candidate; the
                    // ordinary hanging layout handles them.
                    && !body.contains_forced_break()
                {
                    let sep = sep_before_next.take().unwrap_or(Ir::Line);
                    flush_atom(&mut atom, &mut parts, &mut sep_before_next);
                    let head = if parts.len() == 1 {
                        parts.drain(..).next().unwrap()
                    } else {
                        Ir::StickyFill(std::mem::take(&mut parts).into())
                    };
                    let space = if spaced { Ir::verbatim(" ") } else { Ir::Nil };
                    // Measure every candidate with nested groups flat. This prevents
                    // an inner wrap from making an overlong inline candidate appear
                    // to fit. The last candidate is the fully broken fallback.
                    let c_flat = Ir::concat(vec![
                        head.clone(),
                        sep,
                        open_ir.clone(),
                        space.clone(),
                        body.clone(),
                        space.clone(),
                        close_ir.clone(),
                    ]);
                    let c_allman_inline = Ir::concat(vec![
                        head.clone(),
                        Ir::indent(Ir::concat(vec![
                            Ir::hard_line(),
                            open_ir.clone(),
                            space.clone(),
                            body.clone(),
                            space,
                            close_ir.clone(),
                        ])),
                    ]);
                    let c_allman_broken = Ir::concat(vec![
                        head,
                        Ir::indent(Ir::concat(vec![
                            Ir::hard_line(),
                            open_ir,
                            Ir::indent(Ir::concat(vec![Ir::hard_line(), body])),
                            Ir::hard_line(),
                            close_ir,
                        ])),
                    ]);
                    seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                    lines.push(Ir::conditional_group_all_lines(vec![
                        c_flat,
                        c_allman_inline,
                        c_allman_broken,
                    ]));
                    after_block = true;
                    idx += 1;
                    continue;
                }
                // A brace group that *starts a fresh atom* (nothing glued before
                // it — any trivia flushed the atom) is a *continuation*: it indents
                // one step under its head statement, the l3 house style
                // (`\cs_new:Npn \foo:n #1` / `  { body }`, or `\bool_if:nTF {cond}`
                // / `  { true }`). The step is carried by an `Indent` folded around
                // the break *and the group alone* — never the rest of the line,
                // whose atoms the reparse reads as ordinary base-indent statements
                // when a width break separates them (the group's own internal lines
                // must sit at the deeper level either way, so break and body travel
                // together). This holds identically whether statement boundaries
                // are structural (`Structural`) or absent within one command's
                // attached arguments (`Ignore`), so the rule keys only on the
                // group shape, not the statement mode.
                let hang_group = child.kind() == SyntaxKind::GROUP && atom.is_empty();
                let starts_line = parts.is_empty();
                // Each brace argument breaks on its own merits: the l3styleguide's
                // own example keeps a short branch inline (`{ \module_foo_aux:n
                // { X #2 } }`) beside a multi-line sibling, and l3kernel does the
                // same throughout. A sibling's forced break is not this group's
                // business.
                let ir = lower_node(child, cx);
                // Unknown runs use the hugging fill throughout; committing at
                // an internal group would detach its following siblings.
                debug_assert!(
                    !in_fallback || !line_sticky,
                    "a fallback statement must commit as a hugging fill"
                );
                let forced_dispatch = !in_fallback && ir.contains_forced_break();
                // A junk-bearing glued statement: plain atom accumulation, hard
                // separators, no line commits until the boundary (see above).
                if in_glued {
                    atom.push(ir);
                }
                // Keep a trailing command attached to its head while its own
                // argument supplies the hanging break. Use the same path for
                // soft and forced bodies.
                else if map.is_some()
                    && child.kind() == SyntaxKind::COMMAND
                    && atom.is_empty()
                    && !parts.is_empty()
                    && is_trailing_in_statement(&elements, idx, map.as_ref())
                    && child.children().any(|c| c.kind() == SyntaxKind::GROUP)
                {
                    let head = line_fill(std::mem::take(&mut parts), line_sticky);
                    // The head↔command separator is the pending gap's *flat*
                    // form: a space for an ordinary inter-token gap, nothing
                    // after a tie (`plus ~\__char_show_code:n {…}` must not
                    // grow a space the next parse does not have).
                    let sep = match sep_before_next.take() {
                        Some(Ir::SoftLine) | Some(Ir::Nil) => Ir::Nil,
                        _ => Ir::verbatim(" "),
                    };
                    seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                    lines.push(Ir::concat(vec![head, sep, ir]));
                    after_block = true;
                } else if forced_dispatch {
                    if !atom.is_empty() {
                        // A block hanging off a *directly-abutting* atom stays
                        // glued (`\cs_if_exist:NF\tag_if_active:T { … }` with a
                        // multi-line body): committing the atom alone would split
                        // the abutting pair — but on the pass before, when the
                        // body still fit softly, they rendered glued, so the two
                        // passes would never agree. Gluing is the fixed point:
                        // the pair abuts identically on every pass.
                        atom.push(ir);
                        commit_line(
                            &mut atom,
                            &mut parts,
                            &mut sep_before_next,
                            &mut lines,
                            &mut seps,
                            &mut pending_sep,
                            &mut line_sticky,
                        );
                    } else if hang_group {
                        // A multi-line brace group separated from its head by a
                        // space (Allman): end the current line — flushing any head
                        // (`\__kernel…` before its `{T}`) as its own line — then
                        // place the group on its own line(s) hung one step, folding
                        // the line separator into the `Indent` (the seps slot gets
                        // `Nil` so the two stay paired). The run's first line has no
                        // separator to fold and stays at the current level.
                        commit_line(
                            &mut atom,
                            &mut parts,
                            &mut sep_before_next,
                            &mut lines,
                            &mut seps,
                            &mut pending_sep,
                            &mut line_sticky,
                        );
                        if !lines.is_empty() {
                            let sep = std::mem::replace(&mut pending_sep, Ir::hard_line());
                            seps.push(Ir::Nil);
                            lines.push(Ir::indent(Ir::concat(vec![sep, ir])));
                        } else {
                            seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                            lines.push(ir);
                        }
                    } else if !parts.is_empty() {
                        // Head-hug: a detonating *non-group* child (a command
                        // subtree whose first line is a head atom, e.g. the N-arg
                        // `\__kernel…{T}{F}` of `\cs_if_exist:NTF`) follows a head
                        // on this line, separated by a space. Keep them on one line
                        // when the prefix up to the block's first forced break fits,
                        // letting the block body break below — a rest-aware
                        // `group_hug`, so pass-stable (never the `step_fill` local
                        // cascade that would split a short head off a detonating
                        // trailing block).
                        let head = line_fill(std::mem::take(&mut parts), line_sticky);
                        let sep = sep_before_next.take().unwrap_or(Ir::Line);
                        seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                        lines.push(Ir::group_hug(Ir::concat(vec![head, sep, ir])));
                    } else {
                        // A multi-line block with no head to hug and no group to
                        // hang: place it on its own line(s) at the current level.
                        commit_line(
                            &mut atom,
                            &mut parts,
                            &mut sep_before_next,
                            &mut lines,
                            &mut seps,
                            &mut pending_sep,
                            &mut line_sticky,
                        );
                        seps.push(std::mem::replace(&mut pending_sep, Ir::hard_line()));
                        lines.push(ir);
                    }
                    after_block = true;
                } else if hang_group && starts_line && !lines.is_empty() {
                    // Line-initial: fold the statement separator in; the
                    // `commit_line` seps slot then carries `Nil`.
                    let sep = std::mem::replace(&mut pending_sep, Ir::Nil);
                    atom.push(Ir::indent(Ir::concat(vec![sep, ir])));
                } else if hang_group && !starts_line {
                    // Mid-statement: if the width fill breaks at this gap, the
                    // group starts a continuation line hung one step. Flat, the
                    // leading `Line` is the single inter-token space (the `Nil`
                    // separator adds nothing). A `{`-led continuation line is
                    // safe in a fallback statement too: the reparse reads it as
                    // a statement-leading group and the continuation-hang fold
                    // below re-indents it identically. (A glued statement never
                    // reaches here — the `in_glued` arm above owns its nodes.)
                    // In a fallback statement this is also the *only* path a
                    // hanging group takes, forced or soft (`forced_dispatch`
                    // above): a forced body's `flat_width` is `None`, so
                    // `step_fill` dispatches this atom `Mode::Break` on every
                    // pass at every width and the leading `Line` breaks — the
                    // same bytes the forced arm would have emitted, minus its
                    // line commit.
                    sep_before_next = Some(Ir::Nil);
                    atom.push(Ir::indent(Ir::concat(vec![Ir::Line, ir])));
                } else {
                    atom.push(ir);
                }
            }
        }
        // A structural boundary whose gap holds no trivia (`\foo:\bar:`
        // abutting, or a statement split out of an authored same-line pair):
        // the trivia arm never sees a run there, so commit here. Re-committing
        // an already-committed line is a no-op.
        if map.as_ref().is_some_and(|m| m.boundary_after(idx)) {
            commit_line(
                &mut atom,
                &mut parts,
                &mut sep_before_next,
                &mut lines,
                &mut seps,
                &mut pending_sep,
                &mut line_sticky,
            );
        }
        idx += 1;
    }
    commit_line(
        &mut atom,
        &mut parts,
        &mut sep_before_next,
        &mut lines,
        &mut seps,
        &mut pending_sep,
        &mut line_sticky,
    );

    let mut result: Vec<Ir> = Vec::with_capacity(lines.len().saturating_mul(2));
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            result.push(seps[i].clone());
        }
        result.push(line);
    }
    Ir::concat(result)
}

/// Comments, guards and docstrip margins require a broken group at every width.
pub(super) fn expl_group_forces_break(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .any(|t| {
            matches!(
                t.kind(),
                SyntaxKind::COMMENT | SyntaxKind::GUARD | SyntaxKind::DOC_MARGIN
            )
        })
}

/// Whether the group body contains multiple top-level commands. This selects
/// a layout candidate that measures the whole body before hanging its opening brace.
pub(super) fn expl_group_body_is_multi_atom(node: &SyntaxNode) -> bool {
    node.children()
        .filter(|n| n.kind() == SyntaxKind::COMMAND)
        .count()
        >= 2
}

/// Whether an expl3 brace group's body is a *simple run of parameters* — `{#1}`,
/// `{#1#2}`, `{##1}` — the l3styleguide's explicit exception to the
/// divide-with-spaces rule ("With the exception of simple runs of parameter
/// (`{#1}`, `#1#2`, etc.), everything should be divided up using spaces"). Such a
/// run stays tight even inside an expl3-named command's arguments. Outer padding
/// is ignored, so `{ #1 }` normalizes to tight `{#1}`; but any whitespace
/// *between* the parameters, or any non-parameter token, disqualifies the run, so
/// `{ #1 #2 }` and `{ X #2 }` keep the canonical inner spaces (matching the
/// l3styleguide's own worked example). Reads only token kinds and digit text — no
/// semantics, no signature lookup.
pub(super) fn is_simple_param_run(node: &SyntaxNode) -> bool {
    // Body tokens with the delimiters dropped; a non-token child (a nested group
    // or command) is never a bare parameter run.
    let mut body: Vec<SyntaxToken> = Vec::new();
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Token(t) => match t.kind() {
                SyntaxKind::L_BRACE
                | SyntaxKind::R_BRACE
                | SyntaxKind::L_BRACKET
                | SyntaxKind::R_BRACKET => {}
                _ => body.push(t),
            },
            SyntaxElement::Node(_) => return false,
        }
    }
    // Trim the padding whitespace we may be about to remove; any *interior*
    // whitespace survives and disqualifies the run below.
    let is_space =
        |t: &SyntaxToken| matches!(t.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE);
    while body.first().is_some_and(is_space) {
        body.remove(0);
    }
    while body.last().is_some_and(is_space) {
        body.pop();
    }
    // A run is `#`s and single-digit indices, adjacent, each digit preceded by a
    // `#` (so `##1` counts, a stray `{1}` or `{ #1 #2 }` does not).
    let mut saw_hash = false;
    let mut prev_hash = false;
    for t in &body {
        match t.kind() {
            SyntaxKind::HASH => {
                saw_hash = true;
                prev_hash = true;
            }
            SyntaxKind::WORD if prev_hash && is_param_digit(t) => {
                prev_hash = false;
            }
            _ => return false,
        }
    }
    saw_hash
}

/// Whether the element at `idx` is the last *meaningful* element of its statement —
/// the boundary map ends the statement at it, or (under [`Statements::Ignore`],
/// where `map` is `None`) only collapsible trivia follows it in the stream. Used
/// to gate the trailing-conditional width-conditional lowering in
/// [`lower_expl_code`]: a conditional with content after it on the same statement
/// is not a clean trailing value, so it stays on the ordinary fill path. A `~`
/// (`TILDE`) or comment is not collapsible trivia, so it counts as following
/// content.
pub(super) fn is_trailing_in_statement(
    elements: &[SyntaxElement],
    idx: usize,
    map: Option<&StatementMap>,
) -> bool {
    if let Some(m) = map
        && m.boundary_after(idx)
    {
        return true;
    }
    for element in &elements[idx + 1..] {
        match element {
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {}
            _ => return false,
        }
    }
    true
}

/// Whether an *earlier argument* of `child`'s owning command is a brace or
/// bracket group — the mark of a multi-argument call
/// (`\prop_get:NnNTF \g…_prop {#2} \l…_tl {branch}`) rather than a plain
/// `\cmd \target {body}` hang. The trailing-hang three-way sees only the
/// owning command's [`Statements::Ignore`] stream from `child` onward, so it
/// cannot tell the two apart without this look at the earlier children.
///
/// A recognized call owns every argument its argspec consumes, so earlier
/// grouped material appears among `child`'s preceding siblings. A node's
/// children form one statement.
pub(super) fn head_command_has_grouped_sibling_arg(child: &SyntaxNode) -> bool {
    let Some(owner) = child.parent() else {
        return false;
    };
    // Only an *attached* argument has a head with sibling arguments: a
    // stream-level group's parent is the container itself, which is not a
    // call whose earlier slots could have consumed a group.
    if owner.kind() != SyntaxKind::COMMAND {
        return false;
    }
    let child_start = child.text_range().start();
    owner
        .children_with_tokens()
        .take_while(|el| el.text_range().start() < child_start)
        .any(|el| matches!(el.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL))
}

/// Whether a `GROUP`/`OPTIONAL` sibling precedes the element at `idx` within its
/// statement (back to the previous boundary in the map, or to the stream start
/// under [`Statements::Ignore`], where `map` is `None`). Used to keep the
/// trailing-hang three-way off a group that is *one of several* trailing brace
/// groups — the branch list of a conditional call whose N/V argument broke greedy
/// attachment (`\prop_get:NnNTF \g…_prop {#2} \l…_tl {T} {F}`), or any
/// multi-argument shape — where the existing hang path already lays the branches
/// out stably. A lone trailing group (`\l…_tl {body}`, `\bool_if:NF\…_bool
/// {body}`) has no preceding group and still qualifies.
pub(super) fn statement_has_preceding_group(
    elements: &[SyntaxElement],
    idx: usize,
    map: Option<&StatementMap>,
) -> bool {
    for j in (0..idx).rev() {
        // A boundary after `j` puts `j` in the previous statement.
        if let Some(m) = map
            && m.boundary_after(j)
        {
            return false;
        }
        match &elements[j] {
            SyntaxElement::Node(n)
                if matches!(n.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Whether an expl3-region group's *flat* form carries the l3 house style's
/// canonical inner spaces (`{ value }`, per the l3styleguide) or stays tight
/// (`{parbox/after}`). Spaced when the group is the attached argument of an
/// expl3-*named* command — the name contains `_` or `:`, a purely lexical fact
/// (no signature or semantics lookup) — or a bare code block (an expl3 function
/// body). Tight when it belongs to an embedded LaTeX2e-named command
/// (`\UseTaggingSocket`, `\@parboxto`), whose authors write tight braces; the
/// house style governs expl3 functions, not 2e code that happens to sit inside
/// a region. Tight, too, for a *simple run of parameters* ([`is_simple_param_run`]),
/// the l3styleguide's own exception (`{#1}`, `#1#2`), regardless of the command.
pub(super) fn expl_group_is_spaced(node: &SyntaxNode) -> bool {
    if is_simple_param_run(node) {
        return false;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
    if parent.kind() != SyntaxKind::COMMAND {
        return true;
    }
    match command_name(&parent) {
        Some(name) => name.contains('_') || name.contains(':'),
        None => true,
    }
}

/// Expl3-named commands get a space before attached brace arguments, including
/// parameter runs: `\clist_count:n {#1}`. Embedded LaTeX2e commands retain their
/// authored gap. Inner padding is handled separately by [`expl_group_is_spaced`].
pub(super) fn expl_arg_takes_leading_space(node: &SyntaxNode) -> bool {
    node.parent()
        .filter(|parent| parent.kind() == SyntaxKind::COMMAND)
        .and_then(|parent| command_name(&parent))
        .is_some_and(|name| name.contains('_') || name.contains(':'))
}

/// Lay out an expl3 group inline when its body fits, otherwise as an indented
/// block. Padding follows [`expl_group_is_spaced`]; each argument breaks according
/// to its own body, independently of its siblings.
pub(super) fn lower_expl_group(
    node: &SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    cx: LowerCtx<'_>,
) -> Ir {
    let (open_ir, body, close_ir, spaced, has_comment) =
        match expl_group_pieces(node, open, close, cx) {
            ExplGroupPieces::Assembled(ir) => return ir,
            ExplGroupPieces::Pieces {
                open_ir,
                body,
                close_ir,
                spaced,
                forced,
            } => (open_ir, body, close_ir, spaced, forced),
        };
    // A forced block gets hard boundary separators *in-shape*, so
    // `propagate_breaks` marks the group `expand` and the printer lays the
    // body out in break mode — never the K&R hybrid of a flat-dispatched
    // concat (issue #97, `l3auxdata.dtx`). Otherwise the flat boundary is a
    // space (l3 house style) or nothing (tight); both break identically.
    let boundary = if has_comment {
        Ir::hard_line()
    } else if spaced {
        Ir::Line
    } else {
        Ir::SoftLine
    };
    Ir::group(Ir::concat([
        open_ir,
        Ir::indent(Ir::concat([boundary.clone(), body])),
        boundary,
        close_ir,
    ]))
}

/// Decompose an expl3 brace `{…}` (or optional `[…]`) group into the pieces
/// [`lower_expl_group`] and the trailing-hang branch in [`lower_expl_code`] share.
/// The empty-body and glued-lead-comment shapes have bespoke, already-stable
/// assembly, so they are returned pre-assembled as [`ExplGroupPieces::Assembled`];
/// every body-bearing group is returned as [`ExplGroupPieces::Pieces`] for the
/// caller to lay out flat, K&R, or Allman.
pub(super) fn expl_group_pieces(
    node: &SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    cx: LowerCtx<'_>,
) -> ExplGroupPieces {
    let mut open_ir = Ir::Nil;
    let mut close_ir = Ir::Nil;
    let mut body_elements: Vec<SyntaxElement> = Vec::new();
    for element in node.children_with_tokens() {
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
    // A comment opening the body (`{%`, the macro-code continuation idiom, with
    // inline whitespace tolerated) rides the opening bracket's line — it never
    // gets a line of its own. Inside a region the masked newline is inert
    // catcode-9 whitespace either way; this just keeps the authored shape.
    let mut lead_comment = Ir::Nil;
    {
        let mut i = 0;
        let mut spaced = false;
        while let Some(SyntaxElement::Token(t)) = body_elements.get(i) {
            match t.kind() {
                SyntaxKind::WHITESPACE => {
                    spaced = true;
                    i += 1;
                }
                SyntaxKind::COMMENT => {
                    lead_comment = if spaced {
                        Ir::concat([Ir::verbatim(" "), Ir::verbatim(t.text())])
                    } else {
                        Ir::verbatim(t.text())
                    };
                    body_elements.drain(..=i);
                    break;
                }
                _ => break,
            }
        }
    }
    // A body holding a `%` comment can never flatten: inline, everything after
    // the comment on the line — the closing bracket included — would be
    // swallowed into the comment on the next parse (`{ …% }`). The lead comment
    // glued to the opening bracket forces the broken form the same way.
    //
    // A docstrip guard (`%<…>`) or `.dtx` margin (`%`) is line-oriented the same
    // way, and worse: it is only recognized at line start, so flattening it into
    // `{ %<trace> … }` re-lexes it as an *ordinary* `%` comment that swallows the
    // rest of the line — braces included — unbalancing the enclosing group on the
    // next parse (issue #61). Force the broken form so each rides its own line,
    // where `lower_loose_token` pins it to column 0 and it stays a guard/margin.
    let has_lead_comment = !matches!(lead_comment, Ir::Nil);
    let has_comment = has_lead_comment
        || node
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::COMMENT | SyntaxKind::GUARD | SyntaxKind::DOC_MARGIN
                )
            });
    let open_ir = Ir::concat([open_ir, lead_comment]);
    let body = trim_trailing_break(trim_leading_break(lower_expl_code(
        body_elements.into_iter(),
        cx,
        Statements::Structural,
    )));
    if matches!(body, Ir::Nil) {
        // A glued lead comment owns the rest of its line, so an empty body
        // still breaks before the closing bracket (`{%…` + `}` on its own
        // line), never `{%…}` with the bracket swallowed.
        return ExplGroupPieces::Assembled(if has_lead_comment {
            Ir::concat([open_ir, Ir::hard_line(), close_ir])
        } else {
            Ir::concat([open_ir, close_ir])
        });
    }
    // A glued lead comment on a body-bearing group still forces the broken form
    // (it owns the rest of the opening line) and, more to the point, means the
    // three-candidate trailing-hang treatment must not apply — the flat/K&R
    // candidates would put content after the comment on the same line. Route it
    // through the forced-break path (`forced` covers both the lead comment and
    // any interior comment/guard/margin).
    ExplGroupPieces::Pieces {
        open_ir,
        body,
        close_ir,
        spaced: expl_group_is_spaced(node),
        forced: has_comment,
    }
}

/// Lower a statement-leading expl3 conditional (`\…:nTF {c} {T} {F}`, its branch
/// count recognized by [`expl3::conditional_branches`]) to the l3styleguide's
/// exploded shape (R4/R5): the head and any leading arguments on one line, then
/// each of the `n` trailing brace branches on its own line hung one indent step
/// (+2 relative to the head, so +6 inside a +4 body; a multi-line branch nests its
/// interior +8). The break is **unconditional** — width-independent, so it is
/// pass-stable: the exploded output re-parses to the same greedy `COMMAND` (brace
/// arguments attach across the inserted newlines) in statement position and
/// re-explodes identically. Each branch is a *soft* [`lower_expl_group`], so a
/// short branch stays `{ … }` inline on its line and a long one breaks internally.
///
/// An annotated branch keeps its comment: a trailing `%` rides the branch's own
/// line, an own-line one stays on its own line between branches.
///
/// Returns `None` unless the command's own last `n` argument children are `GROUP`s
/// with only trivia and comments beyond them — i.e. the branches actually attach to
/// the conditional. An `:NTF`/`:nNnTF` whose single-token (`N`/`V`/operator)
/// argument breaks greedy attachment leaves the branch groups on a following
/// sibling, which this node-local scan cannot see;
/// [`lower_expl_conditional_unit`] picks those up from the resolved call unit, and
/// [`expl_conditional_at`] tries the two in order.
pub(super) fn lower_expl_conditional(cmd: &SyntaxNode, cx: LowerCtx<'_>, n: usize) -> Option<Ir> {
    let children: Vec<SyntaxElement> = cmd.children_with_tokens().collect();
    let group_positions: Vec<usize> = children
        .iter()
        .enumerate()
        .filter(|(_, e)| e.as_node().is_some_and(|nd| nd.kind() == SyntaxKind::GROUP))
        .map(|(i, _)| i)
        .collect();
    if group_positions.len() < n {
        return None;
    }
    // The last `n` groups are the branches; everything before the first of them is
    // the head (control word, leading brace/operator args, and their trivia).
    let first_branch = group_positions[group_positions.len() - n];
    // The branches must be the *trailing* arguments: nothing but groups, trivia, and
    // comments may sit from the first branch onward, else this is not a clean
    // conditional call and the width path is safer. Comments are admitted (rather
    // than bailing to the width path) because annotated branches are ordinary l3
    // style — `{ \exp_not:n { equations~ } } % You might prefer \nobreakspace to ~`
    // — and the bail cost the whole exploded shape for one `%` (issue #101).
    for element in &children[first_branch..] {
        match element {
            SyntaxElement::Node(nd) if nd.kind() == SyntaxKind::GROUP => {}
            SyntaxElement::Token(t)
                if is_collapsible_trivia(t.kind()) || t.kind() == SyntaxKind::COMMENT => {}
            _ => return None,
        }
    }
    let branch_lines = expl_branch_lines(&children[first_branch..], cx)?;
    let head = trim_trailing_break(lower_expl_code(
        children[..first_branch].iter().cloned(),
        cx,
        Statements::Ignore,
    ));
    Some(Ir::concat(std::iter::once(head).chain(branch_lines)))
}

/// The exploded branch lines of a conditional: each `GROUP` in `tail` on its own
/// line hung one indent step, as a *soft* [`lower_expl_group`] so a short branch
/// stays `{ … }` inline on its line and a long one breaks internally.
///
/// `None` when `tail` holds anything but groups, collapsible trivia, and comments
/// — the branch list is then not clean and the width-driven path is safer. Shared
/// by [`lower_expl_conditional`] (branches attached to the head node) and
/// [`lower_expl_conditional_unit`] (branches greedy attachment gave to a sibling),
/// so the two spellings of the same layout cannot drift.
pub(super) fn expl_branch_lines(tail: &[SyntaxElement], cx: LowerCtx<'_>) -> Option<Vec<Ir>> {
    // Comments are admitted (rather than bailing to the width path) because
    // annotated branches are ordinary l3 style — `{ \exp_not:n { equations~ } } %
    // You might prefer \nobreakspace to ~` — and the bail cost the whole exploded
    // shape for one `%` (issue #101).
    for element in tail {
        match element {
            SyntaxElement::Node(nd) if nd.kind() == SyntaxKind::GROUP => {}
            SyntaxElement::Token(t)
                if is_collapsible_trivia(t.kind()) || t.kind() == SyntaxKind::COMMENT => {}
            _ => return None,
        }
    }
    let mut parts = Vec::new();
    // Trivia seen since the last emitted branch or comment: `gap` renders as the one
    // space before a trailing comment, `own_line` (a newline in that run) keeps an
    // own-line comment on its own line. Own-line-ness is a *preserved* predicate, so
    // reading it is trivia-invariant and stable in both directions — a trailing
    // comment re-parses trailing, an own-line one re-parses own-line. Relocating
    // either way would change its attachment.
    let mut gap = false;
    let mut own_line = false;
    for element in tail {
        match element {
            SyntaxElement::Node(nd) => {
                let group = lower_expl_group(nd, SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, cx);
                parts.push(Ir::indent(Ir::concat([Ir::hard_line(), group])));
                (gap, own_line) = (false, false);
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::COMMENT => {
                let comment = Ir::verbatim(t.text());
                parts.push(if own_line {
                    Ir::indent(Ir::concat([Ir::hard_line(), comment]))
                } else if gap {
                    Ir::concat([Ir::verbatim(" "), comment])
                } else {
                    comment
                });
                (gap, own_line) = (false, false);
            }
            SyntaxElement::Token(t) => {
                gap = true;
                own_line |= t.kind() == SyntaxKind::NEWLINE;
            }
        }
    }
    Some(parts)
}

/// The exploded form of the expl3 conditional headed by `elements[idx]`, plus
/// the index of the unit's last element (the caller resumes at `last + 1`).
///
/// Node-local: arity attachment gives a recognized conditional its branches as
/// the head's own trailing groups, so [`lower_expl_conditional`] covers every
/// resolvable shape — including a head whose *arity* is underivable while its
/// branch count is not (`:wTF`), when greed happened to attach the branches.
/// Returns `None` to leave the call on the width-driven path.
pub(super) fn expl_conditional_at(
    elements: &[SyntaxElement],
    idx: usize,
    cx: LowerCtx<'_>,
) -> Option<(Ir, usize)> {
    let node = elements.get(idx)?.as_node()?;
    if node.kind() != SyntaxKind::COMMAND {
        return None;
    }
    let n = expl3::conditional_branches(&command_name(node)?)?;
    let ir = lower_expl_conditional(node, cx, n)?;
    Some((ir, idx))
}
