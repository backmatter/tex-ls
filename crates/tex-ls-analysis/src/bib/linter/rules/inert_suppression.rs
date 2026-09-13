//! `inert-suppression`: a structured BibTeX suppression directive that cannot
//! take effect, or an `off` region that reaches EOF without its closer.
//!
//! The BibTeX suppression map retains the placement outcome while resolving
//! ranges. This whole-file rule reports that fact without re-parsing the
//! `@comment` body. It offers no fix because the intended entry, region boundary,
//! or formatting policy must come from the author.

use std::path::PathBuf;

use crate::directives::DirectiveOutcome;
use crate::linter::diagnostic::Diagnostic;

use super::{BibRule, BibRuleContext, Example};

const EXAMPLES: &[Example] = &[Example {
    caption: "BibTeX recognizes the directive grammar, but its formatter has no suppression mechanism:",
    source: "@comment{tex-ls-format skip-file: preserve this file}\n@book{key}\n",
}];

pub struct InertSuppression;

impl BibRule for InertSuppression {
    fn id(&self) -> &'static str {
        "inert-suppression"
    }

    fn description(&self) -> &'static str {
        "Report `skip` without a following entry, `on` without `off`, and format-suppression \
         directives, which BibTeX formatting does not support. Also report an unclosed `off`, \
         which suppresses through the end of the file. No fix is offered because the intended \
         boundary is unknown. Suppression comments cannot hide this finding; use `[lint].ignore` \
         to disable it."
    }

    fn examples(&self) -> &'static [Example] {
        EXAMPLES
    }

    fn check_file(&self, ctx: &BibRuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for located in ctx
            .suppressions
            .directives()
            .iter()
            .filter(|located| located.outcome != DirectiveOutcome::Honored)
        {
            let message = match located.outcome {
                DirectiveOutcome::Honored => continue,
                DirectiveOutcome::DanglingSkip => {
                    "`skip` has no following entry, so this directive does nothing"
                }
                DirectiveOutcome::UnmatchedOn => {
                    "`on` has no matching `off`, so this directive closes no region"
                }
                DirectiveOutcome::UnclosedOff => {
                    "`off` reaches the end of the file without a matching `on`"
                }
                DirectiveOutcome::Unsupported => {
                    "the BibTeX formatter does not support format suppression, so this \
                     directive does nothing"
                }
            };
            sink.push(Diagnostic {
                rule: self.id(),
                severity: self.default_severity(),
                path: PathBuf::new(),
                start: usize::from(located.range.start()),
                end: usize::from(located.range.end()),
                message: message.to_owned(),
                fix: None,
                related: Vec::new(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bib::linter::rules::BibRuleContext;
    use crate::bib::linter::suppression::BibSuppressionMap;
    use crate::bib::parse;
    use crate::bib::semantic::Model;
    use crate::linter::diagnostic::Diagnostic;

    fn findings(src: &str) -> Vec<Diagnostic> {
        let root = parse(src).syntax();
        assert_eq!(root.to_string(), src);
        let model = Model::build(&root);
        let suppressions = BibSuppressionMap::build(&root);
        let ctx = BibRuleContext {
            project: None,
            path: std::path::Path::new("x.bib"),
            root: &root,
            model: &model,
            db: crate::bib::semantic::builtin(),
            suppressions: &suppressions,
        };
        let mut out = Vec::new();
        InertSuppression.check_file(&ctx, &mut out);
        out
    }

    #[test]
    fn reports_each_inert_or_incomplete_shape() {
        for (src, message) in [
            (
                "@comment{tex-ls-lint skip missing-required-field}\n",
                "no following entry",
            ),
            (
                "@comment{tex-ls-lint on missing-required-field}\n",
                "no matching `off`",
            ),
            (
                "@comment{tex-ls-lint off missing-required-field}\n@book{a}\n",
                "without a matching `on`",
            ),
            (
                "@comment{tex-ls-format skip-file}\n@book{a}\n",
                "does not support format suppression",
            ),
        ] {
            let out = findings(src);
            assert_eq!(out.len(), 1, "{src:?}: {out:?}");
            assert_eq!(out[0].rule, "inert-suppression");
            assert_eq!(&src[out[0].start..out[0].end], src.lines().next().unwrap());
            assert!(out[0].message.contains(message), "{:?}", out[0].message);
            assert!(out[0].fix.is_none());
        }
    }

    #[test]
    fn accepts_honored_lint_and_combined_directives() {
        for src in [
            "@comment{tex-ls-lint skip missing-required-field}\n@book{a}\n",
            "@comment{tex-ls-lint off missing-required-field}\n@book{a}\n@comment{tex-ls-lint on missing-required-field}\n",
            "@comment{tex-ls-lint skip-file missing-required-field}\n",
            "@comment{tex-ls skip}\n@book{a}\n",
        ] {
            assert!(findings(src).is_empty(), "{src:?}");
        }
    }
}
