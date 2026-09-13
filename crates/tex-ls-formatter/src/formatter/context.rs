//! Per-run formatter context: bundles the active [`FormatStyle`] with the
//! sentence-mode language options used by lowering.

use super::sentence::SentenceOptions;
use super::style::FormatStyle;

#[derive(Debug, Clone, Copy)]
pub(crate) struct FormatContext<'a> {
    style: FormatStyle,
    /// Language configuration for the [`Sentence`](super::WrapMode::Sentence) /
    /// [`Semantic`](super::WrapMode::Semantic) wrap modes. Borrows the merged
    /// no-break abbreviation slice, so it keeps [`FormatStyle`] `Copy` and out of
    /// the language-config business.
    sentence: SentenceOptions<'a>,
}

impl<'a> FormatContext<'a> {
    pub(crate) fn with_sentence(style: FormatStyle, sentence: SentenceOptions<'a>) -> Self {
        Self { style, sentence }
    }

    pub(crate) fn style(self) -> FormatStyle {
        self.style
    }

    pub(crate) fn sentence(self) -> SentenceOptions<'a> {
        self.sentence
    }
}
