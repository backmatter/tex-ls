use super::*;

// --- `debug format`: per-file invariant checks -----------------------------
// Perturbation-based trivia checks are deliberately excluded from `--checks all`.

/// One invariant (or the failure to even run it) checked per file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckKind {
    Losslessness,
    Idempotency,
    ContentChange,
    CommentChange,
    Trivia,
    TriviaStrict,
    FormatError,
}

impl CheckKind {
    fn label(self) -> &'static str {
        match self {
            CheckKind::Losslessness => "losslessness",
            CheckKind::Idempotency => "idempotency",
            CheckKind::ContentChange => "content-change",
            CheckKind::CommentChange => "comment-change",
            CheckKind::Trivia => "trivia",
            CheckKind::TriviaStrict => "trivia-strict",
            CheckKind::FormatError => "format-error",
        }
    }
}

/// Every `%` comment in `text`, one per line, in document order and with trailing
/// whitespace trimmed (the printer drops a comment's trailing spaces with the
/// line's). Rendered as text so a divergence diffs like the other checks.
///
/// `DOC_MARGIN` and `GUARD` are excluded on purpose: a `.dtx` margin is
/// re-synthesized per output line by the doc-paragraph reflow, so its count is
/// layout. A `COMMENT` never is.
fn comment_sequence(text: &str, config: LexConfig) -> String {
    parse_with_flavor(text, config)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == tex_ls_parser::syntax::SyntaxKind::COMMENT)
        .map(|token| token.text().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A failed check: the two texts whose divergence is the finding. For
/// `format-error`, `left` is the formatter's error message and `right` is
/// empty (there is nothing to diff).
struct DebugFailure {
    kind: CheckKind,
    left: String,
    right: String,
    /// Extra context shown after the label — the trivia checks' offending
    /// variant. `None` for the other kinds.
    detail: Option<String>,
    class: Option<&'static str>,
}

/// Everything one file's check run produced: the pass texts (for `--dump-dir`)
/// plus any failures.
#[derive(Default)]
struct DebugArtifacts {
    /// `(input, parsed-reconstruction)` when the losslessness check ran.
    losslessness: Option<(String, String)>,
    /// `(input, once, twice)` when the idempotency check ran to completion.
    idempotency: Option<(String, String, String)>,
    /// The perturbed input (the reproducer) when either trivia check failed;
    /// its two formattings are the failure's `left`/`right`. Only one of the
    /// two ever runs per invocation, so they share the slot.
    trivia_perturbed: Option<String>,
    failures: Vec<DebugFailure>,
}

/// The `--checks` value as it appears in output (report header and the
/// all-passed line).
fn checks_label(checks: DebugChecksArg) -> &'static str {
    match checks {
        DebugChecksArg::Idempotency => "idempotency",
        DebugChecksArg::Losslessness => "losslessness",
        DebugChecksArg::Trivia => "trivia",
        DebugChecksArg::TriviaStrict => "trivia-strict",
        DebugChecksArg::All => "all",
    }
}

