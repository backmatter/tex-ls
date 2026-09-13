//! Bibliography entries, strings and field children in source order.
use crate::bib::{
    ast,
    semantic::Model,
    syntax::{SyntaxKind, SyntaxNode},
};
use rowan::TextRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibSymbolKind {
    Book,
    Article,
    Collection,
    Entry,
    String,
    Field,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BibOutlineItem {
    pub name: String,
    pub detail: String,
    pub kind: BibSymbolKind,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub children: Vec<BibOutlineItem>,
}

pub fn outline(model: &Model, root: &SyntaxNode) -> Vec<BibOutlineItem> {
    let mut items: Vec<_> = model
        .entries()
        .iter()
        .map(|entry| {
            let children = ast::entry_at_range(root, entry.range)
                .into_iter()
                .flat_map(|node| ast::fields(&node).collect::<Vec<_>>())
                .filter_map(|field| {
                    let name = ast::field_name(&field)?;
                    let selection_range = field
                        .children()
                        .find(|node| node.kind() == SyntaxKind::FIELD_NAME)?
                        .text_range();
                    Some(BibOutlineItem {
                        name,
                        detail: String::new(),
                        kind: BibSymbolKind::Field,
                        range: field.text_range(),
                        selection_range,
                        children: Vec::new(),
                    })
                })
                .collect();
            BibOutlineItem {
                name: entry.key.to_string(),
                detail: entry.entry_type.to_string(),
                kind: match entry.entry_type.as_str() {
                    "book" | "mvbook" | "manual" | "thesis" | "phdthesis" | "mastersthesis" => {
                        BibSymbolKind::Book
                    }
                    "article" | "inbook" | "incollection" | "inproceedings" => {
                        BibSymbolKind::Article
                    }
                    "collection" | "mvcollection" | "proceedings" | "mvproceedings" => {
                        BibSymbolKind::Collection
                    }
                    _ => BibSymbolKind::Entry,
                },
                range: entry.range,
                selection_range: entry.key_range,
                children,
            }
        })
        .collect();
    for node in root
        .children()
        .filter(|node| node.kind() == SyntaxKind::STRING_ENTRY)
    {
        if let Some((name, selection_range)) = ast::string_def_name(&node) {
            items.push(BibOutlineItem {
                name,
                detail: "string".into(),
                kind: BibSymbolKind::String,
                range: node.text_range(),
                selection_range,
                children: Vec::new(),
            });
        }
    }
    items.sort_by_key(|item| item.range.start());
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn symbols_include_strings_and_fields_but_not_comments() {
        let source = "@string{pub = {Publisher}}\n@comment{x}\n@book{k, title = {Title}}";
        let root = crate::bib::parse(source).syntax();
        let items = outline(&Model::build(&root), &root);
        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["pub", "k"]
        );
        assert_eq!(items[0].kind, BibSymbolKind::String);
        assert_eq!(items[1].kind, BibSymbolKind::Book);
        assert_eq!(&source[items[1].children[0].selection_range], "title");
    }
}
