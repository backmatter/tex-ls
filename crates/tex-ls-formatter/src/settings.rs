//! Formatter input settings shared by native and embedded hosts.
use crate::formatter::{FormatStyle, ItemIndent, LineEnding, MathWrap, WrapMode};
use std::collections::BTreeMap;
const DEFAULT_LINE_WIDTH: u32 = 80;
const DEFAULT_INDENT_WIDTH: u32 = 2;
fn default_line_width() -> u32 {
    DEFAULT_LINE_WIDTH
}
fn default_indent_width() -> u32 {
    DEFAULT_INDENT_WIDTH
}
/// The `[format]` section of `tex-ls.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "serde",
    serde(deny_unknown_fields, rename_all = "kebab-case")
)]
pub struct FormatConfig {
    #[cfg_attr(feature = "serde", serde(default = "default_line_width"))]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1000)))]
    pub line_width: u32,
    #[cfg_attr(feature = "serde", serde(default = "default_indent_width"))]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1000)))]
    pub indent_width: u32,
    /// How continuation lines in list items are indented from the `\item`
    /// column. See [`ItemIndent`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub item_indent: ItemIndent,
    /// The paragraph line-break policy. See [`WrapMode`]. When omitted,
    /// every file kind uses [`WrapMode::default`] (`reflow`) — the formatter
    /// declines to reflow content that is unsafe to reflow on its own, in every
    /// mode, so the file's extension is not a layout input.
    #[cfg_attr(feature = "serde", serde(default))]
    pub wrap: Option<WrapMode>,
    /// The display-math line-break policy. See [`MathWrap`]. When omitted
    /// (or `auto`), it derives from the effective `wrap`: `preserve` keeps
    /// authored math breaks, every other wrap mode uses the amsmath-style
    /// breaker.
    #[cfg_attr(feature = "serde", serde(default))]
    pub math_wrap: Option<MathWrap>,
    /// How formatted line breaks are spelled. See [`LineEnding`]. When
    /// omitted (or `auto`), each file keeps the endings it was written with.
    #[cfg_attr(feature = "serde", serde(default))]
    pub line_ending: Option<LineEnding>,
    /// Document language (a BCP-47-style code, e.g. `en`, `de`, `pt-BR`), used by
    /// the `sentence`/`semantic` wrap modes to pick the sentence-boundary
    /// abbreviation profile. Unknown or absent languages fall back to English.
    /// (Auto-detection from babel/polyglossia is not yet implemented.)
    #[cfg_attr(feature = "serde", serde(default))]
    pub lang: Option<String>,
    /// User-supplied no-break abbreviations for the `sentence`/`semantic` wrap
    /// modes, keyed by language code or the literal `default` bucket (applied to
    /// every document). An abbreviation here never ends a sentence, so a line is
    /// not broken after it. Merged on top of the built-in per-language lists.
    #[cfg_attr(feature = "serde", serde(default))]
    pub no_break_abbreviations: BTreeMap<String, Vec<String>>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            line_width: default_line_width(),
            indent_width: default_indent_width(),
            item_indent: ItemIndent::default(),
            wrap: None,
            math_wrap: None,
            line_ending: None,
            lang: None,
            no_break_abbreviations: BTreeMap::new(),
        }
    }
}

impl From<&FormatConfig> for FormatStyle {
    /// Maps the two width knobs. `wrap` is left at [`WrapMode::default`] as a
    /// placeholder: the effective wrap is resolved *per file* by the caller (CLI
    /// flag → configured `wrap` → file-kind default), so this field is always
    /// overwritten before the style reaches the formatter.
    fn from(config: &FormatConfig) -> Self {
        FormatStyle {
            line_width: config.line_width as usize,
            indent_width: config.indent_width as usize,
            item_indent: config.item_indent,
            wrap: WrapMode::default(),
            // Unlike `wrap`, `math-wrap` has no per-file-kind default: the
            // configured value (or `Auto`) maps straight through, and `Auto`
            // resolves against the effective wrap inside the formatter.
            math_wrap: config.math_wrap.unwrap_or(MathWrap::Auto),
            // Also unlike `wrap`: the same for every file kind, and `Auto` is
            // resolved per document inside the formatter (from the endings the
            // source used).
            line_ending: config.line_ending.unwrap_or(LineEnding::Auto),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    pub field: &'static str,
    pub message: String,
}
impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}
impl std::error::Error for SettingsError {}
impl FormatConfig {
    pub fn resolve(&self) -> Result<ResolvedFormatSettings, SettingsError> {
        self.validate()?;
        let (language, no_break) = crate::formatter::sentence::resolve_owned(
            self.lang.as_deref(),
            &self.no_break_abbreviations,
        );
        let mut style = FormatStyle::from(self);
        style.wrap = self.wrap.unwrap_or_default();
        Ok(ResolvedFormatSettings {
            style,
            language,
            no_break,
        })
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        for (field, value) in [
            ("line-width", self.line_width),
            ("indent-width", self.indent_width),
        ] {
            if !(1..=1000).contains(&value) {
                return Err(SettingsError {
                    field,
                    message: format!("must be between 1 and 1000, got {value}"),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFormatSettings {
    pub style: FormatStyle,
    pub language: crate::formatter::sentence::SentenceLanguage,
    pub no_break: Vec<String>,
}
impl ResolvedFormatSettings {
    pub fn sentence_options(&self) -> crate::formatter::SentenceOptions<'_> {
        crate::formatter::SentenceOptions::from_resolved(self.language, &self.no_break)
    }
}
