//! Theme-controlled syntax purposes, enriched with source declarations.
#[cfg(test)]
use crate::convert::byte_range_to_lsp;
use crate::convert::file_kind_for;
use lsp_types::{Position, Range};
use rowan::{TextRange, TextSize};
use serde_json::{Value, json};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};
#[cfg(test)]
use tex_ls_analysis::text::TextBuffer;
use tex_ls_analysis::{incremental::Analysis, source::FileKind, text::PositionEncoding};
use tex_ls_parser::{
    ast::{command_name, environment_name, nth_group_inner},
    semantic::{DefSiteKind, Signatures, context::TokenListContexts, mode::Mode},
    syntax::SyntaxKind,
};

pub const TYPES: &[&str] = &[
    "macro",
    "type",
    "variable",
    "property",
    "keyword",
    "string",
    "number",
    "namespace",
    "texText",
    "texLiteral",
    "texPath",
    "texMath",
    "comment",
    "texEmbedded",
    "function",
    "parameter",
    "operator",
];
pub const MODIFIERS: &[&str] = &["declaration", "texBold", "texItalic"];
const TEXT: usize = 8;
const LITERAL: usize = 9;
const PATH: usize = 10;
const MATH: usize = 11;
const COMMENT: usize = 12;
const FUNCTION: usize = 14;
const PARAMETER: usize = 15;
const OPERATOR: usize = 16;

struct DefinitionIndex<'a> {
    commands: HashSet<&'a str>,
    ranges: Vec<(TextSize, TextSize)>,
}

impl<'a> DefinitionIndex<'a> {
    fn new(sites: &'a [tex_ls_parser::semantic::DefSite]) -> Self {
        let commands = sites
            .iter()
            .filter(|site| site.kind == DefSiteKind::Command)
            .map(|site| site.name.as_str())
            .collect();
        let mut ranges: Vec<_> = sites.iter().map(|site| site.range).collect();
        ranges.sort_unstable_by_key(|range| range.start());
        let mut end = TextSize::new(0);
        let ranges = ranges
            .into_iter()
            .map(|range| {
                end = end.max(range.end());
                (range.start(), end)
            })
            .collect();
        Self { commands, ranges }
    }

    fn contains(&self, range: TextRange) -> bool {
        let next = self
            .ranges
            .partition_point(|&(start, _)| start <= range.start());
        next > 0 && self.ranges[next - 1].1 >= range.end()
    }
}

