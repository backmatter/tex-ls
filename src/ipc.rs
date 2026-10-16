//! Inverse search transport: a PDF viewer telling the running language server to
//! reveal a source position.
//!
//! Inverse search is always viewer-initiated, and viewers drive it by running a
//! command. So `tex-ls inverse-search` is a *client* that has to find the server
//! belonging to the file the viewer resolved — a different process, started by
//! the editor, that this one knows nothing about. This module is that rendezvous.
//! The server half's policy (does this server own the file? what does the editor
//! get told?) lives in `lsp.rs`.
//!
//! # Discovery
//!
//! Each listening server writes an [`Advertisement`] — `<ipc_dir>/<pid>.json` —
//! naming its transport address, an authentication token, and the workspace roots
//! it owns. The client reads every advertisement, prefers the server whose root
//! contains the file (longest match first, so nested workspaces are at least
//! deterministic), and tries each in turn until one accepts.
//!
//! texlab instead binds one fixed `texlab.sock`, `unlink`ing whatever was there.
//! With two editor windows open — a paper and a thesis, the ordinary case — the
//! second server silently orphans the first, and inverse search always lands in
//! whichever started last, which may not even have the file. Advertisements cost
//! a directory read and remove that failure mode entirely; the flags a viewer is
//! configured with are identical either way.
//!
//! A crashed server leaves a stale advertisement behind. Nothing reaps it on a
//! timer: the client unlinks advertisements whose sockets are missing or refuse
//! connections, which needs no liveness probing and no background thread.
//!
//! # Transport
//!
//! A Unix-domain socket where one exists, and a loopback TCP socket on Windows,
//! whose port the advertisement already carries. `std` does not expose AF_UNIX on
//! Windows (texlab reaches for `uds_windows`), and the advertisement makes the
//! TCP branch about twenty lines rather than a second discovery mechanism.
//!
//! A same-user process can read the advertisement either way, so the token is not
//! a boundary against one. It exists because a *loopback port is reachable
//! without the file*: without it, another local user could scan ports and make
//! your editor jump to a path of their choosing.
//!
//! # Framing
//!
//! One JSON object, one line, one reply. Deliberately not JSON-RPC: both ends are
//! the same binary at the same version, so a `Content-Length` header and an id
//! would buy nothing.

use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long a client waits for a server's reply before writing it off. A server
/// that has not answered by then is wedged, and the viewer is blocked on us.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_FRAME_BYTES: usize = 64 * 1024;
const READ_POLL: Duration = Duration::from_millis(50);

/// A viewer's resolved source position, as sent to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InverseSearchRequest {
    /// The `.tex` file, canonicalized by the client.
    #[serde(with = "crate::native_path::path")]
    pub path: PathBuf,
    /// 1-based, as SyncTeX and every viewer count.
    pub line: u32,
    /// 0-based column, or 0 when the viewer supplies none.
    pub character: u32,
    /// The token from the advertisement this request was addressed to.
    pub token: String,
}

/// A server's answer. `accepted: false` means "not my file, try another server",
/// which is what makes the client's fan-out terminate correctly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InverseSearchResponse {
    pub accepted: bool,
    pub reason: Option<String>,
}

/// Which socket family an [`Advertisement`]'s address names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// A Unix-domain socket path.
    Unix,
    /// A file URI for a Unix socket path that is not valid UTF8.
    #[serde(rename = "unix-uri")]
    UnixUri,
    /// A `127.0.0.1:<port>` address.
    Tcp,
}

/// A listening server, as published for clients to find.
///
/// Written at bind and unlinked at shutdown. Clients also unlink advertisements
/// after a definitive missing or refused connection; transient errors retain them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advertisement {
    pub pid: u32,
    pub transport: Transport,
    /// A socket path, a Unix socket file URI, or `127.0.0.1:<port>`.
    pub address: String,
    /// Echoed back by the client and checked by the server.
    pub token: String,
    /// The workspace roots this server owns. Empty means "will take anything",
    /// which is what a server started with no workspace folders reports.
    #[serde(with = "crate::native_path::paths")]
    pub roots: Vec<PathBuf>,
}

/// Something that went wrong delivering an inverse search.
#[derive(Debug)]
pub enum IpcError {
    /// No advertisement at all, so no language server is listening.
    NoServer,
    /// Servers were found, but none claimed the file.
    NoServerForFile(PathBuf),
    /// The IPC directory could not be read.
    Io(std::io::Error),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoServer => write!(
                f,
                "no tex-ls language server is listening for inverse search \
                 (is your editor running one, and does it support window/showDocument?)"
            ),
            Self::NoServerForFile(path) => write!(
                f,
                "no listening tex-ls language server has {} in its workspace",
                path.display()
            ),
            Self::Io(err) => write!(f, "could not read the IPC directory: {err}"),
        }
    }
}

impl std::error::Error for IpcError {}

