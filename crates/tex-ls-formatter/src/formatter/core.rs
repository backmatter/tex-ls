//! The formatter entry points and the CST → `Ir` lowering.
//!
//! Lowering normalizes whitespace and indentation, reflows prose arguments under
//! [`WrapMode::Reflow`], formats structured math, and owns layout inside expl3
//! regions. Protected regions remain verbatim.
//!
//! `lower_node` contains the LaTeX-specific lowering. The surrounding format
//! entry points and the Wadler-style `Ir` printer are language-independent.

mod trivia;
use trivia::*;
mod math;
use math::*;
mod commands;
use commands::*;
mod groups;
use groups::*;
mod alignment;
pub use alignment::is_paren_trim_word;
use alignment::*;
mod lists;
use lists::*;
mod environments;
use environments::*;
mod expl;
use expl::*;
mod prose;
use prose::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::iter::Peekable;

use rowan::{TextRange, TextSize};

use super::colspec::{self, ColAlign};
use tex_ls_parser::ast::{AstNode, Environment, Group, command_name};
use tex_ls_parser::declarations::ResolvedDeclarations;
use tex_ls_parser::directives;
use tex_ls_parser::parser::is_def_prefix_command;
use tex_ls_parser::parser::lexer::{ExplToggle, expl_toggle};
use tex_ls_parser::parser::{LatexFlavor, parse_with_declarations, parse_with_flavor};
use tex_ls_parser::semantic::expl3::{StatementMap, segment_expl_statements};
use tex_ls_parser::semantic::tikz::statement_glue;
use tex_ls_parser::semantic::{
    ArgKind, ArgSpec, ArgumentDomain, CitationPlacement, ContentKind, DelimiterRole, MathClass,
    SignatureDb, Signatures, expl3, match_arg_slot, match_verbatim_arg_slot, math_atoms,
    scan_definitions,
};
use tex_ls_parser::syntax::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, is_collapsible_trivia, is_param_digit,
    is_trivia,
};

use super::context::FormatContext;
use super::ir::Ir;
use super::printer::Printer;
use super::sentence::{ResolvedProfile, SentenceOptions, is_sentence_boundary_text};
use super::style::{
    FormatStyle, ItemIndent, LineEnding, MathWrap, WrapMode, apply_line_ending, detect_line_ending,
};

/// Why a document could not be formatted. The formatter only operates on a clean
/// parse: anything the parser flagged, or any `ERROR` token, is refused rather
/// than silently reshaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The input parsed with `count` syntax error(s); the formatter only
    /// supports input the parser accepts without diagnostics.
    ParseErrors { count: usize },
    /// The CST contains an `ERROR` token the lowering does not handle.
    UnsupportedConstruct { kind: SyntaxKind, snippet: String },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParseErrors { count } => write!(
                f,
                "input contains {count} parser diagnostic(s); formatter only supports parseable input"
            ),
            Self::UnsupportedConstruct { kind, snippet } => {
                write!(
                    f,
                    "unsupported construct for formatter: {kind:?} near {snippet:?}"
                )
            }
        }
    }
}

impl std::error::Error for FormatError {}

/// Format `input` with the default [`FormatStyle`].
pub fn format(input: &str) -> Result<String, FormatError> {
    format_with_style(input, FormatStyle::default())
}

/// Format `input` under `style`. Returns [`FormatError`] if the input does not
/// parse cleanly. Note: tex-ls's [`tex_ls_parser::parser::Parse`] carries `errors` +
/// `syntax()`. Uses the
/// [`Document`](LatexFlavor::Document) flavor; [`format_with_style_flavored`] is
/// the entry for `.sty`/`.cls`.
pub fn format_with_style(input: &str, style: FormatStyle) -> Result<String, FormatError> {
    format_with_style_flavored(input, style, LatexFlavor::Document)
}

/// Like [`format_with_style`] but parses `input` under an explicit
/// [`LexConfig`](tex_ls_parser::parser::LexConfig), so a [`Package`](LatexFlavor::Package) flavor (`.sty`/`.cls`)
/// lexes with `@` as a letter (the implicit `\makeatletter`) and a `.dtx` runs
/// the docstrip mode. A bare [`LatexFlavor`] coerces in. The wrap mode is a
/// `style` concern, decided by the caller and independent of the flavor; it
/// defaults to [`WrapMode::Reflow`] for every file kind.
pub fn format_with_style_flavored(
    input: &str,
    style: FormatStyle,
    config: impl Into<tex_ls_parser::parser::LexConfig>,
) -> Result<String, FormatError> {
    format_with_style_flavored_with_signatures(input, style, config, &SignatureDb::default())
}

/// Like [`format_with_style_flavored`] but with explicit
/// [`SentenceOptions`] for the
/// `sentence`/`semantic` wrap modes (the document language and user no-break
/// abbreviations). The CLI resolves these from `tex-ls.toml`; other wrap modes
/// ignore them.
pub fn format_with_style_flavored_sentence(
    input: &str,
    style: FormatStyle,
    config: impl Into<tex_ls_parser::parser::LexConfig>,
    sentence: SentenceOptions<'_>,
) -> Result<String, FormatError> {
    let parsed = parse_with_flavor(input, config);
    if !parsed.errors.is_empty() {
        return Err(FormatError::ParseErrors {
            count: parsed.errors.len(),
        });
    }
    format_node_with_signatures_sentence(&parsed.syntax(), style, &SignatureDb::default(), sentence)
}

/// Like [`format_with_style_flavored_sentence`] but under a project's
/// [declarations](tex_ls_parser::declarations), and with no other signature scope.
///
/// This is the entry for content with no path to anchor local `.sty`/`.cls`
/// resolution against: the CLI's **stdin**, the language server's
/// cache-miss/cancellation fallback, and a WASM embedder, which is sandboxed
/// with no filesystem at all. Each of those would otherwise reach for
/// [`format_with_style_flavored_sentence`], which parses declaration-blind — and
/// a formatter honoring `[environments.…]` for `tex-ls format file.tex` but not
/// for `tex-ls format < file.tex` (nor for the one editor request that races an
/// edit) would be a trap.
///
/// The declarations reach **both** the parse and the signature scope, exactly as
/// they do on the path-bearing entries in the `tex-ls` crate
/// (`formatter::format_file_with_packages_sentence`), so this differs from the
/// full path only by the package tiers it cannot reach — never by precedence,
/// and never by which constructs it recognizes.
pub fn format_with_declarations_sentence(
    input: &str,
    style: FormatStyle,
    config: impl Into<tex_ls_parser::parser::LexConfig>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
) -> Result<String, FormatError> {
    let parsed = parse_with_declarations(input, config, declared);
    if !parsed.errors.is_empty() {
        return Err(FormatError::ParseErrors {
            count: parsed.errors.len(),
        });
    }
    format_node_with_signatures_sentence(
        &parsed.syntax(),
        style,
        &declared_scope(declared),
        sentence,
    )
}

/// The signature scope a project's declarations alone make up: the top tier of
/// the disk- and salsa-backed scopes (`semantic::collect_package_signatures`,
/// `incremental::scope_signatures`, both in the `tex-ls` crate), with nothing
/// under it.
pub fn declared_scope(declared: &ResolvedDeclarations) -> SignatureDb {
    let mut scope = SignatureDb::default();
    scope.merge_declarations(declared);
    scope
}

/// Like [`format_with_style_flavored`] but additionally folds an `external`
/// signature scope — the merged definitions of the document's loaded local
/// packages (`semantic::load::collect_package_signatures` and the salsa-cached
/// `incremental::scope_signatures`, both in the `tex-ls` crate) — into the lowering, so calls to
/// package-defined macros are shaped by their real arity/verbatim-ness. The
/// document's own definitions always win over `external`. The CLI uses this for a
/// real file path; passing an empty DB recovers [`format_with_style_flavored`].
pub fn format_with_style_flavored_with_signatures(
    input: &str,
    style: FormatStyle,
    config: impl Into<tex_ls_parser::parser::LexConfig>,
    external: &SignatureDb,
) -> Result<String, FormatError> {
    let parsed = parse_with_flavor(input, config);
    if !parsed.errors.is_empty() {
        return Err(FormatError::ParseErrors {
            count: parsed.errors.len(),
        });
    }

    format_node_with_signatures(&parsed.syntax(), style, external)
}

/// Format an already-parsed CST `root` under `style`. This is the
/// reparse-free entry: the language server hands it the salsa-cached tree
/// (`db.parsed_tree`) instead of re-running the parser. The caller owns the
/// `ParseErrors` guard — this entry assumes the parse was clean and only
/// enforces the `ERROR`-token invariant (`validate_supported_tokens`).
/// [`format_with_style`] is the parse-then-format convenience wrapper.
pub fn format_node(root: &SyntaxNode, style: FormatStyle) -> Result<String, FormatError> {
    format_node_with_signatures(root, style, &SignatureDb::default())
}

/// Like [`format_node`] but folds an `external` signature scope (loaded local
/// packages' merged definitions) into the lowering. The language server passes the
/// salsa-cached `incremental::scope_signatures` (in the `tex-ls` crate) here; the document's own
/// definitions always win over `external`. An empty DB recovers [`format_node`].
pub fn format_node_with_signatures(
    root: &SyntaxNode,
    style: FormatStyle,
    external: &SignatureDb,
) -> Result<String, FormatError> {
    format_node_with_signatures_sentence(root, style, external, SentenceOptions::default())
}

/// Like [`format_node_with_signatures`] but with explicit
/// [`SentenceOptions`] for the
/// `sentence`/`semantic` wrap modes.
pub fn format_node_with_signatures_sentence(
    root: &SyntaxNode,
    style: FormatStyle,
    external: &SignatureDb,
    sentence: SentenceOptions<'_>,
) -> Result<String, FormatError> {
    validate_supported_tokens(root)?;

    let ctx = FormatContext::with_sentence(style, sentence);
    let mut formatted = format_root(root, ctx, external, None);
    // Normalize the document's trailing edge: drop any trailing blank lines and
    // per-line trailing whitespace at EOF, then guarantee exactly one final
    // newline. Empty output stays empty. Only ASCII whitespace/newlines are
    // trimmed, so trailing Unicode content (e.g. a non-breaking space) survives.
    let trimmed_len = formatted.trim_end_matches([' ', '\t', '\n', '\r']).len();
    formatted.truncate(trimmed_len);
    if !formatted.is_empty() {
        formatted.push('\n');
    }
    apply_line_ending(&mut formatted, resolve_line_ending(root, style));
    Ok(formatted)
}

/// Range formatting: lay out only the document-level blocks overlapping `range`,
/// returning the formatted text for the `[first block start, last block end]`
/// span. The caller (the `tex-ls` crate's LSP) expands the editor selection to whole
/// document-level-block boundaries before calling, so `range` is already
/// block-aligned. Direct children of the canonical no-indent `document` environment
/// count as document-level blocks alongside children of `ROOT`.
///
/// The whole document is still scanned for `\newcommand` signatures and expl3
/// regions (`format_root`), so a selected block depending on an earlier
/// definition or sitting inside an ancestor `\ExplSyntaxOn` is laid out exactly as
/// in a full format; only *emission* is filtered (see `LowerCtx::range`). Unlike
/// [`format_node_with_signatures`], the document-level trailing-edge normalization
/// is **not** applied — this is a mid-document fragment, so no final newline is
/// forced. Trailing whitespace is trimmed (the slice it replaces ends at a block
/// boundary), keeping the diff against the original slice clean.
pub fn format_node_range_with_signatures(
    root: &SyntaxNode,
    style: FormatStyle,
    external: &SignatureDb,
    range: TextRange,
) -> Result<String, FormatError> {
    format_node_range_with_signatures_sentence(
        root,
        style,
        external,
        range,
        SentenceOptions::default(),
    )
}

