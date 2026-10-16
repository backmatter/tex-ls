//! Bounded per-file lint fix application and safety checks.
use super::super::inputs::report_discovery_error;
use rayon::prelude::*;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};
use tex_ls::file_discovery::{ExcludeFilter, collect_lint_files};
use tex_ls_analysis::{
    linter::{RuleSelection, apply_fixes, check_document_fixable},
    source::FileKind,
};
use tex_ls_parser::{
    declarations::ResolvedDeclarations, parser::parse_with_declarations, syntax::SyntaxNode,
};

/// Cap on fixpoint iterations per file, guarding against a fix that fails to
/// clear its own diagnostic.
const MAX_FIX_ITERATIONS: usize = 10;

/// Discover lintable files under `paths` and apply autofixes in place. Returns
/// `Some(exit_code)` only on a hard error (discovery / IO); on success returns
/// `None` so the caller falls through to the normal reporting pass.
///
/// Both `.tex` and `.bib` files are fixed, each through its own linter; rules that
/// emit no autofix (the report-only majority) leave their findings for the
/// reporting pass that follows.
pub(super) fn apply_fixes_to_paths(
    paths: &[PathBuf],
    include_unsafe: bool,
    exclude: &ExcludeFilter,
    rules: &RuleSelection,
    declared: &ResolvedDeclarations,
) -> Option<ExitCode> {
    let files = match collect_lint_files(paths, exclude) {
        Ok(files) => files,
        Err(err) => {
            report_discovery_error(&err);
            return Some(ExitCode::FAILURE);
        }
    };
    if files.is_empty() {
        if exclude.force() {
            return Some(ExitCode::SUCCESS);
        }
        eprintln!("tex-ls: no .tex or .bib files found under the provided input paths");
        return Some(ExitCode::FAILURE);
    }

    // Fix each file in parallel: `fix_file` is a pure per-file fixpoint (read, lint,
    // apply, write back) with no shared mutable state, and distinct output files
    // never race. The order-preserving collect lets the serial fold below report
    // "n fixes applied" messages and read failures deterministically, in discovered
    // order, mirroring `run_format_paths`.
    let outcomes: Vec<FixOutcome> = files
        .par_iter()
        .map(
            |(path, kind)| match fix_file(path, *kind, include_unsafe, rules, declared) {
                Ok(0) => FixOutcome::Unchanged,
                Ok(n) => FixOutcome::Applied {
                    path: path.clone(),
                    count: n,
                },
                Err(err) => {
                    FixOutcome::Failed(format!("tex-ls: cannot fix {}: {err}", path.display()))
                }
            },
        )
        .collect();

    let mut failed = false;
    for outcome in outcomes {
        match outcome {
            FixOutcome::Unchanged => {}
            FixOutcome::Applied { path, count } => {
                eprintln!("{}: {count} fix{} applied", path.display(), plural(count))
            }
            FixOutcome::Failed(message) => {
                eprintln!("{message}");
                failed = true;
            }
        }
    }

    failed.then_some(ExitCode::FAILURE)
}

/// Per-file result of the parallel autofix pass in [`apply_fixes_to_paths`],
/// folded serially afterward so messages print in discovered order.
enum FixOutcome {
    /// The file was already clean; nothing to report.
    Unchanged,
    /// `count` fixes were applied to `path`.
    Applied { path: PathBuf, count: usize },
    /// The file could not be fixed; carries the ready-to-print error message.
    Failed(String),
}

