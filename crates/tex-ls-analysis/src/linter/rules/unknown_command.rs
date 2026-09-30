//! Unresolved document commands, with optional spelling suggestions.
use super::{Example, Rule, RuleContext};
use crate::ast::{command_name, control_word_range};
use crate::linter::{Diagnostic, Severity};
use crate::project::{PackageEdgeKey, PackageKind, PackageTarget};
use crate::semantic::signature;
use crate::syntax::{SyntaxKind, SyntaxNode};
use std::collections::HashSet;

pub struct UnknownCommand;

impl Rule for UnknownCommand {
    fn id(&self) -> &'static str {
        "unknown-command"
    }
    fn description(&self) -> &'static str {
        "Report document commands absent from core or loaded-package metadata and scanned \
         definitions. This is a static check, not proof that TeX will reject the command. \
         Package source files, definition bodies, and carried code are skipped. Commands \
         created dynamically by packages may require a `tex-ls skip unknown-command` \
         comment. A unique close spelling gets a suggestion; no automatic fix is offered."
    }
    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A misspelled list item:",
            source: "\\begin{itemize}\n\\tem First\n\\end{itemize}\n",
        }]
    }
    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        if ctx.file_kind != crate::source::FileKind::Tex {
            return;
        }
        let sites = crate::semantic::scan_definition_sites(ctx.root);
        let commands: HashSet<_> = sites
            .iter()
            .filter(|site| site.kind == crate::semantic::DefSiteKind::Command)
            .map(|site| site.name.as_str())
            .collect();
        let metadata =
            CommandMetadata::from_edges(&crate::project::collect_package_edge_keys(ctx.root, None));
        let mut ranges: Vec<_> = sites.iter().map(|site| site.range).collect();
        ranges.sort_by_key(|range| range.start());
        let mut index = 0;
        let mut definition_end = rowan::TextSize::new(0);
        for node in ctx
            .root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::COMMAND)
        {
            let inside_definition = node.text_range().end() <= definition_end;
            while index < ranges.len() && ranges[index].start() <= node.text_range().start() {
                definition_end = definition_end.max(ranges[index].end());
                index += 1;
            }
            if inside_definition {
                continue;
            }
            check_command(&node, ctx, &commands, &metadata, sink);
        }
    }
}

fn check_command(
    node: &SyntaxNode,
    ctx: &RuleContext<'_>,
    commands: &HashSet<&str>,
    metadata: &CommandMetadata,
    sink: &mut Vec<Diagnostic>,
) {
    let Some(name) = command_name(node) else {
        return;
    };
    if !name.bytes().all(|b| b.is_ascii_alphabetic())
        || commands.contains(name.as_str())
        || metadata.contains(&name)
        || ctx.user_definitions().command(&name).is_some()
        || is_carried_command(node)
    {
        return;
    }
    let Some(range) = control_word_range(node) else {
        return;
    };
    if ctx.in_expl3(usize::from(range.start())) {
        return;
    }
    let candidates: Vec<_> = [
        "item",
        "textbf",
        "textit",
        "emph",
        "section",
        "subsection",
        "begin",
        "end",
        "label",
        "ref",
        "cite",
        "documentclass",
        "usepackage",
        "include",
        "input",
        "author",
        "title",
        "date",
        "frac",
        "sqrt",
    ]
    .into_iter()
    .filter(|candidate| one_edit(&name, candidate))
    .collect();
    let suggestion = match candidates.as_slice() {
        [candidate] => format!("; did you mean `\\{candidate}`?"),
        _ => String::new(),
    };
    sink.push(Diagnostic {
        rule: "unknown-command",
        severity: Severity::Warning,
        path: Default::default(),
        start: range.start().into(),
        end: range.end().into(),
        message: format!("Unknown command `\\{name}`{suggestion}"),
        fix: None,
        related: Vec::new(),
    });
}

/// Completion metadata knows names from packages that a document may not load.
/// Keep that vocabulary separate from the names available to this lint check.
struct CommandMetadata {
    packages: HashSet<String>,
}

