// Virtual fixture paths must be absolute on the native host too. Windows requires
// a drive; Unix and WASM use the filesystem root. No directories are created.
#[allow(unused_macros)]
macro_rules! fixture_path {
    ($path:literal) => {{
        #[cfg(windows)]
        {
            concat!("C:", $path)
        }
        #[cfg(not(windows))]
        {
            $path
        }
    }};
}

macro_rules! fixture_uri {
    ($path:literal) => {{
        #[cfg(windows)]
        {
            concat!("file:///C:", $path)
        }
        #[cfg(not(windows))]
        {
            concat!("file://", $path)
        }
    }};
}
