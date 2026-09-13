//! Formatting edits computed against one analysis snapshot.
use super::*;

/// Format a captured source, refusing malformed input and returning no edit for
/// unchanged text. Cancellation propagates to the host's response boundary.
pub fn compute_format(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    sentence: SentenceOptions<'_>,
) -> Option<Vec<TextEdit>> {
    let file = snapshot.lookup_file(path)?;
    let formatted = if kind == FileKind::Bib {
        if !snapshot.bib_parse_diagnostics(file).is_empty() {
            return None;
        }
        bib_format_node(&snapshot.parsed_bib_tree(file), style).ok()?
    } else {
        if !snapshot.parse_diagnostics(file).is_empty() {
            return None;
        }
        format_node_with_signatures_sentence(
            &snapshot.parsed_tree(file),
            style,
            snapshot.scope_signatures(file),
            sentence,
        )
        .ok()?
    };
    let text = snapshot.file_text(file);
    let idx = snapshot.file_line_index(file, encoding);
    Some(diff_to_edits(
        &idx,
        text,
        TextRange::new(TextSize::new(0), TextSize::from(text.len() as u32)),
        &formatted,
    ))
}

/// Format the document blocks overlapping the requested range.
pub fn compute_range_format(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    sel_range: Range,
    sentence: SentenceOptions<'_>,
) -> Option<Vec<TextEdit>> {
    if kind == FileKind::Bib {
        use tex_ls_analysis::bib::syntax::SyntaxKind as Kind;
        let file = snapshot.lookup_file(path)?;
        if !snapshot.bib_parse_diagnostics(file).is_empty() {
            return None;
        }
        let idx = snapshot.file_line_index(file, encoding);
        let start = idx.offset_at(sel_range.start.line, sel_range.start.character);
        let end = idx.offset_at(sel_range.end.line, sel_range.end.character);
        let mut edits = Vec::new();
        for node in snapshot.parsed_bib_tree(file).children().filter(|node| {
            matches!(
                node.kind(),
                Kind::ENTRY | Kind::STRING_ENTRY | Kind::PREAMBLE_ENTRY
            )
        }) {
            let range = node.text_range();
            let overlaps = if start == end {
                range.contains_inclusive(TextSize::from(start as u32))
            } else {
                usize::from(range.start()) < end && usize::from(range.end()) > start
            };
            if !overlaps {
                continue;
            }
            let original = node.text().to_string();
            let formatted = tex_ls_analysis::bib::format_with_style(&original, style).ok()?;
            let formatted = formatted.trim_end_matches(['\r', '\n']);
            if formatted != original {
                edits.push(TextEdit {
                    range: lsp_range(&idx, range),
                    new_text: formatted.into(),
                });
            }
        }
        return Some(edits);
    }
    let file = snapshot.lookup_file(path)?;
    if !snapshot.parse_diagnostics(file).is_empty() {
        return None;
    }
    let idx = snapshot.file_line_index(file, encoding);
    let start = idx.offset_at(sel_range.start.line, sel_range.start.character);
    let end = idx.offset_at(sel_range.end.line, sel_range.end.character);
    let sel = TextRange::new(
        TextSize::new(start.min(end).min(u32::MAX as usize) as u32),
        TextSize::new(start.max(end).min(u32::MAX as usize) as u32),
    );
    range_edits_for_root(
        &snapshot.parsed_tree(file),
        snapshot.file_text(file),
        &idx,
        sel,
        style,
        snapshot.scope_signatures(file),
        sentence,
    )
}

/// Re-indent a block when the typed brace closes a multiline construct.
pub fn compute_on_type_format(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    position: Position,
    sentence: SentenceOptions<'_>,
) -> Option<Vec<TextEdit>> {
    if kind == FileKind::Bib {
        return None;
    }
    let file = snapshot.lookup_file(path)?;
    if !snapshot.parse_diagnostics(file).is_empty() {
        return None;
    }
    let text = snapshot.file_text(file);
    let idx = snapshot.file_line_index(file, encoding);
    let off = idx
        .offset_at(position.line, position.character)
        .min(u32::MAX as usize) as u32;
    let root = snapshot.parsed_tree(file);
    if !closes_multiline_construct(&root, text, off) {
        return Some(Vec::new());
    }
    range_edits_for_root(
        &root,
        text,
        &idx,
        TextRange::empty(TextSize::new(off)),
        style,
        snapshot.scope_signatures(file),
        sentence,
    )
}

