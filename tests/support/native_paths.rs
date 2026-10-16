//! Filesystem preconditions for native-path tests.

#[cfg(unix)]
pub(crate) fn supports_non_utf8_names(parent: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let probe = parent.join(std::ffi::OsStr::from_bytes(b"native-name-probe\xff"));
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            std::fs::remove_dir(&probe).expect("remove native-name capability probe");
            true
        }
        Err(error) => {
            // APFS rejects these names before the operation under test can run.
            // Other filesystem or permission failures must still fail the test.
            if cfg!(target_os = "macos") && error.raw_os_error() == Some(92) {
                eprintln!("fixture filesystem rejects non-UTF8 names with EILSEQ");
                false
            } else {
                panic!("create native-name capability probe at {probe:?}: {error}");
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn native_names(
    parent: &std::path::Path,
    unicode: &str,
    non_utf8: &[u8],
) -> Vec<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    let mut names = vec![std::ffi::OsString::from(unicode)];
    if supports_non_utf8_names(parent) {
        names.push(std::ffi::OsString::from_vec(non_utf8.to_vec()));
    }
    names
}