/// A custom purpose may use a standard fallback only if that preserves its
/// meaning. Clients without neutral/literal support retain their own grammar.
pub fn fallback(kind: &str) -> Option<&'static str> {
    match kind {
        "texEmbedded" => Some("texLiteral"),
        "texPath" => Some("string"),
        "texMath" => Some("operator"),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Span {
    range: TextRange,
    kind: usize,
    priority: u8,
    modifiers: u32,
}
fn add(spans: &mut Vec<Span>, range: TextRange, kind: usize, priority: u8, modifiers: u32) {
    if !range.is_empty() {
        spans.push(Span {
            range,
            kind,
            priority,
            modifiers,
        });
    }
}

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
    add(
        &mut spans,
        TextRange::new(0.into(), TextSize::try_from(text.len()).unwrap()),
        TEXT,
        0,
        0,
    );
    if file_kind_for(path) == FileKind::Bib {
        use tex_ls_parser::bib::syntax::SyntaxKind as K;
        let root = snapshot.parsed_bib_tree(file);
        for link in tex_ls_analysis::bib::document_link::document_links(&root) {
            add(&mut spans, link.range, PATH, 30, 0);
        }
        for field in root.descendants().filter(|node| node.kind() == K::FIELD) {
            let name = tex_ls_parser::bib::ast::field_name(&field);
            if name.as_deref().is_some_and(|name| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "crossref" | "xref" | "xdata" | "entryset"
                )
            }) && let Some(value) = tex_ls_parser::bib::ast::field_value(&field)
                && let Some((inner, range)) =
                    tex_ls_analysis::bib::document_link::single_delimited_value(&value)
                && !inner.contains(['\\', '#', '{', '}'])
            {
                let mut offset = 0;
                for part in inner.split(',') {
                    let key = part.trim();
                    let start = range.start()
                        + TextSize::try_from(offset + part.len() - part.trim_start().len())
                            .unwrap();
                    let end = start + TextSize::try_from(key.len()).unwrap();
                    add(&mut spans, TextRange::new(start, end), 2, 30, 0);
                    offset += part.len() + 1;
                }
            }
        }
        for node in root.descendants() {
            let string_declaration = node.kind() == K::FIELD_NAME
                && node
                    .ancestors()
                    .any(|ancestor| ancestor.kind() == K::STRING_ENTRY);
            let kind = match node.kind() {
                K::ENTRY_TYPE
                    if matches!(
                        node.text()
                            .to_string()
                            .to_ascii_lowercase()
                            .trim_start_matches('@'),
                        "string" | "comment" | "preamble"
                    ) =>
                {
                    4
                }
                K::ENTRY_TYPE => 1,
                K::FIELD_NAME if string_declaration => 2,
                K::FIELD_NAME => 3,
                K::KEY => 2,
                K::LITERAL if !node.text().to_string().chars().all(|c| c.is_ascii_digit()) => 2,
                K::COMMENT | K::COMMENT_ENTRY | K::JUNK => COMMENT,
                _ => continue,
            };
            add(
                &mut spans,
                node.text_range(),
                kind,
                10,
                u32::from(node.kind() == K::KEY || string_declaration),
            );
        }
    } else {
        let root = snapshot.parsed_tree(file);
        let sites = snapshot.definition_sites(file);
        let definitions = DefinitionIndex::new(sites);
        let contexts = TokenListContexts::new(&root);
        let file_roles =
            tex_ls_analysis::external::links::FileRoleContext::from_definition_sites_and_operands(
                &root,
                sites,
                snapshot.file_operand_contexts(file),
            );
        let signatures = Signatures::new(snapshot.document_signatures(file));
        let modes = snapshot.file_modes(file);
        for body in contexts.ranges() {
            let start = body.start() + TextSize::new(1);
            let end = body.end()
                - TextSize::new(u32::from(text[..usize::from(body.end())].ends_with('}')));
            if start < end {
                add(&mut spans, TextRange::new(start, end), LITERAL, 100, 0);
            }
        }
        for environment in root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ENVIRONMENT)
        {
            if let Some(begin) = environment
                .children()
                .find(|node| node.kind() == SyntaxKind::BEGIN)
                && environment_name(&begin).as_deref() == Some("alltt")
                && signatures
                    .environment_at(&begin)
                    .is_some_and(|sig| sig.verbatim_body)
            {
                let end = environment
                    .children()
                    .find(|node| node.kind() == SyntaxKind::END)
                    .map_or(environment.text_range().end(), |node| {
                        node.text_range().start()
                    });
                if begin.text_range().end() < end {
                    add(
                        &mut spans,
                        TextRange::new(begin.text_range().end(), end),
                        LITERAL,
                        5,
                        0,
                    );
                }
            }
        }
        literal_command_names(&root, text, &mut spans, &definitions, &file_roles);
        for statement in root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::STATEMENT)
        {
            tikz_statement(&statement, text, &mut spans, &definitions);
        }
        for element in root.descendants_with_tokens() {
            match element {
                rowan::NodeOrToken::Token(token) => {
                    let r = token.text_range();
                    let mode = modes.mode_at(usize::from(r.start()));
                    if mode != Mode::Text {
                        add(&mut spans, r, TEXT, 7, 0);
                    }
                    match token.kind() {
                        SyntaxKind::DELEGATED_CODE => add(&mut spans, r, 13, 100, 0),
                        SyntaxKind::STRINGIFIED_TOKEN => add(&mut spans, r, LITERAL, 100, 0),
                        SyntaxKind::FOREIGN_TEXT | SyntaxKind::UNCERTAIN => {
                            add(&mut spans, r, TEXT, 100, 0)
                        }
                        SyntaxKind::FOREIGN_KEYWORD => add(&mut spans, r, 4, 100, 0),
                        SyntaxKind::FOREIGN_FUNCTION => add(&mut spans, r, FUNCTION, 100, 1),
                        SyntaxKind::FOREIGN_COMMENT | SyntaxKind::EXCLUDED_BODY => {
                            add(&mut spans, r, COMMENT, 100, 0)
                        }
                        SyntaxKind::ESCAPE_OPEN | SyntaxKind::ESCAPE_CLOSE => {
                            add(&mut spans, r, MATH, 100, 0)
                        }
                        SyntaxKind::CODE_OPEN | SyntaxKind::CODE_CLOSE => {
                            add(&mut spans, r, LITERAL, 100, 0)
                        }
                        SyntaxKind::CONTROL_WORD | SyntaxKind::CONTROL_SYMBOL => {
                            let operand = file_roles.is_operand_token(r);
                            let boundary = !operand
                                && matches!(token.text(), "\\(" | "\\)" | "\\[" | "\\]")
                                && token.parent().is_some_and(|node| {
                                    matches!(
                                        node.kind(),
                                        SyntaxKind::INLINE_MATH | SyntaxKind::DISPLAY_MATH
                                    )
                                });
                            let row = !operand
                                && token.text() == "\\\\"
                                && in_alignment(&token, &signatures);
                            add(&mut spans, r, if boundary || row { MATH } else { 0 }, 20, 0);
                        }
                        SyntaxKind::DOLLAR
                            if token.parent().is_some_and(|node| {
                                matches!(
                                    node.kind(),
                                    SyntaxKind::INLINE_MATH | SyntaxKind::DISPLAY_MATH
                                )
                            }) =>
                        {
                            add(&mut spans, r, MATH, 20, 0)
                        }
                        SyntaxKind::CARET | SyntaxKind::UNDERSCORE if mode == Mode::Math => {
                            add(&mut spans, r, MATH, 20, 0)
                        }
                        SyntaxKind::AMPERSAND if in_alignment(&token, &signatures) => {
                            add(&mut spans, r, MATH, 20, 0)
                        }
                        SyntaxKind::DOC_MARGIN => add(&mut spans, r, LITERAL, 20, 0),
                        SyntaxKind::COMMENT | SyntaxKind::GUARD => {
                            add(&mut spans, r, COMMENT, 110, 0)
                        }
                        SyntaxKind::VERBATIM_BODY => {
                            let ignored = token.parent().is_some_and(|node| {
                                node.children()
                                    .find(|child| child.kind() == SyntaxKind::BEGIN)
                                    .and_then(|node| environment_name(&node))
                                    .as_deref()
                                    == Some("comment")
                            });
                            add(
                                &mut spans,
                                r,
                                if ignored { COMMENT } else { LITERAL },
                                100,
                                0,
                            );
                        }
                        SyntaxKind::VERB => {
                            let raw = token.text();
                            let (head, after) = if let Some(after) = raw.strip_prefix("\\verb") {
                                add(
                                    &mut spans,
                                    TextRange::new(r.start(), r.start() + TextSize::new(5)),
                                    0,
                                    101,
                                    0,
                                );
                                (
                                    raw.len() - after.strip_prefix('*').unwrap_or(after).len(),
                                    after.strip_prefix('*').unwrap_or(after),
                                )
                            } else {
                                (0, raw)
                            };
                            if let Some(opener) = after.chars().next() {
                                let closer = if opener == '{' { '}' } else { opener };
                                let start = head + opener.len_utf8();
                                let end = raw.len()
                                    - if after.len() > opener.len_utf8() && after.ends_with(closer)
                                    {
                                        closer.len_utf8()
                                    } else {
                                        0
                                    };
                                let owner = token.parent().and_then(|node| command_name(&node));
                                let kind = if matches!(
                                    owner.as_deref(),
                                    Some("url" | "href" | "nolinkurl")
                                ) {
                                    PATH
                                } else {
                                    LITERAL
                                };
                                if start < end {
                                    add(
                                        &mut spans,
                                        TextRange::new(
                                            r.start() + TextSize::try_from(start).unwrap(),
                                            r.start() + TextSize::try_from(end).unwrap(),
                                        ),
                                        kind,
                                        100,
                                        0,
                                    );
                                }
                            }
                        }
                        SyntaxKind::HASH if definitions.contains(r) => {
                            if token.prev_token().is_some_and(|previous| {
                                previous.kind() == SyntaxKind::HASH
                                    && previous.text_range().end() == r.start()
                            }) {
                                continue;
                            }
                            // Parameters are a source fact only inside a declaration.
                            let start = usize::from(r.start());
                            let suffix = &text[start..];
                            let hashes = suffix.bytes().take_while(|byte| *byte == b'#').count();
                            if suffix
                                .as_bytes()
                                .get(hashes)
                                .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
                            {
                                add(
                                    &mut spans,
                                    TextRange::new(
                                        r.start(),
                                        r.start() + TextSize::try_from(hashes + 1).unwrap(),
                                    ),
                                    PARAMETER,
                                    120,
                                    0,
                                );
                            }
                        }
                        _ => {}
                    }
                }
                rowan::NodeOrToken::Node(node)
                    if matches!(node.kind(), SyntaxKind::BEGIN | SyntaxKind::END) =>
                {
                    if let Some(r) = tex_ls_parser::ast::environment_name_range(&node) {
                        let name = &text[usize::from(r.start())..usize::from(r.end())];
                        if !name.is_empty()
                            && name
                                .chars()
                                .all(|ch| ch.is_alphanumeric() || "*_-:@".contains(ch))
                        {
                            add(&mut spans, r, 1, 20, 0);
                        }
                    }
                    if node.kind() == SyntaxKind::BEGIN {
                        if let Some(name) = environment_name(&node)
                            && snapshot
                                .document_signatures(file)
                                .environment(&name)
                                .is_none()
                        {
                            let column = match name.as_str() {
                                "tabular" | "array" => Some(0),
                                "tabular*" | "tabularx" | "tabulary" => {
                                    numeric_group(&node, 0, text, &mut spans);
                                    Some(1)
                                }
                                _ => None,
                            };
                            if let Some(column) = column
                                && let Some(group) = tex_ls_parser::ast::nth_group(&node, column)
                            {
                                column_spec(&group, text, &mut spans);
                            }
                            if name == "minted"
                                && let Some((r, _)) = nth_group_inner(&node, 0)
                            {
                                add(&mut spans, r, 1, 30, 0);
                            }
                        }
                        keyval_arguments(
                            &node,
                            signatures.environment_at(&node).map(|sig| &*sig.args),
                            text,
                            &mut spans,
                        );
                    }
                }
                rowan::NodeOrToken::Node(node) if node.kind() == SyntaxKind::COMMAND => {
                    if file_roles.is_operand_head(&node) {
                        continue;
                    }
                    use tex_ls_parser::semantic::roles::FileRoleKind;
                    let name = command_name(&node);
                    if let Some(name) = name.as_deref() {
                        keyval_arguments(
                            &node,
                            signatures.command(name).map(|sig| &*sig.args),
                            text,
                            &mut spans,
                        );
                        // Local definitions invalidate built-in presentation assumptions.
                        if !definitions.commands.contains(name) {
                            command_arguments(&node, name, text, &mut spans);
                        }
                        if let Some(sig) = signatures.command(name)
                            && sig.sectioning.is_some()
                        {
                            for group in node
                                .children()
                                .filter(|child| child.kind() == SyntaxKind::GROUP)
                            {
                                add(&mut spans, group.text_range(), TEXT, 6, 2);
                            }
                        }
                    }
                    if file_roles.allows(&node) {
                        spans.extend(
                            tex_ls_parser::semantic::roles::biblatex_style_arguments(&node)
                                .into_iter()
                                .map(|(_, range, _)| Span {
                                    range,
                                    kind: 7,
                                    priority: 30,
                                    modifiers: 0,
                                }),
                        );
                    }
                    if let Some(role) = file_roles.role(&node) {
                        if let Some(directory) = role.directory
                            && let Some((range, _)) = nth_group_inner(&node, directory)
                        {
                            add(&mut spans, range, PATH, 30, 0);
                        }
                        let kind = if matches!(
                            role.kind,
                            FileRoleKind::Package
                                | FileRoleKind::Class
                                | FileRoleKind::NamedFile { .. }
                        ) {
                            7
                        } else {
                            PATH
                        };
                        for (_, r) in
                            tex_ls_analysis::external::links::file_argument_spans(&node, role)
                        {
                            add(&mut spans, r, kind, 30, 0);
                        }
                    }
                }
                _ => {}
            }
        }
        let model = snapshot.semantic_model(file);
        for (r, declaration) in model
            .labels()
            .iter()
            .map(|x| (x.key_range, true))
            .chain(model.refs().iter().map(|x| (x.key_range, false)))
            .chain(model.citations().iter().map(|x| (x.key_range, false)))
            .chain(model.bibitems().iter().map(|x| (x.key_range, true)))
            .chain(model.glossary_defs().iter().map(|x| (x.key_range, true)))
            .chain(model.glossary_uses().iter().map(|x| (x.key_range, false)))
        {
            add(&mut spans, r, 2, 40, u32::from(declaration));
        }
        for site in sites {
            add(
                &mut spans,
                site.name_range,
                if site.kind == DefSiteKind::Command {
                    if text[usize::from(site.name_range.start())..].starts_with('\\') {
                        0
                    } else {
                        2
                    }
                } else {
                    1
                },
                40,
                1,
            );
        }
    }
    if let Some(wanted) = range {
        // Keep whole boundary lines so intersecting tokens retain the same
        // boundaries as a full response, including mid-token range endpoints.
        // All ownership and typography facts above still use the whole tree.
        let start = TextSize::try_from(idx.offset_at(wanted.start.line, 0)).unwrap();
        let end = TextSize::try_from(idx.offset_at(wanted.end.line.saturating_add(1), 0)).unwrap();
        spans.retain_mut(|span| {
            let from = span.range.start().max(start);
            let to = span.range.end().min(end);
            if from >= to {
                return false;
            }
            span.range = TextRange::new(from, to);
            true
        });
    }
    let mut data = Vec::new();
    let (mut line, mut column) = (0, 0);
    let mut positions = idx.position_cursor();
    for span in resolve_spans(&spans) {
        let start = usize::from(span.range.start());
        let end = usize::from(span.range.end());
        let mut offset = start;
        // CR, LF and CRLF are all source line boundaries. No token crosses one.
        for part in text[start..end].split_inclusive(['\r', '\n']) {
            let len = part.trim_end_matches(['\r', '\n']).len();
            if len > 0 {
                let (start_line, start_character) = positions.position(offset);
                let (end_line, end_character) = positions.position(offset + len);
                let r = Range::new(
                    Position::new(start_line, start_character),
                    Position::new(end_line, end_character),
                );
                if range.is_none_or(|wanted| r.start < wanted.end && r.end > wanted.start) {
                    data.extend([
                        r.start.line - line,
                        if r.start.line == line {
                            r.start.character - column
                        } else {
                            r.start.character
                        },
                        r.end.character - r.start.character,
                        span.kind as u32,
                        span.modifiers,
                    ]);
                    (line, column) = (r.start.line, r.start.character);
                }
            }
            offset += part.len();
        }
    }
    json!({"data":data})
}