/// Expand `sel` to top-level-block boundaries, format those blocks, and diff the
/// result against the original slice into minimal edits. `None` when the selection
/// touches no block or the formatter refuses; `Some(vec![])` when already
/// formatted.
#[allow(clippy::too_many_arguments)]
pub fn range_edits_for_root(
    root: &SyntaxNode,
    text: &str,
    idx: &LineIndex,
    sel: TextRange,
    style: FormatStyle,
    external: &SignatureDb,
    sentence: SentenceOptions<'_>,
) -> Option<Vec<TextEdit>> {
    let block_range = expand_to_document_blocks(root, sel)?;
    let fragment =
        format_node_range_with_signatures_sentence(root, style, external, block_range, sentence)
            .ok()?;
    let base = usize::from(block_range.start());
    let end = usize::from(block_range.end());
    if fragment == text[base..end] {
        return Some(Vec::new());
    }
    Some(diff_to_edits(idx, text, block_range, &fragment))
}

/// Decide whether a `}` typed at byte `offset` (cursor just past the brace)
/// structurally closes a *multi-line* construct that warrants a re-indent: a plain
/// multi-line group, or an `\end{…}` terminating a multi-line environment. A `}`
/// that closes an inline group (e.g. `\textbf{x}`) or that *opens* an environment
/// (`\begin{…}`) returns `false`.
pub fn closes_multiline_construct(root: &SyntaxNode, text: &str, offset: u32) -> bool {
    let Some(brace) = root.token_at_offset(TextSize::new(offset)).left_biased() else {
        return false;
    };
    if brace.kind() != SyntaxKind::R_BRACE {
        return false;
    }
    // The node this brace closes: a `GROUP`, or the `NAME_GROUP` of a
    // `\begin`/`\end`.
    let Some(close_node) = brace.parent() else {
        return false;
    };
    // The structural unit to (potentially) re-indent.
    let unit = match close_node.parent() {
        // Closing `\end{…}` → re-indent the whole environment.
        Some(p) if p.kind() == SyntaxKind::END => p
            .parent()
            .filter(|e| e.kind() == SyntaxKind::ENVIRONMENT)
            .unwrap_or(p),
        // This `}` *opens* an environment; nothing is closed yet.
        Some(p) if p.kind() == SyntaxKind::BEGIN => return false,
        // A plain group (command body, brace group, …).
        _ => close_node,
    };
    let range = unit.text_range();
    let (lo, hi) = (usize::from(range.start()), usize::from(range.end()));
    text.get(lo..hi).is_some_and(|s| s.contains('\n'))
}

/// Expand a selection to whole document-level-block boundaries: the cover of every
/// `ROOT` child *node* overlapping `sel`, except that a canonical `document`
/// environment exposes its direct body nodes as document-level blocks. Its body is
/// formatter-defined to sit flush at the root indentation, so those nodes have the
/// same independent layout context as root children. Other environments remain
/// indivisible: their specialized list, alignment, math, and indentation layouts
/// need the complete environment.
///
/// A partial selection always pulls in the whole structural units it touches.
/// Child-node iteration naturally skips inter-block trivia.
/// Returns `None` when the selection touches no block (e.g. a cursor in blank space
/// between blocks), semantics there is nothing to format.
pub fn expand_to_document_blocks(root: &SyntaxNode, sel: TextRange) -> Option<TextRange> {
    let mut acc: Option<TextRange> = None;
    for child in root.children() {
        let r = child.text_range();
        // A cursor (empty selection) hits the block whose range contains it
        // (touch-inclusive, so a cursor at a block edge still selects it); a
        // non-empty selection hits any block it genuinely overlaps.
        let hit = if sel.is_empty() {
            r.contains_inclusive(sel.start())
        } else {
            sel.start() < r.end() && r.start() < sel.end()
        };
        if !hit {
            continue;
        }
        if document_body_contains(&child, sel)
            && let Some(body) = cover_overlapping_children(&child, sel)
        {
            acc = Some(acc.map_or(body, |a| a.cover(body)));
        } else {
            acc = Some(acc.map_or(r, |a| a.cover(r)));
        }
    }
    acc
}

