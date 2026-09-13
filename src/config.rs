//! Native project configuration discovery and loading.
mod schema;
pub use schema::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct ConfigLoader;
impl ConfigLoader {
    /// Parse a `tex-ls.toml` from disk and validate it.
    pub fn load_from(path: &Path) -> Result<ValidatedConfig, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Config::parse_str(&text, path)
    }

    /// Walk `start` and its ancestors looking for a `tex-ls.toml`. Stops at the
    /// first match or at a directory that contains a `.git` entry (repo root),
    /// whichever comes first. Returns `None` if neither is found before the
    /// filesystem root. The global user config is *not* consulted here; that
    /// fallback lives in [`resolve`](Self::resolve).
    pub fn discover(start: &Path) -> Result<Option<(PathBuf, ValidatedConfig)>, ConfigError> {
        let canonical = start.canonicalize().map_err(|source| ConfigError::Io {
            path: start.to_path_buf(),
            source,
        })?;
        for dir in canonical.ancestors() {
            let candidate = dir.join(CONFIG_FILE_NAME);
            if candidate.is_file() {
                let config = Self::load_from(&candidate)?;
                return Ok(Some((candidate, config)));
            }
            if dir.join(".git").exists() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// Config resolution for the CLI and the language server. Precedence:
    /// an explicit `--config` path, then a discovered project `tex-ls.toml`,
    /// then a file named by [`$TEX_LS_CONFIG`](CONFIG_ENV_VAR), then the
    /// global user config, then built-in defaults.
    /// Whole-file fallback, never a merge. `no_config` skips every file
    /// (project, env, and global). CLI flag overrides for the formatter/lint
    /// knobs are applied by the caller after this returns.
    pub fn resolve(
        explicit: Option<&Path>,
        no_config: bool,
        anchor: &Path,
    ) -> Result<(ValidatedConfig, ConfigSource), ConfigError> {
        Self::resolve_with_fallbacks(
            explicit,
            no_config,
            anchor,
            env_config_path().as_deref(),
            global_config_path().as_deref(),
        )
    }

    /// [`resolve`](Self::resolve) with the env and global fallback paths
    /// injected, so tests can exercise them without touching the real
    /// environment or home directory.
    fn resolve_with_fallbacks(
        explicit: Option<&Path>,
        no_config: bool,
        anchor: &Path,
        env: Option<&Path>,
        global: Option<&Path>,
    ) -> Result<(ValidatedConfig, ConfigSource), ConfigError> {
        if no_config {
            return Ok((ValidatedConfig::default(), ConfigSource::None));
        }
        if let Some(path) = explicit {
            let config = Self::load_from(path)?;
            return Ok((config, ConfigSource::Explicit(path.to_path_buf())));
        }
        if let Some((path, config)) = Self::discover(anchor)? {
            return Ok((config, ConfigSource::Discovered(path)));
        }
        // A set `$TEX_LS_CONFIG` shadows the global config entirely, and a
        // missing or broken file is a hard error rather than a fall-through:
        // it is the config that would apply, and silently ignoring it would
        // hide a typo'd path indefinitely.
        if let Some(path) = env {
            let config = Self::load_from(path)?;
            return Ok((config, ConfigSource::Env(path.to_path_buf())));
        }
        // Same rationale: a broken global config is a hard error, not a
        // silent fall-through to built-in defaults.
        if let Some(path) = global {
            let config = Self::load_from(path)?;
            return Ok((config, ConfigSource::Global(path.to_path_buf())));
        }
        Ok((ValidatedConfig::default(), ConfigSource::None))
    }
}
/// Which configuration source [`ConfigLoader::resolve`] loaded, carrying its path.
///
/// The distinction matters for relative exclude patterns: a project-local file
/// anchors them at its own directory, while the global config has no project
/// location and anchors at the caller's directory instead (see
/// [`exclude_root`](Self::exclude_root)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Loaded from an explicit `--config <path>`.
    Explicit(PathBuf),
    /// Discovered by the ancestor walk from the input's directory.
    Discovered(PathBuf),
    /// Named by the [`$TEX_LS_CONFIG`](CONFIG_ENV_VAR) environment variable,
    /// used when no project config is discovered.
    Env(PathBuf),
    /// The global user config (e.g. `~/.config/tex-ls/config.toml`), used when
    /// no project config is discovered and `$TEX_LS_CONFIG` is unset.
    Global(PathBuf),
    /// No config file found; built-in defaults are in use.
    None,
}

