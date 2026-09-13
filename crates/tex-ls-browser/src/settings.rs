//! Embedding settings assembled from the owning subsystems' input types.
use serde::{Deserialize, Serialize};
use tex_ls_analysis::linter::{RuleSelection, settings::LintConfig};
use tex_ls_formatter::settings::{FormatConfig, ResolvedFormatSettings};
use tex_ls_parser::declarations::{Declarations, ResolvedDeclarations};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub format: FormatConfig,
    pub lint: LintConfig,
    pub declarations: Declarations,
    pub outline: tex_ls_protocol::presentation::OutlineOptions,
    #[serde(rename = "inlayHints")]
    pub inlay_hints: tex_ls_protocol::presentation::HintOptions,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SettingsError {
    pub field: String,
    pub message: String,
}
impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}
impl std::error::Error for SettingsError {}

pub(crate) struct ResolvedSettings {
    pub format: ResolvedFormatSettings,
    pub rules: RuleSelection,
    pub declarations: ResolvedDeclarations,
    pub outline: tex_ls_protocol::presentation::OutlineOptions,
    pub inlay_hints: tex_ls_protocol::presentation::HintOptions,
}
impl Default for ResolvedSettings {
    fn default() -> Self {
        Settings::default().resolve().expect("valid defaults")
    }
}
impl Settings {
    pub(crate) fn resolve(self) -> Result<ResolvedSettings, SettingsError> {
        self.inlay_hints
            .validate()
            .map_err(|message| SettingsError {
                field: "inlayHints".into(),
                message,
            })?;
        let format = self.format.resolve().map_err(|error| SettingsError {
            field: format!("format.{}", error.field),
            message: error.message,
        })?;
        let rules = self.lint.resolve().map_err(|error| SettingsError {
            field: "lint".into(),
            message: error.to_string(),
        })?;
        let declarations = self.declarations.resolve().map_err(|error| SettingsError {
            field: "declarations".into(),
            message: error.to_string(),
        })?;
        Ok(ResolvedSettings {
            format,
            rules,
            declarations,
            outline: self.outline,
            inlay_hints: self.inlay_hints,
        })
    }
}
