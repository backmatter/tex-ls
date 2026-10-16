//! Lint command IO, output, and phase coordination.
mod bibliographies;
mod fixes;
mod project;

use super::inputs::{
    Inputs, LINT_MISSING_INPUT, color_enabled, inputs_or_exit, report_discovery_error,
};
use bibliographies::supplemental_bibliographies;
use fixes::apply_fixes_to_paths;
use project::collect_project_diagnostics_with_bibliographies;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};
use tex_ls::{
    cli::{ColorChoice, LintOutput},
    file_discovery::{ExcludeFilter, collect_lint_files},
};
use tex_ls_analysis::{
    linter::{OutputMode, RuleSelection, render_findings, render_json},
    source::{FileKind, file_kind_or_tex},
};
use tex_ls_parser::declarations::ResolvedDeclarations;

/// Lower the CLI [`LintOutput`] to the renderer's [`OutputMode`] by output format.
pub(super) fn lint_output_mode(arg: LintOutput) -> OutputMode {
    match arg {
        LintOutput::Pretty => OutputMode::Pretty,
        LintOutput::Concise => OutputMode::Concise,
        LintOutput::Json => OutputMode::Json,
    }
}

/// Print a rule's description and examples (`lint --explain <rule>`), then exit.
/// The id is looked up in the LaTeX registry first, then the bib registry (the
/// two share one namespace, so at most one matches). Unknown ids exit `2` after
/// listing every known built-in rule id across both linters.
pub(super) fn run_explain(id: &str) -> ExitCode {
    let doc = tex_ls_analysis::linter::docs::explain_rule(id)
        .or_else(|| tex_ls_analysis::bib::linter::docs::explain_rule(id));
    match doc {
        Some(doc) => {
            print!("{doc}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("tex-ls: unknown lint rule `{id}`");
            eprintln!(
                "known rules: {}",
                tex_ls_analysis::linter::rules::all_known_rule_ids()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            ExitCode::from(2)
        }
    }
}

/// One source read, preserving its reporting path on failure.
type ReadResult = Result<(PathBuf, String, FileKind), (PathBuf, std::io::Error)>;

/// Lint each path (or stdin), rendering parse diagnostics. Exits non-zero if
/// any diagnostics are reported or any file fails to read. With `fix`, safe
/// autofixes (plus unsafe ones when `unsafe_fixes` is set) are applied in place
/// first; the reporting pass below then shows whatever findings remain.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_lint(
    paths: &[PathBuf],
    fix: bool,
    unsafe_fixes: bool,
    stdin_filepath: Option<&Path>,
    exclude: &ExcludeFilter,
    rules: &RuleSelection,
    declared: &ResolvedDeclarations,
    mode: OutputMode,
    color: ColorChoice,
) -> ExitCode {
    let paths = match inputs_or_exit(paths, "lint", LINT_MISSING_INPUT) {
        Inputs::Stdin => None,
        Inputs::Paths(paths) => Some(paths),
    };

    // Apply fixes in place first; the reporting pass below then re-reads from
    // disk and shows whatever findings remain. This is a two-pass flow.
    // Stdin has nowhere to write back, so `--fix` only acts on files.
    if fix
        && let Some(paths) = paths
        && let Some(code) = apply_fixes_to_paths(paths, unsafe_fixes, exclude, rules, declared)
    {
        return code;
    }

    // Hold each file's text (and which pipeline it feeds) in memory keyed by the
    // label we report it under, so the renderer can fetch source for snippets
    // without re-reading from disk (and so stdin, which has no path, still gets a
    // source). Stdin has no extension to dispatch on, so it is LaTeX unless
    // `--stdin-filepath` names the buffer (`.bib` → BibTeX); the label stays
    // `<stdin>` regardless, so the named path never reaches the report or disk.
    let mut sources: Vec<(PathBuf, String, FileKind)> = Vec::new();
    let mut failed = false;

    if let Some(paths) = paths {
        let files = match collect_lint_files(paths, exclude) {
            Ok(files) => files,
            Err(err) => {
                report_discovery_error(&err);
                return ExitCode::FAILURE;
            }
        };
        if files.is_empty() {
            // Under `--force-exclude` an empty set is expected (a runner like
            // pre-commit may pass only excluded files), so it is a clean no-op.
            if exclude.force() {
                return ExitCode::SUCCESS;
            }
            eprintln!(
                "tex-ls: no .tex, .sty, .cls, .dtx, .ins, or .bib files found under the provided input paths"
            );
            return ExitCode::FAILURE;
        }
        // Read every file in parallel (IO-bound; the OS serves many opens at once).
        // Order-preserving collect keeps `sources` in the discovered (sorted) order,
        // then a serial fold reports read failures deterministically.
        let read_results: Vec<ReadResult> = files
            .par_iter()
            .map(|(path, kind)| match std::fs::read_to_string(path) {
                Ok(content) => Ok((
                    tex_ls_analysis::source::normalize_path(path),
                    content,
                    *kind,
                )),
                Err(err) => Err((path.clone(), err)),
            })
            .collect();
        for result in read_results {
            match result {
                Ok(source) => sources.push(source),
                Err((path, err)) => {
                    eprintln!("tex-ls: cannot read {}: {err}", path.display());
                    failed = true;
                }
            }
        }
    } else {
        let mut input = String::new();
        if let Err(err) = std::io::stdin().read_to_string(&mut input) {
            eprintln!("tex-ls: cannot read stdin: {err}");
            return ExitCode::FAILURE;
        }
        let kind = stdin_filepath.map_or(FileKind::Tex, file_kind_or_tex);
        sources.push((PathBuf::from("<stdin>"), input, kind));
    }

    // Build project resolution before linting. The CLI and incremental queries
    // share the label, citation, and dependency extraction helpers.
    let supplemental = supplemental_bibliographies(&sources, declared);
    let mut diagnostics =
        collect_project_diagnostics_with_bibliographies(&sources, declared, &supplemental);

    // Drop findings from rules the config/CLI deselected. Parse diagnostics
    // (`rule == "parse"`) are always kept (see `RuleSelection::is_active`).
    diagnostics.retain(|d| rules.is_active(d.rule));

    // Findings from the two pipelines arrive interleaved by file; sort so the
    // renderer presents them deterministically (by path, then position).
    diagnostics.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.start.cmp(&b.start))
            .then(a.end.cmp(&b.end))
            .then(a.rule.cmp(b.rule))
    });

    if mode == OutputMode::Json {
        // Serialize the entire report before writing any machine-readable data.
        match render_json(&diagnostics) {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("tex-ls: cannot serialize lint findings: {error}");
                return ExitCode::from(2);
            }
        }
    } else if !diagnostics.is_empty() {
        // Index sources by path so the renderer's per-file source lookup is O(1),
        // not a linear scan of every source (quadratic over a large project).
        let source_index: HashMap<&Path, &str> = sources
            .iter()
            .map(|(p, text, _)| (p.as_path(), text.as_str()))
            .collect();
        let source_for = |path: &Path| source_index.get(path).map(|s| s.to_string());
        // The text modes print to stderr, so that is the stream `--color auto`
        // has to test — a `2>log` run stays plain even with stdout on a tty.
        let use_color = color_enabled(color, std::io::stderr().is_terminal());
        eprint!(
            "{}",
            render_findings(&diagnostics, mode, use_color, &source_for)
        );
    }

    if failed || !diagnostics.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