// Only literal csname bodies have a statically constructed command name.
fn literal_command_names(
    root: &tex_ls_parser::syntax::SyntaxNode,
    text: &str,
    spans: &mut Vec<Span>,
    definitions: &DefinitionIndex<'_>,
    file_roles: &tex_ls_analysis::external::links::FileRoleContext,
) {
    let mut pending: Option<(TextSize, bool, Vec<TextRange>)> = None;
    let mut previous: Option<tex_ls_parser::syntax::SyntaxToken> = None;
    for token in root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        let r = token.text_range();
        if token.kind() == SyntaxKind::CONTROL_WORD
            && token.text() == "\\csname"
            && !definitions.commands.contains("csname")
            && token
                .parent()
                .is_none_or(|node| !file_roles.is_operand_head(&node))
        {
            if let Some((_, literal, _)) = pending.as_mut() {
                *literal = false;
            } else {
                pending = Some((r.end(), true, Vec::new()));
            }
        } else if token.kind() == SyntaxKind::CONTROL_WORD
            && token.text() == "\\endcsname"
            && token
                .parent()
                .is_none_or(|node| !file_roles.is_operand_head(&node))
        {
            if let Some((start, true, words)) = pending.take() {
                let value = &text[usize::from(start)..usize::from(r.start())];
                if !value.trim().is_empty()
                    && value
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || ch.is_whitespace() || "_:@-".contains(ch))
                {
                    for word in words {
                        add(spans, word, 2, 30, 0);
                    }
                }
            }
        } else if let Some((_, literal, words)) = pending.as_mut() {
            if !matches!(
                token.kind(),
                SyntaxKind::WORD | SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE
            ) {
                *literal = false;
            } else if *literal && token.kind() == SyntaxKind::WORD {
                words.push(r);
            }
        }
        if token.kind() == SyntaxKind::WORD
            && matches!(
                previous.as_ref().map(|token| token.text()),
                Some(
                    "\\only"
                        | "\\uncover"
                        | "\\visible"
                        | "\\invisible"
                        | "\\onslide"
                        | "\\alt"
                        | "\\temporal"
                )
            )
            && token.text().starts_with('<')
            && token.text().ends_with('>')
            && token
                .text()
                .chars()
                .all(|ch| ch.is_ascii_digit() || "<>,|-+ ".contains(ch))
            && previous.as_ref().is_none_or(|token| {
                token
                    .text()
                    .strip_prefix('\\')
                    .is_none_or(|name| !definitions.commands.contains(name))
                    && token
                        .parent()
                        .is_none_or(|node| !file_roles.is_operand_head(&node))
            })
        {
            numeric_range(r, text, spans);
        }
        if token.kind() == SyntaxKind::CONTROL_WORD
            && let Some(name) = token.text().strip_prefix('\\')
            && matches!(
                name,
                "char" | "mathchar" | "catcode" | "mathcode" | "delcode"
            )
            && token.parent().is_some_and(|node| file_roles.allows(&node))
            && !definitions.commands.contains(name)
        {
            let after = &text[usize::from(r.end())..];
            let operand_end = if let Some((start, end)) = literal_integer_prefix(after) {
                add(
                    spans,
                    TextRange::new(
                        r.end() + TextSize::try_from(start).unwrap(),
                        r.end() + TextSize::try_from(end).unwrap(),
                    ),
                    6,
                    30,
                    0,
                );
                Some(end)
            } else {
                let mut operand = token.next_token();
                while operand.as_ref().is_some_and(|token| {
                    matches!(
                        token.kind(),
                        SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
                    )
                }) {
                    operand = operand.and_then(|token| token.next_token());
                }
                operand
                    .filter(|token| {
                        token.kind() == SyntaxKind::WORD && literal_character_constant(token.text())
                    })
                    .map(|token| usize::from(token.text_range().end() - r.end()))
            };
            if matches!(name, "catcode" | "mathcode" | "delcode")
                && let Some(end) = operand_end
            {
                let remaining = &after[end..];
                let trimmed = remaining.trim_start();
                let skip = remaining.len() - trimmed.len() + usize::from(trimmed.starts_with('='));
                if let Some((start, finish)) = literal_integer_prefix(&remaining[skip..]) {
                    let base = r.end() + TextSize::try_from(end + skip).unwrap();
                    add(
                        spans,
                        TextRange::new(
                            base + TextSize::try_from(start).unwrap(),
                            base + TextSize::try_from(finish).unwrap(),
                        ),
                        6,
                        30,
                        0,
                    );
                }
            }
        }
        if !matches!(
            token.kind(),
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
        ) {
            previous = Some(token);
        }
    }
}

// Read only an authored integer prefix; macros and expressions remain unproved.
fn literal_integer_prefix(source: &str) -> Option<(usize, usize)> {
    let trimmed = source.trim_start();
    let start = source.len() - trimmed.len();
    let mut digits = trimmed;
    let sign = usize::from(digits.starts_with(['+', '-']));
    digits = &digits[sign..];
    let (base, prefix) = if digits.starts_with('"') {
        (16, 1)
    } else if digits.starts_with('\'') {
        (8, 1)
    } else {
        (10, 0)
    };
    let count = digits[prefix..]
        .chars()
        .take_while(|ch| ch.is_digit(base))
        .count();
    let number_start = start + if prefix == 0 { 0 } else { sign + prefix };
    (count > 0).then_some((number_start, start + sign + prefix + count))
}

fn literal_character_constant(source: &str) -> bool {
    let Some(source) = source.strip_prefix('`') else {
        return false;
    };
    let mut chars = source.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first == '\\' {
        chars.next().is_some_and(|ch| !matches!(ch, '\n' | '\r')) && chars.next().is_none()
    } else {
        !matches!(first, '\n' | '\r') && chars.next().is_none()
    }
}

