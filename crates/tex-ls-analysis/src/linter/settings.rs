//! Lint selection settings shared by native and embedded hosts.
use super::RuleSelection;
/// The `[lint]` section of `tex-ls.toml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LintConfig {
    /// Explicit allowlist of rule IDs. When `Some`, only these rules run.
    #[serde(default)]
    pub select: Option<Vec<String>>,
    /// Rule IDs to disable. Applied on top of either `select` (subtracts) or the
    /// default rule set.
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub external: crate::external::compiler::Filter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownRules(pub Vec<String>);
impl std::fmt::Display for UnknownRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unknown lint rules: {}", self.0.join(", "))
    }
}
impl std::error::Error for UnknownRules {}
impl LintConfig {
    pub fn resolve(&self) -> Result<RuleSelection, UnknownRules> {
        let (mut rules, unknown) = RuleSelection::resolve(self.select.as_deref(), &self.ignore);
        rules.external = self.external.clone();
        if unknown.is_empty() {
            Ok(rules)
        } else {
            Err(UnknownRules(unknown))
        }
    }
}
