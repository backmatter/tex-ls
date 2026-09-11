//! Native configuration-file schema and validation.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use meaning_analysis::linter::RuleSelection;
pub use meaning_analysis::linter::settings::LintConfig;
pub use meaning_formatter::settings::FormatConfig;
use meaning_parser::declarations::{
    CommandDecl, CommandDecls, DeclarationError, Declarations, EnvironmentDecl, EnvironmentDecls,
    ResolvedDeclarations,
};

pub const CONFIG_FILE_NAME: &str = "meaning.toml";

/// Environment variable naming a config file to use when no project
/// `meaning.toml` is discovered. Sits between project discovery and the
/// global user config in `Config::resolve`'s
/// precedence, and shadows the global file entirely when set.
pub const CONFIG_ENV_VAR: &str = "MEANING_CONFIG";

/// Built-in exclude patterns applied when `exclude` is unset (a present `exclude`
/// replaces them). Kept deliberately small: meaning only ever processes
/// `.tex`/`.sty`/`.cls`/`.dtx`/`.ins`/`.bib`, so most generated-file noise never
/// reaches discovery anyway. Tune as real-world LaTeX trees demand. `extend-exclude`
/// is always layered on top of whichever base is in effect.
pub const DEFAULT_EXCLUDE: &[&str] = &[".git/"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Gitignore-style patterns to exclude from directory discovery, resolved
    /// relative to the directory containing this `meaning.toml`. Applies to *both*
    /// `format` and `lint` (which share one file walk), so it is a top-level key,
    /// not nested under `[format]`.
    ///
    /// When present it **replaces** the built-in [`DEFAULT_EXCLUDE`] set (Ruff's
    /// `exclude` semantics); when absent the defaults apply. Either way,
    /// [`extend_exclude`](Self::extend_exclude) is added on top.
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// Gitignore-style patterns added *in addition to* whichever base set
    /// [`exclude`](Self::exclude) selects (Ruff's `extend-exclude` semantics). Use
    /// this to skip a few extra paths without restating the defaults.
    #[serde(default)]
    pub extend_exclude: Vec<String>,
    /// Formatter settings from the `[format]` section.
    #[serde(default)]
    pub format: FormatConfig,
    /// Linter rule selection from the `[lint]` section.
    #[serde(default)]
    pub lint: LintConfig,
    /// Build-artifact locations from the `[build]` section.
    #[serde(default)]
    pub build: BuildConfig,
    /// Project-defined reference and citation command families. Unlike
    /// environment declarations, these affect semantic analysis only.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, CommandDecl>")]
    pub commands: CommandDecls,
    /// The `[environments.<name>]` declaration map: what a user-defined
    /// environment behaves like, and which command spellings stand in for its
    /// delimiters (`\bea`/`\eea`).
    ///
    /// Different in kind from the sections above, because it reaches the
    /// *parser* rather than the formatter or the linter (`AGENTS.md` decision
    /// #12). It is deserialized straight into the `meaning-parser` type instead
    /// of a local mirror: serde is a hard dependency of that crate, so unlike
    /// `FormatStyle` there is no feature to turn on, and a mirror would only add
    /// a second wire spelling that could drift from the one the dprint plugin
    /// reads.
    ///
    /// A top-level map rather than a nested section, and a keyed table rather
    /// than an `[[environments]]` array: the key *is* the environment, so
    /// entries merge per name once config layers or per-file overrides appear.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, EnvironmentDecl>")]
    pub environments: EnvironmentDecls,
}

impl Config {
    /// The final, ordered exclude pattern list: the base set (configured
    /// `exclude`, or [`DEFAULT_EXCLUDE`] when unset) followed by `extend-exclude`.
    /// `extra` (the CLI `--exclude` patterns) is appended last so command-line
    /// excludes always layer on top. The
    /// `ExcludeFilter` compiles this list.
    pub fn exclude_patterns(&self, extra: &[String]) -> Vec<String> {
        let mut patterns: Vec<String> = match &self.exclude {
            Some(patterns) => patterns.clone(),
            None => DEFAULT_EXCLUDE.iter().map(|p| p.to_string()).collect(),
        };
        patterns.extend(self.extend_exclude.iter().cloned());
        patterns.extend(extra.iter().cloned());
        patterns
    }

    /// The project's [`Declarations`], gathered from the top-level declaration
    /// maps. One value rather than a field per map, because everything
    /// downstream — resolution, the `ParseCtx` seed, the salsa input — takes the
    /// whole vocabulary at once, and because the other front ends (the dprint
    /// plugin, a future comment directive) produce a `Declarations` with no
    /// `Config` in sight.
    pub fn declarations(&self) -> Declarations {
        Declarations {
            commands: self.commands.clone(),
            environments: self.environments.clone(),
        }
    }
}

