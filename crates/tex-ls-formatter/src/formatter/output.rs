use super::style::{FormatStyle, LineEnding, apply_line_ending, detect_line_ending};

pub(crate) enum Tail {
    Trivia,
    Comment,
    Protected,
}

pub(crate) fn finish_output(
    output: &mut String,
    ending: LineEnding,
    tail: Tail,
    final_newline: bool,
) {
    let trimmed = match tail {
        Tail::Trivia => output.trim_end_matches([' ', '\t', '\n', '\r']).len(),
        Tail::Comment => output.trim_end_matches(['\n', '\r']).len(),
        Tail::Protected => output.len(),
    };
    output.truncate(trimmed);
    if final_newline
        && !output.is_empty()
        && (!matches!(tail, Tail::Protected) || !output.ends_with('\n'))
    {
        output.push('\n');
    }
    apply_line_ending(output, ending);
}

pub(crate) fn resolve_line_ending<L: rowan::Language>(
    root: &rowan::SyntaxNode<L>,
    style: FormatStyle,
) -> LineEnding {
    if style.line_ending == LineEnding::Auto {
        style.line_ending.resolve(detect_line_ending(&root.text()))
    } else {
        style.line_ending.resolve(LineEnding::Lf)
    }
}
