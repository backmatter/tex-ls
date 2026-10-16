//! Literal bibliography dependencies need definitions in a closed root scope.
use super::{BibRule, BibRuleContext, Example};
use crate::linter::diagnostic::{Diagnostic, Severity};

pub struct UndefinedEntry;
impl BibRule for UndefinedEntry {
    fn id(&self) -> &'static str {
        "undefined-entry"
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn description(&self) -> &'static str {
        "Report literal crossref keys missing from every complete document root loading this bibliography. Proven Biber scopes also check xref, xdata, related and entryset keys. String expressions and incomplete or unrooted scopes are excluded. No automatic fix is offered."
    }
    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A complete document loads this resource, without a parent entry:",
            source: "@misc{child,title={Draft},crossref={missing}}\n",
        }]
    }
    fn check_file(&self, ctx: &BibRuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(project) = ctx.project.filter(|project| project.complete) else {
            return;
        };
        for (start, (key, end)) in &project.undefined_dependencies {
            sink.push(Diagnostic { rule: self.id(), severity: self.default_severity(), path: ctx.path.into(), start: *start, end: *end,
                message: format!("bibliography entry `{key}` is referenced but not defined in its complete document roots"), fix: None, related: Vec::new() });
        }
    }
}