impl ConfigSource {
    /// Path of the resolved config file, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Explicit(p) | Self::Discovered(p) | Self::Env(p) | Self::Global(p) => Some(p),
            Self::None => None,
        }
    }

    /// The directory relative exclude patterns resolve against: the config
    /// file's own directory for a project-local file, or `anchor` (the CLI
    /// working directory, or the document's directory in the LSP) for the
    /// env and global configs and the no-config case, which have no project
    /// location.
    pub fn exclude_root<'a>(&'a self, anchor: &'a Path) -> &'a Path {
        match self {
            Self::Explicit(p) | Self::Discovered(p) => p.parent().unwrap_or(anchor),
            Self::Env(_) | Self::Global(_) | Self::None => anchor,
        }
    }
}

/// Path named by the [`$TEX_LS_CONFIG`](CONFIG_ENV_VAR) environment variable,
/// or `None` when unset or empty (an empty value counts as unset, the usual
/// shell convention).
fn env_config_path() -> Option<PathBuf> {
    let value = std::env::var_os(CONFIG_ENV_VAR)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Path to the global user config, the fallback when no project `tex-ls.toml`
/// is discovered: the first existing file among
/// `$XDG_CONFIG_HOME/tex-ls/config.toml`, `~/.config/tex-ls/config.toml`, and
/// `<platform config dir>/tex-ls/config.toml` (Windows `%APPDATA%`, macOS
/// `~/Library/Application Support`). The `~/.config` candidate is checked on
/// every platform so the CLI-dotfile convention works on macOS and Windows too.
fn global_config_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        candidates.push(PathBuf::from(xdg));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".config"));
    }
    if let Some(config) = dirs::config_dir() {
        candidates.push(config);
    }
    candidates
        .into_iter()
        .map(|dir| dir.join("tex-ls").join("config.toml"))
        .find(|path| path.is_file())
}

