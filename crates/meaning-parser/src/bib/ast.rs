//! Typed, read-only wrappers over the BibTeX CST.
//!
//! Accessors expose syntax without assigning meaning to fields or entries.
//!
//! Free functions provide the same operations for untyped [`SyntaxNode`] callers.

pub mod nodes;

pub use nodes::{Entry, EntryType, Field, FieldName, Key, StringEntry, Value};

use rowan::TextRange;

use crate::bib::syntax::{SyntaxKind, SyntaxNode};

/// A typed wrapper over BibTeX CST nodes of a particular [`SyntaxKind`].
pub trait AstNode {
    fn can_cast(kind: SyntaxKind) -> bool
    where
        Self: Sized;
    fn cast(syntax: SyntaxNode) -> Option<Self>
    where
        Self: Sized;
    fn syntax(&self) -> &SyntaxNode;
}

/// The first child node castable to `N`.
pub fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

/// All child nodes castable to `N`, in source order.
pub fn children<N: AstNode>(parent: &SyntaxNode) -> impl Iterator<Item = N> {
    parent.children().filter_map(N::cast)
}

/// Locate an entry whose complete range is already known from this tree's model.
/// Range descent avoids constructing cursors for preceding entries and fields.
/// The caller must use the matching tree revision. Non-entry and out-of-bounds
/// ranges return `None`.
pub fn entry_at_range(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    if !root.text_range().contains_range(range) {
        return None;
    }
    let element = root.covering_element(range);
    let node = match element {
        rowan::NodeOrToken::Node(node) => node,
        rowan::NodeOrToken::Token(token) => token.parent()?,
    };
    node.ancestors()
        .find(|node| node.kind() == SyntaxKind::ENTRY && node.text_range() == range)
}

// --- Free-function shims (kind-agnostic; see module docs) ---------------------

/// The entry type of an `ENTRY` / `STRING_ENTRY` / … node — the word following `@`
/// (e.g. `"article"`, `"string"`). `None` for a malformed entry with no `ENTRY_TYPE`
/// child. Case is preserved; callers normalize.
pub fn entry_type(entry: &SyntaxNode) -> Option<String> {
    child::<EntryType>(entry).and_then(|t| t.text())
}

/// The cite key of a regular `ENTRY` and the byte range of its `KEY` node. `None` when
/// the entry has no key (a recovery case) or the key is empty.
pub fn cite_key(entry: &SyntaxNode) -> Option<(String, TextRange)> {
    let key = child::<Key>(entry)?;
    key.text().map(|text| (text, key.syntax().text_range()))
}

/// The macro name defined by a `STRING_ENTRY` (`@string{ name = value }`) and the
/// byte range of its `FIELD_NAME` node. `None` for a malformed `@string` with no
/// `name = …` field.
pub fn string_def_name(string_entry: &SyntaxNode) -> Option<(String, TextRange)> {
    let name = child::<Field>(string_entry)?.name_node()?;
    name.text().map(|text| (text, name.syntax().text_range()))
}

