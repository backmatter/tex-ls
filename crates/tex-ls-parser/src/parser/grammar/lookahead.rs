//! Recovery boundaries and the shared lookahead driver.
use super::*;

impl Parser<'_> {
    /// The environment twin of [`Self::delim_math_closes`]: whether the
    /// `\begin` at `open` is cut short by the closing brace of a group it sits
    /// *inside*, with no `\end` of its own reachable first.
    ///
    /// Brace groups are catcode-level structure while `\begin`/`\end` are only
    /// macros, so a `}` closing a group opened before the `\begin` always wins —
    /// the environment cannot span it. Package code leans on this constantly:
    /// the two halves sit in sibling groups
    /// (`\newcolumntype{w}[2]{>{\begin{lrbox}…}c<{\end{lrbox}…}}`, array.sty),
    /// in sibling macros (`\newcommand\BeginExample{…\begin{VerbatimOut}…}`
    /// paired with `\EndExample`, rotex.tex), or the `\begin` is prose in a
    /// message argument that never runs as structure
    /// (`\PackageError{amstex}{\string\begin{split} is not allowed…}`,
    /// amstex.sty — all issue #71). In each the `\begin` is an ordinary token:
    /// it opens no `ENVIRONMENT` and draws **no diagnostic**, the same shape
    /// gate `\[` already gets from [`Self::delim_math_closes`]. Without it the
    /// environment swallows the `}` and cascades into unmatched-brace noise
    /// that fails the whole file for the formatter.
    ///
    /// Only the *group boundary* suppresses the environment. A `\begin` that
    /// merely runs out of file still opens one, so the unclosed-environment
    /// diagnostic keeps firing on a genuinely forgotten `\end`. A `\end` of
    /// another name terminates the scan too, leaving the existing mismatch
    /// recovery in [`Self::finish_environment`] untouched. Does not consume.
    pub(super) fn environment_escapes_group(&self, open: usize) -> bool {
        // Only a group the `\begin` is *actually* inside can cut it short. At
        // the outer level there is no such brace, and a later unbalanced `}`
        // is somebody else's business — notably a `.dtx` doc-line
        // `\begin{macro}`, whose intervening `macrocode` chunks split
        // definitions across braces on purpose ([`Self::plain_braces`], only
        // populated once that chunk is entered). Without this guard the scan
        // reads those as its own boundary and unnests the whole doc layer.
        if !self.in_group() {
            return false;
        }
        // `.dtx` doc-margin lines are exempt, exactly as they are from the
        // expl3 carve-out ([`Self::expl_toggles`]): `\begin{macro}` and friends
        // are the *documentation* layer and must keep pairing across the
        // macrocode chunks between them. Those bodies routinely span code that
        // leaves a brace open on purpose — a `\iffalse}\fi` editor-balance
        // hack, a `` \char`} `` constant, a catcode-swapped region — which
        // leaves a group open for the rest of the file and would
        // otherwise unnest the whole doc layer behind it. (A paragraph-break
        // bound cannot stand in here: a blank `.dtx` doc line is still a `%`
        // margin, so it never reads as a `\par`.)
        //
        // The exemption is about *stranded* braces, so it lifts when the
        // enclosing group opened on a doc-margin line too: that `{` is the
        // documentation layer's own, locally visible, and the `\begin` really is
        // inside it. `% \def\deflist#1{\begin{list}…}` paired with
        // `% \def\enddeflist{\end{list}}` (theorem.dtx, issue #71) is the split
        // environment definition the gate exists for, merely written as doc
        // prose.
        if self.doc_margin_exempt(open) {
            return false;
        }
        // Both checks above are per-opener walk state, so they stay outside the
        // batch: a `\begin` they reject never consults it, and the batch stores
        // only what the *scan* decided.
        //
        // The `{name}` group of the `\begin` itself nests and unnests inside the
        // scan, so it resumes at the environment's own level. The only escape is
        // a `}` at that level, so the last `}` in the file bounds the scan
        // ([`Self::last_r_brace`]) — sound, but rarely effective, since a
        // `\begin{…}` opener's own name group carries one and pushes the index
        // toward EOF. That is why this gate needed the batch
        // ([`EnvGate`]): the bound alone left it
        // quadratic in the number of openers.
        self.gated_closer(open, &EnvGate, &self.env_batch).is_some()
    }

    /// The conditional twin of [`Self::delim_math_closes`]: whether the live
    /// opener at token `open` ([`Self::conditional_openers`]) has its own `\fi`
    /// reachable before a token that would end it.
    ///
    /// `\if…\else…\or…\fi` is not a construct the surface syntax guarantees. A
    /// `\fi` is routinely assembled elsewhere — `\def\stopit{\fi}`,
    /// `\expandafter\fi`, an `\iffalse…\fi` used to comment a region out — so
    /// after subtracting the `\newif` and `\ifthenelse` families 268 of 6205
    /// corpus files still have unbalanced opener/`\fi` counts. An opener that
    /// does not pair is therefore ordinary macro code: it stays a plain
    /// `COMMAND` with **no diagnostic**, exactly as a gated `$`/`\[`/`\begin`
    /// does (`AGENTS.md` decision #1). Does not consume.
    ///
    /// The anchors mirror the math gates — an unbalanced `}`, an `\end` not owed
    /// to an intervening `\begin`, a paragraph break, the macrocode chunk end,
    /// EOF — with two deliberate differences from
    /// [`Self::environment_escapes_group`]:
    ///
    /// - **EOF does not pair.** The environment gate keeps a run-out-of-file
    ///   `\begin` so `finish_environment` can still report an unclosed
    ///   environment. A conditional has no diagnostic to preserve, and an
    ///   unpaired `\if` is routine, so running out of file just demotes.
    /// - **No `.dtx` doc-margin exemption.** That exemption exists so the
    ///   documentation layer keeps pairing `\begin{macro}` across the macrocode
    ///   chunks between them. A conditional has no such split-across-chunks
    ///   story, and bounding the scan at `macrocode_end` is precisely what makes
    ///   the `\iffalse}\fi` editor-balance hack demote instead of swallowing the
    ///   chunk.
    ///
    /// A paragraph break anchors at the construct's own level only, so the ~11%
    /// of corpus conditionals that span a blank line demote and keep their
    /// pre-node layout. That keeps
    /// `CONDITIONAL` a within-paragraph construct: it can never straddle a
    /// `PARAGRAPH` boundary, so no paragraph nests inside one.
    ///
    /// The closer must be reachable at the opener's **own level of every nesting
    /// the parse itself recognizes** — braces, environments, and math alike — not
    /// just braces. A token scan that counts a `\fi` the parse will consume inside
    /// some other construct promises a pairing the walk cannot honor, and
    /// [`Self::conditional`] then runs past it looking for a closer that is gone:
    /// `ltboxes.dtx`'s `\else\@pboxswtrue $\vcenter \fi\fi\fi … \if@pboxsw
    /// \m@th$\fi` puts all three `\fi`s inside a `$…$`, and the construct ran over
    /// 160 lines and every `macrocode` chunk in between. Hence the `envs == 0`
    /// requirement on the closer and the math anchor.
    ///
    /// The guarantee this buys is **one-directional, and that is the direction
    /// that matters**: the walk never runs *past* the index returned here (it is
    /// bounded by it outright). The walk may still stop *earlier*, because this
    /// scan counts nested openers by name while the walk re-gates each one and may
    /// demote it — and a demoted opener's `\fi` is then a closer the walk reaches
    /// first. `\ifA \begin{center} \ifB \end{center} \fi \fi` is the shape: the
    /// scan counts `\ifB` as nested and picks the second `\fi`, while the walk
    /// demotes `\ifB` (whose own scan meets an unowed `\end`) and closes at the
    /// first, leaving the second a plain `COMMAND`. Lossless, and the node is still
    /// well formed — but it is why [`crate::ast::Conditional::closer`] is fallible
    /// and why nothing downstream may assume the two indices agree
    /// (`conditional_walk_may_close_before_the_located_fi`, `tests/parser.rs`).
    ///
    /// **Cost.** Verdicts are computed in *batches*: one forward scan seeded at the queried opener settles every
    /// same-frame opener it passes ([`Self::gate_batch`] under
    /// [`ConditionalGate`]), and the batch is memoized against the walk state
    /// it read
    /// ([`Self::conditional_batch`]) — so a run of top-level openers costs one
    /// O(n) pass where it used to cost one scan each. The scan stays bounded
    /// by the last `\fi`-flavored word in the file ([`Self::last_fi`], C0), so
    /// a file with none refuses without scanning at all. Openers the batch did
    /// not settle (they sat behind a brace at batch time) and queries under a
    /// changed walk state re-batch; every ordinary anchor still cuts a scan
    /// short, which is why real conditional-heavy packages (`biblatex.sty`,
    /// `latexrelease.sty`, `memoir.cls`) were within noise of the pre-node
    /// parser even before the batch.
    pub(super) fn conditional_closer(&self, open: usize) -> Option<usize> {
        self.gated_closer(open, &ConditionalGate, &self.conditional_batch)
    }

    /// The walk state a gate batch's scan reads — see [`WalkKey`].
    pub(super) fn walk_key(&self) -> WalkKey {
        WalkKey {
            macrocode_end: self.macrocode_end,
            in_def_body: self.in_def_body,
            in_group: self.in_group(),
            plain_braces: self.plain_braces_version,
            enclosing_math_is_dollar: self.enclosing_math_is_dollar(),
        }
    }

    /// Whether the innermost enclosing math body is dollar-delimited
    /// ([`Self::math_dollar`]). Outside math the answer is unused; `false` is
    /// the reading a bracket gate would take there anyway.
    pub(super) fn enclosing_math_is_dollar(&self) -> bool {
        self.math_dollar.last().copied().unwrap_or(false)
    }

    /// Whether the token at `i` is a `[` that **directly abuts** a command, and
    /// so claims the next `]` for itself when parsed — the bracket family's
    /// nested opener ([`TextBracketGate`]). The pre-batch scans derived this
    /// from a running `abuts_command` flag that every token kind but a control
    /// word or symbol cleared, trivia included, which is this test one token
    /// back.
    pub(super) fn bracket_abuts_command(&self, i: usize) -> bool {
        self.tokens[i].kind == SyntaxKind::L_BRACKET
            && i > 0
            && matches!(
                self.tokens[i - 1].kind,
                SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL
            )
    }

    /// The memoized front of [`Self::gate_batch`]: answer `open` from `memo`
    /// when the batch there was harvested under the current walk state and
    /// settled this opener, and otherwise re-batch from `open` and keep the
    /// result.
    ///
    /// One slot per gate is all the reuse there is *for verdicts*: the walk
    /// queries each opener once, in ascending order, under a state that is
    /// stable between re-batches. The slot's **storage** is reused further
    /// than that — a miss takes the stale map, clears it, and refills it, so a
    /// gate allocates about once per parse instead of once per re-batch. A
    /// cleared `HashMap` keeps its capacity, and the batches of one gate over
    /// one file are all much of a size.
    pub(super) fn gated_closer<P: GatePolicy>(
        &self,
        open: usize,
        policy: &P,
        memo: &std::cell::RefCell<Option<GateBatch>>,
    ) -> Option<usize> {
        // The C0 bound as an early-out: a file with no closer of this gate's
        // shape refuses without scanning at all.
        policy.last_closer(self)?;
        let key = self.walk_key();
        if let Some(batch) = memo.borrow().as_ref()
            && batch.key == key
            && let Some(&verdict) = batch.verdicts.get(&open)
        {
            return verdict;
        }
        // Recycle the superseded batch's map: its verdicts are stale (the key
        // missed, or it did not settle this opener), but its allocation is not.
        let mut verdicts =
            memo.borrow_mut()
                .take()
                .map_or_else(std::collections::HashMap::new, |stale| {
                    let mut map = stale.verdicts;
                    map.clear();
                    map
                });
        self.gate_batch(open, policy, &mut verdicts);
        let verdict = verdicts.get(&open).copied();
        debug_assert!(verdict.is_some(), "the batch must settle its own seed");
        *memo.borrow_mut() = Some(GateBatch { key, verdicts });
        verdict.flatten()
    }

    /// The unmemoized front, for a **single-entry** gate ([`DelimMathGate`],
    /// [`DollarGate`]): one that opens no nested entry, so its batch settles the
    /// seed and nothing else and there is no neighbor to save.
    ///
    /// A memo slot would not merely be idle here, it would be a hazard. The one
    /// re-query these gates see is a demoted `$$` whose second `$` re-enters
    /// [`Self::element`] as a fresh opener: same token index, same walk state,
    /// but `display: false` — a *different question*, which a slot keyed on the
    /// walk state alone would answer from the display verdict.
    ///
    /// With nothing to memoize and nothing but the seed to settle, the batch
    /// collects into a [`SeedVerdict`] rather than a map: these are the gates
    /// the walk queries most (`$` and `\[` are everywhere), and a per-query
    /// allocation for a single verdict is the whole cost of asking.
    pub(super) fn gate_verdict<P: GatePolicy>(&self, open: usize, policy: &P) -> Option<usize> {
        // The C0 bound as an early-out, as in [`Self::gated_closer`].
        policy.last_closer(self)?;
        let mut sink = SeedVerdict {
            seed: open,
            verdict: None,
        };
        self.gate_batch(open, policy, &mut sink);
        debug_assert!(sink.verdict.is_some(), "the batch must settle its own seed");
        sink.verdict.flatten()
    }

    /// The batched walk behind every shape gate: one forward scan seeded at
    /// `open` that also settles, as a by-product, every opener it passes in
    /// the seed's own brace frame — the exact verdict each one's own scan
    /// would have computed under the current walk state. Settled verdicts go
    /// to `verdicts`, whose two implementations decide how many are kept
    /// ([`VerdictSink`]); the scan itself never reads them back.
    ///
    /// The transform from a per-opener scan is possible because such a scan
    /// counts nested openers only at `depth == 0`: every opener this scan
    /// passes shares the seed's brace frame exactly, so `depth` is common to
    /// all of them, an entry's environment count relative to itself is
    /// `envs - envs_at_push`, and its nested-opener count is the number of
    /// stack entries above it — closer matching is pure LIFO.
    ///
    /// The one non-obvious rule: a refuted entry is **settled, never
    /// removed**. A per-opener scan counts nested openers *by name*
    /// ([`GatePolicy::opens_at`] membership) and never un-counts one, so a
    /// later closer must still be consumed by the refuted entry's slot. In
    /// `\ifA \begin{center} \ifB \end{center} \fi \fi`, the unowed `\end`
    /// refutes `\ifB` — but `\ifA`'s own scan still counts `\ifB` as nested
    /// and pairs with the *second* `\fi`. Popping `\ifB` at the `\end` would
    /// hand the first `\fi` to `\ifA`: a different verdict, a different tree.
    /// A closer that pops an already-settled entry records nothing. Every gate
    /// that joins this driver has the same never-un-counted countdown, so the
    /// rule is the driver's, not the conditional gate's.
    ///
    /// Per anchor, mirroring the pre-batch conditional scan token for token:
    /// - a closer at depth 0 pops the top entry; if it was still live, its
    ///   verdict is `Some` iff no `\begin`-opened environment stands in the
    ///   way (`envs == envs_at_push`, the old `envs == 0` restated — waived by
    ///   [`GatePolicy::CLOSER_NEEDS_ENV_BALANCE`]) and [`GatePolicy::pairs`]
    ///   accepts it;
    /// - a paragraph break (for a gate that anchors on one) or an unowed
    ///   `\end` refutes exactly the live
    ///   entries at their own level (`envs_at_push == envs`) — a contiguous
    ///   top suffix of the live stack, whose `envs_at_push` values are
    ///   non-decreasing and capped at `envs` by construction — and the `\end`
    ///   then decrements `envs` for the survivors;
    /// - math, an unbalanced `}` (under an enclosing group, or anywhere for a
    ///   gate reading [`StrayBrace::RefutesAlways`]), a `macrocode` frame, and
    ///   the end bound refute everything still live.
    ///
    /// The scan ends as soon as no live entry remains.
    pub(super) fn gate_batch<P: GatePolicy, S: VerdictSink>(
        &self,
        open: usize,
        policy: &P,
        verdicts: &mut S,
    ) {
        struct Entry {
            opener: usize,
            envs_at_push: usize,
            settled: bool,
        }
        /// Settle every live entry sitting at its own environment level: the
        /// level anchor at hand refutes exactly those.
        pub(super) fn settle_level<S: VerdictSink>(
            pending: &mut [Entry],
            live: &mut Vec<usize>,
            verdicts: &mut S,
            envs: usize,
        ) {
            while let Some(&idx) = live.last() {
                let entry = &mut pending[idx];
                if entry.envs_at_push != envs {
                    break;
                }
                entry.settled = true;
                verdicts.insert(entry.opener, None);
                live.pop();
            }
        }
        /// The [`Nesting::Interleaved`] twin of [`settle_level`]: settle the one
        /// entry that owns the innermost frame, and only when no environment
        /// stands inside it. The entries below are shielded by that frame and
        /// keep scanning — a settled entry keeps its frame, so a later closer
        /// still consumes it.
        pub(super) fn settle_innermost<S: VerdictSink>(
            pending: &mut [Entry],
            live: &mut Vec<usize>,
            verdicts: &mut S,
            envs: usize,
        ) {
            let Some(entry) = pending.last_mut() else {
                return;
            };
            if entry.settled || entry.envs_at_push != envs {
                return;
            }
            entry.settled = true;
            verdicts.insert(entry.opener, None);
            // An unsettled top of `pending` is the topmost live entry: an entry
            // leaves `live` only by being settled or by being popped from
            // `pending` outright.
            debug_assert_eq!(live.last().copied(), Some(pending.len() - 1));
            live.pop();
        }
        let mut pending = vec![Entry {
            opener: open,
            envs_at_push: 0,
            settled: false,
        }];
        // Indices into `pending` of the entries still awaiting a verdict,
        // ascending.
        let mut live = vec![0usize];
        let mut depth = 0usize;
        let mut envs = 0usize;
        let mut newlines = 0;
        // Inside a `$…$` region the entries read *through*: their openers and
        // closers stop counting until the matching `$`. Only
        // [`DollarAnchor::Transparent`] ever sets it.
        let mut transparent = false;
        let end = self
            .macrocode_end
            .unwrap_or(self.tokens.len())
            .min(self.tokens.len())
            .min(policy.last_closer(self).map_or(0, |last| last + 1));
        let mut i = open + 1;
        while i < end {
            self.tick_scan();
            let t = &self.tokens[i];
            match t.kind {
                SyntaxKind::NEWLINE => {
                    newlines += 1;
                    // A break anchors at an entry's *own* level only,
                    // `depth == 0 && envs == envs_at_push`. Deeper than that it
                    // is ordinary body trivia, and a gate stricter than the
                    // parse it guards drops the node: a display equation built
                    // out of `tikzpicture` cells (`\[ \begin{array}…
                    // \begin{tikzpicture}<blank line>… \]`, issue #70) lost its
                    // math node and reported its own `\]` as unmatched. The
                    // bracket family is the exception, and for the same reason:
                    // `optional` bails at a break wherever the cursor stands
                    // ([`ParagraphAnchor::AnyDepth`]).
                    if newlines >= BLANK_LINE_NEWLINES
                        && match P::PARAGRAPH_ANCHOR {
                            ParagraphAnchor::None => false,
                            ParagraphAnchor::OwnLevel => depth == 0,
                            ParagraphAnchor::AnyDepth => true,
                        }
                    {
                        if P::PARAGRAPH_ANCHOR == ParagraphAnchor::AnyDepth {
                            break;
                        }
                        // Under interleaved nesting the break is seen only by
                        // the entry owning the innermost frame: every entry
                        // below has that frame on its own stack, so its
                        // `stack.is_empty()` test cannot fire ([`Nesting`]).
                        match P::NESTING {
                            Nesting::Counted => {
                                settle_level(&mut pending, &mut live, verdicts, envs);
                            }
                            Nesting::Interleaved => {
                                settle_innermost(&mut pending, &mut live, verdicts, envs);
                            }
                        }
                        if live.is_empty() {
                            return;
                        }
                    }
                    i += 1;
                    continue;
                }
                // A `.dtx` doc margin floats like whitespace — it is one byte of
                // layout, not content — so a margin-only line `%\n%\n` still reads
                // as the blank line its two `NEWLINE`s make it.
                SyntaxKind::WHITESPACE | SyntaxKind::DOC_MARGIN => {
                    i += 1;
                    continue;
                }
                // A docstrip guard is content *on its line*, and a line docstrip
                // deletes outright when it strips the file, so `%<*dtx>` between
                // two lines does not part them (issue #71): it breaks the newline
                // run without being a newline. That is
                // [`TriviaScan::saw_blank_line_outside_guards`], the considered
                // model, and every gate reads it — see the type-level note on
                // [`GatePolicy`].
                SyntaxKind::GUARD => {
                    newlines = 0;
                    i += 1;
                    continue;
                }
                // Math swallows whatever it spans, and this scan does not model
                // the `$`/`\[`/`\(` shape gates that decide whether a delimiter
                // opens any. Rather than re-derive them, a gate that lives in
                // text refuses at math *starting*: a construct whose closer sits
                // behind such a delimiter stays a plain command. A conservative
                // false negative, per the parser's standing preference for them.
                // The demotion gate reverses the direction and a gate that lives
                // *inside* math reverses the side ([`MathAnchor`]). A `$` is both
                // sides at once, so it anchors for either — unless it opens a
                // region the gate reads *through* ([`DollarAnchor`]).
                SyntaxKind::DOLLAR
                    if depth == 0 && policy.dollar_anchor() == DollarAnchor::Refutes =>
                {
                    break;
                }
                SyntaxKind::DOLLAR
                    if depth == 0 && policy.dollar_anchor() == DollarAnchor::Transparent =>
                {
                    transparent = !transparent;
                }
                SyntaxKind::CONTROL_SYMBOL
                    if (depth == 0 || P::ANCHORS_AT_ANY_DEPTH)
                        && P::MATH_ANCHOR.anchors(t.text.as_str()) =>
                {
                    break;
                }
                SyntaxKind::L_BRACE if !self.plain_braces.contains(&i) => depth += 1,
                SyntaxKind::R_BRACE if !self.plain_braces.contains(&i) => {
                    if depth == 0 {
                        // A `}` closing a group opened before the opener always
                        // wins: braces are catcode structure while the gated
                        // delimiters are only macros. Whether one with *no* such
                        // group behind it (the walk is at the outer level) means anything,
                        // and what it means at all, is the gate's own call
                        // ([`StrayBrace`]).
                        match P::STRAY_BRACE {
                            StrayBrace::RefutesInGroup if self.in_group() => break,
                            StrayBrace::ClosesInGroup if self.in_group() => {
                                // Every live entry escapes at the same brace:
                                // `depth` is common to the whole frame, so each
                                // one's own scan would reach this `}` at its own
                                // depth 0 too.
                                for &idx in &live {
                                    verdicts.insert(pending[idx].opener, Some(i));
                                }
                                return;
                            }
                            StrayBrace::RefutesAlways => break,
                            _ => {}
                        }
                    } else {
                        depth -= 1;
                    }
                }
                // Any token at the entries' own brace level may be a delimiter:
                // the pairing gates close on a `CONTROL_WORD`, but the math
                // gates close on a `DOLLAR` and a `CONTROL_SYMBOL`. Every policy
                // tests the kind inside its own predicate, so asking wider costs
                // the narrow ones nothing but the call.
                _ => {
                    if !transparent && depth == 0 && policy.opens_at(self, i) {
                        // A gate whose openers are `\begin`s counts this one
                        // before pushing, so the entry's own environment is not
                        // in its `envs_at_push` — its per-opener scan starts one
                        // token past the `\begin` and never saw it either.
                        if P::OPENER_IS_ENV_BEGIN {
                            envs += 1;
                        }
                        live.push(pending.len());
                        pending.push(Entry {
                            opener: i,
                            envs_at_push: envs,
                            settled: false,
                        });
                    } else if !transparent && depth == 0 && policy.closes_at(self, i) {
                        let entry = pending
                            .pop()
                            .expect("a live entry remains, so pending is non-empty");
                        // Under interleaved nesting the closer pops the
                        // *innermost frame*, so an environment opened since this
                        // entry is a frame mismatch — and one every outer entry
                        // sees too, since this entry's frame is their innermost
                        // one. It refuses the whole scan rather than one entry
                        // ([`Nesting`]). This entry is out of `pending` already,
                        // so it settles itself here and the trailing refusal
                        // covers the rest.
                        if P::NESTING == Nesting::Interleaved && envs != entry.envs_at_push {
                            if !entry.settled {
                                live.pop();
                                verdicts.insert(entry.opener, None);
                            }
                            break;
                        }
                        if !entry.settled {
                            live.pop();
                            // `envs == envs_at_push` for the same reason as
                            // `depth == 0`: a closer inside an environment the
                            // construct opened is consumed by that
                            // environment's body, so it is not a closer the
                            // walk can reach — unless the closer is a *math
                            // delimiter*, which ends the body wherever it sits
                            // ([`GatePolicy::CLOSER_NEEDS_ENV_BALANCE`]).
                            let balanced =
                                !P::CLOSER_NEEDS_ENV_BALANCE || envs == entry.envs_at_push;
                            let paired = balanced && policy.pairs(self, entry.opener, i);
                            verdicts.insert(entry.opener, paired.then_some(i));
                            if live.is_empty() {
                                return;
                            }
                        }
                    } else if t.kind == SyntaxKind::CONTROL_WORD
                        && (depth == 0 || P::ANCHORS_AT_ANY_DEPTH)
                        && (P::ENV_ANCHOR_IN_MACRO_CODE || !self.in_macro_code(i))
                    {
                        // In a definition body or an expl3 region `\begin`/`\end`
                        // are plain commands that need not pair, so neither
                        // anchors nor nests there (issues #45/#60) — bar the one
                        // gate whose pre-batch scan never carried the filter
                        // ([`GatePolicy::ENV_ANCHOR_IN_MACRO_CODE`]).
                        if self.env_begin_at(i) {
                            // A `macrocode` chunk is a hard boundary in both
                            // directions: docstrip is line-oriented, so the code
                            // layer and the documentation layer around it are
                            // different files as far as TeX is concerned. Nothing
                            // is gained by pairing across one, and a `.dtx` doc
                            // layer that does — `%<latexrelease>` guarded
                            // `\if#1b\vbox \else…` blocks in `ltboxes.dtx` — runs
                            // the construct over every chunk in between, stranding
                            // the cursor past `macrocode_end` for every
                            // chunk-bounded scan downstream. (The other direction
                            // is already bounded: a conditional *inside* a chunk
                            // scans only to `macrocode_end`.) The math gates opt
                            // out ([`GatePolicy::MACROCODE_FRAME_ANCHORS`]).
                            if P::MACROCODE_FRAME_ANCHORS
                                && peek_begin_name(self.tokens, i).is_some_and(|n| {
                                    matches!(n.as_ref(), "macrocode" | "macrocode*")
                                })
                            {
                                break;
                            }
                            // An optional never legitimately spans an
                            // environment, so for the bracket family either half
                            // is a runaway `[` and there is nothing to count
                            // ([`EnvAnchor`]).
                            if P::ENV_ANCHOR == EnvAnchor::Refutes {
                                break;
                            }
                            envs += 1;
                        } else if self.env_end_at(i) {
                            if P::ENV_ANCHOR == EnvAnchor::Refutes {
                                break;
                            }
                            if P::ENV_END_UNWINDS_OPENERS {
                                let end_name = peek_end_name(self.tokens, i);
                                let mut matched = false;
                                while let Some(entry) = pending.pop() {
                                    envs = entry.envs_at_push;
                                    if !entry.settled {
                                        let live_entry = live.pop();
                                        debug_assert_eq!(live_entry, Some(pending.len()));
                                        verdicts.insert(entry.opener, None);
                                    }
                                    if peek_begin_name(self.tokens, entry.opener).as_deref()
                                        == end_name.as_deref()
                                    {
                                        matched = true;
                                        break;
                                    }
                                }
                                // A mismatched closer is the same recovery
                                // anchor every per-opener scan used. A named
                                // match may unwind several nested environments,
                                // exactly as `finish_environment` does.
                                if !matched || live.is_empty() {
                                    for &idx in &live {
                                        verdicts.insert(pending[idx].opener, None);
                                    }
                                    return;
                                }
                                i += 1;
                                newlines = 0;
                                continue;
                            }
                            match P::NESTING {
                                // The `\end` must find an environment innermost.
                                // It does not when the entry on top of `pending`
                                // was pushed at the current `envs`: that entry's
                                // frame is in the way, for it and for every entry
                                // below it alike, so the mismatch refuses the
                                // whole scan. A settled entry still holds its
                                // frame ([`Nesting`]).
                                Nesting::Interleaved => {
                                    if pending.last().is_some_and(|e| e.envs_at_push == envs) {
                                        break;
                                    }
                                }
                                Nesting::Counted => {
                                    settle_level(&mut pending, &mut live, verdicts, envs);
                                    if live.is_empty() {
                                        return;
                                    }
                                }
                            }
                            // A survivor has `envs_at_push < envs`, so the
                            // decrement cannot underflow.
                            envs -= 1;
                        }
                    }
                }
            }
            newlines = 0;
            i += 1;
        }
        // Global refusals — math, an unbalanced `}`, a `macrocode` frame, the
        // end bound: everything still live demotes.
        for &idx in &live {
            verdicts.insert(pending[idx].opener, None);
        }
    }

    /// The token index closing the environment-alias opener at `open`, or `None`
    /// when it does not pair — in which case the opener stays a plain `COMMAND`
    /// with **no diagnostic**, like a gated `$`/`\[`/`\begin`.
    ///
    /// This is a **positive** gate, transcribed from [`Self::conditional_closer`]
    /// rather than from [`Self::environment_escapes_group`]. The `\begin` gate is a
    /// *demotion* gate on a construct that pairs by default and carries an
    /// unclosed-environment diagnostic worth preserving. An alias opener is a bare
    /// control word with no `{name}` corroborating it and no diagnostic to keep, so
    /// "pair unless refuted" would be far too optimistic: it must be refused unless
    /// its closer is positively located, and the walk is then bounded by that index.
    ///
    /// Requirements the driver ([`Self::gate_batch`]) carries for it, shared
    /// with the sibling gates:
    ///
    /// - **Brace level.** A `}` closing a group opened before the opener always
    ///   wins — braces are catcode structure, an alias is only a macro (issue #71).
    /// - **`envs == 0`.** A closer inside an environment the alias opened is
    ///   consumed by that environment's body, so the walk cannot reach it.
    /// - **Math refuses.** The scan does not model the `$`/`\[`/`\(` shape gates,
    ///   so rather than re-derive them it declines behind one.
    /// - **`macrocode` bounds it both ways**, as for conditionals.
    ///
    /// What is this gate's own is in [`AliasGate`]: no paragraph anchor, and a
    /// closer that must name the opener's target.
    ///
    /// Batched and memoized like the conditional gate — and here the memo was
    /// load-bearing before the batch existed, since the caller asks twice
    /// ([`Self::alias_batch`]).
    pub(super) fn alias_closer(&self, open: usize) -> Option<usize> {
        // Total in `open`: [`Self::starts_block_env`] asks about any index, and
        // the driver would otherwise seed an entry for a token that opens
        // nothing.
        self.alias_openers.get(&open)?;
        self.gated_closer(open, &AliasGate, &self.alias_batch)
    }

    /// The environment the token at `idx` closes, under *either* spelling: a
    /// closer alias (`\eea`), or the literal `\end{X}` an alias-opened `X` pairs
    /// with (issue #117). `None` when it closes neither.
    ///
    /// The two maps stay separate ([`Self::literal_alias_closers`]) because the
    /// consumers differ; this is the one place that reads them as one. The
    /// literal arm re-tests [`Self::env_end_at`] because the pre-scan's index is
    /// built from the looser `peek_end_name` — a `\end` the walk would treat as
    /// a plain command must not become an `END` here, or the `NAME_GROUP`
    /// [`Self::alias_environment`] then asks for is not there.
    pub(super) fn closer_target(&self, idx: usize) -> Option<&str> {
        if let Some(target) = self.alias_closers.get(&idx) {
            return Some(target.as_str());
        }
        let target = self.literal_alias_closers.get(&idx)?;
        self.env_end_at(idx).then_some(target.as_str())
    }

    /// Whether the closer at `idx` is spelled out as `\end{X}` rather than as a
    /// closer alias — the one thing the two [`Self::closer_target`] arms are
    /// consumed differently for.
    pub(super) fn closer_is_literal(&self, idx: usize) -> bool {
        !self.alias_closers.contains_key(&idx) && self.literal_alias_closers.contains_key(&idx)
    }
}