/// Whether `range` lies wholly between a canonical `document` environment's
/// delimiters. Only this built-in no-indent environment is transparent here;
/// custom and specialized environments keep their full structural context.
pub fn document_body_contains(node: &SyntaxNode, range: TextRange) -> bool {
    let Some(environment) = Environment::cast(node.clone()) else {
        return false;
    };
    if environment.name().as_deref() != Some("document") {
        return false;
    }
    let (Some(begin), Some(end)) = (environment.begin(), environment.end()) else {
        return false;
    };
    range.start() >= begin.syntax().text_range().end()
        && range.end() <= end.syntax().text_range().start()
}

pub fn cover_overlapping_children(container: &SyntaxNode, sel: TextRange) -> Option<TextRange> {
    container
        .children()
        .filter(|child| !matches!(child.kind(), SyntaxKind::BEGIN | SyntaxKind::END))
        .filter_map(|child| {
            let range = child.text_range();
            let hit = if sel.is_empty() {
                range.contains_inclusive(sel.start())
            } else {
                sel.start() < range.end() && range.start() < sel.end()
            };
            hit.then_some(range)
        })
        .reduce(TextRange::cover)
}

/// Diff the formatted `fragment` against the original `text[block_range]` slice and
/// emit ordered [`TextEdit`] values mapped back into document coordinates.
/// Whitespace-gap alignment keeps ordinary formatting edits small in linear time.
/// Canonical non-trivia changes (such as BibTeX field ordering) use a line-level
/// LCS; large blocks use one bounded replacement. Equal edges are trimmed safely.
pub fn diff_to_edits(
    idx: &LineIndex,
    text: &str,
    block_range: TextRange,
    fragment: &str,
) -> Vec<TextEdit> {
    let base = usize::from(block_range.start());
    let end = usize::from(block_range.end());
    let original = &text[base..end];

    if let Some(edits) = trivia_edits(idx, text, base, original, fragment) {
        return edits;
    }

    // Lines keep their trailing `\n` (`split_inclusive`), so equality compares whole
    // lines and the byte offsets stay exact.
    let a: Vec<&str> = original.split_inclusive('\n').collect();
    let b: Vec<&str> = fragment.split_inclusive('\n').collect();
    let (n, m) = (a.len(), b.len());

    // Safety valve: cap the LCS table so a huge block cannot blow up; fall back to
    // one wholesale replace of the block range.
    if n.saturating_mul(m) > 4_000_000 {
        return narrow_edit(idx, text, base, end, fragment)
            .into_iter()
            .collect();
    }

    // lcs[i][j] = length of the longest common subsequence of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    // Walk the table, coalescing each run of deletes/inserts into one replace edit.
    let mut edits = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut a_off = base; // byte offset of a[i] within `text`
    let mut del_start = base;
    let mut del_end = base;
    let mut ins = String::new();
    let mut in_hunk = false;
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            if in_hunk {
                edits.extend(narrow_edit(idx, text, del_start, del_end, &ins));
                ins.clear();
                in_hunk = false;
            }
            a_off += a[i].len();
            i += 1;
            j += 1;
        } else if j == m || (i < n && lcs[i + 1][j] >= lcs[i][j + 1]) {
            // delete a[i]
            if !in_hunk {
                del_start = a_off;
                in_hunk = true;
            }
            a_off += a[i].len();
            del_end = a_off;
            i += 1;
        } else {
            // insert b[j]
            if !in_hunk {
                del_start = a_off;
                del_end = a_off;
                in_hunk = true;
            }
            ins.push_str(b[j]);
            j += 1;
        }
    }
    if in_hunk {
        edits.extend(narrow_edit(idx, text, del_start, del_end, &ins));
    }
    edits
}

