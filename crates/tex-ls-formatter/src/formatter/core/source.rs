//! Document-wide preparation before CST lowering. Source facts are built once.
use super::ranges::ranges_overlap;
use rowan::{TextRange, TextSize};
use std::cell::RefCell;
use std::collections::HashMap;
use tex_ls_parser::ast::{AstNode, Environment, command_name};
use tex_ls_parser::directives;
use tex_ls_parser::parser::is_def_prefix_command;
use tex_ls_parser::parser::lexer::{ExplToggle, expl_toggle};
use tex_ls_parser::semantic::context::TokenListContexts;
use tex_ls_parser::semantic::{SignatureDb, scan_definitions};
use tex_ls_parser::syntax::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, is_collapsible_trivia,
};

pub(super) struct PreparedSource {
    pub(super) signatures: SignatureDb,
    pub(super) token_lists: TokenListContexts,
    pub(super) expl3_regions: Vec<TextRange>,
    pub(super) suppressions: directives::Suppressions,
    pub(super) uncertain: UncertainOwnership,
    pub(super) dtx_reflow_cache: RefCell<HashMap<SyntaxNode, bool>>,
    pub(super) is_dtx: bool,
}

impl PreparedSource {
    pub(super) fn new(root: &SyntaxNode, external: &SignatureDb, has_uncertain: bool) -> Self {
        let mut signatures = external.clone();
        signatures.merge_from(&scan_definitions(root), None);
        let expl3_regions = expl3_regions(root);
        let suppressions = directives::Suppressions::build(root);
        let token_lists = TokenListContexts::new(root);
        let uncertain = if has_uncertain {
            UncertainOwnership::build(root)
        } else {
            UncertainOwnership::default()
        };
        Self {
            signatures,
            expl3_regions,
            suppressions,
            token_lists,
            uncertain,
            dtx_reflow_cache: RefCell::new(HashMap::new()),
            is_dtx: root
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .any(|t| matches!(t.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD)),
        }
    }
}

/// The nearest preceding sibling *element* of `node`, skipping `WHITESPACE`/`NEWLINE`
/// trivia. Used by the definee gate to find the command a toggle would be the
/// definee of.
fn prev_nontrivia_element(node: &SyntaxNode) -> Option<SyntaxElement> {
    let mut prev = node.prev_sibling_or_token();
    while let Some(el) = prev {
        if let SyntaxElement::Token(t) = &el
            && is_collapsible_trivia(t.kind())
        {
            prev = el.prev_sibling_or_token();
            continue;
        }
        return Some(el);
    }
    None
}

/// Whether an expl3 toggle `CONTROL_WORD` sits at *top-level statement* position, so
/// TeX actually executes it and switches catcodes at load. Two shapes are rejected
/// (issue #69), both false positives of the name-only model:
///
/// - **Definee position:** the toggle command's immediately-preceding non-trivia
///   sibling is a `\def`/`\let`-family primitive, so the toggle is the control
///   sequence being defined (`\protected\def\ProvidesExplPackage{…}`), never run.
/// - **Nested in a group / definition body:** an ancestor of the toggle's command is
///   a `GROUP` or `OPTIONAL`, so the toggle is tokenized into a replacement text and
///   executed — if ever — only when that macro runs, not at load.
///
/// The lexer's letter mode keeps the naive name-only model: mis-lexing a name only
/// splits CST tokens (lossless, cosmetic); only mis-*owning* layout rewrites tex-ls.
fn toggle_is_top_level(token: &SyntaxToken) -> bool {
    let Some(command) = token.parent() else {
        return true;
    };
    if command.kind() != SyntaxKind::COMMAND {
        // Not the head of a command node — leave the naive model in charge.
        return true;
    }
    // Rule 1: nested inside an attached group or definition body.
    for ancestor in command.ancestors().skip(1) {
        match ancestor.kind() {
            SyntaxKind::GROUP | SyntaxKind::OPTIONAL => return false,
            SyntaxKind::ROOT => break,
            _ => {}
        }
    }
    // Rule 2: definee of a `\def`/`\let`-family command. `command_name` strips the
    // leading `\`, so reconstruct it for the shared curated set.
    if let Some(SyntaxElement::Node(prev)) = prev_nontrivia_element(&command)
        && prev.kind() == SyntaxKind::COMMAND
        && let Some(name) = command_name(&prev)
        && (is_def_prefix_command(&format!("\\{name}"))
            || matches!(name.as_str(), "let" | "futurelet"))
    {
        return false;
    }
    true
}

