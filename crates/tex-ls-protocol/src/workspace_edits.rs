//! Versioned workspace-edit presentation from captured client document versions.
use crate::convert::{locations_for_ranges, lsp_range};
use lsp_types::{Location, TextEdit, Uri, WorkspaceEdit};
use rowan::TextRange;
use serde_json::{Value, json};
use std::collections::HashMap;
use tex_ls_analysis::{
    incremental::Analysis,
    text::{LineIndex, PositionEncoding},
};

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

/// Convert a semantics-preserving byte edit to the requested position encoding.
pub fn push_edit(
    changes: &mut HashMap<Uri, Vec<TextEdit>>,
    uri: &Uri,
    idx: &LineIndex,
    range: TextRange,
    new_name: &str,
) {
    changes.entry(uri.clone()).or_default().push(TextEdit {
        range: lsp_range(idx, range),
        new_text: new_name.to_owned(),
    });
}

/// Sort and dedup each file's edits, drop empty files, and wrap the rest in a
/// [`WorkspaceEdit`]. `None` when nothing is left to rewrite (so the handler replies
/// `null`).
pub fn finalize_rename(mut changes: HashMap<Uri, Vec<TextEdit>>) -> Option<WorkspaceEdit> {
    changes.retain(|_, edits| {
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        edits.dedup();
        !edits.is_empty()
    });
    (!changes.is_empty()).then(|| WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Whether `new_name` is a safe replacement key: non-empty after trimming and free of
/// characters that would break the surface syntax or the comma key-list split (so an
/// applied rename can never introduce a parse/format error). Conservative — a few
/// exotic-but-legal key characters are rejected rather than risk a corrupt edit.
pub fn is_valid_key(new_name: &str) -> bool {
    !new_name.trim().is_empty()
        && !new_name.chars().any(|c| {
            matches!(
                c,
                '{' | '}' | '%' | '\\' | ',' | '#' | '~' | '$' | '^' | '&' | '\n' | '\r'
            )
        })
}

/// Whether `new_name` is a safe replacement *command* name (leading `\` already
/// stripped): non-empty ASCII letters, plus `@`/`_`/`:` only when `old_name`
/// already used that character. The old token lexing as one `CONTROL_WORD` proves
/// every occurrence site is inside the right letter-mode region (`\makeatletter`
/// for `@`, expl3 for `_`/`:`), so the new name re-lexes identically there — while
/// a plain name never gains `@`, which would mis-lex in ordinary text. Letters-only
/// also preserves the token boundary at every occurrence (a control word ends at
/// the first non-letter, exactly where the old one did).
pub fn is_valid_command_name(new_name: &str, old_name: &str) -> bool {
    !new_name.is_empty()
        && new_name.chars().all(|c| {
            c.is_ascii_alphabetic() || (matches!(c, '@' | '_' | ':') && old_name.contains(c))
        })
}

pub(crate) fn occurrence_edits(
    snapshot: &Analysis,
    occurrences: Vec<tex_ls_analysis::navigation::KeyOccurrence>,
    new_name: &str,
    encoding: PositionEncoding,
) -> HashMap<Uri, Vec<TextEdit>> {
    let Some(replacements) = occurrences
        .iter()
        .map(|item| snapshot.key_replacement(&item.path, item.key_range, new_name))
        .collect::<Option<Vec<_>>>()
    else {
        return HashMap::new();
    };
    let Some(locations) = locations_for_ranges(
        snapshot,
        occurrences
            .iter()
            .map(|item| (item.path.as_path(), item.key_range)),
        encoding,
    )
    .into_iter()
    .collect::<Option<Vec<_>>>() else {
        return HashMap::new();
    };
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for (location, new_text) in locations.into_iter().zip(replacements) {
        changes.entry(location.uri).or_default().push(TextEdit {
            range: location.range,
            new_text,
        });
    }
    changes
}

pub(crate) fn edits_for_locations(
    locations: impl IntoIterator<Item = Location>,
    new_name: &str,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for location in locations {
        changes.entry(location.uri).or_default().push(TextEdit {
            range: location.range,
            new_text: new_name.to_owned(),
        });
    }
    changes
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
