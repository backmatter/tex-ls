//! Source-qualified readers with a bounded, curated definition shape.

use crate::semantic::define::{DefSite, DefSiteKind};
use crate::syntax::{SyntaxKind, SyntaxNode, is_trivia};

use super::lexer::{LatexFlavor, ParseCtx, lex_with};

const VERBATIM_COMMENT: &str = r"\def\comment{\@bsphack
\let\do\@makeother\dospecials\catcode`\^^M\active
\let\verbatim@startline\relax
\let\verbatim@addtoline\@gobble
\let\verbatim@processline\relax
\let\verbatim@finish\relax
\verbatim@}";

fn significant(kind: SyntaxKind) -> bool {
    !is_trivia(kind) && !matches!(kind, SyntaxKind::DOC_MARGIN | SyntaxKind::GUARD)
}

/// Parameterized definition sites end at the name. Match a fixed token budget
/// from that proved owner through the curated parameter text and closing brace.
fn definition_end(root: &SyntaxNode, site: &DefSite, expected: &str) -> Option<usize> {
    let expected = lex_with(expected, &ParseCtx::default(), LatexFlavor::Package.into());
    let mut cursor = crate::syntax::token_at_offset(root, site.range.start()).right_biased();
    let mut end = 0;
    let mut parameters_started = false;
    let mut replacement_started = false;
    // Parameter spaces and blank-line paragraph tokens change argument scanning.
    // A control-word definee ignores its blanks and first endline. Comments
    // consume their endline and the next line's indentation instead of adding one.
    enum ParameterTrivia {
        Meaningful,
        CommentLine,
        Indent,
    }
    let mut parameter_trivia = ParameterTrivia::Meaningful;
    for expected in expected.into_iter().filter(|token| significant(token.kind)) {
        let actual = loop {
            let token = cursor.take()?;
            cursor = token.next_token();
            if significant(token.kind()) {
                break token;
            }
            if !replacement_started {
                match token.kind() {
                    SyntaxKind::COMMENT => parameter_trivia = ParameterTrivia::CommentLine,
                    SyntaxKind::NEWLINE
                        if matches!(parameter_trivia, ParameterTrivia::CommentLine)
                            || !parameters_started
                                && matches!(parameter_trivia, ParameterTrivia::Meaningful) =>
                    {
                        parameter_trivia = ParameterTrivia::Indent;
                    }
                    SyntaxKind::WHITESPACE
                        if !parameters_started
                            || matches!(parameter_trivia, ParameterTrivia::Indent) => {}
                    SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => return None,
                    _ => {}
                }
            }
        };
        if actual.kind() != expected.kind || actual.text() != expected.text.as_str() {
            return None;
        }
        if !replacement_started {
            parameters_started |= expected.kind == SyntaxKind::HASH;
            replacement_started = expected.kind == SyntaxKind::L_BRACE;
        }
        parameter_trivia = ParameterTrivia::Meaningful;
        end = usize::from(actual.text_range().end());
    }
    Some(end)
}

/// Multiple bindings decline the profile, including later aliases and malformed
/// definitions. A same-name ordinary replacement must keep its ordinary reader.
pub(super) fn verbatim_comment_start(root: &SyntaxNode, sites: &[DefSite]) -> Option<usize> {
    let mut candidates = sites
        .iter()
        .filter(|site| site.kind == DefSiteKind::Command && site.name == "comment");
    let candidate = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    definition_end(root, candidate, VERBATIM_COMMENT)
}

const RELEASE_DEFINITIONS: &[(&str, &str)] = &[
    (
        "IncludeInRelease",
        r"\def\IncludeInRelease#1{%
 \if@includeinrelease
   \PackageError{latexrelease}{mis-matched IncludeInRelease}%
                {There is an \string\EndIncludeRelease\space missing}%
  \@includeinreleasefalse
  \fi
  \ifnum0%
      \ifx\new@moduledate\@empty\else 1\fi
      \ifnum \expandafter\@parse@version#1//00\@nil=0 1\fi
      =11
    \expandafter\@firstoftwo
  \else
    \expandafter\@secondoftwo
  \fi
    {\finish@module@release{#1}}%
    {\kernel@ifnextchar[%
      {\@IncludeInRelease{#1}}
      {\@IncludeInRelease{#1}[#1]}}}",
    ),
    (
        "finish@module@release",
        r"\def\finish@module@release#1#2#3{%
  \toks@{[#1] #3}%
  \begingroup
    \edef\x{\detokenize\expandafter{\new@modulename}}%
    \edef\y{\detokenize{#2}}%
  \expandafter\endgroup
  \ifx\x\y \else
    \@latex@error{\noexpand\IncludeInRelease dated #1 in a module is not
      allowed.\MessageBreak Use a date at least equal to \new@moduledate
      \space for complete rollback}\@ehd
  \fi
  \ifnum\expandafter\@parse@version\new@moduledate//00\@nil
       >\expandafter\@parse@version\fmtversion//00\@nil
    \GenericInfo{}{Applying: \the\toks@}%
  \else
    \GenericInfo{}{Skipping: \the\toks@}%
    \expandafter\gobble@finish@module@release
  \fi}",
    ),
    (
        "@IncludeInRelease",
        r"\def\@IncludeInRelease#1[#2]{\@IncludeInRele@se{#2}}",
    ),
    (
        "@IncludeInRele@se",
        r"\def\@IncludeInRele@se#1#2#3{%
  \toks@{[#1] #3}%
  \expandafter\ifx\csname\string#2+\@currname+IIR\endcsname\relax
    \ifnum\expandafter\@parse@version#1//00\@nil
         >\expandafter\@parse@version\fmtversion//00\@nil
      \GenericInfo{}{Skipping: \the\toks@}%
      \expandafter\expandafter\expandafter\@gobble@IncludeInRelease
    \else
      \GenericInfo{}{Applying: \the\toks@}%
      \@includeinreleasetrue
      \expandafter\let\csname\string#2+\@currname+IIR\endcsname\@empty
    \fi
  \else
    \GenericInfo{}{Already applied: \the\toks@}%
    \expandafter\@gobble@IncludeInRelease
  \fi
}",
    ),
];

pub(super) fn release_header_start(
    input: &str,
    dtx: bool,
    root: &SyntaxNode,
    sites: &[DefSite],
    operands: &crate::semantic::context::PrimitiveOperandContexts,
) -> Option<usize> {
    if !input.contains("\\IncludeInRelease") {
        return None;
    }
    if !sites.iter().any(|site| {
        RELEASE_DEFINITIONS
            .iter()
            .any(|(name, _)| site.name == *name)
    }) {
        // The lexer cannot apply this profile in docstrip mode. Keep the old
        // qualified-profile path below unchanged, but avoid a new unused context.
        if dtx {
            return None;
        }
        // Standalone kernel files use the format's closed reader without defining
        // it locally. A source binding to any helper instead requires the full
        // profile below; copied names and stored token lists cannot activate it.
        return root
            .descendants()
            .filter(|node| {
                node.kind() == SyntaxKind::COMMAND
                    && node
                        .first_token()
                        .is_some_and(|token| token.text() == "\\IncludeInRelease")
            })
            .any(|command| {
                !operands.contains_head(&command)
                    && command
                        .first_token()
                        .is_some_and(|token| !crate::semantic::context::in_token_list(&token))
            })
            .then_some(0);
    }
    let mut start = 0;
    for (name, expected) in RELEASE_DEFINITIONS {
        let mut candidates = sites
            .iter()
            .filter(|site| site.kind == DefSiteKind::Command && site.name == *name);
        let candidate = candidates.next()?;
        let end = if *name == "@IncludeInRelease" {
            // The package's rollback option forwards the mandatory date instead
            // of the optional date. Both exact variants retain the same stored
            // name/description roles. Reject every other binding.
            let alternate = r"\def\@IncludeInRelease#1[#2]{\@IncludeInRele@se{#1}}";
            let matches = |site| {
                definition_end(root, site, expected)
                    .or_else(|| definition_end(root, site, alternate))
            };
            let end = matches(candidate)?;
            if let Some(second) = candidates.next() {
                matches(second)?;
                if candidates.next().is_some() {
                    return None;
                }
            }
            end
        } else {
            if candidates.next().is_some() {
                return None;
            }
            definition_end(root, candidate, expected)?
        };
        start = start.max(end);
    }
    Some(start)
}
