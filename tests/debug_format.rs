//! CLI-level tests for `tex-ls debug format`, the per-file invariant checker.
//! These exercise failure labels, report contents and dump paths through the
//! real executable. `--no-config` isolates cases from local configuration.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn tex_ls(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tex-ls"))
        .arg("--no-config")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run tex-ls")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn passing_file_exits_zero() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "Hello \\emph{world}.\n").unwrap();

    let output = tex_ls(
        dir.path(),
        &["debug", "format", "--checks", "all", "ok.tex"],
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("All checks passed (checks: all, files: 1)"));
}

#[test]
fn report_on_passing_file_has_no_failure_headings() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "Hello world.\n").unwrap();

    let output = tex_ls(dir.path(), &["debug", "format", "--report", "ok.tex"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("# Debug-format regression report"));
    assert!(report.contains("All checks passed."));
    assert!(!report.contains("(idempotency)"));
    assert!(!report.contains("(losslessness)"));
}

#[test]
fn unreadable_text_is_an_execution_error_even_with_a_report() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("invalid.tex"), [0xff]).unwrap();
    let output = tex_ls(dir.path(), &["debug", "format", "--report", "invalid.tex"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("cannot read"));
}

#[test]
fn artifact_write_failure_is_an_execution_error() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "Hello.\n").unwrap();
    std::fs::write(dir.path().join("blocked"), "not a directory").unwrap();
    let output = tex_ls(
        dir.path(),
        &[
            "debug",
            "format",
            "--report",
            "--dump-passes",
            "--dump-dir",
            "blocked",
            "ok.tex",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("cannot write debug artifacts"));
}

#[test]
fn dump_passes_writes_sanitized_artifact_names() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("sub dir")).unwrap();
    let source = "Hello \\emph{world}.\n";
    std::fs::write(dir.path().join("sub dir/a b.tex"), source).unwrap();

    let output = tex_ls(
        dir.path(),
        &[
            "debug",
            "format",
            "--dump-dir",
            "dumps",
            "--dump-passes",
            "sub dir/a b.tex",
        ],
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    // The stem is the path as passed, sanitized exactly like the workflow's
    // `sed 's/[^[:alnum:]._-]/_/g'` — this is the artifact-lookup contract.
    let dumps = dir.path().join("dumps");
    let input = std::fs::read_to_string(dumps.join("sub_dir_a_b.tex.idempotency.input.txt"))
        .expect("input dump exists");
    let once = std::fs::read_to_string(dumps.join("sub_dir_a_b.tex.idempotency.once.txt"))
        .expect("once dump exists");
    let twice = std::fs::read_to_string(dumps.join("sub_dir_a_b.tex.idempotency.twice.txt"))
        .expect("twice dump exists");
    assert_eq!(input, source);
    assert_eq!(once, twice);
    assert!(
        dumps
            .join("sub_dir_a_b.tex.losslessness.input.txt")
            .exists()
    );
    assert!(
        dumps
            .join("sub_dir_a_b.tex.losslessness.parsed.txt")
            .exists()
    );
}

#[test]
fn dump_passes_requires_dump_dir() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "x\n").unwrap();

    let output = tex_ls(dir.path(), &["debug", "format", "--dump-passes", "ok.tex"]);

    assert_eq!(output.status.code(), Some(2), "clap usage error expected");
}

#[test]
fn trivia_check_passes_on_stable_prose() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "alpha\nbeta gamma delta.\n").unwrap();

    let output = tex_ls(
        dir.path(),
        &["debug", "format", "--checks", "trivia", "ok.tex"],
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("All checks passed (checks: trivia, files: 1)"));
}

#[test]
fn trivia_check_counts_skipped_bib_files_separately() {
    // The trivia oracle is LaTeX-CST-based and runs nothing on a `.bib` file:
    // the summary must report it as skipped, never fold it into the checked
    // count (which would overstate oracle coverage on a `.bib`-heavy sweep).
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "alpha beta.\n").unwrap();
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{key, title = {T}, year = {2020}}\n",
    )
    .unwrap();

    let output = tex_ls(dir.path(), &["debug", "format", "--checks", "trivia", "."]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("All checks passed (checks: trivia, files: 1, skipped: 1)"),
        "stdout: {}",
        stdout(&output)
    );

    let output = tex_ls(
        dir.path(),
        &["debug", "format", "--checks", "trivia", "--report", "."],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("- Files checked: 1"), "report: {report}");
    assert!(report.contains("- Files skipped: 1"), "report: {report}");
}

