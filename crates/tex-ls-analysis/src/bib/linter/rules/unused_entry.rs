//! Opt-in hints require complete compilation-root coverage of a bibliography.
use super::{BibRule, BibRuleContext};
use crate::linter::diagnostic::{Diagnostic, Severity};

pub struct UnusedEntry;
impl BibRule for UnusedEntry {
    fn id(&self) -> &'static str {
        "unused-entry"
    }
    fn default_enabled(&self) -> bool {
        false
    }
    fn default_severity(&self) -> Severity {
        Severity::Hint
    }
    fn description(&self) -> &'static str {
        "Hint when a bibliography entry is unused in every complete, rooted document that loads this file. Disabled by default for shared libraries. Wildcard citations count as use; incomplete or unrooted namespaces produce no hints."
    }
    fn examples(&self) -> &'static [super::Example] {
        &[super::Example {
            caption: "A complete document loads this resource but cites no entries:",
            source: "@misc{unused, title = {Draft}}\n",
        }]
    }
    fn check_file(&self, ctx: &BibRuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(project) = ctx
            .project
            .filter(|project| project.complete && !project.wildcard)
        else {
            return;
        };
        for entry in ctx
            .model
            .entries()
            .iter()
            .filter(|entry| !project.used.contains(&entry.key.to_lowercase()))
        {
            sink.push(Diagnostic {
                rule: self.id(),
                severity: self.default_severity(),
                path: ctx.path.into(),
                start: entry.key_range.start().into(),
                end: entry.key_range.end().into(),
                message: format!(
                    "bibliography entry `{}` is unused in its complete document roots",
                    entry.key
                ),
                fix: None,
                related: Vec::new(),
            });
        }
    }
}
