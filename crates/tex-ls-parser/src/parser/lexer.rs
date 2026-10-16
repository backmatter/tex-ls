//! Lossless lexer for LaTeX surface syntax.
//!
//! The lexer recognizes only bounded modes whose delimiters are visible in the
//! source, including verbatim regions and explicit catcode toggles.

mod context;
mod operands;
use crate::parser::reader_facts::Pending;
use crate::parser::reader_facts::declaration_header;
use crate::parser::reader_facts::next_pending;
use crate::parser::reader_facts::primitive_replacement_reader;
use crate::parser::reader_facts::short_verb_observer;
use crate::parser::reader_facts::token_list_reader;
pub use context::{ParseCtx, VerbCtx};
pub(crate) use operands::PrimitiveOperandScan;

use std::collections::HashSet;
use std::rc::Rc;

use smol_str::SmolStr;

use crate::semantic::signature::{ArgKind, ArgSpec, builtin};
use crate::syntax::SyntaxKind;

/// A single lexed token: its kind plus the exact source slice it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub text: SmolStr,
}

/// The LaTeX file flavor, fixing the lexer's *initial* catcode regime. A
/// document (`.tex`) starts in the ordinary regime; a package or class
/// (`.sty`/`.cls`) is loaded under an implicit `\makeatletter`, so `@` is a
/// letter from the first byte. A trailing explicit `\makeatother` still applies.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LatexFlavor {
    /// A `.tex` document: ordinary catcodes at the start.
    #[default]
    Document,
    /// A `.sty`/`.cls` package or class: `@` is a letter from the start.
    Package,
}

impl LatexFlavor {
    /// Whether the lexer should begin with `@` already a letter (the implicit
    /// `\makeatletter` of a package/class load).
    fn letter_mode_start(self) -> bool {
        matches!(self, LatexFlavor::Package)
    }
}

/// The lexer's per-parse mode. [`flavor`](Self::flavor) fixes the *initial*
/// catcode regime (a `.sty`/`.cls` starts under an implicit `\makeatletter`),
/// while [`dtx`](Self::dtx) is an orthogonal axis: when set, the lexer runs the
/// bounded line-oriented docstrip mode for a `.dtx` file — line-leading `%`
/// margins become [`DOC_MARGIN`](SyntaxKind::DOC_MARGIN) trivia, line-leading
/// `%<…>` guards become [`GUARD`](SyntaxKind::GUARD) trivia, and `macrocode`
/// bodies lex as ordinary code. The two axes are
/// independent because a `.dtx`'s catcode regime varies *by layer* (its
/// documentation is `Document`-flavored, its `macrocode` `Package`-flavored), so
/// `dtx` cannot be folded into a [`LatexFlavor`] variant.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LexConfig {
    /// The initial catcode regime.
    pub flavor: LatexFlavor,
    /// Run the docstrip (`.dtx`) line-oriented lexer mode.
    pub dtx: bool,
}

