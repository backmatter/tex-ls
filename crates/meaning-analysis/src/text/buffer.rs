//! Immutable source text and shared position tables.

use super::line_index::{LineIndex, LineTable, PositionEncoding};
use std::ops::{Deref, Range};
use std::sync::{Arc, OnceLock};

/// Source bytes and their encoding-independent position table.
#[derive(Debug)]
pub struct SourceText {
    text: String,
    table: OnceLock<LineTable>,
}

impl SourceText {
    pub fn new(text: String) -> Self {
        Self {
            text,
            table: OnceLock::new(),
        }
    }

    pub fn line_index(&self, encoding: PositionEncoding) -> LineIndex<'_> {
        LineIndex::with_table(&self.text, self.line_table(), encoding)
    }

    fn line_table(&self) -> &LineTable {
        self.table.get_or_init(|| LineTable::new(&self.text))
    }

    pub fn with_replacement(&self, range: Range<usize>, insert: &str) -> Self {
        let removed = self.text[range.clone()].len();
        let mut text = String::with_capacity(self.text.len() - removed + insert.len());
        text.push_str(&self.text[..range.start]);
        text.push_str(insert);
        text.push_str(&self.text[range.end..]);
        let table = OnceLock::new();
        if let Some(current) = self.table.get() {
            let mut patched = current.clone();
            patched.patch(range, insert.len(), &text);
            debug_assert!(patched == LineTable::new(&text));
            let _ = table.set(patched);
        }
        Self { text, table }
    }
}

impl Deref for SourceText {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}
impl PartialEq for SourceText {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}
impl Eq for SourceText {}

/// Constructors accept owned bytes or an existing shared revision.
pub trait IntoSourceText {
    fn into_source_text(self) -> Arc<SourceText>;
}
impl IntoSourceText for String {
    fn into_source_text(self) -> Arc<SourceText> {
        Arc::new(SourceText::new(self))
    }
}
impl IntoSourceText for &str {
    fn into_source_text(self) -> Arc<SourceText> {
        self.to_owned().into_source_text()
    }
}
impl IntoSourceText for Arc<SourceText> {
    fn into_source_text(self) -> Arc<SourceText> {
        self
    }
}

/// Protocol position encoding paired with a shared source revision.
#[derive(Debug)]
pub struct TextBuffer {
    text: Arc<SourceText>,
    encoding: PositionEncoding,
}
impl TextBuffer {
    pub fn new(text: impl IntoSourceText, encoding: PositionEncoding) -> Self {
        Self {
            text: text.into_source_text(),
            encoding,
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn text_arc(&self) -> Arc<SourceText> {
        Arc::clone(&self.text)
    }
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }
    pub fn line_index(&self) -> LineIndex<'_> {
        self.text.line_index(self.encoding)
    }
    #[cfg(test)]
    fn line_table(&self) -> &LineTable {
        self.text.line_table()
    }
    pub fn with_replacement(&self, range: Range<usize>, insert: &str) -> Self {
        Self {
            text: Arc::new(self.text.with_replacement(range, insert)),
            encoding: self.encoding,
        }
    }
}
impl Deref for TextBuffer {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_string_allocation_moves_into_shared_source() {
        let text = String::from("Unicode 𝕏\r\ntext");
        let pointer = text.as_ptr();
        let source = text.into_source_text();
        assert_eq!(source.as_ptr(), pointer);
        let utf8 = TextBuffer::new(source.clone(), PositionEncoding::Utf8);
        let utf16 = TextBuffer::new(source, PositionEncoding::Utf16);
        assert!(std::ptr::eq(utf8.line_table(), utf16.line_table()));
        assert_eq!(utf8.line_index().position(12), (0, 12));
        assert_eq!(utf16.line_index().position(12), (0, 10));
    }

    #[test]
    fn source_equality_ignores_lazy_index_state() {
        let left = SourceText::new("𝕏\r\n".into());
        let right = SourceText::new("𝕏\r\n".into());
        left.line_index(PositionEncoding::Utf16);
        assert_eq!(left, right);
    }

    fn buffer(text: &str) -> TextBuffer {
        TextBuffer::new(text, PositionEncoding::Utf16)
    }

    /// The table is scanned once per buffer, not once per query: `line_index`
    /// only pairs it with the text.
    #[test]
    fn the_line_table_is_built_once_and_shared() {
        let buf = buffer("ab\ncd\nef");
        let first = buf.line_table();
        assert_eq!(buf.line_index().line_start(1), 3);
        assert!(std::ptr::eq(buf.line_table(), first));
    }