/// Where advertisements live: `$TEX_LS_IPC_DIR`, else the user's runtime
/// directory, else the temp directory — plus a user-scoped component.
///
/// The last fallback matters: a bare `/tmp` is shared between users on some
/// systems, so the directory is named per-user and (on Unix) created `0700`, and
/// advertisements inside it are `0600`. Without that, another local user could
/// read which files you have open.
pub fn ipc_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TEX_LS_IPC_DIR") {
        return PathBuf::from(dir);
    }
    let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
    base.join(format!("tex-ls-{}", user_scope()))
}

/// A per-user directory-name component. The uid on Unix; on Windows the temp
/// directory is already per-user, so a constant is enough.
fn user_scope() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` is always successful and touches no memory we own.
        unsafe { libc_getuid() }.to_string()
    }
    #[cfg(not(unix))]
    {
        "user".to_owned()
    }
}

// `getuid(2)`, declared rather than pulled in: `std` exposes no accessor for the
// process's own uid, and one four-line declaration is a smaller commitment than a
// `libc` dependency in a tree that has none. The call cannot fail, takes no
// arguments, and touches no memory, so every use site is trivially sound. `uid_t`
// is `u32` on every Unix tex-ls builds for.
#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Create `dir` if needed, `0700` on Unix.
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A 128-bit hex token.
///
/// `RandomState` is seeded from the OS random source; two of them plus the
/// process id and a monotonic instant give a value another local user cannot
/// predict. That is the whole bar here — the token guards a loopback port
/// against port scanning, not a filesystem against its owner — so it is not
/// worth a `rand` dependency.
fn token() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut hi = RandomState::new().build_hasher();
    let mut lo = RandomState::new().build_hasher();
    let salt = std::time::Instant::now();
    hi.write_u32(std::process::id());
    hi.write_usize(&salt as *const _ as usize);
    lo.write_u32(std::process::id());
    lo.write_usize(format!("{salt:?}").len());
    format!("{:016x}{:016x}", hi.finish(), lo.finish())
}

/// The half of an accepted connection that answers it.
///
/// Held separately from the request so the LSP main loop can decide acceptance
/// with the rest of its state in hand, and reply before it goes on to talk to the
/// editor. Dropping one without answering leaves the client waiting out its
/// timeout, so [`reject`](Self::reject) exists for the "not my file" path.
pub struct Responder {
    stream: Option<Stream>,
}

impl Responder {
    /// Accept the request. The client stops fanning out.
    pub fn accept(self) {
        self.answer(InverseSearchResponse {
            accepted: true,
            reason: None,
        });
    }

    /// Decline it, so the client tries the next server.
    pub fn reject(self, reason: &str) {
        self.answer(InverseSearchResponse {
            accepted: false,
            reason: Some(reason.to_owned()),
        });
    }

    fn answer(mut self, response: InverseSearchResponse) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        let mut out = BufWriter::new(stream);
        if serde_json::to_writer(&mut out, &response).is_ok() {
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Transport: a Unix socket where one exists, loopback TCP on Windows.
// ---------------------------------------------------------------------------

#[cfg(unix)]
use std::os::unix::net::{UnixListener as SysListener, UnixStream as SysStream};

#[cfg(unix)]
type Stream = SysStream;
#[cfg(not(unix))]
type Stream = std::net::TcpStream;

#[cfg(unix)]
type Acceptor = SysListener;
#[cfg(not(unix))]
type Acceptor = std::net::TcpListener;

/// The longest socket path a Unix domain socket can carry. `sun_path` is 108
/// bytes on Linux and 104 on macOS and the BSDs; take the smaller so a diagnosis
/// is never wrong in the direction that matters.
#[cfg(unix)]
const MAX_SOCKET_PATH: usize = 104;

#[cfg(unix)]
fn bind(dir: &Path) -> std::io::Result<(Acceptor, Transport, String)> {
    // Advertisements are consumed by processes with another working directory.
    // Keep configured symlink spellings: a short alias may be necessary to fit
    // the Unix socket path limit even when its physical directory is long.
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    let path = dir.join(format!("{}.sock", std::process::id()));
    // `bind` reports this as a bare "File name too long", which tells the user
    // nothing they can act on. The default directory is nowhere near the limit,
    // so anyone hitting it set `ipcDir` themselves and can move it.
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the socket path {} is {} bytes, over the {MAX_SOCKET_PATH}-byte limit \
                 for a Unix socket; set `forwardSearch.ipcDir` to a shorter directory",
                path.display(),
                path.as_os_str().len(),
            ),
        ));
    }
    // A previous run with the same pid (after a reboot, or a recycled pid) may
    // have left the node behind; `bind` fails on an existing path.
    let _ = std::fs::remove_file(&path);
    let (transport, address) = match path.to_str() {
        Some(path) => (Transport::Unix, path.to_owned()),
        None => (
            Transport::UnixUri,
            tex_ls_protocol::path_to_uri(&path)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Cannot represent Unix socket path as a file URI",
                    )
                })?
                .to_string(),
        ),
    };
    let listener = SysListener::bind(&path)?;
    Ok((listener, transport, address))
}

#[cfg(not(unix))]
fn bind(_dir: &Path) -> std::io::Result<(Acceptor, Transport, String)> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?.to_string();
    Ok((listener, Transport::Tcp, address))
}

#[cfg(unix)]
fn unix_socket_path(transport: Transport, address: &str) -> Option<PathBuf> {
    match transport {
        Transport::Unix => Some(address.into()),
        Transport::UnixUri => address
            .parse::<lsp_types::Uri>()
            .ok()
            .and_then(|uri| tex_ls_protocol::uri_to_fs_path(&uri))
            .filter(|path| path.is_absolute()),
        Transport::Tcp => None,
    }
}

fn connect(ad: &Advertisement, timeout: Duration) -> std::io::Result<Stream> {
    match ad.transport {
        #[cfg(unix)]
        Transport::Unix | Transport::UnixUri => {
            let path = unix_socket_path(ad.transport, &ad.address).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid Unix socket address",
                )
            })?;
            let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
            socket.connect_timeout(&socket2::SockAddr::unix(&path)?, timeout)?;
            let stream = SysStream::from(std::os::fd::OwnedFd::from(socket));
            // A saturated Unix backlog can return EAGAIN without starting a
            // connection. A writable socket must still name a connected peer.
            stream.peer_addr()?;
            Ok(stream)
        }
        #[cfg(not(unix))]
        Transport::Unix | Transport::UnixUri => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "unix sockets are unavailable on this platform",
        )),
        #[cfg(unix)]
        Transport::Tcp => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this server advertised TCP, which this platform does not dial",
        )),
        #[cfg(not(unix))]
        Transport::Tcp => {
            let address = ad
                .address
                .parse()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
            std::net::TcpStream::connect_timeout(&address, timeout)
        }
    }
}

// ---------------------------------------------------------------------------
// Server half.
// ---------------------------------------------------------------------------

/// A bound inverse-search socket plus its published advertisement.
///
/// [`Drop`] unlinks both, so a clean shutdown leaves nothing behind. That matters
/// more than it looks: the language server runs in-process in the LSP test
/// binary, so a leaked listener would hold a bound socket for the whole run.
pub struct Listener {
    acceptor: Acceptor,
    advertisement: PathBuf,
    address: String,
    transport: Transport,
    token: String,
    shutdown: AtomicBool,
}

impl Listener {
    /// Bind a socket in `dir` and publish an advertisement claiming `roots`.
    ///
    /// `None` when the directory or the socket cannot be created — inverse search
    /// is a convenience, so a server that cannot listen still serves everything
    /// else.
    pub fn bind_in(dir: &Path, roots: Vec<PathBuf>) -> Option<Self> {
        if let Err(err) = ensure_dir(dir) {
            log::warn!(
                "inverse search: cannot use the IPC directory {}: {err}",
                dir.display()
            );
            return None;
        }
        let (acceptor, transport, address) = bind(dir)
            .inspect_err(|err| log::warn!("inverse search: cannot bind a socket: {err}"))
            .ok()?;
        let token = token();
        let advertisement = dir.join(format!("{}.json", std::process::id()));
        let listener = Self {
            acceptor,
            advertisement,
            address,
            transport,
            token,
            shutdown: AtomicBool::new(false),
        };
        listener.publish(roots).ok()?;
        Some(listener)
    }

    fn publish(&self, roots: Vec<PathBuf>) -> std::io::Result<()> {
        let ad = Advertisement {
            pid: std::process::id(),
            transport: self.transport,
            address: self.address.clone(),
            token: self.token.clone(),
            roots,
        };
        let body = serde_json::to_vec(&ad).map_err(std::io::Error::other)?;
        std::fs::write(&self.advertisement, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.advertisement, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Block until one request arrives, or until [`wake`](Self::wake) is called.
    ///
    /// A connection carrying a bad token, or no parsable request, is dropped
    /// without an answer and does not surface here.
    pub fn accept_one(&self) -> Option<(InverseSearchRequest, Responder)> {
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return None;
            }
            let stream = self.acceptor.accept().ok()?.0;
            if self.shutdown.load(Ordering::SeqCst) {
                return None;
            }
            let Some(request) = read_request(&stream, &self.shutdown) else {
                continue;
            };
            if request.token != self.token {
                log::warn!("inverse search: rejecting a request with a bad token");
                continue;
            }
            return Some((
                request,
                Responder {
                    stream: Some(stream),
                },
            ));
        }
    }

    /// Unblock a thread parked in [`accept_one`](Self::accept_one).
    ///
    /// Dropping a channel cannot wake a blocking `accept`, so shutdown dials the
    /// socket once with a bounded connect and lets the flag do the rest. A full
    /// backlog already contains a connection that can unblock the accept.
    pub fn wake(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let ad = Advertisement {
            pid: std::process::id(),
            transport: self.transport,
            address: self.address.clone(),
            token: self.token.clone(),
            roots: Vec::new(),
        };
        let _ = connect(&ad, READ_POLL);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.advertisement);
        #[cfg(unix)]
        if let Some(path) = unix_socket_path(self.transport, &self.address) {
            let _ = std::fs::remove_file(path);
        }
        // The directory itself is left alone: `TEX_LS_IPC_DIR` may point at one
        // we did not create, and an empty directory costs nothing.
    }
}

fn read_request(stream: &Stream, shutdown: &AtomicBool) -> Option<InverseSearchRequest> {
    stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
    let line = read_frame(stream, Some(shutdown))?;
    serde_json::from_str(&line).ok()
}

/// Bound the entire frame, including peers that keep sending partial data.
/// Short reads let shutdown interrupt an accepted connection as well as accept.
fn read_frame(mut stream: &Stream, shutdown: Option<&AtomicBool>) -> Option<String> {
    let deadline = std::time::Instant::now() + REPLY_TIMEOUT;
    let mut frame = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        if shutdown.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return None;
        }
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        stream
            .set_read_timeout(Some(remaining.min(READ_POLL)))
            .ok()?;
        match stream.read(&mut buffer) {
            Ok(0) => return None,
            Ok(count) => {
                let bytes = &buffer[..count];
                let newline = bytes.iter().position(|byte| *byte == b'\n');
                let end = newline.map_or(count, |index| index + 1);
                if frame.len() + end > MAX_FRAME_BYTES {
                    return None;
                }
                frame.extend_from_slice(&bytes[..end]);
                if newline.is_some() {
                    return String::from_utf8(frame).ok();
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Client half.
// ---------------------------------------------------------------------------

/// Deliver an inverse search to whichever running server owns `path`.
///
/// Reads every advertisement in `dir`, orders them so a server whose workspace
/// root contains `path` is tried first (longest matching root wins), and sends to
/// each until one accepts. Missing or connection-refusing sockets are stale and
/// get unlinked on the way past; transient delivery errors retain advertisements.
pub fn send_inverse_search_in(
    dir: &Path,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<(), IpcError> {
    let mut candidates = read_advertisements(dir)?;
    if candidates.is_empty() {
        return Err(IpcError::NoServer);
    }
    // Longest matching root first, then unowned servers; ties broken by pid so
    // the order is at least stable.
    candidates.sort_by_key(|(_, ad)| {
        let score = ad
            .roots
            .iter()
            .filter(|root| path.starts_with(root))
            .map(|root| root.components().count())
            .max();
        // `None` (no matching root) must sort after every match.
        (std::cmp::Reverse(score), ad.pid)
    });

    for (file, ad) in &candidates {
        match deliver(ad, path, line, character) {
            Ok(true) => return Ok(()),
            // Declined: this server does not own the file, so try the next.
            Ok(false) => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                // Unreachable: the server died without cleaning up.
                let _ = std::fs::remove_file(file);
            }
            // A busy or temporarily failing server still owns its advertisement.
            Err(_) => continue,
        }
    }
    Err(IpcError::NoServerForFile(path.to_path_buf()))
}

/// [`send_inverse_search_in`] against the default [`ipc_dir`].
pub fn send_inverse_search(path: &Path, line: u32, character: u32) -> Result<(), IpcError> {
    send_inverse_search_in(&ipc_dir(), path, line, character)
}

/// Every readable advertisement in `dir`, paired with its file so a stale one can
/// be unlinked. A missing directory means no server has ever listened here.
fn read_advertisements(dir: &Path) -> Result<Vec<(PathBuf, Advertisement)>, IpcError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(IpcError::NoServer),
        Err(err) => return Err(IpcError::Io(err)),
    };
    Ok(entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter(|path| trustworthy(path))
        .filter_map(|path| {
            let body = std::fs::read(&path).ok()?;
            Some((path, serde_json::from_slice(&body).ok()?))
        })
        .collect())
}

/// Whether an advertisement is safe to act on: on Unix, that it is ours and that
/// no one else can write it. A planted advertisement would otherwise capture an
/// inverse search and learn which file is being edited.
fn trustworthy(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        // SAFETY: `getuid` is always successful and touches no memory we own.
        if meta.uid() != unsafe { libc_getuid() } {
            return false;
        }
        meta.permissions().mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// Send one request and read the answer. `Ok(false)` is a live server declining.
fn deliver(ad: &Advertisement, path: &Path, line: u32, character: u32) -> std::io::Result<bool> {
    let stream = connect(ad, REPLY_TIMEOUT)?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT))?;

    let request = InverseSearchRequest {
        path: path.to_path_buf(),
        line,
        character,
        token: ad.token.clone(),
    };
    {
        let mut out = BufWriter::new(&stream);
        serde_json::to_writer(&mut out, &request).map_err(std::io::Error::other)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }

    let line = read_frame(&stream, None)
        .ok_or_else(|| std::io::Error::other("Invalid or timed out inverse-search response"))?;
    let response: InverseSearchResponse =
        serde_json::from_str(&line).map_err(std::io::Error::other)?;
    Ok(response.accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn absolute_short_ipc_alias_preserves_socket_path_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let physical = tmp.path().join("x".repeat(MAX_SOCKET_PATH));
        std::fs::create_dir(&physical).unwrap();
        let alias = tmp.path().join("i");
        std::os::unix::fs::symlink(&physical, &alias).unwrap();
        let listener =
            Listener::bind_in(&alias, vec![]).expect("short configured alias remains usable");
        let advertisements = read_advertisements(&alias).unwrap();
        let expected = alias.join(format!("{}.sock", std::process::id()));
        assert_eq!(Path::new(&advertisements[0].1.address), expected);
        let result = connect(&advertisements[0].1, READ_POLL);
        let connected = result.is_ok();
        drop(result);
        drop(listener);
        assert!(connected);
        assert!(!expected.exists());
    }

    #[cfg(unix)]
    #[test]
    fn relative_utf8_ipc_directory_advertises_absolute_socket() {
        let cwd = std::env::current_dir().unwrap();
        let parent = cwd.join("target");
        std::fs::create_dir_all(&parent).unwrap();
        let tmp = tempfile::tempdir_in(parent).unwrap();
        let dir = tmp.path().strip_prefix(&cwd).unwrap().join("ipc");
        assert!(!dir.is_absolute());
        let listener = Listener::bind_in(&dir, vec![]).unwrap();
        let advertisements = read_advertisements(&dir).unwrap();
        assert_eq!(advertisements.len(), 1);
        let ad = &advertisements[0].1;
        let expected = tmp
            .path()
            .join("ipc")
            .join(format!("{}.sock", std::process::id()));
        let absolute = Path::new(&ad.address).is_absolute();
        let actual = ad.address.clone();
        drop(listener);
        assert!(
            absolute,
            "advertisement must reach the socket from another working directory"
        );
        assert_eq!(ad.transport, Transport::Unix);
        assert_eq!(Path::new(&actual), expected);
    }

    #[cfg(unix)]
    #[test]
    fn inverse_delivery_preserves_native_source_and_workspace_root() {
        use std::os::unix::ffi::OsStringExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .join(std::ffi::OsString::from_vec(b"project\xff".to_vec()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join(std::ffi::OsString::from_vec(b"main\xfe.tex".to_vec()));
        std::fs::write(&source, "text").unwrap();
        let source = source.canonicalize().unwrap();
        let dir = tmp.path().join("ipc");
        let listener = std::sync::Arc::new(Listener::bind_in(&dir, vec![root.clone()]).unwrap());
        let ads = read_advertisements(&dir).unwrap();
        assert_eq!(ads[0].1.roots, vec![root]);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = {
            let listener = listener.clone();
            std::thread::spawn(move || {
                let request = listener.accept_one().map(|(request, responder)| {
                    let path = request.path;
                    responder.accept();
                    path
                });
                done_tx.send(request).unwrap();
            })
        };
        let result = send_inverse_search_in(&dir, &source, 42, 7);
        if result.is_err() {
            listener.wake();
        }
        let received = done_rx.recv_timeout(Duration::from_secs(1));
        if received.is_err() {
            listener.wake();
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        worker.join().unwrap();
        assert!(result.is_ok(), "native inverse request must be delivered");
        assert_eq!(received.unwrap(), Some(source));
    }

    #[cfg(unix)]
    #[test]
    fn inverse_request_and_advertisement_roots_roundtrip_native_paths() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(
            b"/project/main\xff.tex".to_vec(),
        ));
        let request = InverseSearchRequest {
            path: path.clone(),
            line: 42,
            character: 7,
            token: "token".into(),
        };
        let value = serde_json::to_value(&request).expect("native request path serializes");
        assert_eq!(
            value["path"]["uri"],
            tex_ls_protocol::path_to_uri(&path).unwrap().as_str()
        );
        assert_eq!(
            serde_json::from_value::<InverseSearchRequest>(value).unwrap(),
            request
        );
        let advertisement = Advertisement {
            pid: 7,
            transport: Transport::Unix,
            address: "/normal.sock".into(),
            token: "token".into(),
            roots: vec![PathBuf::from("/normal"), path],
        };
        let value = serde_json::to_value(&advertisement).expect("native workspace roots serialize");
        assert_eq!(value["roots"][0], "/normal");
        assert!(value["roots"][1]["uri"].is_string());
        assert_eq!(
            serde_json::from_value::<Advertisement>(value).unwrap(),
            advertisement
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_advertisement_preserves_non_utf8_socket_directory() {
        use std::os::unix::ffi::OsStringExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join(std::ffi::OsString::from_vec(b"ipc\xff".to_vec()));
        let listener = Listener::bind_in(&dir, vec![]).expect("bind exact native directory");
        let advertisements = read_advertisements(&dir).unwrap();
        assert_eq!(advertisements.len(), 1);
        let result = connect(&advertisements[0].1, READ_POLL);
        // No accept thread is needed. Close both sockets and remove the exact
        // native node even if old lossy cleanup cannot find it.
        let connected = result.is_ok();
        drop(result);
        drop(listener);
        let socket = dir.join(format!("{}.sock", std::process::id()));
        let removed_on_drop = !socket.exists();
        let _ = std::fs::remove_file(socket);
        assert!(removed_on_drop, "drop must unlink the exact native socket");
        assert!(
            connected,
            "published address must reach the bound native socket"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_uri_listener_wake_and_drop_preserve_native_socket_path() {
        use std::os::unix::ffi::OsStringExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join(std::ffi::OsString::from_vec(b"wake\xff".to_vec()));
        let listener = std::sync::Arc::new(Listener::bind_in(&dir, vec![]).unwrap());
        let socket = dir.join(format!("{}.sock", std::process::id()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = {
            let listener = listener.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                done_tx.send(listener.accept_one().is_none()).unwrap();
            })
        };
        started_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        listener.wake();
        let completed = done_rx.recv_timeout(Duration::from_secs(1));
        if completed.is_err() {
            // Unblock the original exact socket on failure before joining.
            let peer = SysStream::connect(&socket).unwrap();
            drop(peer);
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        worker.join().unwrap();
        drop(listener);
        let removed = !socket.exists();
        let _ = std::fs::remove_file(socket);
        assert!(
            completed.is_ok_and(|stopped| stopped),
            "self-wake must unblock accept"
        );
        assert!(removed, "drop must unlink the native socket path");
    }

    /// Bind a listener in a fresh directory. Every test passes its directory
    /// explicitly: `std::env::set_var` is `unsafe` under edition 2024 and the
    /// test binary is multi-threaded, so the environment is never touched.
    fn listener(dir: &Path, roots: Vec<PathBuf>) -> Listener {
        Listener::bind_in(dir, roots).expect("bind an inverse-search socket")
    }

    #[test]
    fn request_round_trips_and_is_acked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        let listener = listener(&dir, vec![]);

        let file = tmp.path().join("main.tex");
        let client = {
            let (dir, file) = (dir.clone(), file.clone());
            std::thread::spawn(move || send_inverse_search_in(&dir, &file, 42, 7))
        };

        let (request, responder) = listener.accept_one().expect("a request");
        assert_eq!(request.path, file);
        assert_eq!(request.line, 42);
        assert_eq!(request.character, 7);
        assert_eq!(request.token, listener.token);
        responder.accept();

        client.join().unwrap().expect("delivered");
    }

    #[test]
    fn a_rejecting_server_does_not_swallow_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        let listener = listener(&dir, vec![]);

        let file = tmp.path().join("main.tex");
        let client = {
            let (dir, file) = (dir.clone(), file.clone());
            std::thread::spawn(move || send_inverse_search_in(&dir, &file, 1, 0))
        };

        let (_, responder) = listener.accept_one().expect("a request");
        responder.reject("not mine");

        let err = client.join().unwrap().expect_err("nobody claimed the file");
        assert!(matches!(err, IpcError::NoServerForFile(_)), "{err:?}");
    }

    #[test]
    fn a_server_whose_roots_exclude_the_file_is_tried_last() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        // Two listeners cannot share a pid, so bind one and fake the other's
        // advertisement pointing at the same socket: what is under test is the
        // *ordering*, and the accept side proves which one was dialled first.
        let owner = listener(&dir, vec![tmp.path().join("owned")]);
        let stranger = Advertisement {
            pid: std::process::id() + 1,
            transport: owner.transport,
            address: owner.address.clone(),
            token: owner.token.clone(),
            roots: vec![tmp.path().join("elsewhere")],
        };
        std::fs::write(
            dir.join(format!("{}.json", stranger.pid)),
            serde_json::to_vec(&stranger).unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join(format!("{}.json", stranger.pid)),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }

        let file = tmp.path().join("owned").join("ch1.tex");
        let client = {
            let (dir, file) = (dir.clone(), file.clone());
            std::thread::spawn(move || send_inverse_search_in(&dir, &file, 1, 0))
        };

        // The first connection must carry the owner's token, i.e. the owning
        // advertisement was ordered first.
        let (request, responder) = owner.accept_one().expect("a request");
        assert_eq!(request.token, owner.token);
        responder.accept();
        client.join().unwrap().expect("delivered");
    }

    #[test]
    fn a_stale_advertisement_is_unlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        ensure_dir(&dir).unwrap();
        let stale = dir.join("999999.json");
        let ad = Advertisement {
            pid: 999_999,
            transport: if cfg!(unix) {
                Transport::Unix
            } else {
                Transport::Tcp
            },
            address: if cfg!(unix) {
                dir.join("999999.sock").to_string_lossy().into_owned()
            } else {
                "127.0.0.1:9".to_owned()
            },
            token: "deadbeef".to_owned(),
            roots: vec![],
        };
        std::fs::write(&stale, serde_json::to_vec(&ad).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let err = send_inverse_search_in(&dir, &tmp.path().join("main.tex"), 1, 0)
            .expect_err("the advertised server is not there");
        assert!(matches!(err, IpcError::NoServerForFile(_)), "{err:?}");
        assert!(!stale.exists(), "a stale advertisement must be unlinked");
    }

    #[cfg(unix)]
    #[test]
    fn an_over_long_socket_path_is_diagnosed() {
        // `bind` reports only "File name too long"; the user needs to be told
        // which knob moves it.
        let tmp = tempfile::tempdir().unwrap();
        let mut dir = tmp.path().to_path_buf();
        while dir.as_os_str().len() <= MAX_SOCKET_PATH {
            dir.push("deeply-nested-directory-name");
        }
        let err = bind(&dir).expect_err("this path cannot hold a socket");
        let message = err.to_string();
        assert!(message.contains("ipcDir"), "{message}");
        assert!(message.contains(&MAX_SOCKET_PATH.to_string()), "{message}");
    }

    #[test]
    fn an_empty_directory_reports_no_server() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in [tmp.path().to_path_buf(), tmp.path().join("never-used")] {
            let err = send_inverse_search_in(&dir, &tmp.path().join("main.tex"), 1, 0)
                .expect_err("nothing is listening");
            assert!(matches!(err, IpcError::NoServer), "{err:?}");
        }
    }

    #[test]
    fn wake_unblocks_accept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        let listener = std::sync::Arc::new(listener(&dir, vec![]));
        let parked = {
            let listener = std::sync::Arc::clone(&listener);
            std::thread::spawn(move || listener.accept_one().is_none())
        };
        listener.wake();
        assert!(parked.join().unwrap(), "wake must end the accept loop");
    }

    #[test]
    fn malformed_reply_preserves_the_live_server_for_the_next_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        let listener = std::sync::Arc::new(listener(&dir, vec![]));
        let worker = {
            let listener = listener.clone();
            std::thread::spawn(move || {
                let (_, mut responder) = listener.accept_one().expect("first request");
                let mut stream = responder.stream.take().unwrap();
                stream.write_all(b"not-json\n").unwrap();
                drop(stream);
                if let Some((_, responder)) = listener.accept_one() {
                    responder.accept();
                    true
                } else {
                    false
                }
            })
        };
        let file = tmp.path().join("main.tex");
        let first = send_inverse_search_in(&dir, &file, 1, 0);
        let preserved = listener.advertisement.exists();
        let second = send_inverse_search_in(&dir, &file, 1, 0);
        // A failed pre-fix attempt removes the advertisement. Wake the server
        // before joining even when no second request could reach it.
        listener.wake();
        let recovered = worker.join().unwrap();
        assert!(
            first.is_err(),
            "malformed reply must reject the first attempt"
        );
        assert!(
            preserved,
            "a malformed reply does not make a live server stale"
        );
        assert!(
            second.is_ok(),
            "the next attempt must still discover the server"
        );
        assert!(recovered);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wake_does_not_block_when_the_unix_backlog_is_full() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = std::sync::Arc::new(listener(&tmp.path().join("ipc"), vec![]));
        let address = socket2::SockAddr::unix(&listener.address).unwrap();
        let mut queued = Vec::new();
        let mut full = false;
        for _ in 0..8192 {
            let peer =
                socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
            peer.set_nonblocking(true).unwrap();
            match peer.connect(&address) {
                Ok(()) => queued.push(peer),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    full = true;
                    break;
                }
                Err(error) => panic!("unexpected backlog fill error: {error}"),
            }
        }
        assert!(full, "fixture must saturate the Unix accept backlog");
        assert!(!queued.is_empty());
        let (done, completed) = std::sync::mpsc::channel();
        let waking = {
            let listener = listener.clone();
            std::thread::spawn(move || {
                listener.wake();
                done.send(()).unwrap();
            })
        };
        let bounded = completed.recv_timeout(Duration::from_millis(500));
        if bounded.is_err() {
            // The original blocking connect needs one vacant backlog slot.
            // Always release it before joining, even when this test fails.
            drop(listener.acceptor.accept().unwrap());
            completed.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        waking.join().unwrap();
        assert!(
            bounded.is_ok(),
            "wake must bound a saturated-backlog connect"
        );
    }

    #[test]
    fn wake_interrupts_a_peer_that_never_finishes_its_request() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = std::sync::Arc::new(listener(&tmp.path().join("ipc"), vec![]));
        let ad = Advertisement {
            pid: std::process::id(),
            transport: listener.transport,
            address: listener.address.clone(),
            token: listener.token.clone(),
            roots: vec![],
        };
        let mut peer = connect(&ad, REPLY_TIMEOUT).unwrap();
        peer.write_all(b"{\"path\":").unwrap();
        // Establish the accepted connection before waking, so this exercises
        // request reading rather than only the listening socket.
        let accepted = listener.acceptor.accept().unwrap().0;
        let (done, completed) = std::sync::mpsc::channel();
        let parked = {
            let listener = listener.clone();
            std::thread::spawn(move || {
                let request = read_request(&accepted, &listener.shutdown);
                done.send(request.is_none()).unwrap();
            })
        };
        listener.wake();
        let interrupted = completed.recv_timeout(Duration::from_secs(1));
        // Always release the peer before joining, even if the regression fails.
        drop(peer);
        parked.join().unwrap();
        assert!(
            interrupted.unwrap(),
            "wake must interrupt a partial request"
        );
    }

    #[test]
    fn oversized_ipc_frames_are_rejected_without_waiting_for_a_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = listener(&tmp.path().join("ipc"), vec![]);
        let ad = Advertisement {
            pid: std::process::id(),
            transport: listener.transport,
            address: listener.address.clone(),
            token: listener.token.clone(),
            roots: vec![],
        };
        let mut peer = connect(&ad, REPLY_TIMEOUT).unwrap();
        let accepted = listener.acceptor.accept().unwrap().0;
        let writer = std::thread::spawn(move || {
            let _ = peer.write_all(&vec![b'x'; MAX_FRAME_BYTES + 1]);
            peer
        });
        assert!(read_frame(&accepted, None).is_none());
        drop(accepted);
        drop(writer.join().unwrap());
    }

    #[test]
    fn partial_data_does_not_extend_the_ipc_frame_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = listener(&tmp.path().join("ipc"), vec![]);
        let ad = Advertisement {
            pid: std::process::id(),
            transport: listener.transport,
            address: listener.address.clone(),
            token: listener.token.clone(),
            roots: vec![],
        };
        let mut peer = connect(&ad, REPLY_TIMEOUT).unwrap();
        let accepted = listener.acceptor.accept().unwrap().0;
        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let writer = {
            let finished = finished.clone();
            std::thread::spawn(move || {
                // A per-read timeout would keep waiting for the entire six
                // seconds. The frame deadline must expire while data arrives.
                let deadline = std::time::Instant::now() + REPLY_TIMEOUT * 2;
                while std::time::Instant::now() < deadline && peer.write_all(b"x").is_ok() {
                    std::thread::sleep(READ_POLL / 2);
                }
                finished.store(true, Ordering::SeqCst);
            })
        };
        let result = read_frame(&accepted, None);
        let expired_while_sending = !finished.load(Ordering::SeqCst);
        drop(accepted);
        writer.join().unwrap();
        assert!(result.is_none());
        assert!(
            expired_while_sending,
            "partial bytes must not reset the deadline"
        );
    }

    #[test]
    fn ipc_frame_limit_accepts_unicode_at_the_exact_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = listener(&tmp.path().join("ipc"), vec![]);
        let ad = Advertisement {
            pid: std::process::id(),
            transport: listener.transport,
            address: listener.address.clone(),
            token: listener.token.clone(),
            roots: vec![],
        };
        let mut peer = connect(&ad, REPLY_TIMEOUT).unwrap();
        let accepted = listener.acceptor.accept().unwrap().0;
        let frame = format!(
            "{}💡{}\n",
            "x".repeat(1023),
            "x".repeat(MAX_FRAME_BYTES - 1028)
        );
        assert_eq!(frame.len(), MAX_FRAME_BYTES);
        let writer = {
            let frame = frame.clone();
            std::thread::spawn(move || peer.write_all(frame.as_bytes()).unwrap())
        };
        assert_eq!(read_frame(&accepted, None).as_deref(), Some(frame.as_str()));
        writer.join().unwrap();
    }

    #[test]
    fn drop_unlinks_the_advertisement_and_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        let (advertisement, address) = {
            let listener = listener(&dir, vec![]);
            (listener.advertisement.clone(), listener.address.clone())
        };
        assert!(!advertisement.exists());
        if cfg!(unix) {
            assert!(!Path::new(&address).exists());
        }
    }

    #[test]
    fn advertisement_round_trips() {
        let ad = Advertisement {
            pid: 7,
            transport: Transport::Unix,
            address: "/run/user/1000/tex-ls-1000/7.sock".to_owned(),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
            roots: vec![PathBuf::from("/home/u/paper")],
        };
        let json = serde_json::to_string(&ad).unwrap();
        assert!(json.contains("\"transport\":\"unix\""), "{json}");
        assert_eq!(
            serde_json::from_str::<Advertisement>(&json).unwrap(),
            ad,
            "the advertisement is a private protocol, but both ends must agree \
             across a rolling upgrade"
        );
    }

    #[test]
    fn tokens_differ_between_calls() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b, "a predictable token would not guard a loopback port");
    }
}