/// Like [`format_node_range_with_signatures`] but with explicit
/// [`SentenceOptions`] for the
/// `sentence`/`semantic` wrap modes.
pub fn format_node_range_with_signatures_sentence(
    root: &SyntaxNode,
    style: FormatStyle,
    external: &SignatureDb,
    range: TextRange,
    sentence: SentenceOptions<'_>,
) -> Result<String, FormatError> {
    validate_supported_tokens(root)?;

    let ctx = FormatContext::with_sentence(style, sentence);
    let mut formatted = format_root(root, ctx, external, Some(range));
    let trimmed_len = formatted.trim_end_matches([' ', '\t', '\n', '\r']).len();
    formatted.truncate(trimmed_len);
    // Detected from the whole document, not the fragment: the replacement has to
    // match the endings of the text it splices into, and a block that happens to
    // hold no line break of its own would otherwise answer `Lf`.
    apply_line_ending(&mut formatted, resolve_line_ending(root, style));
    Ok(formatted)
}

/// The concrete ending `style` calls for on this document ([`LineEnding::Auto`]
/// resolved against what the source used).
fn resolve_line_ending(root: &SyntaxNode, style: FormatStyle) -> LineEnding {
    if style.line_ending == LineEnding::Auto {
        style.line_ending.resolve(detect_line_ending(&root.text()))
    } else {
        style.line_ending.resolve(LineEnding::Lf)
    }
}

/// Refuse any `ERROR` token. A clean parse should contain none, but the parser
/// can emit them on recovery; the formatter never reshapes around them.
fn validate_supported_tokens(root: &SyntaxNode) -> Result<(), FormatError> {
    for element in root.descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if token.kind() == SyntaxKind::ERROR {
            return Err(FormatError::UnsupportedConstruct {
                kind: token.kind(),
                snippet: token.text().to_string(),
            });
        }
    }
    Ok(())
}

fn format_root(
    root: &SyntaxNode,
    ctx: FormatContext,
    external: &SignatureDb,
    range: Option<TextRange>,
) -> String {
    // Scan the document's own `\newcommand`/`\newenvironment`/xparse definitions
    // once, so the lowering resolves a locally-defined construct's arity (not just
    // the built-in DB's). They are overlaid on top of `external` — the merged
    // signatures of any loaded local packages — so a document redefinition wins
    // over a package. `external` is empty for the contextless entry points, in
    // which case this is exactly the old document-only scan. Held by value for the
    // whole lowering.
    let mut user = external.clone();
    user.merge_from(&scan_definitions(root), None);
    // The expl3 source regions, recomputed read-only from the same toggle set the
    // lexer uses ([`expl_toggle`]). Inside them source whitespace is catcode-9
    // (ignored) and `~` is catcode-10 (a literal space), so the formatter fully owns
    // layout. Held by value for the whole lowering, like `user`.
    let regions = expl3_regions(root);
    // The spans the author turned layout off over, resolved from this file's own
    // comment directives (see [`tex_ls_parser::directives`]). A pure function of the
    // tree, held by value for the whole lowering like `regions`. Empty for the
    // overwhelming majority of documents, in which case every query is free.
    let suppressed = directives::Suppressions::build(root);
    // The sentence-boundary profile for the `sentence`/`semantic` wrap modes,
    // resolved from the run's [`SentenceOptions`]. `Copy`, borrowing the merged
    // no-break slice `ctx` still owns for the whole call, so it rides `LowerCtx`
    // like the bare `wrap` mode. Never consulted under `reflow`/`preserve`.
    let profile = ctx.sentence().resolved();
    // The `.dtx` doc-paragraph reflow-safety memo (see [`DtxReflowCache`]),
    // owned here for the whole lowering like `user` and `regions`.
    let dtx_reflow_cache = DtxReflowCache::default();
    let cx = LowerCtx {
        wrap: ctx.style().wrap,
        item_indent: ctx.style().item_indent,
        indent_width: ctx.style().indent_width,
        // Resolved here (never `Auto` past this point), so library callers get the
        // derivation from `wrap` for free.
        math_wrap: ctx.style().math_wrap.resolve(ctx.style().wrap),
        stable_target: ctx.style().stable_wrap_target(),
        signatures: Signatures::new(&user),
        expl3_regions: &regions,
        suppressed: suppressed.format_ranges(),
        profile,
        range,
        dtx_reflow_cache: &dtx_reflow_cache,
        dtx_margin_probe: false,
        preserve_dtx_nested_layout: false,
        in_dtx_doc_region: false,
        in_alignment_cell: false,
        absorbed_control_newline: None,
        omitted_leading_comment: None,
        is_dtx: root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| matches!(t.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD)),
    };
    // Saturate `Group::expand` at the lowering->printer seam: after this, the
    // flag is the single representation of "forced open" the printer trusts.
    // Lowering-time `contains_forced_break` queries use the immutable summary
    // stored at each group boundary; this final pass still marks every nested
    // group for the printer in one bottom-up walk.
    let ir = lower_node(root, cx).propagate_breaks();
    Printer::new(ctx.style()).print(&ir)
}

/// Whether two byte ranges overlap (share at least one byte). Half-open, so ranges
/// that merely touch at a boundary (`a.end == b.start`) do not overlap — used by
/// the range-formatting emission filter to keep a top-level block's leading/trailing
/// trivia (which abuts but does not overlap the block-aligned range) out of the
/// fragment.
fn ranges_overlap(a: TextRange, b: TextRange) -> bool {
    a.start() < b.end() && b.start() < a.end()
}

/// Whether `range` lies wholly inside the body of the canonical `document`
/// environment. Its body is the formatter's canonical no-indent case, so exposing
/// direct body blocks does not discard any ancestor indentation context.
fn document_body_contains(node: &SyntaxNode, range: TextRange) -> bool {
    let Some(environment) = Environment::cast(node.clone()) else {
        return false;
    };
    if environment.name().as_deref() != Some("document") {
        return false;
    }
    let (Some(begin), Some(end)) = (environment.begin(), environment.end()) else {
        return false;
    };
    range.start() >= begin.syntax().text_range().end()
        && range.end() <= end.syntax().text_range().start()
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

/// The state threaded through every lowering call: the active [`WrapMode`] plus the
/// per-document [`Signatures`] overlay (scanned definitions over the built-in DB)
/// that [`lower_begin`] consults for environment arity. `Copy`, so it passes by
/// value like the bare `wrap` mode it replaced.
#[derive(Clone, Copy)]
struct LowerCtx<'a> {
    wrap: WrapMode,
    /// How far list-item continuation lines sit from the `\item` column.
    item_indent: ItemIndent,
    /// One structural indentation step, used by [`ItemIndent::Indent`].
    indent_width: usize,
    /// Display-math break policy, pre-resolved against `wrap` in
    /// [`format_root`] — never [`MathWrap::Auto`] here.
    math_wrap: MathWrap,
    /// Soft equilibrium line target for [`WrapMode::Stable`]. Already derived and
    /// clamped against the hard width by [`FormatStyle::stable_wrap_target`].
    stable_target: usize,
    signatures: Signatures<'a>,
    /// Sorted, non-overlapping byte ranges of the document's expl3 regions (see
    /// [`expl3_regions`]). Inside these, source whitespace is catcode-9 (ignored)
    /// and `~` is catcode-10 (a literal space), so the formatter lays out the code
    /// itself — regardless of [`WrapMode`]. Borrowed from a `Vec` owned by
    /// [`format_root`], exactly like `signatures`.
    expl3_regions: &'a [TextRange],
    /// Sorted, non-overlapping byte ranges the author turned layout off over
    /// (`% tex-ls-format off`/`skip`/`skip-file` and the combined `% tex-ls`
    /// family — see [`tex_ls_parser::directives`]). Content overlapping one of these is
    /// reproduced byte-for-byte instead of laid out. Borrowed from a `Vec` owned
    /// by [`format_root`], exactly like `expl3_regions`.
    suppressed: &'a [TextRange],
    /// The sentence-boundary profile (built-in language plus user no-break
    /// abbreviations) for the [`WrapMode::Sentence`]/[`WrapMode::Semantic`] modes.
    /// `Copy`, borrowing the merged slice owned by [`format_root`]. Never consulted
    /// under [`WrapMode::Reflow`]/[`WrapMode::Stable`]/[`WrapMode::Preserve`], so an
    /// English default (see [`SentenceOptions::default`]) is harmless there.
    profile: ResolvedProfile<'a>,
    /// Range-formatting emission filter. When `Some`, only document-level blocks
    /// overlapping this byte range are lowered; the rest are skipped and never
    /// produce IR (see [`lower_node`]). These are children of [`SyntaxKind::ROOT`]
    /// or direct body children of the canonical no-indent `document` environment.
    /// `None` (the default) lowers the whole document. Every selected block still
    /// lowers in full, at its real indent-0 context, so the formatter stays the sole
    /// authority on layout.
    range: Option<TextRange>,
    /// Memo for [`dtx_doc_paragraph_reflows_safely`]. The answer is needed twice
    /// per `.dtx` doc paragraph — once by the paragraph's own lowering, once by
    /// [`margin_floats_into_paragraph`] deciding whether the floated leading `%`
    /// may be dropped — and computing it means lowering the paragraph, so without
    /// the memo a nested document pays for it repeatedly. Borrowed from
    /// [`format_root`] like `expl3_regions`.
    dtx_reflow_cache: &'a DtxReflowCache,
    /// Set while *probing* whether a `.dtx` doc paragraph reflows safely. The probe
    /// lowers the paragraph, and that lowering must not consult
    /// [`margin_floats_into_paragraph`] again — the two would recurse into each
    /// other, once per nesting level, until the stack runs out. Dropping the
    /// floated margin is an emission detail that cannot change whether the reflow
    /// escapes the margin, so the probe simply keeps it.
    dtx_margin_probe: bool,
    /// A structured `.dtx` documentation paragraph may normalize its margin
    /// frames, but nested inline constructs must not synthesize unmargined lines.
    preserve_dtx_nested_layout: bool,
    /// The current node is being lowered as virtual LaTeX from a fully margined
    /// `.dtx` documentation region. Its physical `DOC_MARGIN` tokens are omitted;
    /// one canonical margin is re-applied by the enclosing IR.
    in_dtx_doc_region: bool,
    /// The current node is part of a non-math alignment cell that must collapse
    /// to one source line. Lone trivia newlines soften to spaces even when parser
    /// attachment nests them inside a command; blank lines and structural block
    /// breaks remain forced and make the grid decline.
    in_alignment_cell: bool,
    /// The exact trailing `\\<newline>` control symbol whose newline is supplied
    /// by an enclosing block's closing frame. Keeping the token's backslash here
    /// while letting the existing structural [`Ir::hard_line`] spell its newline
    /// prevents a second, blank line before the closer (issue #141).
    absorbed_control_newline: Option<TextRange>,
    /// A statement-leading [`SyntaxKind::DOC_COMMENT`] already emitted outside
    /// the statement's hanging indent. Descending lowerers omit exactly this
    /// node while retaining their ordinary command or conditional layout.
    omitted_leading_comment: Option<TextRange>,
    /// Whether the document carries any `.dtx` documentation margin at all — the
    /// cheap short-circuit for the no-`.dtx` majority, so gates that would
    /// otherwise walk back to the start of a physical line
    /// ([`doc_margin_opens_line`]) cost nothing in an ordinary `.tex` file.
    /// Computed once in [`format_root`].
    is_dtx: bool,
}

/// Memoized [`dtx_doc_paragraph_reflows_safely`] answers, keyed by paragraph node.
type DtxReflowCache = RefCell<HashMap<SyntaxNode, bool>>;

impl<'a> LowerCtx<'a> {
    /// Whether the active wrap mode lays out prose paragraphs at all (as opposed to
    /// [`WrapMode::Preserve`], which leaves authored breaks untouched). Reflow,
    /// sentence, and semantic all route prose through [`reflow_elements`]; the mode
    /// then decides how a completed run is rendered (width fill vs. sentences).
    fn wraps_prose(self) -> bool {
        matches!(
            self.wrap,
            WrapMode::Reflow | WrapMode::Stable | WrapMode::Sentence | WrapMode::Semantic
        )
    }

    /// Mark a body-final `\\<newline>` for absorption into its closing frame.
    /// If this body has no such token, preserve an outer body's marker while
    /// recursively lowering its children.
    fn absorbing_trailing_control_newline(self, body: &[SyntaxElement]) -> Self {
        let Some(token) = trailing_control_newline(body) else {
            return self;
        };
        Self {
            absorbed_control_newline: Some(token.text_range()),
            ..self
        }
    }

