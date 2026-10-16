//! `inert-suppression`: a suppression directive that cannot take effect, or an
//! `off` region that reaches EOF without its intended closer.
//!
//! Placement and region matching come from the shared parser-side resolver. The
//! rule never re-parses comments or repeats the CST attachment walk. It is
//! report-only: repairing a dangling `skip`, unmatched `on`, unclosed `off`, or
//! directive-shaped `.dtx` prose requires knowing which construct or boundary
//! the author intended.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::suppression_meta::{self, Carrier};

use super::{Example, Rule, RuleContext};

const EXAMPLES: &[Example] = &[Example {
    caption: "An `on` directive with no matching open region does nothing:",
    source: "% tex-ls-lint on deprecated-command\n{\\bf text}\n",
}];

pub struct InertSuppression;

impl Rule for InertSuppression {
    fn id(&self) -> &'static str {
        "inert-suppression"
    }

    fn description(&self) -> &'static str {
        "Report suppression directives that cannot act: `skip` without a following construct, \
         `on` without `off`, or directives in `.dtx` documentation margins. Also report an \
         unclosed `off`, which suppresses through the end of the file. No fix is offered because \
         the intended boundary is unknown. Suppression comments cannot hide this finding; use \
         `[lint].ignore` to disable it."
    }

    fn examples(&self) -> &'static [Example] {
        EXAMPLES
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        sink.extend(ctx.suppressions.directives().iter().filter_map(|located| {
            suppression_meta::inert(located, Carrier::Latex, self.default_severity())
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linter::rules::RuleContext;
    use crate::parser::{LatexFlavor, LexConfig, parse_with_flavor};
    use crate::semantic::SemanticModel;

    fn findings_with(src: &str, config: LexConfig) -> Vec<Diagnostic> {
        let root = parse_with_flavor(src, config).syntax();
        assert_eq!(root.to_string(), src);
        let model = SemanticModel::build(&root);
        let ctx = RuleContext::new(
            std::path::Path::new("x.tex"),
            &root,
            &model,
            None,
            None,
            None,
        );
        let mut out = Vec::new();
        InertSuppression.check_file(&ctx, &mut out);
        out
    }

    fn findings(src: &str) -> Vec<Diagnostic> {
        findings_with(src, LexConfig::default())
    }

    #[test]
    fn reports_each_inert_or_incomplete_shape() {
        for (src, message) in [
            (
                "% tex-ls-lint skip deprecated-command\n",
                "no following construct",
            ),
            ("% tex-ls-lint on deprecated-command\n", "no matching `off`"),
            (
                "% tex-ls-lint off deprecated-command\n\\bf\n",
                "without a matching `on`",
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
    fn reports_a_directive_on_a_dtx_documentation_line() {
        let src = "% tex-ls-lint skip deprecated-command\nDocumentation.\n";
        let out = findings_with(
            src,
            LexConfig {
                flavor: LatexFlavor::Document,
                dtx: true,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            &src[out[0].start..out[0].end],
            "% tex-ls-lint skip deprecated-command"
        );
        assert!(out[0].message.contains("documentation line"));
    }

    #[test]
    fn accepts_honored_directives() {
        for src in [
            "% tex-ls-lint skip deprecated-command\n\\bf\n",
            "% tex-ls-lint off deprecated-command\n\\bf\n% tex-ls-lint on deprecated-command\n",
            "% tex-ls-lint skip-file deprecated-command\n",
        ] {
            assert!(findings(src).is_empty(), "{src:?}");
        }
    }
}
