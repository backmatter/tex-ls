//! The `tex-ls` command-line interface.
//!
//! Native format, lint, and language-server entry points.
//!
//! Clap renders CLI help directly from the command definitions.

mod cli_debug;
use cli_debug::*;

use std::io::{BufWriter, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tex_ls::format::{ChangedFile, check_paths_with_style, format_file_with_packages_sentence};
use tex_ls_analysis::linter::{OutputMode, render_findings};

use std::collections::{BTreeSet, HashMap};
use tex_ls::config::Config;
use tex_ls::config::ConfigSource;
use tex_ls::file_discovery::{ExcludeFilter, FileDiscoveryError, collect_lint_files};
use tex_ls_analysis::linter::{
    Diagnostic, RuleSelection, apply_fixes, check_document_fixable, lint_document,
};
use tex_ls_analysis::source::{FileKind, file_kind_or_tex};
use tex_ls_formatter::formatter::perturb::{
    ConvergenceError, DEFAULT_SINGLE_FLIP_SAMPLES, check_trivia_convergence, nontrivia_content,
    survey_trivia_invariance,
};
use tex_ls_formatter::formatter::{
    FormatStyle, ItemIndent, LineEnding, MathWrap, SentenceOptions, WrapMode,
    format_with_declarations_sentence,
};
use tex_ls_parser::declarations::ResolvedDeclarations;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use rayon::prelude::*;
use rowan::{GreenNode, NodeOrToken};
use similar::{Algorithm, ChangeTag, TextDiff};
use smol_str::SmolStr;
use tex_ls::cli::{
    Cli, ColorChoice, Command, DebugChecksArg, DebugCommand, ItemIndentArg, LineEndingArg,
    LintOutput, MathWrapArg, WrapArg,
};
use tex_ls_analysis::project::labels::{
    document_label_names, document_ref_names, is_document_root,
};
use tex_ls_analysis::project::{
    BibTarget, CiteFileFacts, FileFacts, IncludeGraph, PackageOptionFacts, ResolvedCitations,
    ResolvedLabels, ResolvedPackageOptions, collect_bib_resource_targets,
    collect_include_edge_keys, package_option_facts,
};
use tex_ls_parser::parser::{LexConfig, parse_with_declarations, parse_with_flavor};
use tex_ls_parser::semantic::SemanticModel;
use tex_ls_parser::syntax::SyntaxNode;

/// Lower the CLI [`WrapArg`] to the formatter's [`WrapMode`]. Kept as a free
/// function (not a `From` impl) because the orphan rule forbids implementing a
/// foreign trait for a foreign type in the binary crate, now that both types
/// live in the library.
fn wrap_mode(arg: WrapArg) -> WrapMode {
    match arg {
        WrapArg::Reflow => WrapMode::Reflow,
        WrapArg::Stable => WrapMode::Stable,
        WrapArg::Sentence => WrapMode::Sentence,
        WrapArg::Semantic => WrapMode::Semantic,
        WrapArg::Preserve => WrapMode::Preserve,
    }
}

/// Lower the CLI [`ItemIndentArg`] to the formatter's [`ItemIndent`].
fn item_indent_mode(arg: ItemIndentArg) -> ItemIndent {
    match arg {
        ItemIndentArg::Hang => ItemIndent::Hang,
        ItemIndentArg::Indent => ItemIndent::Indent,
        ItemIndentArg::None => ItemIndent::None,
    }
}

/// Lower the CLI [`LintOutput`] to the renderer's [`OutputMode`] (same
/// orphan-rule story as [`wrap_mode`]).
fn lint_output_mode(arg: LintOutput) -> OutputMode {
    match arg {
        LintOutput::Pretty => OutputMode::Pretty,
        LintOutput::Concise => OutputMode::Concise,
        LintOutput::Json => OutputMode::Json,
    }
}

/// Lower the CLI [`MathWrapArg`] to the formatter's [`MathWrap`] (same orphan-rule
/// story as [`wrap_mode`]).
fn math_wrap_mode(arg: MathWrapArg) -> MathWrap {
    match arg {
        MathWrapArg::Auto => MathWrap::Auto,
        MathWrapArg::Preserve => MathWrap::Preserve,
        MathWrapArg::SingleLine => MathWrap::SingleLine,
        MathWrapArg::Break => MathWrap::Break,
    }
}

/// Lower the CLI [`LineEndingArg`] to the formatter's [`LineEnding`] (same
/// orphan-rule story as [`wrap_mode`]).
fn line_ending_mode(arg: LineEndingArg) -> LineEnding {
    match arg {
        LineEndingArg::Auto => LineEnding::Auto,
        LineEndingArg::Lf => LineEnding::Lf,
        LineEndingArg::Crlf => LineEnding::Crlf,
        LineEndingArg::Native => LineEnding::Native,
    }
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    let Cli {
        command,
        config: config_arg,
        no_config,
        color,
        quiet,
    } = Cli::parse();
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
            let anchor = match cwd_anchor() {
                Ok(anchor) => anchor,
                Err(code) => return code,
            };
            let (config, config_source) =
                match resolve_config(config_arg.as_deref(), no_config, &anchor) {
                    Ok(resolved) => resolved,
                    Err(code) => return code,
                };
            let exclude_filter =
                match build_exclude_filter(&config, &config_source, &anchor, &exclude) {
                    Ok(filter) => filter.with_force_exclude(force_exclude),
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
            let anchor = match cwd_anchor() {
                Ok(anchor) => anchor,
                Err(code) => return code,
            };
            let (config, config_source) =
                match resolve_config(config_arg.as_deref(), no_config, &anchor) {
                    Ok(resolved) => resolved,
                    Err(code) => return code,
                };
            let exclude_filter =
                match build_exclude_filter(&config, &config_source, &anchor, &exclude) {
                    Ok(filter) => filter.with_force_exclude(force_exclude),
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
                let anchor = match cwd_anchor() {
                    Ok(anchor) => anchor,
                    Err(code) => return code,
                };
                let (config, config_source) =
                    match resolve_config(config_arg.as_deref(), no_config, &anchor) {
                        Ok(resolved) => resolved,
                        Err(code) => return code,
                    };
                let exclude_filter =
                    match build_exclude_filter(&config, &config_source, &anchor, &exclude) {
                        Ok(filter) => filter.with_force_exclude(force_exclude),
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

/// `tex-ls debug echo-args`: record the trailing arguments, one per line.
///
/// The forward-search tests configure this as their PDF viewer, so what lands in
/// `out` is exactly the argument vector the viewer would have been launched with.
fn run_debug_echo_args(out: &Path, args: &[String]) -> ExitCode {
    let mut body = args.join("\n");
    if !args.is_empty() {
        body.push('\n');
    }
    // Stage and rename rather than writing `out` in place. The test polling for
    // this recording lives in *another process* (the viewer runs detached), and
    // a plain `fs::write` leaves the file existing-and-empty between its
    // truncate and its write. A reader landing in that window reads back a
    // complete-looking recording of zero arguments, which is indistinguishable
    // from a viewer launched with none — the flake behind an `assert_eq!` whose
    // left side was `[]`. A rename within one directory is atomic, so the
    // reader sees either no file at all or the entire recording.
    let staging = out.with_file_name(format!(
        "{}.{}.tmp",
        out.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    match std::fs::write(&staging, body).and_then(|()| std::fs::rename(&staging, out)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = std::fs::remove_file(&staging);
            eprintln!("tex-ls: failed to write {}: {err}", out.display());
            ExitCode::from(2)
        }
    }
}

/// Resolve the effective [`FormatStyle`] and wrap override from the config plus
/// the CLI flags (each `None` when not given).
///
/// Wrap precedence: `--wrap` > config `wrap` > file-kind default. The override
/// is `None` only when neither is set, leaving each file on its kind's default
/// wrap (`.sty`/`.cls`/`.dtx`/`.ins` → Preserve, `.tex` → Reflow), resolved per
/// file at dispatch. Math-wrap precedence: `--math-wrap` > config `math-wrap` >
/// `auto`; the style already carries the config value (or `Auto`), the flag
/// just overwrites it, and `Auto` resolves against the effective wrap inside
/// the formatter, so no per-file dispatch is needed here.
fn resolve_style(
    config: &Config,
    line_width: Option<usize>,
    indent_width: Option<usize>,
    item_indent: Option<ItemIndentArg>,
    wrap: Option<WrapArg>,
    math_wrap: Option<MathWrapArg>,
    line_ending: Option<LineEndingArg>,
) -> (FormatStyle, Option<WrapMode>) {
    let mut style = FormatStyle::from(&config.format);
    if let Some(w) = line_width {
        style.line_width = w;
    }
    if let Some(w) = indent_width {
        style.indent_width = w;
    }
    if let Some(mode) = item_indent {
        style.item_indent = item_indent_mode(mode);
    }
    let wrap_override: Option<WrapMode> = wrap.map(wrap_mode).or(config.format.wrap);
    if let Some(mw) = math_wrap {
        style.math_wrap = math_wrap_mode(mw);
    }
    // Same precedence story as `math-wrap`: the style already carries the config
    // value (or `Auto`), and `Auto` resolves per document inside the formatter.
    if let Some(le) = line_ending {
        style.line_ending = line_ending_mode(le);
    }
    (style, wrap_override)
}

/// The directory to anchor config discovery and exclude-pattern roots at: the
/// current working directory.
fn cwd_anchor() -> Result<PathBuf, ExitCode> {
    std::env::current_dir().map_err(|err| {
        eprintln!("tex-ls: cannot determine the current directory: {err}");
        ExitCode::from(2)
    })
}

/// Resolve the effective config, mapping any [`ConfigError`] to a stderr message
/// and exit code 2.
fn resolve_config(
    explicit: Option<&Path>,
    no_config: bool,
    anchor: &Path,
) -> Result<(tex_ls::config::ValidatedConfig, ConfigSource), ExitCode> {
    tex_ls::config::ConfigLoader::resolve(explicit, no_config, anchor).map_err(|err| {
        eprintln!("tex-ls: {err}");
        ExitCode::from(2)
    })
}

/// Build the directory-discovery exclude filter from the resolved config plus any
/// `--exclude` CLI patterns. Patterns resolve relative to the directory holding
/// `tex-ls.toml`, or relative to `anchor` for the env and global user configs
/// and the no-config case ([`ConfigSource::exclude_root`]).
fn build_exclude_filter(
    config: &Config,
    source: &ConfigSource,
    anchor: &Path,
    cli_excludes: &[String],
) -> Result<ExcludeFilter, ExitCode> {
    let root = source.exclude_root(anchor);
    let patterns = config.exclude_patterns(cli_excludes);
    ExcludeFilter::new(root, &patterns).map_err(|err| {
        eprintln!("tex-ls: {err}");
        ExitCode::from(2)
    })
}

/// A commented starter `tex-ls.toml` showing every key at its default.
const STARTER_CONFIG: &str = "\
# tex-ls configuration. All keys are optional; values shown are the defaults.

# Gitignore-style patterns to skip during directory discovery. `exclude` replaces
# the built-in default set (`.git/`); `extend-exclude` adds on top of it. Both
# apply to `format` and `lint`.
# exclude = [\".git/\"]
# extend-exclude = []

[format]
# line-width = 80
# indent-width = 2
# item-indent = \"hang\"  # hang | indent | none
# wrap = \"reflow\"  # reflow | stable | sentence | semantic | preserve
                     # omit to use each file kind's default
                     # (.tex -> reflow, .sty/.cls/.dtx/.ins -> preserve)
# math-wrap = \"auto\"  # auto | preserve | single-line | break
                        # display-math line breaking; auto derives from wrap
                        # (preserve -> preserve, else break)
# line-ending = \"auto\"  # auto | lf | crlf | native
                          # auto keeps the endings each file was written with

[lint]
# select = [\"...\"]  # if set, only these rules run
# ignore = []        # rules to disable
# default-off rules remain available through select

# Where the compiler leaves its artifacts, and which file it ran on. Read by the
# language server only: `.aux` for label/section numbers, the PDF for forward
# search. Paths are relative to the root document's directory (`root` itself is
# relative to this file).
# [build]
# aux-dir = \"out\"
# pdf-dir = \"out\"
# pdf-filename = \"main.pdf\"  # bare file name; defaults to the root's stem
# root = \"main.tex\"          # only needed when the root is not auto-detected
";

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

/// `tex-ls init`: write a commented starter config to `<cwd>/tex-ls.toml`.
fn run_init(force: bool) -> ExitCode {
    let anchor = match cwd_anchor() {
        Ok(anchor) => anchor,
        Err(code) => return code,
    };
    let path = anchor.join(tex_ls::config::CONFIG_FILE_NAME);
    if path.exists() && !force {
        eprintln!(
            "tex-ls: {} already exists; pass --force to overwrite",
            path.display()
        );
        return ExitCode::from(2);
    }
    match std::fs::write(&path, STARTER_CONFIG) {
        Ok(()) => {
            println!("Wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("tex-ls: failed to write {}: {err}", path.display());
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

/// Cap on fixpoint iterations per file, guarding against a fix that fails to
/// clear its own diagnostic.
const MAX_FIX_ITERATIONS: usize = 10;

/// Print a rule's description and examples (`lint --explain <rule>`), then exit.
/// The id is looked up in the LaTeX registry first, then the bib registry (the
/// two share one namespace, so at most one matches). Unknown ids exit `2` after
/// listing every known built-in rule id across both linters.
fn run_explain(id: &str) -> ExitCode {
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

/// Per-file result of the parallel Phase-1 parse+analyze in [`run_lint`]. Carries
/// only `Send` data across the rayon boundary — a `GreenNode` (Send), never a red
/// `SyntaxNode` (not Send; AGENTS.md decision #7). The red tree is materialized
/// thread-locally to extract facts and dropped before returning; Phase 3
/// re-materializes it from the green node to lint.
/// Result of reading one discovered source in parallel: its `(path, text, kind)`
/// on success, or the `(path, error)` to report on failure.
type ReadResult = Result<(PathBuf, String, FileKind), (PathBuf, std::io::Error)>;

#[derive(Default)]
struct SupplementalBibliographies {
    sources: Vec<(PathBuf, String)>,
    aliases: HashMap<PathBuf, PathBuf>,
}

enum FileAnalysis {
    Bib {
        diagnostics: Vec<Diagnostic>,
        path: PathBuf,
        keys: Vec<SmolStr>,
        green: GreenNode,
        model: Box<tex_ls_analysis::bib::semantic::Model>,
    },
    // Boxed: the `.tex` payload is far larger than the `.bib` one, so an unboxed
    // variant would bloat every `FileAnalysis` to its size.
    Tex(Box<TexAnalysis>),
}

/// The `.tex`/`.sty`/… parse+analyze payload carried by [`FileAnalysis::Tex`].
struct TexAnalysis {
    diagnostics: Vec<Diagnostic>,
    path: PathBuf,
    green: GreenNode,
    model: SemanticModel,
    facts: FileFacts,
    label_input: (PathBuf, Vec<SmolStr>, Vec<SmolStr>, bool),
    cite_fact: CiteFileFacts,
    /// The file's declared-option surface when it is a `.sty`, feeding the
    /// cross-file package-option model (`unknown-option`).
    option_facts: Option<PackageOptionFacts>,
}

/// Parse and analyze one source. Pure and thread-safe (no shared mutable state,
/// no environment access), so [`run_lint`] maps it over all files with rayon. The
/// resolver-feeding facts use the same pure helpers the salsa queries do, so CLI
/// and LSP agree.
fn analyze_source(
    path: &Path,
    content: &str,
    kind: FileKind,
    declared: &ResolvedDeclarations,
) -> FileAnalysis {
    match kind {
        FileKind::Bib => {
            // Build the model once: it yields both the lint diagnostics and the
            // cite keys this `.bib` contributes to the citation resolver.
            let parsed = tex_ls_analysis::bib::parse(content);
            let diagnostics: Vec<Diagnostic> = parsed
                .errors
                .iter()
                .map(|err| Diagnostic {
                    rule: "parse",
                    severity: tex_ls_analysis::linter::Severity::Error,
                    path: path.to_path_buf(),
                    start: err.start,
                    end: err.end,
                    message: err.message.clone(),
                    fix: None,
                    related: Vec::new(),
                })
                .collect();
            let root = parsed.syntax();
            let model = tex_ls_analysis::bib::semantic::Model::build(&root);
            let keys = model.entries().iter().map(|e| e.key.clone()).collect();
            FileAnalysis::Bib {
                diagnostics,
                path: path.to_path_buf(),
                keys,
                green: parsed.green,
                model: Box::new(model),
            }
        }
        FileKind::Tex
        | FileKind::CodeTex
        | FileKind::Sty
        | FileKind::Cls
        | FileKind::Dtx
        | FileKind::Ins => {
            let parsed = parse_with_declarations(content, kind.lex_config(), declared);
            let diagnostics: Vec<Diagnostic> = parsed
                .errors
                .iter()
                .map(|err| Diagnostic::from_parse(path.to_path_buf(), err))
                .collect();
            let green = parsed.green;
            let root = SyntaxNode::new_root(green.clone());
            let model = SemanticModel::build_with_declarations(&root, declared);
            let facts = FileFacts {
                include_only: tex_ls_parser::semantic::roles::include_only(&root),
                path: path.to_path_buf(),
                include_edges: collect_include_edge_keys(&root, path.parent()),
            };
            let label_input = (
                path.to_path_buf(),
                document_label_names(&model),
                document_ref_names(&model),
                is_document_root(&root),
            );
            let cite_fact = CiteFileFacts {
                path: path.to_path_buf(),
                bib_targets: collect_bib_resource_targets(&root, None),
                manual_keys: model
                    .bibitems()
                    .iter()
                    .map(|item| item.name.clone())
                    .collect(),
                nocite_all: model.has_wildcard_nocite(),
                is_document_root: is_document_root(&root),
            };
            let option_facts = package_option_facts(path, &root, &model);
            FileAnalysis::Tex(Box::new(TexAnalysis {
                diagnostics,
                path: path.to_path_buf(),
                green,
                model,
                facts,
                label_input,
                cite_fact,
                option_facts,
            }))
        }
    }
}

/// Load literal bibliography resources that were not among the user's lint
/// inputs. These files feed citation resolution but are not themselves lint
/// targets, so a system-wide database cannot unexpectedly add standalone
/// BibTeX findings to `tex-ls lint doc.tex`.
fn supplemental_bibliographies(
    sources: &[(PathBuf, String, FileKind)],
    declared: &ResolvedDeclarations,
) -> SupplementalBibliographies {
    let mut out = SupplementalBibliographies::default();
    let mut loaded: BTreeSet<PathBuf> = sources.iter().map(|(path, _, _)| path.clone()).collect();

    let analyzed: Vec<_> = sources
        .iter()
        .filter_map(|(path, content, kind)| {
            if !kind.is_latex() {
                return None;
            }
            let parsed = parse_with_declarations(content, kind.lex_config(), declared);
            let root = SyntaxNode::new_root(parsed.green);
            let model = SemanticModel::build(&root);
            Some((path, root, model))
        })
        .collect();
    let graph = IncludeGraph::build(
        &analyzed
            .iter()
            .map(|(path, root, model)| FileFacts {
                path: (*path).clone(),
                include_edges: collect_include_edge_keys(root, path.parent()),
                include_only: model.include_only().clone(),
            })
            .collect::<Vec<_>>(),
        None,
    );
    let labels = ResolvedLabels::build(
        &analyzed
            .iter()
            .map(|(path, root, _)| {
                (
                    (*path).clone(),
                    Vec::new(),
                    Vec::new(),
                    is_document_root(root),
                )
            })
            .collect::<Vec<_>>(),
        &graph,
    );
    for (path, root, _) in &analyzed {
        let base_dir = path.parent().filter(|base| !base.as_os_str().is_empty());
        let mut bases = Vec::new();
        for compilation_root in labels.candidate_roots(path) {
            bases.extend(
                graph
                    .root_contexts(compilation_root)
                    .remove(*path)
                    .unwrap_or_default(),
            );
        }
        if bases.is_empty() {
            bases.extend(base_dir.map(Path::to_path_buf));
        }
        for target in tex_ls_analysis::project::citations::contextual_targets(
            &collect_bib_resource_targets(root, None),
            &bases,
        ) {
            let BibTarget::Path(requested) = target else {
                continue;
            };
            if loaded.contains(&requested) {
                continue;
            }
            let Some(actual) =
                tex_ls::bibliography::resolve_bibliography_file(&requested, base_dir)
            else {
                continue;
            };
            if actual != requested {
                out.aliases.insert(requested, actual.clone());
            }
            if !loaded.insert(actual.clone()) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&actual) {
                out.sources.push((actual, content));
            }
        }
    }
    out
}

/// Lint each path (or stdin), rendering parse diagnostics. Exits non-zero if
/// any diagnostics are reported or any file fails to read. With `fix`, safe
/// autofixes (plus unsafe ones when `unsafe_fixes` is set) are applied in place
/// first; the reporting pass below then shows whatever findings remain.
#[allow(clippy::too_many_arguments)]
fn run_lint(
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
                Ok(content) => Ok((path.clone(), content, *kind)),
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
        // JSON goes to stdout unconditionally (`[]` when clean) so consumers
        // always receive a valid document. It serializes byte offsets and
        // needs no source lookup, and never carries color.
        println!("{}", render_findings(&diagnostics, mode, false, &|_| None));
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

/// Collect parse and lint findings using project label and citation resolution.
fn collect_project_diagnostics_with_bibliographies(
    sources: &[(PathBuf, String, FileKind)],
    declared: &ResolvedDeclarations,
    supplemental: &SupplementalBibliographies,
) -> Vec<Diagnostic> {
    // Phase 1 — parse + analyze every source in parallel. Each task is pure and
    // returns only `Send` data (`analyze_source`); rayon preserves input order.
    let analyses: Vec<FileAnalysis> = sources
        .par_iter()
        .map(|(path, content, kind)| analyze_source(path, content, *kind, declared))
        .collect();

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut analyzed: Vec<(PathBuf, GreenNode, SemanticModel)> = Vec::new();
    let mut bib_analyzed = Vec::new();
    let mut supplemental_models = Vec::new();
    let mut facts: Vec<FileFacts> = Vec::new();
    let mut label_inputs = Vec::new();
    let mut cite_facts: Vec<CiteFileFacts> = Vec::new();
    let mut option_facts: Vec<PackageOptionFacts> = Vec::new();
    // Cite keys per analyzed `.bib` path, feeding the cross-file citation resolver.
    let mut bib_keys: HashMap<PathBuf, Vec<SmolStr>> = HashMap::new();
    for analysis in analyses {
        match analysis {
            FileAnalysis::Bib {
                diagnostics: d,
                path,
                keys,
                green,
                model,
            } => {
                diagnostics.extend(d);
                bib_keys.insert(path.clone(), keys);
                bib_analyzed.push((path, green, model));
            }
            FileAnalysis::Tex(tex) => {
                let TexAnalysis {
                    diagnostics: d,
                    path,
                    green,
                    model,
                    facts: f,
                    label_input,
                    cite_fact,
                    option_facts: o,
                } = *tex;
                diagnostics.extend(d);
                facts.push(f);
                label_inputs.push(label_input);
                cite_facts.push(cite_fact);
                option_facts.extend(o);
                analyzed.push((path, green, model));
            }
        }
    }
    for (path, content) in &supplemental.sources {
        let parsed = tex_ls_analysis::bib::parse(content);
        let model = tex_ls_analysis::bib::semantic::Model::build(&parsed.syntax());
        bib_keys.insert(
            path.clone(),
            model
                .entries()
                .iter()
                .map(|entry| entry.key.clone())
                .collect(),
        );
        supplemental_models.push((path.clone(), model));
    }

    // Phase 2 — cross-file resolution: a serial barrier (needs the whole analyzed
    // set) over the collected facts. Pure graph work, no re-parsing.
    let graph = IncludeGraph::build(&facts, None);
    let resolved = ResolvedLabels::build(&label_inputs, &graph);
    let resolved_citations = ResolvedCitations::build_with_aliases(
        &cite_facts,
        &graph,
        &bib_keys,
        &supplemental.aliases,
    );
    let resolved_packages = ResolvedPackageOptions::build(option_facts);
    for (path, green, model) in &bib_analyzed {
        let project = tex_ls_analysis::bib::linter::project::ProjectFacts::build(
            path,
            &resolved_citations,
            analyzed
                .iter()
                .map(|(path, _, model)| (path.as_path(), model)),
            bib_analyzed
                .iter()
                .map(|(path, _, model)| (path.as_path(), model.as_ref()))
                .chain(
                    supplemental_models
                        .iter()
                        .map(|(path, model)| (path.as_path(), model)),
                ),
        );
        diagnostics.extend(tex_ls_analysis::bib::linter::lint_document_with_project(
            path,
            &tex_ls_analysis::bib::syntax::SyntaxNode::new_root(green.clone()),
            model,
            Some(&project),
        ));
    }

    // Phase 3 — lint every analyzed file in parallel, sharing the resolution by
    // reference. The red tree is materialized thread-locally from each green node
    // (red trees are not `Send`).
    let lint_results: Vec<Vec<Diagnostic>> = analyzed
        .par_iter()
        .map(|(path, green, model)| {
            let root = SyntaxNode::new_root(green.clone());
            lint_document(
                path,
                &root,
                model,
                Some(&resolved),
                Some(&resolved_citations),
                Some(&resolved_packages),
            )
        })
        .collect();
    for result in lint_results {
        diagnostics.extend(result);
    }
    diagnostics
}

/// Discover lintable files under `paths` and apply autofixes in place. Returns
/// `Some(exit_code)` only on a hard error (discovery / IO); on success returns
/// `None` so the caller falls through to the normal reporting pass.
///
/// Both `.tex` and `.bib` files are fixed, each through its own linter; rules that
/// emit no autofix (the report-only majority) leave their findings for the
/// reporting pass that follows.
fn apply_fixes_to_paths(
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

/// Parse a single file (or stdin) and print its CST to stdout. Parse errors are
/// printed after the tree; the command exits non-zero if any are reported.
fn run_parse(paths: &[PathBuf], declared: &ResolvedDeclarations) -> ExitCode {
    // Clap caps `parse` at one positional, so the slice holds at most one path.
    let path = match inputs_or_exit(paths, "parse", PARSE_MISSING_INPUT) {
        Inputs::Stdin => None,
        Inputs::Paths(paths) => Some(paths[0].as_path()),
    };
    let input = match path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                eprintln!("tex-ls: cannot read {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => {
            let mut input = String::new();
            if let Err(err) = std::io::stdin().read_to_string(&mut input) {
                eprintln!("tex-ls: cannot read stdin: {err}");
                return ExitCode::FAILURE;
            }
            input
        }
    };

    let config = path.map_or(LexConfig::default(), |p| file_kind_or_tex(p).lex_config());
    let parsed = parse_with_declarations(&input, config, declared);
    let mut out = String::new();
    render_cst(&parsed.syntax(), 0, &mut out);
    if let Err(err) = std::io::stdout().write_all(out.as_bytes()) {
        eprintln!("tex-ls: cannot write stdout: {err}");
        return ExitCode::FAILURE;
    }

    if parsed.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        for err in &parsed.errors {
            eprintln!("error @{}..{}: {}", err.start, err.end, err.message);
        }
        ExitCode::FAILURE
    }
}

/// Render a CST as an indented `KIND@range` tree, with token text. Kept in sync
/// with the test renderer in `tests/parser.rs`.
fn render_cst(node: &SyntaxNode, depth: usize, out: &mut String) {
    out.push_str(&format!(
        "{:indent$}{:?}@{:?}\n",
        "",
        node.kind(),
        node.text_range(),
        indent = depth * 2
    ));
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(n) => render_cst(&n, depth + 1, out),
            NodeOrToken::Token(t) => out.push_str(&format!(
                "{:indent$}{:?}@{:?} {:?}\n",
                "",
                t.kind(),
                t.text_range(),
                t.text(),
                indent = (depth + 1) * 2
            )),
        }
    }
}

/// The global output flags (`--color`, `--quiet`), threaded to the commands that
/// write human-facing output.
#[derive(Debug, Clone, Copy)]
struct OutputOptions {
    color: ColorChoice,
    quiet: bool,
}

/// Where a subcommand's positional arguments say to read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inputs<'a> {
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
fn is_stdin_path(path: &Path) -> bool {
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
fn usage_error(subcommand: &str, kind: ErrorKind, message: &str) -> ! {
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
const FORMAT_MISSING_INPUT: &str =
    "no input paths; pass files or directories to format, or `-` to read from stdin";
const LINT_MISSING_INPUT: &str =
    "no input paths; pass files or directories to lint, or `-` to read from stdin";
const PARSE_MISSING_INPUT: &str = "no input path; pass a file to parse, or `-` to read from stdin";

/// [`resolve_inputs`] against the real stdin, exiting on a usage mistake.
/// `missing` spells the "nothing to read" case in the subcommand's own terms.
fn inputs_or_exit<'a>(paths: &'a [PathBuf], subcommand: &str, missing: &str) -> Inputs<'a> {
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
fn color_enabled(choice: ColorChoice, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && is_terminal,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_format(
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

/// Print a file-discovery error to stderr, prefixed like the other CLI errors.
fn report_discovery_error(err: &FileDiscoveryError) {
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

    #[test]
    fn color_choice_overrides_ignore_the_terminal() {
        assert!(color_enabled(ColorChoice::Always, false));
        assert!(!color_enabled(ColorChoice::Never, true));
    }

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