    #[test]
    fn the_index_answers_in_the_buffers_encoding() {
        // "𝕏" is 4 UTF-8 bytes and 2 UTF-16 units, so the encodings disagree
        // about the column just past it.
        let utf16 = TextBuffer::new("a𝕏b", PositionEncoding::Utf16);
        let utf8 = TextBuffer::new("a𝕏b", PositionEncoding::Utf8);
        let off = "a𝕏".len();
        assert_eq!(utf16.line_index().position(off), (0, 3));
        assert_eq!(utf8.line_index().position(off), (0, 5));
    }

    /// The point of the `Arc<str>` representation: handing the text out shares
    /// one allocation, and an edit yields a new buffer without disturbing
    /// handles taken before it — which is what lets the salsa layer and every
    /// in-flight read job hold the text without copying it.
    #[test]
    fn an_edit_leaves_earlier_handles_alone() {
        let before = buffer("ab\ncd");
        let handle = before.text_arc();
        assert!(Arc::ptr_eq(&handle, &before.text_arc()));

        let after = before.with_replacement(2..2, "\nxy");
        assert_eq!(&**handle, "ab\ncd");
        assert_eq!(after.text(), "ab\nxy\ncd");
        assert!(!Arc::ptr_eq(&handle, &after.text_arc()));
        assert_eq!(after.line_index().line_start(1), 3);
    }

    /// The keystroke path's whole claim: the edited buffer arrives with a table
    /// already, patched off the one before it, so no rescan happens. Nothing a
    /// caller can observe changes when this regresses — the next query just
    /// rebuilds — so it is asserted on the `OnceLock` directly.
    #[test]
    fn an_edit_patches_the_table_onto_the_new_buffer() {
        let before = buffer("alpha\nbeta\ngamma\n");
        // What `apply_content_changes` does before splicing, and so what makes
        // the table present.
        assert_eq!(before.line_index().offset_at(1, 0), 6);

        let after = before.with_replacement(6..6, "x\ny\n");
        assert!(
            after.text.table.get().is_some(),
            "the edited buffer must arrive with a table, not rebuild one"
        );
        assert_eq!(after.text(), "alpha\nx\ny\nbeta\ngamma\n");
        // The patched table has to answer for the lines the edit created, not
        // merely exist.
        assert_eq!(after.line_index().line_start(3), 10);
        assert_eq!(after.line_index().offset_at(4, 2), 17);
    }

    /// The other direction, which is what keeps the patch from costing a scan on
    /// a document nobody asks a positional question about: no table in, no table
    /// out.
    #[test]
    fn an_edit_to_an_unindexed_buffer_builds_no_table() {
        let before = buffer("alpha\nbeta\n");
        let after = before.with_replacement(0..0, "x");
        assert!(after.text.table.get().is_none());
        assert_eq!(after.text(), "xalpha\nbeta\n");
    }

    /// A `didChange` batch chains buffers, so the second edit patches a table
    /// that is itself patched. The `debug_assert` inside `with_replacement` is
    /// what checks each step; this pins that the chain happens at all.
    #[test]
    fn a_chain_of_edits_keeps_patching() {
        let mut buf = buffer("one\ntwo\n");
        assert_eq!(buf.line_index().line_start(1), 4);
        for insert in ["\r\n", "x", "\r"] {
            buf = buf.with_replacement(4..4, insert);
            assert!(buf.text.table.get().is_some());
        }
        assert_eq!(buf.text(), "one\n\rx\r\ntwo\n");
    }

    /// A malformed range must panic where [`String::replace_range`] would.
    /// Rebuilding the text around the splice can no longer rely on the string
    /// machinery to reject one, and the arithmetic that replaced it accepts
    /// both shapes below: a reversed range measures zero and would duplicate
    /// the region it names, an out-of-bounds one underflows.
    #[test]
    #[should_panic(expected = "byte range starts at 4 but ends at 2")]
    #[expect(
        clippy::reversed_empty_ranges,
        reason = "the malformed range is the subject of the test"
    )]
    fn a_reversed_edit_range_panics() {
        buffer("abcdefgh").with_replacement(4..2, "Z");
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn an_out_of_bounds_edit_range_panics() {
        buffer("abcdefgh").with_replacement(0..99, "Z");
    }

    #[test]
    #[should_panic(expected = "not a char boundary")]
    fn an_edit_range_off_a_char_boundary_panics() {
        buffer("\u{1F600}x").with_replacement(1..2, "Z");
    }
}
