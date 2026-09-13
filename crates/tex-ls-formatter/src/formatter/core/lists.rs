use super::*;

/// True if `node` (an `ENVIRONMENT`) names a list environment the signature DB
/// marks `list` — `itemize`/`enumerate`/`description`, whose `\item`s the
/// formatter lays out one per line with a hanging indent (see
/// [`lower_list_environment`]).
pub(super) fn is_list_env(node: &SyntaxNode, cx: LowerCtx<'_>) -> bool {
    cx.signatures
        .environment_at(node)
        .is_some_and(|sig| sig.list)
}

/// Lower a list environment (`itemize`/`enumerate`/`description`): each `\item`
/// starts its own line at the body indent and its body is reflowed with the
/// configured [`ItemIndent`]. Under the default [`ItemIndent::Hang`], a
/// `description` item's wide `[label]` trails on the first line but does not deepen
/// the body indent. The framing (`\begin`/`\end`, the indented body with
/// leading/trailing `hard_line`) matches [`lower_environment`].
///
/// Under [`WrapMode::Preserve`] the body is *not* reflowed: the author's line breaks
/// and inner spacing are kept byte-faithful (see [`lower_item_chunks`]) and only the
/// continuation-line indentation is re-hung under the marker. Falls back to the plain
/// [`lower_environment`] when the body has no `\item` to anchor on, so an unusual
/// shape degrades to today's indented body rather than misformatting.
pub(super) fn lower_list_environment(node: &SyntaxNode, cx: LowerCtx<'_>) -> Ir {
    let EnvParts {
        leading,
        begin,
        body,
        end,
        lifted,
        // The `BEGIN` tail (see [`lower_begin`]) rides `body` as ordinary leading
        // elements: this path flattens the body itself, so it needs no splice.
        tail_len: _,
        body_header_token: _,
    } = split_environment(node, cx);

    let body_cx = cx.absorbing_trailing_control_newline(&body);
    let Some(body) = lower_list_body(&body, body_cx, lifted.as_ref()) else {
        return lower_environment(node, cx);
    };
    Ir::concat([
        leading,
        begin,
        Ir::indent(Ir::concat([Ir::hard_line(), body])),
        Ir::hard_line(),
        end,
    ])
}

/// Build the body IR of a list environment: split into items at each top-level
/// `\item`, collect a bounded Beamer overlay suffix into the marker, and render a
/// hanging-indented reflow of the content. Returns `None` (caller falls back) when
/// the body carries no `\item`.
pub(super) fn lower_list_body(
    body_elements: &[SyntaxElement],
    cx: LowerCtx<'_>,
    lifted: Option<&SyntaxToken>,
) -> Option<Ir> {
    let flat = flatten_list_body(body_elements, lifted);

    // Content before the first `\item` (usually just trivia); kept as its own
    // leading segment so nothing is dropped.
    let mut preamble: Vec<Vec<SyntaxElement>> = vec![Vec::new()];
    let mut items: Vec<ListItem> = Vec::new();
    let mut blank_pending = false;
    let mut index = 0;
    while index < flat.len() {
        match &flat[index] {
            FlatItem::Blank => {
                // A paragraph boundary: it separates items (recorded on the next
                // item) and, within an item, starts a fresh content chunk.
                blank_pending = true;
                match items.last_mut() {
                    Some(item) => item.chunks.push(Vec::new()),
                    None => preamble.push(Vec::new()),
                }
                index += 1;
            }
            FlatItem::El(el) if is_item_command(el) => {
                let mut item = split_item_marker(el, cx);
                index += 1;
                if let Some((suffix, end)) = item_overlay_marker_suffix(&flat, index) {
                    item.marker.push_str(&suffix);
                    item.glue_body = flat.get(end).is_some_and(
                        |next| matches!(next, FlatItem::El(el) if el.kind() == SyntaxKind::COMMENT),
                    );
                    index = end;
                }
                item.blank_before = blank_pending;
                items.push(item);
                blank_pending = false;
            }
            FlatItem::El(el) => {
                match items.last_mut() {
                    Some(item) => item.chunks.last_mut().unwrap().push(el.clone()),
                    None => preamble.last_mut().unwrap().push(el.clone()),
                }
                blank_pending = false;
                index += 1;
            }
        }
    }

    if items.is_empty() {
        return None;
    }

    let mut segments: Vec<Ir> = Vec::new();
    let mut seps: Vec<Ir> = Vec::new();
    let preamble_ir = lower_item_chunks(&preamble, cx);
    if !matches!(preamble_ir, Ir::Nil) {
        seps.push(Ir::hard_line()); // unused (segment 0 has no preceding separator)
        segments.push(preamble_ir);
    }
    for item in &items {
        seps.push(if item.blank_before {
            Ir::empty_line()
        } else {
            Ir::hard_line()
        });
        segments.push(render_list_item(item, cx));
    }

    let mut result: Vec<Ir> = Vec::with_capacity(segments.len().saturating_mul(2));
    for (i, segment) in segments.into_iter().enumerate() {
        if i > 0 {
            result.push(seps[i].clone());
        }
        result.push(segment);
    }
    Some(Ir::concat(result))
}