    fn absorbs_control_newline(self, token: &SyntaxToken) -> bool {
        self.absorbed_control_newline == Some(token.text_range())
    }

    /// Whether the document has any expl3 region at all — the cheap short-circuit
    /// for the no-expl3 majority (the slice is empty, so every query is free).
    fn any_expl3(self) -> bool {
        !self.expl3_regions.is_empty()
    }

    /// Whether byte offset `at` falls inside some expl3 region. O(log n) over the
    /// sorted, disjoint range list.
    fn in_expl3_region(self, at: TextSize) -> bool {
        self.expl3_regions
            .binary_search_by(|r| {
                use std::cmp::Ordering;
                if at < r.start() {
                    Ordering::Greater
                } else if at >= r.end() {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Whether `range` intersects some expl3 region (used to route a paragraph that
    /// is wholly or partly in-region).
    fn overlaps_expl3(self, range: TextRange) -> bool {
        self.expl3_regions
            .iter()
            .any(|r| r.start() < range.end() && range.start() < r.end())
    }

    /// Whether `range` lies wholly inside a directive-suppressed span, so
    /// whatever occupies it must be reproduced byte-for-byte instead of laid out.
    ///
    /// **Containment, not overlap**, and the difference is the whole granularity
    /// story. An `off`/`on` region is delimited by comments the author placed,
    /// not by CST boundaries, so it can begin halfway through a construct — and
    /// every construct it begins inside is an *ancestor* of the content it means
    /// to cover. Overlap would therefore suppress the outermost such ancestor:
    /// one directive anywhere in a document body suppresses the whole
    /// `document` environment, and with it the entire file. Containment picks
    /// the outermost node that fits *within* the region instead, so an ancestor
    /// merely straddling the boundary keeps descending and only the blocks the
    /// author actually enclosed are reproduced.
    ///
    /// A node straddling the boundary is laid out normally while its wholly
    /// enclosed children are still reproduced. That is a finer granularity than
    /// ruff's statement level, and it stays sound in the direction that matters:
    /// every byte the author enclosed is preserved.
    fn suppressed(self, range: TextRange) -> bool {
        self.suppressed.iter().any(|r| r.contains_range(range))
    }
}

/// Lower a CST node to IR. Most nodes lower generically (see
/// [`lower_element_stream`]); an [`SyntaxKind::ENVIRONMENT`] is special-cased to
/// indent its body (see [`lower_environment`]), and under each prose-wrapping mode a
/// [`SyntaxKind::PARAGRAPH`] is routed through its line policy (see
/// [`lower_paragraph_reflow`]). The [`LowerCtx`] (wrap mode + signature overlay) is
/// threaded through so it reaches every nested paragraph (including environment and
/// group bodies).
fn lower_node(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    // Directive-suppressed content is reproduced, never laid out. Checked first,
    // above every routing decision, so no arm below can claim a node the author
    // turned the formatter off over.
    //
    // `ROOT` is excluded because a whole-file `skip-file` covers it: suppressing
    // there would emit the document as one opaque blob, which is the right bytes
    // by luck but skips the emission filter a range format depends on. Excluded,
    // the same directive reaches every *child* instead, and both the full and the
    // ranged path fall out of the one mechanism.
    if node.kind() != SyntaxKind::ROOT && cx.suppressed(node.text_range()) {
        return Ir::verbatim(node.text().to_string());
    }
    if node.kind() == SyntaxKind::DOC_COMMENT
        && cx.omitted_leading_comment == Some(node.text_range())
    {
        return Ir::Nil;
    }
    if cx.is_dtx
        && node.kind() != SyntaxKind::ROOT
        && !inside_macrocode(node)
        && contains_indented_dtx_comment(node)
    {
        let padding = leading_indented_dtx_comment_padding(node)
            .map(Ir::verbatim)
            .unwrap_or(Ir::Nil);
        return Ir::concat([padding, Ir::verbatim(node.text().to_string())]);
    }
    if dtx_doc_region(node, cx) {
        let virtual_doc = LowerCtx {
            in_dtx_doc_region: true,
            preserve_dtx_nested_layout: false,
            ..cx
        };
        return Ir::doc_margin(lower_node(node, virtual_doc));
    }
    if cx.is_dtx
        && node.kind() == SyntaxKind::ENVIRONMENT
        && doc_margin_opens_line(node, cx)
        && (environment_begin_has_newline(node) || !is_margin_framed(node))
    {
        return Ir::verbatim(node.text().to_string());
    }
    if cx.preserve_dtx_nested_layout
        && matches!(
            node.kind(),
            SyntaxKind::COMMAND
                | SyntaxKind::ENVIRONMENT
                | SyntaxKind::GROUP
                | SyntaxKind::OPTIONAL
                | SyntaxKind::INLINE_MATH
                | SyntaxKind::DISPLAY_MATH
                | SyntaxKind::MATH
        )
    {
        return Ir::verbatim(node.text().to_string());
    }
    // A `.dtx` command that opens on a docstrip guard and absorbs later guard
    // tokens is one fully guarded physical-line construct.  Its first guard is
    // a sibling (the command range starts at the control sequence), while the
    // continuation guards are children attached by the parser.  Relaying the
    // node would therefore join those child guards to the preceding tokens and
    // strand the continuations on unguarded lines.  Preserve the command as one
    // opaque slice; an unguarded command with guarded arguments still takes the
    // ordinary expl3 path and keeps its width-driven layout.
    if node.kind() == SyntaxKind::COMMAND
        && doc_margin_opens_line(node, cx)
        && node
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::GUARD)
    {
        return Ir::verbatim(node.text().to_string());
    }
    // A guard-led paragraph can carry later guards as children while the first
    // guard remains a sibling. Reflowing that mixed shape treats the first
    // command as ordinary prose, then emits the next column-zero guard without
    // first closing the line, joining two docstrip variants. Preserve the whole
    // paragraph whenever its opening line and a continuation are guarded.
    if node.kind() == SyntaxKind::PARAGRAPH
        && doc_margin_opens_line(node, cx)
        && node
            .first_token()
            .is_some_and(|token| token.kind() == SyntaxKind::CONTROL_WORD)
        && node
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::GUARD)
    {
        return Ir::verbatim(node.text().to_string());
    }
    // A fully docstrip-guarded paragraph is the whole hole cut out of an
    // expl3 region. With no byte of the paragraph left in-region it never
    // reaches `lower_expl_paragraph`; the generic Preserve paragraph path
    // would otherwise normalize its whitespace and move guards off column 0.
    if node.kind() == SyntaxKind::PARAGRAPH {
        let text = node.text().to_string();
        if text_is_fully_guarded(&text) {
            // The surrounding environment owns the body-leading break; keeping
            // the paragraph's leading newline inside an opaque IR would give
            // the printer two competing boundary breaks and float the first
            // guard onto the frame line.
            return Ir::verbatim(text.trim_start_matches(['\r', '\n']));
        }
    }
    // Range-formatting emission filter: at the document root, lower only the
    // children (top-level blocks plus the trivia between them) overlapping the
    // requested range; skip the rest entirely. A canonical `document` environment
    // is transparent when the range lies wholly in its body: that body's layout is
    // deliberately flush with the root, so its direct children are equally safe
    // independent blocks. Other environments still lower in full. A `None` range
    // (the whole-document default) never reaches here.
    if let Some(range) = cx.range
        && node.kind() == SyntaxKind::ROOT
    {
        let mut filtered = Vec::new();
        for element in node
            .children_with_tokens()
            .filter(|el| ranges_overlap(range, el.text_range()))
        {
            if let SyntaxElement::Node(child) = &element
                && document_body_contains(child, range)
            {
                filtered.extend(
                    child
                        .children_with_tokens()
                        .filter(|el| ranges_overlap(range, el.text_range())),
                );
            } else {
                filtered.push(element);
            }
        }
        return Ir::concat(lower_element_stream(filtered.into_iter(), cx));
    }
    // expl3 code layout (catcode-9 whitespace / catcode-10 `~`) applies regardless
    // of `WrapMode`, so it is checked before the wrap-gated arms below. A paragraph
    // overlapping a region is split at the toggles; a brace/optional group inside a
    // region lays out its body as expl3 code.
    if cx.any_expl3() {
        match node.kind() {
            SyntaxKind::PARAGRAPH if cx.overlaps_expl3(node.text_range()) => {
                return lower_expl_paragraph(node, cx);
            }
            SyntaxKind::GROUP if cx.in_expl3_region(node.text_range().start()) => {
                return lower_expl_group(node, SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, cx);
            }
            SyntaxKind::OPTIONAL if cx.in_expl3_region(node.text_range().start()) => {
                return lower_expl_group(node, SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET, cx);
            }
            // A command and its greedily-attached `{…}`/`[…]` arguments lay out as a
            // fill so the arguments break independently (only an over-long one
            // detonates) rather than the generic concat breaking every group.
            SyntaxKind::COMMAND if cx.in_expl3_region(node.text_range().start()) => {
                // `Statements::Ignore`: within one command's attached arguments a
                // source newline is just catcode-9 whitespace, not a statement
                // boundary — the width fill alone decides the breaks. Otherwise a
                // fill-broken argument would read as a new statement on the next
                // pass and the layout would never reach a fixed point.
                let fill = lower_expl_code(node.children_with_tokens(), cx, Statements::Ignore);
                // A recognized conditional renders **all-or-nothing**: flat when the
                // whole call fits, else the R4/R5 explosion. Left to the fill above,
                // the branch groups are independent atoms, so an overflow hangs only
                // the last one and splits the branch list across two indents —
                // `\…:nTF {c} {T}` / `{F}`, which reads as a continuation of the
                // enclosing statement rather than as the false branch.
                //
                // Attached to the *node*, not to statement position. The two
                // position-keyed paths in `lower_expl_code` (statement-leading,
                // trailing) additionally join the head, but both are gated off inside
                // a *fallback* statement — deliberately, since whether a conditional
                // name sits trailing there depends on where the line's junk ends,
                // which is not pass-invariant (xtemplate's spliced
                // `cs_ \str_if_eq:nnT … set:Npn` name assembly). The node is the node
                // on every pass, so this carries no such question and covers the
                // fallback case the other two decline.
                if let Some(exploded) = command_name(node)
                    .and_then(|name| expl3::conditional_branches(&name))
                    .and_then(|n| lower_expl_conditional(node, cx, n))
                {
                    return Ir::group(Ir::if_break(fill, exploded));
                }
                return fill;
            }
            _ => {}
        }
    }
    match node.kind() {
        // A `.dtx` documentation-layer prose paragraph (its first content token is
        // a `DOC_MARGIN`): reflow the bare prose and re-emit a `% ` margin on each
        // wrapped line. Checked before the generic paragraph reflow so the margin
        // is stripped and re-synthesized rather than glued into the fill.
        SyntaxKind::PARAGRAPH
            if !cx.in_dtx_doc_region && cx.wraps_prose() && is_dtx_doc_paragraph(node) =>
        {
            return lower_dtx_doc_paragraph(node, cx);
        }
        SyntaxKind::PARAGRAPH if cx.wraps_prose() => {
            return lower_paragraph_reflow(node, cx);
        }
        // Under `Preserve` a (non-`.dtx`) prose paragraph keeps its authored line
        // breaks, but inter-word spacing on each line still normalizes to a single
        // space (see [`lower_prose_stream`]) — `Preserve` governs line breaks only.
        // Inline-prose command bodies (`\emph{…}`) are flattened in so their text
        // collapses too, matching every wrapping mode; opaque argument bodies (a
        // `\newcommand` definition) recurse through [`lower_node`] and stay verbatim.
        // A `.dtx` doc paragraph is excluded (the arm above requires `wraps_prose`),
        // so it falls through to the generic stream and keeps its `%` margins.
        SyntaxKind::PARAGRAPH if cx.wrap == WrapMode::Preserve && !is_dtx_doc_paragraph(node) => {
            let flat = flatten_inline_prose(
                flatten_statements(node.children_with_tokens().collect()),
                cx,
                false,
            );
            return Ir::concat(lower_prose_stream(flat.into_iter(), cx));
        }
        // A `.dtx` docstrip frame (`%␣␣␣␣\begin{macrocode}`, a documentation-layer
        // `% \begin{itemize}`): the body is never indented and the closing frame is
        // kept whole at column 0. Routed before the alignment/list lowerers so a
        // margin-framed environment never reaches a layout that would reindent its
        // frame margins. A *math* environment is excluded: a `bmatrix`/`array` whose
        // `\begin` happens to open a `%␣␣␣␣` line inside a `% \[…\]` doc-math block
        // (l3backend-draw.dtx, l3ldb.dtx) is doc-comment prose, not a docstrip frame —
        // margin-framing it re-breaks `\end{bmatrix}` off its `%` margin and leaves the
        // block's `\]` unparseable on pass 2. It falls through to the generic stream,
        // which keeps the authored margins verbatim (same margin rule as the math /
        // group / optional arms below).
        SyntaxKind::ENVIRONMENT
            if !cx.in_dtx_doc_region
                && !has_verbatim_body(node)
                && is_margin_framed(node)
                && !is_math_env(node, cx) =>
        {
            return lower_margin_framed_environment(node, cx);
        }
        // A named math environment (`equation`, `align`, `gather`, matrix, …) — its
        // body is a `MATH` node (the parser entered math mode). Checked before the
        // generic alignment arm so a math *grid* (`align`/`pmatrix`, both `math` and
        // `align`) takes the math-aware path; a non-math grid (`tabular`,
        // `align` but not `math`) still falls through to `lower_aligned_environment`.
        // The `contains_doc_margin` gate is the same margin rule as the generic
        // arm below: a `bmatrix` nested in a `% \[…\]` doc-math block (l3backend-draw.dtx)
        // must keep its `%` margins verbatim, or the re-broken `\end{bmatrix}` drops
        // off column 0 and the block's closing `\]` is unparseable on pass 2.
        SyntaxKind::ENVIRONMENT
            if !has_verbatim_body(node)
                && !cx.in_dtx_doc_region
                && is_math_env(node, cx)
                && !contains_doc_margin(node, cx) =>
        {
            return lower_math_environment(node, cx);
        }
        // A grid inside a fully owned virtual `.dtx` documentation region lays
        // out after its physical margins have been stripped. The enclosing
        // `Ir::doc_margin` then restores the prefix at column zero, outside the
        // grid's padding. Other margin-carrying grids still decline through
        // `contains_doc_margin`.
        SyntaxKind::ENVIRONMENT
            if !has_verbatim_body(node)
                && is_alignment_env(node, cx)
                && !contains_doc_margin(node, cx) =>
        {
            return lower_aligned_environment(node, cx);
        }
        // A list environment (`itemize`/`enumerate`/`description`): its `\item`s get
        // their continuation lines hanging-indented under the marker. Under a
        // prose-wrapping mode the body is reflowed; under `Preserve` the authored
        // breaks and inner spacing are kept byte-faithful and only the continuation
        // *indentation* is re-hung (see [`lower_item_chunks`]). A doc-margined list
        // under `Preserve` is excluded — like the generic arm below, its `%` margins
        // must stay pinned at column 0 — so it falls through to the generic stream.
        SyntaxKind::ENVIRONMENT
            if !has_verbatim_body(node)
                && is_list_env(node, cx)
                && (cx.wraps_prose() || !contains_doc_margin(node, cx)) =>
        {
            return lower_list_environment(node, cx);
        }
        // A user-defined or otherwise unclassified environment whose body carries a
        // top-level `&` reads as an alignment (`myaligned`, issue #84): `&` at
        // catcode 4 is a column tab, a static CST-shape fact. Known align/math/list
        // environments were routed by the arms above; this generalizes `&`-column
        // layout to the environments the signature DB cannot name, exactly as the
        // environment group-boundary gate generalizes the curated definition-body set
        // Whitespace-only (the grid renderer reflows only trivia) and
        // self-correcting: any shape the grid cannot lay out falls back to
        // [`lower_environment`]. Doc-margined bodies are excluded (same margin rule as
        // the arms above and below), except inside a fully owned virtual region,
        // where `contains_doc_margin` is false and framing is stripped before grid
        // layout.
        SyntaxKind::ENVIRONMENT
            if !has_verbatim_body(node)
                && !contains_doc_margin(node, cx)
                && body_has_top_level_ampersand(node) =>
        {
            return lower_aligned_environment(node, cx);
        }
        // Same margin rule as the math/group/optional arms below: an environment
        // continuing across `.dtx` doc-margined lines is never re-laid. A
        // *margin-framed* environment (its `\begin`/`\end` on `%` frame lines) took
        // the `is_margin_framed` arm above; what reaches here is an environment
        // merely *nested* in doc-margined prose — an `array` inside a `% \[…\]`
        // display-math block (l3color.dtx). Re-breaking its `\begin`/body/`\end`
        // onto fresh lines would push `\end{array}` off its `%` margin, a semantics
        // change (a column-0 line stops being a comment at package-load time) that
        // leaves the orphaned `\]` unparseable on pass 2. The generic stream keeps
        // the authored margins verbatim.
        SyntaxKind::ENVIRONMENT if !has_verbatim_body(node) && !contains_doc_margin(node, cx) => {
            return lower_environment(node, cx);
        }
        // Same margin rule as the environment arm: a conditional spanning `.dtx`
        // doc-margined lines is never re-laid, since moving a divider off its `%`
        // margin is a semantics change. The generic stream keeps margins pinned.
        SyntaxKind::CONDITIONAL if !contains_doc_margin(node, cx) => {
            return lower_conditional(node, cx);
        }
        // Same margin rule as the environment/math/group/optional arms: a command
        // whose argument continues across `.dtx` doc-margined lines
        // (`% \title{^^A\n%   …}`) is never re-laid. Reflowing a managed argument
        // breaks its body onto fresh lines, which drops the `%` margin — and on an
        // unmargined line a `^^A` doc comment re-lexes as content, so the layout
        // stops being whitespace-only and pass 2 no longer parses. The generic
        // stream keeps the authored margins verbatim.
        SyntaxKind::COMMAND
            if (command_has_math_arg(node, cx)
                || cx.wraps_prose() && command_has_managed_arg(node, cx))
                && !contains_doc_margin(node, cx) =>
        {
            return lower_command(node, cx);
        }
        // Like the multi-line group below, math continuing across ordinary `.dtx`
        // doc-margined lines is never re-laid: math relayout would move a `%`
        // margin off column 0. A virtual doc region is different—its shared CST
        // view strips physical framing before specialized math lowering, and the
        // region wrapper regenerates the margins afterward.
        SyntaxKind::INLINE_MATH if !contains_doc_margin(node, cx) => {
            return lower_math(node, cx);
        }
        SyntaxKind::DISPLAY_MATH if !contains_doc_margin(node, cx) => {
            return lower_display_math(node, cx);
        }
        // A bare `MATH` node inside a virtual environment may be a grid cell;
        // its enclosing grid owns separators and row boundaries. Complete
        // inline/display nodes enter their math lowerers through the two arms
        // above and do not need this fallback.
        SyntaxKind::MATH if !cx.in_dtx_doc_region && !contains_doc_margin(node, cx) => {
            return lower_math_body(node, cx);
        }
        // A `.dtx` doc-layer group continuing across margined lines
        // (`\changes{…}{…\n%  …}`) is excluded: re-laying it out would move
        // content off its `%` margin — a semantics change (the line stops being a
        // comment at package-load time). Such a group falls through to the
        // generic stream, which keeps the authored margins verbatim.
        SyntaxKind::GROUP if !contains_doc_margin(node, cx) => {
            if opaque_group_has_glued_environment_sibling(node) {
                return Ir::verbatim(node.text().to_string());
            }
            // Width-driven Opaque layout under the default mode: block-vs-inline
            // is decided by width, content, and preserved predicates — never by
            // whether the author happened to break the line. A group *opening
            // on* a doc-margined line holds no margin token of its own, so it
            // stays on the residue path below (a width break would land content
            // off its margin — the same gate `lower_optional` carries).
            if matches!(cx.wrap, WrapMode::Reflow) && !doc_margin_opens_line(node, cx) {
                return lower_opaque_group(node, cx);
            }
            // Tier-2 residue (the non-`Reflow` modes and the margined-line
            // corner): the pre-existing behaviour, byte for byte — block form
            // when the author broke the line, the generic inline stream below
            // otherwise. Fixed-point argument on [`spans_multiple_lines`].
            if spans_multiple_lines(node) {
                return lower_bracketed(node, SyntaxKind::L_BRACE, SyntaxKind::R_BRACE, cx, false);
            }
        }
        // Same margin rule as the group above: a `[…]` continuing across
        // doc-margined lines keeps its authored margins.
        // No signature context on the generic path, so no keyval proof: only gaps
        // the author already wrote are break opportunities.
        SyntaxKind::OPTIONAL if !contains_doc_margin(node, cx) => {
            if let Some(ir) = lower_optional(node, cx, false) {
                return ir;
            }
        }
        SyntaxKind::ROOT => return lower_root(node, cx),
        _ => {}
    }
    Ir::concat(lower_element_stream(node.children_with_tokens(), cx))
}

/// Lower a complete document, discarding indentation before its first top-level
/// block. Indentation after a newline is normally absorbed into that newline's
/// [`Gap`], but the first physical line has no preceding boundary to own it, so
/// its padding otherwise survives as verbatim text. Only direct `ROOT` trivia is
/// removed—leading whitespace inside a paragraph remains under that paragraph's
/// wrap policy—and a formatter-suppressed prefix stays byte-exact.
fn lower_root(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let mut elements = node.children_with_tokens().peekable();
    while let Some(SyntaxElement::Token(token)) = elements.peek() {
        if token.kind() != SyntaxKind::WHITESPACE || cx.suppressed(token.text_range()) {
            break;
        }
        elements.next();
    }
    Ir::concat(lower_element_stream(elements, cx))
}

/// Greedily reflow a stream of inline elements to the line width, the shared core
/// of paragraph reflow ([`lower_paragraph_reflow`]) and prose-argument reflow
/// ([`lower_prose_group`]). Maximal runs of *adjacent* non-whitespace elements glue
/// into one unbreakable *atom* (so `Hello,` and `\emph{x}` never split); inter-word
/// whitespace or a lone newline is a break opportunity. A run of atoms lowers to an
/// [`Ir::fill`], which the printer wraps word-by-word.
///
/// Three things end a fill line rather than flow into it: an explicit `\\` line
/// break (a [`SyntaxKind::LINE_BREAK`] node), a `%` comment (which must terminate
/// its line), and a nested *block* (an environment or multi-line group whose IR
/// carries a forced break). Each commits the run-so-far as a fill, then a fresh run
/// continues after. Ordinary blocks are joined by [`Ir::hard_line`]; a sectioning
/// command that is a direct child of a prose paragraph uses [`Ir::empty_line`] on
/// both sides. Adjacent `\label` commands remain attached below that heading, with
/// the trailing empty line deferred until after the label run.
///
/// A lone newline is normally a break opportunity the fill rejoins, *except* when a
/// physical line is made up solely of command(s) (a `\usepackage{…}` line, a
/// `\section{…}` header — see [`line_is_command_only`]): the break on either side of
/// such a line is preserved, keeping it on its own line. Prose lines around it still
/// reflow.
///
/// Unlike a `PARAGRAPH` (which holds no blank lines by construction), an argument
/// *group* body may contain blank-line paragraph breaks; a blank-line trivia run
/// ends the current line and separates the next with an [`Ir::empty_line`].
///
/// [`ReflowKind`] selects how a *lone* source newline is treated (see that type).
/// `Prose` rejoins it into the surrounding fill (paragraphs, prose arguments);
/// `Statement` preserves it, so a code-like brace-group body keeps one logical line
/// per source line and only an *over-long* line wraps — never collapsing the author's
/// statement-per-line structure (`\draw …;` / `\draw …;`) into a single run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReflowKind {
    /// Running prose: a lone newline is a break opportunity the fill rejoins.
    Prose,
    /// A signature-proven prose *argument* body ([`lower_prose_group`]): like
    /// [`Self::Prose`], but the command-only-line preservation does not apply —
    /// width alone owns the layout. Preserving a command-only line here turns a
    /// width break of pass 1 into a *forced* break on pass 2, and that force
    /// bit leaks upward through every `contains_forced_break` reader (the
    /// opaque-group and optional declines, `collapse_arg_group`,
    /// `finish_cell`), flipping the enclosing construct between its inline and
    /// block forms across passes (pgf's `\emph{… \href{…} …}` table headers).
    /// The residue's own fixed-point argument covers the *interior* refill,
    /// not the propagated bit, so inside a width-owned argument the rule must
    /// not fire at all.
    ProseArg,
    /// Code-like statements (a `\newcommand` definition body, a picture body's
    /// fallback content): a lone newline ends the line, so each source line stays
    /// its own logical line; only width forces a wrap. Flush continuation keeps
    /// the wrap idempotent (a wrapped tail re-parses as a line already at the
    /// body indent). In a curated `statementBody` *environment* body under
    /// [`WrapMode::Reflow`], `STATEMENT` nodes are lowered structurally instead
    /// ([`lower_statement`]: boundaries from the node, continuations hung) and
    /// this authored-line contract governs only the interleaved content no `;`
    /// terminates.
    Statement,
    /// The interior of one structural `STATEMENT` ([`lower_statement`]): like
    /// [`Self::ProseArg`] — width owns the layout, a lone newline is a plain
    /// atom boundary, and the command-only residue is off — but the gaps
    /// additionally consult the TikZ unit model
    /// (`semantic::tikz::statement_glue`): a unit-internal gap (`-- (1,1)`,
    /// `at (2,3)`, `circle (3)`) renders as a single space and never breaks,
    /// so a width wrap lands only at unit boundaries. The verdicts read
    /// non-trivia token text only, so this stays Tier 1: a wrap re-derives
    /// the same units on every pass.
    StatementInterior,
    /// A `.dtx` documentation-layer prose paragraph: behaves like [`Self::Prose`]
    /// (a lone newline rejoins), but the per-line `%` documentation margin
    /// (`DOC_MARGIN`) is *dropped* from each line and each fill segment is wrapped
    /// in an [`Ir::margin_prefix`] so a canonical `% ` margin is re-emitted at
    /// column 0 on every reflowed line.
    DtxProse,
}

/// The canonical `.dtx` documentation margin re-emitted on each reflowed prose
/// line under [`ReflowKind::DtxProse`]: a `%` plus one space.
const DTX_DOC_MARGIN: &str = "% ";

/// One committed atom of a logical-line run: the printed [`Ir`] plus the atom's
/// source `text`, retained so the [`WrapMode::Sentence`]/[`WrapMode::Semantic`]
/// renderer can run sentence-boundary detection over the words. Under
/// [`WrapMode::Reflow`]/[`WrapMode::Stable`] only the layout fields are used.
#[derive(Clone, Copy)]
struct CitationSuffix {
    start: usize,
    placement: CitationPlacement,
}

struct RunAtom {
    ir: Ir,
    text: String,
    /// Byte offset at which a suffix of structurally recognized inline citation
    /// commands begins. `Some(0)` means the whole atom is a citation chain;
    /// `Some(n)` means the citations directly abut preceding prose. Ordinary
    /// content appended after the chain clears the marker.
    trailing_citation: Option<CitationSuffix>,
    /// Whether the gap immediately before this atom was a source newline. False
    /// for the first atom and for ordinary inter-word whitespace.
    preferred_break_before: bool,
}

/// How a completed logical-line run is rendered into a single segment.
#[derive(Clone, Copy)]
enum RunRender<'a> {
    /// Greedy width fill (reflow): one [`Ir::fill`] over the run's atoms, the
    /// printer breaking word-by-word at the line width.
    Fill,
    /// Source-break-aware optimal fill used by [`WrapMode::Stable`].
    Stable { target: usize },
    /// One sentence per line (sentence/semantic): cut the run at sentence
    /// boundaries and lay each sentence flat (space-joined), separating sentences
    /// with a hard break. Width is ignored — a long sentence stays on one line.
    Sentence(ResolvedProfile<'a>),
}

/// Accumulator for [`reflow_elements`]: glues atom pieces, collects them into the
/// current logical-line run, and commits completed lines (with their preceding
/// separators). Bundles the state the former nested `flush_atom`/`end_line`/
/// `push_segment` helpers shared so the retained atom text and the render policy
/// ride along without threading extra parameters through every call site.
struct LineBuilder<'a> {
    /// Glued pieces of the atom in progress (its [`Ir`] and its source text).
    atom: Vec<Ir>,
    atom_text: String,
    atom_trailing_citation: Option<CitationSuffix>,
    /// Source-newline preference to attach to the next committed atom.
    preferred_break_before_next: bool,
    /// Atoms of the current run (the current logical line).
    run: Vec<RunAtom>,
    /// Completed lines (fills/sentences and blocks), interleaved with `seps` at the
    /// end.
    lines: Vec<Ir>,
    /// Pieces riding the *last* committed line (see [`Self::append_to_last_line`]),
    /// held flat until the line is sealed. Folding each one into the line as it
    /// arrives would nest a `Concat` per piece, and a whole document collapsed onto
    /// one physical line (the trivia oracle's `all-newlines-to-spaces` variant) then
    /// recurses one frame per piece and overflows the stack.
    line_tail: Vec<Ir>,
    /// The separator *preceding* each committed line (`seps[0]` is unused). A blank
    /// line in the source promotes the next separator to an [`Ir::empty_line`].
    seps: Vec<Ir>,
    /// The separator to record before the next committed line. Default: one break.
    pending_sep: Ir,
    /// Under `.dtx` prose reflow, the `% ` margin re-emitted on each line; `None`
    /// otherwise.
    margin: Option<&'static str>,
    /// Whether some content was committed *outside* the [`Self::margin`] — a
    /// forced-break block whose interior lines do not all ride their own
    /// margins ([`block_rides_own_margins`]), one opening on an unmargined
    /// line, or a column-0 `GUARD` whose line could not be isolated
    /// ([`collect_guard_line`]). Such content lands on a line with no leading
    /// `%`, where a `.dtx` doc comment (`^^A`, a `%` run) re-lexes as content:
    /// the layout would no longer be whitespace-only.
    /// [`lower_dtx_doc_paragraph`] reads this and falls back to the
    /// byte-faithful preserve path. Always `false` when `margin` is `None`.
    margin_escaped: bool,
    /// How a completed run is turned into a segment (fill vs. sentences).
    render: RunRender<'a>,
}

impl<'a> LineBuilder<'a> {
    fn new(margin: Option<&'static str>, render: RunRender<'a>) -> Self {
        Self {
            atom: Vec::new(),
            atom_text: String::new(),
            atom_trailing_citation: None,
            preferred_break_before_next: false,
            run: Vec::new(),
            lines: Vec::new(),
            line_tail: Vec::new(),
            seps: Vec::new(),
            pending_sep: Ir::hard_line(),
            margin,
            margin_escaped: false,
            render,
        }
    }

