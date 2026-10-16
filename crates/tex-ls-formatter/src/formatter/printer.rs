//! The layout engine: walks an [`Ir`] tree and renders it to a string, deciding
//! for each [`Ir::Group`] whether it fits flat on the current line or must break.
//!
//! This is a language-agnostic Wadler/Prettier-style layout engine. Alongside
//! ordinary greedy fills it supports source-break-aware preferred fills whose
//! gaps are selected by a global lexicographic cost.

use super::ir::Ir;
use super::style::FormatStyle;
use std::rc::Rc;

#[cfg(test)]
thread_local! {
    static FIT_SEED_ITEMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_fit_seed_item() {
    FIT_SEED_ITEMS.with(|count| count.set(count.get() + 1));
}

/// Lexicographic cost for a source-break-aware paragraph layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct LayoutCost {
    overflow: usize,
    underflow: usize,
    changed_breaks: usize,
    displacement: usize,
    raggedness: usize,
    lines: usize,
}

impl LayoutCost {
    fn add(self, other: Self) -> Self {
        Self {
            overflow: self.overflow.saturating_add(other.overflow),
            underflow: self.underflow.saturating_add(other.underflow),
            changed_breaks: self.changed_breaks.saturating_add(other.changed_breaks),
            displacement: self.displacement.saturating_add(other.displacement),
            raggedness: self.raggedness.saturating_add(other.raggedness),
            lines: self.lines.saturating_add(other.lines),
        }
    }
}

/// The layout mode used to render a subtree. `Flat` means the producer has
/// verified that the entire flat rendering fits. `Break` lets child groups make
/// their own choices.
///
/// `FlatPrefix` is the weaker claim used by trailing-block hugs: measurement
/// stops at the first forced break, so nested groups beyond it must re-evaluate.
///
/// An `expand` group always renders in `Break`, even under an incoming `Flat`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    FlatPrefix,
    Break,
}

/// Policy for the shared flat-measurement traversal ([`Printer::flat_end`]):
/// one walker, three deliberate readings of the same subtree, kept as explicit
/// variants so the differences stay visible parameters instead of divergent
/// copies that can drift apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlatMeasure {
    /// The flat-rendered *footprint*, unbounded by the line width. A
    /// single-line forced-break `Verbatim` (a comment) counts its width — it
    /// shares the line, forcing a break only *after* — as does a group forced
    /// open only by one (`expand` is ignored; a genuinely unflattenable group
    /// surfaces its `HardLine` by recursion). Behind [`Printer::flat_width`],
    /// the fill layout's pair-fit width.
    Footprint,
    /// Can the subtree lie *fully flat* within the line width from the start
    /// column: any forced break (a comment included) fails, an `expand` group
    /// fails, and the measurement fails as soon as the width is exceeded. The
    /// non-hug `Group` decision.
    Fits,
    /// The trailing-block hug's prefix claim: a forced line break
    /// (`HardLine`/`EmptyLine`, or a *multi-line* `Verbatim`) stops the
    /// measurement *successfully* — only the prefix up to the block's opening
    /// needs to fit — while a single-line comment still fails, and content
    /// decides instead of the `expand` flag.
    HugPrefix,
}

/// Outcome of accounting one unbreakable atom in [`Printer::flat_end`].
enum AtomStep {
    Counted,
    Overflow,
}

/// How the shared line measurement ([`Printer::line_fits`]) treats a
/// single-line forced-break `Verbatim` (a standalone comment) — the one
/// deliberate policy difference between its two contexts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommentFit {
    /// A candidate carrying a comment can never render flat: fail, so the
    /// candidate picker falls through to a more broken layout.
    Fails,
    /// The rest of an already-committed line: the comment is there either
    /// way, so its width counts and the line simply ends at the break the
    /// comment forces after itself.
    SharesLine,
}

/// A physical line prefix active while rendering a virtual document layer.
#[derive(Clone, Copy)]
struct ActivePrefix<'a> {
    line: &'a str,
    blank: Option<&'a str>,
}

/// A unit of pending work on the printer's layout stack. Most IR nodes are a
/// plain [`Cmd::Node`]; [`Ir::Fill`] is processed incrementally as a
/// [`Cmd::Fill`], while [`Ir::PreferredFill`] carries the globally selected break
/// plan through [`Cmd::PreferredFill`].
enum Cmd<'a> {
    Node {
        indent: usize,
        mode: Mode,
        node: &'a Ir,
        /// The active margin prefix (see [`Ir::MarginPrefix`]), re-emitted at
        /// column 0 on every line break this command's subtree produces.
        /// Inherited by child commands; `None` outside any `MarginPrefix`.
        prefix: Option<ActivePrefix<'a>>,
    },
    Fill {
        indent: usize,
        mode: Mode,
        parts: &'a [Ir],
        prefix: Option<ActivePrefix<'a>>,
        /// A [`Ir::StickyFill`]: once an atom has broken, the rest break too.
        sticky: bool,
        /// A [`Ir::HugFill`]: an atom carrying a forced break is measured by
        /// its first line instead of a flat width it cannot have.
        hug: bool,
        /// Sticky bookkeeping: set once any earlier atom in this fill broke, so
        /// every remaining atom is forced to break. Always `false` for a plain
        /// (non-sticky) fill.
        broken: bool,
    },
    PreferredFill {
        indent: usize,
        mode: Mode,
        atoms: &'a [Ir],
        breaks: Rc<[bool]>,
        index: usize,
        prefix: Option<ActivePrefix<'a>>,
    },
}

/// Read the printer stack in print order without inspecting later lines.
struct RestSeeds<'a, 'stack> {
    commands: std::iter::Rev<std::slice::Iter<'stack, Cmd<'a>>>,
    fill: Option<(Mode, std::slice::Iter<'a, Ir>)>,
}

impl<'a, 'stack> RestSeeds<'a, 'stack> {
    fn new(commands: &'stack [Cmd<'a>]) -> Self {
        Self {
            commands: commands.iter().rev(),
            fill: None,
        }
    }
}

impl<'a> Iterator for RestSeeds<'a, '_> {
    type Item = (Mode, bool, &'a Ir);

    fn next(&mut self) -> Option<Self::Item> {
        let (mode, node) = loop {
            if let Some((mode, parts)) = &mut self.fill {
                if let Some(part) = parts.next() {
                    break (*mode, part);
                }
                self.fill = None;
            }
            match self.commands.next()? {
                Cmd::Node { mode, node, .. } => break (*mode, *node),
                Cmd::Fill { mode, parts, .. } => {
                    self.fill = Some((*mode, parts.iter()));
                }
                Cmd::PreferredFill {
                    mode, atoms, index, ..
                } => {
                    break (*mode, &atoms[*index]);
                }
            }
        };
        #[cfg(test)]
        record_fit_seed_item();
        Some((mode, mode == Mode::Flat, node))
    }
}

pub(crate) struct Printer {
    line_width: usize,
    indent_unit: usize,
}

/// Accumulates output while deferring indentation until visible content is
/// written, so blank lines never carry trailing whitespace.
struct Writer {
    out: String,
    col: usize,
    pending_indent: usize,
    needs_indent: bool,
}

impl Writer {
    fn new() -> Self {
        Self {
            out: String::new(),
            col: 0,
            pending_indent: 0,
            needs_indent: false,
        }
    }

    /// The column the next visible character would land at, accounting for an
    /// indent that has been queued (`needs_indent`) but not yet flushed — so a
    /// fill decision made right after a newline measures from the indent, not 0.
    fn current_col(&self) -> usize {
        self.col
            + if self.needs_indent {
                self.pending_indent
            } else {
                0
            }
    }

    fn flush_indent(&mut self) {
        if self.needs_indent {
            for _ in 0..self.pending_indent {
                self.out.push(' ');
            }
            self.col += self.pending_indent;
            self.needs_indent = false;
        }
    }

    /// Write text that contains no newline.
    fn write_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.flush_indent();
        self.out.push_str(s);
        self.col += s.chars().count();
    }

    /// Move to a fresh line. With no `prefix`, indentation is deferred to `indent`
    /// columns (the default path). With a `prefix` (a [`Ir::MarginPrefix`] margin),
    /// the prefix is written eagerly flush at column 0 right after the newline and
    /// no indent is queued, so the prefix re-appears on every wrapped line.
    fn newline(&mut self, indent: usize, prefix: Option<ActivePrefix<'_>>) {
        self.out.push('\n');
        self.col = 0;
        self.start_line(indent, prefix);
    }

    /// Emit a blank line, then position on a fresh line. The empty middle line
    /// never carries the `prefix` (no trailing whitespace); the fresh line does.
    fn empty_line(&mut self, indent: usize, prefix: Option<ActivePrefix<'_>>) {
        self.out.push('\n');
        if let Some(blank) = prefix.and_then(|active| active.blank) {
            self.out.push_str(blank);
        }
        self.out.push('\n');
        self.col = 0;
        self.start_line(indent, prefix);
    }

    /// Position at the start of a fresh line: write an eager margin `prefix` at
    /// column 0 if active, else queue `indent` spaces (deferred until content).
    fn start_line(&mut self, indent: usize, prefix: Option<ActivePrefix<'_>>) {
        match prefix {
            Some(p) => {
                self.out.push_str(p.line);
                self.col = p.line.chars().count();
                self.pending_indent = indent;
                self.needs_indent = indent > 0;
            }
            None => {
                self.pending_indent = indent;
                self.needs_indent = true;
            }
        }
    }

    /// Splice a possibly multi-line string verbatim. The string is assumed to
    /// already carry its own indentation, so only a pending indent on the very
    /// first line is honored.
    fn write_verbatim(&mut self, s: &str) {
        let mut first = true;
        for segment in s.split('\n') {
            if first {
                self.flush_indent();
                first = false;
            } else {
                self.out.push('\n');
                self.col = 0;
                self.needs_indent = false;
            }
            self.out.push_str(segment);
            self.col += segment.chars().count();
        }
    }

    /// Write a single-line chunk pinned to column 0: discard any pending indent so
    /// the chunk starts flush at the line's left margin (a `.dtx` margin/guard,
    /// see [`Ir::ColumnZero`]). The caller guarantees this is the first visible
    /// token of its physical line, so dropping the indent is exactly the column-0
    /// rule and never clobbers already-emitted content.
    fn write_column_zero(&mut self, s: &str) {
        self.needs_indent = false;
        self.pending_indent = 0;
        self.out.push_str(s);
        self.col += s.chars().count();
    }
}

impl Printer {
    pub(crate) fn new(style: FormatStyle) -> Self {
        Self {
            line_width: style.line_width,
            indent_unit: style.indent_width,
        }
    }