/// The byte ranges of the document's expl3 regions, in document order. A region runs
/// from an opener (`\ExplSyntaxOn`, or a `\ProvidesExpl*` declaration, which opens
/// expl3 for the rest of the file) through the matching `\ExplSyntaxOff` (inclusive
/// of both toggle commands), or to end of input when unclosed. The toggle *name set*
/// is read from [`expl_toggle`] — the same fixed set the lexer flips its `expl_syntax`
/// flag on — but the formatter additionally applies a *positional* gate
/// (`toggle_is_top_level`): only a top-level toggle opens a formatter-owned region.
/// The name set stays shared so the two never drift; positional layout ownership
/// remains formatter-specific.
///
/// Matches only [`SyntaxKind::CONTROL_WORD`] tokens, so a `\ExplSyntaxOn` written
/// inside `\verb`/a comment (a `VERB`/`COMMENT` token, never a `CONTROL_WORD`) is
/// not a toggle, exactly as in the lexer. The CST is untouched.
///
/// `pub` so the linter (in the `tex-ls` crate) shares the *same* region
/// computation (the `unclosed-math-delimiter` rule suppresses inside expl3
/// code), keeping the formatter and linter from drifting on what counts as an
/// expl3 region.
pub fn expl3_regions(root: &SyntaxNode) -> Vec<TextRange> {
    let mut regions: Vec<TextRange> = Vec::new();
    let mut open: Option<TextSize> = None;
    for token in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::CONTROL_WORD)
    {
        // Positional gate: a gated-out toggle is skipped entirely (`On` and `Off`
        // alike), so a stored or definee toggle neither opens nor closes a region.
        let toggle = match expl_toggle(token.text()) {
            Some(t) if toggle_is_top_level(&token) => t,
            _ => continue,
        };
        match toggle {
            // A redundant inner `\ExplSyntaxOn` does not restart the region (the
            // lexer's flag is an idempotent set-true).
            ExplToggle::On if open.is_none() => open = Some(token.text_range().start()),
            ExplToggle::On => {}
            ExplToggle::Off => {
                if let Some(start) = open.take() {
                    regions.push(TextRange::new(start, token.text_range().end()));
                }
                // A stray `\ExplSyntaxOff` with no open region is ignored (toggling
                // an already-false flag is a no-op), matching the lexer.
            }
        }
    }
    if let Some(start) = open.take() {
        // An unclosed region runs to end of input (the lexer's flag simply stays
        // true to EOF).
        regions.push(TextRange::new(start, root.text_range().end()));
    }
    let regions = intersect_macrocode_bodies(root, regions);
    let regions = subtract_doc_margin_lines(root, regions);
    subtract_guarded_line_runs(root, regions)
}

/// In a `.dtx` (any `DOC_MARGIN` token present), restrict the expl3 regions to
/// `macrocode`/`macrocode*` chunk *bodies* — the only lines docstrip extracts as
/// code. The margin subtraction below removes doc lines that carry their `%` in
/// column 0, but the doc part is not obliged to margin every line (a stray
/// `␣%` comment, issue #58): any non-chunk line is documentation regardless of
/// its first column, so relayout must never own it. A no-op for non-`.dtx`
/// documents, where an unmargined `\begin{macrocode}` is just an ordinary
/// user environment.
fn intersect_macrocode_bodies(root: &SyntaxNode, regions: Vec<TextRange>) -> Vec<TextRange> {
    if regions.is_empty()
        || !root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::DOC_MARGIN)
    {
        return regions;
    }
    // `macrocode` never nests, so the bodies are disjoint and in document order.
    // A body runs from the end of the `\begin` frame to the start of the `\end`
    // frame's `\end` (or the chunk end when the frame is missing at EOF); the
    // frame line's own margin/whitespace inside that span is removed by the
    // margin subtraction pass.
    let bodies: Vec<TextRange> = root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ENVIRONMENT)
        .filter_map(Environment::cast)
        .filter(|e| matches!(e.name().as_deref(), Some("macrocode" | "macrocode*")))
        .filter_map(|e| {
            let start = e.begin()?.syntax().text_range().end();
            let end = e
                .end()
                .map(|end| end.syntax().text_range().start())
                .unwrap_or_else(|| e.syntax().text_range().end());
            (start < end).then_some(TextRange::new(start, end))
        })
        .collect();
    let mut out = Vec::with_capacity(regions.len());
    let mut b = bodies.iter().peekable();
    for region in regions {
        while let Some(&&body) = b.peek() {
            if body.end() <= region.start() {
                b.next();
                continue;
            }
            if body.start() >= region.end() {
                break;
            }
            let start = region.start().max(body.start());
            let end = region.end().min(body.end());
            if start < end {
                out.push(TextRange::new(start, end));
            }
            if body.end() >= region.end() {
                break;
            }
            b.next();
        }
    }
    out
}