    /// Record that content is about to be committed outside the `% ` margin (see
    /// [`Self::margin_escaped`]). A no-op when no margin is in force.
    fn note_margin_escape(&mut self) {
        if self.margin.is_some() {
            self.margin_escaped = true;
        }
    }

    /// Glue one piece (its [`Ir`] and source `text`) onto the atom in progress.
    fn push_atom_piece(&mut self, ir: Ir, text: &str) {
        self.atom.push(ir);
        self.atom_text.push_str(text);
        self.atom_trailing_citation = None;
    }

    /// Append a structurally recognized inline citation while retaining the
    /// start of a trailing citation chain. Unlike an ordinary atom piece, another
    /// directly abutting citation extends the same chain.
    fn push_trailing_citation(&mut self, ir: Ir, text: &str, placement: CitationPlacement) {
        self.atom_trailing_citation.get_or_insert(CitationSuffix {
            start: self.atom_text.len(),
            placement,
        });
        self.atom.push(ir);
        self.atom_text.push_str(text);
    }

    /// Commit the atom in progress (if any) as one atom of the current run.
    fn flush_atom(&mut self) {
        if !self.atom.is_empty() {
            let text = std::mem::take(&mut self.atom_text);
            // DTX prose owns wrapping only between atoms. Letting a nested group
            // break inside one can split macro-like documentation at an arbitrary
            // brace and synthesize margins that change the next pass's lowering.
            let ir = if self.margin.is_some() {
                self.atom.clear();
                Ir::verbatim(text.clone())
            } else {
                Ir::concat(self.atom.drain(..))
            };
            self.run.push(RunAtom {
                ir,
                text,
                trailing_citation: self.atom_trailing_citation.take(),
                preferred_break_before: std::mem::take(&mut self.preferred_break_before_next),
            });
        }
    }