    /// Print a complete document starting at column 0.
    pub(crate) fn print(&self, ir: &Ir) -> String {
        self.run(ir, 0, 0)
    }

    /// Render `ir` on a single line (every break primitive laid out flat). Used by
    /// the alignment lowering to measure and emit a table cell's content; callers
    /// must ensure `ir` carries no unconditional forced break (a `HardLine` would
    /// still emit a newline in flat mode), which the alignment grid guarantees by
    /// falling back when any cell `contains_forced_break`. Width is taken as
    /// effectively infinite so a width-driven `Group`/`ConditionalGroupAllLines` inside a
    /// cell stays flat rather than breaking on the configured line width.
    pub(crate) fn print_flat(&self, ir: &Ir) -> String {
        self.wide().run_with_mode(ir, 0, 0, Mode::Flat)
    }

    /// A copy of this printer with an effectively infinite line width, so a
    /// width-driven `Group`/`ConditionalGroupAllLines` never breaks and only structural
    /// `HardLine`s split the output. The probe behind [`Self::print_flat`] and
    /// [`Self::all_lines_fit`].
    fn wide(&self) -> Printer {
        Printer {
            line_width: usize::MAX / 2,
            indent_unit: self.indent_unit,
        }
    }

    fn run(&self, ir: &Ir, base_indent: usize, init_col: usize) -> String {
        self.run_with_mode(ir, base_indent, init_col, Mode::Break)
    }

    fn run_with_mode(&self, ir: &Ir, base_indent: usize, init_col: usize, mode: Mode) -> String {
        self.run_with_mode_prefix(ir, base_indent, init_col, mode, None)
    }