/// Remove every `.dtx` documentation line from the expl3 regions. In a `.dtx`,
/// an `\ExplSyntaxOn` in one `macrocode` chunk is regularly matched by the
/// `\ExplSyntaxOff` several chunks later, so the lexical region spans the
/// documentation in between — margined doc lines and the `%    \end{macrocode}`
/// frame lines themselves. Those lines are *not* expl3 code (at package-load
/// time they are `%` comments; the margin must stay in column 0), so relayout
/// must never own them: subtract each doc-margined line (its `DOC_MARGIN` opens
/// the line by construction — the lexer emits margins at line start only)
/// through its terminating newline. Code lines inside chunk bodies carry no
/// margin and stay in-region. A no-op for non-`.dtx` documents (no `DOC_MARGIN`
/// tokens, and the common all-code case short-circuits on the first hole scan).
fn subtract_doc_margin_lines(root: &SyntaxNode, regions: Vec<TextRange>) -> Vec<TextRange> {
    if regions.is_empty() {
        return regions;
    }
    // One pass over the leaves: a DOC_MARGIN opens a hole, the next NEWLINE
    // (inclusive) closes it.
    let mut holes: Vec<TextRange> = Vec::new();
    let mut hole_start: Option<TextSize> = None;
    for token in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        match token.kind() {
            SyntaxKind::DOC_MARGIN => {
                hole_start.get_or_insert(token.text_range().start());
            }
            SyntaxKind::NEWLINE => {
                if let Some(start) = hole_start.take() {
                    holes.push(TextRange::new(start, token.text_range().end()));
                }
            }
            _ => {}
        }
    }
    if let Some(start) = hole_start.take() {
        holes.push(TextRange::new(start, root.text_range().end()));
    }
    subtract_holes(regions, &holes)
}

/// Remove every maximal run of two-or-more consecutive docstrip-guarded lines
/// (`%<…>…`) from the expl3 regions. A fully-guarded chunk (a docstrip release
/// block, `%<latexrelease>…%<latexrelease>\EndIncludeInRelease`, or any run of
/// `%<*name>`/`%<name>` lines) pins **every** line to column 0 by its guard, so
/// the expl3 block layout cannot own it: reflowing indents a delimiter or wraps a
/// long line off its guard, stranding code onto an *unguarded* line — a docstrip
/// semantics change (the code leaves its guard's scope) that also re-parses
/// differently, so the layout never reaches a fixed point (issue #72, latex2e
/// `ltcmdhooks.dtx`). Handing such a run to the generic (non-expl3) lowering
/// preserves it verbatim, which is idempotent and semantics-preserving.
///
/// The two-line threshold keeps an *isolated* guarded line (a lone `%<trace> …`
/// statement amid unguarded code, or a lone `%<*name>` block marker) in-region,
/// where it lays out on its own line as before — such a line's content stays on
/// its line and never strands. A no-op for non-`.dtx` documents (no `GUARD`
/// tokens).
fn subtract_guarded_line_runs(root: &SyntaxNode, regions: Vec<TextRange>) -> Vec<TextRange> {
    if regions.is_empty() {
        return regions;
    }
    // One pass over the leaves grouping guard-led source lines into runs: a line
    // is guard-led when its first token is a `GUARD`. A run of two or more
    // adjacent guard-led lines becomes one hole spanning them (each line reaches
    // through its terminating newline).
    let mut holes: Vec<TextRange> = Vec::new();
    let mut at_line_start = true;
    let mut line_is_guard = false;
    let mut cur_line_start = TextSize::new(0);
    // The run of consecutive guard-led lines in progress: (start, end, line count).
    let mut run: Option<(TextSize, TextSize, usize)> = None;
    fn flush(run: &mut Option<(TextSize, TextSize, usize)>, holes: &mut Vec<TextRange>) {
        if let Some((start, end, count)) = run.take()
            && count >= 2
        {
            holes.push(TextRange::new(start, end));
        }
    }
    for token in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if at_line_start {
            cur_line_start = token.text_range().start();
            line_is_guard = token.kind() == SyntaxKind::GUARD;
            at_line_start = false;
        }
        if token.kind() == SyntaxKind::NEWLINE {
            let line_end = token.text_range().end();
            if line_is_guard {
                match run {
                    // Adjacent to the run in progress (its end abuts this line's
                    // start): extend it.
                    Some((start, end, count)) if end == cur_line_start => {
                        run = Some((start, line_end, count + 1));
                    }
                    // A gap (a non-guarded line intervened) or the first guarded
                    // line: close any prior run and open a fresh one.
                    _ => {
                        flush(&mut run, &mut holes);
                        run = Some((cur_line_start, line_end, 1));
                    }
                }
            } else {
                flush(&mut run, &mut holes);
            }
            at_line_start = true;
        }
    }
    flush(&mut run, &mut holes);
    subtract_holes(regions, &holes)
}

