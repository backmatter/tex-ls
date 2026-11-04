//! Findings derived from already resolved suppression directives.
use super::diagnostic::{Diagnostic, Severity};
use crate::directives::{DirectiveOutcome, LocatedDirective};

#[derive(Clone, Copy)]
pub(crate) enum Carrier {
    Latex,
    Bibtex,
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
