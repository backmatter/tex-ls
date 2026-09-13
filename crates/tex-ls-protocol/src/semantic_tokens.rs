//! Conservative semantic highlighting from proved syntax and signature identities.
use super::*;
use serde_json::{Value, json};

pub const TYPES: &[&str] = &[
    "macro", "type", "variable", "property", "keyword", "string", "number",
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
        let scope = snapshot.scope_signatures(file);
        for element in snapshot.parsed_tree(file).descendants_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::CONTROL_WORD => {
                    let name = token.text().trim_start_matches('\\');
                    if crate::hover::lookup_command(scope, name).is_some() {
                        spans.push((token.text_range(), 0));
                    }
                }
                rowan::NodeOrToken::Node(node)
                    if matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END) =>
                {
                    if let Some(name) = tex_ls_parser::ast::environment_name(&node)
                        && crate::hover::lookup_environment(scope, &name).is_some()
                        && let Some(range) = tex_ls_parser::ast::environment_name_range(&node)
                    {
                        spans.push((range, 1));
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