    /// Mark the next inter-atom gap as an authored line break.
    fn prefer_next_break(&mut self) {
        if !self.run.is_empty() {
            self.preferred_break_before_next = true;
        }
    }

    /// Commit `content` as the next logical line, recording the separator before it
    /// and resetting `pending_sep` to a single break.
    fn push_segment(&mut self, content: Ir) {
        self.seal_last_line();
        self.seps
            .push(std::mem::replace(&mut self.pending_sep, Ir::hard_line()));
        self.lines.push(content);
    }

    /// Separate a paragraph-level sectioning command from adjacent prose. A
    /// `.dtx` documentation paragraph needs a bare `%` on the empty physical
    /// line so the separator remains inside the documentation layer.
    fn separate_section(&mut self) {
        self.pending_sep = if self.margin.is_some() {
            Ir::concat([Ir::hard_line(), Ir::column_zero("%"), Ir::hard_line()])
        } else {
            Ir::empty_line()
        };
    }

    /// Commit a forced-break block as its own segment under the `.dtx` margin:
    /// the canonical `% ` re-attached for its first line (the source margin was
    /// dropped by the `DOC_MARGIN` arm, or floated out of the paragraph), then
    /// the block raw — its interior lines carry their own column-0 margins
    /// byte-faithfully ([`block_rides_own_margins`]), so no [`Ir::margin_prefix`]
    /// wrap is needed (or safe: the printer's re-emitted prefix would collide
    /// with the block's own `Ir::column_zero` margins).
    fn push_margined_block(&mut self, ir: Ir) {
        let margin = self.margin.expect("only called under `.dtx` prose reflow");
        self.push_segment(Ir::concat([Ir::column_zero(margin), ir]));
    }

    /// Glue a trailing comment onto the run so it rides the end of its line: onto
    /// the atom in progress when one is open (a directly-glued `word%…`, whose
    /// missing space is the space-suppression idiom), else onto the last committed
    /// atom with the single separating space restored (`word %…`). The comment must
    /// never become a fill atom of its own — a width break before it would commit
    /// it to the next line, where the own-line `%` re-binds as the next command's
    /// doc comment on reparse and breaks idempotence.
    fn append_trailing_comment(&mut self, text: &str) {
        if !self.atom.is_empty() {
            self.push_atom_piece(Ir::verbatim(text), text);
            return;
        }
        if let Some(last) = self.run.last_mut() {
            let prev = std::mem::replace(&mut last.ir, Ir::Nil);
            last.ir = Ir::concat([prev, Ir::verbatim(" "), Ir::verbatim(text)]);
            last.text.push(' ');
            last.text.push_str(text);
            return;
        }
        // Empty run (the caller guards with `line_has_content`, so this is a
        // safety net): the comment becomes the line's only atom.
        self.push_atom_piece(Ir::verbatim(text), text);
    }

    /// Glue `ir` onto the end of the last committed line (a trailing comment, or
    /// content still on a block's last physical line). No-op when no line has been
    /// committed. Buffered in [`Self::line_tail`] and folded in by
    /// [`Self::seal_last_line`], so N riders cost one `Concat`, not N nested ones.
    fn append_to_last_line(&mut self, ir: Ir) {
        if !self.lines.is_empty() {
            self.line_tail.push(ir);
        }
    }

    /// Fold any buffered riders into the last committed line.
    fn seal_last_line(&mut self) {
        if self.line_tail.is_empty() {
            return;
        }
        let tail = std::mem::take(&mut self.line_tail);
        if let Some(last) = self.lines.last_mut() {
            let prev = std::mem::replace(last, Ir::Nil);
            *last = Ir::concat(std::iter::once(prev).chain(tail));
        }
    }

