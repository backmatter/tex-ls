//! Diagnostics operations.
use super::*;

/// Compute diagnostics for a `.bib` file off the snapshot: bib parse diagnostics
/// plus root-scoped lint findings over the cached tree and bibliography model.
pub fn analyze_bib(
    snapshot: &Analysis,
    path: &Path,
    rules: &RuleSelection,
    enc: PositionEncoding,
) -> Option<Vec<Diagnostic>> {
    let file = snapshot.lookup_file(path)?;
    let idx = snapshot.file_line_index(file, enc);
    let mut diags: Vec<Diagnostic> = snapshot
        .bib_parse_diagnostics(file)
        .iter()
        .map(|d| Diagnostic {
            range: byte_range_to_lsp(&idx, d.start, d.end),
            severity: Some(DiagnosticSeverity::Error),
            source: Some("tex-ls".to_owned()),
            message: d.message.clone().into(),
            ..Default::default()
        })
        .collect();
    for d in snapshot.bib_lint_findings(file) {
        if rules.is_active(d.rule) {
            diags.push(lint_to_lsp(&idx, d.clone(), false, path));
        }
    }
    Some(diags)
}

/// Map a linter [`tex_ls_analysis::linter::Diagnostic`] (shared by the LaTeX and BibTeX
/// linters) onto an LSP [`Diagnostic`].
/// Base URL of the published LaTeX linter-rules reference; each rule is a
/// heading whose Markdown anchor is the rule id (`#deprecated-command`), so
/// `{BASE}#{rule}` deep-links the rule's docs.
pub const LATEX_RULES_DOC_URL: &str =
    "https://github.com/backmatter/tex-ls/blob/main/docs/reference/linter-rules.md";
pub const BIB_RULES_DOC_URL: &str =
    "https://github.com/backmatter/tex-ls/blob/main/docs/reference/bib-linter-rules.md";

/// Convert a finding with a link to the appropriate rule catalogue. Shared rule
/// IDs still link to the source language's examples. ResponsePolicy negotiates
/// links and markup at the wire boundary.
pub fn lint_to_lsp(
    idx: &LineIndex,
    d: tex_ls_analysis::linter::Diagnostic,
    latex_rules: bool,
    self_path: &Path,
) -> Diagnostic {
    let base = if latex_rules {
        LATEX_RULES_DOC_URL
    } else {
        BIB_RULES_DOC_URL
    };
    let code_description = format!("{base}#{}", d.rule)
        .parse()
        .ok()
        .map(|href| CodeDescription { href });
    let related_information = lint_related_to_lsp(idx, self_path, &d.related);
    let help = match d.rule {
        "undefined-ref" => " Check the key or add a matching `\\label` in this document's sources.",
        "undefined-citation" => " Check the key against a loaded bibliography or `\\bibitem`.",
        _ => "",
    };
    Diagnostic {
        range: byte_range_to_lsp(idx, d.start, d.end),
        severity: Some(severity_to_lsp(d.severity)),
        code: Some(Code::String(d.rule.to_owned())),
        code_description,
        source: Some("tex-ls".to_owned()),
        message: format!("{}{help}", d.message).into(),
        related_information,
        tags: lint_diagnostic_tags(d.rule),
        ..Default::default()
    }
}

/// Turn a finding's [`RelatedInfo`](tex_ls_analysis::linter::RelatedInfo) secondary
/// locations into LSP `DiagnosticRelatedInformation`, the clickable "see also"
/// links (e.g. the first definition behind a `duplicate-label`). Returns `None`
/// when there are none, so the field stays absent for the common case.
///
/// A secondary in the *current* file (`self_path`) resolves its range against
/// `idx`; one in another file is **file-level** — a `0..0` byte range
/// maps to the document start regardless of encoding, so we need neither that
/// file's text nor its line index. An entry whose path cannot form a `file://`
/// URI is skipped (mirrors [`location_for`]).
pub fn lint_related_to_lsp(
    idx: &LineIndex,
    self_path: &Path,
    related: &[tex_ls_analysis::linter::RelatedInfo],
) -> Option<Vec<DiagnosticRelatedInformation>> {
    if related.is_empty() {
        return None;
    }
    let items: Vec<DiagnosticRelatedInformation> = related
        .iter()
        .filter_map(|ri| {
            let range = if ri.path == self_path {
                byte_range_to_lsp(idx, ri.start, ri.end)
            } else {
                // File-level link: `0..0` at the document start.
                Range::default()
            };
            Some(DiagnosticRelatedInformation {
                location: Location {
                    uri: path_to_uri(&ri.path)?,
                    range,
                },
                message: ri.message.clone(),
            })
        })
        .collect();
    (!items.is_empty()).then_some(items)
}