/// The `[build]` section: where the TeX compiler leaves its artifacts, and which
/// file it was run on. Read by the language server only (label-number hover and
/// document symbols pull resolved numbers from the `.aux`; forward search locates
/// the compiled PDF); never by the formatter or linter, which stay hermetic (see
/// `AGENTS.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct BuildConfig {
    /// Directory holding the build's `.aux` files (latexmk's `-auxdir`/`-outdir`),
    /// resolved relative to the root document's directory when not absolute. When
    /// unset, each document's `.aux` is expected next to it (plain
    /// `latex`/`pdflatex` runs).
    #[serde(default)]
    pub aux_dir: Option<PathBuf>,
    /// Directory holding the build's PDF output (latexmk's `-outdir`), resolved
    /// relative to the root document's directory when not absolute. When unset,
    /// the PDF is expected next to the root document. Read by forward search.
    #[serde(default)]
    pub pdf_dir: Option<PathBuf>,
    /// The compiled PDF's file name, when the build does not name it after the
    /// root document (latexmk's `-jobname`). A bare file name resolved inside
    /// [`pdf_dir`](Self::pdf_dir), never a path; `.pdf` is appended when it
    /// carries no extension.
    #[serde(default)]
    pub pdf_filename: Option<String>,
    /// The project's root document — the file the compiler was run on — resolved
    /// relative to this `meaning.toml`'s directory when not absolute.
    ///
    /// Overrides the include-graph scan for a `\documentclass`/`\begin{document}`
    /// member, which can only see files the server has already loaded: editing
    /// `chapters/ch1.tex` in a project rooted at `../main.tex` seeds only
    /// `chapters/`, so the scan finds no root at all.
    #[serde(default)]
    pub root: Option<PathBuf>,
}

impl BuildConfig {
    pub fn validate(&self, path: Option<&Path>) -> Result<(), ConfigError> {
        // A directory here would be silently ignored (the name is joined onto
        // `pdf-dir`), so reject it rather than resolve a PDF the user did not
        // mean.
        if let Some(name) = &self.pdf_filename
            && Path::new(name).components().count() != 1
        {
            return Err(ConfigError::InvalidValue {
                path: path.map(Path::to_path_buf),
                field: "pdf-filename",
                message: format!(
                    "must be a bare file name; use `pdf-dir` for the directory, got `{name}`"
                ),
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
    InvalidValue {
        path: Option<PathBuf>,
        field: &'static str,
        message: String,
    },
    /// A declaration broke one of its rules. Separate from
    /// [`InvalidValue`](Self::InvalidValue) because the offending key is
    /// dynamic (`environments.myenv.like`, not a fixed field name) and the
    /// explanation is the parser crate's to give.
    Declaration {
        path: Option<PathBuf>,
        source: DeclarationError,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::Parse {
                path,
                line,
                column,
                message,
            } => write!(f, "{}:{line}:{column}: {message}", path.display()),
            Self::InvalidValue {
                path,
                field,
                message,
            } => match path {
                Some(path) => write!(f, "{}: invalid `{field}`: {message}", path.display()),
                None => write!(f, "invalid `{field}`: {message}"),
            },
            Self::Declaration { path, source } => match path {
                Some(path) => write!(f, "{}: {source}", path.display()),
                None => write!(f, "{source}"),
            },
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Declaration { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A configuration whose declarations and subsystem settings have passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedConfig {
    input: Config,
    declarations: ResolvedDeclarations,
    rules: RuleSelection,
    resolved_format: meaning_formatter::settings::ResolvedFormatSettings,
}
impl Default for ValidatedConfig {
    fn default() -> Self {
        Config::default()
            .into_validated(None)
            .expect("valid defaults")
    }
}
impl std::ops::Deref for ValidatedConfig {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.input
    }
}
impl ValidatedConfig {
    pub fn formatter_settings(&self) -> &meaning_formatter::settings::ResolvedFormatSettings {
        &self.resolved_format
    }
    pub fn rules(&self) -> &RuleSelection {
        &self.rules
    }
    pub fn resolved_declarations(&self) -> &ResolvedDeclarations {
        &self.declarations
    }
}

impl Config {
    pub fn parse_str(text: &str, path: &Path) -> Result<ValidatedConfig, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|err| {
            let (line, column) = match err.span() {
                Some(span) => byte_offset_to_line_col(text, span.start),
                None => (1, 1),
            };
            ConfigError::Parse {
                path: path.to_path_buf(),
                line,
                column,
                message: err.message().to_string(),
            }
        })?;
        config.into_validated(Some(path))
    }

    pub fn into_validated(self, path: Option<&Path>) -> Result<ValidatedConfig, ConfigError> {
        let (declarations, rules, resolved_format) = self.validate_values(path)?;
        Ok(ValidatedConfig {
            input: self,
            declarations,
            rules,
            resolved_format,
        })
    }

    pub fn validate(&self, path: Option<&Path>) -> Result<(), ConfigError> {
        self.validate_values(path).map(|_| ())
    }

    fn validate_values(
        &self,
        path: Option<&Path>,
    ) -> Result<
        (
            ResolvedDeclarations,
            RuleSelection,
            meaning_formatter::settings::ResolvedFormatSettings,
        ),
        ConfigError,
    > {
        let resolved_format = self
            .format
            .resolve()
            .map_err(|error| ConfigError::InvalidValue {
                path: path.map(Path::to_path_buf),
                field: error.field,
                message: error.message,
            })?;
        self.build.validate(path)?;
        let rules = self
            .lint
            .resolve()
            .map_err(|error| ConfigError::InvalidValue {
                path: path.map(Path::to_path_buf),
                field: "lint",
                message: error.to_string(),
            })?;
        self.declarations()
            .resolve()
            .map(|declarations| (declarations, rules, resolved_format))
            .map_err(|source| ConfigError::Declaration {
                path: path.map(Path::to_path_buf),
                source,
            })
    }
}

fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    let clamped = offset.min(source.len());
    for ch in source[..clamped].chars() {
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
