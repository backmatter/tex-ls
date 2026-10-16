//! Signature prototypes and facts shared by hover, completion, and signature help.

use std::fmt::Write as _;
use tex_ls_analysis::hover::Provenance;
use tex_ls_parser::semantic::signature::{ArgKind, CommandSig, EnvironmentSig, OutlineKind};

/// The provenance fact line: `command` / `user-defined command` /
/// ``command defined by package `mypkg` `` (same for `environment`).
pub fn provenance_label(provenance: &Provenance, word: &str) -> String {
    match provenance {
        Provenance::Base => word.to_string(),
        Provenance::Document => format!("user-defined {word}"),
        Provenance::Package(pkg) => format!("{word} defined by package `{pkg}`"),
    }
}

/// Render a required-brace or optional-bracket slot in a signature prototype.
pub fn arg_slot(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Brace => "{}",
        ArgKind::Bracket => "[]",
    }
}

/// A human summary of an argument list: e.g. `2 required, 1 optional`. Empty when the
/// construct takes no arguments.
pub fn arg_summary(args: &[tex_ls_parser::semantic::signature::ArgSpec]) -> Option<String> {
    let req = args.iter().filter(|a| a.required).count();
    let opt = args.len() - req;
    let mut parts = Vec::new();
    if req > 0 {
        parts.push(format!("{req} required"));
    }
    if opt > 0 {
        parts.push(format!("{opt} optional"));
    }
    (!parts.is_empty()).then(|| format!("{} argument{}", parts.join(", "), plural(args.len())))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `\name{}{}` prototype + a `·`-joined facts line.
pub fn render_command(name: &str, sig: &CommandSig, provenance: &Provenance) -> String {
    let mut out = String::new();
    let _ = write!(out, "```latex\n\\{name}");
    for arg in sig.args.iter() {
        out.push_str(arg_slot(arg.kind));
    }
    out.push_str("\n```\n");

    if matches!(provenance, Provenance::Base)
        && let Some(glyph) = tex_ls_parser::semantic::math::command_glyph(name)
    {
        let _ = write!(out, "\nSymbol: {glyph}\n\n");
    }
    let mut facts = vec![provenance_label(provenance, "command")];
    if let Some(level) = sig.sectioning {
        facts.push(format!("sectioning level {level}"));
    }
    if sig.verbatim {
        facts.push("verbatim argument".to_string());
    }
    if let Some(summary) = arg_summary(&sig.args) {
        facts.push(summary);
    }
    out.push_str(&facts.join(" · "));
    if matches!(provenance, Provenance::Base) {
        for (index, doc) in crate::source_cards::argument_docs(name).iter().enumerate() {
            let _ = write!(out, "\n\nArgument {}: {doc}", index + 1);
        }
    }
    out
}

/// `\begin{name} … \end{name}` prototype + a `·`-joined facts line.
pub fn render_environment(name: &str, sig: &EnvironmentSig, provenance: &Provenance) -> String {
    let mut out = String::new();
    let _ = write!(out, "```latex\n\\begin{{{name}}}");
    for arg in sig.args.iter() {
        out.push_str(arg_slot(arg.kind));
    }
    let _ = write!(out, " … \\end{{{name}}}\n```\n");

    let mut facts = vec![provenance_label(provenance, "environment")];
    match sig.outline {
        Some(OutlineKind::Float) => facts.push("float".to_string()),
        Some(OutlineKind::Theorem) => facts.push("theorem-like".to_string()),
        Some(OutlineKind::Frame) => facts.push("Beamer frame".to_string()),
        None => {}
    }
    if sig.math {
        facts.push("math".to_string());
    }
    if sig.align {
        facts.push("alignment".to_string());
    }
    if sig.list {
        facts.push("list".to_string());
    }
    if sig.verbatim_body {
        facts.push("verbatim body".to_string());
    } else if sig.code {
        facts.push("code body".to_string());
    }
    if let Some(summary) = arg_summary(&sig.args) {
        facts.push(summary);
    }
    out.push_str(&facts.join(" · "));
    out
}
