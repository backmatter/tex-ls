//! Format inputs and render check diffs.
use super::inputs::{
    FORMAT_MISSING_INPUT, Inputs, OutputOptions, color_enabled, inputs_or_exit, is_stdin_path,
    report_discovery_error, usage_error,
};
use clap::error::ErrorKind;
use rayon::prelude::*;
use similar::{Algorithm, ChangeTag, TextDiff};
use std::{
    io::{BufWriter, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};
use tex_ls::{
    file_discovery::{ExcludeFilter, collect_lint_files},
    format::{ChangedFile, check_paths_with_style, format_file_with_packages_sentence},
};
use tex_ls_analysis::source::{FileKind, file_kind_or_tex};
use tex_ls_formatter::formatter::{
    FormatStyle, SentenceOptions, WrapMode, format_with_declarations_sentence,
};
use tex_ls_parser::declarations::ResolvedDeclarations;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_format(
    paths: &[PathBuf],
    check: bool,
    stdin_filepath: Option<&Path>,
    style: FormatStyle,
    wrap_override: Option<WrapMode>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
    exclude: &ExcludeFilter,
    out: OutputOptions,
) -> ExitCode {
    if check {
        // `--check` reports on files it leaves on disk, so stdin is not an input
        // here; `run_check` rejects the empty path list on its own.
        if paths.iter().any(|path| is_stdin_path(path)) {
            usage_error(
                "format",
                ErrorKind::ArgumentConflict,
                "`--check` reports on files and cannot read from stdin",
            );
        }
        return run_check(
            paths,
            style,
            wrap_override,
            sentence,
            declared,
            exclude,
            out,
        );
    }
    match inputs_or_exit(paths, "format", FORMAT_MISSING_INPUT) {
        // Stdin has no directory to walk, so the exclude filter never applies.
        Inputs::Stdin => run_format_stdin(stdin_filepath, style, wrap_override, sentence, declared),
        Inputs::Paths(paths) => {
            run_format_paths(paths, style, wrap_override, sentence, declared, exclude)
        }
    }
}

/// `--check`: report unformatted files, exit code 1 if any.
///
/// The report goes to stdout (only the error path uses stderr) so it can be
/// piped, and each changed file is rendered as a diff — under `--check` nothing
/// is written, so this output is the only account of what would change. That
/// matters most where `--check` is actually used: a CI step log, and a
/// pre-commit hook configured with `args: [--check]`, neither of which has a
/// modified file to inspect afterwards. `--quiet` drops the diffs for callers
/// that want just the file list.
fn run_check(
    paths: &[PathBuf],
    style: FormatStyle,
    wrap_override: Option<WrapMode>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
    exclude: &ExcludeFilter,
    out: OutputOptions,
) -> ExitCode {
    match check_paths_with_style(paths, style, wrap_override, sentence, declared, exclude) {
        Ok(result) => {
            if result.changed_files.is_empty() {
                return ExitCode::SUCCESS;
            }
            if out.quiet {
                for path in result.changed_paths() {
                    println!("would reformat {}", path.display());
                }
            } else {
                let use_color = color_enabled(out.color, std::io::stdout().is_terminal());
                for (idx, file) in result.changed_files.iter().enumerate() {
                    if idx > 0 {
                        println!();
                    }
                    print_diff(file, use_color);
                }
            }
            println!(
                "{} of {} file(s) would be reformatted",
                result.changed_files.len(),
                result.checked_files
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("tex-ls: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Line-diff `original` against `formatted`.
///
/// **Histogram, not the default Myers.** Myers is `O((N+M)*D)`, and `D` is the
/// whole file whenever the formatter relays a document rather than touching a
/// few lines: `phd_dissertation.tex` reflows 27 482 lines into 14 364 with 13.6%
/// line overlap, which cost 2.0 s of a 2.2 s `--check` run. `similar`'s disjoint
/// fast path cannot rescue that — it runs before prefix trimming and bails as
/// soon as the two texts share a first line, which a preamble always does.
/// Histogram anchors on low-frequency lines instead of searching for a minimal
/// edit script, and is deterministic (no wall-clock deadline).
///
/// **Measured over the pinned gate corpora, not a synthetic.** Summed diff cost
/// across the 60 largest real files in `corpora/` plus `benches/documents/`:
/// Myers 2418 ms, Patience 919 ms, Histogram **643 ms**. The gap is in the big
/// documents — on the 36 192-line `paperSS122018arxivv2.tex` the diff costs
/// Myers 7.1 s and Patience 2.8 s while Histogram is free; on
/// `phd_dissertation.tex`, 2124 / 680 / 68 ms.
///
/// **Histogram's cliff is real but small here.** It can lose to Myers on
/// self-similar input — `ltfssaxes.dtx` is 2 ms under Myers and 76 ms under
/// Histogram — so its *worst* single file across that scan is 76 ms, against
/// Patience's 657 ms and Myers' 2023 ms. It therefore wins on the worst case
/// too, which Patience does not.
///
/// Re-measure on representative LaTeX inputs before changing the algorithm.
fn diff_lines<'a>(original: &'a str, formatted: &'a str) -> TextDiff<'a, 'a, str> {
    TextDiff::configure()
        .algorithm(Algorithm::Histogram)
        .diff_lines(original, formatted)
}

/// Print a per-file formatting diff: a `Diff in <path>:<line>:` header
/// followed by context-grouped hunks.
fn print_diff(file: &ChangedFile, use_color: bool) {
    let mut out = BufWriter::new(std::io::stdout().lock());
    let _ = write_diff(&mut out, file, use_color);
}

/// The body of [`print_diff`], generic over the sink so it can be rendered to a
/// buffer in tests. Writes through a single locked handle: stdout is a
/// `LineWriter`, so a `println!` per line was a syscall per line - 37 761 of
/// them on `phd_dissertation.tex`.
fn write_diff<W: Write>(out: &mut W, file: &ChangedFile, use_color: bool) -> std::io::Result<()> {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const RESET: &str = "\x1b[0m";

    let diff = diff_lines(&file.original, &file.formatted);
    // Three lines of context around each changed region, so a single stray
    // space in a long file does not print the whole file.
    for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
        if idx > 0 {
            writeln!(out, "---")?;
        }
        let start = group[0].old_range().start + 1;
        writeln!(out, "Diff in {}:{}:", file.path.display(), start)?;
        for op in group {
            for change in diff.iter_changes(op) {
                let (sign, color) = match change.tag() {
                    ChangeTag::Delete => ("-", RED),
                    ChangeTag::Insert => ("+", GREEN),
                    ChangeTag::Equal => (" ", ""),
                };
                // A final line without a trailing newline must not gain one, so
                // the newline is re-emitted only when the change carried it.
                let value = change.value();
                let newline = value.ends_with('\n');
                let line = value.strip_suffix('\n').unwrap_or(value);
                if use_color && !color.is_empty() {
                    write!(out, "{color}{sign}{line}{RESET}")?;
                } else {
                    write!(out, "{sign}{line}")?;
                }
                if newline {
                    writeln!(out)?;
                }
            }
        }
    }
    Ok(())
}

/// No paths: read stdin, format, write to stdout. The pipeline is chosen from
/// `stdin_filepath`'s extension (`.bib` → BibTeX, else LaTeX); with no name given,
/// stdin stays LaTeX, the long-standing conservative default.
fn run_format_stdin(
    stdin_filepath: Option<&Path>,
    mut style: FormatStyle,
    wrap_override: Option<WrapMode>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
) -> ExitCode {
    let mut input = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("tex-ls: cannot read stdin: {err}");
        return ExitCode::FAILURE;
    }
    let kind = stdin_filepath.map_or(FileKind::Tex, file_kind_or_tex);
    style.wrap = wrap_override.unwrap_or_default();
    let formatted = match kind {
        FileKind::Tex
        | FileKind::CodeTex
        | FileKind::Sty
        | FileKind::Cls
        | FileKind::Dtx
        | FileKind::Ins => {
            format_with_declarations_sentence(&input, style, kind.lex_config(), sentence, declared)
                .map_err(|e| e.to_string())
        }
        FileKind::Bib => {
            tex_ls_analysis::bib::format_with_style(&input, style).map_err(|e| e.to_string())
        }
    };
    match formatted {
        Ok(formatted) => {
            if let Err(err) = std::io::stdout().write_all(formatted.as_bytes()) {
                eprintln!("tex-ls: cannot write stdout: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("tex-ls: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the input paths to `.tex`/`.bib` files and format each in place,
/// writing only files whose content changes. Each file is routed to its own
/// formatter by [`FileKind`].
fn run_format_paths(
    paths: &[PathBuf],
    style: FormatStyle,
    wrap_override: Option<WrapMode>,
    sentence: SentenceOptions<'_>,
    declared: &ResolvedDeclarations,
    exclude: &ExcludeFilter,
) -> ExitCode {
    let files = match collect_lint_files(paths, exclude) {
        Ok(files) => files,
        Err(err) => {
            report_discovery_error(&err);
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        if exclude.force() {
            return ExitCode::SUCCESS;
        }
        eprintln!(
            "tex-ls: no .tex, .sty, .cls, .dtx, .ins, or .bib files found under the provided input paths"
        );
        return ExitCode::FAILURE;
    }

    // Read, format, and write each file in parallel (formatting is a pure function
    // of input plus shipped data, so it is thread-safe; distinct output files never
    // race). Each task returns `Some(message)` on failure; the order-preserving
    // collect lets the serial fold below report errors deterministically.
    let outcomes: Vec<Option<String>> = files
        .par_iter()
        .map(|(path, kind)| {
            let content = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(err) => return Some(format!("tex-ls: cannot read {}: {err}", path.display())),
            };
            let mut style = style;
            style.wrap = wrap_override.unwrap_or_default();
            let formatted = match kind {
                FileKind::Tex
                | FileKind::CodeTex
                | FileKind::Sty
                | FileKind::Cls
                | FileKind::Dtx
                | FileKind::Ins => format_file_with_packages_sentence(
                    &content,
                    path,
                    style,
                    kind.lex_config(),
                    sentence,
                    declared,
                )
                .map_err(|e| e.to_string()),
                FileKind::Bib => tex_ls_analysis::bib::format_with_style(&content, style)
                    .map_err(|e| e.to_string()),
            };
            match formatted {
                Ok(formatted) => {
                    if formatted != *content
                        && let Err(err) = std::fs::write(path, formatted)
                    {
                        return Some(format!("tex-ls: cannot write {}: {err}", path.display()));
                    }
                    None
                }
                Err(msg) => Some(format!("tex-ls: cannot format {}: {msg}", path.display())),
            }
        })
        .collect();

    let mut failed = false;
    for message in outcomes.into_iter().flatten() {
        eprintln!("{message}");
        failed = true;
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The property that outlives any particular diff algorithm: the emitted
    /// change stream must replay both sides exactly. Hunk boundaries are the
    /// algorithm's business (Histogram groups differently from Myers), but
    /// dropping or duplicating a line is a bug in any of them, and `--check`
    /// is the only account a CI log gets of what would change.
    fn assert_diff_reconstructs(original: &str, formatted: &str) {
        let diff = diff_lines(original, formatted);
        let (mut old, mut new) = (String::new(), String::new());
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Delete => old.push_str(change.value()),
                ChangeTag::Insert => new.push_str(change.value()),
                ChangeTag::Equal => {
                    old.push_str(change.value());
                    new.push_str(change.value());
                }
            }
        }
        assert_eq!(
            old, original,
            "delete+equal stream must replay the original"
        );
        assert_eq!(
            new, formatted,
            "insert+equal stream must replay the formatted text"
        );
    }

    #[test]
    fn diff_reconstructs_both_sides() {
        assert_diff_reconstructs("a\nb\nc\n", "a\nB\nc\n");
        // No trailing newline on either side.
        assert_diff_reconstructs("a\nb", "a\nc");
        assert_diff_reconstructs("", "x\n");
        assert_diff_reconstructs("x\n", "");
        // Reindent-everything: the shape that made Myers quadratic.
        let flat: String = "\\begin{itemize}\n".repeat(400);
        let indented: String = "  \\begin{itemize}\n".repeat(400);
        assert_diff_reconstructs(&flat, &indented);
        // Histogram's hard case: no anchor is unique on either side.
        assert_diff_reconstructs(&"same\n".repeat(300), &"other\n".repeat(300));
    }

    #[test]
    fn diff_scales_linearly_when_every_line_changes() {
        // The shape Myers was quadratic on: a reindent-everything change, where
        // the edit distance is the whole file. Lives here rather than in
        // `tests/scaling.rs` because `diff_lines` is a binary-crate item; the
        // rationale for a ratio bound and a best-of-N minimum is in that file's
        // module docs.
        let time = |n: usize| {
            // Shared first and last line on purpose: that is what stops
            // `similar`'s disjoint fast path from rescuing Myers here, exactly
            // as a real file's unchanged preamble line does. The body relays
            // four openers onto each line, the change `{` + N `\begin{itemize}`
            // + `}` actually produces.
            let old = format!("{{\n{}}}\n", "\\begin{itemize}\n".repeat(n));
            let new = format!(
                "{{\n{}}}\n",
                "  \\begin{itemize} \\begin{itemize} \\begin{itemize} \\begin{itemize}\n"
                    .repeat(n / 4)
            );
            (0..5)
                .map(|_| {
                    let start = std::time::Instant::now();
                    let diff = diff_lines(&old, &new);
                    std::hint::black_box(diff.grouped_ops(3).len());
                    start.elapsed()
                })
                .min()
                .unwrap()
        };
        // Keep the smaller side above tens of milliseconds; shorter samples
        // were swamped by concurrent test work on macOS ARM runners.
        let (small, large) = (time(16000), time(32000));
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(f64::EPSILON);
        assert!(
            ratio < 3.0,
            "doubling the changed-line count took {ratio:.2}x ({small:?} -> {large:?})",
        );
    }

    #[test]
    fn diff_renders_a_header_and_signed_lines() {
        let file = ChangedFile {
            path: PathBuf::from("doc.tex"),
            original: "a\nb\nc\n".to_owned(),
            formatted: "a\nB\nc\n".to_owned(),
        };
        let mut out = Vec::new();
        write_diff(&mut out, &file, false).unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert_eq!(rendered, "Diff in doc.tex:1:\n a\n-b\n+B\n c\n");
    }
}