/// Render one [`ListItem`]: any bound doc-comment lines on their own lines above,
/// then the marker, then a space and the item's body reflowed inside an
/// [`Ir::align`] whose width comes from [`LowerCtx::item_indent`]. The `hang`
/// width is the control word plus its separating space (`\item `), deliberately
/// excluding a label or overlay. A comment glued to an overlay receives no
/// separating space; an empty item (marker with no body) renders as the bare
/// marker.
pub(super) fn render_list_item(item: &ListItem, cx: LowerCtx<'_>) -> Ir {
    let content = lower_item_chunks(&item.chunks, cx);
    let marker = Ir::verbatim(item.marker.clone());
    let body = if matches!(content, Ir::Nil) {
        marker
    } else {
        let separator = if item.glue_body {
            Ir::Nil
        } else {
            Ir::verbatim(" ")
        };
        let continuation_indent = match cx.item_indent {
            ItemIndent::Hang => item.hang,
            ItemIndent::Indent => cx.indent_width,
            ItemIndent::None => 0,
        };
        Ir::concat([marker, separator, Ir::align(continuation_indent, content)])
    };
    if item.doc_lines.is_empty() {
        return body;
    }
    let doc = item.doc_lines.iter().map(|line| Ir::verbatim(line.clone()));
    Ir::concat([Ir::join(Ir::hard_line(), doc), Ir::hard_line(), body])
}

/// Lower an item body's paragraph chunks, dispatching on the wrap mode: a
/// prose-wrapping mode reflows each chunk to width ([`reflow_chunks`]), while
/// [`WrapMode::Preserve`] keeps the author's breaks and inner spacing byte-faithful
/// ([`preserve_chunks`]). Either way the result sits inside the item's hanging
/// [`Ir::align`], so continuation lines indent under the marker.
pub(super) fn lower_item_chunks(chunks: &[Vec<SyntaxElement>], cx: LowerCtx<'_>) -> Ir {
    if cx.wraps_prose() {
        reflow_chunks(chunks, cx)
    } else {
        preserve_chunks(chunks, cx)
    }
}

/// Preserve-mode analogue of [`reflow_chunks`]: lower each paragraph chunk as prose
/// under [`WrapMode::Preserve`] ([`lower_prose_stream`]) so the author's line breaks
/// become `hard_line`s while inter-word spacing collapses to a single space, then
/// join the (non-empty) chunks with an [`Ir::empty_line`]. Inline-prose command
/// bodies are flattened in first (matching the paragraph path), so an `\emph{…}`
/// body collapses too while an opaque argument group stays verbatim. Each chunk's
/// own edge breaks are trimmed — the leading whitespace after `\item ` and any
/// trailing break — so the first line glues after the marker and no blank line
/// leaks; the interior newlines survive as `hard_line`s that hang under the marker
/// via the enclosing [`Ir::align`].
pub(super) fn preserve_chunks(chunks: &[Vec<SyntaxElement>], cx: LowerCtx<'_>) -> Ir {
    let parts = chunks
        .iter()
        .map(|chunk| {
            let flat = flatten_inline_prose(chunk.clone(), cx, false);
            let ir = Ir::concat(lower_prose_stream(flat.into_iter(), cx));
            let (_, ir) = peel_leading_break(ir);
            let (_, ir) = peel_trailing_break(ir);
            ir
        })
        .filter(|ir| !matches!(ir, Ir::Nil));
    Ir::join(Ir::empty_line(), parts)
}

