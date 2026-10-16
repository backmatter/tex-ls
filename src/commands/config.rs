//! Configuration discovery and CLI option precedence.
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};
use tex_ls::{
    cli::{ItemIndentArg, LineEndingArg, MathWrapArg, WrapArg},
    config::{Config, ConfigSource},
    file_discovery::ExcludeFilter,
};
use tex_ls_formatter::formatter::{FormatStyle, ItemIndent, LineEnding, MathWrap, WrapMode};

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
pub(super) fn resolve_style(
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
pub(super) fn cwd_anchor() -> Result<PathBuf, ExitCode> {
    std::env::current_dir().map_err(|err| {
        eprintln!("tex-ls: cannot determine the current directory: {err}");
        ExitCode::from(2)
    })
}

/// Resolve the effective config, mapping any [`ConfigError`] to a stderr message
/// and exit code 2.
pub(super) fn resolve_config(
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

/// Resolve configuration and discovery policy for commands that walk files.
pub(super) fn load_config(
    explicit: Option<&Path>,
    no_config: bool,
    cli_excludes: &[String],
    force_exclude: bool,
) -> Result<(tex_ls::config::ValidatedConfig, ExcludeFilter), ExitCode> {
    let anchor = cwd_anchor()?;
    let (config, source) = resolve_config(explicit, no_config, &anchor)?;
    let filter = build_exclude_filter(&config, &source, &anchor, cli_excludes)?
        .with_force_exclude(force_exclude);
    Ok((config, filter))
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

/// `tex-ls init`: write a commented starter config to `<cwd>/tex-ls.toml`.
pub(super) fn run_init(force: bool) -> ExitCode {
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