#[cfg(test)]
use tex_ls_formatter::formatter::{FormatStyle, ItemIndent, LineEnding, MathWrap, WrapMode};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn parse(text: &str) -> Result<ValidatedConfig, ConfigError> {
        Config::parse_str(text, Path::new("tex-ls.toml"))
    }

    #[test]
    fn default_config_matches_format_style_defaults() {
        let config = ValidatedConfig::default();
        let style = FormatStyle::from(&config.format);
        assert_eq!(style.line_width, FormatStyle::default().line_width);
        assert_eq!(style.indent_width, FormatStyle::default().indent_width);
        assert_eq!(style.item_indent, FormatStyle::default().item_indent);
    }

    #[test]
    fn empty_file_yields_defaults() {
        let config = parse("").expect("parse");
        assert_eq!(config, ValidatedConfig::default());
    }

    #[test]
    fn parses_minimal_format_section() {
        let config = parse("[format]\nline-width = 100\n").expect("parse");
        let style = FormatStyle::from(&config.format);
        assert_eq!(style.line_width, 100);
        assert_eq!(style.indent_width, 2);
    }

    #[test]
    fn parses_indent_width() {
        let config = parse("[format]\nindent-width = 4\n").expect("parse");
        let style = FormatStyle::from(&config.format);
        assert_eq!(style.indent_width, 4);
        assert_eq!(style.line_width, 80);
    }

    #[test]
    fn parses_item_indent() {
        let config = parse("[format]\nitem-indent = \"indent\"\n").expect("parse");
        let style = FormatStyle::from(&config.format);
        assert_eq!(style.item_indent, ItemIndent::Indent);
    }

    #[test]
    fn rejects_unknown_item_indent() {
        assert!(parse("[format]\nitem-indent = \"same\"\n").is_err());
    }

    #[test]
    fn wrap_defaults_to_none() {
        let config = parse("[format]\n").expect("parse");
        assert_eq!(config.format.wrap, None);
    }

    #[test]
    fn rejects_texmf_section() {
        // `[texmf]` moved to the LSP editor settings (it is machine configuration,
        // not project data); a leftover section is surfaced, not silently ignored.
        assert!(parse("[texmf]\nenabled = false\n").is_err());
    }

    #[test]
    fn build_aux_dir_defaults_to_none() {
        let config = parse("").expect("parse");
        assert_eq!(config.build, BuildConfig::default());
    }

    #[test]
    fn parses_build_section() {
        let config = parse(
            "[build]\naux-dir = \"out\"\npdf-dir = \"out\"\npdf-filename = \"thesis.pdf\"\nroot = \"main.tex\"\n",
        )
        .expect("parse");
        assert_eq!(config.build.aux_dir, Some(PathBuf::from("out")));
        assert_eq!(config.build.pdf_dir, Some(PathBuf::from("out")));
        assert_eq!(config.build.pdf_filename, Some("thesis.pdf".to_owned()));
        assert_eq!(config.build.root, Some(PathBuf::from("main.tex")));
    }

    #[test]
    fn rejects_a_pdf_filename_carrying_a_directory() {
        for name in ["out/thesis.pdf", ""] {
            let err = parse(&format!("[build]\npdf-filename = \"{name}\"\n"))
                .expect_err("a path is not a bare file name");
            assert!(
                matches!(err, ConfigError::InvalidValue { field, .. } if field == "pdf-filename"),
                "unexpected error for `{name}`: {err}"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_build_key() {
        assert!(parse("[build]\npdf-directory = \"out\"\n").is_err());
    }

    #[test]
    fn parses_wrap_variants() {
        for (key, expected) in [
            ("reflow", WrapMode::Reflow),
            ("stable", WrapMode::Stable),
            ("sentence", WrapMode::Sentence),
            ("semantic", WrapMode::Semantic),
            ("preserve", WrapMode::Preserve),
        ] {
            let text = format!("[format]\nwrap = \"{key}\"\n");
            let config = parse(&text).unwrap_or_else(|e| panic!("parse {key}: {e}"));
            assert_eq!(config.format.wrap, Some(expected), "for {key}");
        }
    }

    #[test]
    fn rejects_unknown_wrap() {
        let err = parse("[format]\nwrap = \"smart\"\n").expect_err("unknown variant");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn parses_math_wrap_variants() {
        for (key, expected) in [
            ("auto", MathWrap::Auto),
            ("preserve", MathWrap::Preserve),
            ("single-line", MathWrap::SingleLine),
            ("break", MathWrap::Break),
        ] {
            let text = format!("[format]\nmath-wrap = \"{key}\"\n");
            let config = parse(&text).unwrap_or_else(|e| panic!("parse {key}: {e}"));
            assert_eq!(config.format.math_wrap, Some(expected), "for {key}");
        }
    }

    #[test]
    fn math_wrap_defaults_to_none_and_maps_to_auto() {
        let config = parse("[format]\n").expect("parse");
        assert_eq!(config.format.math_wrap, None);
        let style = FormatStyle::from(&config.format);
        assert_eq!(style.math_wrap, MathWrap::Auto);
    }

    #[test]
    fn rejects_unknown_math_wrap() {
        let err = parse("[format]\nmath-wrap = \"never\"\n").expect_err("unknown variant");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn parses_line_ending_variants() {
        for (key, expected) in [
            ("auto", LineEnding::Auto),
            ("lf", LineEnding::Lf),
            ("crlf", LineEnding::Crlf),
            ("native", LineEnding::Native),
        ] {
            let text = format!("[format]\nline-ending = \"{key}\"\n");
            let config = parse(&text).unwrap_or_else(|e| panic!("parse {key}: {e}"));
            assert_eq!(config.format.line_ending, Some(expected), "for {key}");
            let style = FormatStyle::from(&config.format);
            assert_eq!(style.line_ending, expected, "for {key}");
        }
    }

    #[test]
    fn line_ending_defaults_to_none_and_maps_to_auto() {
        let config = parse("[format]\n").expect("parse");
        assert_eq!(config.format.line_ending, None);
        let style = FormatStyle::from(&config.format);
        assert_eq!(style.line_ending, LineEnding::Auto);
    }

    #[test]
    fn rejects_unknown_line_ending() {
        let err = parse("[format]\nline-ending = \"cr\"\n").expect_err("unknown variant");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn stable_wrap_target_sits_below_line_width() {
        let config = parse("[format]\nline-width = 100\nwrap = \"stable\"\n").expect("parse");
        let style = FormatStyle::from(&config.format);
        assert_eq!(config.format.wrap, Some(WrapMode::Stable));
        assert_eq!(style.stable_wrap_target(), 85);
    }

    #[test]
    fn rejects_unknown_top_level_table() {
        let err = parse("[formatt]\nline-width = 80\n").expect_err("unknown table");
        match err {
            ConfigError::Parse { message, .. } => {
                assert!(message.contains("formatt"), "got: {message}");
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_field_in_format() {
        let err = parse("[format]\nline-widht = 80\n").expect_err("unknown field");
        match err {
            ConfigError::Parse { message, .. } => {
                assert!(message.contains("line-widht"), "got: {message}");
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_snake_case_keys() {
        // We use kebab-case in the schema; snake_case must be rejected so users get
        // a clear error instead of silent fallthrough to defaults.
        let err = parse("[format]\nline_width = 80\n").expect_err("snake_case");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn lang_defaults_to_none_and_abbreviations_empty() {
        let config = parse("[format]\n").expect("parse");
        assert_eq!(config.format.lang, None);
        assert!(config.format.no_break_abbreviations.is_empty());
    }

    #[test]
    fn parses_lang_and_no_break_abbreviations() {
        let text = "[format]\nwrap = \"sentence\"\nlang = \"de\"\n\n\
                    [format.no-break-abbreviations]\n\
                    default = [\"ibid.\"]\n\
                    de = [\"bzw.\", \"Abb.\"]\n";
        let config = parse(text).expect("parse");
        assert_eq!(config.format.lang.as_deref(), Some("de"));
        assert_eq!(
            config.format.no_break_abbreviations.get("default"),
            Some(&vec!["ibid.".to_string()])
        );
        assert_eq!(
            config.format.no_break_abbreviations.get("de"),
            Some(&vec!["bzw.".to_string(), "Abb.".to_string()])
        );
    }

    #[test]
    fn rejects_zero_line_width() {
        let err = parse("[format]\nline-width = 0\n").expect_err("zero width");
        match err {
            ConfigError::InvalidValue { field, message, .. } => {
                assert_eq!(field, "line-width");
                assert!(message.contains('0'));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn rejects_huge_line_width() {
        let err = parse("[format]\nline-width = 10000\n").expect_err("too big");
        assert!(matches!(
            err,
            ConfigError::InvalidValue {
                field: "line-width",
                ..
            }
        ));
    }

    #[test]
    fn rejects_negative_width_as_parse_error() {
        // u32 deserialization rejects negatives at the type layer.
        let err = parse("[format]\nline-width = -1\n").expect_err("negative");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn exclude_defaults_to_none_and_uses_builtin_set() {
        let config = ValidatedConfig::default();
        assert_eq!(config.exclude, None);
        assert!(config.extend_exclude.is_empty());
        // With no `exclude`, the base list is the built-in defaults.
        assert_eq!(
            config.exclude_patterns(&[]),
            DEFAULT_EXCLUDE
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn present_exclude_replaces_defaults() {
        let config = parse("exclude = [\"vendor/\"]\n").expect("parse");
        assert_eq!(
            config.exclude.as_deref(),
            Some(&["vendor/".to_string()][..])
        );
        assert_eq!(config.exclude_patterns(&[]), vec!["vendor/".to_string()]);
    }

    #[test]
    fn empty_exclude_drops_defaults() {
        let config = parse("exclude = []\n").expect("parse");
        assert_eq!(config.exclude.as_deref(), Some(&[][..]));
        assert!(config.exclude_patterns(&[]).is_empty());
    }

    #[test]
    fn extend_exclude_is_additive_over_defaults() {
        let config = parse("extend-exclude = [\"build/\"]\n").expect("parse");
        let mut expected: Vec<String> = DEFAULT_EXCLUDE.iter().map(|p| p.to_string()).collect();
        expected.push("build/".to_string());
        assert_eq!(config.exclude_patterns(&[]), expected);
    }

    #[test]
    fn extend_exclude_layers_on_present_exclude_then_cli() {
        let config =
            parse("exclude = [\"vendor/\"]\nextend-exclude = [\"build/\"]\n").expect("parse");
        assert_eq!(
            config.exclude_patterns(&["tmp/".to_string()]),
            vec![
                "vendor/".to_string(),
                "build/".to_string(),
                "tmp/".to_string(),
            ]
        );
    }

    #[test]
    fn rejects_exclude_under_format() {
        // `exclude` is a top-level key (it governs both format and lint), never
        // nested under `[format]`.
        let err = parse("[format]\nexclude = [\"x\"]\n").expect_err("exclude is top-level");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn accepts_empty_lint_section() {
        let config = parse("[lint]\n").expect("parse");
        assert_eq!(config.lint, LintConfig::default());
    }

    #[test]
    fn rejects_unknown_field_in_lint() {
        let err = parse("[lint]\nstyle = \"strict\"\n").expect_err("unknown field");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn parses_lint_select() {
        let config = parse("[lint]\nselect = [\"duplicate-label\"]\n").expect("parse");
        assert_eq!(
            config.lint.select.as_deref(),
            Some(&["duplicate-label".to_string()][..])
        );
    }

    #[test]
    fn parses_lint_ignore() {
        let config = parse("[lint]\nignore = [\"deprecated-command\"]\n").expect("parse");
        assert_eq!(config.lint.ignore, vec!["deprecated-command".to_string()]);
    }

    #[test]
    fn parse_error_reports_file_path_and_line() {
        let path = Path::new("/tmp/oops.toml");
        let err = Config::parse_str("[format]\nbogus = 1\n", path).expect_err("bad field");
        let rendered = err.to_string();
        assert!(rendered.starts_with("/tmp/oops.toml:"));
    }

    #[test]
    fn load_from_missing_file_returns_io_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let err = ConfigLoader::load_from(&path).expect_err("missing file");
        assert!(matches!(err, ConfigError::Io { .. }));
    }

    #[test]
    fn discover_finds_config_in_parent() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[format]\nline-width = 70\n",
        )
        .unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();

        let (path, config) = ConfigLoader::discover(&nested)
            .expect("discover")
            .expect("found");
        assert_eq!(
            path,
            dir.path().canonicalize().unwrap().join(CONFIG_FILE_NAME)
        );
        assert_eq!(config.format.line_width, 70);
    }

    #[test]
    fn discover_stops_at_git_boundary() {
        let dir = tempdir().unwrap();
        // Ancestor sets a config we must NOT pick up.
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[format]\nline-width = 70\n",
        )
        .unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let nested = repo.join("src");
        fs::create_dir_all(&nested).unwrap();

        let found = ConfigLoader::discover(&nested).expect("discover");
        assert!(
            found.is_none(),
            "should stop at .git boundary, got {found:?}"
        );
    }

    #[test]
    fn discover_prefers_config_at_repo_root() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(CONFIG_FILE_NAME), "[format]\nline-width = 70\n").unwrap();
        let nested = repo.join("src");
        fs::create_dir_all(&nested).unwrap();

        let (path, config) = ConfigLoader::discover(&nested)
            .expect("discover")
            .expect("found");
        assert_eq!(path, repo.canonicalize().unwrap().join(CONFIG_FILE_NAME));
        assert_eq!(config.format.line_width, 70);
    }

    #[test]
    fn resolve_no_config_returns_defaults() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[format]\nline-width = 20\n",
        )
        .unwrap();
        let (config, source) = ConfigLoader::resolve(None, true, dir.path()).expect("resolve");
        assert_eq!(config, ValidatedConfig::default());
        assert_eq!(source, ConfigSource::None);
    }

    #[test]
    fn resolve_explicit_overrides_discovery() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[format]\nline-width = 20\n",
        )
        .unwrap();
        let explicit = dir.path().join("custom.toml");
        fs::write(&explicit, "[format]\nline-width = 40\n").unwrap();

        let (config, source) =
            ConfigLoader::resolve(Some(&explicit), false, dir.path()).expect("resolve");
        assert_eq!(config.format.line_width, 40);
        assert_eq!(source, ConfigSource::Explicit(explicit.clone()));
    }

    #[test]
    fn resolve_discovers_when_no_explicit_and_not_disabled() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[format]\nline-width = 50\n",
        )
        .unwrap();
        let (config, source) = ConfigLoader::resolve(None, false, dir.path()).expect("resolve");
        assert_eq!(config.format.line_width, 50);
        assert!(matches!(source, ConfigSource::Discovered(_)));
    }

    /// A project directory bounded by a `.git` entry, so the discovery walk in
    /// the global-fallback tests never escapes the tempdir.
    fn bounded_project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        (dir, repo)
    }

    fn write_global(dir: &Path) -> PathBuf {
        let global = dir.join("config-home").join("tex-ls").join("config.toml");
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        fs::write(&global, "[format]\nline-width = 66\n").unwrap();
        global
    }

    #[test]
    fn resolve_falls_back_to_global_config() {
        let (dir, repo) = bounded_project();
        let global = write_global(dir.path());

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(None, false, &repo, None, Some(&global))
                .expect("resolve");
        assert_eq!(config.format.line_width, 66);
        assert_eq!(source, ConfigSource::Global(global.clone()));
        // Global config has no project location: excludes anchor at the caller.
        assert_eq!(source.exclude_root(&repo), repo.as_path());
    }

    #[test]
    fn discovered_config_beats_global() {
        let (dir, repo) = bounded_project();
        let global = write_global(dir.path());
        fs::write(repo.join(CONFIG_FILE_NAME), "[format]\nline-width = 50\n").unwrap();

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(None, false, &repo, None, Some(&global))
                .expect("resolve");
        assert_eq!(config.format.line_width, 50);
        assert!(matches!(source, ConfigSource::Discovered(_)));
    }

    #[test]
    fn no_config_skips_global() {
        let (dir, repo) = bounded_project();
        let global = write_global(dir.path());

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(None, true, &repo, None, Some(&global))
                .expect("resolve");
        assert_eq!(config, ValidatedConfig::default());
        assert_eq!(source, ConfigSource::None);
    }

    #[test]
    fn broken_global_config_is_an_error() {
        let (dir, repo) = bounded_project();
        let global = dir
            .path()
            .join("config-home")
            .join("tex-ls")
            .join("config.toml");
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        fs::write(&global, "[format]\nline-widht = 80\n").unwrap();

        let err = ConfigLoader::resolve_with_fallbacks(None, false, &repo, None, Some(&global))
            .expect_err("typo'd global config must not be silently ignored");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    fn write_env_config(dir: &Path) -> PathBuf {
        let env = dir.join("synced").join("tex-ls.toml");
        fs::create_dir_all(env.parent().unwrap()).unwrap();
        fs::write(&env, "[format]\nline-width = 44\n").unwrap();
        env
    }

    #[test]
    fn env_config_beats_global() {
        let (dir, repo) = bounded_project();
        let env = write_env_config(dir.path());
        let global = write_global(dir.path());

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(None, false, &repo, Some(&env), Some(&global))
                .expect("resolve");
        assert_eq!(config.format.line_width, 44);
        assert_eq!(source, ConfigSource::Env(env.clone()));
        // Env config has no project location: excludes anchor at the caller.
        assert_eq!(source.exclude_root(&repo), repo.as_path());
    }

    #[test]
    fn discovered_config_beats_env() {
        let (dir, repo) = bounded_project();
        let env = write_env_config(dir.path());
        fs::write(repo.join(CONFIG_FILE_NAME), "[format]\nline-width = 50\n").unwrap();

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(None, false, &repo, Some(&env), None)
                .expect("resolve");
        assert_eq!(config.format.line_width, 50);
        assert!(matches!(source, ConfigSource::Discovered(_)));
    }

    #[test]
    fn explicit_config_beats_env() {
        let (dir, repo) = bounded_project();
        let env = write_env_config(dir.path());
        let explicit = dir.path().join("custom.toml");
        fs::write(&explicit, "[format]\nline-width = 40\n").unwrap();

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(Some(&explicit), false, &repo, Some(&env), None)
                .expect("resolve");
        assert_eq!(config.format.line_width, 40);
        assert_eq!(source, ConfigSource::Explicit(explicit.clone()));
    }

    #[test]
    fn no_config_skips_env() {
        let (dir, repo) = bounded_project();
        let env = write_env_config(dir.path());

        let (config, source) =
            ConfigLoader::resolve_with_fallbacks(None, true, &repo, Some(&env), None)
                .expect("resolve");
        assert_eq!(config, ValidatedConfig::default());
        assert_eq!(source, ConfigSource::None);
    }

    #[test]
    fn missing_env_config_is_an_error() {
        let (dir, repo) = bounded_project();
        let env = dir.path().join("nowhere").join("tex-ls.toml");
        let global = write_global(dir.path());

        // A set but dangling `$TEX_LS_CONFIG` must not silently fall through
        // to the global config or the defaults.
        let err =
            ConfigLoader::resolve_with_fallbacks(None, false, &repo, Some(&env), Some(&global))
                .expect_err("typo'd $TEX_LS_CONFIG path must not be silently ignored");
        assert!(matches!(err, ConfigError::Io { .. }));
    }

    #[test]
    fn broken_env_config_is_an_error() {
        let (dir, repo) = bounded_project();
        let env = dir.path().join("synced").join("tex-ls.toml");
        fs::create_dir_all(env.parent().unwrap()).unwrap();
        fs::write(&env, "[format]\nline-widht = 80\n").unwrap();

        let err = ConfigLoader::resolve_with_fallbacks(None, false, &repo, Some(&env), None)
            .expect_err("broken $TEX_LS_CONFIG file must not be silently ignored");
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn exclude_root_for_project_config_is_its_directory() {
        let source = ConfigSource::Discovered(PathBuf::from("/proj/tex-ls.toml"));
        assert_eq!(
            source.exclude_root(Path::new("/elsewhere")),
            Path::new("/proj")
        );
        let explicit = ConfigSource::Explicit(PathBuf::from("/proj/custom.toml"));
        assert_eq!(
            explicit.exclude_root(Path::new("/elsewhere")),
            Path::new("/proj")
        );
        assert_eq!(
            ConfigSource::None.exclude_root(Path::new("/elsewhere")),
            Path::new("/elsewhere")
        );
    }

    // --- declarations (`AGENTS.md` decision #12)
    //
    // The type and its wire spellings are tested in `tex_ls_parser::declarations`;
    // what these pin is the *TOML* surface, which that crate cannot see (`toml` is
    // a dependency of this crate only).

    #[test]
    fn a_project_declares_no_environments_by_default() {
        assert!(parse("").expect("parse").declarations().is_empty());
    }

    #[test]
    fn parses_an_environment_declared_by_behavior() {
        let config = parse("[environments.myenv]\nlike = \"align\"\n").expect("parse");
        let decls = config.declarations();
        assert_eq!(decls.environments["myenv"].like.as_deref(), Some("align"));
    }

    #[test]
    fn parses_a_declared_reference_command() {
        let config = parse("[commands.eqrefs]\nlike = \"cref\"\n").expect("parse");
        let decls = config.declarations();
        assert_eq!(decls.commands["eqrefs"].like.as_deref(), Some("cref"));
    }

    /// TOML literal strings are what spares users `"\\bea"`, and the leading
    /// backslash normalizes away either way.
    #[test]
    fn parses_delimiter_spellings_in_either_toml_string_form() {
        let config = parse("[environments.eqnarray]\nbegin = ['\\bea']\nend = [\"\\\\eea\"]\n")
            .expect("parse");
        let entry = &config.declarations().environments["eqnarray"];
        assert_eq!(entry.begin[0].as_str(), "bea");
        assert_eq!(entry.end[0].as_str(), "eea");
    }

    /// The `\startmyenv … \endmyenv` shape from issue #109: behavior and
    /// spellings in one entry, keyed by the environment it is.
    #[test]
    fn parses_an_environment_declared_by_behavior_and_spellings() {
        let config = parse(
            "[environments.mytheorem]\nlike = \"theorem\"\nbegin = ['\\startmyenv']\nend = ['\\endmyenv']\n",
        )
        .expect("parse");
        let entry = &config.declarations().environments["mytheorem"];
        assert_eq!(entry.like.as_deref(), Some("theorem"));
        assert!(entry.has_delimiters());
    }

    #[test]
    fn a_misspelled_declaration_key_is_rejected() {
        let err = parse("[environments.myenv]\nliek = \"align\"\n")
            .expect_err("unknown key must not be silently ignored");
        assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
        assert!(err.to_string().contains("liek"), "{err}");
    }

    /// An environment may legitimately be named after a config section, so the
    /// map must be its own table rather than sharing a key space with scalars.
    #[test]
    fn an_environment_may_be_named_like_a_config_section() {
        let config = parse("[environments.format]\nlike = \"center\"\n").expect("parse");
        assert!(config.declarations().environments.contains_key("format"));
        assert_eq!(config.format, FormatConfig::default());
    }

    /// The rules themselves are tested in `tex_ls_parser::declarations`; what
    /// this pins is that loading a config *runs* them, so a broken declaration
    /// is reported at load rather than silently doing nothing to the document.
    #[test]
    fn a_broken_declaration_fails_at_load() {
        let err = parse("[environments.myenv]\nlike = \"algin\"\n")
            .expect_err("an unknown `like` target must not load");
        assert!(matches!(err, ConfigError::Declaration { .. }), "{err:?}");
        let rendered = err.to_string();
        assert!(rendered.contains("tex-ls.toml"), "{rendered}");
        assert!(rendered.contains("environments.myenv.like"), "{rendered}");
        assert!(rendered.contains("algin"), "{rendered}");
    }

    /// Issue #117: an opener alone is a complete declaration, since the literal
    /// `\end{eqnarray}` closes it. The TOML surface has to carry that through —
    /// it used to be a load error.
    #[test]
    fn a_declared_opener_without_a_closer_loads() {
        let config = parse("[environments.eqnarray]\nbegin = ['\\bea']\n").expect("loads");
        let entry = &config.declarations().environments["eqnarray"];
        assert_eq!(
            entry.begin,
            vec![tex_ls_parser::declarations::CommandName::new("bea")]
        );
        assert!(entry.end.is_empty());
    }
}