/// When canonical formatting preserves non-trivia bytes, align them directly,
/// producing independent whitespace-gap edits in linear time, even in huge files.
fn trivia_edits(
    idx: &LineIndex,
    text: &str,
    base: usize,
    original: &str,
    formatted: &str,
) -> Option<Vec<TextEdit>> {
    let (a, b) = (original.as_bytes(), formatted.as_bytes());
    let (mut i, mut j) = (0, 0);
    let mut edits = Vec::new();
    while i < a.len() || j < b.len() {
        let (start, replacement_start) = (i, j);
        while a.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        while b.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        if i != start || j != replacement_start {
            edits.extend(narrow_edit(
                idx,
                text,
                base + start,
                base + i,
                &formatted[replacement_start..j],
            ));
        }
        if i == a.len() && j == b.len() {
            break;
        }
        if a.get(i) != b.get(j) {
            return None;
        }
        i += 1;
        j += 1;
    }
    Some(edits)
}

/// Preserve equal prefix/suffix bytes without cutting a UTF code point or CRLF.
fn narrow_edit(
    idx: &LineIndex,
    text: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> Option<TextEdit> {
    let original = &text[start..end];
    if original == replacement {
        return None;
    }
    fn boundary(text: &str, at: usize) -> bool {
        text.is_char_boundary(at)
            && !(at > 0
                && text.as_bytes().get(at - 1) == Some(&b'\r')
                && text.as_bytes().get(at) == Some(&b'\n'))
    }
    let mut prefix = original
        .bytes()
        .zip(replacement.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !boundary(original, prefix) || !boundary(replacement, prefix) {
        prefix -= 1;
    }
    let mut suffix = original[prefix..]
        .bytes()
        .rev()
        .zip(replacement[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !boundary(original, original.len() - suffix)
        || !boundary(replacement, replacement.len() - suffix)
    {
        suffix -= 1;
    }
    Some(TextEdit {
        range: byte_range_to_lsp(idx, start + prefix, end - suffix),
        new_text: replacement[prefix..replacement.len() - suffix].into(),
    })
}

/// Format multiple selections atomically, merging expanded blocks first.
#[allow(clippy::too_many_arguments)]
pub fn compute_ranges_format(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
    style: FormatStyle,
    kind: FileKind,
    ranges: &[Range],
    sentence: SentenceOptions<'_>,
) -> Option<Vec<TextEdit>> {
    let file = snapshot.lookup_file(path)?;
    if kind == FileKind::Bib {
        let mut edits = Vec::new();
        for range in ranges {
            for edit in
                compute_range_format(snapshot, path, encoding, style, kind, *range, sentence)?
            {
                if !edits.contains(&edit) {
                    edits.push(edit);
                }
            }
        }
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        return Some(edits);
    }
    if !snapshot.parse_diagnostics(file).is_empty() {
        return None;
    }
    let root = snapshot.parsed_tree(file);
    let idx = snapshot.file_line_index(file, encoding);
    let mut blocks: Vec<_> = ranges
        .iter()
        .filter_map(|range| {
            let a = idx.offset_at(range.start.line, range.start.character) as u32;
            let b = idx.offset_at(range.end.line, range.end.character) as u32;
            expand_to_document_blocks(
                &root,
                TextRange::new(TextSize::from(a.min(b)), TextSize::from(a.max(b))),
            )
        })
        .collect();
    blocks.sort_by_key(|r| (r.start(), r.end()));
    let mut merged: Vec<TextRange> = Vec::new();
    for block in blocks {
        if let Some(last) = merged.last_mut()
            && block.start() <= last.end()
        {
            *last = last.cover(block);
        } else {
            merged.push(block);
        }
    }
    let mut edits = Vec::new();
    for block in merged {
        let fragment = format_node_range_with_signatures_sentence(
            &root,
            style,
            snapshot.scope_signatures(file),
            block,
            sentence,
        )
        .ok()?;
        edits.extend(diff_to_edits(
            &idx,
            snapshot.file_text(file),
            block,
            &fragment,
        ));
    }
    Some(edits)
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use tex_ls_analysis::incremental::IncrementalDatabase;

    fn apply(source: &str, target: &str, encoding: PositionEncoding) -> Vec<TextEdit> {
        let index = LineIndex::with_encoding(source, encoding);
        let edits = diff_to_edits(
            &index,
            source,
            TextRange::new(0.into(), (source.len() as u32).into()),
            target,
        );
        let mut result = source.to_owned();
        let mut previous = source.len();
        for edit in edits.iter().rev() {
            let start = index.offset_at(edit.range.start.line, edit.range.start.character);
            let end = index.offset_at(edit.range.end.line, edit.range.end.character);
            assert!(start <= end && end <= previous);
            for offset in [start, end] {
                assert!(source.is_char_boundary(offset));
                assert!(
                    !(offset > 0
                        && source.as_bytes().get(offset - 1) == Some(&b'\r')
                        && source.as_bytes().get(offset) == Some(&b'\n'))
                );
            }
            result.replace_range(start..end, &edit.new_text);
            previous = start;
        }
        assert_eq!(result, target);
        edits
    }

    #[test]
    fn canonical_edits_round_trip_both_encodings_and_protected_text() {
        let tex = "\\section{𝕏é}\r\nalpha  beta % keep  this\r\n\\begin{verbatim}\r\n𝕏  x\r\n\\end{verbatim}\r\n";
        let bib = "@book{clé,title={𝕏 title},author={Doe, Jane}}\r\n";
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            for (source, kind) in [(tex, FileKind::Tex), (bib, FileKind::Bib)] {
                let mut db = IncrementalDatabase::default();
                let path = PathBuf::from(if kind == FileKind::Bib {
                    fixture_path!("/project/main.bib")
                } else {
                    fixture_path!("/project/main.tex")
                });
                db.apply_change(&path, source, None);
                let snapshot = db.snapshot();
                let style = FormatStyle::default();
                let edits = compute_format(
                    &snapshot,
                    &path,
                    encoding,
                    style,
                    kind,
                    SentenceOptions::default(),
                )
                .unwrap();
                let file = snapshot.lookup_file(&path).unwrap();
                let target = if kind == FileKind::Bib {
                    bib_format_node(&snapshot.parsed_bib_tree(file), style).unwrap()
                } else {
                    format_node_with_signatures_sentence(
                        &snapshot.parsed_tree(file),
                        style,
                        snapshot.scope_signatures(file),
                        SentenceOptions::default(),
                    )
                    .unwrap()
                };
                assert_eq!(edits, apply(source, &target, encoding));
                assert!(!edits.is_empty());
                if kind == FileKind::Tex {
                    assert!(
                        edits
                            .iter()
                            .all(|edit| edit.new_text.chars().all(char::is_whitespace))
                    );
                }
                drop(snapshot);
                db.apply_change(&path, target.as_str(), None);
                assert!(
                    compute_format(
                        &db.snapshot(),
                        &path,
                        encoding,
                        style,
                        kind,
                        SentenceOptions::default()
                    )
                    .unwrap()
                    .is_empty()
                );
            }
        }
    }

    #[test]
    fn large_and_nontrivia_fallbacks_preserve_exact_output() {
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let source = "𝕏  x\r\n".repeat(3_000);
            let target = "𝕏 x\n".repeat(3_000);
            assert_eq!(apply(&source, &target, encoding).len(), 6_000);
            let target = "𝕐 x\n".repeat(3_000);
            assert_eq!(apply(&source, &target, encoding).len(), 1);
            apply("é𝕏 x\r\nz\r\n", "ê𝕐 y\nz\n", encoding);
            assert!(apply("same\r\n", "same\r\n", encoding).is_empty());
        }
    }
}
