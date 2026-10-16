//! Shared input selection, usage errors, and output policy.
use clap::{CommandFactory, error::ErrorKind};
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
};
use tex_ls::{
    cli::{Cli, ColorChoice},
    file_discovery::FileDiscoveryError,
};

/// The global output flags (`--color`, `--quiet`), threaded to the commands that
/// write human-facing output.
#[derive(Debug, Clone, Copy)]
pub(super) struct OutputOptions {
    pub(super) color: ColorChoice,
    pub(super) quiet: bool,
}

/// Where a subcommand's positional arguments say to read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Inputs<'a> {
    /// Read the one buffer on stdin.
    Stdin,
    /// Read the named files and directories (never empty).
    Paths(&'a [PathBuf]),
}

/// A positional-argument mistake, rendered as a clap usage error by
/// [`inputs_or_exit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputError {
    /// `-` was mixed with file paths; there is no sane order to read them in.
    StdinWithPaths,
    /// Nothing to read: no paths, and stdin is an interactive terminal.
    NoInput,
}

/// `-` is the conventional spelling for "the stdin buffer" (black, ruff), and
/// is never a real file we would format.
pub(super) fn is_stdin_path(path: &Path) -> bool {
    path.as_os_str() == "-"
}

/// Decide what the positional paths point at.
///
/// Bare `tex-ls format` reads stdin, matching rustfmt/gofmt/clang-format. That
/// default is right behind a pipe and a trap at a prompt: a new user runs it
/// expecting the current directory to be formatted and instead sees the process
/// hang on a terminal it is quietly reading (issue #111). So no paths *and* an
/// interactive stdin is a usage error instead.
///
/// The terminal check is the whole gate, and it is deliberately narrow: it can
/// only fire where a human is typing, never behind a pipe, a redirect, a
/// heredoc, or a CI runner, so no scripted invocation changes behavior. Taking
/// `stdin_is_terminal` as an argument (as [`color_enabled`] does) keeps the
/// decision testable without a pty.
fn resolve_inputs(paths: &[PathBuf], stdin_is_terminal: bool) -> Result<Inputs<'_>, InputError> {
    if paths.iter().any(|path| is_stdin_path(path)) {
        if paths.len() > 1 {
            return Err(InputError::StdinWithPaths);
        }
        return Ok(Inputs::Stdin);
    }
    if paths.is_empty() {
        if stdin_is_terminal {
            return Err(InputError::NoInput);
        }
        return Ok(Inputs::Stdin);
    }
    Ok(Inputs::Paths(paths))
}

/// Exit with a clap-rendered usage error for `subcommand` (its own `Usage:`
/// line, the `--help` pointer, and clap's exit code 2), so an argument mistake
/// we diagnose ourselves is spelled exactly like one clap caught.
pub(super) fn usage_error(subcommand: &str, kind: ErrorKind, message: &str) -> ! {
    let mut cli = Cli::command();
    let Some(sub) = cli.find_subcommand_mut(subcommand) else {
        Cli::command().error(kind, message).exit()
    };
    // Fetched off a freshly built `Command`, the subcommand has no bin name yet,
    // so name it or the usage line reads `Usage: format …`.
    sub.clone()
        .bin_name(format!("tex-ls {subcommand}"))
        .error(kind, message)
        .exit()
}

/// The "nothing to read" usage errors, one per subcommand. Each names `-` so the
/// way out of the trap in issue #111 rides in the message that reports it.
pub(super) const FORMAT_MISSING_INPUT: &str =
    "no input paths; pass files or directories to format, or `-` to read from stdin";
pub(super) const LINT_MISSING_INPUT: &str =
    "no input paths; pass files or directories to lint, or `-` to read from stdin";
pub(super) const PARSE_MISSING_INPUT: &str =
    "no input path; pass a file to parse, or `-` to read from stdin";

/// [`resolve_inputs`] against the real stdin, exiting on a usage mistake.
/// `missing` spells the "nothing to read" case in the subcommand's own terms.
pub(super) fn inputs_or_exit<'a>(
    paths: &'a [PathBuf],
    subcommand: &str,
    missing: &str,
) -> Inputs<'a> {
    match resolve_inputs(paths, std::io::stdin().is_terminal()) {
        Ok(inputs) => inputs,
        Err(InputError::StdinWithPaths) => usage_error(
            subcommand,
            ErrorKind::ArgumentConflict,
            "`-` reads from stdin and cannot be combined with other paths",
        ),
        Err(InputError::NoInput) => {
            usage_error(subcommand, ErrorKind::MissingRequiredArgument, missing)
        }
    }
}

/// Resolve `--color` against the destination stream. `Auto` honors `NO_COLOR`
/// (any value, per no-color.org) and requires a terminal, so redirected output
/// and CI logs stay plain unless `--color always` is passed.
pub(super) fn color_enabled(choice: ColorChoice, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && is_terminal,
    }
}

/// Print a file-discovery error to stderr, prefixed like the other CLI errors.
pub(super) fn report_discovery_error(err: &FileDiscoveryError) {
    match err {
        FileDiscoveryError::UnsupportedLintFilePath { path } => {
            eprintln!(
                "tex-ls: input file {} is not a .tex, .sty, .cls, .dtx, .ins, or .bib file",
                path.display()
            );
        }
        FileDiscoveryError::WalkError { path, message } => {
            eprintln!(
                "tex-ls: failed while scanning {}: {message}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_choice_overrides_ignore_the_terminal() {
        assert!(color_enabled(ColorChoice::Always, false));
        assert!(!color_enabled(ColorChoice::Never, true));
    }

    fn paths(entries: &[&str]) -> Vec<PathBuf> {
        entries.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn no_paths_reads_stdin_unless_it_is_a_terminal() {
        // Behind a pipe, a redirect, or a CI runner, bare `tex-ls format` keeps
        // reading stdin exactly as rustfmt/gofmt do — the gate cannot reach any
        // scripted invocation.
        assert_eq!(resolve_inputs(&[], false), Ok(Inputs::Stdin));
        // At a prompt there is nothing to read, so it is a usage error instead of
        // a silent wait on the terminal (issue #111).
        assert_eq!(resolve_inputs(&[], true), Err(InputError::NoInput));
    }

    #[test]
    fn dash_names_stdin_even_at_a_terminal() {
        // The explicit spelling is how you still hand-type a buffer once the
        // implicit one is gated.
        let dash = paths(&["-"]);
        assert_eq!(resolve_inputs(&dash, true), Ok(Inputs::Stdin));
        assert_eq!(resolve_inputs(&dash, false), Ok(Inputs::Stdin));
    }

    #[test]
    fn dash_cannot_be_mixed_with_file_paths() {
        // There is no sane order to interleave a buffer with named files in, so
        // the mix is rejected rather than resolved.
        assert_eq!(
            resolve_inputs(&paths(&["-", "a.tex"]), false),
            Err(InputError::StdinWithPaths)
        );
        assert_eq!(
            resolve_inputs(&paths(&["a.tex", "-"]), false),
            Err(InputError::StdinWithPaths)
        );
    }

    #[test]
    fn named_paths_are_passed_through() {
        let named = paths(&["a.tex", "chapters"]);
        assert_eq!(resolve_inputs(&named, true), Ok(Inputs::Paths(&named)));
    }

    #[test]
    fn color_auto_is_off_when_not_a_terminal() {
        // Env-independent: a redirected stream (a CI step log, a pipe) is plain
        // whatever `NO_COLOR` says, which is what keeps the `--check` diff
        // readable in captured output.
        assert!(!color_enabled(ColorChoice::Auto, false));
    }
}