fn tikz_statement(
    statement: &tex_ls_parser::syntax::SyntaxNode,
    text: &str,
    spans: &mut Vec<Span>,
    definitions: &DefinitionIndex<'_>,
) {
    let mut declaration = false;
    for child in statement.children_with_tokens() {
        if let Some(node) = child.as_node() {
            if node.kind() == SyntaxKind::COMMAND
                && let Some(name) = command_name(node)
                && matches!(
                    name.as_str(),
                    "draw" | "node" | "coordinate" | "path" | "clip" | "fill" | "filldraw"
                )
                && !definitions.commands.contains(name.as_str())
            {
                declaration = matches!(name.as_str(), "node" | "coordinate");
                for optional in node
                    .children()
                    .filter(|child| child.kind() == SyntaxKind::OPTIONAL)
                {
                    keyval_group(&optional, text, spans);
                }
            } else if node.kind() == SyntaxKind::OPTIONAL {
                keyval_group(node, text, spans);
            }
            continue;
        }
        let Some(token) = child
            .as_token()
            .filter(|token| token.kind() == SyntaxKind::WORD)
        else {
            continue;
        };
        let value = token.text();
        if tex_ls_parser::semantic::tikz::is_operation_keyword(value)
            || tex_ls_parser::semantic::tikz::is_path_operator(value)
            || value == "at"
        {
            add(
                spans,
                token.text_range(),
                if tex_ls_parser::semantic::tikz::is_path_operator(value) {
                    OPERATOR
                } else {
                    4
                },
                30,
                0,
            );
            declaration = matches!(value, "node" | "coordinate");
        }
        let mut from = 0;
        while let Some(open) = value[from..].find('(') {
            let start = from + open + 1;
            let Some(close) = value[start..].find(')') else {
                break;
            };
            let end = start + close;
            let inside = &value[start..end];
            let r = TextRange::new(
                token.text_range().start() + TextSize::try_from(start).unwrap(),
                token.text_range().start() + TextSize::try_from(end).unwrap(),
            );
            if inside.contains([',', ':']) && inside.split([',', ':']).all(numeric_setting) {
                numeric_range(r, text, spans);
            } else if !inside.is_empty()
                && inside
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || "_:@.-".contains(ch))
            {
                add(spans, r, 2, 30, u32::from(declaration));
            }
            declaration = false;
            from = end + 1;
        }
    }
}
fn numeric_setting(source: &str) -> bool {
    let value = source.trim().trim_start_matches(['+', '-']);
    let end = value
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(value.len());
    value[..end].parse::<f64>().is_ok()
        && (value[end..].is_empty() || dimension_unit(&value[end..]))
}

fn dimension_unit(source: &str) -> bool {
    matches!(
        source,
        "pt" | "pc" | "in" | "bp" | "cm" | "mm" | "dd" | "cc" | "sp" | "em" | "ex" | "mu"
    )
}

fn column_spec(group: &tex_ls_parser::syntax::SyntaxNode, text: &str, spans: &mut Vec<Span>) {
    let mut previous = String::new();
    for child in group.children_with_tokens() {
        if let Some(token) = child.as_token() {
            if token.kind() == SyntaxKind::WORD {
                for (offset, ch) in token
                    .text()
                    .char_indices()
                    .filter(|(_, ch)| ch.is_ascii_alphabetic())
                {
                    add(
                        spans,
                        TextRange::at(
                            token.text_range().start() + TextSize::try_from(offset).unwrap(),
                            TextSize::try_from(ch.len_utf8()).unwrap(),
                        ),
                        1,
                        30,
                        0,
                    );
                }
                previous = token.text().to_owned();
            }
        } else if let Some(child) = child.as_node()
            && child.kind() == SyntaxKind::GROUP
        {
            if previous.ends_with(['p', 'm', 'b', 'w', 'W', '*']) {
                numeric_range(child.text_range(), text, spans);
            }
            if previous == "*" {
                previous = "repeat".to_owned();
            } else if previous == "repeat" {
                column_spec(child, text, spans);
                previous.clear();
            }
        }
    }
}

fn in_alignment(token: &tex_ls_parser::syntax::SyntaxToken, signatures: &Signatures<'_>) -> bool {
    // A command argument can change modes or contain an opaque key; do not
    // inherit a surrounding table's alignment through that boundary.
    for (depth, node) in token
        .parent()
        .into_iter()
        .flat_map(|node| node.ancestors())
        .enumerate()
    {
        if depth == 0 && node.kind() == SyntaxKind::COMMAND && token.text() == "\\\\" {
            continue;
        }
        if matches!(
            node.kind(),
            SyntaxKind::COMMAND | SyntaxKind::GROUP | SyntaxKind::OPTIONAL
        ) {
            return false;
        }
        if node.kind() == SyntaxKind::ENVIRONMENT {
            return signatures
                .environment_at(&node)
                .is_some_and(|sig| sig.align);
        }
    }
    false
}

fn command_arguments(
    node: &tex_ls_parser::syntax::SyntaxNode,
    name: &str,
    text: &str,
    spans: &mut Vec<Span>,
) {
    match name {
        "textbf" | "emph" | "textit" => {
            if let Some(group) = tex_ls_parser::ast::nth_group(node, 0) {
                add(
                    spans,
                    group.text_range(),
                    TEXT,
                    6,
                    if name == "textbf" { 2 } else { 4 },
                );
            }
        }
        "newcommand"
        | "renewcommand"
        | "providecommand"
        | "newenvironment"
        | "renewenvironment"
        | "DeclareRobustCommand" => {
            if let Some(optional) = node
                .children()
                .find(|child| child.kind() == SyntaxKind::OPTIONAL)
            {
                let value = optional.text().to_string();
                if value.starts_with('[')
                    && value.ends_with(']')
                    && value[1..value.len() - 1]
                        .trim()
                        .parse::<u8>()
                        .is_ok_and(|n| n <= 9)
                {
                    numeric_range(optional.text_range(), text, spans);
                }
            }
        }
        "hyperref" => {
            if let Some(optional) = node
                .children()
                .find(|child| child.kind() == SyntaxKind::OPTIONAL)
                && optional.children().next().is_none()
            {
                let raw = optional.text().to_string();
                if let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                    && !inner.contains(['\\', '%', '{', '}', '[', ']', '#'])
                {
                    let key = inner.trim();
                    let start = optional.text_range().start()
                        + TextSize::try_from(1 + inner.len() - inner.trim_start().len()).unwrap();
                    let end = start + TextSize::try_from(key.len()).unwrap();
                    add(spans, TextRange::new(start, end), 2, 30, 0);
                }
            }
        }
        "hypertarget" | "hyperlink" => {
            if let Some((r, _)) = nth_group_inner(node, 0) {
                add(spans, r, 2, 30, u32::from(name == "hypertarget"));
            }
        }
        "DeclareOption" | "newcolumntype" => {
            if let Some((r, _)) = nth_group_inner(node, 0) {
                add(spans, r, 1, 30, 1);
            }
        }
        "NewDocumentCommand"
        | "RenewDocumentCommand"
        | "ProvideDocumentCommand"
        | "DeclareDocumentCommand"
        | "NewDocumentEnvironment"
        | "RenewDocumentEnvironment" => {
            if let Some(group) = tex_ls_parser::ast::nth_group(node, 1)
                && tex_ls_parser::semantic::xparse::parse_editor_spec(
                    &tex_ls_parser::ast::group_inner_source(&group),
                )
                .is_some()
            {
                for token in group
                    .children_with_tokens()
                    .filter_map(|child| child.into_token())
                {
                    if token.kind() == SyntaxKind::WORD {
                        for (offset, ch) in token
                            .text()
                            .char_indices()
                            .filter(|(_, ch)| "mroOdDsStvbeE".contains(*ch))
                        {
                            let start =
                                token.text_range().start() + TextSize::try_from(offset).unwrap();
                            add(
                                spans,
                                TextRange::at(start, TextSize::try_from(ch.len_utf8()).unwrap()),
                                1,
                                30,
                                0,
                            );
                        }
                    }
                }
            }
        }
        "newtheorem" => {
            for optional in node
                .children()
                .filter(|child| child.kind() == SyntaxKind::OPTIONAL)
            {
                let source = optional.text().to_string();
                let value = source.trim_start_matches('[').trim_end_matches(']');
                if !value.is_empty()
                    && value
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || "_:-".contains(ch))
                {
                    add(
                        spans,
                        TextRange::new(
                            optional.text_range().start() + TextSize::new(1),
                            optional.text_range().end() - TextSize::new(1),
                        ),
                        2,
                        30,
                        0,
                    );
                }
            }
        }
        "multicolumn" => {
            numeric_group(node, 0, text, spans);
            if let Some(group) = tex_ls_parser::ast::nth_group(node, 1) {
                column_spec(&group, text, spans);
            }
        }
        "mintinline" | "inputminted" => {
            if let Some((r, _)) = nth_group_inner(node, 0) {
                add(spans, r, 1, 30, 0);
            }
        }
        "href" => {
            if let Some((r, _)) = nth_group_inner(node, 0) {
                add(spans, r, PATH, 30, 0);
            }
        }
        "newcounter" | "setcounter" | "addtocounter" | "value" | "arabic" | "roman" | "Roman"
        | "alph" | "Alph" | "stepcounter" | "refstepcounter" => {
            if let Some((r, _)) = nth_group_inner(node, 0) {
                add(spans, r, 2, 30, u32::from(name == "newcounter"));
            }
            if matches!(name, "setcounter" | "addtocounter") {
                numeric_group(node, 1, text, spans);
            }
        }
        "setlength" | "addtolength" => numeric_group(node, 1, text, spans),
        "definecolor" | "providecolor" | "colorlet" | "color" | "textcolor" | "pagecolor" => {
            let declaration = matches!(name, "definecolor" | "providecolor" | "colorlet");
            if let Some((r, _)) = nth_group_inner(node, 0) {
                if declaration {
                    add(spans, r, 2, 30, 1);
                } else {
                    color_expression(r, text, spans);
                }
            }
            if name == "colorlet"
                && let Some((r, _)) = nth_group_inner(node, 1)
            {
                color_expression(r, text, spans);
            }
            if matches!(name, "definecolor" | "providecolor")
                && let Some((r, model)) = nth_group_inner(node, 1)
            {
                add(spans, r, 1, 30, 0);
                if model.trim() == "HTML" {
                    if let Some((r, value)) = nth_group_inner(node, 2)
                        && value.trim().chars().all(|ch| ch.is_ascii_hexdigit())
                    {
                        add(spans, r, 6, 30, 0);
                    }
                } else {
                    numeric_group(node, 2, text, spans);
                }
            }
        }
        _ => {}
    }
}
fn color_expression(range: TextRange, text: &str, spans: &mut Vec<Span>) {
    let source = &text[usize::from(range.start())..usize::from(range.end())];
    let mut offset = 0;
    for (index, part) in source.split('!').enumerate() {
        let name = part.trim();
        let start = range.start()
            + TextSize::try_from(offset + part.len() - part.trim_start().len()).unwrap();
        if !name.is_empty() {
            let r = TextRange::at(start, TextSize::try_from(name.len()).unwrap());
            if index % 2 == 1 && name.parse::<f64>().is_ok() {
                add(spans, r, 6, 30, 0);
            } else if name
                .chars()
                .all(|ch| ch.is_alphanumeric() || "_:-".contains(ch))
            {
                add(spans, r, 2, 30, 0);
            }
        }
        offset += part.len() + 1;
    }
}