/// Map every character outside `[A-Za-z0-9._-]` to `_`, matching the smoke-test
/// workflow's `sed 's/[^[:alnum:]._-]/_/g'` so it can predict artifact names
/// from a repo-relative path.
fn sanitize_path_for_filename(path: &str) -> String {
    path.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 1-based line number of the first difference between two texts. When the
/// visible lines all match (the difference is only in trailing newline
/// material) this points just past the common lines; identical texts return 1,
/// which never occurs on a failure.
fn first_diff_line(left: &str, right: &str) -> usize {
    let mut left_lines = left.lines();
    let mut right_lines = right.lines();
    let mut line = 1;
    loop {
        match (left_lines.next(), right_lines.next()) {
            (Some(a), Some(b)) if a == b => line += 1,
            _ => return line,
        }
    }
}

/// Render a minimal diff body: a few common context lines, then the two sides'
/// remaining lines as `-`/`+` runs starting at the first difference, capped so
/// a whole-file divergence stays readable. The smoke-test workflow never
/// parses this block (only `Approx. diff start line`), so first-mismatch
/// granularity is enough — no diff dependency needed.
fn render_window_diff(left: &str, right: &str, out: &mut String) {
    const CONTEXT: usize = 3;
    const MAX_SIDE: usize = 40;
    let start = first_diff_line(left, right) - 1;
    let context_start = start.saturating_sub(CONTEXT);
    for line in left.lines().skip(context_start).take(start - context_start) {
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    for (side, text) in [('-', left), ('+', right)] {
        let mut lines = text.lines().skip(start);
        for line in lines.by_ref().take(MAX_SIDE) {
            out.push(side);
            out.push_str(line);
            out.push('\n');
        }
        if lines.next().is_some() {
            out.push(side);
            out.push_str(" [truncated]\n");
        }
    }
}

/// Build the `--report` Markdown. Contract with the smoke-test workflow: the
/// `### k. \`file\` (kind)` headings carry the parenthesized failure label, and
/// each diffable failure has an `Approx. diff start line: N` bullet.
fn build_debug_report(
    checks: DebugChecksArg,
    files_checked: usize,
    files_skipped: usize,
    failures: &[(String, DebugFailure)],
) -> String {
    let mut out = String::new();
    out.push_str("# Debug-format regression report\n\n");
    out.push_str(&format!(
        "- Checks: `{}`\n- Files checked: {files_checked}\n",
        checks_label(checks)
    ));
    // Only the two trivia checks skip files today, and for the same reason
    // (`.bib` runs nothing under either); parameterize if that ever diverges.
    if files_skipped > 0 {
        out.push_str(&format!(
            "- Files skipped: {files_skipped} (`.bib` — the trivia oracle is LaTeX-CST-based)\n"
        ));
    }
    out.push_str(&format!("- Failures: {}\n\n", failures.len()));
    if failures.is_empty() {
        out.push_str("All checks passed.\n");
        return out;
    }
    out.push_str("## Failures\n\n");
    for (idx, (file, failure)) in failures.iter().enumerate() {
        out.push_str(&format!(
            "### {}. `{}` ({})\n\n",
            idx + 1,
            file,
            failure.kind.label()
        ));
        if let Some(detail) = &failure.detail {
            out.push_str(&format!("- Variant: `{detail}`\n"));
        }
        if failure.kind == CheckKind::FormatError {
            out.push_str(&format!("- Error: {}\n\n", failure.left));
            continue;
        }
        out.push_str(&format!(
            "- Approx. diff start line: {}\n\n",
            first_diff_line(&failure.left, &failure.right)
        ));
        out.push_str("```diff\n");
        render_window_diff(&failure.left, &failure.right, &mut out);
        out.push_str("```\n\n");
    }
    out
}

/// Write one file's pass texts and failure sides into `dump_dir`. Pass texts
/// are written when their check failed, or always under `--dump-passes`. The
/// `{stem}.idempotency.{input,once,twice}.txt` names identify each formatting pass.
fn write_debug_artifacts(
    dump_dir: &Path,
    stem: &str,
    artifacts: &DebugArtifacts,
    dump_passes: bool,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dump_dir)?;
    let failed = |kind: CheckKind| artifacts.failures.iter().any(|f| f.kind == kind);

    if let Some((input, parsed)) = artifacts.losslessness.as_ref()
        && (dump_passes || failed(CheckKind::Losslessness))
    {
        std::fs::write(
            dump_dir.join(format!("{stem}.losslessness.input.txt")),
            input,
        )?;
        std::fs::write(
            dump_dir.join(format!("{stem}.losslessness.parsed.txt")),
            parsed,
        )?;
    }

    if let Some((input, once, twice)) = artifacts.idempotency.as_ref()
        && (dump_passes || failed(CheckKind::Idempotency))
    {
        std::fs::write(
            dump_dir.join(format!("{stem}.idempotency.input.txt")),
            input,
        )?;
        std::fs::write(dump_dir.join(format!("{stem}.idempotency.once.txt")), once)?;
        std::fs::write(
            dump_dir.join(format!("{stem}.idempotency.twice.txt")),
            twice,
        )?;
    }

    // One filename for both trivia checks: only one of them runs per
    // invocation, so the reproducer is never ambiguous.
    if let Some(perturbed) = artifacts.trivia_perturbed.as_ref()
        && (failed(CheckKind::Trivia) || failed(CheckKind::TriviaStrict))
    {
        std::fs::write(
            dump_dir.join(format!("{stem}.trivia.perturbed-input.txt")),
            perturbed,
        )?;
    }

    for failure in &artifacts.failures {
        let kind = failure.kind.label();
        std::fs::write(
            dump_dir.join(format!("{stem}.{kind}.left.txt")),
            &failure.left,
        )?;
        std::fs::write(
            dump_dir.join(format!("{stem}.{kind}.right.txt")),
            &failure.right,
        )?;
    }

    Ok(())
}

/// Run the selected checks over one file's content.
///
/// Losslessness parses under the file's own lex config (like
/// [`debug_assert_fixes_preserved`]) so `.sty`/`.dtx` are checked under their
/// real catcode regime, and compares the CST's text to the input.
///
/// Idempotency formats twice through the same pipeline `tex-ls format` uses.
/// A first-pass [`FormatError`] is a `format-error` finding (the invariant
/// could not be evaluated); a second-pass error on the first pass's own output
/// *is* a fixed-point violation and is reported as `idempotency`.
// One parameter per formatting axis the real `format` path carries, so the
// oracles run through exactly the pipeline they are checking.
#[allow(clippy::too_many_arguments)]
fn run_debug_checks_for_file(
    path: &Path,
    kind: FileKind,
    content: &str,
    style: FormatStyle,
    wrap_override: Option<WrapMode>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
    checks: DebugChecksArg,
) -> DebugArtifacts {
    let mut artifacts = DebugArtifacts::default();

    if matches!(checks, DebugChecksArg::Losslessness | DebugChecksArg::All) {
        let reconstructed = match kind {
            FileKind::Bib => tex_ls_analysis::bib::parse(content).syntax().to_string(),
            _ => parse_with_flavor(content, kind.lex_config())
                .syntax()
                .to_string(),
        };
        artifacts.losslessness = Some((content.to_string(), reconstructed.clone()));
        if reconstructed != content {
            artifacts.failures.push(DebugFailure {
                class: None,
                kind: CheckKind::Losslessness,
                left: content.to_string(),
                right: reconstructed,
                detail: None,
            });
        }
    }

    if matches!(checks, DebugChecksArg::Idempotency | DebugChecksArg::All) {
        let mut style = style;
        style.wrap = wrap_override.unwrap_or_default();
        let fmt = |input: &str| match kind {
            FileKind::Bib => {
                tex_ls_analysis::bib::format_with_style(input, style).map_err(|e| e.to_string())
            }
            _ => format_file_with_packages_sentence(
                input,
                path,
                style,
                kind.lex_config(),
                sentence,
                declared,
            )
            .map_err(|e| e.to_string()),
        };
        match fmt(content) {
            Err(msg) => artifacts.failures.push(DebugFailure {
                class: None,
                kind: CheckKind::FormatError,
                left: msg,
                right: String::new(),
                detail: None,
            }),
            Ok(once) => {
                // Whitespace-only: the formatter changes only trivia, never a
                // non-trivia token (tenet 1). Checked here, not just in the
                // trivia oracle, because content corruption needs no
                // perturbation to reproduce — `\emph{a [b] c}` dropped its `]`
                // at default settings while this gate reported the file clean.
                // `.bib` is skipped: the comparison is LaTeX-CST-based.
                if kind != FileKind::Bib {
                    let config = kind.lex_config();
                    let before = nontrivia_content(&format!("{content}\n"), config);
                    let after = nontrivia_content(&once, config);
                    if before != after {
                        artifacts.failures.push(DebugFailure {
                            class: None,
                            kind: CheckKind::ContentChange,
                            left: before,
                            right: after,
                            detail: None,
                        });
                    }
                    // Comments are a protected region: the formatter picks the line a
                    // `%` lands on, never whether it exists. Invisible to the check
                    // above, since a comment is trivia to the CST — which is how a
                    // lowering that walked a node's *expected* children silently
                    // deleted a `DOC_COMMENT` the grammar had bound into it. Its own
                    // kind so reports distinguish comment loss from content changes.
                    let before = comment_sequence(&format!("{content}\n"), config);
                    let after = comment_sequence(&once, config);
                    if before != after {
                        artifacts.failures.push(DebugFailure {
                            class: None,
                            kind: CheckKind::CommentChange,
                            left: before,
                            right: after,
                            detail: None,
                        });
                    }
                }
                match fmt(&once) {
                    Ok(twice) => {
                        artifacts.idempotency =
                            Some((content.to_string(), once.clone(), twice.clone()));
                        if once != twice {
                            artifacts.failures.push(DebugFailure {
                                class: None,
                                kind: CheckKind::Idempotency,
                                left: once,
                                right: twice,
                                detail: None,
                            });
                        }
                    }
                    Err(msg) => {
                        artifacts.idempotency =
                            Some((content.to_string(), once.clone(), String::new()));
                        artifacts.failures.push(DebugFailure {
                            class: None,
                            kind: CheckKind::Idempotency,
                            left: once,
                            right: format!("second pass failed to format: {msg}"),
                            detail: None,
                        });
                    }
                }
            }
        }
    }

    // The trivia-convergence oracle (opt-in): every TeX-identical
    // newline<->space perturbation must format to a fixed point upholding the
    // invariants — the perturbations synthesize the trivia configurations a
    // hybrid needs, so no corpus file has to land on the right column
    // arithmetic. Wrap is pinned to `reflow` regardless of `--wrap` or the
    // file kind's default (`Preserve` reproduces authored breaks verbatim, so
    // it converges trivially and stresses nothing), and `.bib` files are
    // skipped (the oracle is LaTeX-CST-based). A refusal to format the
    // original is a `format-error` finding, mirroring the idempotency check's
    // first pass.
    if checks == DebugChecksArg::Trivia && kind != FileKind::Bib {
        let mut style = style;
        style.wrap = WrapMode::Reflow;
        let fmt = |input: &str| {
            format_file_with_packages_sentence(
                input,
                path,
                style,
                kind.lex_config(),
                sentence,
                declared,
            )
            .map_err(|e| e.to_string())
        };
        match check_trivia_convergence(content, kind.lex_config(), DEFAULT_SINGLE_FLIP_SAMPLES, fmt)
        {
            Ok(_) => {}
            Err(ConvergenceError::Original(msg)) => artifacts.failures.push(DebugFailure {
                class: None,
                kind: CheckKind::FormatError,
                left: msg,
                right: String::new(),
                detail: None,
            }),
            Err(ConvergenceError::Violation(failure)) => {
                artifacts.trivia_perturbed = Some(failure.perturbed_input);
                artifacts.failures.push(DebugFailure {
                    class: Some(failure.class),
                    kind: CheckKind::Trivia,
                    left: failure.once,
                    right: failure.twice,
                    detail: Some(format!("{}, {}", failure.label, failure.reason)),
                });
            }
        }
    }

    // The strict trivia-invariance oracle (opt-in): `fmt(perturbed) ==
    // fmt(original)`. This is the end-state contract, so it still fails
    // wherever the formatter deliberately preserves an authored break — it is
    // a *survey*, not a gate, and exists because a layout decision that reads
    // the lone-newline predicate is self-consistent on both spellings and so
    // invisible to every other check here. Same wrap pin and `.bib` skip as the
    // convergence check above, and the same `format-error` mapping when the
    // original will not format.
    if checks == DebugChecksArg::TriviaStrict && kind != FileKind::Bib {
        let mut style = style;
        style.wrap = WrapMode::Reflow;
        let fmt = |input: &str| {
            format_file_with_packages_sentence(
                input,
                path,
                style,
                kind.lex_config(),
                sentence,
                declared,
            )
            .map_err(|e| e.to_string())
        };
        match survey_trivia_invariance(content, kind.lex_config(), DEFAULT_SINGLE_FLIP_SAMPLES, fmt)
        {
            Err(msg) => artifacts.failures.push(DebugFailure {
                class: None,
                kind: CheckKind::FormatError,
                left: msg,
                right: String::new(),
                detail: None,
            }),
            Ok(survey) => {
                // Report the localized reproducer when there is one: the bulk
                // variants come first and are whole-file mega-lines, which name
                // no construct. The count rides in `detail` so a `--report` run
                // over a corpus ranks itself.
                if let Some(failure) = survey.best_reproducer() {
                    artifacts.trivia_perturbed = Some(failure.perturbed_input.clone());
                    artifacts.failures.push(DebugFailure {
                        class: None,
                        kind: CheckKind::TriviaStrict,
                        left: failure.formatted_original.clone(),
                        right: failure.formatted_perturbed.clone(),
                        detail: Some(format!(
                            "{}/{} variants diverged, reported: {}",
                            survey.violations.len(),
                            survey.variants_checked,
                            failure.label
                        )),
                    });
                }
            }
        }
    }

    artifacts
}

/// `tex-ls debug format`: check invariants over the discovered files, writing
/// nothing back. Exit 0 on success, 1 for invariant findings, and 2 for IO or
/// discovery errors.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_debug_format(
    paths: &[PathBuf],
    checks: DebugChecksArg,
    report: bool,
    report_json: bool,
    dump_dir: Option<&Path>,
    dump_passes: bool,
    style: FormatStyle,
    wrap_override: Option<WrapMode>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
    exclude: &ExcludeFilter,
) -> ExitCode {
    if paths.is_empty() {
        eprintln!("tex-ls: debug format requires at least one file or directory");
        return ExitCode::from(2);
    }
    let files = match collect_lint_files(paths, exclude) {
        Ok(files) => files,
        Err(err) => {
            report_discovery_error(&err);
            return ExitCode::from(2);
        }
    };
    if files.is_empty() {
        if exclude.force() {
            return ExitCode::SUCCESS;
        }
        eprintln!(
            "tex-ls: no .tex, .sty, .cls, .dtx, .ins, or .bib files found under the provided input paths"
        );
        return ExitCode::from(2);
    }

    // Checks are pure functions of the file content, so they parallelize like
    // `run_format_paths`; the order-preserving collect keeps output and report
    // numbering deterministic.
    let outcomes: Vec<(String, FileKind, Result<DebugArtifacts, String>)> = files
        .par_iter()
        .map(|(path, kind)| {
            let label = path.display().to_string();
            let outcome = match std::fs::read_to_string(path) {
                Ok(content) => Ok(run_debug_checks_for_file(
                    path,
                    *kind,
                    &content,
                    style,
                    wrap_override,
                    sentence,
                    declared,
                    checks,
                )),
                Err(err) => Err(format!("tex-ls: cannot read {label}: {err}")),
            };
            (label, *kind, outcome)
        })
        .collect();

    let mut files_checked = 0usize;
    let mut files_skipped = 0usize;
    let mut io_failed = false;
    let mut collected: Vec<(String, DebugFailure)> = Vec::new();
    for (label, kind, outcome) in outcomes {
        match outcome {
            Err(msg) => {
                eprintln!("{msg}");
                io_failed = true;
            }
            Ok(artifacts) => {
                // A `.bib` file under either trivia check runs nothing (the
                // oracle is LaTeX-CST-based): count it as skipped, not
                // checked, so the summary reports real oracle coverage.
                if matches!(
                    checks,
                    DebugChecksArg::Trivia | DebugChecksArg::TriviaStrict
                ) && kind == FileKind::Bib
                {
                    files_skipped += 1;
                } else {
                    files_checked += 1;
                }
                if let Some(dir) = dump_dir {
                    let stem = sanitize_path_for_filename(&label);
                    if let Err(err) = write_debug_artifacts(dir, &stem, &artifacts, dump_passes) {
                        eprintln!(
                            "tex-ls: cannot write debug artifacts to {}: {err}",
                            dir.display()
                        );
                        io_failed = true;
                    }
                }
                for failure in artifacts.failures {
                    if !report && !report_json {
                        match &failure.detail {
                            Some(detail) => eprintln!(
                                "Debug check failed ({}: {detail}) in {label}",
                                failure.kind.label()
                            ),
                            None => eprintln!(
                                "Debug check failed ({}) in {label}",
                                failure.kind.label()
                            ),
                        }
                        if failure.kind == CheckKind::FormatError {
                            eprintln!("  {}", failure.left);
                        }
                    }
                    collected.push((label.clone(), failure));
                }
            }
        }
    }

    if report_json {
        let failures: Vec<_> = collected
            .iter()
            .map(|(path, failure)| {
                serde_json::json!({
                    "path": path, "kind": failure.kind.label(),
                    "class": failure.class.unwrap_or(failure.kind.label()),
                    "detail": failure.detail, "left": failure.left, "right": failure.right,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1, "checks": checks_label(checks),
                "files_checked": files_checked, "files_skipped": files_skipped,
                "execution_failed": io_failed, "failure_count": failures.len(), "failures": failures,
            })
        );
    } else if report {
        print!(
            "{}",
            build_debug_report(checks, files_checked, files_skipped, &collected)
        );
    } else if collected.is_empty() && !io_failed {
        if files_skipped > 0 {
            println!(
                "All checks passed (checks: {}, files: {files_checked}, skipped: {files_skipped})",
                checks_label(checks)
            );
        } else {
            println!(
                "All checks passed (checks: {}, files: {files_checked})",
                checks_label(checks)
            );
        }
    }
    if io_failed {
        ExitCode::from(2)
    } else if collected.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sanitize_matches_the_workflow_sed_class() {
        assert_eq!(
            sanitize_path_for_filename("sub dir/a b.tex"),
            "sub_dir_a_b.tex"
        );
        assert_eq!(sanitize_path_for_filename("ok-1.2_x.bib"), "ok-1.2_x.bib");
        // Non-ASCII maps per *character* (`å`, `/`, `ü` → three `_`). GNU sed in
        // a UTF-8 locale may instead keep non-ASCII alnums; the workflow's
        // artifact lookups are existence-guarded, so that divergence only
        // drops the artifact link for such paths.
        assert_eq!(sanitize_path_for_filename("å/ü.tex"), "___.tex");
    }

    #[test]
    fn first_diff_line_finds_the_first_mismatch() {
        assert_eq!(first_diff_line("a\nb\nc\n", "a\nB\nc\n"), 2);
        assert_eq!(first_diff_line("a\n", "a\nb\n"), 2);
        assert_eq!(first_diff_line("x", "y"), 1);
        // Difference past the last visible line (trailing newline material).
        assert_eq!(first_diff_line("a\nb", "a\nb\n\n"), 3);
    }

    #[test]
    fn report_carries_the_ci_contract_strings() {
        let failures = vec![(
            "sub/file.tex".to_string(),
            DebugFailure {
                class: None,
                kind: CheckKind::Idempotency,
                left: "a\nb\nc\n".to_string(),
                right: "a\nB\nc\n".to_string(),
                detail: None,
            },
        )];
        let report = build_debug_report(DebugChecksArg::All, 3, 0, &failures);
        assert!(report.contains("# Debug-format regression report"));
        assert!(report.contains("- Files checked: 3"));
        assert!(report.contains("### 1. `sub/file.tex` (idempotency)"));
        assert!(report.contains("- Approx. diff start line: 2"));
        assert!(report.contains("```diff\n a\n-b\n-c\n+B\n+c\n```"));
    }

    #[test]
    fn report_on_all_passing_files_has_no_failure_sections() {
        let report = build_debug_report(DebugChecksArg::All, 2, 0, &[]);
        assert!(report.contains("- Failures: 0"));
        assert!(report.contains("All checks passed."));
        assert!(!report.contains("## Failures"));
    }

    #[test]
    fn format_error_report_entry_avoids_the_invariant_substrings() {
        let failures = vec![(
            "bad.tex".to_string(),
            DebugFailure {
                class: None,
                kind: CheckKind::FormatError,
                left:
                    "input contains 1 parser diagnostic(s); formatter only supports parseable input"
                        .to_string(),
                right: String::new(),
                detail: None,
            },
        )];
        let report = build_debug_report(DebugChecksArg::Idempotency, 1, 0, &failures);
        assert!(report.contains("### 1. `bad.tex` (format-error)"));
        let lower = report.to_lowercase();
        // `- Checks: `idempotency`` is the run configuration, not a failure
        // label; strip the header before asserting the classification
        // guarantee on the failure section.
        let failure_section = &lower[lower.find("## failures").unwrap()..];
        assert!(!failure_section.contains("idempot"));
        assert!(!failure_section.contains("lossless"));
    }
}