impl From<LatexFlavor> for LexConfig {
    /// A plain (non-`.dtx`) config of the given flavor — the common case, so a
    /// bare [`LatexFlavor`] coerces into a [`LexConfig`] at call sites.
    fn from(flavor: LatexFlavor) -> Self {
        Self { flavor, dtx: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An expl3 lexer-mode transition.
pub enum ExplToggle {
    On,
    Off,
}

/// Returns the expl3 transition named by a control word.
pub fn expl_toggle(text: &str) -> Option<ExplToggle> {
    match text {
        "\\ExplSyntaxOn"
        | "\\ProvidesExplPackage"
        | "\\ProvidesExplClass"
        | "\\ProvidesExplFile" => Some(ExplToggle::On),
        "\\ExplSyntaxOff" => Some(ExplToggle::Off),
        _ => None,
    }
}

pub(crate) fn dtx_has_expl_signal(input: &str) -> bool {
    input.contains("\\ProvidesExpl")
        || input
            .lines()
            .any(|l| l.starts_with("%<@@=") && l[5..].contains('>'))
}

/// Lexes a document into a lossless token stream.
pub fn lex(input: &str) -> Vec<Token> {
    lex_with(input, &ParseCtx::default(), LexConfig::default())
}

/// Lexes with per-parse facts and an explicit file configuration.
pub fn lex_with(input: &str, ctx: &ParseCtx, config: LexConfig) -> Vec<Token> {
    Lexer::new(input, ctx, config, None).run()
}

pub(crate) fn lex_with_offset(
    input: &str,
    ctx: &ParseCtx,
    config: LexConfig,
    source_offset: usize,
) -> Vec<Token> {
    let mut lexer = Lexer::new(input, ctx, config, None);
    lexer.source_offset = source_offset;
    lexer.run()
}

pub(crate) fn lex_with_implicit_expl_offset(
    input: &str,
    ctx: &ParseCtx,
    config: LexConfig,
    implicit_expl: bool,
    source_offset: usize,
) -> Vec<Token> {
    let mut lexer = Lexer::new(input, ctx, config, Some(implicit_expl));
    lexer.source_offset = source_offset;
    lexer.run()
}

pub(crate) fn lex_literal_commands(
    input: &str,
    chars: [char; 3],
    comment: Option<char>,
    ctx: &ParseCtx,
    config: LexConfig,
    source_offset: usize,
) -> Vec<Token> {
    let normalized: String = input
        .chars()
        .map(|c| {
            if c == chars[0] {
                '\\'
            } else if c == chars[1] {
                '{'
            } else if c == chars[2] {
                '}'
            } else if matches!(c, '\\' | '{' | '}') {
                '\u{1}'
            } else {
                c
            }
        })
        .collect();
    let mut lexer = Lexer::new(&normalized, ctx, config, None);
    lexer.source_offset = source_offset;
    lexer.alltt = true;
    lexer.alltt_comment = comment;
    lexer.opaque_nested_captures = true;
    let mut tokens = lexer.run();
    let mut offset = 0;
    let mut depth = 0usize;
    for token in &mut tokens {
        let len = token.text.len();
        token.text = SmolStr::new(&input[offset..offset + len]);
        offset += len;
        match token.kind {
            SyntaxKind::L_BRACE => depth += 1,
            SyntaxKind::R_BRACE => depth = depth.saturating_sub(1),
            SyntaxKind::WORD | SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE if depth == 0 => {
                token.kind = SyntaxKind::VERBATIM_BODY;
            }
            _ => {}
        }
    }
    tokens
}

#[derive(Debug, Clone, Copy)]
struct MacrocodeSave {
    at_letter: bool,
    expl_syntax: bool,
}

#[derive(Clone, Copy)]
struct AllttSave {
    depth: usize,
    at_letter: bool,
    expl_syntax: bool,
    alltt: bool,
}

#[derive(Clone)]
struct ScopeSave {
    at_letter: bool,
    expl_syntax: bool,
    percent_comment: bool,
    dollar_math: bool,
    uncertain_percent: bool,
    uncertain_dollar: bool,
    uncertain_at: bool,
    short_verbs: Rc<HashSet<char>>,
    short_verb_ascii: u128,
    embedded: Rc<super::embedded::State>,
}

#[derive(Clone, Copy)]
enum StoredPlan {
    PrimitiveName,
    PrimitiveBody,
    DeclarationName {
        groups: u8,
        optional: u8,
        environment: bool,
        star: bool,
    },
    DeclarationBody {
        groups: u8,
        optional: u8,
    },
    Arguments {
        args: &'static [ArgSpec],
        slot: usize,
        star: bool,
    },
}

#[derive(Clone, Copy)]
enum StoredGroup {
    Name {
        valid: bool,
        seen: bool,
        environment: bool,
    },
    Brace,
    Bracket,
}

struct StoredFrame {
    base: usize,
    plan: StoredPlan,
    group: Option<StoredGroup>,
}

#[derive(Default)]
struct StoredArguments {
    frames: Vec<StoredFrame>,
    operands: PrimitiveOperandScan,
    offset: usize,
    protected: usize,
    brackets: Vec<usize>,
}

impl StoredArguments {
    fn protects(&self, kind: SyntaxKind, depth: usize) -> bool {
        self.protected > 0 && matches!(kind, SyntaxKind::L_BRACE | SyntaxKind::R_BRACE)
            || kind == SyntaxKind::R_BRACKET && self.brackets.last() == Some(&depth)
            || kind == SyntaxKind::L_BRACE
                && self.frames.last().is_some_and(|frame| {
                    frame.base == depth
                        && frame.group.is_none()
                        && matches!(frame.plan, StoredPlan::PrimitiveBody)
                })
    }

    fn pop(&mut self) {
        if let Some(frame) = self.frames.pop() {
            match frame.group {
                Some(StoredGroup::Brace) => self.protected -= 1,
                Some(StoredGroup::Bracket) => {
                    self.protected -= 1;
                    self.brackets.pop();
                }
                _ => {}
            }
        }
    }

    fn observe(&mut self, token: &Token, depth: usize, ctx: &ParseCtx, source_offset: usize) {
        let kind = token.kind;
        let text = token.text.as_str();
        let offset = source_offset + self.offset;
        self.offset += text.len();
        let inert = self.operands.observe_at(kind, text, ctx, offset);
        if crate::syntax::is_trivia(kind)
            || matches!(kind, SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD)
        {
            return;
        }
        // A malformed header cannot retain a scope after its enclosing group.
        while self.frames.last().is_some_and(|frame| {
            depth < frame.base
                || matches!(kind, SyntaxKind::R_BRACE | SyntaxKind::R_BRACKET)
                    && depth == frame.base
                    && frame.group.is_none()
        }) {
            self.pop();
        }
        let mut in_name = false;
        if let Some(frame) = self.frames.last_mut() {
            match frame.group {
                Some(StoredGroup::Name {
                    valid,
                    seen,
                    environment,
                }) => {
                    in_name = true;
                    if kind == SyntaxKind::R_BRACE && depth == frame.base + 1 {
                        if valid && seen {
                            if let StoredPlan::DeclarationName {
                                groups, optional, ..
                            } = frame.plan
                            {
                                frame.plan = StoredPlan::DeclarationBody { groups, optional };
                            }
                            frame.group = None;
                        } else {
                            self.pop();
                        }
                    } else {
                        frame.group = Some(StoredGroup::Name {
                            valid: valid
                                && !seen
                                && if environment {
                                    kind == SyntaxKind::WORD
                                } else {
                                    matches!(
                                        kind,
                                        SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL
                                    )
                                },
                            seen: true,
                            environment,
                        });
                    }
                }
                Some(StoredGroup::Brace)
                    if kind == SyntaxKind::R_BRACE && depth == frame.base + 1 =>
                {
                    self.protected -= 1;
                    frame.group = None;
                    if Self::finished(frame.plan) {
                        self.pop();
                    }
                }
                Some(StoredGroup::Bracket)
                    if kind == SyntaxKind::R_BRACKET && depth == frame.base =>
                {
                    self.protected -= 1;
                    self.brackets.pop();
                    frame.group = None;
                    if Self::finished(frame.plan) {
                        self.pop();
                    }
                }
                Some(_) => {}
                None => match frame.plan {
                    StoredPlan::PrimitiveName => {
                        if matches!(kind, SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL) {
                            frame.plan = StoredPlan::PrimitiveBody;
                        } else {
                            self.pop();
                        }
                        in_name = true;
                    }
                    StoredPlan::PrimitiveBody => {
                        in_name = true;
                        if kind == SyntaxKind::L_BRACE {
                            frame.plan = StoredPlan::DeclarationBody {
                                groups: 0,
                                optional: 0,
                            };
                            frame.group = Some(StoredGroup::Brace);
                            self.protected += 1;
                        }
                    }
                    StoredPlan::DeclarationName {
                        groups,
                        optional,
                        environment,
                        star,
                    } => {
                        in_name = true;
                        if star && kind == SyntaxKind::WORD && text == "*" {
                            frame.plan = StoredPlan::DeclarationName {
                                groups,
                                optional,
                                environment,
                                star: false,
                            };
                        } else if kind == SyntaxKind::L_BRACE {
                            frame.group = Some(StoredGroup::Name {
                                valid: true,
                                seen: false,
                                environment,
                            });
                        } else if !environment
                            && matches!(kind, SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL)
                        {
                            frame.plan = StoredPlan::DeclarationBody { groups, optional };
                        } else {
                            self.pop();
                        }
                    }
                    StoredPlan::DeclarationBody { groups, optional } => {
                        if kind == SyntaxKind::L_BRACKET && optional > 0 {
                            frame.plan = StoredPlan::DeclarationBody {
                                groups,
                                optional: optional - 1,
                            };
                            frame.group = Some(StoredGroup::Bracket);
                            self.protected += 1;
                            self.brackets.push(depth);
                        } else if kind == SyntaxKind::L_BRACE && groups > 0 {
                            frame.plan = StoredPlan::DeclarationBody {
                                groups: groups - 1,
                                optional: 0,
                            };
                            frame.group = Some(StoredGroup::Brace);
                            self.protected += 1;
                        } else {
                            self.pop();
                        }
                    }
                    StoredPlan::Arguments {
                        args,
                        mut slot,
                        star,
                    } => {
                        if star && kind == SyntaxKind::WORD && text == "*" {
                            frame.plan = StoredPlan::Arguments {
                                args,
                                slot,
                                star: false,
                            };
                            return;
                        }
                        let delimiter = match kind {
                            SyntaxKind::L_BRACE => Some(ArgKind::Brace),
                            SyntaxKind::VERB if text.starts_with('{') => Some(ArgKind::Brace),
                            SyntaxKind::L_BRACKET => Some(ArgKind::Bracket),
                            _ => None,
                        };
                        if delimiter.is_some_and(|delimiter| {
                            crate::semantic::signature::match_arg_slot(args, &mut slot, delimiter)
                                .is_some()
                        }) {
                            frame.plan = StoredPlan::Arguments {
                                args,
                                slot,
                                star: false,
                            };
                            frame.group = match kind {
                                SyntaxKind::L_BRACE => Some(StoredGroup::Brace),
                                SyntaxKind::L_BRACKET => Some(StoredGroup::Bracket),
                                _ => None,
                            };
                            match frame.group {
                                Some(StoredGroup::Brace) => self.protected += 1,
                                Some(StoredGroup::Bracket) => {
                                    self.protected += 1;
                                    self.brackets.push(depth);
                                }
                                _ => {}
                            }
                            if frame.group.is_none() && Self::finished(frame.plan) {
                                self.pop();
                            }
                        } else {
                            self.pop();
                        }
                    }
                },
            }
        }
        if !in_name
            && !inert
            && kind == SyntaxKind::CONTROL_WORD
            && let Some(name) = text.strip_prefix('\\')
            && !ctx.is_reader_suppressed_at(name, offset)
        {
            let plan = if primitive_replacement_reader(name) {
                Some(StoredPlan::PrimitiveName)
            } else if let Some((groups, optional, environment)) = declaration_header(name) {
                Some(StoredPlan::DeclarationName {
                    groups,
                    optional,
                    environment,
                    star: optional > 0,
                })
            } else {
                builtin()
                    .command(name)
                    .filter(|sig| !sig.args.is_empty() && !sig.verbatim)
                    .map(|sig| StoredPlan::Arguments {
                        args: sig.args.as_ref(),
                        slot: 0,
                        star: matches!(
                            name,
                            "part"
                                | "chapter"
                                | "section"
                                | "subsection"
                                | "subsubsection"
                                | "paragraph"
                                | "subparagraph"
                                | "operatorname"
                                | "includegraphics"
                                | "DeclareMathOperator"
                        ),
                    })
            };
            if let Some(plan) = plan {
                self.frames.push(StoredFrame {
                    base: depth,
                    plan,
                    group: None,
                });
            }
        }
    }

    fn finished(plan: StoredPlan) -> bool {
        match plan {
            StoredPlan::DeclarationBody { groups, .. } => groups == 0,
            StoredPlan::Arguments { args, slot, .. } => slot >= args.len(),
            _ => false,
        }
    }
}

struct ReleaseHeader {
    first_name_token: Option<usize>,
    description: std::ops::Range<usize>,
}

struct Lexer<'a> {
    input: &'a str,
    ctx: &'a ParseCtx,
    config: LexConfig,
    implicit_expl: bool,
    out: Vec<Token>,
    pos: usize,
    at_letter: bool,
    expl_syntax: bool,
    at_line_start: bool,
    in_doc_line: bool,
    short_verbs: Rc<HashSet<char>>,
    short_verb_ascii: u128,
    macrocode: Option<MacrocodeSave>,
    pending: Option<Pending>,
    brace_depth: usize,
    brace_counted: usize,
    alltt: bool,
    alltt_stack: Vec<AllttSave>,
    token_list_depth: Option<usize>,
    token_list_pending: bool,
    scopes: Vec<ScopeSave>,
    embedded: Rc<super::embedded::State>,
    percent_comment: bool,
    dollar_math: bool,
    uncertain_percent: bool,
    uncertain_dollar: bool,
    uncertain_at: bool,
    conditional_depth: usize,
    alltt_comment: Option<char>,
    opaque_nested_captures: bool,
    global_prefix: bool,
    stringify_next: bool,
    stored: StoredArguments,
    source_offset: usize,
    primitive_operands: PrimitiveOperandScan,
    raw_readers: RawReaders<'a>,
    release_header: Option<ReleaseHeader>,
}

impl<'a> Lexer<'a> {
    fn new(
        input: &'a str,
        ctx: &'a ParseCtx,
        config: LexConfig,
        implicit_expl_override: Option<bool>,
    ) -> Self {
        let implicit_expl = if config.dtx {
            implicit_expl_override.unwrap_or_else(|| dtx_has_expl_signal(input))
        } else {
            false
        };
        Self {
            input,
            ctx,
            config,
            implicit_expl,
            out: Vec::new(),
            pos: 0,
            at_letter: config.flavor.letter_mode_start(),
            expl_syntax: false,
            at_line_start: true,
            in_doc_line: false,
            short_verbs: Rc::new(if config.dtx {
                HashSet::from(['|'])
            } else {
                HashSet::new()
            }),
            short_verb_ascii: if config.dtx { 1u128 << b'|' } else { 0 },
            macrocode: None,
            pending: None,
            brace_depth: 0,
            brace_counted: 0,
            alltt: false,
            alltt_stack: Vec::new(),
            token_list_depth: None,
            token_list_pending: false,
            scopes: Vec::new(),
            embedded: Rc::default(),
            percent_comment: true,
            dollar_math: true,
            uncertain_percent: false,
            uncertain_dollar: false,
            uncertain_at: false,
            conditional_depth: 0,
            alltt_comment: None,
            opaque_nested_captures: false,
            global_prefix: false,
            stringify_next: false,
            stored: StoredArguments::default(),
            source_offset: 0,
            primitive_operands: PrimitiveOperandScan::default(),
            raw_readers: RawReaders::new(input),
            release_header: None,
        }
    }

    fn run(mut self) -> Vec<Token> {
        while self.pos < self.input.len() {
            self.sync_brace_depth();
            if self.try_release_argument() {
                continue;
            }
            if self
                .token_list_depth
                .is_some_and(|depth| self.brace_depth < depth)
            {
                self.token_list_depth = None;
            }
            // Docstrip margins and guards belong to the physical documentation
            // layer, including while a TeX operation is waiting for an operand.
            if self.config.dtx
                && self.at_line_start
                && ((!self.stringify_next
                    && self.token_list_depth.is_none()
                    && !self.primitive_operands.has_operand()
                    && self.try_macrocode_frame())
                    || self.try_guard()
                    || self.try_doc_margin())
            {
                continue;
            }
            if (self.stringify_next
                || self.token_list_depth.is_some()
                || self.primitive_operands.has_operand())
                && self.try_doc_comment()
            {
                continue;
            }
            if self.stringify_next {
                let word_len = control_word_len(self.rest(), self.at_letter, self.expl_syntax);
                self.lex_token(word_len);
                continue;
            }
            if self.token_list_depth.is_some() {
                let word_len = control_word_len(self.rest(), self.at_letter, self.expl_syntax);
                self.lex_token(word_len);
                continue;
            }
            if self.primitive_operands.has_operand() {
                let word_len = control_word_len(self.rest(), self.at_letter, self.expl_syntax);
                self.lex_token(word_len);
                continue;
            }
            // These bytes cannot start a capture or a mode transition. Keep the
            // ordinary token routine authoritative for boundaries and state;
            // avoid probing every raw-capture family for each prose token.
            if !self.config.dtx
                && self.pending.is_none()
                && self.short_verbs.is_empty()
                && matches!(self.input.as_bytes()[self.pos], b'a'..=b'z' | b'A'..=b'Z' | b' ' | b'\t' | b'\n' | b'\r')
            {
                self.lex_token(None);
                continue;
            }
            if self.try_alltt_environment()
                || self.try_verbatim_environment()
                || self.try_verbatim_arg_environment()
            {
                continue;
            }
            let word_len = control_word_len(self.rest(), self.at_letter, self.expl_syntax);
            if self.try_release_header(word_len)
                || self.try_verbatim_command(word_len)
                || self.try_short_verb()
                || self.try_char_constant()
                || self.try_doc_comment()
            {
                continue;
            }
            self.lex_token(word_len);
        }
        self.out
    }

    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn push(&mut self, kind: SyntaxKind, text: &str) {
        self.out.push(Token {
            kind,
            text: SmolStr::new(text),
        });
    }

    fn consume(&mut self, len: usize) {
        self.pos += len;
        self.at_line_start = false;
        self.pending = None;
    }

    fn sync_brace_depth(&mut self) {
        while self.brace_counted < self.out.len() {
            let token = &self.out[self.brace_counted];
            self.stored
                .observe(token, self.brace_depth, self.ctx, self.source_offset);
            match token.kind {
                SyntaxKind::L_BRACE => {
                    self.brace_depth += 1;
                    self.save_scope();
                }
                SyntaxKind::R_BRACE => {
                    self.brace_depth = self.brace_depth.saturating_sub(1);
                    self.restore_scope();
                }
                _ => {}
            }
            self.brace_counted += 1;
        }
    }

    fn save_scope(&mut self) {
        self.scopes.push(ScopeSave {
            at_letter: self.at_letter,
            expl_syntax: self.expl_syntax,
            percent_comment: self.percent_comment,
            dollar_math: self.dollar_math,
            uncertain_percent: self.uncertain_percent,
            uncertain_dollar: self.uncertain_dollar,
            uncertain_at: self.uncertain_at,
            short_verbs: self.short_verbs.clone(),
            short_verb_ascii: self.short_verb_ascii,
            embedded: self.embedded.clone(),
        });
    }

    fn restore_scope(&mut self) {
        if let Some(save) = self.scopes.pop() {
            self.at_letter = save.at_letter;
            self.expl_syntax = save.expl_syntax;
            self.percent_comment = save.percent_comment;
            self.dollar_math = save.dollar_math;
            self.uncertain_percent = save.uncertain_percent;
            self.uncertain_dollar = save.uncertain_dollar;
            self.uncertain_at = save.uncertain_at;
            self.short_verbs = save.short_verbs;
            self.short_verb_ascii = save.short_verb_ascii;
            self.embedded = save.embedded;
        }
    }

    fn try_macrocode_frame(&mut self) -> bool {
        if !(self.config.dtx && self.at_line_start) {
            return false;
        }
        let rest = self.rest();
        let want_begin = self.macrocode.is_none();
        let Some(consumed) =
            lex_macrocode_frame(rest, want_begin, &mut self.raw_readers, &mut self.out)
        else {
            return false;
        };
        match self.macrocode.take() {
            Some(saved) => {
                self.at_letter = saved.at_letter;
                self.expl_syntax = saved.expl_syntax;
            }
            None => {
                self.macrocode = Some(MacrocodeSave {
                    at_letter: self.at_letter,
                    expl_syntax: self.expl_syntax,
                });
                self.at_letter = true;
                if self.implicit_expl {
                    self.expl_syntax = true;
                }
            }
        }
        self.consume(consumed);
        true
    }

    fn try_guard(&mut self) -> bool {
        let rest = self.rest();
        if !(self.config.dtx && self.at_line_start && rest.starts_with("%<")) {
            return false;
        }
        let Some(rel) = rest[2..].find(['>', '\n', '\r']) else {
            return false;
        };
        if rest.as_bytes()[2 + rel] != b'>' {
            return false;
        }
        let len = 2 + rel + 1;
        self.push(SyntaxKind::GUARD, &rest[..len]);
        self.pos += len;
        self.at_line_start = false;
        true
    }

    fn try_doc_margin(&mut self) -> bool {
        let rest = self.rest();
        if !(self.config.dtx
            && self.at_line_start
            && self.macrocode.is_none()
            && rest.starts_with('%')
            && !rest.starts_with("%<"))
        {
            return false;
        }
        self.push(SyntaxKind::DOC_MARGIN, "%");
        self.pos += 1;
        self.at_line_start = false;
        self.in_doc_line = true;
        true
    }

    // alltt keeps control sequences and groups active, but makes the other
    // TeX special characters literal. Never treat it as an opaque raw body.
    fn try_alltt_environment(&mut self) -> bool {
        let rest = self.rest();
        if self.pending == Some(Pending::Def) {
            return false;
        }
        if let Some(("alltt", prefix_len)) = self.raw_readers.begin_name(rest) {
            if !self.ctx.is_literal_alltt_environment("alltt") {
                return false;
            }
            self.alltt_stack.push(AllttSave {
                depth: self.brace_depth,
                at_letter: self.at_letter,
                expl_syntax: self.expl_syntax,
                alltt: self.alltt,
            });
            push_env_delimiter(&mut self.out, "\\begin", "alltt");
            self.consume(prefix_len);
            self.alltt = true;
            return true;
        }
        if self.alltt
            && let Some(after) = rest.strip_prefix("\\end")
        {
            let gap_len = inline_ws_len(after);
            let gap = &after[..gap_len];
            let after = &after[gap_len..];
            if let Some(tail) = after.strip_prefix("{alltt}")
                && self
                    .alltt_stack
                    .last()
                    .is_some_and(|save| save.depth == self.brace_depth)
            {
                let save = self.alltt_stack.pop().unwrap();
                push_env_delimiter_with_gap(&mut self.out, "\\end", gap, "alltt");
                self.consume(rest.len() - tail.len());
                self.at_letter = save.at_letter;
                self.expl_syntax = save.expl_syntax;
                self.alltt = save.alltt;
                return true;
            }
        }
        false
    }

    fn try_verbatim_environment(&mut self) -> bool {
        let (rest, ctx) = (self.rest(), self.ctx);
        if let Some((name, prefix_len)) = self.raw_readers.begin_name(rest) {
            if !builtin_environment_name_lengths().contains(&name.len())
                && !ctx.has_environment_name_length(name.len())
                && !self.embedded.environment_name_lengths.contains(&name.len())
            {
                return false;
            }
            let comment = self
                .embedded
                .comments
                .get(name)
                .copied()
                .or_else(|| (name == "comment").then_some(Some(false)));
            if let Some(included) = comment
                && !ctx.is_environment_suppressed(name)
                && ctx
                    .declared_environment(name)
                    .is_none_or(|sig| sig.verbatim_body && builtin().environment(name) == Some(sig))
            {
                push_env_delimiter(&mut self.out, "\\begin", name);
                if included == Some(true) && !self.opaque_nested_captures {
                    self.consume(prefix_len);
                    return true;
                }
                let region = &rest[prefix_len..];
                let len = if name == "comment"
                    && ctx
                        .verbatim_comment_start
                        .is_some_and(|start| start <= self.source_offset + self.pos)
                {
                    verbatim_comment_body_len(region)
                } else {
                    comment_body_len(region, name)
                };
                if len > 0 {
                    self.push(
                        if included == Some(false) {
                            SyntaxKind::EXCLUDED_BODY
                        } else {
                            SyntaxKind::VERBATIM_BODY
                        },
                        &region[..len],
                    );
                }
                self.consume(prefix_len + len);
                return true;
            }
            if let Some(spec) =
                self.embedded
                    .environment(name, &rest[prefix_len..], &mut self.raw_readers)
                && !ctx.is_environment_suppressed(name)
                && ctx
                    .declared_environment(name)
                    .is_none_or(|sig| builtin().environment(name) == Some(sig))
            {
                let header = &rest[prefix_len..];
                let mut args_len = 0;
                if !header
                    .trim_start_matches([' ', '\t'])
                    .starts_with(['\n', '\r'])
                    && let Some((_, tail)) = self.raw_readers.embedded_argument(header, '[', ']')
                {
                    args_len = header.len() - tail.len();
                }
                if (name == "minted"
                    || name.ends_with('*') && self.embedded.environments.contains_key(name))
                    && let Some((_, tail)) =
                        self.raw_readers
                            .embedded_argument(&header[args_len..], '{', '}')
                {
                    args_len = header.len() - tail.len();
                }
                push_env_delimiter(&mut self.out, "\\begin", name);
                lex_into(&header[..args_len], &mut self.out);
                let region = &header[args_len..];
                let len = verbatim_body_len(region, name);
                if self.opaque_nested_captures {
                    self.push(SyntaxKind::VERBATIM_BODY, &region[..len]);
                } else {
                    super::embedded::body(
                        &region[..len],
                        &spec,
                        &mut self.out,
                        ctx,
                        self.config,
                        self.source_offset + self.pos + prefix_len + args_len,
                    );
                }
                self.consume(prefix_len + args_len + len);
                return true;
            }
        }
        let Some(consumed) =
            lex_verbatim_environment(rest, ctx, &mut self.raw_readers, &mut self.out)
        else {
            return false;
        };
        self.consume(consumed);
        true
    }

    fn try_verbatim_arg_environment(&mut self) -> bool {
        if self.macrocode.is_some() {
            return false;
        }
        let rest = self.rest();
        let Some(consumed) = lex_verbatim_arg_environment(
            rest,
            self.ctx,
            self.config.dtx && self.in_doc_line,
            &mut self.raw_readers,
            &mut self.out,
        ) else {
            return false;
        };
        self.consume(consumed);
        true
    }

    fn verb_at_stored_closer(&self, word_len: Option<usize>) -> bool {
        word_len.is_some_and(|len| {
            let rest = self.rest();
            &rest[..len] == "\\verb"
                && self.stored.protects(SyntaxKind::R_BRACE, self.brace_depth)
                && rest[len..]
                    .strip_prefix('*')
                    .unwrap_or(&rest[len..])
                    .starts_with('}')
        })
    }

    fn try_release_header(&mut self, word_len: Option<usize>) -> bool {
        let Some(head_len) = word_len else {
            return false;
        };
        let rest = self.rest();
        if &rest[..head_len] != "\\IncludeInRelease"
            || self.release_header.is_some()
            || self.pending == Some(Pending::Def)
            || self.config.dtx
            || self.alltt
            || self.uncertain_percent
            || !self.percent_comment
            || !self
                .ctx
                .release_header_start
                .is_some_and(|start| start <= self.source_offset + self.pos)
        {
            return false;
        }
        let Some(mut header) = release_header_roles(&rest[head_len..], &mut self.raw_readers)
        else {
            return false;
        };
        let offset = self.pos + head_len;
        header.first_name_token = header.first_name_token.map(|position| position + offset);
        header.description = header.description.start + offset..header.description.end + offset;
        self.release_header = Some(header);
        self.push(SyntaxKind::CONTROL_WORD, &rest[..head_len]);
        self.consume(head_len);
        true
    }

    fn try_release_argument(&mut self) -> bool {
        let Some(header) = self.release_header.as_mut() else {
            return false;
        };
        if self.alltt
            || self.uncertain_percent
            || !self.percent_comment
            || self.pos > header.description.start
        {
            self.release_header = None;
            return false;
        }
        if self.pos == header.description.start {
            let end = header.description.end;
            self.release_header = None;
            self.push(SyntaxKind::VERB, &self.input[self.pos..end]);
            self.consume(end - self.pos);
            return true;
        }
        if header
            .first_name_token
            .is_some_and(|position| position <= self.pos)
        {
            let position = header.first_name_token.take().unwrap();
            if position == self.pos && !self.uncertain_at {
                let rest = self.rest();
                let len = if rest.starts_with('\\') {
                    control_word_len(rest, self.at_letter, self.expl_syntax)
                        .or_else(|| rest[1..].chars().next().map(|c| 1 + c.len_utf8()))
                } else {
                    rest.chars().next().map(char::len_utf8)
                };
                let Some(len) = len else { return false };
                self.push(SyntaxKind::STRINGIFIED_TOKEN, &rest[..len]);
                self.consume(len);
                return true;
            }
        }
        false
    }

    fn try_verbatim_command(&mut self, word_len: Option<usize>) -> bool {
        if self.pending == Some(Pending::Def)
            || self.verb_at_stored_closer(word_len)
            || word_len.is_some_and(|len| {
                self.ctx
                    .is_reader_suppressed_at(&self.rest()[1..len], self.source_offset + self.pos)
            })
        {
            return false;
        }
        let (rest, ctx) = (self.rest(), self.ctx);
        if let Some(word_len) = word_len
            && !ctx.is_suppressed(&rest[1..word_len])
            && let Some(spec) =
                self.embedded
                    .inline(&rest[1..word_len], &rest[word_len..], &mut self.raw_readers)
        {
            let name = &rest[1..word_len];
            if !spec.settings.python() && !self.embedded.inline.contains_key(name) {
                let consumed = lex_verbatim_command(
                    rest,
                    Some(word_len),
                    ctx,
                    self.config.dtx && self.in_doc_line,
                    &mut self.raw_readers,
                    &mut self.out,
                );
                if let Some(consumed) = consumed {
                    self.consume(consumed);
                    return true;
                }
                return false;
            }
            let mut head = word_len;
            if let Some((_, tail)) = self.raw_readers.embedded_argument(&rest[head..], '[', ']') {
                head = rest.len() - tail.len();
            }
            if matches!(name, "mintinline" | "mint")
                && let Some((_, tail)) = self.raw_readers.embedded_argument(&rest[head..], '{', '}')
            {
                head = rest.len() - tail.len();
            }
            head += tex_whitespace_len(&rest[head..]);
            let region = &rest[head..];
            let len = if region.starts_with('{') {
                self.raw_readers.balanced_group_len(region, b'}')
            } else {
                delimited_len(region)
            };
            if let Some(len) = len
                && let Some(open) = region.chars().next()
            {
                let close = if open == '{' { '}' } else { open };
                let closed = len > open.len_utf8() && region[..len].ends_with(close);
                let end = len - usize::from(closed) * close.len_utf8();
                self.push(SyntaxKind::CONTROL_WORD, &rest[..word_len]);
                lex_into(&rest[word_len..head], &mut self.out);
                if !closed {
                    self.push(SyntaxKind::VERB, &region[..len]);
                    self.consume(head + len);
                    return true;
                }
                self.push(SyntaxKind::CODE_OPEN, &region[..open.len_utf8()]);
                super::embedded::body(
                    &region[open.len_utf8()..end],
                    &spec,
                    &mut self.out,
                    ctx,
                    self.config,
                    self.source_offset + self.pos + head + open.len_utf8(),
                );
                if closed {
                    self.push(SyntaxKind::CODE_CLOSE, &region[end..len]);
                }
                self.consume(head + len);
                return true;
            }
        }
        let Some(consumed) = lex_verbatim_command(
            rest,
            word_len,
            ctx,
            self.config.dtx && self.in_doc_line,
            &mut self.raw_readers,
            &mut self.out,
        ) else {
            return false;
        };
        self.consume(consumed);
        true
    }

    fn short_verb_contains(&self, character: &char) -> bool {
        if character.is_ascii() {
            self.short_verb_ascii & (1u128 << u32::from(*character)) != 0
        } else {
            self.short_verbs.contains(character)
        }
    }

    fn try_short_verb(&mut self) -> bool {
        if self.short_verbs.is_empty()
            || self.macrocode.is_some()
            || matches!(self.pending, Some(Pending::Delim | Pending::LiteralToken))
        {
            return false;
        }
        let rest = self.rest();
        if !rest
            .chars()
            .next()
            .is_some_and(|c| self.short_verb_contains(&c))
        {
            return false;
        }
        let Some(len) = delimited_len(rest) else {
            return false;
        };
        let delimiter = rest.chars().next().unwrap();
        if len <= delimiter.len_utf8() || !rest[..len].ends_with(delimiter) {
            return false;
        }
        self.push(SyntaxKind::VERB, &rest[..len]);
        self.consume(len);
        true
    }

    fn try_char_constant(&mut self) -> bool {
        let numeric_context = self.pending == Some(Pending::CharConstant);
        let rest = self.rest();
        let Some(after) = rest.strip_prefix('`') else {
            return false;
        };
        let Some(c) = after.chars().next() else {
            return false;
        };
        if matches!(c, '\n' | '\r') || (self.brace_depth > 0 && matches!(c, '{' | '}')) {
            return false;
        }
        let len = if c == '\\' {
            match after[1..]
                .chars()
                .next()
                .filter(|e| !matches!(e, '\n' | '\r'))
            {
                Some(e) => 2 + e.len_utf8(),
                None => return false,
            }
        } else {
            1 + c.len_utf8()
        };
        let alignment_cell = self.pending.is_none() && c == '\\' && rest[len..].starts_with('&');
        if !numeric_context && !alignment_cell {
            return false;
        }
        self.push(SyntaxKind::WORD, &rest[..len]);
        self.consume(len);
        true
    }

    fn try_doc_comment(&mut self) -> bool {
        let rest = self.rest();
        if !(self.in_doc_line && rest.starts_with("^^A")) {
            return false;
        }
        let len = run_len(rest, |c| c != '\n' && c != '\r');
        self.primitive_operands.observe_at(
            SyntaxKind::COMMENT,
            &rest[..len],
            self.ctx,
            self.source_offset + self.pos,
        );
        self.push(SyntaxKind::COMMENT, &rest[..len]);
        let pending = self.pending;
        self.consume(len);
        self.pending = pending;
        true
    }

    fn lex_token(&mut self, word_len: Option<usize>) {
        let rest = self.rest();
        // A literal control-sequence operand has no active inline delimiter.
        // Decide before token sizing: trimming a captured VERB afterwards can
        // already have swallowed the opening brace of a definition or group.
        let literal_verb = word_len.filter(|&len| {
            &rest[..len] == "\\verb"
                && (self.pending == Some(Pending::Def)
                    || self.primitive_operands.has_operand()
                    || self.token_list_depth.is_some()
                    || self.stringify_next
                    || self.verb_at_stored_closer(word_len)
                    || self
                        .ctx
                        .is_reader_suppressed_at("verb", self.source_offset + self.pos))
        });
        let (mut kind, mut len) = if let Some(len) = literal_verb {
            (SyntaxKind::CONTROL_WORD, len)
        } else {
            next_token(rest, word_len, self.expl_syntax)
        };
        if self.alltt && self.alltt_comment.is_some_and(|c| rest.starts_with(c)) {
            kind = SyntaxKind::COMMENT;
            len = run_len(rest, |c| c != '\n' && c != '\r');
        } else if self.alltt
            && kind == SyntaxKind::WORD
            && let Some(comment) = self.alltt_comment
            && let Some(at) = rest[..len].find(comment)
        {
            len = at;
        }
        if !self.alltt {
            if rest.starts_with('%') && (self.uncertain_percent || !self.percent_comment) {
                kind = if self.uncertain_percent {
                    SyntaxKind::UNCERTAIN
                } else {
                    SyntaxKind::WORD
                };
                len = if self.uncertain_percent {
                    run_len(rest, |c| c != '\n' && c != '\r')
                } else {
                    1
                };
            } else if rest.starts_with('$') && (self.uncertain_dollar || !self.dollar_math) {
                kind = if self.uncertain_dollar {
                    SyntaxKind::UNCERTAIN
                } else {
                    SyntaxKind::WORD
                };
                len = if self.uncertain_dollar {
                    run_len(rest, |c| c != '\n' && c != '\r')
                } else {
                    1
                };
            }
        }
        if self.uncertain_at && rest.starts_with('\\') {
            let count = run_len(&rest[1..], |c| {
                c.is_ascii_alphabetic() || c == '@' || self.expl_syntax && matches!(c, ':' | '_')
            });
            if rest[1..1 + count].contains('@') {
                kind = SyntaxKind::UNCERTAIN;
                len = 1 + count;
            }
        }
        if self.token_list_pending && kind == SyntaxKind::L_BRACE {
            self.token_list_depth = Some(self.brace_depth + 1);
        }
        if self.token_list_pending
            && !matches!(
                kind,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
            )
        {
            self.token_list_pending = false;
        }
        if self.token_list_depth.is_some()
            && !matches!(
                kind,
                SyntaxKind::L_BRACE
                    | SyntaxKind::R_BRACE
                    | SyntaxKind::HASH
                    | SyntaxKind::COMMENT
                    | SyntaxKind::WHITESPACE
                    | SyntaxKind::NEWLINE
            )
        {
            kind = SyntaxKind::WORD;
        }
        let operand_kind = if self.stringify_next && !crate::syntax::is_trivia(kind) {
            SyntaxKind::STRINGIFIED_TOKEN
        } else {
            kind
        };
        let inert_operand = self.primitive_operands.observe_at(
            operand_kind,
            &rest[..len],
            self.ctx,
            self.source_offset + self.pos,
        );
        if kind == SyntaxKind::CONTROL_WORD
            && rest[..len].get(1..).is_some_and(token_list_reader)
            && !inert_operand
            && !self.stringify_next
            && self.pending != Some(Pending::Def)
            && !self.ctx.is_suppressed(&rest[1..len])
        {
            self.token_list_pending = true;
        }
        if self.alltt
            && !(kind == SyntaxKind::COMMENT
                && self.alltt_comment.is_some_and(|c| rest.starts_with(c)))
            && matches!(
                rest.as_bytes()[0],
                b'%' | b'$' | b'#' | b'^' | b'_' | b'&' | b'~'
            )
        {
            kind = SyntaxKind::WORD;
            len = 1;
        }
        if self.pending == Some(Pending::Delim) && kind == SyntaxKind::WORD {
            len = rest.chars().next().expect("rest is non-empty").len_utf8();
        }
        if kind == SyntaxKind::WORD
            && !self.short_verbs.is_empty()
            && let Some((i, c)) = rest[..len]
                .char_indices()
                .find(|(_, c)| self.short_verb_contains(c))
        {
            len = if i == 0 { c.len_utf8() } else { i };
        }
        if self.stringify_next
            && !matches!(
                kind,
                SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
            )
        {
            self.stringify_next = false;
            // A replacement list is scanned and brace-balanced before its
            // commands execute. String may hide a command spelling here, but
            // cannot take a structural delimiter away from that scanner.
            if kind != SyntaxKind::UNCERTAIN && !(self.stored.protects(kind, self.brace_depth)) {
                len = if let Some(word_len) = word_len {
                    word_len
                } else if kind == SyntaxKind::CONTROL_SYMBOL {
                    len
                } else {
                    rest.chars().next().unwrap().len_utf8()
                };
                kind = SyntaxKind::STRINGIFIED_TOKEN;
            }
        }
        debug_assert!(len > 0, "lexer made no progress at byte {}", self.pos);
        let text = &rest[..len];
        if kind == SyntaxKind::CONTROL_WORD && !inert_operand {
            if text == "\\string"
                && self.pending != Some(Pending::Def)
                && !self.ctx.is_suppressed("string")
            {
                self.stringify_next = true;
            }
            self.apply_toggles(text, &rest[len..]);
        }
        // A literal operand consumes an existing pending reader but cannot
        // arm another reader from an inert or source-retired control word.
        let pending_kind = if inert_operand
            || kind == SyntaxKind::CONTROL_WORD
                && self
                    .ctx
                    .is_reader_suppressed_at(&text[1..], self.source_offset + self.pos)
        {
            SyntaxKind::STRINGIFIED_TOKEN
        } else {
            kind
        };
        self.pending = next_pending(self.pending, pending_kind, text);
        self.push(kind, text);
        self.at_line_start =
            kind == SyntaxKind::NEWLINE || text.ends_with('\n') || text.ends_with('\r');
        if self.at_line_start {
            self.in_doc_line = false;
        }
        self.pos += len;
    }

    fn apply_toggles(&mut self, text: &str, after: &str) {
        if self.pending == Some(Pending::Def) {
            return;
        }
        if text == "\\global" {
            self.global_prefix = true;
            return;
        }
        if text != "\\catcode" {
            self.global_prefix = false;
        }
        if short_verb_observer(&text[1..])
            && self
                .ctx
                .is_reader_suppressed_at(&text[1..], self.source_offset + self.pos)
        {
            return;
        }
        if super::embedded::observes_settings_command(&text[1..])
            && !self
                .ctx
                .is_reader_suppressed_at(&text[1..], self.source_offset + self.pos)
        {
            Rc::make_mut(&mut self.embedded).observe(
                text,
                after,
                self.conditional_depth > 0,
                &mut self.raw_readers,
            );
        }
        match text {
            "\\begingroup" | "\\bgroup" => self.save_scope(),
            "\\endgroup" | "\\egroup" => self.restore_scope(),
            "\\catcode" => {
                if let Some((character, value)) = literal_catcode(after) {
                    let uncertain = self.conditional_depth > 0;
                    match character {
                        '%' => {
                            self.uncertain_percent = uncertain || !matches!(value, Some(12 | 14));
                            if !self.uncertain_percent {
                                self.percent_comment = value == Some(14);
                            }
                        }
                        '$' => {
                            self.uncertain_dollar = uncertain || !matches!(value, Some(3 | 12));
                            if !self.uncertain_dollar {
                                self.dollar_math = value == Some(3);
                            }
                        }
                        '@' => {
                            self.uncertain_at = uncertain || !matches!(value, Some(11 | 12));
                            if !self.uncertain_at {
                                self.at_letter = value == Some(11);
                            }
                        }
                        _ => {}
                    }
                    if self.global_prefix {
                        for scope in &mut self.scopes {
                            match character {
                                '%' => {
                                    scope.percent_comment = self.percent_comment;
                                    scope.uncertain_percent = self.uncertain_percent;
                                }
                                '$' => {
                                    scope.dollar_math = self.dollar_math;
                                    scope.uncertain_dollar = self.uncertain_dollar;
                                }
                                '@' => {
                                    scope.at_letter = self.at_letter;
                                    scope.uncertain_at = self.uncertain_at;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                self.global_prefix = false;
            }

            "\\makeatletter" => self.at_letter = true,
            "\\makeatother" => self.at_letter = false,
            "\\MakeShortVerb" | "\\DefineShortVerb" => {
                if let Some(c) = short_verb_char(after, Some(&mut self.raw_readers))
                    && !self.short_verb_contains(&c)
                {
                    Rc::make_mut(&mut self.short_verbs).insert(c);
                    if c.is_ascii() {
                        self.short_verb_ascii |= 1u128 << u32::from(c);
                    }
                }
            }
            "\\DeleteShortVerb" | "\\UndefineShortVerb" => {
                if let Some(c) = short_verb_char(after, Some(&mut self.raw_readers))
                    && self.short_verb_contains(&c)
                {
                    Rc::make_mut(&mut self.short_verbs).remove(&c);
                    if c.is_ascii() {
                        self.short_verb_ascii &= !(1u128 << u32::from(c));
                    }
                }
            }
            "\\documentclass" | "\\LoadClass" => {
                if doc_class_enables_bar(after, &mut self.raw_readers)
                    && !self.short_verb_contains(&'|')
                {
                    Rc::make_mut(&mut self.short_verbs).insert('|');
                    self.short_verb_ascii |= 1u128 << b'|';
                }
            }
            _ => {
                if text.starts_with("\\if") {
                    self.conditional_depth += 1;
                } else if text == "\\fi" {
                    self.conditional_depth = self.conditional_depth.saturating_sub(1);
                }
                if let Some(toggle) = expl_toggle(text) {
                    self.expl_syntax = matches!(toggle, ExplToggle::On);
                }
            }
        }
    }
}

fn control_word_len(rest: &str, at_letter: bool, expl_syntax: bool) -> Option<usize> {
    let after = rest.strip_prefix('\\')?;
    let letters = run_len(after, |c| is_letter(c, at_letter, expl_syntax));
    (letters > 0).then_some(1 + letters)
}

fn next_token(rest: &str, word_len: Option<usize>, expl_syntax: bool) -> (SyntaxKind, usize) {
    let c = rest.chars().next().expect("rest is non-empty");
    match c {
        '\\' => lex_control(rest, word_len),
        '%' => (
            SyntaxKind::COMMENT,
            run_len(rest, |c| c != '\n' && c != '\r'),
        ),
        '{' => (SyntaxKind::L_BRACE, 1),
        '}' => (SyntaxKind::R_BRACE, 1),
        '[' => (SyntaxKind::L_BRACKET, 1),
        ']' => (SyntaxKind::R_BRACKET, 1),
        '$' => (SyntaxKind::DOLLAR, 1),
        '&' => (SyntaxKind::AMPERSAND, 1),
        '#' => (SyntaxKind::HASH, 1),
        '^' => (SyntaxKind::CARET, 1),
        '_' if !expl_syntax => (SyntaxKind::UNDERSCORE, 1),
        '~' => (SyntaxKind::TILDE, 1),
        '\n' => (SyntaxKind::NEWLINE, 1),
        '\r' => {
            let len = if rest.as_bytes().get(1) == Some(&b'\n') {
                2
            } else {
                1
            };
            (SyntaxKind::NEWLINE, len)
        }
        ' ' | '\t' => (
            SyntaxKind::WHITESPACE,
            run_len(rest, |c| c == ' ' || c == '\t'),
        ),
        _ => (
            SyntaxKind::WORD,
            run_len(rest, |c| is_word_char(c) || (expl_syntax && c == '_')),
        ),
    }
}

fn lex_control(rest: &str, word_len: Option<usize>) -> (SyntaxKind, usize) {
    match word_len {
        Some(word_len) => {
            if &rest[..word_len] == "\\verb"
                && let Some(arg_len) = verb_len(&rest[word_len..])
            {
                return (SyntaxKind::VERB, word_len + arg_len);
            }
            (SyntaxKind::CONTROL_WORD, word_len)
        }
        None => {
            let after = &rest[1..];
            let symbol_len = if after.starts_with("\r\n") {
                2
            } else {
                after.chars().next().map_or(0, char::len_utf8)
            };
            (SyntaxKind::CONTROL_SYMBOL, 1 + symbol_len)
        }
    }
}

fn verb_len(after: &str) -> Option<usize> {
    match after.strip_prefix('*') {
        Some(rest) => Some(1 + delimited_len(rest)?),
        None => delimited_len(after),
    }
}

fn delimited_len(after: &str) -> Option<usize> {
    let mut chars = after.chars();
    let delim = chars.next()?;
    if delim.is_whitespace() {
        return None;
    }
    let mut consumed = delim.len_utf8();
    for c in chars {
        if c == '\n' || c == '\r' {
            return Some(consumed);
        }
        consumed += c.len_utf8();
        if c == delim {
            return Some(consumed);
        }
    }
    Some(consumed)
}

/// Bound optional-argument probes in the source-wide reparse guard. Repeated
/// unclosed options must not repeatedly scan the entire remaining source.
/// An option beyond the budget conservatively retains the full-parse fallback.
pub(crate) fn may_declare_letter_short_verb(after: &str) -> bool {
    const MAX_OPTION_BYTES: usize = 1024;
    let operand = skip_inline_ws(after.strip_prefix('*').unwrap_or(after)).trim_start();
    if operand.starts_with('[')
        && operand.len() > MAX_OPTION_BYTES
        && super::embedded::argument(
            &operand[..operand.floor_char_boundary(MAX_OPTION_BYTES)],
            '[',
            ']',
        )
        .is_none()
    {
        return true;
    }
    short_verb_char(after, None).is_some_and(|character| character.is_ascii_alphabetic())
}

fn short_verb_char(after: &str, readers: Option<&mut RawReaders<'_>>) -> Option<char> {
    let mut s = skip_inline_ws(after.strip_prefix('*').unwrap_or(after));
    let option = match readers {
        Some(readers) => readers.embedded_argument(s, '[', ']'),
        None => super::embedded::argument(s, '[', ']'),
    };
    if let Some((_, rest)) = option {
        s = skip_inline_ws(rest);
    }
    let (body, braced) = match s.strip_prefix('{') {
        Some(inner) => (skip_inline_ws(inner), true),
        None => (s, false),
    };
    let arg = body.strip_prefix('\\')?;
    let c = arg.chars().next()?;
    if c == '\n' || c == '\r' {
        return None;
    }
    if braced && !skip_inline_ws(&arg[c.len_utf8()..]).starts_with('}') {
        return None;
    }
    Some(c)
}

const BAR_SHORT_VERB_CLASSES: [&str; 5] = ["ltxdoc", "ltxguide", "ltnews", "l3doc", "amsldoc"];

fn doc_class_enables_bar(after: &str, readers: &mut RawReaders<'_>) -> bool {
    let mut s = skip_inline_ws(after);
    if let Some(rest) = s.strip_prefix('[') {
        match readers.next_close(rest, 1) {
            Some(i) => s = rest[i + 1..].trim_start_matches([' ', '\t', '\n', '\r']),
            None => return false,
        }
    }
    let Some(rest) = s.strip_prefix('{') else {
        return false;
    };
    let Some(close) = readers.next_close(rest, 0) else {
        return false;
    };
    BAR_SHORT_VERB_CLASSES.contains(&rest[..close].trim())
}

fn lex_verbatim_environment(
    rest: &str,
    ctx: &ParseCtx,
    readers: &mut RawReaders<'_>,
    out: &mut Vec<Token>,
) -> Option<usize> {
    let (name, prefix_len) = readers.begin_name(rest)?;
    if !builtin_environment_name_lengths().contains(&name.len())
        && !ctx.has_environment_name_length(name.len())
    {
        return None;
    }
    if ctx.is_environment_suppressed(name)
        || ctx
            .declared_environment(name)
            .is_some_and(|sig| !sig.verbatim_body)
    {
        return None;
    }
    let args: &[ArgSpec] = match ctx.verbatim_environment_args(name) {
        Some(args) => args,
        None => {
            &builtin()
                .environment(name)
                .filter(|e| e.verbatim_body)?
                .args
        }
    };

    push_env_delimiter(out, "\\begin", name);

    let args_region = &rest[prefix_len..];
    let args_len = scan_verbatim_args(args_region, args, readers);
    lex_into(&args_region[..args_len], out);

    let body_region = &args_region[args_len..];
    let body_len = verbatim_body_len(body_region, name);
    if body_len > 0 {
        out.push(Token {
            kind: SyntaxKind::VERBATIM_BODY,
            text: SmolStr::new(&body_region[..body_len]),
        });
    }
    Some(prefix_len + args_len + body_len)
}

fn verbatim_body_len(body: &str, name: &str) -> usize {
    const LEAD: &str = "\\end{";
    let mut from = 0;
    while let Some(rel) = body[from..].find(LEAD) {
        let at = from + rel;
        let after = &body[at + LEAD.len()..];
        if let Some(tail) = after.strip_prefix(name)
            && tail.starts_with('}')
        {
            return at;
        }
        from = at + LEAD.len();
    }
    body.len()
}

fn comment_body_len(body: &str, name: &str) -> usize {
    let closer = format!("\\end{{{name}}}");
    let mut start = 0;
    for line in body.split_inclusive(['\n', '\r']) {
        if line.trim_end_matches(['\n', '\r']) == closer {
            return start;
        }
        start += line.len();
    }
    body.len()
}

// The verbatim package's comment reader accepts indentation and horizontal
// space after end, then resumes the ordinary reader on the same physical line.
fn verbatim_comment_body_len(body: &str) -> usize {
    for (at, _) in body.match_indices("\\end") {
        if body[at + "\\end".len()..]
            .trim_start_matches([' ', '\t'])
            .starts_with("{comment}")
        {
            return at;
        }
    }
    body.len()
}

fn release_header_roles(source: &str, readers: &mut RawReaders<'_>) -> Option<ReleaseHeader> {
    fn gap(source: &str) -> usize {
        let mut offset = 0;
        loop {
            offset += tex_whitespace_len(&source[offset..]);
            if source[offset..].starts_with('%') {
                offset += source[offset..]
                    .find(['\n', '\r'])
                    .unwrap_or(source.len() - offset);
            } else {
                return offset;
            }
        }
    }
    let mut offset = gap(source);
    let mut first_name_token = None;
    for argument in 0..3 {
        let start = offset;
        offset += readers.stored_group_len(&source[offset..], b'}')?;
        if argument == 1 {
            first_name_token = release_name_first_token(&source[start + 1..offset - 1])
                .map(|position| position + start + 1);
        }
        if argument == 2 {
            return Some(ReleaseHeader {
                first_name_token,
                description: start..offset,
            });
        }
        offset += gap(&source[offset..]);
        if argument == 0 && source[offset..].starts_with('[') {
            offset += readers.stored_group_len(&source[offset..], b']')?;
            offset += gap(&source[offset..]);
        }
    }
    None
}

// \string consumes the first actual name token, including a leading space.
// A comment removes its endline; indentation on the following line is skipped
// in TeX's new_line state. A further blank endline introduces a paragraph token.
fn release_name_first_token(source: &str) -> Option<usize> {
    let mut offset = 0;
    while source[offset..].starts_with('%') {
        offset += source[offset..].find(['\r', '\n'])?;
        offset += if source[offset..].starts_with("\r\n") {
            2
        } else {
            1
        };
        offset += source[offset..]
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
    }
    // Keep the balanced source group topology. Leading whitespace consumes the
    // string operand without protecting a later token; a blank endline also
    // precedes any later control sequence with a paragraph token.
    source[offset..].chars().next().and_then(|character| {
        (!matches!(character, ' ' | '\t' | '\r' | '\n' | '{' | '}')).then_some(offset)
    })
}

fn literal_catcode(after: &str) -> Option<(char, Option<u8>)> {
    let after = after.trim_start();
    let (character, after) = if let Some(after) = after.strip_prefix('`') {
        let after = after.strip_prefix('\\').unwrap_or(after);
        let c = after.chars().next()?;
        (c, &after[c.len_utf8()..])
    } else {
        let len = after.bytes().take_while(u8::is_ascii_digit).count();
        let number: u32 = after[..len].parse().ok()?;
        (char::from_u32(number)?, &after[len..])
    };
    let after = after.trim_start();
    let after = after.strip_prefix('=').unwrap_or(after).trim_start();
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    let end = &after[digits..];
    let value = if digits > 0 && (!end.starts_with('\\') || end.starts_with("\\relax")) {
        after[..digits].parse().ok()
    } else {
        None
    };
    Some((character, value))
}

fn lex_verbatim_arg_environment(
    rest: &str,
    ctx: &ParseCtx,
    on_dtx_doc_line: bool,
    readers: &mut RawReaders<'_>,
    out: &mut Vec<Token>,
) -> Option<usize> {
    let (name, prefix_len) = readers.begin_name(rest)?;
    if !builtin_environment_name_lengths().contains(&name.len())
        && !ctx.has_environment_name_length(name.len())
    {
        return None;
    }
    if !ctx.is_verbatim_arg_environment(name) {
        return None;
    }

    let region = &rest[prefix_len..];
    let gap = |text: &str| {
        if on_dtx_doc_line {
            dtx_doc_argument_gap_len(text)
                .filter(|_| {
                    !text
                        .trim_start_matches([' ', '\t', '\n', '\r'])
                        .starts_with("%<")
                })
                .unwrap_or_else(|| inline_ws_len(text))
        } else {
            tex_whitespace_len(text)
        }
    };
    let leading_gap = gap(region);
    let mut args_len = leading_gap;
    let mut option_len = 0;
    if let Some(after) = region[args_len..].strip_prefix('[') {
        let i = readers.next_close(after, 2)?;
        if after.as_bytes()[i] != b']' {
            return None;
        }
        option_len = 1 + i + 1;
        args_len += option_len;
        args_len += gap(&region[args_len..]);
    }
    let arg_region = &region[args_len..];
    let delim = arg_region.chars().next()?;
    let braced_content_len = if delim == '{' {
        Some(readers.braced_verb_content_len(&arg_region[1..], on_dtx_doc_line)?)
    } else {
        if !delim.is_ascii_punctuation()
            || matches!(delim, '\\' | '}' | '[' | ']' | '%' | '*' | '$')
        {
            return None;
        }
        None
    };

    push_env_delimiter(out, "\\begin", name);
    let emit_gap = |text: &str, out: &mut Vec<Token>| {
        if on_dtx_doc_line && dtx_doc_argument_gap_len(text).is_some() {
            lex_dtx_doc_argument_gap(text, out);
        } else {
            lex_into(text, out);
        }
    };
    emit_gap(&region[..leading_gap], out);
    lex_into(&region[leading_gap..leading_gap + option_len], out);
    emit_gap(&region[leading_gap + option_len..args_len], out);
    let verb_len = match braced_content_len {
        Some(content_len) => {
            out.push(Token {
                kind: SyntaxKind::L_BRACE,
                text: SmolStr::new("{"),
            });
            out.push(Token {
                kind: SyntaxKind::VERB,
                text: SmolStr::new(&arg_region[1..1 + content_len]),
            });
            out.push(Token {
                kind: SyntaxKind::R_BRACE,
                text: SmolStr::new("}"),
            });
            1 + content_len + 1
        }
        None => {
            let verb_len = delimited_len(arg_region)?;
            out.push(Token {
                kind: SyntaxKind::VERB,
                text: SmolStr::new(&arg_region[..verb_len]),
            });
            verb_len
        }
    };
    Some(prefix_len + args_len + verb_len)
}

fn lex_macrocode_frame(
    rest: &str,
    want_begin: bool,
    readers: &mut RawReaders<'_>,
    out: &mut Vec<Token>,
) -> Option<usize> {
    let indent = if want_begin { inline_ws_len(rest) } else { 0 };
    let after_pct = rest[indent..].strip_prefix('%')?;
    let ws_len = inline_ws_len(after_pct);
    let body = &after_pct[ws_len..];
    let (control, open) = if want_begin {
        ("\\begin", "\\begin{")
    } else {
        ("\\end", "\\end{")
    };
    let after_open = body.strip_prefix(open)?;
    let close = readers.next_close(after_open, 0)?;
    let name = &after_open[..close];
    if name != "macrocode" && name != "macrocode*" {
        return None;
    }
    let after_close = &after_open[close + 1..];
    let tail = skip_inline_ws(after_close);
    let comment_tail = !want_begin && tail.starts_with('%');
    if !(tail.is_empty() || tail.starts_with('\n') || tail.starts_with('\r') || comment_tail) {
        return None;
    }

    if indent > 0 {
        out.push(Token {
            kind: SyntaxKind::WHITESPACE,
            text: SmolStr::new(&rest[..indent]),
        });
    }
    out.push(Token {
        kind: SyntaxKind::DOC_MARGIN,
        text: SmolStr::new("%"),
    });
    if ws_len > 0 {
        out.push(Token {
            kind: SyntaxKind::WHITESPACE,
            text: SmolStr::new(&after_pct[..ws_len]),
        });
    }
    push_env_delimiter(out, control, name);
    Some(indent + 1 + ws_len + control.len() + 1 + name.len() + 1)
}

fn lex_verbatim_command(
    rest: &str,
    word_len: Option<usize>,
    ctx: &ParseCtx,
    on_dtx_doc_line: bool,
    readers: &mut RawReaders<'_>,
    out: &mut Vec<Token>,
) -> Option<usize> {
    let word_len = word_len?;
    let name = &rest[1..word_len];
    if name == "verb" {
        return None;
    }
    let (leading, delimited): (&[ArgSpec], bool) = match ctx.leading_args(name) {
        Some(args) => (args, false),
        None => {
            if ctx.is_suppressed(name) {
                return None;
            }
            let sig = builtin().command(name)?;
            if sig.verbatim {
                (&sig.args, sig.verbatim_delimited)
            } else {
                let raw = sig.args.iter().position(|arg| arg.verbatim)?;
                if sig.args[raw].kind != ArgKind::Brace {
                    return None;
                }
                (&sig.args[..raw], false)
            }
        }
    };

    let after_word = &rest[word_len..];
    let args_len = scan_verbatim_args(after_word, leading, readers);

    let region = &after_word[args_len..];
    let dtx_gap = (!delimited && on_dtx_doc_line)
        .then(|| dtx_doc_argument_gap_len(region))
        .flatten();
    let ws_len = if let Some(len) = dtx_gap {
        len
    } else if delimited {
        inline_ws_len(region)
    } else {
        tex_whitespace_len(region)
    };
    let arg_region = &region[ws_len..];
    let arg_len = match arg_region.bytes().next() {
        Some(b'{') => readers.balanced_group_len(arg_region, b'}')?,
        Some(_) if delimited => delimited_len(arg_region)?,
        _ => return None,
    };

    out.push(Token {
        kind: SyntaxKind::CONTROL_WORD,
        text: SmolStr::new(&rest[..word_len]),
    });
    lex_into(&after_word[..args_len], out);
    if ws_len > 0 {
        if dtx_gap.is_some() {
            lex_dtx_doc_argument_gap(&region[..ws_len], out);
        } else {
            out.push(Token {
                kind: SyntaxKind::WHITESPACE,
                text: SmolStr::new(&region[..ws_len]),
            });
        }
    }
    out.push(Token {
        kind: SyntaxKind::VERB,
        text: SmolStr::new(&arg_region[..arg_len]),
    });
    Some(word_len + args_len + ws_len + arg_len)
}

fn scan_verbatim_args(region: &str, args: &[ArgSpec], readers: &mut RawReaders<'_>) -> usize {
    let bytes = region.as_bytes();
    let mut pos = 0;
    for arg in args {
        let probe = pos + inline_ws_len(&region[pos..]);
        let (open, close) = match arg.kind {
            ArgKind::Bracket => (b'[', b']'),
            ArgKind::Brace => (b'{', b'}'),
        };
        if bytes.get(probe) != Some(&open) {
            continue;
        }
        match readers.balanced_group_len(&region[probe..], close) {
            Some(len) => pos = probe + len,
            None => break, // unbalanced: treat the remainder as body
        }
    }
    pos
}

fn builtin_environment_name_lengths() -> &'static HashSet<usize> {
    static LENGTHS: std::sync::OnceLock<HashSet<usize>> = std::sync::OnceLock::new();
    LENGTHS.get_or_init(|| builtin().environment_names().map(str::len).collect())
}

// Every reader receives a suffix of this lexer's immutable source. Cache only
// unmatched openings left by a failed scan, under that reader's exact rules.
// Balanced nested groups and escaped openings are never marked as failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RawReaderMode {
    Brace,
    Bracket,
    NameArgument,
    DtxNameArgument,
    EmbeddedBrace,
    EmbeddedBracket,
    StoredBrace,
    StoredBracket,
}

pub(crate) struct RawReaders<'a> {
    source: &'a str,
    next_closes: [Option<(usize, Option<usize>)>; 3],
    failed: HashSet<(usize, usize, RawReaderMode)>,
    #[cfg(test)]
    scanned_bytes: usize,
}

impl<'a> RawReaders<'a> {
    // Stored arguments balance braces and ignore comments before executing any
    // token. Square brackets are data inside a brace argument.
    fn stored_group_len(&mut self, source: &str, close: u8) -> Option<usize> {
        let open = if close == b'}' { b'{' } else { b'[' };
        if source.as_bytes().first() != Some(&open) {
            return None;
        }
        let mode = if close == b'}' {
            RawReaderMode::StoredBrace
        } else {
            RawReaderMode::StoredBracket
        };
        let opening = self.offset(source);
        let end = opening + source.len();
        if self.failed.contains(&(opening, end, mode)) {
            return None;
        }
        let mut stack = vec![(opening, mode)];
        let bytes = source.as_bytes();
        let mut at = 1;
        while at < bytes.len() {
            #[cfg(test)]
            {
                self.scanned_bytes += 1;
            }
            match bytes[at] {
                b'\\' => {
                    at += 2;
                    continue;
                }
                b'%' => {
                    at += source[at..].find(['\n', '\r']).unwrap_or(source.len() - at);
                    continue;
                }
                byte if byte == close && stack.len() == 1 => return Some(at + 1),
                b'{' => stack.push((opening + at, RawReaderMode::StoredBrace)),
                b'}' if stack.len() > 1 => {
                    stack.pop();
                }
                b'}' => break,
                _ => {}
            }
            at += 1;
        }
        self.failed
            .extend(stack.into_iter().map(|(offset, mode)| (offset, end, mode)));
        None
    }

    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            source,
            next_closes: [None; 3],
            failed: HashSet::new(),
            #[cfg(test)]
            scanned_bytes: 0,
        }
    }

    fn offset(&self, suffix: &str) -> usize {
        let offset = suffix.as_ptr() as usize - self.source.as_ptr() as usize;
        debug_assert!(offset + suffix.len() <= self.source.len());
        offset
    }

    fn next_close(&mut self, source: &str, boundary: usize) -> Option<usize> {
        let start = self.offset(source);
        debug_assert_eq!(start + source.len(), self.source.len());
        let close = match self.next_closes[boundary] {
            Some((from, close)) if start >= from && close.is_none_or(|close| start <= close) => {
                close
            }
            _ => {
                let close = source
                    .bytes()
                    .position(|byte| {
                        #[cfg(test)]
                        {
                            self.scanned_bytes += 1;
                        }
                        match boundary {
                            0 => byte == b'}',
                            1 => byte == b']',
                            2 => matches!(byte, b']' | b'\n' | b'\r'),
                            _ => unreachable!("unknown raw-reader boundary"),
                        }
                    })
                    .map(|relative| start + relative);
                self.next_closes[boundary] = Some((start, close));
                close
            }
        }?;
        Some(close - start)
    }

    fn begin_name<'s>(&mut self, rest: &'s str) -> Option<(&'s str, usize)> {
        let after = rest.strip_prefix("\\begin{")?;
        let close = self.next_close(after, 0)?;
        Some((&after[..close], "\\begin{".len() + close + 1))
    }

    fn balanced_group_len(&mut self, s: &str, close: u8) -> Option<usize> {
        let mode = if close == b'}' {
            RawReaderMode::Brace
        } else {
            RawReaderMode::Bracket
        };
        let offset = self.offset(s);
        let end = offset + s.len();
        if self.failed.contains(&(offset, end, mode)) {
            return None;
        }
        let bytes = s.as_bytes();
        let mut stack = vec![(offset, close)];
        let mut i = 1;
        while i < bytes.len() {
            #[cfg(test)]
            {
                self.scanned_bytes += 1;
            }
            match bytes[i] {
                b'\\' => {
                    i += 2;
                    continue;
                }
                b'{' => stack.push((offset + i, b'}')),
                b'[' => stack.push((offset + i, b']')),
                c @ (b'}' | b']') if stack.last().is_some_and(|&(_, close)| close == c) => {
                    stack.pop();
                    if stack.is_empty() {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        self.failed
            .extend(stack.into_iter().map(|(opening, close)| {
                (
                    opening,
                    end,
                    if close == b'}' {
                        RawReaderMode::Brace
                    } else {
                        RawReaderMode::Bracket
                    },
                )
            }));
        None
    }

    pub(crate) fn embedded_argument<'s>(
        &mut self,
        source: &'s str,
        open: char,
        close: char,
    ) -> Option<(&'s str, &'s str)> {
        let source = source.trim_start();
        let after = source.strip_prefix(open)?;
        let opening = self.offset(source);
        let end = opening + source.len();
        let mode = if open == '{' {
            RawReaderMode::EmbeddedBrace
        } else {
            RawReaderMode::EmbeddedBracket
        };
        if self.failed.contains(&(opening, end, mode)) {
            return None;
        }
        let mut stack = vec![(opening, mode)];
        let mut escaped = false;
        let mut failed_end = after.len();
        for (i, c) in after.char_indices() {
            #[cfg(test)]
            {
                self.scanned_bytes += c.len_utf8();
            }
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c == close && stack.len() == 1 {
                return Some((&after[..i], &after[i + c.len_utf8()..]));
            }
            if c == '{' {
                let nested = opening + open.len_utf8() + i;
                stack.push((nested, RawReaderMode::EmbeddedBrace));
                // A proven unclosed brace prevents this enclosing option's
                // closing bracket from ever becoming eligible.
                if self
                    .failed
                    .contains(&(nested, end, RawReaderMode::EmbeddedBrace))
                {
                    failed_end = i;
                    break;
                }
            } else if c == '}' {
                if stack.len() == 1 {
                    failed_end = i;
                    break;
                }
                stack.pop();
            }
        }
        if open == '[' {
            // This reader ignores nested square brackets. A same-level '['
            // after the opener has the same failed suffix, but one inside a
            // brace group may find a closer that the outer probe ignored.
            let mut depth = 0usize;
            let mut escaped = false;
            for (i, c) in after[..failed_end].char_indices() {
                #[cfg(test)]
                {
                    self.scanned_bytes += c.len_utf8();
                }
                if escaped {
                    escaped = false;
                    continue;
                }
                match c {
                    '\\' => escaped = true,
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    '[' if depth == 0 => {
                        self.failed
                            .insert((opening + open.len_utf8() + i, end, mode));
                    }
                    _ => {}
                }
            }
        }
        self.failed.extend(
            stack
                .into_iter()
                .map(|(opening, mode)| (opening, end, mode)),
        );
        None
    }

    fn braced_verb_content_len(&mut self, content: &str, on_dtx_doc_line: bool) -> Option<usize> {
        let mode = if on_dtx_doc_line {
            RawReaderMode::DtxNameArgument
        } else {
            RawReaderMode::NameArgument
        };
        let opening = self.offset(content) - 1;
        let end = opening + 1 + content.len();
        if self.failed.contains(&(opening, end, mode)) {
            return None;
        }
        // Clean curated name arguments need no allocation. Only a failed scan
        // collects unmatched openings, with one additional linear pass.
        let mut depth = 1usize;
        let mut failed_end = content.len();
        let mut chars = content.char_indices();
        while let Some((i, c)) = chars.next() {
            #[cfg(test)]
            {
                self.scanned_bytes += c.len_utf8();
            }
            match c {
                '\\' => {
                    if chars.next().is_none() {
                        break;
                    }
                }
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (i > 0).then_some(i);
                    }
                }
                '\n' | '\r' if on_dtx_doc_line => {
                    let after = &content[i + 1..];
                    let after = if c == '\r' {
                        after.strip_prefix('\n').unwrap_or(after)
                    } else {
                        after
                    };
                    if !after.starts_with('%') || after.starts_with("%<") {
                        failed_end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let mut stack = vec![opening];
        let mut chars = content[..failed_end].char_indices();
        while let Some((i, c)) = chars.next() {
            #[cfg(test)]
            {
                self.scanned_bytes += c.len_utf8();
            }
            match c {
                '\\' => {
                    chars.next();
                }
                '{' => stack.push(opening + 1 + i),
                '}' => {
                    stack.pop();
                }
                _ => {}
            }
        }
        self.failed
            .extend(stack.into_iter().map(|opening| (opening, end, mode)));
        None
    }
}

fn lex_into(region: &str, out: &mut Vec<Token>) {
    let mut pos = 0;
    while pos < region.len() {
        let rest = &region[pos..];
        let (kind, len) = next_token(rest, control_word_len(rest, false, false), false);
        debug_assert!(len > 0, "lexer made no progress in verbatim args");
        out.push(Token {
            kind,
            text: SmolStr::new(&region[pos..pos + len]),
        });
        pos += len;
    }
}

fn push_env_delimiter(out: &mut Vec<Token>, control: &str, name: &str) {
    push_env_delimiter_with_gap(out, control, "", name);
}

fn push_env_delimiter_with_gap(out: &mut Vec<Token>, control: &str, gap: &str, name: &str) {
    out.push(Token {
        kind: SyntaxKind::CONTROL_WORD,
        text: SmolStr::new(control),
    });
    if !gap.is_empty() {
        out.push(Token {
            kind: SyntaxKind::WHITESPACE,
            text: SmolStr::new(gap),
        });
    }
    out.push(Token {
        kind: SyntaxKind::L_BRACE,
        text: SmolStr::new("{"),
    });
    out.push(Token {
        kind: SyntaxKind::WORD,
        text: SmolStr::new(name),
    });
    out.push(Token {
        kind: SyntaxKind::R_BRACE,
        text: SmolStr::new("}"),
    });
}

fn inline_ws_len(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

fn tex_whitespace_len(s: &str) -> usize {
    s.bytes()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .count()
}

fn dtx_doc_argument_gap_len(s: &str) -> Option<usize> {
    let inline = inline_ws_len(s);
    let rest = &s[inline..];
    let newline = if rest.starts_with("\r\n") {
        2
    } else if rest.starts_with(['\n', '\r']) {
        1
    } else {
        return None;
    };
    let after_newline = &rest[newline..];
    let after_margin = after_newline.strip_prefix('%')?;
    Some(inline + newline + 1 + inline_ws_len(after_margin))
}

fn lex_dtx_doc_argument_gap(gap: &str, out: &mut Vec<Token>) {
    let inline = inline_ws_len(gap);
    if inline > 0 {
        out.push(Token {
            kind: SyntaxKind::WHITESPACE,
            text: SmolStr::new(&gap[..inline]),
        });
    }
    let rest = &gap[inline..];
    let newline = if rest.starts_with("\r\n") { 2 } else { 1 };
    out.push(Token {
        kind: SyntaxKind::NEWLINE,
        text: SmolStr::new(&rest[..newline]),
    });
    out.push(Token {
        kind: SyntaxKind::DOC_MARGIN,
        text: SmolStr::new("%"),
    });
    let trailing = &rest[newline + 1..];
    if !trailing.is_empty() {
        out.push(Token {
            kind: SyntaxKind::WHITESPACE,
            text: SmolStr::new(trailing),
        });
    }
}

fn skip_inline_ws(s: &str) -> &str {
    &s[inline_ws_len(s)..]
}

fn run_len(s: &str, pred: impl Fn(char) -> bool) -> usize {
    let mut len = 0;
    for c in s.chars() {
        if pred(c) {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    len
}

fn is_letter(c: char, at_letter: bool, expl_syntax: bool) -> bool {
    c.is_ascii_alphabetic() || (at_letter && c == '@') || (expl_syntax && (c == '_' || c == ':'))
}

/// Returns whether `name` can be a control word in any supported mode.
pub fn is_control_word_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| is_letter(c, true, true))
}

/// Returns whether `c` belongs to an ordinary text token.
pub fn is_word_char(c: char) -> bool {
    !matches!(
        c,
        '\\' | '%'
            | '{'
            | '}'
            | '['
            | ']'
            | '$'
            | '&'
            | '#'
            | '^'
            | '_'
            | '~'
            | ' '
            | '\t'
            | '\n'
            | '\r'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_lossless(input: &str) {
        let joined: String = lex(input).iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, input);
    }

    #[test]
    fn malformed_raw_reader_scan_work_is_linear() {
        for newline in ["\n", "\r\n"] {
            for pattern in ["\\begin{", "\\url{"] {
                let source = format!("{pattern}{newline}").repeat(4096);
                let mut readers = RawReaders::new(&source);
                for (offset, _) in source.match_indices(pattern) {
                    let rest = &source[offset..];
                    if pattern == "\\begin{" {
                        for _ in 0..3 {
                            assert_eq!(readers.begin_name(rest), None);
                        }
                    } else {
                        assert_eq!(readers.balanced_group_len(&rest[4..], b'}'), None);
                    }
                }
                assert!(
                    readers.scanned_bytes <= source.len() * 2,
                    "{pattern:?}/{newline:?}: {} scanned bytes for {} input bytes",
                    readers.scanned_bytes,
                    source.len()
                );
            }
            for dtx in [false, true] {
                let pattern = if dtx {
                    "% \\begin{function}{"
                } else {
                    "\\begin{function}{"
                };
                let source = format!("{pattern}{newline}").repeat(4096);
                let mut readers = RawReaders::new(&source);
                for (offset, _) in source.match_indices(pattern) {
                    let content = &source[offset + pattern.len()..];
                    assert_eq!(readers.braced_verb_content_len(content, dtx), None);
                }
                assert!(
                    readers.scanned_bytes <= source.len() * 2,
                    "curated/dtx={dtx}/{newline:?}: {} scanned bytes for {} input bytes",
                    readers.scanned_bytes,
                    source.len()
                );
            }
        }
    }

    #[test]
    fn environment_length_gate_preserves_explicit_and_embedded_names() {
        for name in [
            "unusually-long-environment-".repeat(8),
            "literal{\n\\name".to_owned(),
        ] {
            let source = format!("\\begin{{{name}}}\\ref{{printed}}\\end{{{name}}}");
            let mut ctx = ParseCtx::default();
            ctx.insert_environment(SmolStr::new(&name), Vec::new());
            let tokens = lex_with(&source, &ctx, LexConfig::default());
            assert_eq!(
                tokens
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<String>(),
                source
            );
            assert!(
                tokens
                    .iter()
                    .any(|token| token.kind == SyntaxKind::VERBATIM_BODY
                        && token.text == "\\ref{printed}")
            );
        }
        let name = "embeddedalias".repeat(16);
        let source =
            format!("\\newminted[{name}]{{json}}{{}}\n\\begin{{{name}}}{{data}}\\end{{{name}}}");
        let tokens = lex_with(&source, &ParseCtx::default(), LexConfig::default());
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            source
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::VERBATIM_BODY && token.text == "{data}")
        );
    }

    #[test]
    fn documentary_comparison_comments_suppress_the_following_newline() {
        let source = "% \\ifx a^^A note\n% \\begin{verbatim}\n% \\ref{real}\n% \\fi\n";
        for source in [source.to_owned(), source.replace('\n', "\r\n")] {
            let tokens = lex_with(
                &source,
                &ParseCtx::default(),
                LexConfig {
                    dtx: true,
                    ..LexConfig::default()
                },
            );
            assert_eq!(
                tokens
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<String>(),
                source
            );
            assert!(
                !tokens
                    .iter()
                    .any(|token| token.kind == SyntaxKind::VERBATIM_BODY)
            );
            assert!(
                tokens
                    .iter()
                    .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\ref")
            );
        }
    }

    #[test]
    fn malformed_delimiter_probes_match_reference_and_scan_linearly() {
        for boundary in 0..3 {
            let source = "\\documentclass[\\begin{unfinished[ ".repeat(4096);
            let mut readers = RawReaders::new(&source);
            for (offset, _) in source.match_indices('[') {
                assert_eq!(readers.next_close(&source[offset + 1..], boundary), None);
            }
            assert!(readers.scanned_bytes <= source.len());
        }
        let source = "\\begin{function}[".repeat(4096);
        let mut readers = RawReaders::new(&source);
        let mut tokens = Vec::new();
        for (offset, _) in source.match_indices("\\begin{") {
            assert_eq!(
                lex_verbatim_arg_environment(
                    &source[offset..],
                    &ParseCtx::default(),
                    false,
                    &mut readers,
                    &mut tokens,
                ),
                None
            );
        }
        assert!(tokens.is_empty());
        assert!(readers.scanned_bytes <= source.len() * 2);
        let source = "é{a}b[c]\r\n{d}e]f";
        let mut readers = RawReaders::new(source);
        for (offset, _) in source.char_indices().rev().chain(source.char_indices()) {
            let suffix = &source[offset..];
            for boundary in 0..3 {
                let expected = suffix.bytes().position(|byte| match boundary {
                    0 => byte == b'}',
                    1 => byte == b']',
                    _ => matches!(byte, b']' | b'\r' | b'\n'),
                });
                assert_eq!(readers.next_close(suffix, boundary), expected);
            }
        }
    }

    #[test]
    fn documentary_comments_do_not_consume_pending_literal_operands() {
        for source in [
            "% \\def\n% ^^A \\fake{wrong}\n% \\foo{body}\n",
            "% \\string\n% ^^A \\fake\n% \\verb{printed}\n",
            "% \\detokenize{\n% ^^A \\fake{wrong}\n% data}\n",
        ] {
            for source in [source.to_owned(), source.replace('\n', "\r\n")] {
                let tokens = lex_with(
                    &source,
                    &ParseCtx::default(),
                    LexConfig {
                        dtx: true,
                        ..LexConfig::default()
                    },
                );
                assert_eq!(
                    tokens
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>(),
                    source
                );
                assert!(
                    tokens
                        .iter()
                        .any(|token| token.kind == SyntaxKind::COMMENT
                            && token.text.starts_with("^^A")),
                    "{source}"
                );
                assert!(
                    !tokens
                        .iter()
                        .any(|token| token.kind == SyntaxKind::CONTROL_WORD
                            && token.text == "\\fake"),
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn curated_documented_names_do_not_capture_docstrip_guards() {
        for source in [
            "% \\begin{function}{name\n%<package>\\def\\real{body}\n% }\n% \\end{function}\n",
            "% \\begin{function}\n%<package>{\\real}\n% \\end{function}\n",
        ] {
            for source in [source.to_owned(), source.replace('\n', "\r\n")] {
                let tokens = lex_with(
                    &source,
                    &ParseCtx::default(),
                    LexConfig {
                        dtx: true,
                        ..LexConfig::default()
                    },
                );
                assert_eq!(
                    tokens
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>(),
                    source
                );
                assert!(
                    tokens
                        .iter()
                        .any(|token| token.kind == SyntaxKind::GUARD && token.text == "%<package>")
                );
                assert!(!tokens.iter().any(|token| token.kind == SyntaxKind::VERB));
            }
        }
    }

    #[test]
    fn embedded_argument_failure_cache_preserves_bounded_slice_rules() {
        let samples = [
            " {a}tail",
            "\u{2003} [a{b}c]tail",
            "[a[b]tail",
            "[a}tail]",
            "{[a]b}tail",
            "{{balanced} unfinished",
            "{\\{escaped}tail",
            "{\\é{nested}unfinished",
            "[{a]still}]tail",
            "{a\\}b}tail",
            "{}tail",
            "{a\\",
            "[a\\]b]tail",
        ];
        for source in samples {
            let mut readers = RawReaders::new(source);
            for (offset, c) in source.char_indices() {
                if !matches!(c, '{' | '[') {
                    continue;
                }
                let close = if c == '{' { '}' } else { ']' };
                let suffix = &source[offset..];
                for (end, _) in suffix.char_indices().skip(1) {
                    let slice = &suffix[..end];
                    assert_eq!(
                        readers.embedded_argument(slice, c, close),
                        super::super::embedded::argument(slice, c, close),
                        "bounded {slice:?}"
                    );
                }
                assert_eq!(
                    readers.embedded_argument(suffix, c, close),
                    super::super::embedded::argument(suffix, c, close),
                    "full {suffix:?}"
                );
                assert_eq!(
                    readers.embedded_argument(suffix, c, close),
                    super::super::embedded::argument(suffix, c, close),
                    "cached {suffix:?}"
                );
            }
        }
        for pattern in ["\\lstset{\n", "\\mintinline[\n", "\\mintinline[{\n"] {
            let source = pattern.repeat(4096);
            let mut readers = RawReaders::new(&source);
            let open = if pattern.contains('[') { '[' } else { '{' };
            let close = if open == '[' { ']' } else { '}' };
            for (offset, _) in source.match_indices(pattern) {
                let argument = &source[offset + pattern.len() - 2..];
                assert_eq!(readers.embedded_argument(argument, open, close), None);
            }
            let passes = if pattern.contains("[{") { 3 } else { 2 };
            assert!(
                readers.scanned_bytes <= source.len() * passes,
                "{pattern:?}: {} scanned bytes for {} source bytes",
                readers.scanned_bytes,
                source.len()
            );
        }
    }

    #[test]
    fn failed_raw_reader_cache_matches_independent_suffix_scans() {
        fn balanced(s: &str, close: u8) -> Option<usize> {
            let mut stack = vec![close];
            let mut i = 1;
            while i < s.len() {
                match s.as_bytes()[i] {
                    b'\\' => {
                        i += 2;
                        continue;
                    }
                    b'{' => stack.push(b'}'),
                    b'[' => stack.push(b']'),
                    c @ (b'}' | b']') if stack.last() == Some(&c) => {
                        stack.pop();
                        if stack.is_empty() {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        fn name_argument(content: &str, dtx: bool) -> Option<usize> {
            let mut depth = 1;
            let mut chars = content.char_indices();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        chars.next()?;
                    }
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return (i > 0).then_some(i);
                        }
                    }
                    '\n' | '\r' if dtx => {
                        let after = &content[i + 1..];
                        let after = if c == '\r' {
                            after.strip_prefix('\n').unwrap_or(after)
                        } else {
                            after
                        };
                        if !after.starts_with('%') || after.starts_with("%<") {
                            return None;
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        let mut seed = 0x4d31_94ab_u64;
        let alphabet = [
            "{", "}", "[", "]", "\\", "%", "<", "é", "\n", "\r", "a", "\\begin{",
        ];
        for _ in 0..1024 {
            let mut source = String::from("\\begin{{");
            for _ in 0..40 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                source.push_str(alphabet[seed as usize % alphabet.len()]);
            }
            let offsets: Vec<_> = source.char_indices().map(|(i, _)| i).collect();
            let mut readers = RawReaders::new(&source);
            // Reverse queries also check that the next-name-close cache does
            // not reuse a result from a later opening.
            let reversed: Vec<_> = offsets.iter().copied().rev().collect();
            for offsets in [&offsets[..], &reversed[..]] {
                for &offset in offsets {
                    let suffix = &source[offset..];
                    let expected_name = suffix.strip_prefix("\\begin{").and_then(|after| {
                        after
                            .find('}')
                            .map(|close| (&after[..close], 7 + close + 1))
                    });
                    assert_eq!(readers.begin_name(suffix), expected_name);
                    let close = match suffix.as_bytes()[0] {
                        b'{' => b'}',
                        b'[' => b']',
                        _ => continue,
                    };
                    let open = if close == b'}' { '{' } else { '[' };
                    let embedded_close = close as char;
                    assert_eq!(
                        readers.embedded_argument(suffix, open, embedded_close),
                        super::super::embedded::argument(suffix, open, embedded_close)
                    );
                    assert_eq!(
                        readers.balanced_group_len(suffix, close),
                        balanced(suffix, close),
                        "{suffix:?}"
                    );
                    if close == b'}' {
                        for dtx in [true, false, true] {
                            assert_eq!(
                                readers.braced_verb_content_len(&suffix[1..], dtx),
                                name_argument(&suffix[1..], dtx),
                                "dtx={dtx}: {suffix:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn alltt_keeps_commands_and_groups_but_disables_other_specials() {
        let input = "\\begin{alltt}\n% $ # ^ _ & ~ \\textbf{active} \\ref{real}\n\\end{alltt}";
        let tokens = lex(input);
        assert_lossless(input);
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\ref")
        );
        assert!(tokens.iter().any(|token| token.kind == SyntaxKind::L_BRACE));
        assert!(!tokens.iter().any(|token| matches!(
            token.kind,
            SyntaxKind::COMMENT | SyntaxKind::DOLLAR | SyntaxKind::HASH | SyntaxKind::VERBATIM_BODY
        )));
    }

    #[test]
    fn alltt_closers_obey_control_sequences_groups_and_raw_captures() {
        let source = concat!(
            "\\begin{alltt}\n",
            "\\\\end{alltt} \\verb|\\end{alltt}|\n",
            "\\newcommand{\\closer}{\\end{alltt}}\n",
            "\\begin{alltt}$inner$\\end{alltt}\n",
            "$outer$ % literal \\ref{real}\n",
            "\\end{alltt}\n% real comment\n",
        );
        assert_lossless(source);
        let tokens = lex(source);
        let comments: Vec<_> = tokens
            .iter()
            .filter(|token| token.kind == SyntaxKind::COMMENT)
            .map(|token| token.text.as_str())
            .collect();
        assert_eq!(comments, ["% real comment"]);
        assert!(!tokens.iter().any(|token| token.kind == SyntaxKind::DOLLAR));
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\ref")
        );
    }

    #[test]
    fn unfinished_inline_literals_do_not_expose_commands_on_the_same_line() {
        for newline in ["\n", "\r\n", "\r", ""] {
            let input = format!("\\verb|\\ref{{fake}}%${newline}\\ref{{real}}");
            let tokens = lex(&input);
            assert_lossless(&input);
            assert_eq!(
                tokens
                    .iter()
                    .filter(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\ref")
                    .count(),
                usize::from(!newline.is_empty())
            );
        }
    }

    #[test]
    fn detokenize_does_not_run_lexer_commands_or_raw_captures() {
        let input = r"\detokenize{\verb|\ref{fake}| \makeatletter \begin{verbatim}} \foo@bar";
        let tokens = lex(input);
        assert_lossless(input);
        assert!(
            !tokens
                .iter()
                .any(|token| matches!(token.kind, SyntaxKind::VERB | SyntaxKind::VERBATIM_BODY))
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\foo")
        );
        assert!(
            !tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::CONTROL_WORD && token.text == "\\foo@bar")
        );
    }

    #[test]
    fn a_claimed_construct_spends_the_armed_char_constant_mode() {
        let direct = lex("\\char `\\%");
        assert!(
            direct
                .iter()
                .any(|t| t.kind == SyntaxKind::WORD && t.text == "`\\%")
        );
        let intervened = lex("\\MakeShortVerb{\\|} \\char |a| `\\%");
        assert!(
            intervened
                .iter()
                .any(|t| t.kind == SyntaxKind::VERB && t.text == "|a|")
        );
        assert!(
            !intervened
                .iter()
                .any(|t| t.kind == SyntaxKind::WORD && t.text == "`\\%")
        );
        assert_lossless("\\MakeShortVerb{\\|} \\char |a| `\\%");
    }

    #[test]
    fn block_environment_classification() {
        let ctx = ParseCtx::default();
        assert!(ctx.is_block_environment("figure"));
        assert!(ctx.is_block_environment("itemize")); // derived via `list`
        assert!(!ctx.is_block_environment("myenv")); // unknown
    }

    #[test]
    fn lossless_on_assorted_inputs() {
        for input in [
            "",
            "plain text",
            r"\section{Hi}[x]",
            "$a^2_b$",
            "a%c\n\nb",
            "café ∑ \\\\ \\{ \\,",
            "tab\tand  spaces",
            "trailing\\",
            r"\verb|$x$|",
            "\\begin{verbatim}\n$x$ %not a comment\n\\end{verbatim}",
            "\\begin{lstlisting}[language=C]\nint a[3];  % raw\n\\end{lstlisting}",
            "\\begin{minted}[frame=single]{python}\nprint(\"$x$\")\n\\end{minted}",
            "\\begin{lstlisting}\n[1,2,3]\n\\end{lstlisting}",
            r"\makeatletter\a@b\makeatother\a@b",
            r"\ExplSyntaxOn\seq_new:N \g_@@_x_tl a_b\ExplSyntaxOff\seq_new:N",
            r"$\left(x+y\right)^2 \left.\frac{a}{b}\right|_0$",
        ] {
            assert_lossless(input);
        }
    }

    #[test]
    fn control_word_stops_at_non_letter() {
        let toks = lex(r"\alpha2");
        assert_eq!(toks[0].kind, SyntaxKind::CONTROL_WORD);
        assert_eq!(toks[0].text, "\\alpha");
        assert_eq!(toks[1].kind, SyntaxKind::WORD);
        assert_eq!(toks[1].text, "2");
    }

    #[test]
    fn double_backslash_is_one_control_symbol() {
        let toks = lex(r"\\");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, SyntaxKind::CONTROL_SYMBOL);
        assert_eq!(toks[0].text, r"\\");
    }

    #[test]
    fn comment_stops_before_newline() {
        let toks = lex("% hi\nx");
        assert_eq!(toks[0].kind, SyntaxKind::COMMENT);
        assert_eq!(toks[0].text, "% hi");
        assert_eq!(toks[1].kind, SyntaxKind::NEWLINE);
    }

    #[test]
    fn crlf_is_a_single_newline() {
        let toks = lex("a\r\nb");
        assert_eq!(toks[1].kind, SyntaxKind::NEWLINE);
        assert_eq!(toks[1].text, "\r\n");
    }

    #[test]
    fn control_symbol_swallows_the_whole_line_ending() {
        for ending in ["\n", "\r", "\r\n"] {
            let input = format!("\\{ending}");
            let toks = lex(&input);
            assert_eq!(toks.len(), 1, "split line ending {ending:?}");
            assert_eq!(toks[0].kind, SyntaxKind::CONTROL_SYMBOL);
            assert_eq!(toks[0].text, input);
        }
    }

    #[test]
    fn verb_inline_is_one_token() {
        let toks = lex(r"\verb|$x$|");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, SyntaxKind::VERB);
        assert_eq!(toks[0].text, r"\verb|$x$|");
    }

    #[test]
    fn literal_verb_operands_do_not_consume_following_source() {
        for source in [
            "\\def\\verb{%\n body\n}\n",
            "\\let\\other\\verb{body}\n",
            "\\ifx\\verb\\other{body}\\fi\n",
            "\\meaning\\verb{body}\n",
            "\\noexpand\\verb{body}\n",
            "\\detokenize{\\verb{body}}\n",
        ] {
            let tokens = lex(source);
            assert_lossless(source);
            assert!(
                tokens.iter().any(|token| {
                    matches!(token.kind, SyntaxKind::CONTROL_WORD | SyntaxKind::WORD)
                        && token.text == "\\verb"
                }),
                "{source}"
            );
            assert!(
                !tokens.iter().any(|token| token.kind == SyntaxKind::VERB),
                "{source}"
            );
            let parsed = super::super::core::parse_with_flavor(source, LatexFlavor::Document);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            assert_eq!(parsed.syntax().to_string(), source);
        }
    }

    #[test]
    fn stringified_verb_consumes_only_the_control_sequence() {
        let source = "\\string\\verb|body| \\verb|active|";
        let tokens = lex(source);
        assert_lossless(source);
        let literals: Vec<_> = tokens
            .iter()
            .filter(|token| token.kind == SyntaxKind::VERB)
            .map(|token| token.text.as_str())
            .collect();
        assert_eq!(literals, ["\\verb|active|"]);
        assert!(tokens.iter().any(|token| token.text == "|body|"));
    }

    #[test]
    fn stored_string_operands_cannot_consume_replacement_delimiters() {
        for source in [
            "\\def\\foo{Invalid use of \\string}\\ref{live}",
            "\\long\\gdef\\foo#1{\\string{#1}}\\ref{live}",
            "\\edef\\foo{\\string\\begin}\\ref{live}",
            "\\DoNotIndex{\\relax,\\space,\\string}\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
        // Executed primitive operands have no scanned replacement owner.
        for source in [
            "\\string}",
            "{\\string}",
            "\\let\\foo\\def\\relax{\\string}",
        ] {
            let tokens = lex(source);
            assert!(
                tokens.iter().any(|token| {
                    token.kind == SyntaxKind::STRINGIFIED_TOKEN && token.text == "}"
                }),
                "{source}"
            );
            assert_lossless(source);
        }
    }

    #[test]
    fn stored_declaration_headers_keep_structural_string_delimiters() {
        for source in [
            "\\newcommand{\\foo}{\\string}\\ref{live}",
            "\\newcommand\\foo[1][\\string]{body}\\ref{live}",
            "\\DeclareRobustCommand*{\\foo}[1][\\string]{\\string}\\ref{live}",
            "\\NewDocumentCommand{\\foo}{O{\\string}m}{\\string}\\ref{live}",
            "\\DeclareExpandableDocumentCommand\\foo{m}{\\string}\\ref{live}",
            "\\newenvironment{foo}[1][\\string]{\\string}{\\string}\\ref{live}",
            "\\NewDocumentEnvironment{foo}{O{\\string}m}{\\string}{\\string}\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }

    #[test]
    fn stored_declaration_owners_respect_source_shadowing_and_inert_heads() {
        for source in [
            "\\let\\newcommand\\relax\\newcommand{\\foo}{\\string}",
            "\\let\\def\\relax\\def\\foo{\\string}",
            "\\meaning\\newcommand{\\foo}{\\string}",
            "\\string\\newcommand{\\foo}{\\string}",
            "{\\string}",
            "\\string}",
        ] {
            let parsed = super::super::core::parse(source);
            let tokens: Vec<_> = parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .collect();
            assert!(
                tokens
                    .iter()
                    .any(|token| token.kind() == SyntaxKind::STRINGIFIED_TOKEN
                        && token.text() == "}"),
                "{source}: {tokens:?}"
            );
        }
        let source = "\\def\\newcommand{\\string}\\newcommand{\\foo}{\\string}";
        let parsed = super::super::core::parse(source);
        let tokens: Vec<_> = parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .collect();
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind() == SyntaxKind::STRINGIFIED_TOKEN && token.text() == "}")
                .count(),
            1,
            "the active overriding definition owns its body; only its later shadowed call is executable"
        );
    }

    #[test]
    fn curated_macro_arguments_keep_string_braces_structural() {
        for source in [
            "\\texttt{\\string}\\ref{live}",
            "\\section*{\\string}\\ref{live}",
            "\\operatorname*{\\string}\\ref{live}",
            "\\DeclareMathOperator*{\\foo}{\\string}\\ref{live}",
            "\\includegraphics*{\\string}\\ref{live}",
            "\\textbf{\\string{}}\\ref{live}",
            "\\frac{\\string}{x}\\ref{live}",
            "\\section[\\string]{\\string}\\ref{live}",
            "\\href{https://example.test}{\\string}\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            assert_eq!(parsed.syntax().to_string(), source);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
        for source in [
            "\\hbox{\\string}",
            "\\mathop{\\string}",
            "\\meaning\\texttt{\\string}",
            "\\let\\texttt\\relax\\texttt{\\string}",
            "\\texttt*{\\string}",
        ] {
            let parsed = super::super::core::parse(source);
            assert!(
                parsed
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::STRINGIFIED_TOKEN
                        && token.text() == "}"),
                "{source}"
            );
        }
    }

    #[test]
    fn fancy_readers_use_rebased_source_override_offsets() {
        let source = "\\let\\meaning\\relax\n\\begin{Verbatim}[commandchars=+()]\n+meaning+verb|+ref(eq:fake)|\n\\end{Verbatim}\n";
        let parsed = super::super::core::parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::VERB
                    && token.text() == "+verb|+ref(eq:fake)|"),
            "the source-shadowed meaning cannot make the active verb an inert operand"
        );
        let model = crate::semantic::SemanticModel::build(&parsed.syntax());
        assert!(model.refs().is_empty());
    }

    #[test]
    fn stored_owner_groups_survive_nested_headers_and_raw_arguments() {
        for source in [
            "\\newenvironment{outer}{\\newcommand{\\inner}{\\string}}{\\string}\\ref{live}",
            "\\newcommand{\\foo}[1][{\\string}]{\\string}\\ref{live}",
            "\\def\\foo#1\\texttt{\\string}\\ref{live}",
            "\\href[unicode]{https://example.test}{\\textbf{\\string}}\\ref{live}",
            "\\newcommand*{\\foo}{\\string}\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }

    #[test]
    fn primitive_parameter_control_words_do_not_steal_or_extend_body_ownership() {
        let source = "\\def\\foo\\texttt{\\string} {\\string}";
        let parsed = super::super::core::parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert_eq!(parsed.syntax().descendants_with_tokens().filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::STRINGIFIED_TOKEN && token.text() == "}").count(), 1);
    }
    #[test]
    fn primitive_parameter_string_cannot_consume_the_first_body_brace() {
        for source in [
            "\\def\\foo\\string{X}\\ref{live}",
            "\\def\\foo\\string\\texttt{X}\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1);
            assert_eq!(model.refs()[0].name, "live");
        }
    }

    #[test]
    fn supported_literal_declaration_names_do_not_run_raw_readers() {
        for source in [
            "\\newcommand*{\\verb}{x}\\ref{live}",
            "\\DeclareMathOperator{\\verb}{x}\\ref{live}",
            "\\DeclareMathOperator*{\\verb}{x}\\ref{live}",
            "\\newlength{\\verb}\\ref{live}",
            "\\newcount\\verb\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
            assert!(
                crate::semantic::scan_definition_sites(&parsed.syntax())
                    .iter()
                    .any(|site| site.name == "verb"),
                "{source}"
            );
        }
    }

    #[test]
    fn literal_name_ownership_ends_and_respects_shadowed_or_inert_heads() {
        for source in [
            "\\newlength{\\length}\\verb|active|\\ref{live}",
            "\\let\\newlength\\relax\\newlength{\\verb|active|}\\ref{live}",
            "\\let\\DeclareMathOperator\\relax\\DeclareMathOperator{\\verb|active|}\\ref{live}",
            "\\meaning\\DeclareMathOperator{\\verb|active|}\\ref{live}",
            "\\string\\newlength{\\verb|active|}\\ref{live}",
            "\\let\\copy\\newlength{\\verb|active|}\\ref{live}",
            "\\newlength*{\\verb|active|}\\ref{live}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            assert!(
                parsed
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::VERB
                        && token.text() == "\\verb|active|"),
                "{source}"
            );
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }
    #[test]
    fn embedded_islands_use_exact_source_override_offsets() {
        let source = "\\let\\meaning\\relax\n\\begin{lstlisting}[escapeinside={(*@}{@*)}]\n(*@\\meaning\\verb|\\ref{fake}|@*)\n\\end{lstlisting}\n";
        let parsed = super::super::core::parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(
            parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::VERB
                    && token.text() == "\\verb|\\ref{fake}|")
        );
        assert!(
            crate::semantic::SemanticModel::build(&parsed.syntax())
                .refs()
                .is_empty()
        );

        let source = "\\begin{Verbatim}[commandchars=+()]\n+meaning+verb|payload|\n\\end{Verbatim}\n\\let\\meaning\\relax";
        let parsed = super::super::core::parse(source);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(
            !parsed
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::VERB)
        );
    }
    #[test]
    fn documentary_index_arguments_are_inert_and_source_shadowable() {
        for source in [
            "\\DoNotIndex{\\string,\\ref{fake},\\def\\ghost{x}}\\ref{live}",
            "\\renewcommand{\\DoNotIndex}[1]{#1}\\DoNotIndex{\\ref{live}}",
            "\\let\\DoNotIndex\\relax\\DoNotIndex{\\ref{live}}",
            "\\meaning\\DoNotIndex{\\ref{live}}",
        ] {
            let parsed = super::super::core::parse(source);
            assert_eq!(parsed.syntax().to_string(), source);
            assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
            let model = crate::semantic::SemanticModel::build(&parsed.syntax());
            assert_eq!(model.refs().len(), 1, "{source}");
            assert_eq!(model.refs()[0].name, "live");
        }
    }

    #[test]
    fn ordinary_source_redefinition_suppresses_builtin_verb_capture() {
        let source = "\\renewcommand{\\verb}[1]{\\texttt{#1}}\n\\verb{a_b}\n";
        let parsed = super::super::core::parse_with_flavor(source, LatexFlavor::Document);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let root = parsed.syntax();
        assert_eq!(root.to_string(), source);
        assert!(!root.descendants_with_tokens().any(|element| {
            element
                .into_token()
                .is_some_and(|token| token.kind() == SyntaxKind::VERB)
        }));
        assert!(root.descendants_with_tokens().any(|element| {
            element
                .into_token()
                .is_some_and(|token| token.kind() == SyntaxKind::UNDERSCORE)
        }));
    }

    #[test]
    fn ordinary_release_header_replacement_does_not_hide_argument_calls() {
        let source = "\\renewcommand{\\IncludeInRelease}[3]{#3}\n\\IncludeInRelease{date}{name}{\\ref{live}}\n";
        let parsed = super::super::core::parse_with_flavor(source, LatexFlavor::Document);
        assert_eq!(parsed.syntax().to_string(), source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let model = crate::semantic::SemanticModel::build(&parsed.syntax());
        assert_eq!(model.refs().len(), 1);
    }

    #[test]
    fn verb_star_with_plus_delimiter() {
        let toks = lex(r"a\verb*+b+c");
        assert_eq!(toks[1].kind, SyntaxKind::VERB);
        assert_eq!(toks[1].text, r"\verb*+b+");
        assert_eq!(toks[2].text, "c");
    }

    #[test]
    fn verb_without_closing_delimiter_preserves_an_incomplete_literal() {
        let toks = lex(r"\verb|x");
        assert_eq!(toks[0].kind, SyntaxKind::VERB);
        assert_eq!(toks[0].text, r"\verb|x");
    }

    #[test]
    fn left_right_isolate_word_delimiter() {
        let toks = lex(r"\left(x+y\right)");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert_eq!(
            seen,
            [
                (SyntaxKind::CONTROL_WORD, "\\left"),
                (SyntaxKind::WORD, "("),
                (SyntaxKind::WORD, "x+y"),
                (SyntaxKind::CONTROL_WORD, "\\right"),
                (SyntaxKind::WORD, ")"),
            ]
        );
    }

    #[test]
    fn left_delimiter_carries_across_whitespace() {
        let toks = lex(r"\left ( a");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert_eq!(
            seen,
            [
                (SyntaxKind::CONTROL_WORD, "\\left"),
                (SyntaxKind::WHITESPACE, " "),
                (SyntaxKind::WORD, "("),
                (SyntaxKind::WHITESPACE, " "),
                (SyntaxKind::WORD, "a"),
            ]
        );
    }

    #[test]
    fn left_non_word_delimiters_are_untouched() {
        for input in [r"\left\{", r"\left\langle", r"\left["] {
            assert_lossless(input);
        }
        let toks = lex(r"\left\langle x \right\rangle");
        assert!(toks.iter().any(|t| t.text == "\\langle"));
        assert!(toks.iter().any(|t| t.text == "\\rangle"));
    }

    #[test]
    fn leftarrow_is_not_left() {
        let toks = lex(r"\leftarrow(x)");
        assert_eq!(toks[0].text, "\\leftarrow");
        assert_eq!(toks[1].text, "(x)");
    }

    #[test]
    fn makeatletter_makes_at_a_letter() {
        let toks = lex(r"\makeatletter\foo@bar\makeatother\foo@bar");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo@bar")));
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo")));
    }

    #[test]
    fn expl_syntax_makes_underscore_and_colon_letters() {
        let toks = lex(r"\ExplSyntaxOn\seq_new:N\ExplSyntaxOff\seq_new:N");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq_new:N")));
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq")));
    }

    #[test]
    fn expl_syntax_lexes_internal_double_underscore_name() {
        let toks = lex(r"\ExplSyntaxOn\__module_internal:nn");
        assert_eq!(toks[1].kind, SyntaxKind::CONTROL_WORD);
        assert_eq!(toks[1].text, "\\__module_internal:nn");
    }

    #[test]
    fn provides_expl_package_turns_on_expl_syntax() {
        let toks = lex(r"\ProvidesExplPackage{p}{2026/01/01}{1.0}{d}\tl_set:Nn");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\tl_set:Nn")));
    }

    #[test]
    fn expl_syntax_composes_with_makeatletter() {
        let toks = lex(r"\makeatletter\ExplSyntaxOn\g_@@_frame_title_tl");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\g_@@_frame_title_tl")));
    }

    #[test]
    fn expl_syntax_makes_bare_underscore_a_word_not_subscript() {
        let toks = lex(r"\ExplSyntaxOn a_b");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::WORD, "a_b")));
        assert!(!seen.iter().any(|(k, _)| *k == SyntaxKind::UNDERSCORE));
    }

    fn lex_dtx(input: &str) -> Vec<Token> {
        lex_with(
            input,
            &ParseCtx::default(),
            LexConfig {
                flavor: LatexFlavor::Document,
                dtx: true,
            },
        )
    }

    #[test]
    fn implicit_expl_module_guard_makes_macrocode_body_expl3() {
        let toks = lex_dtx(
            "%<@@=mod>\n\
             %    \\begin{macrocode}\n\
             \\seq_new:N\n\
             %    \\end{macrocode}\n",
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq_new:N")));
    }

    #[test]
    fn no_expl_signal_leaves_macrocode_body_plain() {
        let toks = lex_dtx(
            "%    \\begin{macrocode}\n\
             \\seq_new:N\n\
             %    \\end{macrocode}\n",
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq")));
        assert!(!seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq_new:N")));
    }

    #[test]
    fn implicit_expl_provides_expl_flags_every_body_regardless_of_order() {
        let toks = lex_dtx(
            "%    \\begin{macrocode}\n\
             \\seq_new:N\n\
             %    \\end{macrocode}\n\
             % \\ProvidesExplPackage{p}{2026/01/01}{1.0}{d}\n\
             %    \\begin{macrocode}\n\
             \\tl_set:Nn\n\
             %    \\end{macrocode}\n",
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq_new:N")));
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\tl_set:Nn")));
    }

    #[test]
    fn implicit_expl_is_body_only_doc_layer_stays_plain() {
        let toks = lex_dtx(
            "%<@@=mod>\n\
             % a_b\n\
             %    \\begin{macrocode}\n\
             c_d\n\
             %    \\end{macrocode}\n",
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::WORD, "c_d")));
        assert!(seen.iter().any(|(k, _)| *k == SyntaxKind::UNDERSCORE));
    }

    #[test]
    fn implicit_expl_explicit_off_wins_then_next_body_re_enters() {
        let toks = lex_dtx(
            "%<@@=mod>\n\
             %    \\begin{macrocode}\n\
             \\seq_new:N\n\
             \\ExplSyntaxOff\n\
             a_b\n\
             %    \\end{macrocode}\n\
             %    \\begin{macrocode}\n\
             \\tl_set:Nn\n\
             %    \\end{macrocode}\n",
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq_new:N")));
        assert!(seen.iter().any(|(k, _)| *k == SyntaxKind::UNDERSCORE));
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\tl_set:Nn")));
    }

    #[test]
    fn implicit_expl_gated_off_outside_dtx() {
        let toks = lex_with(
            "%<@@=mod>\n\\seq_new:N",
            &ParseCtx::default(),
            LexConfig {
                flavor: LatexFlavor::Package,
                dtx: false,
            },
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq")));
        assert!(!seen.contains(&(SyntaxKind::CONTROL_WORD, "\\seq_new:N")));
    }

    #[test]
    fn package_flavor_starts_in_letter_mode() {
        let toks = lex_with(
            r"\foo@bar",
            &ParseCtx::default(),
            LatexFlavor::Package.into(),
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert_eq!(seen, vec![(SyntaxKind::CONTROL_WORD, "\\foo@bar")]);
    }

    #[test]
    fn package_flavor_respects_trailing_makeatother() {
        let toks = lex_with(
            r"\foo@bar\makeatother\foo@bar",
            &ParseCtx::default(),
            LatexFlavor::Package.into(),
        );
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo@bar")));
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo")));
    }

    #[test]
    fn document_flavor_keeps_at_non_letter() {
        let toks = lex(r"\foo@bar");
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo")));
        assert!(!seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo@bar")));
    }

    #[test]
    fn dtx_mode_lexes_line_leading_percent_as_a_margin() {
        let dtx = LexConfig {
            flavor: LatexFlavor::Document,
            dtx: true,
        };
        let toks = lex_with("% \\foo\nbar % tail\n", &ParseCtx::default(), dtx);
        let seen: Vec<_> = toks.iter().map(|t| (t.kind, t.text.as_str())).collect();
        assert_eq!(seen[0], (SyntaxKind::DOC_MARGIN, "%"));
        assert!(seen.contains(&(SyntaxKind::CONTROL_WORD, "\\foo")));
        assert!(seen.contains(&(SyntaxKind::COMMENT, "% tail")));
        assert_eq!(
            seen.iter()
                .filter(|(k, _)| *k == SyntaxKind::DOC_MARGIN)
                .count(),
            1
        );
    }

    #[test]
    fn dtx_mode_is_off_by_default_for_margins_and_guards() {
        let plain = lex("% \\foo\n");
        assert_eq!(plain[0].kind, SyntaxKind::COMMENT);
        let plain_guard = lex("%<*driver>\n");
        assert_eq!(plain_guard[0].kind, SyntaxKind::COMMENT);
        assert_eq!(plain_guard[0].text, "%<*driver>");
    }

    #[test]
    fn dtx_mode_lexes_line_leading_guards() {
        let dtx = LexConfig {
            flavor: LatexFlavor::Document,
            dtx: true,
        };
        let block = lex_with("%<*driver>\n%</driver>\n", &ParseCtx::default(), dtx);
        assert_eq!(block[0].kind, SyntaxKind::GUARD);
        assert_eq!(block[0].text, "%<*driver>");
        assert!(
            block
                .iter()
                .any(|t| t.kind == SyntaxKind::GUARD && t.text == "%</driver>")
        );
        let inline = lex_with("%<plain>\\RequirePackage{x}\n", &ParseCtx::default(), dtx);
        assert_eq!(inline[0].kind, SyntaxKind::GUARD);
        assert_eq!(inline[0].text, "%<plain>");
        assert!(
            inline
                .iter()
                .any(|t| t.kind == SyntaxKind::CONTROL_WORD && t.text == "\\RequirePackage")
        );
        let expr = lex_with("%<*package|driver>\n", &ParseCtx::default(), dtx);
        assert_eq!(expr[0].kind, SyntaxKind::GUARD);
        assert_eq!(expr[0].text, "%<*package|driver>");
        let midline = lex_with("a %<x>\n", &ParseCtx::default(), dtx);
        assert!(
            midline
                .iter()
                .any(|t| t.kind == SyntaxKind::COMMENT && t.text == "%<x>")
        );
        assert!(!midline.iter().any(|t| t.kind == SyntaxKind::GUARD));
        let malformed = lex_with("%<unterminated\n", &ParseCtx::default(), dtx);
        assert_eq!(malformed[0].kind, SyntaxKind::COMMENT);
        assert_eq!(malformed[0].text, "%<unterminated");
    }

    #[test]
    fn verbatim_environment_body_is_one_raw_token() {
        let toks = lex("\\begin{verbatim}\n$not$ %literal\n\\end{verbatim}");
        assert_eq!(toks[0].text, "\\begin");
        assert_eq!(toks[2].text, "verbatim");
        assert!(
            toks.iter()
                .any(|t| t.kind == SyntaxKind::VERBATIM_BODY && t.text.contains("$not$ %literal"))
        );
        assert!(!toks.iter().any(|t| t.kind == SyntaxKind::DOLLAR));
        assert!(!toks.iter().any(|t| t.kind == SyntaxKind::COMMENT));
    }

    #[test]
    fn argument_taking_verbatim_separates_args_from_body() {
        let toks = lex("\\begin{minted}[frame=single]{python}\nprint(\"$x$\")\n\\end{minted}");
        let kinds: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&SyntaxKind::L_BRACKET));
        assert!(kinds.contains(&SyntaxKind::R_BRACKET));
        assert!(kinds.contains(&SyntaxKind::L_BRACE));
        assert!(
            toks.iter()
                .any(|t| t.kind == SyntaxKind::FOREIGN_TEXT && t.text.contains("$x$"))
        );
        assert!(!toks.iter().any(|t| t.kind == SyntaxKind::DOLLAR));
    }

    #[test]
    fn verbatim_body_starting_with_bracket_is_not_an_argument() {
        let toks = lex("\\begin{lstlisting}\n[1,2,3]\n\\end{lstlisting}");
        assert!(
            !toks
                .iter()
                .take_while(|t| t.kind != SyntaxKind::VERBATIM_BODY)
                .any(|t| t.kind == SyntaxKind::L_BRACKET),
            "the bracket on the body's first line must not be lexed as an argument"
        );
        assert!(
            toks.iter()
                .any(|t| t.kind == SyntaxKind::VERBATIM_BODY && t.text.contains("[1,2,3]"))
        );
    }

    #[test]
    fn make_short_verb_toggles_pipe_capture() {
        let toks = lex("|a| \\MakeShortVerb{\\|} |$| \\DeleteShortVerb{\\|} |b|");
        let verbs: Vec<_> = toks
            .iter()
            .filter(|t| t.kind == SyntaxKind::VERB)
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(verbs, ["|$|"]);
        assert_lossless("|a| \\MakeShortVerb{\\|} |$| \\DeleteShortVerb{\\|} |b|");
    }

    #[test]
    fn documentclass_ltxguide_enables_the_pipe_short_verb() {
        for preamble in [
            "\\documentclass{ltxguide}",
            "\\documentclass[a4paper]{ltxdoc}",
            "\\documentclass{ltxguide}[1994/11/20]",
            "\\documentclass{l3doc}",
            "\\documentclass[leqno,titlepage]{amsldoc}[1999/12/13]",
        ] {
            let input = format!("{preamble}\n|}}| done");
            let toks = lex(&input);
            assert!(
                toks.iter()
                    .any(|t| t.kind == SyntaxKind::VERB && t.text == "|}|"),
                "no VERB captured after {preamble}"
            );
        }
        let toks = lex("\\documentclass{article}\n|x| done");
        assert!(!toks.iter().any(|t| t.kind == SyntaxKind::VERB));
    }

    #[test]
    fn short_verb_never_captures_a_left_right_delimiter() {
        let toks = lex("\\MakeShortVerb{\\|} $\\left|x\\right|$");
        assert!(!toks.iter().any(|t| t.kind == SyntaxKind::VERB));
        assert_lossless("\\MakeShortVerb{\\|} $\\left|x\\right|$");
    }

    #[test]
    fn short_verb_scope_snapshots_share_large_unicode_delimiter_sets() {
        let mut input = String::new();
        let mut declarations = Vec::new();
        for value in 0xe000..0xf000 {
            let character = char::from_u32(value).unwrap();
            let start = input.len();
            input.push_str(&format!("\\MakeShortVerb{{\\{character}}}\n"));
            declarations.push((start, start + "\\MakeShortVerb".len(), character));
        }
        let ctx = ParseCtx::default();
        let mut lexer = Lexer::new(&input, &ctx, LexConfig::default(), None);
        for &(start, after, _) in &declarations {
            lexer.pos = start;
            lexer.apply_toggles("\\MakeShortVerb", &input[after..]);
        }
        assert_eq!(lexer.short_verbs.len(), 4096);
        lexer.save_scope();
        assert!(
            std::ptr::eq(
                lexer.short_verbs.iter().next().unwrap(),
                lexer
                    .scopes
                    .last()
                    .unwrap()
                    .short_verbs
                    .iter()
                    .next()
                    .unwrap(),
            ),
            "ordinary scopes must share immutable delimiter state rather than copy it"
        );
        let (start, after, character) = declarations[0];
        lexer.pos = start;
        lexer.apply_toggles("\\MakeShortVerb", &input[after..]);
        lexer.apply_toggles("\\DeleteShortVerb", "{\\z}");
        assert!(std::ptr::eq(
            lexer.short_verbs.iter().next().unwrap(),
            lexer
                .scopes
                .last()
                .unwrap()
                .short_verbs
                .iter()
                .next()
                .unwrap(),
        ));
        lexer.apply_toggles("\\DeleteShortVerb", &input[after..]);
        assert!(!lexer.short_verbs.contains(&character));
        assert!(!lexer.short_verb_contains(&character));
        assert!(
            lexer
                .scopes
                .last()
                .unwrap()
                .short_verbs
                .contains(&character)
        );
        lexer.restore_scope();
        assert_eq!(lexer.short_verbs.len(), 4096);
        assert!(lexer.short_verbs.contains(&character));
        assert!(lexer.short_verb_contains(&character));
    }

    #[test]
    fn short_verb_nested_add_remove_restores_outer_delimiters() {
        let input = concat!(
            "\\MakeShortVerb{\\|}|outer|",
            "{\\MakeShortVerb{\\!}!inner!|still|",
            "{\\DeleteShortVerb{\\|}|plain|!nested!}",
            "|restored|!inner-again!}",
            "|after|!plain-again!",
        );
        let tokens = lex(input);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            input
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind == SyntaxKind::VERB)
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>(),
            [
                "|outer|",
                "!inner!",
                "|still|",
                "!nested!",
                "|restored|",
                "!inner-again!",
                "|after|"
            ]
        );
    }

    #[test]
    fn unclosed_short_verb_char_stands_alone() {
        let toks = lex("\\MakeShortVerb{\\|} a|b\nc");
        assert!(!toks.iter().any(|token| token.kind == SyntaxKind::VERB));
        assert!(
            toks.iter()
                .any(|token| token.kind == SyntaxKind::WORD && token.text == "|")
        );
        assert_lossless("\\MakeShortVerb{\\|} a|b\nc");
    }

    #[test]
    fn raw_capture_content_does_not_change_later_lexing() {
        const ENV_BODIES: &[&str] = &[
            "",
            "plain text",
            "\\makeatletter",
            "\\ExplSyntaxOn",
            "\\MakeShortVerb{\\|}",
            "{{{",
            "}}}",
            "% not a comment",
            "$ & # ^ _ ~",
            "\\end{verbatimx}",
            "\\begin{verbatim}",
            "\\char`{",
            "\\left(",
        ];
        const INLINE_BODIES: &[&str] = &[
            "",
            "x",
            "\\makeatletter",
            "\\ExplSyntaxOn",
            "{}",
            "$ & # ^ _ ~",
            "% not a comment",
            "\\char`",
        ];
        const SUFFIX: &str = "after \\my@cmd \\l_tmpa_tl |bar| \\char`{ \\left( x\n";

        for (prefix, open, close, bodies) in [
            (
                "before x\n",
                "\\begin{verbatim}\n",
                "\n\\end{verbatim}\n",
                ENV_BODIES,
            ),
            (
                "before x\n",
                "\\begin{lstlisting}[a=b]\n",
                "\n\\end{lstlisting}\n",
                ENV_BODIES,
            ),
            ("before x ", "\\verb+", "+ ", INLINE_BODIES),
            ("before x ", "\\url{", "} ", INLINE_BODIES),
            ("before x ", "\\href{", "}{visible} ", INLINE_BODIES),
            ("before x ", "\\lstinline+", "+ ", INLINE_BODIES),
        ] {
            let mut expected: Option<Vec<(SyntaxKind, String)>> = None;
            for body in bodies {
                let region = format!("{open}{body}{close}");
                let doc = format!("{prefix}{region}{SUFFIX}");
                assert_lossless(&doc);

                let toks = lex(&doc);
                assert!(
                    toks.iter()
                        .any(|t| matches!(t.kind, SyntaxKind::VERB | SyntaxKind::VERBATIM_BODY))
                        || body.is_empty(),
                    "no raw capture formed, so this case proves nothing\n  \
                     region: {region:?}",
                );

                let from = prefix.len() + region.len();
                let mut off = 0usize;
                let got: Vec<(SyntaxKind, String)> = toks
                    .into_iter()
                    .filter(|t| {
                        let start = off;
                        off += t.text.len();
                        start >= from
                    })
                    .map(|t| (t.kind, t.text.to_string()))
                    .collect();

                match &expected {
                    None => expected = Some(got),
                    Some(want) => assert_eq!(
                        &got, want,
                        "a raw body changed how the text after it lexes\n  \
                         region: {region:?}",
                    ),
                }
            }
        }
    }

    #[test]
    fn a_body_that_breaks_its_capture_changes_later_lexing() {
        let captured = lex("\\url{x} \\char`{");
        assert!(captured.iter().any(|t| t.kind == SyntaxKind::VERB));
        assert!(
            captured
                .iter()
                .any(|t| t.kind == SyntaxKind::WORD && t.text == "`{")
        );

        let broken = lex("\\url{{} \\char`{");
        assert!(!broken.iter().any(|t| t.kind == SyntaxKind::VERB));
        assert!(
            broken
                .iter()
                .any(|t| t.kind == SyntaxKind::WORD && t.text == "`")
        );
    }
}