fn numeric_group(
    node: &tex_ls_parser::syntax::SyntaxNode,
    n: usize,
    text: &str,
    spans: &mut Vec<Span>,
) {
    if let Some(group) = tex_ls_parser::ast::nth_group(node, n) {
        for token in group
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::WORD)
        {
            numeric_range(token.text_range(), text, spans);
        }
    }
}
fn numeric_range(range: TextRange, text: &str, spans: &mut Vec<Span>) {
    let source = &text[usize::from(range.start())..usize::from(range.end())];
    let mut iter = source.char_indices().peekable();
    while let Some((start, ch)) = iter.next() {
        if !ch.is_ascii_digit()
            && !(matches!(ch, '+' | '-' | '.')
                && iter.peek().is_some_and(|(_, c)| c.is_ascii_digit()))
        {
            continue;
        }
        let mut end = start + ch.len_utf8();
        while let Some((at, ch)) = iter.peek().copied() {
            if !ch.is_ascii_digit() && ch != '.' {
                break;
            }
            iter.next();
            end = at + ch.len_utf8();
        }
        add(
            spans,
            TextRange::new(
                range.start() + TextSize::try_from(start).unwrap(),
                range.start() + TextSize::try_from(end).unwrap(),
            ),
            6,
            30,
            0,
        );
    }
}
fn keyval_arguments(
    node: &tex_ls_parser::syntax::SyntaxNode,
    args: Option<&[tex_ls_parser::semantic::signature::ArgSpec]>,
    text: &str,
    spans: &mut Vec<Span>,
) {
    use tex_ls_parser::semantic::signature::{ArgKind, ContentKind, match_arg_slot};
    let Some(args) = args else {
        return;
    };
    let mut slot = 0;
    for group in node
        .children()
        .filter(|node| matches!(node.kind(), SyntaxKind::GROUP | SyntaxKind::OPTIONAL))
    {
        let kind = if group.kind() == SyntaxKind::GROUP {
            ArgKind::Brace
        } else {
            ArgKind::Bracket
        };
        if match_arg_slot(args, &mut slot, kind)
            .is_none_or(|arg| arg.content != ContentKind::Keyval)
        {
            continue;
        }
        keyval_group(&group, text, spans);
    }
}
fn keyval_group(group: &tex_ls_parser::syntax::SyntaxNode, text: &str, spans: &mut Vec<Span>) {
    let mut start = group.text_range().start() + TextSize::new(1);
    let mut flat = true;
    for child in group.children_with_tokens().skip(1) {
        if let Some(node) = child.as_node() {
            let prefix = &text[usize::from(start)..usize::from(node.text_range().start())];
            if node.kind() == SyntaxKind::GROUP
                && prefix
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim().ends_with("/.style"))
            {
                keyval_group(node, text, spans);
            }
            flat = false;
            continue;
        }
        let Some(token) = child.as_token() else {
            continue;
        };
        if matches!(token.kind(), SyntaxKind::R_BRACE | SyntaxKind::R_BRACKET) {
            if start < token.text_range().start() {
                keyval_entry(
                    TextRange::new(start, token.text_range().start()),
                    text,
                    spans,
                    flat,
                );
            }
        } else if token.kind() == SyntaxKind::WORD {
            for (offset, _) in token.text().char_indices().filter(|(_, ch)| *ch == ',') {
                let end = token.text_range().start() + TextSize::try_from(offset).unwrap();
                if start < end {
                    keyval_entry(TextRange::new(start, end), text, spans, flat);
                }
                start = end + TextSize::new(1);
                flat = true;
            }
        } else if token.kind() == SyntaxKind::COMMENT {
            flat = false;
        }
    }
}
fn keyval_entry(range: TextRange, text: &str, spans: &mut Vec<Span>, flat: bool) {
    let entry = &text[usize::from(range.start())..usize::from(range.end())];
    // The caller splits only commas in direct tokens. Commas inside child
    // groups belong to their value and cannot introduce another outer key.
    let key = entry.split_once('=').map_or(entry, |(key, _)| key).trim();
    if !key.is_empty() && !key.contains(['\\', '%', '$', '#', '{', '}']) {
        let begin = entry.find(key).unwrap();
        add(
            spans,
            TextRange::new(
                range.start() + TextSize::try_from(begin).unwrap(),
                range.start() + TextSize::try_from(begin + key.len()).unwrap(),
            ),
            3,
            30,
            u32::from(key.ends_with("/.style")),
        );
        if !flat
            && let Some((_, value)) = entry.split_once('=')
            && matches!(
                key,
                "width"
                    | "height"
                    | "totalheight"
                    | "line width"
                    | "scale"
                    | "xmin"
                    | "xmax"
                    | "ymin"
                    | "ymax"
            )
        {
            let trimmed = value.trim_start();
            let prefix = trimmed.split('\\').next().unwrap_or("").trim_end();
            if prefix.parse::<f64>().is_ok() {
                let begin = entry.find('=').unwrap() + 1 + value.len() - trimmed.len();
                numeric_range(
                    TextRange::at(
                        range.start() + TextSize::try_from(begin).unwrap(),
                        TextSize::try_from(prefix.len()).unwrap(),
                    ),
                    text,
                    spans,
                );
            }
        }
        if flat && let Some((_, value)) = entry.split_once('=') {
            // A complete dimension/count is an operational setting. Words,
            // prose, and embedded expressions do not become numbers by shape.
            let value = value.trim();
            let begin = entry.find('=').unwrap() + 1;
            let value_start = range.start() + TextSize::try_from(begin).unwrap();
            let value_range = TextRange::new(
                value_start,
                range.start() + TextSize::try_from(entry.len()).unwrap(),
            );
            if key == "language" && !value.contains(['\\', '{', '}', '$', '#']) {
                add(spans, value_range, 1, 30, 0);
            } else if !matches!(value, "none" | "true" | "false")
                && matches!(
                    key,
                    "draw" | "fill" | "color" | "text" | "backgroundcolor" | "rulecolor"
                )
            {
                color_expression(value_range, text, spans);
            } else if numeric_keyval_setting(key, value) {
                numeric_range(value_range, text, spans);
            }
        }
    }
}

fn numeric_keyval_setting(key: &str, value: &str) -> bool {
    let key = key.rsplit('/').next().unwrap_or(key);
    if matches!(
        key,
        "alt" | "actualtext" | "pdfauthor" | "pdftitle" | "label" | "title"
    ) || !numeric_setting(value)
    {
        return false;
    }
    matches!(
        key,
        "width"
            | "height"
            | "totalheight"
            | "line width"
            | "scale"
            | "xmin"
            | "xmax"
            | "ymin"
            | "ymax"
            | "page"
            | "opacity"
            | "angle"
    ) || value
        .find(|ch: char| ch.is_ascii_alphabetic())
        .is_some_and(|start| dimension_unit(&value[start..]))
}