/// The `FIELD` children of an entry, in source order.
pub fn fields(entry: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> {
    children::<Field>(entry).map(|f| f.syntax().clone())
}

/// The name of a `FIELD` (the text of its `FIELD_NAME`), or `None` if absent.
pub fn field_name(field: &SyntaxNode) -> Option<String> {
    child::<FieldName>(field).and_then(|n| n.text())
}

/// The `VALUE` node of a `FIELD` (the right-hand side of `=`), or `None` if absent.
pub fn field_value(field: &SyntaxNode) -> Option<SyntaxNode> {
    child::<Value>(field).map(|v| v.syntax().clone())
}

/// A field `VALUE` as plain display text: the node text, trimmed, with one layer of
/// surrounding `{…}`/`"…"` removed and interior whitespace collapsed. Shared by the
/// LSP hover/completion cards and the semantic model's cached title/author facts.
pub fn value_text_cleaned(value: &SyntaxNode) -> String {
    let raw = value.text().to_string();
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .or_else(|| trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(trimmed);
    inner.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The bare-macro *uses* inside a `VALUE`: each `LITERAL` piece whose single token is
/// a `WORD` (an unquoted, unbraced name) is an `@string` reference. A `LITERAL`
/// wrapping a `NUMBER` is a literal number, not a macro use, and is skipped. Yields
/// `(name, range)` with the range of the `LITERAL` piece.
pub fn value_macro_uses(value: &SyntaxNode) -> impl Iterator<Item = (String, TextRange)> {
    nodes::macro_uses_of(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bib::parse;

    fn node(src: &str, kind: SyntaxKind) -> SyntaxNode {
        parse(src)
            .syntax()
            .descendants()
            .find(|n| n.kind() == kind)
            .unwrap_or_else(|| panic!("a {kind:?} node"))
    }

    #[test]
    fn entry_type_reads_word() {
        let entry = node("@article{k, title = {Hi}}\n", SyntaxKind::ENTRY);
        assert_eq!(entry_type(&entry).as_deref(), Some("article"));
    }

    #[test]
    fn cite_key_reassembles_colon_key() {
        let entry = node("@book{westfahl:space, title = {X}}\n", SyntaxKind::ENTRY);
        let (key, _range) = cite_key(&entry).expect("a key");
        assert_eq!(key, "westfahl:space");
    }

    #[test]
    fn cite_key_none_without_key() {
        let entry = node("@misc{", SyntaxKind::ENTRY);
        assert_eq!(cite_key(&entry), None);
    }

    #[test]
    fn string_def_name_reads_field_name() {
        let s = node("@string{jan = \"January\"}\n", SyntaxKind::STRING_ENTRY);
        let (name, _range) = string_def_name(&s).expect("a name");
        assert_eq!(name, "jan");
    }

    #[test]
    fn fields_and_names() {
        let entry = node("@misc{k, a = {x}, b = 3}\n", SyntaxKind::ENTRY);
        let names: Vec<_> = fields(&entry).filter_map(|f| field_name(&f)).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn value_macro_uses_finds_word_not_number() {
        let field = node("@misc{k, t = pub # {x} # 2020}\n", SyntaxKind::FIELD);
        let value = field_value(&field).expect("a value");
        let uses: Vec<_> = value_macro_uses(&value).map(|(n, _)| n).collect();
        assert_eq!(uses, vec!["pub"]);
    }

    #[test]
    fn cast_is_kind_exact() {
        let entry = node("@article{k, title = {Hi}}\n", SyntaxKind::ENTRY);
        assert!(Entry::cast(entry.clone()).is_some());
        assert!(Field::cast(entry).is_none());
    }

    #[test]
    fn entry_wrapper_reads_type_key_and_fields() {
        let entry = Entry::cast(node("@article{k, a = {x}, b = 3}\n", SyntaxKind::ENTRY)).unwrap();
        assert_eq!(entry.entry_type().as_deref(), Some("article"));
        assert_eq!(entry.cite_key().map(|(k, _)| k).as_deref(), Some("k"));
        let names: Vec<_> = entry.fields().filter_map(|f| f.name()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }
}

#[cfg(test)]
mod range_lookup_tests {
    use super::*;

    #[test]
    fn entry_range_lookup_matches_traversal_over_bib_corpus() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bib_corpus");
        let mut count = 0;
        for file in std::fs::read_dir(directory).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|e| e != "bib") {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            let root = crate::bib::parse(&text).syntax();
            for entry in root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ENTRY)
            {
                assert_eq!(entry_at_range(&root, entry.text_range()), Some(entry));
                count += 1;
            }
        }
        assert!(count > 100);
    }

    #[test]
    fn entry_range_lookup_handles_malformed_unicode_and_non_entry_ranges() {
        for text in [
            "@",
            "@article{key",
            "@article{key,title={😀}}",
            "@article{a,}
@book{b,title={α}}",
        ] {
            let root = crate::bib::parse(text).syntax();
            for node in root.descendants() {
                let expected = root.descendants().find(|entry| {
                    entry.kind() == SyntaxKind::ENTRY && entry.text_range() == node.text_range()
                });
                assert_eq!(entry_at_range(&root, node.text_range()), expected);
            }
            let end = root.text_range().end();
            assert!(
                entry_at_range(&root, TextRange::new(end, end + rowan::TextSize::from(1)))
                    .is_none()
            );
        }
    }
}
