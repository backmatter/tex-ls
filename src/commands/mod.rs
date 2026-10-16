//! Native CLI command dispatch.
mod config;
mod debug;
mod format;
mod inputs;
mod lint;
mod parse;

use config::{cwd_anchor, load_config, resolve_config, resolve_style, run_init};
use debug::{run_debug_echo_args, run_debug_format};
use format::run_format;
use inputs::OutputOptions;
use lint::{lint_output_mode, run_explain, run_lint};
use parse::run_parse;
use std::{path::Path, process::ExitCode};
use tex_ls::cli::{Cli, Command, DebugCommand};
use tex_ls_analysis::linter::RuleSelection;
use tex_ls_formatter::formatter::SentenceOptions;

pub(super) fn run(cli: Cli) -> ExitCode {
    let Cli {
        command,
        config: config_arg,
        no_config,
        color,
        quiet,
    } = cli;
    let out = OutputOptions { color, quiet };
    match command {
        Command::Format {
            paths,
            check,
            stdin_filepath,
            line_width,
            indent_width,
            item_indent,
            wrap,
            math_wrap,
            line_ending,
            exclude,
            force_exclude,
        } => {
            // Discover/load `tex-ls.toml` from the working directory (one config
            // per invocation), falling back to the global user config. The exclude
            // filter is rooted at the config's directory so its patterns resolve
            // relative to it.
            let (config, exclude_filter) =
                match load_config(config_arg.as_deref(), no_config, &exclude, force_exclude) {
                    Ok(resolved) => resolved,
                    Err(code) => return code,
                };

            let (style, wrap_override) = resolve_style(
                &config,
                line_width,
                indent_width,
                item_indent,
                wrap,
                math_wrap,
                line_ending,
            );
            // The `sentence`/`semantic` language profile, resolved once from
            // `[format] lang` + `[format.no-break-abbreviations]`; `scratch` owns the
            // merged entries for the whole format run. Ignored by other wrap modes.
            let mut abbrev_scratch = Vec::new();
            let sentence = SentenceOptions::resolve(
                config.format.lang.as_deref(),
                &config.format.no_break_abbreviations,
                &mut abbrev_scratch,
            );
            run_format(
                &paths,
                check,
                stdin_filepath.as_deref(),
                style,
                wrap_override,
                sentence,
                config.resolved_declarations(),
                &exclude_filter,
                out,
            )
        }
        Command::Lint {
            paths,
            fix,
            unsafe_fixes,
            stdin_filepath,
            exclude,
            force_exclude,
            select,
            ignore,
            explain,
            output,
        } => {
            if let Some(rule) = explain {
                return run_explain(&rule);
            }
            let (config, exclude_filter) =
                match load_config(config_arg.as_deref(), no_config, &exclude, force_exclude) {
                    Ok(resolved) => resolved,
                    Err(code) => return code,
                };
            // CLI `--select`/`--ignore` override the configured selection when given.
            let mut lint = config.lint.clone();
            if !select.is_empty() {
                lint.select = Some(select);
            }
            if !ignore.is_empty() {
                lint.ignore = ignore;
            }
            let (rules, unknown) = RuleSelection::resolve(lint.select.as_deref(), &lint.ignore);
            for id in &unknown {
                eprintln!("tex-ls: warning: unknown lint rule `{id}`");
            }
            run_lint(
                &paths,
                fix,
                unsafe_fixes,
                stdin_filepath.as_deref(),
                &exclude_filter,
                &rules,
                config.resolved_declarations(),
                lint_output_mode(output),
                out.color,
            )
        }
        Command::Parse { path } => {
            // The dumped tree must be the tree the formatter and linter see, so
            // `parse` resolves the project's declarations exactly as they do —
            // including `--config`/`--no-config`, which are global flags clap
            // accepts here whether or not this arm reads them. Ignoring them
            // would make `tex-ls parse --no-config` dump a tree no other
            // subcommand would produce, which is the debugging trap threading
            // declarations through this command was meant to close.
            let anchor = match cwd_anchor() {
                Ok(anchor) => anchor,
                Err(code) => return code,
            };
            let (config, _) = match resolve_config(config_arg.as_deref(), no_config, &anchor) {
                Ok(resolved) => resolved,
                Err(code) => return code,
            };
            run_parse(path.as_slice(), config.resolved_declarations())
        }
        Command::Lsp => run_lsp(),
        Command::InverseSearch {
            input,
            line1,
            line0,
            character,
            ipc_dir,
        } => run_inverse_search(&input, line1, line0, character, ipc_dir.as_deref()),
        Command::Init { force } => run_init(force),
        Command::Debug { command } => match command {
            DebugCommand::Format {
                paths,
                checks,
                line_width,
                wrap,
                report,
                report_json,
                dump_dir,
                dump_passes,
                exclude,
                force_exclude,
            } => {
                let (config, exclude_filter) =
                    match load_config(config_arg.as_deref(), no_config, &exclude, force_exclude) {
                        Ok(resolved) => resolved,
                        Err(code) => return code,
                    };
                let (style, wrap_override) =
                    resolve_style(&config, line_width, None, None, wrap, None, None);
                let mut abbrev_scratch = Vec::new();
                let sentence = SentenceOptions::resolve(
                    config.format.lang.as_deref(),
                    &config.format.no_break_abbreviations,
                    &mut abbrev_scratch,
                );
                run_debug_format(
                    &paths,
                    checks,
                    report,
                    report_json,
                    dump_dir.as_deref(),
                    dump_passes,
                    style,
                    wrap_override,
                    sentence,
                    config.resolved_declarations(),
                    &exclude_filter,
                )
            }
            DebugCommand::EchoArgs { out, args } => run_debug_echo_args(&out, &args),
        },
    }
}

/// `tex-ls inverse-search`: hand a viewer's source position to a running
/// language server.
///
/// The path is canonicalized first: a viewer's `%f` is often relative to the
/// compile directory, or reached through a symlink, and the server matches
/// against the paths its editor opened.
///
/// Exits `2` — the CLI's usage/environment code — with a message naming the
/// likely cause, rather than texlab's `-1` (which reaches the shell as 255 and
/// says nothing). The viewer shows this to the user, so it has to be readable.
fn run_inverse_search(
    input: &Path,
    line1: Option<u32>,
    line0: Option<u32>,
    character: u32,
    ipc_dir: Option<&Path>,
) -> ExitCode {
    let Some(line) = line1.or_else(|| line0.and_then(|line| line.checked_add(1))) else {
        eprintln!(
            "tex-ls: pass --line1 (counting from 1, what most viewers emit) \
             or --line0 (counting from 0, below 4294967295)"
        );
        return ExitCode::from(2);
    };
    let path = input.canonicalize().unwrap_or_else(|_| input.to_path_buf());
    let dir = ipc_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(tex_ls::ipc::ipc_dir);
    match tex_ls::ipc::send_inverse_search_in(&dir, &path, line, character) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("tex-ls: {err}");
            ExitCode::from(2)
        }
    }
}

/// Run the language server, mapping a startup failure to a non-zero exit.
fn run_lsp() -> ExitCode {
    match tex_ls::lsp::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("tex-ls: language server error: {err}");
            ExitCode::FAILURE
        }
    }
}