// Resolve nested presentation ranges without discarding their inner commands.
// The highest priority wins. Typography combines only on ordinary prose.
fn resolve_spans(spans: &[Span]) -> Vec<Span> {
    let mut events = Vec::with_capacity(spans.len() * 2);
    for (id, span) in spans.iter().enumerate() {
        events.push((span.range.start(), true, id));
        events.push((span.range.end(), false, id));
    }
    events.sort_unstable();
    let mut active: BTreeSet<(u8, usize)> = BTreeSet::new();
    let mut output: Vec<Span> = Vec::new();
    let mut previous = TextSize::new(0);
    for (position, start, id) in events {
        if previous < position
            && let Some(&(_, selected)) = active.last()
        {
            let mut span = spans[selected];
            span.range = TextRange::new(previous, position);
            if span.kind == TEXT {
                span.modifiers = active
                    .iter()
                    .filter(|(priority, _)| *priority >= span.priority)
                    .fold(0, |bits, (_, id)| bits | spans[*id].modifiers);
            }
            if output.last().is_some_and(|last| {
                last.range.end() == previous
                    && last.kind == span.kind
                    && last.modifiers == span.modifiers
            }) {
                output.last_mut().unwrap().range =
                    TextRange::new(output.last().unwrap().range.start(), position);
            } else {
                output.push(span);
            }
        }
        if start {
            active.insert((spans[id].priority, id));
        } else {
            active.remove(&(spans[id].priority, id));
        }
        previous = position;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    #[test]
    fn requested_ranges_match_full_tokens_including_typography_and_line_endings() {
        fn absolute(value: &Value) -> Vec<(Range, u64, u64)> {
            let (mut line, mut column) = (0, 0);
            value["data"]
                .as_array()
                .unwrap()
                .as_chunks::<5>()
                .0
                .iter()
                .map(|token| {
                    let delta = token[0].as_u64().unwrap() as u32;
                    line += delta;
                    column =
                        if delta == 0 { column } else { 0 } + token[1].as_u64().unwrap() as u32;
                    (
                        Range::new(
                            Position::new(line, column),
                            Position::new(line, column + token[2].as_u64().unwrap() as u32),
                        ),
                        token[3].as_u64().unwrap(),
                        token[4].as_u64().unwrap(),
                    )
                })
                .collect()
        }
        for (name, source) in [
            (
                "main.tex",
                "Before é𝕏.\r\n\\textbf{bold\n\\emph{nested é𝕏}\r$1+\\alpha$ and \\ref{key}}\n\\detokenize{\\ref{fake}\nprinted} After.\r\\begin{verbatim}\nraw \\unknown % literal\n\\end{verbatim}\n",
            ),
            (
                "refs.bib",
                "@book{key,\r\n title={é𝕏\nMore text},\r year=2026,\n month=jan,\n crossref={parent}}\n@comment{literal\nbody}\n",
            ),
        ] {
            let path = PathBuf::from(fixture_path!("/colors")).join(name);
            let mut db = IncrementalDatabase::default();
            db.apply_change(&path, source, None);
            for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                let snapshot = db.snapshot();
                let full = absolute(&compute(&snapshot, &path, encoding, None));
                for line in 0..12 {
                    for start_column in [0, 1, 4, 100] {
                        for end_line in line..=line + 2 {
                            for end_column in [0, 1, 5, 100] {
                                let wanted = Range::new(
                                    Position::new(line, start_column),
                                    Position::new(end_line, end_column),
                                );
                                let expected: Vec<_> = full
                                    .iter()
                                    .copied()
                                    .filter(|(token, _, _)| {
                                        token.start < wanted.end && token.end > wanted.start
                                    })
                                    .collect();
                                assert_eq!(
                                    absolute(&compute(&snapshot, &path, encoding, Some(wanted))),
                                    expected,
                                    "{name} {encoding:?} {wanted:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn coalesced_spans_preserve_purposes_and_nested_typography_at_every_byte() {
        let spans = vec![
            Span {
                range: TextRange::new(0.into(), 20.into()),
                kind: TEXT,
                priority: 0,
                modifiers: 0,
            },
            Span {
                range: TextRange::new(2.into(), 9.into()),
                kind: TEXT,
                priority: 6,
                modifiers: 2,
            },
            Span {
                range: TextRange::new(4.into(), 7.into()),
                kind: TEXT,
                priority: 6,
                modifiers: 4,
            },
            Span {
                range: TextRange::new(5.into(), 6.into()),
                kind: TEXT,
                priority: 7,
                modifiers: 0,
            },
            Span {
                range: TextRange::new(12.into(), 14.into()),
                kind: 0,
                priority: 20,
                modifiers: 0,
            },
            Span {
                range: TextRange::new(14.into(), 16.into()),
                kind: 0,
                priority: 20,
                modifiers: 0,
            },
        ];
        let resolved = resolve_spans(&spans);
        for offset in 0..20 {
            let at = TextSize::new(offset);
            let (selected, _) = spans
                .iter()
                .enumerate()
                .filter(|(_, span)| span.range.contains(at))
                .max_by_key(|(id, span)| (span.priority, *id))
                .unwrap();
            let expected = spans[selected];
            let modifiers = if expected.kind == TEXT {
                spans
                    .iter()
                    .filter(|span| span.range.contains(at) && span.priority >= expected.priority)
                    .fold(0, |bits, span| bits | span.modifiers)
            } else {
                expected.modifiers
            };
            let actual = resolved
                .iter()
                .find(|span| span.range.contains(at))
                .unwrap();
            assert_eq!(
                (actual.kind, actual.modifiers),
                (expected.kind, modifiers),
                "byte {offset}"
            );
        }
        assert!(
            resolved
                .windows(2)
                .all(|pair| pair[0].range.end() <= pair[1].range.start())
        );
        assert_eq!(resolved.iter().filter(|span| span.kind == 0).count(), 1);
    }

    #[test]
    fn indexed_definition_coverage_matches_nested_and_overlapping_sites() {
        let sites: Vec<_> = [(1, 10), (3, 5), (12, 18), (17, 24)]
            .into_iter()
            .map(|(start, end)| tex_ls_parser::semantic::DefSite {
                name: "example".into(),
                kind: DefSiteKind::Command,
                name_range: TextRange::at(TextSize::new(start), TextSize::new(1)),
                range: TextRange::new(TextSize::new(start), TextSize::new(end)),
            })
            .collect();
        let index = DefinitionIndex::new(&sites);
        for start in 0..=25 {
            for end in start..=25 {
                let range = TextRange::new(TextSize::new(start), TextSize::new(end));
                assert_eq!(
                    index.contains(range),
                    sites.iter().any(|site| site.range.contains_range(range)),
                    "{range:?}"
                );
            }
        }
    }

    #[test]
    fn literal_csname_words_do_not_leak_from_nested_dynamic_or_unclosed_names() {
        let source = "\\csname first name\\endcsname \\csname naïve\\endcsname\n\\csname dynamic\\suffix\\endcsname \\csname outer\\csname inner\\endcsname\\endcsname\n\\csname unfinished";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(
                source,
                encoding,
                &compute(&db.snapshot(), &path, encoding, None),
            );
            let names: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == 2)
                .map(|(start, end, _)| &source[*start..*end])
                .collect();
            assert_eq!(names, ["first", "name", "naïve"]);
        }
    }

    #[test]
    fn stringified_tokens_are_literal_and_leave_following_commands_active() {
        let source = r"\string\ref{printed} \string\string\usepackage{real} \string$normal";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let snapshot = db.snapshot();
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(source, encoding, &compute(&snapshot, &path, encoding, None));
            let literal: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == LITERAL)
                .map(|(a, b, _)| &source[*a..*b])
                .collect();
            assert_eq!(literal, [r"\ref", r"\string", "$"]);
            for (needle, expected) in [("printed", TEXT), ("real", 7), ("normal", TEXT)] {
                let at = source.find(needle).unwrap();
                assert_eq!(
                    tokens
                        .iter()
                        .find(|(a, b, _)| *a <= at && at < *b)
                        .unwrap()
                        .2,
                    expected,
                    "{needle}"
                );
            }
        }
        assert!(
            snapshot
                .semantic_model(snapshot.lookup_file(&path).unwrap())
                .refs()
                .is_empty()
        );
    }

    #[test]
    fn primitive_integer_operands_are_numeric_without_consuming_printed_prose() {
        let source = "\\char65tail \\char 66 words \\char\"41 \\char'101 \\mathchar123 \\catcode37=12 \\mathcode65 = \"7001 \\delcode40=0 \\catcode`\\%=12 \\mathcode`A=\"7001\n\\char\\dynamic 1984 \\string\\char99";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        let numbers: Vec<_> = tokens
            .iter()
            .filter(|(_, _, kind)| *kind == 6)
            .map(|(a, b, _)| &source[*a..*b])
            .collect();
        assert_eq!(
            numbers,
            [
                "65", "66", "41", "101", "123", "37", "12", "65", "7001", "40", "0", "12", "7001"
            ]
        );
        for needle in ["tail", "words", "1984", "99"] {
            let at = source.find(needle).unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|(a, b, _)| *a <= at && at < *b)
                    .unwrap()
                    .2,
                TEXT,
                "{needle}"
            );
        }
        let source = r"\renewcommand{\char}[1]{#1}\char{65}";
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        let at = source.rfind("65").unwrap();
        assert_eq!(
            tokens
                .iter()
                .find(|(a, b, _)| *a <= at && at < *b)
                .unwrap()
                .2,
            TEXT
        );
    }

    #[test]
    fn bib_directives_are_keywords_and_entry_categories_are_types() {
        let source = "@string{press = {Literal}}\n@preamble{press}\n@book{key, title={Title}}\n@comment{ignored}\n";
        let path = PathBuf::from(fixture_path!("/colors/refs.bib"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        for (needle, expected) in [
            ("string", 4),
            ("preamble", 4),
            ("book", 1),
            ("comment", 4),
            ("title", 3),
            ("key", 2),
            ("Literal", TEXT),
            ("ignored", COMMENT),
        ] {
            let at = source.find(needle).unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|(a, b, _)| *a <= at && at < *b)
                    .unwrap()
                    .2,
                expected,
                "{needle}"
            );
        }
    }

    #[test]
    fn tikz_commands_keywords_and_operators_have_separate_purposes() {
        let source = r"\begin{tikzpicture}\draw (0,0) -- (1,1) node (mark) at (2,2) {Printed};\end{tikzpicture}";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        for (needle, expected) in [
            (r"\draw", 0),
            ("--", OPERATOR),
            ("node", 4),
            ("at", 4),
            ("mark", 2),
            ("Printed", TEXT),
        ] {
            let at = source.find(needle).unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|(a, b, _)| *a <= at && at < *b)
                    .unwrap()
                    .2,
                expected,
                "{needle}"
            );
        }
    }

    #[test]
    fn only_definition_placeholders_receive_the_parameter_purpose() {
        let source = concat!(
            "Prose #1 \\#1 \\verb|#1|\n",
            "\\newcommand{\\outer}[1]{#1 \\newcommand{\\inner}[1]{##1}}\n",
            "\\detokenize{#1} % #1\n",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(
                source,
                encoding,
                &compute(&db.snapshot(), &path, encoding, None),
            );
            let parameters: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == PARAMETER)
                .map(|(start, end, _)| &source[*start..*end])
                .collect();
            assert_eq!(parameters, ["#1", "##1"]);
            let initial = source.find("#1").unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|(a, b, _)| *a <= initial && initial < *b)
                    .unwrap()
                    .2,
                TEXT
            );
        }
    }

    #[test]
    fn numeric_text_options_and_numeric_color_names_keep_their_actual_purpose() {
        let source = concat!(
            "\\includegraphics[alt=1984,actualtext=2026,page=2,width=3pt]{x}\n",
            "\\hypersetup{pdfauthor=1984,pdftitle=2026}\n",
            "\\tikzset{label=1984,title=2pt,line width=2pt,unknown=123,unknown text=1984letters,custom dimension=3pt,draw=1984}\n",
            "\\definecolor{1984}{RGB}{0,0,0} \\color{1984}\n",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        for (needle, key, expected) in [
            ("alt=1984", "1984", TEXT),
            ("actualtext=2026", "2026", TEXT),
            ("page=2", "2", 6),
            ("width=3pt", "3", 6),
            ("pdfauthor=1984", "1984", TEXT),
            ("pdftitle=2026", "2026", TEXT),
            ("label=1984", "1984", TEXT),
            ("title=2pt", "2pt", TEXT),
            ("line width=2pt", "2", 6),
            ("unknown=123", "123", TEXT),
            ("unknown text=1984letters", "1984letters", TEXT),
            ("custom dimension=3pt", "3", 6),
            ("draw=1984", "1984", 2),
            ("\\color{1984}", "1984", 2),
        ] {
            let at = source.find(needle).unwrap() + needle.find(key).unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|(a, b, _)| *a <= at && at < *b)
                    .unwrap()
                    .2,
                expected,
                "{needle}"
            );
        }
    }

    #[test]
    fn nested_keyval_values_do_not_introduce_outer_keys() {
        let source = concat!(
            "\\includegraphics[alt={hello, middle, bye},width=5pt]{x}\n",
            "\\tikzset{thing/.style={label={first, center, last},draw=red},line width=2pt}\n",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(
                source,
                encoding,
                &compute(&db.snapshot(), &path, encoding, None),
            );
            let properties: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == 3)
                .map(|(start, end, _)| &source[*start..*end])
                .collect();
            assert_eq!(
                properties,
                [
                    "alt",
                    "width",
                    "thing/.style",
                    "label",
                    "draw",
                    "line width"
                ]
            );
            for needle in ["hello", "middle", "bye", "first", "center", "last"] {
                let at = source.find(needle).unwrap();
                assert_eq!(
                    tokens
                        .iter()
                        .find(|(a, b, _)| *a <= at && at < *b)
                        .unwrap()
                        .2,
                    TEXT,
                    "{needle}"
                );
            }
        }
    }

    #[test]
    fn local_file_command_overrides_preserve_only_earlier_builtin_roles() {
        let source = concat!(
            "\\input{before} \\renewcommand{\\input}[1]{Printed #1} \\input{after}\n",
            "\\usepackage[style=numeric]{biblatex} \\renewcommand{\\usepackage}[1]{#1} \\usepackage[style=other]{printed}\n",
            "\\import{chapters/}{real.tex} \\renewcommand{\\import}[2]{#1 #2} \\import{printed/}{text.tex}\n",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(
                source,
                encoding,
                &compute(&db.snapshot(), &path, encoding, None),
            );
            for (needle, expected) in [
                ("before", PATH),
                ("after", TEXT),
                ("numeric", 7),
                ("biblatex", 7),
                ("other", TEXT),
                ("printed}", TEXT),
                ("chapters/", PATH),
                ("real.tex", PATH),
                ("printed/", TEXT),
                ("text.tex", TEXT),
            ] {
                let at = source.find(needle).unwrap();
                assert_eq!(
                    tokens
                        .iter()
                        .find(|(a, b, _)| *a <= at && at < *b)
                        .unwrap()
                        .2,
                    expected,
                    "{needle}"
                );
            }
        }
    }

    #[test]
    fn copied_math_control_symbols_keep_macro_roles() {
        let source = r"\meaning\[ \[\meaning\]x\] \begin{align}\meaning\\ x\\ y\end{align}";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(
                source,
                encoding,
                &compute(&db.snapshot(), &path, encoding, None),
            );
            for needle in [r"\meaning\[", r"\meaning\]", r"\meaning\\"] {
                let at = source.find(needle).unwrap() + r"\meaning".len();
                assert_eq!(
                    tokens
                        .iter()
                        .find(|(start, end, _)| *start <= at && at < *end)
                        .unwrap()
                        .2,
                    0,
                    "{needle}"
                );
            }
        }
    }

    #[test]
    fn primitive_operand_arguments_do_not_inherit_key_option_or_numeric_roles() {
        let source = concat!(
            "é😀 \\let\\copy\\ref{printed-key}\n",
            "\\meaning\\includegraphics[width=2pt]{printed.png}\n",
            "\\show\\setcounter{printed-counter}{42}\n",
            "\\let\\copy\\ensuremath{a_b}\n",
            "$\\let\\copy\\text{a_c}$\\ensuremath{a_d}$\\text{a_e}$\n",
            "\\meaning\\ref{\\ref{nested-key}}\\ref{live-key}\n",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let tokens = decode(
                source,
                encoding,
                &compute(&db.snapshot(), &path, encoding, None),
            );
            for (needle, expected) in [
                ("printed-key", TEXT),
                ("width", TEXT),
                ("2pt", TEXT),
                ("printed.png", TEXT),
                ("printed-counter", TEXT),
                ("42", TEXT),
                ("_b", TEXT),
                ("_c", MATH),
                ("_d", MATH),
                ("_e", TEXT),
                ("nested-key", 2),
                ("live-key", 2),
            ] {
                let at = source.find(needle).unwrap();
                assert_eq!(
                    tokens
                        .iter()
                        .find(|(a, b, _)| *a <= at && at < *b)
                        .unwrap()
                        .2,
                    expected,
                    "{needle}: {encoding:?}"
                );
            }
        }
    }

    #[test]
    fn embedded_roles_follow_the_parser_in_both_position_encodings() {
        let source = "\\begin{minted}[escapeinside=||]{python}\ndef square(x):\n    return \"\\ref{fake}\" |\\ref{real}| # café\n\\end{minted}";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let snapshot = db.snapshot();
            let spans = decode(source, enc, &compute(&snapshot, &path, enc, None));
            let at = |needle: &str| {
                let offset = source.find(needle).unwrap();
                spans
                    .iter()
                    .find(|(a, b, _)| *a <= offset && offset < *b)
                    .unwrap()
                    .2
            };
            assert_eq!(at("def square"), 4);
            assert_eq!(at("square(x)"), FUNCTION);
            assert_eq!(at("return"), 4);
            assert_eq!(at("fake"), TEXT);
            assert_eq!(at("real"), 2);
            assert_eq!(at("# café"), COMMENT);
        }
    }

    #[test]
    fn long_unicode_lines_keep_identical_roles_in_both_encodings() {
        let body = "α + \\beta + 𝕏 + 42 ";
        let source = format!(
            "\\section{{é 😀}}\r\n${}$\ré\\ref{{clé}}\n",
            body.repeat(128)
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source.as_str(), None);
        let snapshot = db.snapshot();
        let utf8 = decode(
            &source,
            PositionEncoding::Utf8,
            &compute(&snapshot, &path, PositionEncoding::Utf8, None),
        );
        let utf16 = decode(
            &source,
            PositionEncoding::Utf16,
            &compute(&snapshot, &path, PositionEncoding::Utf16, None),
        );
        assert_eq!(utf8, utf16);
        assert_eq!(utf16.iter().filter(|(_, _, kind)| *kind == 0).count(), 130);
        for (start, end, kind) in utf16 {
            if kind == TEXT {
                assert!(!source[start..end].contains('\\'));
            }
        }
    }

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
    fn macro_parameters_substitute_before_detokenize_reads_its_argument() {
        let source = r"\newcommand{\show}[1]{\detokenize{#1 \ref{fake}}}";
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        let parameter = source.find("#1").unwrap();
        assert_eq!(
            tokens
                .iter()
                .find(|(a, b, _)| *a <= parameter && parameter < *b)
                .unwrap()
                .2,
            PARAMETER
        );
        assert!(
            db.snapshot()
                .semantic_model(db.snapshot().lookup_file(&path).unwrap())
                .refs()
                .is_empty()
        );
    }

    #[test]
    fn compound_settings_preserve_keys_names_numbers_and_printed_labels() {
        let source = concat!(
            "\\includegraphics[width=0.6\\linewidth,clip]{plot.png}\n",
            "\\tikzset{notice/.style={draw=brand,line width=2pt}}\n",
            "\\pgfkeys{title={Chapter 1984}}\n",
            "\\begin{tikzpicture}\\node (start) at (0,0) {Start 1984};\\draw[clip] (start)--(2,0);\\end{tikzpicture}\n",
            "\\begin{tabular}{lp{2cm}}Left & right \\\\ \\end{tabular}\n",
            "\\newcounter{exercise} \\setlength{\\textwidth}{\\dimexpr2\\linewidth/3\\relax}\n",
            "\\only<2->{Text 1984} \\char\"41 \\csname projectterm\\endcsname\n",
            "\\csname prefix\\suffix\\endcsname\n",
            "\\import{chapters/}{intro.tex}\n",
            "\\hyperref[eq:demo]{See 1984}\n",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        let at = |needle: &str| {
            let offset = source.find(needle).unwrap();
            tokens
                .iter()
                .find(|(a, b, _)| *a <= offset && offset < *b)
                .unwrap()
                .2
        };
        for (needle, kind) in [
            ("width=", 3),
            ("clip]", 3),
            ("0.6", 6),
            ("notice/.style", 3),
            ("brand", 2),
            ("line width", 3),
            ("2pt", 6),
            ("title=", 3),
            ("Chapter 1984", TEXT),
            ("start)", 2),
            ("0,0", 6),
            ("Start 1984", TEXT),
            ("lp{", 1),
            ("2cm", 6),
            ("& right", MATH),
            ("\\\\ ", MATH),
            ("exercise", 2),
            ("2\\linewidth", 6),
            ("3\\relax", 6),
            ("2->", 6),
            ("Text 1984", TEXT),
            ("41", 6),
            ("projectterm", 2),
            ("prefix\\suffix", TEXT),
            ("chapters/", PATH),
            ("intro.tex", PATH),
            ("eq:demo", 2),
            ("See 1984", TEXT),
        ] {
            assert_eq!(at(needle), kind, "{needle}");
        }
    }

    #[test]
    fn purposes_do_not_leak_across_literal_text_math_and_settings() {
        let source = concat!(
            "😀 prose 42 #1 x_y & \\texttt{file.tex} $x_1+42$\r",
            "\\url{https://host/a%20b?x=1_2&y=$z#part} \\path{a_b%file}\r",
            "\\verb|\\ref{fake}\r\\ref{real}\r",
            "\\detokenize{\\ref{fake} $x_1$ % comment\r body}\r",
            "\\newcommand{\\demo}[1]{\\textbf{#1}}\\setcounter{page}{42}\r",
            "\\section{Title \\emph{nested} \\ref{real} $x_1$}\r",
            "\\begin{comment}\r\\ref{fake}\r\\end{comment}\r",
            "\\begin{alltt}% $ x_1 \\ref{real}\\end{alltt}\r",
        );
        let path = PathBuf::from(fixture_path!("/colors/main.tex"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        for enc in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let result = compute(&db.snapshot(), &path, enc, None);
            let tokens = decode(source, enc, &result);
            let at = |needle: &str| {
                let offset = source.find(needle).unwrap();
                let token = tokens
                    .iter()
                    .find(|(start, end, _)| *start <= offset && offset < *end)
                    .unwrap();
                token.2
            };
            assert_eq!(at("42 #1"), TEXT);
            assert_eq!(at("#1 x_y"), TEXT);
            assert_eq!(at("x_y"), TEXT);
            assert_eq!(at("file.tex"), TEXT);
            assert_eq!(at("https://"), PATH);
            assert_eq!(at("a_b%file"), LITERAL);
            assert_eq!(at("\\ref{fake}"), LITERAL);
            assert_eq!(at("% comment"), COMMENT);
            assert_eq!(at("42}"), 6);
            assert!(tokens.windows(2).all(|pair| pair[0].1 <= pair[1].0));
            let references: Vec<_> = tokens
                .iter()
                .filter(|(_, _, kind)| *kind == 2)
                .map(|(a, b, _)| &source[*a..*b])
                .collect();
            assert_eq!(references, ["real", "page", "real", "real"]);
            assert_eq!(at("1+42"), TEXT);
            assert_eq!(at("$x_1"), MATH);
            assert_eq!(at("$\r"), MATH);
            let raw = result["data"].as_array().unwrap();
            let nested = source.find("nested").unwrap();
            let id = tokens
                .iter()
                .position(|(a, b, _)| *a <= nested && nested < *b)
                .unwrap();
            assert_eq!(raw[id * 5 + 4], json!(6));
            let math = source.rfind("$x_1$").unwrap() + 1;
            let id = tokens
                .iter()
                .position(|(a, b, _)| *a <= math && math < *b)
                .unwrap();
            assert_eq!(
                raw[id * 5 + 4],
                json!(0),
                "heading typography does not color mathematical operands"
            );
            let declaration = source.find("\\demo").unwrap();
            let id = tokens
                .iter()
                .position(|(a, b, _)| *a <= declaration && declaration < *b)
                .unwrap();
            assert_eq!(raw[id * 5 + 4], json!(1));
        }
    }

    #[test]
    fn bib_numbers_and_printed_values_are_neutral_but_abbreviations_are_identifiers() {
        let source = "@article{key, title={A 42 {word}}, year=2026, month=jan, note=\"literal\", url={https://host/a_1%20}, xdata={ parent, other }}";
        let path = PathBuf::from(fixture_path!("/colors/refs.bib"));
        let mut db = IncrementalDatabase::default();
        db.apply_change(&path, source, None);
        let tokens = decode(
            source,
            PositionEncoding::Utf16,
            &compute(&db.snapshot(), &path, PositionEncoding::Utf16, None),
        );
        let kind_at = |needle: &str| {
            let offset = source.find(needle).unwrap();
            tokens
                .iter()
                .find(|(a, b, _)| *a <= offset && offset < *b)
                .unwrap()
                .2
        };
        assert_eq!(kind_at("2026"), TEXT);
        assert_eq!(kind_at("A 42"), TEXT);
        assert_eq!(kind_at("literal"), TEXT);
        assert_eq!(kind_at("jan"), 2);
        assert_eq!(kind_at("year"), 3);
        assert_eq!(kind_at("https://"), PATH);
        assert_eq!(kind_at("parent"), 2);
        assert_eq!(kind_at(", other"), TEXT);
        assert_eq!(kind_at("other"), 2);
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
                .filter(|(_, _, kind)| *kind == LITERAL)
                .map(|(start, end, _)| &source[*start..*end])
                .collect();
            assert_eq!(strings, ["\\projectterm", "x y"]);
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
                        .filter(|(a, _, kind)| {
                            *kind == 0 || (*kind == MATH && source[*a..].starts_with("\\"))
                        })
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
