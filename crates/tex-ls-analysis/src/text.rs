//! Text utilities: the language server's live document buffer, and byte-offset
//! ↔ line/column conversion.

pub mod buffer;
pub mod line_index;

pub use buffer::{IntoSourceText, SourceText, TextBuffer};
pub use line_index::{LineCol, LineIndex, LineTable, PositionEncoding};
