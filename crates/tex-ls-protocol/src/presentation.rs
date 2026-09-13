//! Editor presentation settings; these never enter parsing declarations.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct OutlineOptions {
    pub sections: bool,
    pub frames: bool,
    pub floats: bool,
    pub theorems: bool,
    pub labels: bool,
    pub macros: bool,
    pub environments: bool,
    pub labelled_equations: bool,
    pub unlabelled_equations: bool,
    pub items: bool,
    pub environment_names: BTreeMap<String, String>,
}
impl Default for OutlineOptions {
    fn default() -> Self {
        Self {
            sections: true,
            frames: true,
            floats: true,
            theorems: true,
            labels: true,
            macros: true,
            environments: true,
            labelled_equations: true,
            unlabelled_equations: false,
            items: false,
            environment_names: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct HintOptions {
    pub definitions: bool,
    pub references: bool,
    pub max_length: usize,
}
impl Default for HintOptions {
    fn default() -> Self {
        Self {
            definitions: true,
            references: true,
            max_length: 32,
        }
    }
}
impl HintOptions {
    pub fn validate(&self) -> Result<(), String> {
        if (1..=256).contains(&self.max_length) {
            Ok(())
        } else {
            Err("inlayHints.maxLength must be between 1 and 256".into())
        }
    }
}
