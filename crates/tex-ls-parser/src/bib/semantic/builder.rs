//! Builds the bib [`Model`] in a single CST walk, then a resolve pass.
//!
//! Mirrors [`crate::semantic::builder`]: one `root.descendants()` pass collects
//! entries, `@string` definitions, and `@string` uses; then `resolve` flags
//! duplicate cite keys and marks each use resolved or unresolved. The model
//! exposes facts; consumers decide how to diagnose them.

use std::collections::HashSet;

use smol_str::SmolStr;

use crate::bib::ast;
use crate::bib::semantic::Model;
use crate::bib::semantic::entry::{Entry, StringDef, StringUse};
use crate::bib::syntax::{SyntaxKind, SyntaxNode};

/// Month abbreviations BibTeX/biber predefine as `@string` macros. A bare use of one
/// is always resolved, so whitelisting them avoids false "undefined string" findings.
pub const MONTH_MACROS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Build the model from a bib parse-tree root.
pub fn build(root: &SyntaxNode) -> Model {
    let mut model = Model::default();
    let mut walk = root.preorder();
    while let Some(event) = walk.next() {
        let rowan::WalkEvent::Enter(node) = event else {
            continue;
        };
        match node.kind() {
            SyntaxKind::ENTRY => collect_entry(&node, &mut model),
            SyntaxKind::STRING_ENTRY => collect_string(&node, &mut model),
            _ => continue,
        }
        // BibTeX entries cannot nest entries; collect_entry/string already read
        // all their field facts. Quoted/braced value text has no definitions.
        walk.skip_subtree();
    }
    resolve(&mut model);
    model
}

/// Record a regular entry (when it has a key) and any `@string` uses in its values.
fn collect_entry(entry: &SyntaxNode, model: &mut Model) {
    let key = ast::cite_key(entry);
    let mut title = None;
    let mut author: Option<Option<SmolStr>> = None;
    let mut editor = None;
    for field in ast::fields(entry) {
        let Some(value) = ast::field_value(&field) else {
            continue;
        };
        collect_value_uses(&value, model);
        if key.is_some()
            && !(title.is_some() && author.as_ref().is_some_and(Option::is_some))
            && let Some(name) = ast::field_name(&field)
        {
            let target = if name.eq_ignore_ascii_case("title") {
                &mut title
            } else if name.eq_ignore_ascii_case("author") {
                &mut author
            } else if name.eq_ignore_ascii_case("editor") {
                &mut editor
            } else {
                continue;
            };
            // Retain the first actual VALUE even if its cleaned content is empty.
            // An empty first author still falls back to editor, not a later author.
            if target.is_none() {
                *target = Some(cleaned(value));
            }
        }
    }
    if let Some((key, key_range)) = key {
        model.entries.push(Entry {
            entry_type: SmolStr::new(ast::entry_type(entry).unwrap_or_default().to_lowercase()),
            key: SmolStr::new(key),
            title: title.flatten(),
            authors: author.flatten().or_else(|| editor.flatten()),
            key_range,
            range: entry.text_range(),
            duplicate: false,
        });
    }
}

fn cleaned(value: SyntaxNode) -> Option<SmolStr> {
    let text = ast::value_text_cleaned(&value);
    (!text.is_empty()).then(|| SmolStr::new(text))
}

/// Record an `@string` definition and any `@string` uses in its (concatenated) value.
fn collect_string(string_entry: &SyntaxNode, model: &mut Model) {
    if let Some((name, range)) = ast::string_def_name(string_entry) {
        model.string_defs.push(StringDef {
            name: SmolStr::new(name.to_lowercase()),
            range,
        });
    }
    collect_uses(string_entry, model);
}

/// Collect the bare-macro uses across every field value of `node`.
fn collect_uses(node: &SyntaxNode, model: &mut Model) {
    for field in ast::fields(node) {
        let Some(value) = ast::field_value(&field) else {
            continue;
        };
        collect_value_uses(&value, model);
    }
}

fn collect_value_uses(value: &SyntaxNode, model: &mut Model) {
    for (name, range) in ast::value_macro_uses(value) {
        model.string_uses.push(StringUse {
            name: SmolStr::new(name.to_lowercase()),
            range,
            resolved: false,
        });
    }
}

/// Flag duplicate cite keys and mark each `@string` use resolved or not.
fn resolve(model: &mut Model) {
    // Duplicate cite keys (case-insensitive; the first occurrence stays `false`).
    let mut seen: HashSet<SmolStr> = HashSet::new();
    for entry in &mut model.entries {
        let folded = SmolStr::new(entry.key.to_lowercase());
        if !seen.insert(folded) {
            entry.duplicate = true;
        }
    }

    // Undefined `@string` uses: defined names are the in-file defs plus the predefined
    // month macros. Whole-file set, order-independent (no forward-reference rule).
    let mut defined: HashSet<SmolStr> = model.string_defs.iter().map(|d| d.name.clone()).collect();
    defined.extend(MONTH_MACROS.iter().map(|m| SmolStr::new(*m)));
    for string_use in &mut model.string_uses {
        string_use.resolved = defined.contains(&string_use.name);
    }
}

#[cfg(test)]
#[path = "builder_reference.rs"]
mod reference;
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    fn compare(source: &str) {
        let root = crate::bib::parse(source).syntax();
        assert_eq!(build(&root), reference::build(&root), "{source}");
    }
    #[test]
    fn collection_matches_original_through_recovery_and_duplicate_fields() {
        for source in [
            "@misc{k,title={},TITLE={later},author={},author={later},editor={Fallback}}",
            "@misc{k,title=,title={later},author=,editor=macro # {Editor}}",
            "@string{publisher=other}@misc{K,title={@string{x=ignored}},author=publisher}@misc{k,title=jan}",
            "@misc{,title=missing,editor=jan}@comment{tex-ls-lint skip}@misc{key,author=unknown}",
            "@preamble{unknown}@string{foo=bar # baz}@misc{k,title={Nested {ABC}},editor={É Name}}",
        ] {
            compare(source);
            for (at, c) in source.char_indices() {
                let mut changed = source.to_owned();
                changed.replace_range(at..at + c.len_utf8(), "");
                compare(&changed);
            }
        }
    }
    proptest! {
        #[test]
        fn collection_matches_reference_on_malformed_sequences(parts in prop::collection::vec(prop::sample::select(vec!["@misc{", "@string{", "@comment{", "title=", "author=", "editor=", "{", "}", "\"", ",", "#", "key", "jan", "É", " ", "\n"]),0..80)) {
            compare(&parts.concat());
        }
    }
}
