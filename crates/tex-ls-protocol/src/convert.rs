//! Convert operations.
use super::*;

/// Map a document URI to the path the salsa file cache is keyed by. For a `file:`
/// URI this is the real filesystem path (percent-decoded), so `\input`/bib
/// resolution and on-disk sibling reads share one path space and a project can be
/// assembled. A non-`file` buffer (untitled, etc.) falls back to the URI string as
/// a synthetic key; it simply never joins a project.
pub fn uri_to_path(uri: &Uri) -> PathBuf {
    uri_to_fs_path(uri).unwrap_or_else(|| PathBuf::from(uri.as_str()))
}

/// Which language pipeline a document feeds, by its path extension. Defaults to
/// [`FileKind::Tex`] for anything that is not a `.bib` file (including unsaved
/// buffers with no extension), matching the conservative CLI/stdin behavior. The
/// resolution itself lives in [`file_kind_or_tex`], shared with the CLI's
/// `--stdin-filepath`.
pub fn file_kind_for(path: &Path) -> FileKind {
    file_kind_or_tex(path)
}

/// Convert a byte [`TextRange`] to an LSP [`Range`] via `idx`.
pub fn lsp_range(idx: &LineIndex, range: TextRange) -> Range {
    byte_range_to_lsp(idx, usize::from(range.start()), usize::from(range.end()))
}

/// Convert a batch in source order, retaining the caller's order and duplicates.
pub(crate) fn lsp_ranges(idx: &LineIndex, ranges: &[TextRange]) -> Vec<Range> {
    let mut cursor = idx.position_cursor();
    if ranges
        .windows(2)
        .all(|pair| pair[0].start() <= pair[1].start())
    {
        return ranges
            .iter()
            .map(|&range| lsp_range_with_cursor(&mut cursor, range))
            .collect();
    }
    let mut order: Vec<_> = (0..ranges.len()).collect();
    order.sort_unstable_by_key(|&slot| ranges[slot].start());
    let mut converted = vec![Range::default(); ranges.len()];
    for slot in order {
        converted[slot] = lsp_range_with_cursor(&mut cursor, ranges[slot]);
    }
    converted
}

/// Convert nested ranges by ordering both endpoints. Shared enclosing spans
/// then reuse their positions rather than repeatedly scanning to the file end.
pub(crate) fn lsp_nested_ranges(idx: &LineIndex, ranges: &[TextRange]) -> Vec<Range> {
    byte_ranges_to_lsp(
        idx,
        ranges
            .iter()
            .map(|range| (usize::from(range.start()), usize::from(range.end()))),
    )
}

pub(crate) fn byte_ranges_to_lsp(
    idx: &LineIndex,
    ranges: impl ExactSizeIterator<Item = (usize, usize)>,
) -> Vec<Range> {
    let count = ranges.len();
    let mut endpoints: Vec<_> = ranges
        .enumerate()
        .flat_map(|(slot, (start, end))| [(start, slot, false), (end, slot, true)])
        .collect();
    endpoints.sort_unstable_by_key(|&(at, _, _)| at);
    let mut converted = vec![Range::default(); count];
    let mut cursor = idx.position_cursor();
    for (at, slot, end) in endpoints {
        let (line, character) = cursor.position(at);
        let position = Position::new(line, character);
        if end {
            converted[slot].end = position;
        } else {
            converted[slot].start = position;
        }
    }
    converted
}

/// Keep the cursor at each range's start so enclosing ranges do not force the
/// next child to recount its Unicode prefix. The endpoint uses a cheap fork.
pub(crate) fn lsp_range_with_cursor(
    cursor: &mut tex_ls_analysis::text::line_index::PositionCursor<'_, '_>,
    range: TextRange,
) -> Range {
    byte_range_to_lsp_with_cursor(cursor, usize::from(range.start()), usize::from(range.end()))
}

pub(crate) fn byte_range_to_lsp_with_cursor(
    cursor: &mut tex_ls_analysis::text::line_index::PositionCursor<'_, '_>,
    start: usize,
    end: usize,
) -> Range {
    let (sl, sc) = cursor.position(start);
    let (el, ec) = cursor.clone().position(end);
    Range::new(Position::new(sl, sc), Position::new(el, ec))
}

/// Convert a local file URI without losing native path bytes or authorities.
/// This conversion performs no filesystem access. Other document schemes keep
/// their URI identity and never become native paths.
#[cfg(not(target_arch = "wasm32"))]
pub fn uri_to_fs_path(uri: &Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let path = url.to_file_path().ok()?;
    (!path.as_os_str().as_encoded_bytes().contains(&0)).then_some(path)
}