    /// End the current logical line: flush the atom and, when the run is non-empty,
    /// render it (a fill under reflow, sentences under sentence/semantic) and commit
    /// it. Under `.dtx` prose reflow (`margin` set) the segment is wrapped in an
    /// [`Ir::margin_prefix`] so a `% ` margin is re-emitted on every line.
    fn end_line(&mut self) {
        self.flush_atom();
        if self.run.is_empty() {
            return;
        }
        let run = std::mem::take(&mut self.run);
        let body = self.render_run(run);
        let segment = match self.margin {
            Some(m) => Ir::margin_prefix(m, body),
            None => body,
        };
        self.push_segment(segment);
    }

    /// Commit a reflow run as a hugging fill. A final inline construct whose IR
    /// contains hard breaks can then keep its fitting first line beside the
    /// preceding prose; the construct's remaining lines still break internally.
    fn end_hug_line(&mut self) {
        self.flush_atom();
        if self.run.is_empty() {
            return;
        }
        let atoms: Vec<Ir> = std::mem::take(&mut self.run)
            .into_iter()
            .map(|atom| atom.ir)
            .collect();
        let body = if atoms.len() == 1 {
            atoms.into_iter().next().unwrap()
        } else {
            let mut parts = Vec::with_capacity(atoms.len() * 2 - 1);
            for (index, atom) in atoms.into_iter().enumerate() {
                if index > 0 {
                    parts.push(Ir::Line);
                }
                parts.push(atom);
            }
            Ir::HugFill(parts.into())
        };
        let segment = match self.margin {
            Some(margin) => Ir::margin_prefix(margin, body),
            None => body,
        };
        self.push_segment(segment);
    }

    fn render_run(&self, run: Vec<RunAtom>) -> Ir {
        match self.render {
            RunRender::Fill => Ir::fill(run.into_iter().map(|a| a.ir)),
            RunRender::Stable { target } => {
                let preferred: Vec<bool> = run
                    .iter()
                    .skip(1)
                    .map(|atom| atom.preferred_break_before)
                    .collect();
                Ir::preferred_fill(run.into_iter().map(|a| a.ir), preferred, target)
            }
            RunRender::Sentence(profile) => render_sentences(run, profile),
        }
    }

    /// Emit the accumulated lines, interleaving the recorded separators.
    fn finish(mut self) -> Ir {
        self.end_line();
        self.seal_last_line();
        let mut result: Vec<Ir> = Vec::with_capacity(self.lines.len().saturating_mul(2));
        for (i, line) in self.lines.into_iter().enumerate() {
            if i > 0 {
                result.push(self.seps[i].clone());
            }
            result.push(line);
        }
        Ir::concat(result)
    }
}

/// How [`lower_expl_code`] finds statement boundaries: **structurally**, from
/// the argspec-arity segmentation ([`segment_expl_statements`] — a call unit
/// per logical line, owned by the formatter, with the authored physical line
/// as the per-statement fallback for underivable heads), or not at all
/// ([`Statements::Ignore`]: within one command's attached arguments, where a
/// newline is inert catcode-9 whitespace and the width fill owns the breaks).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Statements {
    Structural,
    Ignore,
}

/// The result of decomposing an expl3 brace group into its layout pieces; see
/// [`expl_group_pieces`].
enum ExplGroupPieces {
    /// A special-cased shape ([`expl_group_pieces`] resolved it fully): an empty
    /// body, with or without a glued lead comment. The caller uses the assembled
    /// `Ir` verbatim and does *not* get the three-candidate hang treatment.
    Assembled(Ir),
    /// A body-bearing group split into its head/body pieces so the caller can
    /// reassemble in flat, K&R, or Allman form.
    Pieces {
        /// The opening bracket, plus any glued lead comment.
        open_ir: Ir,
        /// The lowered body (leading/trailing breaks trimmed).
        body: Ir,
        /// The closing bracket.
        close_ir: Ir,
        /// Whether the flat boundary is a space (l3 house style) or tight.
        spaced: bool,
        /// Whether a comment, guard, or `.dtx` margin forces the broken form
        /// regardless of width.
        forced: bool,
    },
}

/// Lower a single loose token (one not collapsed into a trivia run) to inline IR.
/// A `.dtx` documentation margin (`DOC_MARGIN`) or docstrip guard (`GUARD`) pins
/// to column 0 via [`Ir::column_zero`] so docstrip's left-margin anchor survives
/// any surrounding LaTeX nesting; every other token splices verbatim. These tokens
/// only exist under the `.dtx` lexer config, so non-`.dtx` lowering is unaffected.
fn lower_loose_token(token: &SyntaxToken, cx: LowerCtx<'_>) -> Ir {
    if cx.in_dtx_doc_region && token.kind() == SyntaxKind::DOC_MARGIN {
        return Ir::Nil;
    }
    if matches!(token.kind(), SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD) {
        Ir::column_zero(token.text())
    } else if cx.absorbs_control_newline(token) {
        let before = token
            .text()
            .strip_suffix('\n')
            .expect("absorbed control symbol must end in a newline");
        Ir::verbatim(before)
    } else {
        Ir::verbatim(token.text())
    }
}

/// Lower a stream of elements: child nodes recurse, non-trivia tokens (and the
/// protected `\verb`/verbatim/comment tokens) are emitted verbatim, and maximal
/// runs of `WHITESPACE`/`NEWLINE` trivia are collapsed into a single break
/// primitive by [`classify_trivia`]. Comments deliberately *break* a trivia run
/// (they are content, never collapsed away), so the run on either side is
/// classified independently.
fn lower_element_stream(
    elements: impl Iterator<Item = SyntaxElement>,
    cx: LowerCtx<'_>,
) -> Vec<Ir> {
    let mut out = Vec::new();
    let mut iter = elements.peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Node(child) => out.push(lower_node(&child, cx)),
            // A virtual documentation region owns its physical margins. Drop
            // each one together with the source padding after it; ordinary
            // lowering will regenerate indentation in virtual coordinates.
            SyntaxElement::Token(token)
                if cx.in_dtx_doc_region && token.kind() == SyntaxKind::DOC_MARGIN =>
            {
                while let Some(SyntaxElement::Token(next)) = iter.peek() {
                    if next.kind() == SyntaxKind::WHITESPACE {
                        iter.next();
                    } else {
                        break;
                    }
                }
            }
            // The floated leading `%` of a virtual `.dtx` documentation region
            // belongs to the region wrapper. Drop it and its padding before the
            // generic trivia arm can consume it as an ordinary gap; otherwise the
            // wrapper emits a second margin and comments out the region's `\begin`.
            SyntaxElement::Token(token) if margin_starts_dtx_doc_region(&token, cx) => {
                while let Some(SyntaxElement::Token(t)) = iter.peek() {
                    if t.kind() == SyntaxKind::WHITESPACE {
                        iter.next();
                    } else {
                        break;
                    }
                }
            }
            // Trivia inside a suppressed span is reproduced too, one token at a
            // time rather than collapsed into a `Gap`. Without this the *gaps
            // between* two suppressed siblings would still normalize, so an
            // `off`/`on` region spanning several top-level blocks would keep
            // every block byte-exact and quietly rewrite the seams between them.
            SyntaxElement::Token(token)
                if is_collapsible_trivia(token.kind()) && cx.suppressed(token.text_range()) =>
            {
                out.push(Ir::verbatim(token.text().to_string()));
            }
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                out.push(classify_trivia(
                    consume_gap_widened(&token, &mut iter),
                    cx.in_alignment_cell,
                ));
            }
            SyntaxElement::Token(token)
                if cx.wraps_prose()
                    && !cx.dtx_margin_probe
                    && token.kind() == SyntaxKind::DOC_MARGIN
                    && margin_floats_into_paragraph(&token, cx) =>
            {
                while let Some(SyntaxElement::Token(t)) = iter.peek() {
                    if t.kind() == SyntaxKind::WHITESPACE {
                        iter.next();
                    } else {
                        break;
                    }
                }
            }
            SyntaxElement::Token(token) => out.push(lower_loose_token(&token, cx)),
        }
    }
    out
}

/// Lower a *prose* element stream under [`WrapMode::Preserve`]: like
/// [`lower_element_stream`], but a newline-free whitespace run — inter-word spacing
/// on a single line — collapses to a single space instead of surviving verbatim.
/// `Preserve` governs *line breaks* only, so intra-line spacing normalizes exactly
/// as it does in every wrapping mode (runs of spaces/tabs are catcode-10 equivalent
/// to one space, so the collapse is semantics-preserving); a run carrying a newline
/// stays a break and the printer still owns the following indentation.
///
/// Reached only for genuine prose — a non-`.dtx` `PARAGRAPH` (see [`lower_node`]) or
/// a list item body (see [`preserve_chunks`]) — after [`flatten_inline_prose`] has
/// spliced inline-prose command bodies (`\emph{…}`) into the run. A child *node*
/// still recurses through [`lower_node`], so an *opaque* brace body (a `\newcommand`
/// definition, any non-inline argument group) keeps its inner spacing byte-for-byte,
/// exactly as under every other mode.
fn lower_prose_stream(elements: impl Iterator<Item = SyntaxElement>, cx: LowerCtx<'_>) -> Vec<Ir> {
    let mut out = Vec::new();
    let mut iter = elements.peekable();
    while let Some(element) = iter.next() {
        match element {
            SyntaxElement::Node(child) => out.push(lower_node(&child, cx)),
            // Tier 2, shared with [`classify_trivia`]: `Preserve` promises the
            // authored line structure survives, so this boundary reads the newline
            // count and reproduces it. Preservation-only, hence its own fixed point.
            SyntaxElement::Token(token) if is_collapsible_trivia(token.kind()) => {
                out.push(match consume_gap_widened(&token, &mut iter).newlines {
                    0 => Ir::verbatim(" "),
                    1 => Ir::hard_line(),
                    _ => Ir::empty_line(),
                });
            }
            SyntaxElement::Token(token) => out.push(lower_loose_token(&token, cx)),
        }
    }
    out
}

/// The decomposition every environment lowering starts from: the elements before
/// `\begin` (lowered), the lowered `\begin` header, the raw body elements, and
/// the lowered `\end`. A `%` that trails the `\begin{…}` header on the same
/// source line belongs to that line (the space-suppression idiom), not the body,
/// so it is lifted onto `begin` *here* — once, for every layout path — rather
/// than in each lowering, where a path that forgot the lift would relocate the
/// comment onto its own body line and change its semantics (issue #38). The lifted
/// token still sits (nested) in `body`; consumers drop it by identity — the
/// stream path via [`lower_body_dropping_leading_comment`], the flattening paths
/// via [`is_lifted_comment`].
struct EnvParts {
    leading: Ir,
    begin: Ir,
    body: Vec<SyntaxElement>,
    end: Ir,
    lifted: Option<SyntaxToken>,
    /// How many of `body`'s leading elements came from [`lower_begin`]'s tail —
    /// content the greedy parser attached to `BEGIN` past the end of the header.
    /// They are *in* `body` so no consumer can drop them; the count is what lets
    /// [`lower_env_body`] splice them into the body's first paragraph.
    tail_len: usize,
    /// A shape-proved, unbraced argument that the grammar represents as the
    /// first body token. At present this is the parenthesized size tuple of the
    /// standard `picture` environment.
    body_header_token: Option<SyntaxToken>,
}

/// A `CONDITIONAL`'s children, decomposed for [`lower_conditional`]: the elements
/// before the first branch, the branches, and the closing `\fi`.
///
/// The leading run is **not** always empty, which is the whole reason this exists.
/// An own-line `%` run immediately before the opener binds forward as a
/// `DOC_COMMENT` and the grammar reparents it *inside* the `CONDITIONAL`
/// (`Parser::conditional`, and `parse_block` for a top-level one), so it is a
/// sibling of the branches. A lowering that walked only `CONDITIONAL_BRANCH`
/// children would drop it on the floor — and the non-trivia-content oracle cannot
/// see that, because a comment is trivia to the CST. Hence
/// [`comments_survive_formatting`] in `tests/format.rs`.
///
/// `None` when the node is not in the shape the all-or-nothing layout assumes (no
/// closer, no branch, or a stray element between two branches); the caller then
/// takes the byte-faithful stream, which emits every child by construction.
struct ConditionalParts {
    leading: Vec<SyntaxElement>,
    branches: Vec<SyntaxNode>,
    closer: SyntaxNode,
}