#[test]
fn trivia_check_accepts_the_previous_hybrid() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hybrid.tex"), HYBRID_TEX).unwrap();
    for checks in ["all", "trivia"] {
        let output = tex_ls(
            dir.path(),
            &[
                "debug",
                "format",
                "--checks",
                checks,
                "--report",
                "hybrid.tex",
            ],
        );
        assert!(output.status.success(), "stderr: {}", stderr(&output));
        assert!(stdout(&output).contains("- Failures: 0"));
    }
}

#[test]
fn trivia_strict_check_fires_where_an_authored_break_is_preserved() {
    let dir = TempDir::new().unwrap();
    // Two *un-signatured* top-level commands separated by an authored newline.
    // The residual command-only-line rule keeps that break, but glues the pair
    // when the same gap is a space — the same bytes to the next parse, so a
    // read of the lone-newline predicate. The read is sanctioned Tier 2
    // (preservation-only; the fixed-point argument is on
    // `line_is_command_only`), but sanctioned or not only the strict survey can
    // see it. Curated block commands (`\usepackage`, …) no longer reach that
    // rule — they are intercepted as block-level statements via
    // `CommandSig::block` — so the probe uses names no signature tier knows,
    // whose block-ness only the authored break can carry.
    //
    // Neither `all` nor `trivia` can see it: both spellings are self-consistent
    // fixed points that round-trip losslessly, which is the whole reason the
    // strict oracle earns a CLI surface.
    std::fs::write(
        dir.path().join("preserved.tex"),
        "\\zzalpha{a}\n\\zzbeta{b}\nalpha\nbeta gamma\n",
    )
    .unwrap();

    let output = tex_ls(
        dir.path(),
        &[
            "debug",
            "format",
            "--checks",
            "trivia-strict",
            "--report",
            "preserved.tex",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let report = stdout(&output);
    assert!(
        report.contains("- Checks: `trivia-strict`"),
        "report: {report}"
    );
    assert!(
        report.contains("### 1. `preserved.tex` (trivia-strict)"),
        "report: {report}"
    );
    // The reported reproducer must be a localized `flip@…` gap, not one of the
    // two whole-file bulk variants that are generated first — a mega-line diff
    // names no construct.
    assert!(
        report.contains("variants diverged, reported: flip@"),
        "report: {report}"
    );

    // Neither other check sees it, and no `trivia-strict` label may leak into
    // `all` — the smoke-test workflow classifies failures by grepping for its
    // own three labels.
    for checks in ["all", "trivia"] {
        let output = tex_ls(
            dir.path(),
            &[
                "debug",
                "format",
                "--checks",
                checks,
                "--report",
                "preserved.tex",
            ],
        );
        assert!(
            output.status.success(),
            "checks={checks} stderr: {}",
            stderr(&output)
        );
        assert!(!stdout(&output).contains("trivia-strict"));
    }
}

#[test]
fn trivia_strict_check_counts_skipped_bib_files_separately() {
    // Same LaTeX-CST-based skip as the convergence check: a `.bib` runs nothing
    // and must be reported as skipped, never folded into the checked count.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.tex"), "alpha beta.\n").unwrap();
    std::fs::write(
        dir.path().join("refs.bib"),
        "@article{key, title = {T}, year = {2020}}\n",
    )
    .unwrap();

    let output = tex_ls(
        dir.path(),
        &["debug", "format", "--checks", "trivia-strict", "."],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("All checks passed (checks: trivia-strict, files: 1, skipped: 1)"),
        "stdout: {}",
        stdout(&output)
    );
}

#[test]
fn dtx_doc_margin_frame_survives_reflow() {
    let dir = TempDir::new().unwrap();
    // A `.dtx` whose documentation prose sits either side of a margin-framed
    // `macrocode` chunk holding expl3 code. Reflow — now the default for every
    // file kind — must not join the `%    \begin{macrocode}` frame line onto the
    // prose above it: the frame's `%` would leave column 0, stop being a comment
    // at package-load time, and the next pass would not parse. Both checks pass.
    std::fs::write(
        dir.path().join("expl.dtx"),
        "% \\section{Implementation}\n\
         %    \\begin{macrocode}\n\
         \\ExplSyntaxOn\n\
         \\tl_new:N \\l_tmpa_tl\n\
         %    \\end{macrocode}\n\
         %\n\
         % Some prose.\n\
         %    \\begin{macrocode}\n\
         \\ExplSyntaxOff\n\
         %    \\end{macrocode}\n",
    )
    .unwrap();

    for checks in ["all", "trivia"] {
        let output = tex_ls(
            dir.path(),
            &["debug", "format", "--checks", checks, "expl.dtx"],
        );
        assert!(
            output.status.success(),
            "--checks {checks} stderr: {}",
            stderr(&output)
        );
    }
}

/// A reduced expl3 fallback regression. Its exact spacing exercises statement
/// boundaries under the bulk trivia perturbations.
const HYBRID_TEX: &str = r#"\ExplSyntaxOn
{
\module_aux:w { \scan_stop: }
\int_value:w \module_pack:wNNNNNNNN \scan_stop: { \module_int_eval:w \scan_stop: }
}
\ExplSyntaxOff
"#;

#[test]
fn line_width_flag_reaches_the_formatter() {
    let dir = TempDir::new().unwrap();
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa\n";
    std::fs::write(dir.path().join("wide.tex"), long).unwrap();

    let output = tex_ls(
        dir.path(),
        &[
            "debug",
            "format",
            "--checks",
            "idempotency",
            "--line-width",
            "30",
            "--dump-dir",
            "dumps",
            "--dump-passes",
            "wide.tex",
        ],
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let once = std::fs::read_to_string(
        dir.path()
            .join("dumps")
            .join("wide.tex.idempotency.once.txt"),
    )
    .expect("once dump exists");
    assert!(
        once.lines().count() > 1 && once.lines().all(|l| l.len() <= 30),
        "expected a wrap at width 30, got: {once:?}"
    );
}

#[test]
fn format_error_never_reads_as_an_invariant_failure() {
    let dir = TempDir::new().unwrap();
    // An unclosed group parses with diagnostics, so the formatter refuses it:
    // a `format-error`, not an idempotency or losslessness regression.
    std::fs::write(dir.path().join("bad.tex"), "a{\n").unwrap();

    let output = tex_ls(dir.path(), &["debug", "format", "bad.tex"]);

    assert_eq!(output.status.code(), Some(1));
    let log = stderr(&output).to_lowercase();
    assert!(log.contains("(format-error)"), "log: {log}");
    assert!(!log.contains("idempot"), "log: {log}");
    assert!(!log.contains("lossless"), "log: {log}");

    let output = tex_ls(dir.path(), &["debug", "format", "--report", "bad.tex"]);
    assert_eq!(output.status.code(), Some(1));
    let report = stdout(&output);
    assert!(report.contains("### 1. `bad.tex` (format-error)"));
    assert!(report.contains("- Files checked: 1"));
}

#[test]
fn json_report_preserves_coverage_and_failure_classification() {
    let dir = TempDir::new().unwrap();
    for (name, text, status, count) in [("ok.tex", "Hello.\n", 0, 0), ("bad.tex", "a{\n", 1, 1)] {
        std::fs::write(dir.path().join(name), text).unwrap();
        let output = tex_ls(
            dir.path(),
            &[
                "debug",
                "format",
                "--checks",
                "trivia",
                "--report-json",
                name,
            ],
        );
        assert_eq!(output.status.code(), Some(status));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["files_checked"], 1);
        assert_eq!(report["files_skipped"], 0);
        assert_eq!(report["execution_failed"], false);
        assert_eq!(report["failure_count"], count);
        assert_eq!(report["failures"].as_array().unwrap().len(), count as usize);
        if count > 0 {
            assert_eq!(report["failures"][0]["path"], name);
            assert_eq!(report["failures"][0]["kind"], "format-error");
            assert_eq!(report["failures"][0]["class"], "format-error");
        }
    }
    std::fs::write(dir.path().join("invalid.tex"), [0xff]).unwrap();
    let output = tex_ls(
        dir.path(),
        &["debug", "format", "--report-json", "invalid.tex"],
    );
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["execution_failed"], true);
    assert_eq!(report["files_checked"], 0);
}
