//! `SyntaxKind` — the kinds of CST tokens and nodes — and the rowan `Language`
//! binding for tex-ls's LaTeX surface CST.

/// Kinds of tokens (terminals, from the lexer) and nodes (composites, from the
/// parser) in the CST.
///
/// Token kinds come first, node kinds after; `ROOT` is kept **last** so
/// [`rowan::Language::kind_from_raw`] can bounds-check the raw discriminant with
/// a single comparison. Keep the hidden end marker immediately after `ROOT`.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // --- Tokens (terminals, produced by the lexer) ---
    CONTROL_WORD,      // `\foo`  (backslash + ASCII letters)
    CONTROL_SYMBOL,    // `\\`, `\{`, `\%`, `\,` … (backslash + one non-letter)
    L_BRACE,           // {
    R_BRACE,           // }
    L_BRACKET,         // [
    R_BRACKET,         // ]
    DOLLAR,            // $
    AMPERSAND,         // &
    HASH,              // #
    CARET,             // ^
    UNDERSCORE,        // _
    TILDE,             // ~
    COMMENT,           // `% …` up to (not including) the line break
    WHITESPACE,        // spaces / tabs
    NEWLINE,           // `\n`, `\r\n`, or `\r`
    WORD,              // a run of ordinary text characters
    STRINGIFIED_TOKEN, // one token printed by the primitive \string
    VERB,              // `\verb|…|` / `\verb*|…|` inline verbatim (a single token)
    VERBATIM_BODY,     // the raw body of a verbatim-like environment (a single token)
    EXCLUDED_BODY,     // an explicitly excluded, line-terminated comment body
    FOREIGN_TEXT,      // neutral tokens in a supported embedded language
    FOREIGN_KEYWORD,
    FOREIGN_FUNCTION,
    FOREIGN_COMMENT,
    DELEGATED_CODE, // explicit supported language, rendered by an opt-in editor grammar
    ESCAPE_OPEN,    // source delimiter introducing an active TeX island
    ESCAPE_CLOSE,
    CODE_OPEN, // delimiter of a known inline code command
    CODE_CLOSE,
    UNCERTAIN,  // source whose lexical ownership is not statically known
    DOC_MARGIN, // a `.dtx` documentation line's leading `%` margin (trivia)
    GUARD,      // a `.dtx` docstrip guard `%<…>` (`%<*t>`/`%</t>`/inline) (trivia)
    ERROR,      // lexer fallback; the lexer is total, so this is unused today

    // Nodes
    GROUP,         // { … }
    TOKEN_LIST,    // inert body of a source-proved primitive \detokenize group
    OPTIONAL,      // [ … ] optional argument
    ARGUMENT,      // an argument attached to a command
    BARE_ARGUMENT, // a literal unbraced input filename
    COMMAND,       // a control sequence with its arguments
    ENVIRONMENT,   // \begin{…} … \end{…}
    BEGIN,         // \begin{name}
    END,           // \end{name}
    NAME_GROUP,    // {name} following \begin / \end
    // `\if…\else…\or…\fi`, when the shape gate pairs it. The construct is a run
    // of `CONDITIONAL_BRANCH` nodes followed by the closing `\fi` as the last
    // child, mirroring `ENVIRONMENT > BEGIN … END`. The `\if` test's extent is
    // not statically resolvable (`\ifnum\radius>5` scans ⟨number⟩⟨rel⟩⟨number⟩
    // by TeX's own scanner), so the opener and its test ride the *first* branch
    // rather than a head node of their own.
    CONDITIONAL,
    // One branch of a `CONDITIONAL`. The first holds the opener, its test, and
    // the then-body; every later one *starts with* its `\else`/`\or` divider, so
    // a consumer finds the boundaries positionally and never by command name.
    CONDITIONAL_BRANCH,
    INLINE_MATH,  // $ … $   or   \( … \)
    DISPLAY_MATH, // $$ … $$  or   \[ … \]
    MATH,         // a math body (the atoms between the delimiters)
    SCRIPTED,     // a base atom with attached scripts: base (SUBSCRIPT | SUPERSCRIPT)+
    SUBSCRIPT,    // `_` and its tightly-bound script argument
    SUPERSCRIPT,  // `^` and its tightly-bound script argument
    LEFT_RIGHT,   // `\left( … \right)` — a matched delimiter pair wrapping a MATH body
    PARAGRAPH,    // text delimited by blank lines
    DOC_COMMENT,  // a bound leading-`%` comment run, grouped before its construct
    TEXT,         // a run of text and trivia
    LINE_BREAK,   // `\\`, with a tightly-bound `*` and/or `[len]` (`\\*[2ex]`)
    // A `;`-terminated statement in a curated `statementBody` environment body
    // (the TikZ/pgf picture family). Owns everything from the statement's first
    // non-trivia element through the top-level WORD carrying the terminating
    // `;`, so layout can derive statement boundaries from structure rather than
    // authored newlines. A run that never reaches a `;` is left as plain
    // paragraph content — recognition is retrospective and degrades silently.
    STATEMENT,
    EMBEDDED_TEX, // active TeX bounded by explicit listing escape delimiters
    INLINE_CODE,  // protected inline code, with an explicit source language
    ROOT,         // the document root  (keep LAST)
    #[doc(hidden)]
    __LAST,
}

