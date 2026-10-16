//! Synchronization operations.
use lsp_types::TextDocumentContentChangeEvent;
use std::sync::Arc;
use tex_ls_analysis::text::TextBuffer;
use tex_ls_parser::parser::Edit;

#[cfg(test)]
std::thread_local! {
    static BUFFER_CONSTRUCTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn content_change_buffer_constructions() -> usize {
    BUFFER_CONSTRUCTIONS.with(std::cell::Cell::get)
}

/// Original positions remain valid when every preceding edit is strictly after
/// them. Keep another untouched byte when a seam could alter CRLF lookahead.
fn descending_ranges(
    buffer: &TextBuffer,
    changes: &[TextDocumentContentChangeEvent],
) -> Option<Vec<std::ops::Range<usize>>> {
    if changes.len() < 2 {
        return None;
    }
    let mut positions = Vec::with_capacity(changes.len() * 2);
    for change in changes {
        let TextDocumentContentChangeEvent::TextDocumentContentChangePartial(change) = change
        else {
            return None;
        };
        if change.range.start > change.range.end {
            return None;
        }
        for position in [change.range.start, change.range.end] {
            positions.push((position.line, position.character));
        }
    }
    let index = buffer.line_index();
    let offsets = index.offsets_at(&positions);
    let ranges: Vec<_> = offsets
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| pair[0]..pair[1])
        .collect();
    if ranges.windows(2).any(|pair| {
        pair[1].end >= pair[0].start
            || (pair[1].end.checked_add(1) == Some(pair[0].start)
                && buffer.text().as_bytes()[pair[0].start - 1] == b'\r')
    }) {
        return None;
    }
    // Both conversions reuse Unicode prefixes instead of walking a long line
    // once per endpoint. Exact roundtrips exclude clamped and interior positions.
    let mut cursor = index.position_cursor();
    for slot in (0..changes.len()).rev() {
        for side in 0..2 {
            let offset = offsets[slot * 2 + side];
            if !buffer.text().is_char_boundary(offset)
                || cursor.position(offset) != positions[slot * 2 + side]
            {
                return None;
            }
        }
    }
    Some(ranges)
}

/// Apply sequential protocol changes atomically and return parser edit hints.
/// Characters beyond a line's end retain the protocol's clamping behavior.
/// Reversed ranges reject the entire batch, including earlier valid changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentChangeError {
    pub index: usize,
}
impl std::fmt::Display for ContentChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Content change {} has a reversed range", self.index)
    }
}
impl std::error::Error for ContentChangeError {}

pub fn apply_content_changes(
    buffer: &mut Arc<TextBuffer>,
    changes: Vec<TextDocumentContentChangeEvent>,
) -> Result<Option<Vec<Edit>>, ContentChangeError> {
    if let Some(ranges) = descending_ranges(buffer, &changes) {
        let edits: Vec<_> = changes
            .into_iter()
            .zip(ranges)
            .map(|(change, range)| {
                let TextDocumentContentChangeEvent::TextDocumentContentChangePartial(change) =
                    change
                else {
                    unreachable!("descending ranges contain only partial changes")
                };
                Edit {
                    range,
                    insert: change.text,
                }
            })
            .collect();
        let removed: usize = edits.iter().map(|edit| edit.range.len()).sum();
        let inserted: usize = edits.iter().map(|edit| edit.insert.len()).sum();
        let mut text = String::with_capacity(buffer.text().len() - removed + inserted);
        let mut at = 0;
        for edit in edits.iter().rev() {
            text.push_str(&buffer.text()[at..edit.range.start]);
            text.push_str(&edit.insert);
            at = edit.range.end;
        }
        text.push_str(&buffer.text()[at..]);
        #[cfg(test)]
        BUFFER_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));
        *buffer = Arc::new(TextBuffer::new(text, buffer.encoding()));
        return Ok(Some(edits));
    }
    let mut candidate = buffer.clone();
    let mut edits = Some(Vec::with_capacity(changes.len()));
    for (index, change) in changes.into_iter().enumerate() {
        let next = match change {
            TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(change) => {
                edits = None;
                TextBuffer::new(change.text, candidate.encoding())
            }
            TextDocumentContentChangeEvent::TextDocumentContentChangePartial(change) => {
                let range = change.range;
                if range.start > range.end {
                    return Err(ContentChangeError { index });
                }
                let idx = candidate.line_index();
                let start = idx.offset_at(range.start.line, range.start.character);
                let end = idx.offset_at(range.end.line, range.end.character);
                let next = candidate.with_replacement(start..end, &change.text);
                // Recorded after the splice borrowed `change.text`, so the insert
                // moves rather than cloning on every keystroke. `offset_at` answers
                // in bounds and on a char boundary of this text, which is exactly
                // `Edit::fits`, so the chain is well-formed by construction.
                if let Some(edits) = edits.as_mut() {
                    edits.push(Edit {
                        range: start..end,
                        insert: change.text,
                    });
                }
                next
            }
        };
        #[cfg(test)]
        BUFFER_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));
        candidate = Arc::new(next);
    }
    *buffer = candidate;
    Ok(edits)
}