/// Interval subtraction of `holes` from `regions`. Both lists must be sorted and
/// pairwise disjoint; the output is too (the region binary-search lookups rely on
/// that). Shared by the two region-subtraction passes above.
fn subtract_holes(regions: Vec<TextRange>, holes: &[TextRange]) -> Vec<TextRange> {
    if holes.is_empty() {
        return regions;
    }
    let mut out = Vec::with_capacity(regions.len());
    let mut h = holes.iter().peekable();
    for region in regions {
        let mut cursor = region.start();
        while let Some(&&hole) = h.peek() {
            if hole.end() <= cursor {
                h.next();
                continue;
            }
            if hole.start() >= region.end() {
                break;
            }
            if hole.start() > cursor {
                out.push(TextRange::new(cursor, hole.start()));
            }
            cursor = cursor.max(hole.end());
            if cursor >= region.end() {
                break;
            }
            h.next();
        }
        if cursor < region.end() {
            out.push(TextRange::new(cursor, region.end()));
        }
    }
    out
}

// A single pass indexes unknown tokens and their exact adjacent trivia. Queries
// during lowering use sorted ranges, avoiding a subtree scan at every ancestor.
#[derive(Default)]
pub(super) struct UncertainOwnership {
    tokens: Vec<TextRange>,
    protected: Vec<TextRange>,
    physical: Vec<TextRange>,
}

impl UncertainOwnership {
    fn build(root: &SyntaxNode) -> Self {
        let mut ownership = Self::default();
        let mut line_start = root.text_range().start();
        let mut framed_line = false;
        for token in root
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
        {
            if matches!(token.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD) {
                framed_line = true;
            }
            if token.kind() == SyntaxKind::NEWLINE {
                if framed_line {
                    ownership
                        .physical
                        .push(TextRange::new(line_start, token.text_range().end()));
                }
                line_start = token.text_range().end();
                framed_line = false;
            }
            if token.kind() != SyntaxKind::UNCERTAIN {
                continue;
            }
            let range = token.text_range();
            ownership.tokens.push(range);
            let mut start = range.start();
            let mut previous = token.prev_token();
            while let Some(token) = previous.filter(|token| is_collapsible_trivia(token.kind())) {
                start = token.text_range().start();
                previous = token.prev_token();
            }
            let mut end = range.end();
            let mut next = token.next_token();
            while let Some(token) = next.filter(|token| is_collapsible_trivia(token.kind())) {
                end = token.text_range().end();
                next = token.next_token();
            }
            if let Some(previous) = ownership
                .protected
                .last_mut()
                .filter(|previous| previous.end() >= start)
            {
                *previous = TextRange::new(previous.start(), previous.end().max(end));
            } else {
                ownership.protected.push(TextRange::new(start, end));
            }
        }
        if framed_line {
            ownership
                .physical
                .push(TextRange::new(line_start, root.text_range().end()));
        }
        ownership
    }

    pub(super) fn contains_token(&self, range: TextRange) -> bool {
        let index = self
            .tokens
            .partition_point(|token| token.start() < range.start());
        self.tokens
            .get(index)
            .is_some_and(|token| range.contains_range(*token))
    }

    pub(super) fn has_physical_framing(&self, range: TextRange) -> bool {
        let index = self
            .physical
            .partition_point(|line| line.end() <= range.start());
        self.physical
            .get(index)
            .is_some_and(|line| ranges_overlap(*line, range))
    }

    pub(super) fn protects(&self, range: TextRange) -> bool {
        let index = self
            .protected
            .partition_point(|protected| protected.end() <= range.start());
        self.protected
            .get(index)
            .is_some_and(|protected| protected.contains_range(range))
    }
}
