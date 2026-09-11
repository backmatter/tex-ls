//! Synchronization operations.
use super::*;

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
        candidate = Arc::new(next);
    }
    *buffer = candidate;
    Ok(edits)
}
