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
