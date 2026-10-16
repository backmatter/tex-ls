//! Findings derived from already resolved suppression directives.
use super::diagnostic::{Diagnostic, Fix, Severity};
use crate::directives::{DirectiveOutcome, LocatedDirective, Verb};

#[derive(Clone, Copy)]
pub(crate) enum Carrier {
    Latex,
    Bibtex,
}

pub(crate) fn deprecated(located: &LocatedDirective, carrier: Carrier) -> Option<Diagnostic> {
    // LaTeX documentation margins do not carry active comments. BibTeX keeps
    // reporting retired spellings even if their directive's role is unsupported.
    if !located.directive.deprecated
        || matches!(carrier, Carrier::Latex) && located.outcome == DirectiveOutcome::Unsupported
    {
        return None;
    }
    let replacement = match located.directive.verb {
        Verb::Skip => "tex-ls-lint skip",
        Verb::SkipFile => "tex-ls-lint skip-file",
        Verb::Off | Verb::On => return None,
    };
    let start = usize::from(located.family_range.start());
    let end = usize::from(located.family_range.end());
    let message = match carrier {
        Carrier::Latex => format!("retired suppression syntax; use `% {replacement}` instead"),
        Carrier::Bibtex => {
            format!("retired suppression syntax; use `@comment{{{replacement} …}}` instead")
        }
    };
    Some(Diagnostic {
        rule: "deprecated-suppression-syntax",
        severity: Severity::Warning,
        path: Default::default(),
        start,
        end,
        message,
        fix: Some(Fix::safe(
            start,
            end,
            replacement,
            "Rewrite the retired suppression directive",
        )),
        related: Vec::new(),
    })
}

pub(crate) fn inert(
    located: &LocatedDirective,
    carrier: Carrier,
    severity: Severity,
) -> Option<Diagnostic> {
    let message = match located.outcome {
        DirectiveOutcome::Honored => return None,
        DirectiveOutcome::DanglingSkip => match carrier {
            Carrier::Latex => "`skip` has no following construct, so this directive does nothing",
            Carrier::Bibtex => "`skip` has no following entry, so this directive does nothing",
        },
        DirectiveOutcome::UnmatchedOn => {
            "`on` has no matching `off`, so this directive closes no region"
        }
        DirectiveOutcome::UnclosedOff => {
            "`off` reaches the end of the file without a matching `on`"
        }
        DirectiveOutcome::Unsupported => match carrier {
            Carrier::Latex => {
                "this directive is on a `.dtx` documentation line, where `%` is typeset prose rather than a comment"
            }
            Carrier::Bibtex => {
                "the BibTeX formatter does not support format suppression, so this directive does nothing"
            }
        },
    };
    Some(Diagnostic {
        rule: "inert-suppression",
        severity,
        path: Default::default(),
        start: usize::from(located.range.start()),
        end: usize::from(located.range.end()),
        message: message.to_owned(),
        fix: None,
        related: Vec::new(),
    })
}