/// A `\begin{…}` header, split from the content the greedy parser attached past
/// the end of that header. See [`lower_begin`].
struct BeginParts {
    header: Ir,
    /// Elements [`split_environment`] prepends to the environment's body, so they
    /// indent and reflow with it instead of riding the header line.
    tail: Vec<SyntaxElement>,
}

/// One `\item` of a list environment: the rendered marker (`\item`, an optional
/// `[label]`, and a bounded Beamer `<overlay>` suffix), the width to hang
/// continuation lines at (the rendered width of the control word plus a space —
/// `\item `, *not* the label or overlay, so a wide marker does not push the body's
/// left edge around), and the item's body split into paragraph *chunks* (a blank
/// line in the source starts a new chunk). `blank_before` records whether a blank
/// line separated this item from the previous one, so it is reproduced.
struct ListItem {
    /// The comment lines of a `DOC_COMMENT` bound leading into the `\item`
    /// (`% note` on its own line directly above), rendered one per line above
    /// the marker at the item indent.
    doc_lines: Vec<String>,
    marker: String,
    hang: usize,
    glue_body: bool,
    chunks: Vec<Vec<SyntaxElement>>,
    blank_before: bool,
}

/// A flattened list-body element: either a real CST element or an explicit
/// paragraph boundary (a blank line), which [`flatten_list_body`] reifies because
/// item collection spans paragraph breaks but the trivia carrying them lives
/// *between* the body's `PARAGRAPH` nodes.
enum FlatItem {
    El(SyntaxElement),
    Blank,
}

/// One rendered grid cell: its flat, trimmed text, the number of columns it spans
/// (`1` for an ordinary cell, `n` for `\multicolumn{n}{…}{…}`), and an optional
/// alignment override (a `\multicolumn`'s own `{spec}`; `None` means "use the
/// column's declared alignment").
///
/// A *block* cell (`block` is `Some`, math grids only) is a cell that cannot
/// collapse to one line because it holds a nested multi-line construct — a block
/// environment (`\begin{aligned}…`, `\begin{cases}…`, a matrix), possibly inside
/// a `\left…\right` pair or a group. Its IR replaces `text` (which stays empty):
/// the first line continues the row and every later line hangs at the breaking
/// node's start column ([`Ir::Align`]), so a bare nested environment gets its
/// `\end{…}` directly under its `\begin{…}` and the body one indent step deeper.
/// A block cell is only ever the last cell of its row, never defines a column
/// width, and simply overflows — the same posture as a spanning cell.
struct Cell {
    text: String,
    span: usize,
    align: Option<ColAlign>,
    block: Option<BlockCell>,
}

/// The rendered IR of a block cell (see [`Cell`]) plus its hang offset: the flat
/// width of the cell content *before* the breaking node (`= ` in
/// `= \begin{aligned}…`), which the renderer adds to the cell's start column so
/// the hanging lines anchor at the node itself.
struct BlockCell {
    hang: usize,
    ir: Ir,
}

/// One row of an alignment grid: its rendered cells, the flat text of the `\\` that
/// terminated the row (`None` for a final row written without a trailing line
/// break), and an optional end-of-line comment that trails the row (rendered
/// *after* the `\\`, so the break is never commented out).
struct AlignRow {
    cells: Vec<Cell>,
    line_break: Option<String>,
    trailing_comment: Option<String>,
}

/// One item in an alignment grid: either a [`AlignRow`] or a *passthrough* line —
/// a physical line that is not a grid row (a comment-only line, or a line made up
/// solely of horizontal-rule commands like `\hline`/`\midrule`). A passthrough is
/// kept verbatim between rows and never counted toward column widths.
enum GridItem {
    Row(AlignRow),
    Passthrough(String),
}

/// A non-row line recognized at a grid boundary: its rendered text and the index
/// at which the body resumes (past the line's terminating newline).
struct NonRowLine {
    text: String,
    next: usize,
    has_rule: bool,
}

/// Which end of an `[…]` body [`peel_padding`] works on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Leading,
    Trailing,
}

/// A delimited body cut into its top-level entries: the delimiters, the entry IR
/// with [`Gap::separator`] split points already interleaved, and how many there are.
struct GroupSegments {
    open: Ir,
    parts: Vec<Ir>,
    close: Ir,
    splits: usize,
}

/// The line-breaking role of a top-level math atom (see [`lower_display_math_body`]).
#[derive(Clone, Copy, PartialEq)]
enum MathRole {
    /// A term: a variable, number, group, command-with-arguments, script, etc.
    Operand,
    /// A binary operator (`+`, `-`, `\cdot`, `\times`, …) sitting between two
    /// operands. A break may be inserted *before* it.
    Binary,
    /// A relation (`=`, `\leq`, `\to`, …). The first one anchors the alignment;
    /// a later one is also a break point.
    Relation,
    /// A one-sided-limit `-` immediately before a closing delimiter. It is an
    /// ordinary postfix atom, so both adjacent boundaries stay tight.
    PostfixLeftLimit,
}

impl MathRole {
    fn is_operand_like(self) -> bool {
        matches!(self, Self::Operand | Self::PostfixLeftLimit)
    }
}

/// Source-spacing policy for a math list. Script arguments use TeX's compact
/// convention, while ordinary math lists receive explicit binary/relation padding.
#[derive(Clone, Copy, PartialEq)]
enum MathSpacing {
    Normal,
    Script,
}

/// One top-level atom of a display-math body, paired with its [`MathRole`].
struct MathPiece {
    ir: Ir,
    role: MathRole,
    /// Whether this operator may start a continuation line. Multiplicative
    /// operators stay attached to the factor on their left, so a short
    /// additive term is not stranded between two breaks.
    break_before: bool,
    colon_relation_prefix: bool,
    spaced_slash: bool,
    slash: bool,
    /// Whether authored whitespace preceded this atom. Drives operand-operand
    /// spacing exactly as [`lower_math_seq`] does, so a tight command boundary
    /// (`\gamma)`, `}.`) stays tight rather than gaining a spurious space.
    space_before: bool,
    /// Net change in bracket nesting (`(`/`[`/`\{`/named delimiters vs their
    /// closers) contributed by this atom's own text. Used to suppress operator
    /// breaks inside a bracketed subexpression (`(1 - \gamma)` must not break at
    /// `-`, and a relation inside a set-builder `\{ … \}` is not an anchor).
    bracket_delta: i32,
}

struct MathSurfaceAtom {
    ir: Ir,
    class: MathClass,
    break_kind: MathBreakKind,
    delimiter: Option<DelimiterRole>,
    colon_relation_prefix: bool,
    starts_equals_relation: bool,
    spaced_slash: bool,
    slash: bool,
    control_word_operator: bool,
    starts_control_word_letter: bool,
    ends_control_word: bool,
    postfix_left_limit: bool,
}

/// Formatter-owned precedence for the few math operators whose TeX atom class
/// alone is too coarse for readable line breaking.
#[derive(Clone, Copy, PartialEq)]
enum MathBreakKind {
    /// Use the ordinary [`MathClass`] binary/relation behavior.
    Class,
    /// A low-precedence additive operator, retained as an explicit break point.
    Additive,
    /// A multiplicative operator, kept with the surrounding term.
    Multiplicative,
    /// A conditional relation, kept inline and used to protect its condition's
    /// relation operators from equation-chain alignment.
    Conditional,
}

/// A **normalized** trivia boundary: everything the layout is allowed to know
/// about the gap between two neighbouring elements.
///
/// What this type cannot say is the point of it. There is deliberately no
/// `Newline` variant — inline whitespace and a lone newline both arrive as
/// [`Self::Space`] — because the formatter converts freely between those two
/// spellings in *both* directions (`alpha\nbeta` → `alpha beta`, and a width wrap
/// back again). A layout decision keyed on which one the author wrote is
/// therefore a latent idempotency bug, and it is the root cause of the whole
/// K&R/Allman family (issues #71, #94, #96, #97). Discipline caught those one at
/// a time; deleting the information at the boundary is the enforcement, because a
/// rule cannot key on what it cannot see.
///
/// Glued-ness, blank-line presence, and comment presence *are* predicates the
/// formatter preserves (`P(fmt(x)) == P(x)`), so they keep their own variants and
/// layout may read them freely.
///
/// A Tier-2 site — one whose contract *is* the authored line structure — reads
/// the newline count through [`WideGap`] instead, and owes the written
/// fixed-point argument that goes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Gap {
    /// No trivia at all: the neighbours abut (`\ifmmode y\else`,
    /// `xmin=-5,xmax=5`). Breaking here would materialize a space token TeX
    /// contributes to the horizontal list — a typeset change the CST oracles
    /// cannot see, since whitespace is trivia to them and content to TeX — so it
    /// is a break opportunity only where a processor is proven to discard that
    /// space (see [`ContentKind::Keyval`]).
    Glued,
    /// Collapsible trivia carrying no blank line: inline whitespace, a lone
    /// newline, or any mix of the two. A break opportunity, and the one variant
    /// that must never be split back into its two spellings.
    ///
    /// `flat` is what a one-line rendering writes here, from [`Gap::flat`].
    Space { flat: String },
    /// Two or more newlines: an authored `\par`.
    Blank,
    /// The boundary ends at a `%` comment. The comment must terminate its line, so
    /// a break here is forced — and costs nothing, because the `%` already absorbs
    /// the line end.
    Comment,
}

impl Gap {
    /// A gap whose flat spelling is a single space: what every gap the layout is
    /// free to break renders as when it does not.
    fn space() -> Gap {
        Gap::Space {
            flat: " ".to_string(),
        }
    }

    /// Normalize a consumed trivia run. A run holding *any* newline flattens to one
    /// space — the only spelling a break reproduces — and its leading indentation is
    /// dropped, since the printer owns indentation and recreates it.
    fn from_run(newlines: usize, trailing_ws: String) -> Gap {
        match newlines {
            0 => Gap::Space { flat: trailing_ws },
            1 => Gap::space(),
            _ => Gap::Blank,
        }
    }

    /// What a *flat* rendering writes at this boundary: nothing where the author
    /// glued, the authored whitespace verbatim for a newline-free run, and a single
    /// space wherever the run carried a newline (blank line included).
    ///
    /// So a lone newline and a single authored space are indistinguishable here —
    /// that is the whole point — while a wider run (`\pgfpoint@oncoil{0    }`) still
    /// rides verbatim. That is not a leak of the unsafe predicate: every reader of
    /// `flat` emits it unchanged, so "the run was wider than one space" is a
    /// predicate they all preserve.
    fn flat(&self) -> &str {
        match self {
            Gap::Space { flat } => flat,
            Gap::Blank => " ",
            Gap::Glued | Gap::Comment => "",
        }
    }

    /// How a split point at this boundary renders in each mode: an [`Ir::Line`] (a
    /// space flat, a newline broken) wherever the author already wrote whitespace —
    /// the whitespace ↔ newline exchange that is TeX-identical anywhere, so it needs
    /// no permission — and an [`Ir::SoftLine`] (*nothing* flat) where they glued, so
    /// a fitting line stays byte-identical to the source and only the broken form
    /// materializes a space token.
    fn separator(&self) -> Ir {
        match self {
            Gap::Glued => Ir::soft_line(),
            _ => Ir::line(),
        }
    }
}

/// A [`Gap`] with the **unsafe** predicate — how many newlines the run spanned —
/// still readable.
///
/// Handed only to the Tier-2 sites whose contract *is* the authored line
/// structure: the byte-faithful stream ([`classify_trivia`]), the preserve-shaped
/// wrap modes, [`ReflowKind::Statement`]'s fallback content (a picture body's
/// `STATEMENT` nodes are structural, [`lower_statement`]), the expl3 fallback
/// statement, and the
/// command-only-line residue. Each owes a written fixed-point argument showing
/// every layout it can emit re-reads to itself; preservation-only rules have the
/// easy one (a hard line prints a newline, which re-reads as a newline, and is
/// kept again in place). Everything else takes [`Self::narrow`].
struct WideGap {
    gap: Gap,
    /// **Tier 2.** Reading this is reading the predicate the formatter does not
    /// preserve. Do not add a read without the fixed-point argument.
    newlines: usize,
}

