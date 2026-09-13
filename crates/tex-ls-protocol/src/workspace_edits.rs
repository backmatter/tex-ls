//! Versioned workspace-edit presentation from captured client document versions.
use serde_json::{Value, json};
use std::collections::HashMap;

/// Convert workspace edits (including code-action edits) using captured versions.
/// Closed backing sources carry null versions as required by the wire format.
pub fn attach_versions(value: &mut Value, versions: &HashMap<String, i32>) {
    match value {
        Value::Object(object) => {
            if object.get("changes").is_some_and(Value::is_object)
                && let Some(Value::Object(changes)) = object.remove("changes")
            {
                let edits: Vec<_> = changes.into_iter().map(|(uri, edits)| {
                    json!({"textDocument":{"version":versions.get(&uri),"uri":uri},"edits":edits})
                }).collect();
                object.insert("documentChanges".into(), Value::Array(edits));
            }
            for value in object.values_mut() {
                attach_versions(value, versions);
            }
        }
        Value::Array(values) => {
            for value in values {
                attach_versions(value, versions);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_versions_come_from_the_captured_request() {
        let mut edit =
            json!({"changes":{fixture_uri!("/main.tex"):[],fixture_uri!("/backing.tex"):[]}});
        attach_versions(
            &mut edit,
            &HashMap::from([(fixture_uri!("/main.tex").into(), 7)]),
        );
        assert!(edit.get("changes").is_none());
        assert_eq!(edit["documentChanges"][1]["textDocument"]["version"], 7);
        assert_eq!(
            edit["documentChanges"][0]["textDocument"]["version"],
            Value::Null
        );
    }
}