    fn run_with_mode_prefix<'a>(
        &self,
        ir: &'a Ir,
        base_indent: usize,
        init_col: usize,
        mode: Mode,
        initial_prefix: Option<ActivePrefix<'a>>,
    ) -> String {
        let mut w = Writer::new();
        w.col = init_col;
        let mut stack: Vec<Cmd<'_>> = vec![Cmd::Node {
            indent: base_indent,
            mode,
            node: ir,
            prefix: initial_prefix,
        }];
        while let Some(cmd) = stack.pop() {
            let (indent, mode, node, prefix) = match cmd {
                Cmd::Node {
                    indent,
                    mode,
                    node,
                    prefix,
                } => (indent, mode, node, prefix),
                // A fill continuation: lay out the next word/separator pair (see
                // `step_fill`), pushing the remainder back for the next iteration.
                Cmd::Fill {
                    indent,
                    mode,
                    parts,
                    prefix,
                    sticky,
                    hug,
                    broken,
                } => {
                    self.step_fill(
                        &w, indent, mode, parts, prefix, sticky, hug, broken, &mut stack,
                    );
                    continue;
                }
                Cmd::PreferredFill {
                    indent,
                    mode,
                    atoms,
                    breaks,
                    index,
                    prefix,
                } => {
                    if index > 0 {
                        if mode == Mode::Break && breaks[index - 1] {
                            w.newline(indent, prefix);
                        } else {
                            w.write_text(" ");
                        }
                    }
                    if index + 1 < atoms.len() {
                        stack.push(Cmd::PreferredFill {
                            indent,
                            mode,
                            atoms,
                            breaks: Rc::clone(&breaks),
                            index: index + 1,
                            prefix,
                        });
                    }
                    // The break plan places atoms, it does not verify them:
                    // in `Break` mode each atom's own groups decide for
                    // themselves (an inherited `Flat` was verified by the
                    // parent and stays).
                    stack.push(Cmd::Node {
                        indent,
                        mode,
                        node: &atoms[index],
                        prefix,
                    });
                    continue;
                }
            };
            match node {
                Ir::Nil => {}
                Ir::Text(s) => w.write_text(s),
                Ir::Verbatim { text, .. } => w.write_verbatim(text),
                Ir::ZeroWidth(text) => w.write_verbatim(text),
                Ir::ColumnZero(text) => w.write_column_zero(text),
                Ir::Concat(items) => {
                    for item in items.iter().rev() {
                        stack.push(Cmd::Node {
                            indent,
                            mode,
                            node: item,
                            prefix,
                        });
                    }
                }
                Ir::Fill(parts) => stack.push(Cmd::Fill {
                    indent,
                    mode,
                    parts: &parts[..],
                    prefix,
                    sticky: false,
                    hug: false,
                    broken: false,
                }),
                Ir::StickyFill(parts) => stack.push(Cmd::Fill {
                    indent,
                    mode,
                    parts: &parts[..],
                    prefix,
                    sticky: true,
                    hug: false,
                    broken: false,
                }),
                Ir::HugFill(parts) => stack.push(Cmd::Fill {
                    indent,
                    mode,
                    parts: &parts[..],
                    prefix,
                    sticky: false,
                    hug: true,
                    broken: false,
                }),
                Ir::PreferredFill {
                    atoms,
                    preferred,
                    target,
                } => {
                    let breaks = if mode != Mode::Break {
                        vec![false; atoms.len().saturating_sub(1)].into()
                    } else {
                        self.stable_breaks(
                            w.current_col(),
                            indent,
                            prefix,
                            atoms,
                            preferred,
                            *target,
                        )
                    };
                    stack.push(Cmd::PreferredFill {
                        indent,
                        mode,
                        atoms,
                        breaks,
                        index: 0,
                        prefix,
                    });
                }
                Ir::Indent(inner) => {
                    stack.push(Cmd::Node {
                        indent: indent + self.indent_unit,
                        mode,
                        node: inner,
                        prefix,
                    });
                }
                Ir::Align(width, inner) => {
                    stack.push(Cmd::Node {
                        indent: indent + width,
                        mode,
                        node: inner,
                        prefix,
                    });
                }
                Ir::AlignCurrent(inner) => {
                    // A virtual-document prefix is re-emitted separately on
                    // continuation lines, so the stored indent excludes it.
                    let prefix_width = prefix.map_or(0, |active| active.line.chars().count());
                    stack.push(Cmd::Node {
                        indent: w.current_col().saturating_sub(prefix_width),
                        mode,
                        node: inner,
                        prefix,
                    });
                }
                Ir::BoundedAlign { aligned, fallback } => {
                    let node = if mode != Mode::Break
                        || self.bounded_align_fits(w.current_col(), indent, aligned, prefix)
                    {
                        aligned
                    } else {
                        fallback
                    };
                    stack.push(Cmd::Node {
                        indent,
                        mode,
                        node,
                        prefix,
                    });
                }
                // Activate the margin prefix for `inner`: emit it now for the
                // first line (the leading break that put us here came from the
                // parent, outside this scope) via the column-0 pin, then print
                // `inner` with the prefix active so every later break re-emits it.
                Ir::MarginPrefix {
                    prefix: margin,
                    blank_prefix,
                    inner,
                } => {
                    w.write_column_zero(margin);
                    stack.push(Cmd::Node {
                        indent,
                        mode,
                        node: inner,
                        prefix: Some(ActivePrefix {
                            line: margin,
                            blank: blank_prefix.as_deref(),
                        }),
                    });
                }
                Ir::Line => match mode {
                    Mode::Flat | Mode::FlatPrefix => w.write_text(" "),
                    Mode::Break => w.newline(indent, prefix),
                },
                Ir::TightLine => {
                    if mode == Mode::Break {
                        w.newline(indent, prefix);
                    }
                }
                Ir::SoftLine => {
                    if mode == Mode::Break {
                        w.newline(indent, prefix);
                    }
                }
                Ir::HardLine => w.newline(indent, prefix),
                Ir::EmptyLine => w.empty_line(indent, prefix),
                Ir::IfBreak { flat, broken } => {
                    let chosen = if mode == Mode::Break { broken } else { flat };
                    stack.push(Cmd::Node {
                        indent,
                        mode,
                        node: chosen,
                        prefix,
                    });
                }
                // Requires a `propagate_breaks`-saturated tree: honoring an
                // incoming `Flat` trusts that a group whose subtree carries a
                // hard break is `expand`-marked (the first arm), so a stale
                // flag would wrongly pin its `Line`s flat.
                Ir::Group {
                    inner, expand, hug, ..
                } => {
                    // Both measurements start from `current_col()` (not the
                    // raw `col`): a group dispatched right after a newline
                    // must count the pending indent, or the dropped width
                    // lets an overflowing flat layout be wrongly verified —
                    // an error the honest contract would then pin instead of
                    // letting nested re-decisions paper over it.
                    let m = if *expand {
                        Mode::Break
                    } else if mode == Mode::Flat {
                        // Honest contract: the producer verified the whole
                        // flat rendering, so don't re-decide.
                        Mode::Flat
                    } else if *hug {
                        // A trailing-block hug measures only its own prefix up to
                        // the block's opening brace; what follows sits on the
                        // block's closing line, not this one — so the claim it
                        // makes is `FlatPrefix`, never full `Flat`.
                        let measure = FlatMeasure::HugPrefix;
                        if self.flat_end(w.current_col(), inner, measure).is_some() {
                            Mode::FlatPrefix
                        } else {
                            Mode::Break
                        }
                    } else if self.group_fits(w.current_col(), inner, &stack) {
                        Mode::Flat
                    } else {
                        Mode::Break
                    };
                    stack.push(Cmd::Node {
                        indent,
                        mode: m,
                        node: inner,
                        prefix,
                    });
                }
                Ir::ConditionalGroupAllLines(cands) => {
                    // `current_col()` (not the raw `col`) so a group dispatched right
                    // after a newline measures its first line from the pending
                    // indent, not column 0 — otherwise the indent width is dropped
                    // and an overflowing flat candidate is wrongly accepted.
                    let (m, chosen) = if mode == Mode::Flat {
                        (Mode::Flat, &cands[0])
                    } else {
                        self.pick_candidate_all_lines(w.current_col(), indent, cands)
                    };
                    stack.push(Cmd::Node {
                        indent,
                        mode: m,
                        node: chosen,
                        prefix,
                    });
                }
            }
        }
        w.out
    }

    /// Choose all breaks for a [`Ir::PreferredFill`] together. Authored breaks are
    /// stable once a line reaches `target` (or the next atom would reach it), but
    /// overflow always wins; ties minimize changed breaks and their displacement.
    fn stable_breaks(
        &self,
        first_col: usize,
        indent: usize,
        prefix: Option<ActivePrefix<'_>>,
        atoms: &[Ir],
        preferred: &[bool],
        target: usize,
    ) -> Rc<[bool]> {
        let n = atoms.len();
        if n < 2 {
            return Vec::new().into();
        }
        debug_assert_eq!(preferred.len(), n - 1);

        let widths: Vec<usize> = atoms
            .iter()
            .map(|atom| {
                self.flat_width(atom)
                    .unwrap_or(self.line_width.saturating_add(1))
            })
            .collect();
        let mut sums = Vec::with_capacity(n + 1);
        sums.push(0usize);
        for &width in &widths {
            sums.push(sums.last().copied().unwrap_or(0).saturating_add(width));
        }
        // Source positions of the gaps in the normalized, space-joined run. They
        // are used only to rank displacement from the nearest authored anchor.
        let gap_positions: Vec<usize> = (0..n - 1)
            .map(|gap| sums[gap + 1].saturating_add(gap + 1))
            .collect();
        let authored_positions: Vec<usize> = preferred
            .iter()
            .enumerate()
            .filter_map(|(i, &is_authored)| is_authored.then_some(gap_positions[i]))
            .collect();
        // These costs depend only on the source gaps, not the candidate line.
        // Precompute them instead of scanning authored anchors at every edge.
        let mut anchor_counts = Vec::with_capacity(n);
        anchor_counts.push(0usize);
        for &authored in preferred {
            anchor_counts.push(anchor_counts.last().copied().unwrap_or(0) + usize::from(authored));
        }
        let displacements: Vec<usize> = gap_positions
            .iter()
            .map(|&position| {
                let at = authored_positions.partition_point(|&anchor| anchor < position);
                authored_positions
                    .get(at)
                    .into_iter()
                    .chain(
                        at.checked_sub(1)
                            .and_then(|prior| authored_positions.get(prior)),
                    )
                    .map(|&anchor| anchor.abs_diff(position))
                    .min()
                    .unwrap_or(0)
            })
            .collect();
        let continuation_col = prefix.map_or(indent, |p| p.line.chars().count());
        let next_authored_end =
            (self.line_width.checked_add(1) == Some(continuation_col)).then(|| {
                let mut ends = vec![n; n];
                let mut next = n;
                for gap in (0..n - 1).rev() {
                    if preferred[gap] {
                        next = gap + 1;
                    }
                    ends[gap] = next;
                }
                ends
            });
        let target = target.clamp(1, self.line_width.max(1));

        let mut costs: Vec<Option<LayoutCost>> = vec![None; n + 1];
        let mut previous: Vec<Option<usize>> = vec![None; n + 1];
        costs[0] = Some(LayoutCost::default());

        #[cfg(test)]
        let mut edges = 0usize;
        for start in 0..n {
            let Some(base_cost) = costs[start] else {
                continue;
            };
            let base_col = if start == 0 {
                first_col
            } else {
                continuation_col
            };
            let mut last_fitting_length = None;
            // Beyond width + one space, merging continuation lines strictly
            // reduces overflow. At exactly width + one, suffix overflow is
            // independent of breaks: preserving precisely the authored gaps
            // uniquely minimizes changed_breaks. Both cases need only one edge.
            let fixed_end = if start == 0 {
                None
            } else if continuation_col > self.line_width.saturating_add(1) {
                Some(n)
            } else {
                next_authored_end.as_ref().map(|ends| ends[start])
            };
            let first_end = fixed_end.unwrap_or(start + 1);
            let last_end = fixed_end.unwrap_or(n);
            for end in first_end..=last_end {
                #[cfg(test)]
                {
                    edges += 1;
                }
                let content_width = sums[end]
                    .saturating_sub(sums[start])
                    .saturating_add(end.saturating_sub(start + 1));
                let length = base_col.saturating_add(content_width);
                let next_length =
                    (end < n).then(|| length.saturating_add(1).saturating_add(widths[end]));
                let final_line = end == n;
                let overflow = length.saturating_sub(self.line_width);
                if overflow == 0 {
                    last_fitting_length = Some(length);
                } else if last_fitting_length
                    .is_some_and(|fitting: usize| fitting.saturating_add(1) > continuation_col)
                    || (last_fitting_length.is_none()
                        && end > start + 1
                        && continuation_col <= self.line_width)
                {
                    // Splitting at a fitting prefix of length L has less
                    // overflow than joining the remaining content when L+1>C,
                    // where C is the continuation column. If the first atom
                    // already overflows, keep its singleton edge: joining later
                    // content adds its width plus one to overflow, whereas a
                    // continuation at C<=width adds at most the content width.
                    break;
                }

                let removed_anchors = anchor_counts[end - 1] - anchor_counts[start];
                let new_break = usize::from(!final_line && !preferred[end - 1]);
                let displacement = if new_break == 0 {
                    0
                } else {
                    displacements[end - 1]
                };
                let underflow = if final_line
                    || length >= target
                    || next_length.is_some_and(|next| next >= target)
                {
                    0
                } else {
                    target - length
                };
                let segment = LayoutCost {
                    overflow,
                    underflow,
                    changed_breaks: removed_anchors + new_break,
                    displacement,
                    raggedness: if final_line {
                        0
                    } else {
                        target.abs_diff(length)
                    },
                    lines: 1,
                };
                let candidate = base_cost.add(segment);
                if costs[end].is_none_or(|cost| candidate < cost) {
                    costs[end] = Some(candidate);
                    previous[end] = Some(start);
                }
            }
        }

        // Count candidate edges in tests rather than assert wall-clock timing.
        #[cfg(test)]
        if continuation_col > self.line_width || widths.iter().all(|&width| width > self.line_width)
        {
            assert!(
                edges <= n * 2,
                "overflowing fills must consider at most two edges per atom: {edges} for {n}"
            );
        }
        let mut breaks = vec![false; n - 1];
        let mut cursor = n;
        while let Some(prior) = previous[cursor] {
            if prior > 0 {
                breaks[prior - 1] = true;
            }
            cursor = prior;
        }
        breaks.into()
    }

    // Deliberately retain the scan-based implementation as a test-only
    // exhaustive oracle for edge costs, tie order and dominance pruning.
    #[cfg(test)]
    fn stable_breaks_reference(
        &self,
        first_col: usize,
        indent: usize,
        prefix: Option<ActivePrefix<'_>>,
        atoms: &[Ir],
        preferred: &[bool],
        target: usize,
    ) -> Rc<[bool]> {
        let n = atoms.len();
        if n < 2 {
            return Vec::new().into();
        }
        debug_assert_eq!(preferred.len(), n - 1);

        let widths: Vec<usize> = atoms
            .iter()
            .map(|atom| {
                self.flat_width(atom)
                    .unwrap_or(self.line_width.saturating_add(1))
            })
            .collect();
        let mut sums = Vec::with_capacity(n + 1);
        sums.push(0usize);
        for &width in &widths {
            sums.push(sums.last().copied().unwrap_or(0).saturating_add(width));
        }
        // Source positions of the gaps in the normalized, space-joined run. They
        // are used only to rank displacement from the nearest authored anchor.
        let gap_positions: Vec<usize> = (0..n - 1)
            .map(|gap| sums[gap + 1].saturating_add(gap + 1))
            .collect();
        let authored_positions: Vec<usize> = preferred
            .iter()
            .enumerate()
            .filter_map(|(i, &is_authored)| is_authored.then_some(gap_positions[i]))
            .collect();
        let continuation_col = prefix.map_or(indent, |p| p.line.chars().count());
        let target = target.clamp(1, self.line_width.max(1));

        let mut costs: Vec<Option<LayoutCost>> = vec![None; n + 1];
        let mut previous: Vec<Option<usize>> = vec![None; n + 1];
        costs[0] = Some(LayoutCost::default());

        for start in 0..n {
            let Some(base_cost) = costs[start] else {
                continue;
            };
            let base_col = if start == 0 {
                first_col
            } else {
                continuation_col
            };
            for end in start + 1..=n {
                let content_width = sums[end]
                    .saturating_sub(sums[start])
                    .saturating_add(end.saturating_sub(start + 1));
                let length = base_col.saturating_add(content_width);
                let next_length =
                    (end < n).then(|| length.saturating_add(1).saturating_add(widths[end]));
                let final_line = end == n;
                let overflow = length.saturating_sub(self.line_width);
                let removed_anchors = if end > start + 1 {
                    preferred[start..end - 1]
                        .iter()
                        .filter(|&&is_authored| is_authored)
                        .count()
                } else {
                    0
                };
                let new_break = usize::from(!final_line && !preferred[end - 1]);
                let displacement = if new_break == 0 || authored_positions.is_empty() {
                    0
                } else {
                    authored_positions
                        .iter()
                        .map(|&anchor| anchor.abs_diff(gap_positions[end - 1]))
                        .min()
                        .unwrap_or(0)
                };
                let underflow = if final_line
                    || length >= target
                    || next_length.is_some_and(|next| next >= target)
                {
                    0
                } else {
                    target - length
                };
                let segment = LayoutCost {
                    overflow,
                    underflow,
                    changed_breaks: removed_anchors + new_break,
                    displacement,
                    raggedness: if final_line {
                        0
                    } else {
                        target.abs_diff(length)
                    },
                    lines: 1,
                };
                let candidate = base_cost.add(segment);
                if costs[end].is_none_or(|cost| candidate < cost) {
                    costs[end] = Some(candidate);
                    previous[end] = Some(start);
                }
            }
        }

        let mut breaks = vec![false; n - 1];
        let mut cursor = n;
        while let Some(prior) = previous[cursor] {
            if prior > 0 {
                breaks[prior - 1] = true;
            }
            cursor = prior;
        }
        breaks.into()
    }

    /// One step of laying out an [`Ir::Fill`] — the Wadler/Prettier greedy fill.
    /// `parts` is the alternating `[atom, sep, atom, …]` remainder. In `Flat`
    /// mode every separator is a space (the whole fill on one line); in `Break`
    /// mode each gap is decided independently: the first atom is printed, then
    /// the separator stays flat (a space) iff the *pair* `atom + sep + next-atom`
    /// fits flat from the current column, else it breaks. A lone atom that does
    /// not fit is printed anyway (no break can rescue an unbreakable word). The
    /// remaining fill is pushed back so the next iteration decides the next gap.
    ///
    /// A **sticky** fill (`sticky`, [`Ir::StickyFill`]) instead cascades: `broken`
    /// records that an earlier atom in this fill already broke, so every remaining
    /// atom is forced to break regardless of its own fit — the greedy fill's
    /// independent gaps are exactly what would re-glue an expl3 false-branch onto a
    /// detonated block's short closing line (issue #94).
    ///
    /// A **hugging** fill (`hug`, [`Ir::HugFill`]) changes the fit question rather
    /// than the cascade: an atom that carries a forced break is placed when its
    /// *first line* fits (see [`Self::fill_atom_mode`]), instead of being pushed to
    /// a line of its own for want of a flat width.
    #[allow(clippy::too_many_arguments)]
    fn step_fill<'a>(
        &self,
        w: &Writer,
        indent: usize,
        mode: Mode,
        parts: &'a [Ir],
        prefix: Option<ActivePrefix<'a>>,
        sticky: bool,
        hug: bool,
        broken: bool,
        stack: &mut Vec<Cmd<'a>>,
    ) {
        if parts.is_empty() {
            return;
        }
        if mode != Mode::Break {
            for part in parts.iter().rev() {
                stack.push(Cmd::Node {
                    indent,
                    mode,
                    node: part,
                    prefix,
                });
            }
            return;
        }

        let col = w.current_col();
        let content = &parts[0];
        let w0 = self.flat_width(content);
        // Under a sticky cascade every remaining atom breaks; otherwise the atom
        // is placed here when it fits — fully flat, or (in a hugging fill) with
        // only its first line on this line.
        let content_mode = (!broken)
            .then(|| self.fill_atom_mode(col, content, hug))
            .flatten();

        if parts.len() == 1 {
            // The fill's last atom shares its line with whatever the lowering
            // glued after the fill (a trailing command riding a statement), so
            // its flat claim must survive the rest of the line too — the same
            // rest-awareness `group_fits` has. Without this the atom's folded
            // hang break is never taken: the atom alone fits, goes `Flat`, and
            // the glued tail overflows the line the measurement never saw.
            //
            // A hug claim is exempt: like [`Ir::group_hug`]'s, it never covered
            // the rest of the line to begin with (what follows sits on the
            // atom's *closing* line, not this one). Without the exemption the
            // last atom of a hugging fill would break where the same atom
            // mid-fill hugs, and those are the same atom on consecutive passes
            // — a statement whose trailing `,` moved to its own line ends the
            // fill one atom earlier (`xfm.dtx`).
            let mode = match content_mode {
                Some(Mode::Flat) if matches!(w0, Some(width) if self.rest_fits(col + width, stack)) => {
                    Mode::Flat
                }
                Some(Mode::FlatPrefix) => Mode::FlatPrefix,
                _ => Mode::Break,
            };
            stack.push(Cmd::Node {
                indent,
                mode,
                node: content,
                prefix,
            });
            return;
        }

        let sep = &parts[1];
        // Pair fit: the current atom, its separator, and the next atom, all
        // flat — the next atom by the same rule as `content_mode`, so a hugging
        // fill keeps a detonating atom glued to the head before it. The current
        // atom must still claim a full flat width: after a multi-line atom the
        // next atom's column is unknown, so the gap breaks (a `Nil` separator
        // renders nothing either way, which is what re-glues a sibling onto a
        // hanging group's closing line). Alternating fills always end on an
        // atom, so `parts[2]` exists here.
        let pair_fits = !broken
            && match (w0, self.flat_width(sep)) {
                (Some(a), Some(s)) => self.fill_atom_mode(col + a + s, &parts[2], hug).is_some(),
                _ => false,
            };
        // Once any atom breaks, a sticky fill stays broken for its remainder, so
        // the later gaps break unconditionally instead of each deciding afresh.
        // A *hugged* atom starts the cascade too: it was placed on this line,
        // but only its first line fits — the rest of the line is gone, so the
        // atoms after it are exactly the ones issue #94 must not re-glue onto a
        // detonated block's short closing line.
        let remainder_broken = sticky && (broken || content_mode != Some(Mode::Flat));
        // Push the remainder first (popped last), then the separator, then the
        // content (popped first), so they print in order.
        stack.push(Cmd::Fill {
            indent,
            mode: Mode::Break,
            parts: &parts[2..],
            prefix,
            sticky,
            hug,
            broken: remainder_broken,
        });
        stack.push(Cmd::Node {
            indent,
            mode: if pair_fits { Mode::Flat } else { Mode::Break },
            node: sep,
            prefix,
        });
        stack.push(Cmd::Node {
            indent,
            mode: content_mode.unwrap_or(Mode::Break),
            node: content,
            prefix,
        });
    }

    /// The mode a fill atom is printed in when it can be placed at `col`, or
    /// `None` when it must move to the next line.
    ///
    /// A plain fill asks one question: does the atom render *fully flat* within
    /// the line width. A **hugging** fill ([`Ir::HugFill`]) accepts a second
    /// answer: an atom carrying a forced break has no flat width at all, but its
    /// *first line* may still fit here, in which case it is placed and prints as
    /// [`Mode::FlatPrefix`] — the same claim [`Ir::group_hug`] makes about a
    /// trailing block, so the atom's own body breaks below instead of the atom
    /// being pushed to a line of its own. Flat is tried first, so a single-line
    /// comment (which `HugPrefix` rejects but a flat footprint counts) still
    /// shares its line.
    fn fill_atom_mode(&self, col: usize, atom: &Ir, hug: bool) -> Option<Mode> {
        if matches!(self.flat_width(atom), Some(width) if col + width <= self.line_width) {
            return Some(Mode::Flat);
        }
        let prefix_fits = hug && self.flat_end(col, atom, FlatMeasure::HugPrefix).is_some();
        prefix_fits.then_some(Mode::FlatPrefix)
    }

    /// The flat-rendered width of `node`, or `None` if it cannot be laid flat
    /// (it carries a forced line break: a `HardLine`/`EmptyLine` or a multi-line
    /// `Verbatim`). A single-line force-break `Verbatim` (a comment) *can* share
    /// a line with what precedes it — it only forces a break *after* — so it
    /// counts as its text width here. Used by the fill layout's pair-fit test.
    fn flat_width(&self, node: &Ir) -> Option<usize> {
        self.flat_end(0, node, FlatMeasure::Footprint)
    }

    /// The shared traversal behind every flat measurement: simulate `node`
    /// laid out flat from `start_col` and return the column it ends at, or
    /// `None` when the measurement fails under `measure`'s policy (a forced
    /// break, an overflow, an `expand` group — see [`FlatMeasure`]). For a
    /// [`FlatMeasure::HugPrefix`] stop the
    /// returned column is where the measurement stopped, not a full line end —
    /// those callers only ask `is_some()`.
    fn flat_end(&self, start_col: usize, node: &Ir, measure: FlatMeasure) -> Option<usize> {
        let hug = matches!(measure, FlatMeasure::HugPrefix);
        let mut col = start_col;
        let mut stack: Vec<&Ir> = vec![node];
        while let Some(node) = stack.pop() {
            match node {
                Ir::Nil | Ir::SoftLine | Ir::ZeroWidth(_) => {}
                Ir::Text(s) | Ir::ColumnZero(s) => {
                    match self.flat_atom(&mut col, s.chars().count(), measure) {
                        AtomStep::Counted => {}
                        AtomStep::Overflow => return None,
                    }
                }
                Ir::Verbatim { text, force_break } => {
                    // A multi-line verbatim behaves like a `HardLine`: only a
                    // hug survives it (the prefix up to its own first newline
                    // is what needed to fit).
                    if text.contains('\n') {
                        return hug.then_some(col);
                    }
                    // A standalone comment cannot lie flat (it forces a break
                    // after itself), and a comment in a hug prefix forbids
                    // the hug. Only the footprint counts it: the comment
                    // shares the line, breaking only *after*.
                    if *force_break && measure != FlatMeasure::Footprint {
                        return None;
                    }
                    match self.flat_atom(&mut col, text.chars().count(), measure) {
                        AtomStep::Counted => {}
                        AtomStep::Overflow => return None,
                    }
                }
                Ir::HardLine | Ir::EmptyLine => return hug.then_some(col),
                Ir::Line | Ir::TightLine => {
                    col = col.saturating_add(1);
                    if measure != FlatMeasure::Footprint && col > self.line_width {
                        return None;
                    }
                }
                Ir::Concat(items) => stack.extend(items.iter().rev()),
                Ir::Indent(inner) | Ir::Align(_, inner) | Ir::AlignCurrent(inner) => {
                    stack.push(inner);
                }
                Ir::BoundedAlign { aligned, .. } => stack.push(aligned),
                Ir::MarginPrefix { inner, .. } => stack.push(inner),
                Ir::IfBreak { flat, .. } => stack.push(flat),
                // `Fits` trusts the saturated `expand` flag; `Footprint` and
                // `HugPrefix` deliberately let the content decide instead. A
                // group forced open only by a single-line comment still has a
                // flat footprint (the comment shares the line, forcing a
                // break only after), and a hug must stop *successfully* at a
                // nested block's first hard break while a prefix comment
                // fails it — distinctions the flag cannot carry.
                Ir::Group { inner, expand, .. } => {
                    if *expand && measure == FlatMeasure::Fits {
                        return None;
                    }
                    stack.push(inner);
                }
                // Conservative: measure as the flat-most candidate, matching
                // how the printer resolves a conditional group under a
                // verified `Flat`.
                Ir::ConditionalGroupAllLines(cands) => {
                    if let Some(first) = cands.first() {
                        stack.push(first);
                    }
                }
                // A fill measured flat is its atoms separated by single-space
                // `Line`s; push the parts and let the arms above account them.
                Ir::Fill(parts) | Ir::StickyFill(parts) | Ir::HugFill(parts) => {
                    stack.extend(parts.iter().rev());
                }
                Ir::PreferredFill { atoms, .. } => {
                    let gaps = atoms.len().saturating_sub(1);
                    col = col.saturating_add(gaps);
                    if measure != FlatMeasure::Footprint && col > self.line_width {
                        return None;
                    }
                    stack.extend(atoms.iter().rev());
                }
            }
        }
        Some(col)
    }

    /// Account one unbreakable atom of `width` columns during [`Self::flat_end`].
    fn flat_atom(&self, col: &mut usize, width: usize, measure: FlatMeasure) -> AtomStep {
        *col = col.saturating_add(width);
        match measure {
            FlatMeasure::Footprint => AtomStep::Counted,
            _ if *col <= self.line_width => AtomStep::Counted,
            _ => AtomStep::Overflow,
        }
    }

    /// Whether an aligned candidate's broken continuation lines fit from the
    /// actual current column and indentation. Its first line is deliberately
    /// excluded: alignment does not indent that line, so an intrinsically long
    /// first segment remains an honest overflow. The fallback need not fit—it is
    /// the honest least-indented rendering when alignment causes overflow.
    fn bounded_align_fits<'a>(
        &self,
        start_col: usize,
        indent: usize,
        aligned: &'a Ir,
        prefix: Option<ActivePrefix<'a>>,
    ) -> bool {
        let rendered = self.run_with_mode_prefix(aligned, indent, start_col, Mode::Break, prefix);
        let mut lines = rendered.split('\n');
        let _ = lines.next();
        lines.all(|line| line.chars().count() <= self.line_width)
    }

    /// Flat layout of an [`Ir::PreferredFill`] from `col`: its atoms joined by
    /// single spaces (`atoms.len() - 1` gaps). Returns the resulting column, or
    /// `None` if it overflows the line width or an atom cannot be laid flat
    /// (each atom is measured as its [`Self::flat_width`] footprint). Used by
    /// [`Self::line_fits`]'s flat-mode arm; [`Self::flat_end`] has its own
    /// arm, which measures the atoms under its policy instead of the footprint.
    fn preferred_fill_flat_end(&self, col: usize, atoms: &[Ir]) -> Option<usize> {
        let mut end = col.saturating_add(atoms.len().saturating_sub(1));
        for atom in atoms {
            end = end.checked_add(self.flat_width(atom)?)?;
            if end > self.line_width {
                return None;
            }
        }
        Some(end)
    }

    /// Pick the layout for an [`Ir::ConditionalGroupAllLines`] at the current column:
    /// the first candidate whose first line fits, or the last as the fallback.
    /// Always announced as `Break` — a first-line fit verifies nothing about
    /// the rest of the subtree, so under the honest [`Mode`] contract the
    /// *choice of candidate* is the whole decision and the candidate's own
    /// groups decide for themselves.
    fn pick_candidate<'a>(&self, col: usize, cands: &'a [Ir]) -> (Mode, &'a Ir) {
        let (last, rest) = cands
            .split_last()
            .expect("Ir::ConditionalGroupAllLines builder rejects empty candidate lists");
        let chosen = rest
            .iter()
            .find(|c| self.first_line_fits(col, c))
            .unwrap_or(last);
        (Mode::Break, chosen)
    }

    /// Pick the layout for an [`Ir::ConditionalGroupAllLines`]: the first
    /// candidate every one of whose rendered lines fits within `line_width`
    /// is rendered flat; if none qualifies the last candidate is rendered
    /// broken. The IR-native equivalent of the legacy `fits_with_newlines`
    /// check. The `Flat` announced here is verified: [`Self::all_lines_fit`] verified the candidate's whole
    /// flat-mode rendering — so the chosen candidate's nested groups are
    /// pinned to exactly the layout that was measured.
    fn pick_candidate_all_lines<'a>(
        &self,
        col: usize,
        indent: usize,
        cands: &'a [Ir],
    ) -> (Mode, &'a Ir) {
        let n = cands.len();
        for (i, c) in cands.iter().enumerate() {
            if self.all_lines_fit(col, indent, c) {
                return (Mode::Flat, c);
            }
            if i + 1 == n {
                return (Mode::Break, c);
            }
        }
        unreachable!("Ir::ConditionalGroupAllLines builder rejects empty candidate lists")
    }

    /// Whether every line `node` would render to fits within `line_width`, when
    /// placed at column `start_col` under the active `indent`. Used by
    /// [`Self::pick_candidate_all_lines`]. The candidate is rendered with a
    /// *very wide* line so every width-driven `Group`/`ConditionalGroupAllLines` inside it
    /// stays flat — only the candidate's own structural `HardLine`s split it into
    /// lines — and then each of those lines is measured against the real
    /// `line_width`. A candidate therefore "fits" only when its content genuinely
    /// lays out within the width without any nested group having to break: a flat
    /// candidate whose single line overflows is rejected (the broken fallback is
    /// taken) rather than silently accepted as a hybrid where a nested brace group
    /// broke to keep each printed line short.
    fn all_lines_fit(&self, start_col: usize, indent: usize, node: &Ir) -> bool {
        let rendered = self
            .wide()
            .run_with_mode(node, indent, start_col, Mode::Flat);
        let mut lines = rendered.split('\n');
        if let Some(first) = lines.next()
            && start_col + first.chars().count() > self.line_width
        {
            return false;
        }
        for line in lines {
            if line.chars().count() > self.line_width {
                return false;
            }
        }
        true
    }

    /// Rest-aware fit check for a non-hugging [`Ir::Group`]: whether `inner`
    /// laid flat, *followed by* the already-queued `rest` commands up to the
    /// next line break, fits within the line width from `start_col`. Trailing
    /// same-line content (e.g. the closing `)` of a call hugging this group as
    /// its sole argument) counts toward the decision, so a group breaks when the
    /// inner plus what follows would overflow — not just the inner in isolation.
    /// This is the Wadler/Prettier "fits the rest of the line" rule and the cure
    /// for break decisions that were previously purely local.
    fn group_fits(&self, start_col: usize, inner: &Ir, rest: &[Cmd]) -> bool {
        self.flat_end(start_col, inner, FlatMeasure::Fits)
            .is_some_and(|end| self.rest_fits(end, rest))
    }

    /// Measure the queued commands `rest` (the printer stack after the group
    /// being decided) from `start_col`, stopping at the first line break. Each
    /// command keeps its already-decided mode. A seeding adapter over
    /// [`Self::line_fits`].
    fn rest_fits(&self, start_col: usize, rest: &[Cmd]) -> bool {
        // `rest` is bottom→top; read commands in reverse, and fill parts in
        // forward order, matching the old reversed-push/pop traversal.
        // A stack command's `Flat` is a *verified* flat (the honest contract:
        // the printer only dispatches `Flat` after measuring the whole flat
        // rendering), so it is seeded verified and pins nested groups exactly
        // as the run loop will.
        self.line_fits_with_seeds(
            start_col,
            Vec::new(),
            CommentFit::SharesLine,
            RestSeeds::new(rest),
        )
    }

    /// Does the *first line* of `node` fit starting at `start_col`? Unlike a
    /// flat simulation ([`Self::flat_end`]), this lets nested [`Ir::Group`]s
    /// decide their own break naturally and treats the first newline that
    /// would actually be emitted as success. A single-line forced-break
    /// `Verbatim` (a standalone comment) fails ([`CommentFit::Fails`]), since
    /// a candidate carrying one can't be rendered flat.
    ///
    /// The seed's `Flat` is a *measurement* flat, not a verified one (nothing
    /// has measured the candidate yet — that is what this call is doing), so
    /// it is seeded unverified and nested groups re-decide.
    fn first_line_fits(&self, start_col: usize, node: &Ir) -> bool {
        self.line_fits(
            start_col,
            vec![(Mode::Flat, false, node)],
            CommentFit::Fails,
        )
    }

    /// The line measurement behind [`Self::rest_fits`]: walk pending `work` items from
    /// `start_col`, each in its mode, until the first newline that would
    /// actually be emitted (a `HardLine`/`EmptyLine`, a `Line`/`SoftLine` in
    /// `Break` mode, a multi-line `Verbatim`'s own embedded newline, or a
    /// break inside a nested node decided `Break`) — success — or the line
    /// width is exceeded — failure. Modes govern trivia; a nested `Group` or
    /// conditional group is re-decided here exactly as the printer will
    /// decide it — including the honest contract: a work item carrying a
    /// *verified* `Mode::Flat` (a stack command seeded by [`Self::rest_fits`],
    /// or a subtree this measurement itself verified via [`Self::flat_end`])
    /// pins its nested groups flat instead of re-deciding, mirroring the run
    /// loop's `Group`/conditional arms.
    ///
    /// A nested group is decided *in the mode it will actually print in*:
    /// flat when its own flat rendering still fits from here, broken
    /// otherwise, with its own hug flags honored. Measuring a doomed group
    /// flat would charge the current line for width that will never land on
    /// it — and the charge would depend on where the doomed group's body
    /// happens to break, which the *previous formatting pass* decided; that
    /// was exactly the expl3 `\EditInstance{a}{b}{ …long keyvals… }`
    /// instability (issue #71). The same rule re-decides a conditional group
    /// through [`Self::pick_candidate`] rather than assuming its flat-most
    /// candidate — the two measurements share this one traversal so they
    /// cannot drift apart again.
    fn line_fits(
        &self,
        start_col: usize,
        work: Vec<(Mode, bool, &Ir)>,
        comments: CommentFit,
    ) -> bool {
        self.line_fits_with_seeds(start_col, work, comments, std::iter::empty())
    }

    fn line_fits_with_seeds<'a>(
        &self,
        start_col: usize,
        mut work: Vec<(Mode, bool, &'a Ir)>,
        comments: CommentFit,
        mut seeds: impl Iterator<Item = (Mode, bool, &'a Ir)>,
    ) -> bool {
        let mut col = start_col;
        while let Some((mode, verified, node)) = work.pop().or_else(|| seeds.next()) {
            match node {
                Ir::Nil | Ir::ZeroWidth(_) => {}
                Ir::SoftLine => {
                    if mode == Mode::Break {
                        return true;
                    }
                }
                Ir::Text(s) | Ir::ColumnZero(s) => {
                    col += s.chars().count();
                    if col > self.line_width {
                        return false;
                    }
                }
                Ir::Verbatim { text, force_break } => {
                    if *force_break && comments == CommentFit::Fails {
                        return false;
                    }
                    // A multi-line verbatim's own embedded newline ends the
                    // line: only its first segment is measured.
                    if let Some((first, _)) = text.split_once('\n') {
                        col += first.chars().count();
                        return col <= self.line_width;
                    }
                    col += text.chars().count();
                    if col > self.line_width {
                        return false;
                    }
                }
                Ir::HardLine | Ir::EmptyLine => return true,
                Ir::Line | Ir::TightLine => match mode {
                    Mode::Flat | Mode::FlatPrefix => {
                        col += 1;
                        if col > self.line_width {
                            return false;
                        }
                    }
                    Mode::Break => return true,
                },
                Ir::Concat(items) => {
                    for item in items.iter().rev() {
                        work.push((mode, verified, item));
                    }
                }
                Ir::Indent(inner) | Ir::Align(_, inner) | Ir::AlignCurrent(inner) => {
                    work.push((mode, verified, inner));
                }
                Ir::BoundedAlign { aligned, fallback } => {
                    // This primitive is emitted at a line's base indentation;
                    // `line_fits` tracks columns but not indentation because an
                    // ordinary break ends its query. Using `col` for both keeps
                    // this branch identical to the run loop at that boundary.
                    let chosen = if mode != Mode::Break
                        || self.bounded_align_fits(col, col, aligned, None)
                    {
                        aligned
                    } else {
                        fallback
                    };
                    work.push((mode, verified, chosen));
                }
                Ir::MarginPrefix { inner, .. } => work.push((mode, verified, inner)),
                Ir::IfBreak { flat, broken } => {
                    work.push((
                        mode,
                        verified,
                        if mode == Mode::Break { broken } else { flat },
                    ));
                }
                Ir::Group {
                    inner, expand, hug, ..
                } => {
                    // Mirrors the run loop's `Group` arm, honest contract
                    // included: under a verified `Flat` the printer pins the
                    // nested group flat (`expand` carve-out aside), so the
                    // measurement must too, or the two diverge. A non-hug
                    // group this measurement decides flat is itself verified
                    // ([`Self::flat_end`] measured its whole flat rendering).
                    let (m, v) = if *expand {
                        (Mode::Break, false)
                    } else if verified && mode == Mode::Flat {
                        (Mode::Flat, true)
                    } else {
                        let measure = if *hug {
                            FlatMeasure::HugPrefix
                        } else {
                            FlatMeasure::Fits
                        };
                        if self.flat_end(col, inner, measure).is_none() {
                            (Mode::Break, false)
                        } else if *hug {
                            (Mode::FlatPrefix, false)
                        } else {
                            (Mode::Flat, true)
                        }
                    };
                    work.push((m, v, inner));
                }
                Ir::ConditionalGroupAllLines(cands) => {
                    // Under a verified `Flat`, the flat-most candidate without
                    // re-picking — exactly the run loop's conditional arms.
                    let (m, chosen) = if verified && mode == Mode::Flat {
                        (Mode::Flat, &cands[0])
                    } else {
                        self.pick_candidate(col, cands)
                    };
                    work.push((m, m == Mode::Flat, chosen));
                }
                Ir::Fill(parts) | Ir::StickyFill(parts) | Ir::HugFill(parts) => {
                    for item in parts.iter().rev() {
                        work.push((mode, verified, item));
                    }
                }
                Ir::PreferredFill { atoms, .. } => match mode {
                    // A preferred fill chooses its own breaks: in `Break` mode it
                    // breaks at a gap, so its first line ends after (at most) the
                    // first atom — the line fits iff that atom fits here.
                    // Measuring the whole fill flat would spuriously fail and
                    // force an enclosing group to break. In `Flat`/`FlatPrefix`
                    // mode the whole fill stays on the line.
                    Mode::Break => {
                        return atoms.first().is_none_or(|atom| {
                            self.line_fits(col, vec![(Mode::Flat, false, atom)], comments)
                        });
                    }
                    Mode::Flat | Mode::FlatPrefix => {
                        match self.preferred_fill_flat_end(col, atoms) {
                            Some(end) => col = end,
                            None => return false,
                        }
                    }
                },
            }
        }
        col <= self.line_width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Preserve the original eager stack adapter independently of RestSeeds.
    fn eager_rest_fits(printer: &Printer, start_col: usize, rest: &[Cmd]) -> bool {
        let mut work = Vec::new();
        for command in rest {
            match command {
                Cmd::Node { mode, node, .. } => {
                    record_fit_seed_item();
                    work.push((*mode, *mode == Mode::Flat, *node));
                }
                Cmd::Fill { mode, parts, .. } => {
                    for part in parts.iter().rev() {
                        record_fit_seed_item();
                        work.push((*mode, *mode == Mode::Flat, part));
                    }
                }
                Cmd::PreferredFill {
                    mode, atoms, index, ..
                } => {
                    record_fit_seed_item();
                    work.push((*mode, *mode == Mode::Flat, &atoms[*index]));
                }
            }
        }
        printer.line_fits(start_col, work, CommentFit::SharesLine)
    }

    #[test]
    fn lazy_fit_seeds_match_eager_stack_order_and_modes() {
        let pool = vec![
            Ir::Nil,
            Ir::ZeroWidth("hidden".into()),
            Ir::text("abc"),
            Ir::text("λ😀"),
            Ir::HardLine,
            Ir::EmptyLine,
            Ir::Line,
            Ir::TightLine,
            Ir::SoftLine,
            Ir::verbatim("one\ntwo"),
            Ir::Verbatim {
                text: "comment".into(),
                force_break: true,
            },
            Ir::group(Ir::concat([Ir::text("ab"), Ir::Line, Ir::text("c")])),
            Ir::group_hug(Ir::concat([Ir::text("ab"), Ir::HardLine, Ir::text("c")])),
            Ir::Group {
                inner: Rc::new(Ir::concat([Ir::text("ab"), Ir::Line, Ir::text("c")])),
                expand: true,
                hug: false,
                forced_break: false,
            },
            Ir::IfBreak {
                flat: Rc::new(Ir::text("flat")),
                broken: Rc::new(Ir::HardLine),
            },
            Ir::ConditionalGroupAllLines(vec![Ir::text("abcdef"), Ir::HardLine].into()),
            Ir::Fill(vec![Ir::text("a"), Ir::Line, Ir::text("b")].into()),
            Ir::PreferredFill {
                atoms: vec![Ir::text("a"), Ir::text("b")].into(),
                preferred: vec![true].into(),
                target: 3,
            },
        ];
        let modes = [Mode::Flat, Mode::FlatPrefix, Mode::Break];
        for first in 0..pool.len() {
            for second in 0..pool.len() {
                let parts = [pool[first].clone(), pool[second].clone()];
                for left_mode in modes {
                    for right_mode in modes {
                        for kind in 0..3 {
                            let left = match kind {
                                0 => Cmd::Node {
                                    indent: 7,
                                    mode: left_mode,
                                    node: &parts[0],
                                    prefix: None,
                                },
                                1 => Cmd::Fill {
                                    indent: 7,
                                    mode: left_mode,
                                    parts: &parts,
                                    prefix: None,
                                    sticky: true,
                                    hug: true,
                                    broken: true,
                                },
                                _ => Cmd::PreferredFill {
                                    indent: 7,
                                    mode: left_mode,
                                    atoms: &parts,
                                    breaks: vec![true].into(),
                                    index: first % parts.len(),
                                    prefix: None,
                                },
                            };
                            let rest = [
                                left,
                                Cmd::Node {
                                    indent: 3,
                                    mode: right_mode,
                                    node: &parts[1],
                                    prefix: None,
                                },
                            ];
                            for width in [0, 1, 3, 8] {
                                let printer = Printer::new(FormatStyle {
                                    line_width: width,
                                    ..FormatStyle::default()
                                });
                                for column in [0, 2, 9] {
                                    assert_eq!(
                                        printer.rest_fits(column, &rest),
                                        eager_rest_fits(&printer, column, &rest),
                                        "nodes {first}/{second}, kind {kind}, width {width}, column {column}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        let printer = Printer::new(FormatStyle {
            line_width: 5,
            ..FormatStyle::default()
        });
        assert_eq!(printer.rest_fits(6, &[]), eager_rest_fits(&printer, 6, &[]));
    }

    #[test]
    fn lazy_fit_stops_at_overflow_and_keeps_same_line_suffixes() {
        let printer = Printer::new(FormatStyle {
            line_width: 5,
            ..FormatStyle::default()
        });
        let long = Ir::text("too long");
        let small = Ir::text("abc");
        let break_line = Ir::HardLine;
        let empty = [];
        let rest = [
            Cmd::Node {
                indent: 0,
                mode: Mode::Break,
                node: &break_line,
                prefix: None,
            },
            Cmd::Node {
                indent: 0,
                mode: Mode::Flat,
                node: &long,
                prefix: None,
            },
            Cmd::Fill {
                indent: 0,
                mode: Mode::Break,
                parts: &empty,
                prefix: None,
                sticky: false,
                hug: false,
                broken: false,
            },
        ];
        FIT_SEED_ITEMS.with(|count| count.set(0));
        assert!(!printer.rest_fits(0, &rest));
        assert_eq!(FIT_SEED_ITEMS.with(std::cell::Cell::get), 1);
        let suffix = [Cmd::Node {
            indent: 0,
            mode: Mode::Flat,
            node: &small,
            prefix: None,
        }];
        assert!(
            !printer.group_fits(0, &small, &suffix),
            "genuine trailing overflow still breaks the group"
        );
        assert!(printer.group_fits(0, &small, &[]));
    }

    #[test]
    fn rest_fit_does_not_seed_work_after_its_first_break() {
        let printer = Printer::new(FormatStyle::default());
        let future = (0..4096).map(|_| Ir::text("future")).collect::<Vec<_>>();
        let hard_line = Ir::hard_line();
        let rest = [
            Cmd::Fill {
                indent: 0,
                mode: Mode::Break,
                parts: &future,
                prefix: None,
                sticky: false,
                hug: false,
                broken: false,
            },
            Cmd::Node {
                indent: 0,
                mode: Mode::Break,
                node: &hard_line,
                prefix: None,
            },
        ];
        FIT_SEED_ITEMS.with(|count| count.set(0));
        assert!(
            printer.rest_fits(0, &rest),
            "the next physical break ends the query"
        );
        let seeded = FIT_SEED_ITEMS.with(std::cell::Cell::get);
        assert_eq!(
            seeded, 1,
            "future commands and fill parts must stay untouched"
        );
        let parts = std::iter::once(hard_line).chain(future).collect::<Vec<_>>();
        let inside_fill = [Cmd::Fill {
            indent: 0,
            mode: Mode::Break,
            parts: &parts,
            prefix: None,
            sticky: false,
            hug: false,
            broken: false,
        }];
        FIT_SEED_ITEMS.with(|count| count.set(0));
        assert!(printer.rest_fits(0, &inside_fill));
        assert_eq!(
            FIT_SEED_ITEMS.with(std::cell::Cell::get),
            1,
            "later parts of the same fill must stay untouched"
        );
        FIT_SEED_ITEMS.with(|count| count.set(0));
        assert!(eager_rest_fits(&printer, 0, &inside_fill));
        assert_eq!(FIT_SEED_ITEMS.with(std::cell::Cell::get), parts.len());
    }

    #[test]
    fn real_paragraphs_keep_output_without_seeding_future_blocks() {
        use crate::formatter::format_node;
        use tex_ls_parser::parser::parse;
        const COUNT: usize = 1024;
        let source = "ordinary alpha beta.\n\n".repeat(COUNT);
        let parsed = parse(&source);
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.syntax().text().to_string(), source, "lossless input");
        FIT_SEED_ITEMS.with(|count| count.set(0));
        let formatted = format_node(&parsed.syntax(), FormatStyle::default()).unwrap();
        let seeded = FIT_SEED_ITEMS.with(std::cell::Cell::get);
        assert_eq!(
            formatted,
            format!("{}\n", source.trim_end()),
            "exact canonical bytes"
        );
        let after = parse(&formatted);
        assert!(after.errors.is_empty());
        assert_eq!(
            after.syntax().text().to_string(),
            formatted,
            "lossless output"
        );
        assert_eq!(
            format_node(&after.syntax(), FormatStyle::default()).unwrap(),
            formatted,
            "idempotence"
        );
        assert!(
            seeded >= COUNT - 1,
            "actual paragraph groups must exercise the fit continuation"
        );
        assert!(
            seeded <= COUNT * 8,
            "fit continuation exceeds linear work: {seeded} seeds for {COUNT} paragraphs"
        );
    }

    #[test]
    fn long_overflowing_fills_have_linear_candidate_counts() {
        let printer = Printer::new(FormatStyle {
            line_width: 80,
            ..FormatStyle::default()
        });
        let atoms: Vec<_> = (0..4096).map(|_| Ir::text("x".repeat(90))).collect();
        let preferred: Vec<_> = (0..atoms.len() - 1).map(|gap| gap % 5 == 0).collect();
        for margin in [0, 80, 81, 82] {
            // stable_breaks asserts its candidate count under cfg(test).
            let breaks = printer.stable_breaks(margin, margin, None, &atoms, &preferred, 72);
            let expected = if margin <= 80 {
                vec![true; preferred.len()]
            } else if margin == 81 {
                preferred.clone()
            } else {
                vec![false; preferred.len()]
            };
            assert_eq!(&*breaks, &expected);
        }
    }

    #[test]
    fn overflowing_margin_preserves_authored_suffix_gaps() {
        let printer = Printer::new(FormatStyle {
            line_width: 5,
            ..FormatStyle::default()
        });
        let atoms = [
            Ir::Text("".into()),
            Ir::text("a"),
            Ir::Text("".into()),
            Ir::text("bb"),
            Ir::text("c"),
        ];
        let preferred = [true, false, true, false];
        for first in [0, 6, 20] {
            assert_eq!(
                printer.stable_breaks(first, 6, None, &atoms, &preferred, 4),
                printer.stable_breaks_reference(first, 6, None, &atoms, &preferred, 4)
            );
        }
        assert_eq!(
            &*printer.stable_breaks(6, 6, None, &atoms, &preferred, 4),
            &preferred
        );
    }

    #[test]
    fn preferred_fill_does_not_increase_overflow_to_reach_wide_indentation() {
        let printer = Printer::new(FormatStyle {
            line_width: 80,
            ..FormatStyle::default()
        });
        let atoms = [Ir::text("a".repeat(40)), Ir::text("b".repeat(40))];
        assert_eq!(
            &*printer.stable_breaks(0, 100, None, &atoms, &[true], 72),
            &[false]
        );
        // Even an indent that fits can make a very short fitting prefix a
        // worse split point than retaining a small overflow on the first line.
        let printer = Printer::new(FormatStyle {
            line_width: 5,
            ..FormatStyle::default()
        });
        let atoms = [Ir::text("a"), Ir::text("bbbbb")];
        assert_eq!(
            &*printer.stable_breaks(0, 5, None, &atoms, &[true], 5),
            &[false]
        );
    }

    #[test]
    fn optimized_preferred_breaks_match_scan_reference() {
        // Mixed empty, narrow and overflowing atoms; authored masks; prefixes
        // and indent beyond the width all participate in exact tie decisions.
        for width in [0, 1, 5, 12] {
            let printer = Printer::new(FormatStyle {
                line_width: width,
                ..FormatStyle::default()
            });
            for seed in 0usize..256 {
                let atoms: Vec<_> = (0..6)
                    .map(|i| {
                        let size = [0, 1, 4, 17][(seed >> (i % 4 * 2)) & 3];
                        Ir::Text("x".repeat(size).into())
                    })
                    .collect();
                let preferred: Vec<_> = (0..5).map(|i| seed & (1 << i) != 0).collect();
                for (first, indent) in [(0, 0), (3, 1), (20, width), (0, width + 1)] {
                    for target in [0, width / 2, width + 1] {
                        for prefix in [
                            None,
                            Some(ActivePrefix {
                                line: "%%% ",
                                blank: None,
                            }),
                        ] {
                            let actual = printer
                                .stable_breaks(first, indent, prefix, &atoms, &preferred, target);
                            let reference = printer.stable_breaks_reference(
                                first, indent, prefix, &atoms, &preferred, target,
                            );
                            assert_eq!(
                                actual, reference,
                                "width={width} seed={seed} first={first} indent={indent} target={target}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tight_line_renders_glued_but_keeps_the_line_fit_budget() {
        let ir = Ir::group(Ir::concat([
            Ir::text("aa"),
            Ir::tight_line(),
            Ir::text("b"),
        ]));
        let wide = Printer::new(FormatStyle {
            line_width: 4,
            ..FormatStyle::default()
        });
        let narrow = Printer::new(FormatStyle {
            line_width: 3,
            ..FormatStyle::default()
        });

        assert_eq!(wide.print(&ir), "aab");
        assert_eq!(narrow.print(&ir), "aa\nb");
    }

    /// A block that always breaks: `{`, an indented body, then `}`.
    fn block() -> Ir {
        Ir::concat([
            Ir::text("{"),
            Ir::indent(Ir::concat([Ir::hard_line(), Ir::text("body")])),
            Ir::hard_line(),
            Ir::text("}"),
        ])
    }

    /// `f(a, {block})` as a hug group: prefix `f(a, ` then a trailing block.
    fn hug_call() -> Ir {
        Ir::group_hug(Ir::concat([
            Ir::text("f("),
            Ir::indent(Ir::concat([
                Ir::soft_line(),
                Ir::text("a"),
                Ir::if_break(Ir::text(", "), Ir::text(",")),
            ])),
            Ir::if_break(block(), Ir::indent(Ir::concat([Ir::soft_line(), block()]))),
            Ir::soft_line(),
            Ir::text(")"),
        ]))
    }

    #[test]
    fn hug_group_keeps_prefix_flat_when_it_fits() {
        let printer = Printer::new(FormatStyle::default());
        assert_eq!(printer.print(&hug_call()), "f(a, {\n  body\n})");
    }

    #[test]
    fn hug_group_expands_when_prefix_does_not_fit() {
        // A narrow line forces even the short prefix `f(a, {` to break.
        let style = FormatStyle {
            line_width: 5,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        assert_eq!(
            printer.print(&hug_call()),
            "f(\n  a,\n  {\n    body\n  }\n)"
        );
    }

    #[test]
    fn hug_group_expands_when_prefix_has_a_comment() {
        // A forced-break verbatim (a comment) in the prefix prevents hugging
        // even though the prefix is short.
        let printer = Printer::new(FormatStyle::default());
        let ir = Ir::group_hug(Ir::concat([
            Ir::text("f("),
            Ir::indent(Ir::concat([
                Ir::soft_line(),
                Ir::verbatim_forced("# c"),
                Ir::hard_line(),
                Ir::text("a"),
                Ir::if_break(Ir::text(", "), Ir::text(",")),
            ])),
            Ir::if_break(block(), Ir::indent(Ir::concat([Ir::soft_line(), block()]))),
            Ir::soft_line(),
            Ir::text(")"),
        ]));
        // Expanded: the comment lands on its own line and the block is indented.
        assert_eq!(printer.print(&ir), "f(\n  # c\n  a,\n  {\n    body\n  }\n)");
    }

    #[test]
    fn margin_prefix_re_emits_on_every_wrapped_line() {
        // An opaque `> ` prefix (the engine attaches no LaTeX semantics): a fill of
        // four words at width 8 wraps to two-per-line, each line re-opening with
        // the prefix flush at column 0, content measured from after it.
        let style = FormatStyle {
            line_width: 8,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        let fill = Ir::fill([
            Ir::text("aa"),
            Ir::text("bb"),
            Ir::text("cc"),
            Ir::text("dd"),
        ]);
        let ir = Ir::margin_prefix("> ", fill);
        assert_eq!(printer.print(&ir), "> aa bb\n> cc dd");
    }

    #[test]
    fn margin_prefix_emits_first_line_after_a_leading_break() {
        // The leading break is emitted by the parent (outside the MarginPrefix
        // scope); entering the scope still flushes the prefix for the first line.
        let printer = Printer::new(FormatStyle::default());
        let ir = Ir::concat([
            Ir::text("head"),
            Ir::hard_line(),
            Ir::margin_prefix("% ", Ir::text("body")),
        ]);
        assert_eq!(printer.print(&ir), "head\n% body");
    }

    #[test]
    fn conditional_group_all_lines_pins_the_verified_candidate() {
        // The all-lines pick *did* verify the candidate's whole flat
        // rendering, so its `Flat` is honored downstream: the nested group
        // keeps exactly the layout that was measured instead of re-deciding
        // against the trailing text and detonating into a hybrid the
        // measurement never saw (`abc\ndefXXXX`).
        let style = FormatStyle {
            line_width: 10,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        let candidate = Ir::group(Ir::concat([Ir::text("abc"), Ir::line(), Ir::text("def")]));
        let ir = Ir::concat([
            Ir::conditional_group_all_lines([candidate]),
            Ir::text("XXXX"),
        ])
        .propagate_breaks();
        assert_eq!(printer.print(&ir), "abc defXXXX");
    }

    #[test]
    fn preferred_fill_atoms_inherit_the_fill_mode() {
        // The break plan places atoms, it does not verify them: in `Break`
        // mode an atom is dispatched `Break`, so a mode-sensitive child (here
        // an `IfBreak`) sees the honest mode instead of an unverified `Flat`.
        let printer = Printer::new(FormatStyle::default());
        let ir = Ir::preferred_fill(
            [Ir::text("aa"), Ir::if_break(Ir::text("F"), Ir::text("B"))],
            vec![false],
            10,
        );
        assert_eq!(printer.print(&ir), "aa B");
    }

    #[test]
    fn hug_prefix_claim_does_not_pin_a_group_past_the_forced_break() {
        // The hug's `fits` stops successfully at the first forced break, so a
        // soft group *after* the detonating block (a second trailing brace
        // argument, `\@@_if_key_value:VTF {T}{F}`) was never measured. It must
        // re-decide for itself under the hug's `FlatPrefix` — here it does not
        // fit and breaks — instead of inheriting an unverified `Flat`.
        let style = FormatStyle {
            line_width: 20,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        let t_branch = Ir::group(Ir::concat([
            Ir::text("{"),
            Ir::indent(Ir::concat([Ir::hard_line(), Ir::text("tt")])),
            Ir::hard_line(),
            Ir::text("}"),
        ]));
        let f_branch = Ir::group(Ir::concat([
            Ir::text("{"),
            Ir::indent(Ir::concat([Ir::line(), Ir::text("ffffffffffffffffffff")])),
            Ir::line(),
            Ir::text("}"),
        ]));
        let ir = Ir::group_hug(Ir::concat([
            Ir::text("\\head:TF"),
            Ir::line(),
            Ir::concat([
                Ir::text("\\v"),
                Ir::indent(Ir::concat([Ir::hard_line(), t_branch])),
                Ir::indent(Ir::concat([Ir::hard_line(), f_branch])),
            ]),
        ]))
        .propagate_breaks();
        assert_eq!(
            printer.print(&ir),
            "\\head:TF \\v\n  {\n    tt\n  }\n  {\n    ffffffffffffffffffff\n  }"
        );
    }

    #[test]
    fn hug_prefix_flat_does_not_pin_the_expanded_block() {
        // A hug verifies only its prefix, so its `Flat` must not reach into
        // the detonating block: the block group is `expand`-marked by
        // `propagate_breaks` and still prints `Break` (its `Line` breaks)
        // even though it is dispatched under the hug's flat prefix.
        let printer = Printer::new(FormatStyle::default());
        let block = Ir::group(Ir::concat([
            Ir::text("{"),
            Ir::indent(Ir::concat([
                Ir::hard_line(),
                Ir::text("x"),
                Ir::line(),
                Ir::text("y"),
            ])),
            Ir::hard_line(),
            Ir::text("}"),
        ]));
        let ir = Ir::group_hug(Ir::concat([Ir::text("f(a, "), block])).propagate_breaks();
        assert_eq!(printer.print(&ir), "f(a, {\n  x\n  y\n}");
    }

    #[test]
    fn fill_keeps_everything_on_one_line_when_it_fits() {
        let printer = Printer::new(FormatStyle::default());
        let ir = Ir::fill([Ir::text("a"), Ir::text("b"), Ir::text("c")]);
        assert_eq!(printer.print(&ir), "a b c");
    }

    #[test]
    fn fill_drops_nil_atoms_without_phantom_spaces() {
        let printer = Printer::new(FormatStyle::default());
        let ir = Ir::fill([Ir::text("a"), Ir::Nil, Ir::text("b")]);
        assert_eq!(printer.print(&ir), "a b");
    }

    #[test]
    fn preferred_fill_drops_nil_atoms_and_keeps_the_break_mask_aligned() {
        // A `Nil` atom is filtered out of `atoms`; its gap must be dropped from
        // `preferred` in lockstep, or the mask misindexes the surviving gaps (and
        // trips the `stable_breaks` debug assertions / panics on release).
        let printer = Printer::new(FormatStyle::default());
        // `preferred` has one bool per gap of the *unfiltered* three-atom run.
        let ir = Ir::preferred_fill(
            [Ir::text("a"), Ir::Nil, Ir::text("b")],
            vec![true, false],
            10,
        );
        // Fits on one line, so no break is taken regardless of the mask.
        assert_eq!(printer.print(&ir), "a b");
    }

    #[test]
    fn first_line_fits_lets_a_broken_group_break_its_preferred_fill() {
        // Five 3-char words: flat width is 5*3 + 4 gaps = 19 > 10, so the fill
        // cannot lie flat. But it chooses its own breaks, so a broken group
        // around it has a first line of just the first word (3 cols), which fits.
        // A flat-only measurement would spuriously report the first line as too
        // wide and force needless breaking upstream.
        let style = FormatStyle {
            line_width: 10,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        let fill = Ir::preferred_fill(
            [
                Ir::text("aaa"),
                Ir::text("bbb"),
                Ir::text("ccc"),
                Ir::text("ddd"),
                Ir::text("eee"),
            ],
            vec![false; 4],
            10,
        );
        // A break-aware group so `first_line_fits` reaches the fill in `Break`.
        let group = Ir::group(fill);
        assert!(
            printer.first_line_fits(0, &group),
            "a broken group should let its preferred fill break for a fitting first line"
        );
    }

    #[test]
    fn preferred_fill_keeps_an_authored_break_after_a_dropped_nil() {
        // Width 1 forces every gap to break; the authored-break bit that survives
        // the `Nil` drop still drives the newline, proving the mask stayed aligned.
        let style = FormatStyle {
            line_width: 1,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        let ir = Ir::preferred_fill(
            [Ir::text("a"), Ir::Nil, Ir::text("b")],
            vec![false, true],
            1,
        );
        assert_eq!(printer.print(&ir), "a\nb");
    }

    #[test]
    fn fill_wraps_words_greedily_at_the_width() {
        let style = FormatStyle {
            line_width: 10,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        // "aaa bbb" (7) fits; adding " ccc" would reach 11 > 10, so break; then
        // "ccc ddd" (7) fits. The break is decided per gap, not all-or-nothing.
        let ir = Ir::fill([
            Ir::text("aaa"),
            Ir::text("bbb"),
            Ir::text("ccc"),
            Ir::text("ddd"),
        ]);
        assert_eq!(printer.print(&ir), "aaa bbb\nccc ddd");
    }

    #[test]
    fn fill_continuation_lines_take_the_current_indent() {
        let style = FormatStyle {
            line_width: 6,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        // Inside an indent: "aa bb" (5) fits on the first line (which carries no
        // leading indent here), then "cc" wraps to a fresh line at indent 2.
        let ir = Ir::indent(Ir::fill([Ir::text("aa"), Ir::text("bb"), Ir::text("cc")]));
        assert_eq!(printer.print(&ir), "aa bb\n  cc");
    }

    #[test]
    fn align_hangs_continuation_to_marker_width() {
        let style = FormatStyle {
            line_width: 12,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        // A `\item `-style marker (width 6) followed by a hanging-indented fill:
        // the first word sits after the marker, wrapped words align under it.
        let ir = Ir::concat([
            Ir::text("* "),
            Ir::align(
                2,
                Ir::fill([Ir::text("aa"), Ir::text("bbbb"), Ir::text("cc")]),
            ),
        ]);
        // "* aa" = 4, +" bbbb" = 9, +" cc" = 12 <= 12, so it all fits on one line.
        assert_eq!(printer.print(&ir), "* aa bbbb cc");
        // Narrower: force a wrap and check the continuation aligns to column 2.
        let narrow = Printer::new(FormatStyle {
            line_width: 9,
            indent_width: 2,
            ..FormatStyle::default()
        });
        assert_eq!(narrow.print(&ir), "* aa bbbb\n  cc");
    }

    #[test]
    fn align_current_hangs_from_rendered_start_column() {
        let printer = Printer::new(FormatStyle::default());
        let block = Ir::align_current(Ir::concat([
            Ir::text("open"),
            Ir::hard_line(),
            Ir::text("close"),
        ]));

        assert_eq!(
            printer.print(&Ir::concat([Ir::text("lhs "), block.clone()])),
            "lhs open\n    close"
        );

        // Prefix bytes occupy real columns but are re-emitted independently,
        // so they must not be counted again as continuation indentation.
        assert_eq!(
            printer.print(&Ir::margin_prefix(
                "% ",
                Ir::concat([Ir::text("lhs "), block])
            )),
            "% lhs open\n%     close"
        );
    }

    #[test]
    fn bounded_align_measures_continuations_from_the_current_column() {
        let printer = Printer::new(FormatStyle {
            line_width: 10,
            ..FormatStyle::default()
        });
        let aligned = Ir::align(6, Ir::concat([Ir::text("lhs"), Ir::line(), Ir::text("r")]));
        let fallback = Ir::concat([Ir::text("lhs"), Ir::line(), Ir::text("r")]);
        let ir = Ir::bounded_align(aligned, fallback);

        assert_eq!(
            printer.run_with_mode(&ir, 0, 0, Mode::Break),
            "lhs\n      r"
        );
        assert_eq!(printer.run_with_mode(&ir, 4, 4, Mode::Break), "lhs\n    r");

        // A virtual-document margin contributes to every continuation's real
        // column. Without the active prefix, the six-column hang would appear
        // to fit at width 8; `% ` makes it overflow, selecting the fallback.
        let prefixed = Ir::margin_prefix("% ", ir);
        let narrow = Printer::new(FormatStyle {
            line_width: 8,
            ..FormatStyle::default()
        });
        assert_eq!(narrow.print(&prefixed), "% lhs\n% r");
    }

    #[test]
    fn hug_fill_keeps_a_detonating_atom_on_the_head_line() {
        let printer = Printer::new(FormatStyle::default());
        let parts = [
            Ir::text("head"),
            Ir::line(),
            Ir::concat([Ir::text("cmd"), block()]),
        ];
        // A plain fill has no flat width for the block-bearing atom, so its gap
        // breaks and the atom starts a line of its own.
        let plain = Ir::Fill(parts.to_vec().into()).propagate_breaks();
        assert_eq!(printer.print(&plain), "head\ncmd{\n  body\n}");
        // The hugging fill measures the atom's first line instead: `cmd{` fits
        // after `head `, so the pair stays joined and only the body breaks.
        let hug = Ir::HugFill(parts.to_vec().into()).propagate_breaks();
        assert_eq!(printer.print(&hug), "head cmd{\n  body\n}");
    }

    #[test]
    fn hug_fill_hugs_its_last_atom_too() {
        // The rest-awareness that keeps a *flat* last atom honest must not
        // demote a hug claim: the same atom mid-fill and last-in-fill has to
        // land in the same place, since a statement can end one atom earlier on
        // the next pass.
        let printer = Printer::new(FormatStyle::default());
        let last = Ir::HugFill(
            vec![
                Ir::text("head"),
                Ir::line(),
                Ir::concat([Ir::text("cmd"), block()]),
            ]
            .into(),
        )
        .propagate_breaks();
        let mid = Ir::HugFill(
            vec![
                Ir::text("head"),
                Ir::line(),
                Ir::concat([Ir::text("cmd"), block()]),
                Ir::line(),
                Ir::text("tail"),
            ]
            .into(),
        )
        .propagate_breaks();
        assert_eq!(printer.print(&last), "head cmd{\n  body\n}");
        assert_eq!(printer.print(&mid), "head cmd{\n  body\n}\ntail");
    }

    #[test]
    fn hug_fill_breaks_when_even_the_first_line_does_not_fit() {
        let style = FormatStyle {
            line_width: 8,
            indent_width: 2,
            ..FormatStyle::default()
        };
        let printer = Printer::new(style);
        let ir = Ir::HugFill(
            vec![
                Ir::text("head"),
                Ir::line(),
                Ir::concat([Ir::text("command"), block()]),
            ]
            .into(),
        )
        .propagate_breaks();
        assert_eq!(printer.print(&ir), "head\ncommand{\n  body\n}");
    }

    #[test]
    fn doc_margin_uses_a_bare_percent_on_blank_lines() {
        let printer = Printer::new(FormatStyle::default());
        let ir = Ir::doc_margin(Ir::concat([
            Ir::text("first"),
            Ir::empty_line(),
            Ir::text("second"),
        ]));
        assert_eq!(printer.print(&ir), "% first\n%\n% second");
    }
}