/// Run the fixpoint loop on a single file and write it back if anything changed.
/// Returns the number of individual fixes applied. Re-lints after each round so
/// fixes can cascade; bounded by [`MAX_FIX_ITERATIONS`].
/// Routes to the LaTeX or BibTeX linter by [`FileKind`]. `rules` gates
/// which findings contribute fixes, so a deselected rule's autofix never applies.
fn fix_file(
    path: &Path,
    kind: FileKind,
    include_unsafe: bool,
    rules: &RuleSelection,
    declared: &ResolvedDeclarations,
) -> std::io::Result<usize> {
    let mut content = std::fs::read_to_string(path)?;
    // Tenet #1: a fix owes correctness — the result still parses and is still
    // lossless. Snapshot the pre-fix parse-error count so the debug guard below
    // can assert no fix introduced a *new* syntactic error.
    let errors_before = debug_parse_error_count(&content, kind, declared);
    let mut total = 0usize;
    for _ in 0..MAX_FIX_ITERATIONS {
        let diagnostics = match kind {
            FileKind::Tex
            | FileKind::CodeTex
            | FileKind::Sty
            | FileKind::Cls
            | FileKind::Dtx
            | FileKind::Ins => {
                // Fixpoint loop: only fix-emitting rules can change anything, so run
                // just those each round (report-only rules are surfaced later by the
                // reporting pass).
                check_document_fixable(path, &content, kind.lex_config(), declared)
            }
            FileKind::Bib => tex_ls_analysis::bib::linter::check_document(path, &content),
        };
        let fixes: Vec<_> = diagnostics
            .into_iter()
            .filter(|d| rules.is_active(d.rule))
            .filter_map(|d| d.fix)
            .collect();
        if fixes.is_empty() {
            break;
        }
        let outcome = apply_fixes(&content, &fixes, include_unsafe);
        if outcome.applied == 0 {
            break;
        }
        total += outcome.applied;
        content = outcome.output;
    }
    if total > 0 {
        debug_assert_fixes_preserved(path, kind, &content, errors_before, declared);
        std::fs::write(path, &content)?;
    }
    Ok(total)
}

/// Parse-error count of `content` under `kind`'s flavor, computed only in debug
/// builds (returns `0` in release, where the guard is compiled out). Feeds
/// [`debug_assert_fixes_preserved`].
fn debug_parse_error_count(
    content: &str,
    kind: FileKind,
    declared: &ResolvedDeclarations,
) -> usize {
    if !cfg!(debug_assertions) {
        return 0;
    }
    match kind {
        FileKind::Bib => tex_ls_analysis::bib::parse(content).errors.len(),
        _ => parse_with_declarations(content, kind.lex_config(), declared)
            .errors
            .len(),
    }
}

/// Debug-only tripwire enforcing tenet #1 on the `--fix` output before it is
/// written back: the fixed text must (1) reconstruct losslessly and (2) carry no
/// *new* parse errors relative to the original (`errors_before`). A fix is a
/// textual edit that owes correctness but never layout, so a mis-built fix span
/// that corrupts structure — deleting a closing brace, splicing at the wrong
/// offset — is exactly what this catches before it reaches disk. Compiled out of
/// release builds (`debug_assert!`), so it costs nothing in shipped binaries.
fn debug_assert_fixes_preserved(
    path: &Path,
    kind: FileKind,
    content: &str,
    errors_before: usize,
    declared: &ResolvedDeclarations,
) {
    if !cfg!(debug_assertions) {
        return;
    }
    let (reconstructed, errors_after) = match kind {
        FileKind::Bib => {
            let parsed = tex_ls_analysis::bib::parse(content);
            (parsed.syntax().to_string(), parsed.errors.len())
        }
        _ => {
            // Same declarations as the `errors_before` snapshot: the two counts
            // are only comparable when both parses recognize the same constructs.
            let parsed = parse_with_declarations(content, kind.lex_config(), declared);
            (
                SyntaxNode::new_root(parsed.green.clone()).to_string(),
                parsed.errors.len(),
            )
        }
    };
    debug_assert_eq!(
        reconstructed,
        content,
        "--fix produced non-lossless output for {}",
        path.display()
    );
    debug_assert!(
        errors_after <= errors_before,
        "--fix introduced {} new parse error(s) in {} ({errors_before} -> {errors_after})",
        errors_after.saturating_sub(errors_before),
        path.display()
    );
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "es" }
}