impl CommandMetadata {
    fn from_edges<'a>(edges: impl IntoIterator<Item = &'a PackageEdgeKey>) -> Self {
        let mut packages: HashSet<String> = ["tex", "latex-document", "latex-dev"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        for edge in edges {
            if let PackageTarget::Path(path) = &edge.target
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                packages.insert(name.to_owned());
                if matches!(
                    edge.kind,
                    PackageKind::DocumentClass
                        | PackageKind::LoadClass
                        | PackageKind::LoadClassWithOptions
                ) {
                    packages.insert(format!("class-{name}"));
                }
            }
        }
        let mut pending: Vec<_> = packages.iter().cloned().collect();
        while let Some(package) = pending.pop() {
            for &included in signature::cwl().package_includes(&package) {
                if packages.insert(included.to_owned()) {
                    pending.push(included.to_owned());
                }
            }
        }
        Self { packages }
    }

    fn contains(&self, name: &str) -> bool {
        let providers = signature::cwl().command_packages(name);
        if !providers.is_empty() {
            return providers
                .iter()
                .any(|package| self.packages.contains(*package));
        }
        signature::builtin().command(name).is_some()
            || crate::semantic::math::command_glyph(name).is_some()
    }
}

/// Package loads in another source of the selected root can make a name known.
pub fn suppress_from_packages<'a>(
    findings: &mut Vec<Diagnostic>,
    source: &str,
    edges: impl IntoIterator<Item = &'a PackageEdgeKey>,
) {
    let metadata = CommandMetadata::from_edges(edges);
    findings.retain(|d| {
        d.rule != "unknown-command"
            || source
                .get(d.start..d.end)
                .and_then(|s| s.strip_prefix('\\'))
                .is_none_or(|name| !metadata.contains(name))
    });
}

fn is_carried_command(node: &SyntaxNode) -> bool {
    if node.ancestors().skip(1).any(|ancestor| {
        ancestor.kind() == SyntaxKind::COMMAND
            && command_name(&ancestor).is_some_and(|name| {
                crate::semantic::define::is_definition_command(&name)
                    || signature::builtin().command(&name).is_none()
            })
    }) {
        return true;
    }
    let mut previous = node.prev_sibling_or_token();
    while let Some(element) = previous {
        if !matches!(
            element.kind(),
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
        ) {
            return element
                .as_node()
                .and_then(command_name)
                .is_some_and(|name| {
                    matches!(
                        name.as_str(),
                        "string"
                            | "noexpand"
                            | "meaning"
                            | "show"
                            | "ifdefined"
                            | "let"
                            | "futurelet"
                    )
                });
        }
        previous = element.prev_sibling_or_token();
    }
    false
}

fn one_edit(left: &str, right: &str) -> bool {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    if a.len().abs_diff(b.len()) > 1 || a == b {
        return false;
    }
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    if a.len() < b.len() {
        return a[prefix..] == b[prefix + 1..];
    }
    if a.len() > b.len() {
        return a[prefix + 1..] == b[prefix..];
    }
    a[prefix + 1..] == b[prefix + 1..]
        || (prefix + 1 < a.len()
            && a[prefix] == b[prefix + 1]
            && a[prefix + 1] == b[prefix]
            && a[prefix + 2..] == b[prefix + 2..])
}

