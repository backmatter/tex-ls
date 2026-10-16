//! `unused-string`: an `@string{ name = … }` macro never referenced by any field
//! or preamble value in the file.
//!
//! Built on [`Model::unused_string_defs`](crate::bib::semantic::Model::unused_string_defs).
//! A [`Severity::Warning`]; report-only (deleting the definition is a tex-ls-level
//! edit we leave to the author).
//!
//! Rooted projects include uses in all loaded bibliography resources. Incomplete
//! rooted projects suppress absence claims; no automatic deletion is offered.

use std::path::PathBuf;

use crate::linter::diagnostic::{Diagnostic, Severity};

use super::{BibRule, BibRuleContext, Example};

const EXAMPLES: &[Example] = &[Example {
    caption: "A defined macro no field or preamble value references:",
    source: "@string{cup = {Cambridge University Press}}\n\
             @book{turing50, title = {Draft}, publisher = {Springer}}\n",
}];

pub struct UnusedString;

impl BibRule for UnusedString {
    fn id(&self) -> &'static str {
        "unused-string"
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &'static str {
        "Report an `@string` macro that no field or preamble in its loaded bibliography resources references. Incomplete rooted projects suppress this warning; standalone files check local uses. No automatic fix is offered."
    }

    fn examples(&self) -> &'static [Example] {
        EXAMPLES
    }

    fn check_file(&self, ctx: &BibRuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for def in ctx.model.unused_string_defs() {
            if ctx.project.is_some_and(|project| {
                project.scoped
                    && (!project.complete || project.used_strings.contains(def.name.as_str()))
            }) {
                continue;
            }
            sink.push(Diagnostic {
                rule: self.id(),
                severity: self.default_severity(),
                path: PathBuf::new(),
                start: usize::from(def.range.start()),
                end: usize::from(def.range.end()),
                message: format!("`@string` macro `{}` is defined but never used", def.name),
                fix: None,
                related: Vec::new(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(src: &str) -> Vec<Diagnostic> {
        crate::bib::linter::test_support::with_context(src, |_root, ctx| {
            let mut out = Vec::new();
            UnusedString.check_file(ctx, &mut out);
            out
        })
    }

    #[test]
    fn flags_unused_macro() {
        let out = findings("@string{cup = {C}}\n@book{k, publisher = {Other}}\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, "unused-string");
        assert!(out[0].message.contains("cup"));
    }

    #[test]
    fn used_macro_is_fine() {
        assert!(findings("@string{cup = {C}}\n@book{k, publisher = cup}\n").is_empty());
    }

    #[test]
    fn underlines_the_definition_name() {
        let out = findings("@string{cup = {C}}\n");
        assert_eq!(out.len(), 1);
        let start = "@string{".len();
        let end = start + "cup".len();
        assert_eq!((out[0].start, out[0].end), (start, end));
    }

    #[test]
    fn flags_each_unused_macro() {
        let out = findings("@string{a = {A}}\n@string{b = {B}}\n");
        assert_eq!(out.len(), 2);
    }
}