impl WideGap {
    fn narrow(self) -> Gap {
        self.gap
    }
}

#[cfg(test)]
mod expl3_region_tests {
    use super::*;
    use tex_ls_parser::parser::parse;

    /// Run [`head_command_has_grouped_sibling_arg`] on the *innermost* `GROUP`
    /// descendant of `input` whose text contains `marker`.
    fn grouped_sibling_walk(input: &str, marker: &str) -> bool {
        let parsed = parse(input);
        assert!(parsed.errors.is_empty(), "test input should parse cleanly");
        let root = parsed.syntax();
        // Preorder puts an enclosing group before a nested one, so the last
        // match is the innermost.
        let group = root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::GROUP && n.text().to_string().contains(marker))
            .last()
            .expect("a group containing the marker");
        head_command_has_grouped_sibling_arg(&group)
    }

    #[test]
    fn grouped_sibling_walk_stops_at_the_statement_boundary() {
        // A grouped command in the *previous statement* must not suppress the
        // trailing-hang treatment for an unrelated `\bool_if:NF \l… {body}` —
        // free under the node-local read, since a node's children are one
        // statement by construction.
        let src = "\\ExplSyntaxOn\n\\tl_set:Nn \\l_x { v }\n\\bool_if:NF \\l_bool { body }\n\\ExplSyntaxOff\n";
        assert!(!grouped_sibling_walk(src, "body"));

        // Within one call the earlier grouped argument still counts: arity
        // attachment puts a recognized `\prop_get:NnNTF` call's `{#2}` and its
        // branches on one head node, so the multi-argument shape whose branch
        // list the hang path already lays out stably is read off the node.
        let src = "\\ExplSyntaxOn\n\\prop_get:NnNTF \\g_prop {#2} \\l_tl { branch } { f }\n\\ExplSyntaxOff\n";
        assert!(grouped_sibling_walk(src, "branch"));

        // An *aborted* call (the `F` branch is missing, so the five-slot spec
        // never resolves) keeps its groups on the small trailing command. An
        // unresolvable call is not the multi-argument shape this suppresses.
        let src =
            "\\ExplSyntaxOn\n\\prop_get:NnNTF \\g_prop {#2} \\l_tl { branch }\n\\ExplSyntaxOff\n";
        assert!(!grouped_sibling_walk(src, "branch"));
    }

    #[test]
    fn grouped_sibling_walk_matches_the_body_stream_segmentation() {
        // Inside a brace body, a preceding *statement's* grouped call must not
        // suppress the hang for the next statement's `\bool_if:NF … {body}` —
        // under the node-local read the earlier call's group belongs to that
        // call's own node, never to `{body}`'s owner.
        let src = "\\ExplSyntaxOn\n\\use:n { \\tl_set:Nn \\l_x { v } \\bool_if:NF \\l_b { body } }\n\\ExplSyntaxOff\n";
        assert!(!grouped_sibling_walk(src, "body"));
    }

    #[test]
    fn grouped_sibling_walk_ignores_out_of_region_prefix() {
        // The out-of-region `\emph{y}` sharing the authored line must not
        // read as an earlier grouped argument of the `\tl_put_right:Ne`
        // statement — its group belongs to `\emph`'s node, not the owner's.
        let src = "x \\emph{y} \\ExplSyntaxOn \\tl_put_right:Ne \\l_t { body } \\ExplSyntaxOff\n";
        assert!(!grouped_sibling_walk(src, "body"));
    }

    /// The expl3 regions of `input`, as `(start, end)` byte pairs.
    fn regions(input: &str) -> Vec<(usize, usize)> {
        let parsed = parse(input);
        assert!(parsed.errors.is_empty(), "test input should parse cleanly");
        expl3_regions(&parsed.syntax())
            .into_iter()
            .map(|r| (r.start().into(), r.end().into()))
            .collect()
    }

    #[test]
    fn on_off_pair_spans_both_toggles() {
        let input = r"a \ExplSyntaxOn b \ExplSyntaxOff c";
        let start = input.find(r"\ExplSyntaxOn").unwrap();
        let end = input.find(r"\ExplSyntaxOff").unwrap() + r"\ExplSyntaxOff".len();
        assert_eq!(regions(input), vec![(start, end)]);
    }

    #[test]
    fn unclosed_region_runs_to_eof() {
        let input = r"x \ExplSyntaxOn y z";
        let start = input.find(r"\ExplSyntaxOn").unwrap();
        assert_eq!(regions(input), vec![(start, input.len())]);
    }

    #[test]
    fn provides_expl_opens_to_eof() {
        let input = "\\ProvidesExplPackage\n\\cs_new:N \\foo:";
        assert_eq!(regions(input), vec![(0, input.len())]);
    }

    #[test]
    fn definee_provides_does_not_open_region() {
        // `\ProvidesExplPackage` as the definee of `\protected\def` is tokenized,
        // never executed, so it opens no formatter-owned region (issue #69).
        let input = "\\protected\\def\\ProvidesExplPackage{\\ProvidesPackage{demo}}\ntext";
        assert!(regions(input).is_empty());
    }

    #[test]
    fn definee_off_does_not_close_a_real_region() {
        // A gated-out `\ExplSyntaxOff` in definee position must not close the open
        // region — the gate skips it, so the region still runs to EOF.
        let input = "\\ExplSyntaxOn a \\let\\ExplSyntaxOff\\relax b";
        assert_eq!(regions(input), vec![(0, input.len())]);
    }

    #[test]
    fn stored_toggle_in_group_does_not_open_region() {
        // An `\ExplSyntaxOn` stored inside a definition body / attached group is
        // never executed at load, so it opens no region (issue #69).
        let input = "\\def\\store{\\ExplSyntaxOn \\foo:n {x}}\ntext";
        assert!(regions(input).is_empty());
    }

    #[test]
    fn top_level_provides_after_gated_definee_still_opens() {
        // The gate rejects only the false positives: a genuine top-level
        // `\ProvidesExplPackage` still opens a region even when a definee one
        // precedes it.
        let input = "\\def\\x{\\ExplSyntaxOn}\n\\ProvidesExplPackage\n\\cs_new:N \\foo:";
        let start = input.rfind("\\ProvidesExplPackage").unwrap();
        assert_eq!(regions(input), vec![(start, input.len())]);
    }

    #[test]
    fn stray_off_is_ignored() {
        assert!(regions(r"a \ExplSyntaxOff b").is_empty());
    }

    #[test]
    fn redundant_inner_on_does_not_restart() {
        let input = r"\ExplSyntaxOn a \ExplSyntaxOn b \ExplSyntaxOff";
        let end = input.find(r"\ExplSyntaxOff").unwrap() + r"\ExplSyntaxOff".len();
        assert_eq!(regions(input), vec![(0, end)]);
    }

    #[test]
    fn toggle_inside_verb_is_not_a_region() {
        // `\ExplSyntaxOn` inside a `\verb` argument lexes as a `VERB` token, never a
        // `CONTROL_WORD`, so it must not open a region (mirrors the lexer).
        assert!(regions(r"\verb|\ExplSyntaxOn| text").is_empty());
    }

    #[test]
    fn toggle_inside_comment_is_not_a_region() {
        assert!(regions("% \\ExplSyntaxOn\ntext").is_empty());
    }

    /// The expl3 regions of `input` parsed as a `.dtx`, as `(start, end)` pairs.
    fn regions_dtx(input: &str) -> Vec<(usize, usize)> {
        let config = tex_ls_parser::parser::LexConfig {
            flavor: LatexFlavor::Document,
            dtx: true,
        };
        let parsed = parse_with_flavor(input, config);
        assert!(parsed.errors.is_empty(), "test input should parse cleanly");
        expl3_regions(&parsed.syntax())
            .into_iter()
            .map(|r| (r.start().into(), r.end().into()))
            .collect()
    }

    #[test]
    fn dtx_region_owns_only_macrocode_bodies() {
        // The unmargined `␣%` line between the chunks is documentation, not code
        // (issue #58): the region intersects with the chunk bodies, so neither it
        // nor the margined doc line is formatter-owned.
        let input = "%    \\begin{macrocode}\n\
                     \\ExplSyntaxOn\n\
                     %    \\end{macrocode}\n\
                     \x20%\n\
                     % doc\n\
                     %    \\begin{macrocode}\n\
                     \\foo\n\
                     %    \\end{macrocode}\n";
        let first_frame = input.find("%    \\end{macrocode}").unwrap();
        // The `\begin` frame line's hole spans through its newline, so the second
        // region opens at the body's first code token.
        let second_body = input.find("\\foo").unwrap();
        let second_frame = input.rfind("%    \\end{macrocode}").unwrap();
        assert_eq!(
            regions_dtx(input),
            vec![
                (input.find("\\ExplSyntaxOn").unwrap(), first_frame),
                (second_body, second_frame),
            ]
        );
    }
}

#[cfg(test)]
mod gap_tests {
    use super::*;
    use tex_ls_parser::parser::parse;

    /// The [`Gap`] the boundary reads from the first collapsible-trivia run in
    /// `input`, taken through the same `consume_gap` every width-driven lowering
    /// uses. `descendants` is preorder, so the outermost node holding a trivia run
    /// answers first.
    fn first_gap(input: &str) -> Gap {
        let parsed = parse(input);
        assert!(parsed.errors.is_empty(), "test input should parse cleanly");
        for node in parsed.syntax().descendants() {
            let mut iter = node.children_with_tokens().peekable();
            while let Some(element) = iter.next() {
                let SyntaxElement::Token(token) = element else {
                    continue;
                };
                if is_collapsible_trivia(token.kind()) {
                    return consume_gap(&token, &mut iter);
                }
            }
        }
        panic!("no trivia run in {input:?}");
    }

    /// The property the whole normalization exists for: the two spellings the
    /// formatter converts between are one value at the boundary, so no rule taking
    /// a narrow [`Gap`] can tell them apart. This is the mechanical guard the
    /// K&R/Allman family (issues #71, #94, #96, #97) never had — each of those was
    /// one decision keying on exactly this difference.
    #[test]
    fn a_lone_newline_and_a_space_are_the_same_gap() {
        assert_eq!(first_gap("\\foo{a b}"), first_gap("\\foo{a\nb}"));
        assert_eq!(first_gap("\\foo{a\nb}"), Gap::space());
        // Indentation after the newline is the printer's to recreate, so it is
        // dropped rather than becoming part of the gap.
        assert_eq!(first_gap("\\foo{a\n    b}"), Gap::space());
        assert_eq!(first_gap("\\foo{a \n b}"), Gap::space());
    }

    /// Blank-line presence *is* preserved, so it keeps its own variant — and
    /// flattens to a single space, the only spelling a one-line rendering can
    /// write.
    #[test]
    fn a_blank_line_stays_visible() {
        assert_eq!(first_gap("\\foo{a\n\nb}"), Gap::Blank);
        assert_eq!(first_gap("\\foo{a\n\n\nb}"), Gap::Blank);
        assert_eq!(Gap::Blank.flat(), " ");
    }

    /// A run wider than one space rides verbatim wherever `flat` is read, so
    /// distinguishing it is not a read of the unsafe predicate — every reader
    /// preserves it. It is still not confusable with a break.
    #[test]
    fn a_wide_run_rides_verbatim() {
        assert_eq!(
            first_gap("\\foo{a    b}"),
            Gap::Space {
                flat: "    ".to_string()
            }
        );
        assert_ne!(first_gap("\\foo{a    b}"), first_gap("\\foo{a\nb}"));
    }

    /// The split-point rendering the folded-in `DividerGap`/`KeyBreak` prototypes
    /// agreed on: a glued junction renders as *nothing* flat, so a fitting line
    /// stays byte-identical to the source and only the broken form materializes a
    /// space token TeX would typeset.
    #[test]
    fn a_glued_junction_separates_without_a_flat_space() {
        assert!(matches!(Gap::Glued.separator(), Ir::SoftLine));
        assert_eq!(Gap::Glued.flat(), "");
        assert!(matches!(Gap::space().separator(), Ir::Line));
        assert!(matches!(Gap::Blank.separator(), Ir::Line));
        assert!(matches!(Gap::Comment.separator(), Ir::Line));
    }
}