/// A loaded source can define a name that was unknown in isolation.
/// Accepting extra names is conservative: this check makes no absence claims.
pub fn suppress_defined<'a>(
    findings: &mut Vec<Diagnostic>,
    source: &str,
    names: impl IntoIterator<Item = &'a str>,
) {
    let names: std::collections::HashSet<_> = names.into_iter().collect();
    findings.retain(|d| {
        d.rule != "unknown-command"
            || source
                .get(d.start..d.end)
                .and_then(|s| s.strip_prefix('\\'))
                .is_none_or(|name| !names.contains(name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn findings(source: &str) -> Vec<Diagnostic> {
        crate::linter::check_document(
            std::path::Path::new("main.tex"),
            source,
            crate::parser::LatexFlavor::Document,
            &Default::default(),
        )
        .into_iter()
        .filter(|d| d.rule == "unknown-command")
        .collect()
    }
    #[test]
    fn typos_and_known_names() {
        assert_eq!(
            findings("\\begin{itemize}\n\\tem First\n\\end{itemize}").len(),
            1
        );
        assert_eq!(findings("\\textfb{hello}").len(), 1);
        assert!(findings("\\item First \\emph{hello}").is_empty());
        assert_eq!(findings("\\mycustommacro").len(), 1);
        assert_eq!(findings("{\\tem First}").len(), 1);
        assert_eq!(findings("\\firstunknown\\secondunknown").len(), 2);
        assert!(findings("\\string\\tem").is_empty());
        assert!(findings("\\newcommand{\\tem}{x}\n\\tem").is_empty());
        assert!(findings("\\newcommand{\\foo}{\\tem}").is_empty());
        assert!(findings("\\begin{verbatim}\n\\tem\n\\end{verbatim}").is_empty());
        assert!(findings("% tex-ls-lint skip unknown-command: intentional\n\\tem").is_empty());
    }
    #[test]
    fn cross_file_definition_suppresses_hint() {
        let mut result = findings("\\tem");
        assert_eq!(result.len(), 1);
        suppress_defined(&mut result, "\\tem", ["tem"]);
        assert!(result.is_empty());
    }

    #[test]
    fn local_declarations_without_parser_signatures_are_known() {
        for source in [
            "\\newif\\iflocalflag\n\\localflagtrue\n\\localflagfalse\n",
            "\\let\\localalias=\\textbf\n\\localalias{x}\n",
            "\\newlength{\\locallength}\n\\locallength\n",
        ] {
            assert!(findings(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn literal_definition_bodies_are_skipped_without_hiding_later_unknown_commands() {
        for definer in [
            "csdef",
            "csgdef",
            "csedef",
            "csxdef",
            "protected@csedef",
            "protected@csxdef",
        ] {
            for parameters in ["#1", "#1#2", "#1stop", "#1 #2", "#1% comment\n#2"] {
                let source = format!(
                    "\\usepackage{{etoolbox}}\n\\makeatletter\n\\{definer}{{defined}}{parameters}{{\\unknowninside}}\n\\defined{{x}}\n\\unknownoutside\n"
                );
                let diagnostics = findings(&source);
                assert_eq!(diagnostics.len(), 1, "{source}: {diagnostics:?}");
                assert_eq!(
                    &source[diagnostics[0].start..diagnostics[0].end],
                    "\\unknownoutside",
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn completion_metadata_requires_a_loaded_provider() {
        for (name, package) in [
            ("ce", "mhchem"),
            ("SI", "siunitx"),
            ("cref", "cleveref"),
            ("includegraphics", "graphicx"),
            ("textcolor", "xcolor"),
            ("DeclareMathOperator", "amsmath"),
            ("text", "amsmath"),
            ("captionsetup", "caption"),
            ("pgfkeys", "tikz"),
            ("hdashline", "arydshln"),
            ("newabbreviation", "glossaries-extra"),
        ] {
            let command = format!("\\{name}");
            assert_eq!(findings(&command).len(), 1, "{command}");
            assert!(
                findings(&format!("\\usepackage{{{package}}}\n{command}")).is_empty(),
                "{package}: {command}"
            );
            assert_eq!(
                findings(&format!("\\usepackage{{xspace}}\n{command}")).len(),
                1,
                "unrelated package: {command}"
            );
        }
        assert!(findings("\\documentclass{beamer}\n\\frametitle{Title}\n\\pause").is_empty());
        for source in [
            "\\usepackage{% comment,ignored\n mhchem}\n\\ce{H2O}",
            "\\usepackage{mhchem% trailing\r\n}\n\\ce{H2O}",
            "\\usepackage{xspace,% between\n mhchem}\n\\ce{H2O}",
            "\\usepackage{mh% continued\n  chem}\n\\ce{H2O}",
        ] {
            assert!(findings(source).is_empty(), "{source}");
        }
        assert_eq!(findings("\\documentclass{article}\n\\pause").len(), 1);
        assert!(
            findings("\\documentclass{book}\n\\frontmatter\n\\mainmatter\n\\backmatter").is_empty()
        );
        assert_eq!(
            findings("\\documentclass{article}\n\\frontmatter\n\\mainmatter\n\\backmatter").len(),
            3
        );
        // A loaded package's optional imports are not automatically available.
        assert_eq!(findings("\\usepackage{xcolor}\n\\rowcolor{red}").len(), 1);
        assert!(findings("\\ifx\\relax\\relax\\fi\n$\\alpha+\\beta$\n\\textbf{x}").is_empty());
    }

    #[test]
    fn local_definition_wins_over_unloaded_metadata() {
        assert!(findings("\\newcommand{\\ce}[1]{#1}\n\\ce{x}").is_empty());
        let findings = findings("\\DeclareMathOperator{\\localop}{value}\n$\\localop$\n");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("\\DeclareMathOperator"));
    }
}