impl SyntaxKind {
    /// The number of `SyntaxKind` variants. Sound because the enum is
    /// `#[repr(u16)]` with contiguous discriminants `0..=ROOT` and `ROOT` is kept
    /// last; used to size kind-indexed tables (e.g. the linter's dispatch table).
    pub const COUNT: usize = SyntaxKind::__LAST as usize;
}

/// The rowan language marker for tex-ls's CST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TexLsLang {}

impl_rowan_lang!(TexLsLang, SyntaxKind, "LaTeX");

pub type SyntaxNode = rowan::SyntaxNode<TexLsLang>;
pub type SyntaxToken = rowan::SyntaxToken<TexLsLang>;
pub type SyntaxElement = rowan::SyntaxElement<TexLsLang>;

/// Find adjacent tokens by descending through indexed child ranges. This has
/// the same boundary preference as rowan's lookup, without scanning every
/// sibling at each level. Empty elements do not own any source byte.
pub fn token_at_offset<L: rowan::Language>(
    root: &rowan::SyntaxNode<L>,
    at: rowan::TextSize,
) -> rowan::TokenAtOffset<rowan::SyntaxToken<L>> {
    use rowan::{TextRange, TextSize, TokenAtOffset};
    let range = root.text_range();
    assert!(range.contains_inclusive(at), "offset outside syntax node");
    let token = |start, end| {
        root.covering_element(TextRange::new(start, end))
            .into_token()
    };
    let left = (at > range.start())
        .then(|| token(at - TextSize::new(1), at))
        .flatten();
    let right = (at < range.end())
        .then(|| token(at, at + TextSize::new(1)))
        .flatten();
    match (left, right) {
        (Some(left), Some(right)) if left != right => TokenAtOffset::Between(left, right),
        (Some(token), _) | (_, Some(token)) => TokenAtOffset::Single(token),
        _ => TokenAtOffset::None,
    }
}

/// Whitespace and newlines are the only trivia the formatter rewrites; comments
/// are preserved verbatim and so are *not* collapsible. A shared shape
/// predicate: the formatter's gap normalization and the semantic layer's expl3
/// statement segmentation both skip exactly this class.
pub fn is_collapsible_trivia(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE)
}

/// Trivia ignored by syntax readers: collapsible whitespace plus comments.
pub fn is_trivia(kind: SyntaxKind) -> bool {
    is_collapsible_trivia(kind) || kind == SyntaxKind::COMMENT
}

/// Whether a `WORD` token is a single TeX parameter digit (`1`..=`9`) — the
/// shape that follows `#` in a parameter reference. Reads only the token text.
pub fn is_param_digit(t: &SyntaxToken) -> bool {
    matches!(t.text().as_bytes(), [b'1'..=b'9'])
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    fn check<L: rowan::Language>(root: rowan::SyntaxNode<L>) {
        for node in root.descendants() {
            let range = node.text_range();
            for at in u32::from(range.start())..=u32::from(range.end()) {
                let at = rowan::TextSize::from(at);
                let tokens = |found| match found {
                    rowan::TokenAtOffset::None => Vec::new(),
                    rowan::TokenAtOffset::Single(token) => vec![token],
                    rowan::TokenAtOffset::Between(left, right) => vec![left, right],
                };
                assert_eq!(
                    tokens(token_at_offset(&node, at)),
                    tokens(node.token_at_offset(at))
                );
            }
        }
    }

    #[test]
    fn indexed_lookup_matches_rowan_at_every_byte_and_subtree_boundary() {
        for source in [
            "",
            "é😀 ",
            "\\textbf{é😀} % comment\r\nword",
            "$x^{2}$\\begin{align}a&b\\\\c\\end{align}",
            "{\\unknown[é]{😀}",
            "\\begin{verbatim}\n{é😀}%\n\\end{verbatim}",
        ] {
            check(crate::parser::parse(source).syntax());
        }
        for source in [
            "",
            "@book{k,title={é😀}}",
            "@string{x=\"é😀\"}\r\n@book{k,title=x}",
            "@book{k,title={é😀",
            "@comment{a{b}}",
        ] {
            check(crate::bib::parse(source).syntax());
        }
    }
}