/// Reflow each paragraph chunk of an item body and join the (non-empty) results
/// with an [`Ir::empty_line`], so a blank line inside an item becomes a blank line
/// between its paragraphs (still under the hanging indent).
pub(super) fn reflow_chunks(chunks: &[Vec<SyntaxElement>], cx: LowerCtx<'_>) -> Ir {
    let parts = chunks
        .iter()
        .map(|chunk| reflow_elements(chunk.iter().cloned(), cx, ReflowKind::Prose))
        .filter(|ir| !matches!(ir, Ir::Nil));
    Ir::join(Ir::empty_line(), parts)
}

/// Flatten a list-environment body into a stream of inline elements, reifying each
/// paragraph boundary as a [`FlatItem::Blank`]. Body-level trivia is classified by
/// its newline count: a blank-line run (`≥2` newlines) becomes the `Blank` (its
/// tokens are dropped — the boundary carries them), while a single-newline run is
/// kept in the stream so [`reflow_elements`] still sees the line break — dropping
/// it would glue a body-level own-line `%` onto the preceding content or a nested
/// `\end{…}` (issue #48). Leading and trailing runs (against `\begin`/`\end`) are
/// dropped either way; the list framing re-supplies those breaks.
pub(super) fn flatten_list_body(
    body_elements: &[SyntaxElement],
    lifted: Option<&SyntaxToken>,
) -> Vec<FlatItem> {
    let mut out: Vec<FlatItem> = Vec::new();
    let mut started = false;
    // Pending body-level trivia run: its tokens and how many newlines it spans.
    let mut run: Vec<SyntaxElement> = Vec::new();
    let mut run_newlines = 0usize;
    for element in body_elements {
        match element {
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {
                if t.kind() == SyntaxKind::NEWLINE {
                    run_newlines += 1;
                }
                run.push(element.clone());
                continue;
            }
            other if is_lifted_comment(other, lifted) => continue,
            _ => {}
        }
        if started {
            if run_newlines >= 2 {
                out.push(FlatItem::Blank);
            } else {
                out.extend(run.drain(..).map(FlatItem::El));
            }
        }
        run.clear();
        run_newlines = 0;
        match element {
            SyntaxElement::Node(p) if p.kind() == SyntaxKind::PARAGRAPH => {
                out.extend(
                    p.children_with_tokens()
                        .filter(|e| !is_lifted_comment(e, lifted))
                        .map(FlatItem::El),
                );
            }
            other => out.push(FlatItem::El(other.clone())),
        }
        started = true;
    }
    out
}

/// Whether `el` is a `\item` command node — the marker that starts a new list
/// item.
pub(super) fn is_item_command(el: &SyntaxElement) -> bool {
    el.as_node().is_some_and(|node| {
        node.kind() == SyntaxKind::COMMAND && command_name(node).as_deref() == Some("item")
    })
}

/// Read Beamer's bounded `\item<overlay>[label]<overlay>` suffix from the flat
/// list stream. The parser deliberately leaves angle-delimited syntax generic,
/// so list lowering recognizes only the complete, immediately following shape;
/// incomplete angle text remains ordinary item content.
pub(super) fn item_overlay_marker_suffix(
    flat: &[FlatItem],
    start: usize,
) -> Option<(String, usize)> {
    let (mut suffix, mut end) = angle_suffix(flat, start)?;
    if let Some((label, label_end)) = bracket_suffix(flat, end) {
        suffix.push_str(&label);
        end = label_end;
        if let Some((overlay, overlay_end)) = angle_suffix(flat, end) {
            suffix.push_str(&overlay);
            end = overlay_end;
        }
    }
    Some((suffix, end))
}

pub(super) fn angle_suffix(flat: &[FlatItem], start: usize) -> Option<(String, usize)> {
    let mut index = skip_flat_trivia(flat, start);
    let first = flat_element(flat.get(index)?)?;
    let first_text = element_source_text(first);
    if !first_text.starts_with('<') {
        return None;
    }

    let mut suffix = String::new();
    loop {
        let element = flat_element(flat.get(index)?)?;
        if element.kind() == SyntaxKind::COMMENT {
            return None;
        }
        if is_collapsible_trivia(element.kind()) {
            if !suffix.ends_with(' ') {
                suffix.push(' ');
            }
        } else {
            let text = element_source_text(element);
            suffix.push_str(&text);
            if text.ends_with('>') {
                return Some((suffix, index + 1));
            }
        }
        index += 1;
    }
}