/// Browser file URIs name virtual UTF-8 paths with Unix separators.
#[cfg(target_arch = "wasm32")]
pub fn uri_to_fs_path(uri: &Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    if url.scheme() != "file"
        || url.host_str().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = percent_encoding::percent_decode_str(url.path())
        .decode_utf8()
        .ok()?;
    if path.contains('\0') {
        return None;
    }
    Some(PathBuf::from(path.as_ref()))
}

/// Encode an absolute native path as a file URI without lossy string conversion.
#[cfg(not(target_arch = "wasm32"))]
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return None;
    }
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

#[cfg(target_arch = "wasm32")]
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let path = path.to_str()?;
    if !path.starts_with('/') || path.contains('\0') {
        return None;
    }
    let mut url = url::Url::parse("file:///").ok()?;
    // Segment insertion encodes literal percent signs and backslashes too.
    url.path_segments_mut()
        .ok()?
        .clear()
        .extend(path[1..].split('/'));
    url.as_str().parse().ok()
}

/// Convert a byte range into an LSP range via the (encoding-aware) [`LineIndex`].
pub fn byte_range_to_lsp(idx: &LineIndex, start: usize, end: usize) -> Range {
    let (sl, sc) = idx.position(start);
    let (el, ec) = idx.position(end);
    Range {
        start: Position::new(sl, sc),
        end: Position::new(el, ec),
    }
}

#[cfg(test)]
mod file_uri_tests {
    use super::*;

    #[test]
    fn range_batches_preserve_arbitrary_order_duplicates_and_byte_semantics() {
        let source = "é😀\\section{α} text\\label{clé}\r\n𝕏\ré\n";
        let mut ranges: Vec<_> = (0..=source.len())
            .map(|start| {
                TextRange::new(
                    TextSize::try_from(start).unwrap(),
                    TextSize::try_from((start + 3).min(source.len())).unwrap(),
                )
            })
            .collect();
        ranges.extend([
            TextRange::new(0.into(), TextSize::try_from(source.len()).unwrap()),
            ranges[3],
        ]);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let index = LineIndex::with_encoding(source, encoding);
            for _ in 0..2 {
                let expected: Vec<_> = ranges
                    .iter()
                    .map(|&range| lsp_range(&index, range))
                    .collect();
                assert_eq!(lsp_ranges(&index, &ranges), expected);
                assert_eq!(lsp_nested_ranges(&index, &ranges), expected);
                ranges.reverse();
            }
        }
    }

    #[test]
    fn range_cursor_matches_independent_queries_for_overlapping_ranges() {
        let source = "é😀\\section{α} text\\label{clé}\r\n𝕏\ré\n";
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let index = LineIndex::with_encoding(source, encoding);
            let mut cursor = index.position_cursor();
            for (start, end) in [
                (0, source.len()),
                (0, 6),
                (6, 18),
                (7, 9),
                (0, 0),
                (28, 35),
                (3, 4),
                (source.len(), source.len()),
            ] {
                let range = TextRange::new(
                    rowan::TextSize::try_from(start).unwrap(),
                    rowan::TextSize::try_from(end).unwrap(),
                );
                assert_eq!(
                    lsp_range_with_cursor(&mut cursor, range),
                    byte_range_to_lsp(&index, start, end)
                );
            }
        }
    }

    #[test]
    fn virtual_uris_and_relative_paths_do_not_become_local_files() {
        let uri: Uri = "untitled:buffer".parse().unwrap();
        assert_eq!(uri_to_fs_path(&uri), None);
        assert_eq!(uri_to_path(&uri), PathBuf::from("untitled:buffer"));
        assert_eq!(path_to_uri(Path::new("relative.tex")), None);
        for value in [
            "file:///tmp/a.tex?query",
            "file:///tmp/a.tex#fragment",
            "file:///tmp/%00.tex",
        ] {
            assert_eq!(uri_to_fs_path(&value.parse().unwrap()), None, "{value}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_paths_preserve_bytes_and_literal_backslashes() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/a\\b %\xff.tex".to_vec(),
        ));
        let uri = path_to_uri(&path).unwrap();
        assert!(uri.as_str().contains("%5C"));
        assert!(uri.as_str().contains("%FF"));
        assert_eq!(uri_to_fs_path(&uri), Some(path));
        assert_eq!(
            uri_to_fs_path(&"file://server/share/a.tex".parse().unwrap()),
            None
        );
        assert_eq!(
            uri_to_fs_path(&"file://localhost/tmp/a.tex".parse().unwrap()),
            Some(PathBuf::from("/tmp/a.tex"))
        );
        assert_eq!(
            uri_to_fs_path(&"file:///C:/a.tex".parse().unwrap()),
            Some(PathBuf::from("/C:/a.tex"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_drives_and_unc_paths_round_trip() {
        for path in [r"C:\Users\me\a b.tex", r"\\server\share\a.tex"] {
            let path = PathBuf::from(path);
            assert_eq!(uri_to_fs_path(&path_to_uri(&path).unwrap()), Some(path));
        }
    }
}
