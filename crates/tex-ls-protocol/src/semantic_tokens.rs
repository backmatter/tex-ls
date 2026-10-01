//! Stable syntax classifications, enriched with semantic reference roles.
use super::*;
use serde_json::{Value, json};

pub const TYPES: &[&str] = &[
    "macro",
    "type",
    "variable",
    "property",
    "keyword",
    "string",
    "number",
    "namespace",
];

pub fn compute(
    snapshot: &Analysis,
    path: &Path,
    enc: PositionEncoding,
    range: Option<Range>,
) -> Value {
    let Some(file) = snapshot.lookup_file(path) else {
        return json!({"data":[]});
    };
    let text = snapshot.file_text(file);
    let idx = snapshot.file_line_index(file, enc);
    let mut spans = Vec::new();
    if file_kind_for(path) == FileKind::Bib {
        use tex_ls_parser::bib::syntax::SyntaxKind as K;
        for node in snapshot.parsed_bib_tree(file).descendants() {
            let kind = match node.kind() {
                K::ENTRY_TYPE => 4,
                K::KEY => 2,
                K::FIELD_NAME => 3,
                K::LITERAL => {
                    if node.text().to_string().chars().all(|c| c.is_ascii_digit()) {
                        6
                    } else {
                        2
                    }
                }
                _ => continue,
            };
            spans.push((node.text_range(), kind));
        }
    } else {
        for element in snapshot.parsed_tree(file).descendants_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(token)
                    if matches!(
                        token.kind(),
                        SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL
                    ) =>
                {
                    // Being a control sequence is a syntax fact, even when the
                    // command is unknown or its package has not been acquired.
                    spans.push((token.text_range(), 0));
                }
                rowan::NodeOrToken::Token(token)
                    if token.kind() == SyntaxKind::VERB && token.text().starts_with("\\verb") =>
                {
                    // The lexer preserves inline verbatim as one opaque token.
                    // Highlight the command head and literal body separately;
                    // control sequences printed in the body are not commands.
                    let start = token.text_range().start();
                    spans.push((
                        rowan::TextRange::new(start, start + rowan::TextSize::from(5)),
                        0,
                    ));
                    let after_head = &token.text()[5..];
                    let after_star = after_head.strip_prefix('*').unwrap_or(after_head);
                    if let Some(delimiter) = after_star.chars().next() {
                        let body_start =
                            token.text().len() - after_star.len() + delimiter.len_utf8();
                        let body_end = token.text().len() - delimiter.len_utf8();
                        if body_start < body_end {
                            spans.push((
                                rowan::TextRange::new(
                                    start + rowan::TextSize::try_from(body_start).unwrap(),
                                    start + rowan::TextSize::try_from(body_end).unwrap(),
                                ),
                                5,
                            ));
                        }
                    }
                }
                rowan::NodeOrToken::Node(node)
                    if matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END) =>
                {
                    if let Some(range) = tex_ls_parser::ast::environment_name_range(&node) {
                        // Only literal names are types. Dynamic names can contain
                        // commands, whose own macro classification must survive.
                        let name = &text[usize::from(range.start())..usize::from(range.end())];
                        if !name.is_empty()
                            && name
                                .chars()
                                .all(|ch| ch.is_alphanumeric() || "*_-:@".contains(ch))
                        {
                            spans.push((range, 1));
                        }
                    }
                }
                rowan::NodeOrToken::Node(node) if node.kind() == SyntaxKind::COMMAND => {
                    use tex_ls_parser::semantic::roles::{FileRoleKind, file_role};
                    let name = tex_ls_parser::ast::command_name(&node);
                    // Flat typewriter arguments are printed code examples. Keep
                    // nested TeX commands available for their own classification.
                    if name.as_deref() == Some("texttt")
                        && let Some((range, literal)) =
                            tex_ls_parser::ast::nth_group_inner(&node, 0)
                        && !range.is_empty()
                        && !literal.contains('%')
                    {
                        spans.push((range, 5));
                    }
                    spans.extend(
                        tex_ls_parser::semantic::roles::biblatex_style_arguments(&node)
                            .into_iter()
                            .filter(|(_, range, _)| !range.is_empty())
                            .map(|(_, range, _)| (range, 7)),
                    );
                    if let Some(role) = name.as_deref().and_then(file_role) {
                        let kind = if matches!(
                            role.kind,
                            FileRoleKind::Package
                                | FileRoleKind::Class
                                | FileRoleKind::NamedFile { .. }
                        ) {
                            7
                        } else {
                            5
                        };
                        spans.extend(
                            tex_ls_analysis::external::links::file_argument_spans(&node, role)
                                .into_iter()
                                .map(|(_, range)| (range, kind)),
                        );
                    }
                }
                _ => {}
            }
        }
        let model = snapshot.semantic_model(file);
        spans.extend(model.labels().iter().map(|x| (x.key_range, 2)));
        spans.extend(model.refs().iter().map(|x| (x.key_range, 2)));
        spans.extend(model.citations().iter().map(|x| (x.key_range, 2)));
        spans.extend(model.bibitems().iter().map(|x| (x.key_range, 2)));
        spans.extend(model.glossary_defs().iter().map(|x| (x.key_range, 2)));
        spans.extend(model.glossary_uses().iter().map(|x| (x.key_range, 2)));
    }
    spans.sort_by_key(|(r, _)| (r.start(), r.end()));
    let mut absolute = Vec::new();
    let mut previous_end = 0;
    for (span, kind) in spans {
        let start = usize::from(span.start());
        let end = usize::from(span.end());
        if start < previous_end {
            continue;
        }
        previous_end = end;
        let mut offset = start;
        for line in text[start..end].split_inclusive('\n') {
            let len = line.trim_end_matches(['\r', '\n']).len();
            if len > 0 {
                let r = byte_range_to_lsp(&idx, offset, offset + len);
                if range.is_none_or(|wanted| r.start < wanted.end && r.end > wanted.start) {
                    absolute.push((
                        r.start.line,
                        r.start.character,
                        r.end.character - r.start.character,
                        kind,
                    ));
                }
            }
            offset += line.len();
        }
    }
    let mut data = Vec::new();
    let (mut line, mut column) = (0, 0);
    for (next_line, next_column, len, kind) in absolute {
        data.extend([
            next_line - line,
            if next_line == line {
                next_column - column
            } else {
                next_column
            },
            len,
            kind,
            0,
        ]);
        (line, column) = (next_line, next_column);
    }
    json!({"data":data})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    fn decode(source: &str, enc: PositionEncoding, result: &Value) -> Vec<(usize, usize, usize)> {
        let buffer = TextBuffer::new(source, enc);
        let idx = buffer.line_index();
        let (mut line, mut column) = (0, 0);
        result["data"]
            .as_array()
            .unwrap()
            .as_chunks::<5>()
            .0
            .iter()
            .map(|token| {
                let delta = token[0].as_u64().unwrap() as u32;
                line += delta;
                column = if delta == 0 { column } else { 0 } + token[1].as_u64().unwrap() as u32;
                let start = idx.offset_at(line, column);
                let end = idx.offset_at(line, column + token[2].as_u64().unwrap() as u32);
                (start, end, token[3].as_u64().unwrap() as usize)
            })
            .collect()
    }

    #[test]
    fn printed_code_is_colored_without_making_its_commands_active() {
        let source = "😀 \\texttt{chapters/intro.tex} \\verb|\\projectterm| \\verb*+x y+ \\texttt{\\projectterm}\n";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let result = compute(&db.snapshot(), &path, enc, None);
            let tokens = decode(source, enc, &result);
            let strings: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == 5)
                .map(|(start, end, _)| &source[*start..*end])
                .collect();
            assert_eq!(strings, ["chapters/intro.tex", "\\projectterm", "x y"]);
            let macros: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == 0)
                .map(|(start, end, _)| &source[*start..*end])
                .collect();
            assert_eq!(
                macros,
                ["\\texttt", "\\verb", "\\verb", "\\texttt", "\\projectterm"]
            );
        }
    }

    #[test]
    fn command_colors_are_syntax_based_in_every_context_and_encoding() {
        let source = concat!(
            "😀 \\usepackage{local}\n",
            "\\newcommand{\\custom}[1]{\\textbf{#1}}\n",
            "\\custom{x} \\tem \\packagecommand\n",
            "\\begin{frame}\\begin{customenv}x\\end{customenv}\\end{frame}\n",
            "$\\alpha+\\mathunknown$ \\(\\beta\\) \\[\\gamma\\]\n",
            "\\% \\{ \\} \\\\\n",
            "% \\commenthidden\n",
            "\\verb|\\inlinehidden|\n",
            "\\begin{verbatim}\n\\bodyhidden\n\\end{verbatim}\n",
            "\\ExplSyntaxOn\n\\foo_bar:n {\\l_tmpa_tl}\n\\ExplSyntaxOff\n",
        );
        for extension in ["tex", "sty", "cls", "dtx", "ins"] {
            for newline in ["\n", "\r\n"] {
                let source = source.replace('\n', newline);
                let path =
                    PathBuf::from(fixture_path!("/colors/main.tex")).with_extension(extension);
                let mut db = IncrementalDatabase::default();
                db.apply_change(&path, source.as_str(), None);
                for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                    let before = compute(&db.snapshot(), &path, enc, None);
                    let tokens = decode(&source, enc, &before);
                    let macros: Vec<_> = tokens
                        .iter()
                        .filter(|(_, _, kind)| *kind == 0)
                        .map(|(a, b, _)| &source[*a..*b])
                        .collect();
                    let mut expected = vec![
                        "\\usepackage",
                        "\\newcommand",
                        "\\custom",
                        "\\textbf",
                        "\\custom",
                        "\\tem",
                        "\\packagecommand",
                        "\\begin",
                        "\\begin",
                        "\\end",
                        "\\end",
                        "\\alpha",
                        "\\mathunknown",
                        "\\(",
                        "\\beta",
                        "\\)",
                        "\\[",
                        "\\gamma",
                        "\\]",
                        "\\%",
                        "\\{",
                        "\\}",
                        "\\\\",
                        "\\verb",
                        "\\begin",
                        "\\end",
                        "\\ExplSyntaxOn",
                        "\\foo_bar:n",
                        "\\l_tmpa_tl",
                        "\\ExplSyntaxOff",
                    ];
                    if extension == "dtx" {
                        // A dtx documentation margin is not a TeX comment body.
                        expected.insert(23, "\\commenthidden");
                    }
                    assert_eq!(macros, expected, "{extension}, {enc:?}");
                    let names: Vec<_> = tokens
                        .iter()
                        .filter(|(_, _, kind)| *kind == 1)
                        .map(|(a, b, _)| &source[*a..*b])
                        .collect();
                    assert_eq!(
                        names,
                        [
                            "frame",
                            "customenv",
                            "customenv",
                            "frame",
                            "verbatim",
                            "verbatim"
                        ]
                    );
                    assert!(tokens.windows(2).all(|pair| pair[0].1 <= pair[1].0));
                    let range = Range::new(Position::new(3, 0), Position::new(4, 0));
                    let selected = decode(
                        &source,
                        enc,
                        &compute(&db.snapshot(), &path, enc, Some(range)),
                    );
                    let buffer = TextBuffer::new(source.as_str(), enc);
                    let idx = buffer.line_index();
                    assert_eq!(
                        selected,
                        tokens
                            .iter()
                            .copied()
                            .filter(|(start, end, _)| {
                                let token_range = byte_range_to_lsp(&idx, *start, *end);
                                token_range.start < range.end && token_range.end > range.start
                            })
                            .collect::<Vec<_>>()
                    );
                    db.apply_change(
                        &path.with_file_name("local.sty"),
                        "\\newcommand{\\packagecommand}{}",
                        None,
                    );
                    assert_eq!(
                        before,
                        compute(&db.snapshot(), &path, enc, None),
                        "acquisition must not recolor commands"
                    );
                }
            }
        }
    }
}