pub(super) fn bracket_suffix(flat: &[FlatItem], start: usize) -> Option<(String, usize)> {
    let mut index = skip_flat_trivia(flat, start);
    let first = flat_element(flat.get(index)?)?;
    if first.kind() != SyntaxKind::L_BRACKET {
        return None;
    }

    let mut depth = 0usize;
    let mut suffix = String::new();
    loop {
        let element = flat_element(flat.get(index)?)?;
        match element.kind() {
            SyntaxKind::L_BRACKET => depth += 1,
            SyntaxKind::R_BRACKET => depth = depth.checked_sub(1)?,
            SyntaxKind::COMMENT => return None,
            _ => {}
        }
        if is_collapsible_trivia(element.kind()) {
            if !suffix.ends_with(' ') {
                suffix.push(' ');
            }
        } else {
            suffix.push_str(&element_source_text(element));
        }
        index += 1;
        if depth == 0 {
            return Some((suffix, index));
        }
    }
}

pub(super) fn skip_flat_trivia(flat: &[FlatItem], mut index: usize) -> usize {
    while let Some(FlatItem::El(element)) = flat.get(index) {
        if !is_collapsible_trivia(element.kind()) {
            break;
        }
        index += 1;
    }
    index
}

pub(super) fn flat_element(item: &FlatItem) -> Option<&SyntaxElement> {
    match item {
        FlatItem::El(element) => Some(element),
        FlatItem::Blank => None,
    }
}

pub(super) fn element_source_text(element: &SyntaxElement) -> String {
    match element {
        SyntaxElement::Node(node) => node.text().to_string(),
        SyntaxElement::Token(token) => token.text().to_string(),
    }
}

/// Split a `\item` command node into a [`ListItem`] (`blank_before` is the
/// caller's to set): the rendered marker string (the control word plus any leading
/// optional `[label]`, the only argument an item marker takes), the *hang* width
/// for continuation lines (the control word's rendered width plus one for the
/// separating space — deliberately excluding the `[label]` so a wide `description`
/// label does not deepen the body indent), and the trailing elements that are
/// really body content — a `{…}` group the greedy parser over-attached, which
/// belongs to the item body, not the marker. A `DOC_COMMENT` bound leading into
/// the `\item` yields the item's `doc_lines`, never marker or content.
pub(super) fn split_item_marker(el: &SyntaxElement, cx: LowerCtx<'_>) -> ListItem {
    let node = el.as_node().expect("item command is a node");
    let mut doc_lines: Vec<String> = Vec::new();
    let mut marker_parts: Vec<Ir> = Vec::new();
    let mut content: Vec<SyntaxElement> = Vec::new();
    let mut hang = 1; // the space separating the marker from the body
    let mut in_content = false;
    for child in node.children_with_tokens() {
        if in_content {
            content.push(child);
            continue;
        }
        match &child {
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::DOC_COMMENT => {
                doc_lines.extend(
                    n.children_with_tokens()
                        .filter_map(|e| e.into_token())
                        .filter(|t| t.kind() == SyntaxKind::COMMENT)
                        .map(|t| t.text().to_string()),
                );
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::CONTROL_WORD => {
                hang += t.text().chars().count();
                marker_parts.push(Ir::verbatim(t.text()));
            }
            // Trivia between the control word and an optional label is not part of
            // the marker.
            SyntaxElement::Token(t) if is_collapsible_trivia(t.kind()) => {}
            SyntaxElement::Node(n) if n.kind() == SyntaxKind::OPTIONAL => {
                marker_parts.push(lower_node(n, cx));
            }
            // A brace group (or anything else) is body content, not the marker.
            other => {
                in_content = true;
                content.push(other.clone());
            }
        }
    }
    let marker = Printer::new(FormatStyle::default()).print_flat(&Ir::concat(marker_parts));
    ListItem {
        doc_lines,
        marker,
        hang,
        glue_body: false,
        chunks: vec![content],
        blank_before: false,
    }
}