/// Map a lint rule id onto the LSP diagnostic tags editors render specially:
/// `Unnecessary` dims/greys the span (dead code), `Deprecated` strikes it
/// through. Keyed on the stable rule id rather than a field on
/// [`tex_ls_analysis::linter::Diagnostic`], so it stays a purely presentational LSP concern
/// (the CLI renderer is untouched). Returns `None` for rules with no tag.
pub fn lint_diagnostic_tags(rule: &str) -> Option<Vec<DiagnosticTag>> {
    match rule {
        // A label defined but never referenced is a dead definition.
        "unreferenced-label" => Some(vec![DiagnosticTag::Unnecessary]),
        // Commands/environments superseded by a modern LaTeX equivalent.
        "deprecated-command" | "obsolete-environment" | "primitive-command" => {
            Some(vec![DiagnosticTag::Deprecated])
        }
        _ => None,
    }
}

/// Compute pull diagnostics from the same captured inputs as push diagnostics.
pub fn compute_diagnostics(
    snapshot: &Analysis,
    path: &Path,
    kind: FileKind,
    rules: &RuleSelection,
    enc: PositionEncoding,
) -> Vec<Diagnostic> {
    diagnostic_store::DiagnosticEntry::capture(snapshot, path, kind, rules, enc).items
}

/// Run the linter over the snapshot's cached tree + model, returning the raw
/// findings (with their fixes). The lint half of [`analyze_tex`]/[`analyze_bib`]
/// without the LSP conversion, so code actions can read each finding's `fix`.
pub fn lint_findings(
    snapshot: &Analysis,
    path: &Path,
    kind: FileKind,
    rules: &RuleSelection,
) -> Option<Vec<tex_ls_analysis::linter::Diagnostic>> {
    let file = snapshot.lookup_file(path)?;
    let findings = match kind {
        FileKind::Tex
        | FileKind::CodeTex
        | FileKind::Sty
        | FileKind::Cls
        | FileKind::Dtx
        | FileKind::Ins => snapshot.latex_lint_findings(file),
        FileKind::Bib => snapshot.bib_lint_findings(file),
    };
    Some(
        findings
            .iter()
            .filter(|d| rules.is_active(d.rule))
            .cloned()
            .collect(),
    )
}

/// Derive a stable, content-addressed `result_id` from a diagnostic set, so a
/// re-pull with no change reports `unchanged`. Hashes the JSON encoding because
/// [`Diagnostic`] is not `Hash`; the encoding is order-stable (serde field order +
/// deterministic diagnostic ordering), so identical diagnostics hash identically.
pub fn result_id_for(items: &[Diagnostic]) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(&serde_json::to_vec(items).expect("diagnostics serialize"));
    hasher.finish().to_string()
}

/// Map a linter [`Severity`] onto the LSP severity. Parse diagnostics bypass
/// this (always `ERROR`); lint rules carry their own severity.
pub fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::Error,
        Severity::Warning => DiagnosticSeverity::Warning,
        Severity::Info => DiagnosticSeverity::Information,
        Severity::Hint => DiagnosticSeverity::Hint,
    }
}

pub fn analyze_tex(
    snapshot: &Analysis,
    path: &Path,
    rules: &RuleSelection,
    enc: PositionEncoding,
) -> Option<Vec<Diagnostic>> {
    let file = snapshot.lookup_file(path)?;
    // The file's normalized identity, which keys the cross-file resolvers.
    let lint_path = snapshot.file_path(file).to_path_buf();
    let idx = snapshot.file_line_index(file, enc);
    let mut diags: Vec<Diagnostic> = snapshot
        .parse_diagnostics(file)
        .iter()
        .map(|d| Diagnostic {
            range: byte_range_to_lsp(&idx, d.start, d.end),
            severity: Some(DiagnosticSeverity::Error),
            source: Some("tex-ls".to_owned()),
            message: d.message.clone().into(),
            ..Default::default()
        })
        .collect();
    for d in snapshot.latex_lint_findings(file) {
        if rules.is_active(d.rule) {
            diags.push(lint_to_lsp(&idx, d.clone(), true, &lint_path));
        }
    }
    Some(diags)
}
