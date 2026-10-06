//! Blocking named-pipe client with a reader thread that routes Response and
//! Event frames.  No tokio runtime required — suitable for use inside
//! explorer.exe (shell extension).
//!
//! Frame format (the canonical length-prefixed dag-cbor frame, same as
//! `encode_frame`/`decode_payload` in `lib.rs`):
//!   [u32 LE length][canonical dag-cbor payload]
//!
//! The reader thread reads frames continuously, decodes the first byte to
//! distinguish Response from Event (by trying to decode as Response first,
//! then Event), and routes accordingly:
//!   Response → matched to a pending request by id via a oneshot-style channel
//!   Event    → forwarded to the shared event mpsc channel
//! A frame a newer agent wrote that this build cannot decode is one record,
//! never the connection (`transport.md` § Rule 3 in full): a reply whose `id`
//! still reads fails that one pending call at once with [`ReplyNotUnderstood`],
//! and any other frame — an event of a kind this build does not name — is
//! skipped.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

#[cfg(test)]
use crate::MAX_FRAME_SIZE;
use crate::sync::{Event, Request, RequestMethod, Response};
use crate::{decode_payload, encode_frame};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Sender side of a one-shot channel used to deliver a single Response back
/// to the caller of `request()` — or, for a reply this build cannot decode,
/// the typed failure of that one call.
pub type PendingTx = mpsc::SyncSender<Result<Response, ReplyNotUnderstood>>;

/// The agent answered a request with a reply this build cannot decode — an
/// agent newer than this app or shell extension, naming a payload variant this
/// build predates. Carried inside the `io::Error` a request returns (kind
/// `InvalidData`); test with [`is_reply_not_understood`].
///
/// Distinct from every other request failure on purpose: the agent is alive and
/// answering, so a caller must never read this as "agent not running" — the
/// health reading maps it to *Restart pending* (`sync-agent.md` § Local agent
/// health).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyNotUnderstood {
    /// The request id the unreadable reply answered.
    pub id: u64,
}

impl std::fmt::Display for ReplyNotUnderstood {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the sync agent answered request {} with a reply this version cannot read \
             — the agent is newer than this app",
            self.id
        )
    }
}

impl std::error::Error for ReplyNotUnderstood {}

/// Whether `e` is a [`ReplyNotUnderstood`] — the agent answered, but in a shape
/// this build cannot read.
pub fn is_reply_not_understood(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<ReplyNotUnderstood>())
}

/// Where [`route_frame`] sent one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutedFrame {
    /// A reply, delivered to its pending call (if that call is still waiting).
    Response,
    /// A reply whose body this build cannot decode; its pending call (if still
    /// waiting) was failed with [`ReplyNotUnderstood`].
    UnreadableReply,
    /// A pushed event, forwarded to the event channel.
    Event,
}

/// Ceiling on a single `request()` round-trip.
///
/// Sits deliberately **above** the service's own per-verb ceiling (the
/// nest-backed `ListFileVersions` / `RestoreFileVersion` handlers bound
/// themselves at 4 s) so a slow-but-working call is answered by the service's
/// error reply rather than cut off here — while still guaranteeing a wedged or
/// half-dead service can never hang `explorer.exe`.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

// ---------------------------------------------------------------------------
// Frame I/O helpers (cross-platform, testable)
// ---------------------------------------------------------------------------

/// Read one length-prefixed frame from `reader`.
///
/// Returns the raw payload bytes (without the 4-byte length prefix).
/// Returns an error if the declared length exceeds `MAX_FRAME_SIZE` or if the
/// underlying read fails.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = crate::checked_frame_len(len_buf)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

/// Write a message as a length-prefixed canonical dag-cbor frame to `writer`.
pub fn write_frame(writer: &mut impl Write, msg: &impl serde::Serialize) -> io::Result<()> {
    let frame = encode_frame(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writer.write_all(&frame)
}

/// Decode `payload` and route it to the appropriate channel.
///
/// Returns where the frame went ([`RoutedFrame`]), or `Err(...)` — the caller
/// skips the frame — when it is neither a decodable reply, a reply with a
/// readable `id`, nor a decodable event: an event of a kind this build does not
/// name lands here.
///
/// The function is intentionally infallible with respect to channel send
/// errors: if the receiver has been dropped we silently discard the frame
/// rather than killing the reader thread.
pub fn route_frame(
    payload: &[u8],
    pending: &Mutex<HashMap<u64, PendingTx>>,
    event_tx: &mpsc::Sender<Event>,
) -> Result<RoutedFrame, String> {
    let deliver = |id: u64, outcome: Result<Response, ReplyNotUnderstood>| {
        let tx = pending.lock().unwrap().remove(&id);
        if let Some(tx) = tx {
            // SyncSender::send is non-blocking (bounded=1); ignore send error
            // (caller may have timed out).
            let _ = tx.send(outcome);
        }
    };
    // Try Response first.
    if let Ok(resp) = decode_payload::<Response>(payload) {
        deliver(resp.id, Ok(resp));
        return Ok(RoutedFrame::Response);
    }
    // Try Event.
    if let Ok(evt) = decode_payload::<Event>(payload) {
        let _ = event_tx.send(evt);
        return Ok(RoutedFrame::Event);
    }
    // A reply this build cannot read still names its call: fail that one call
    // now instead of leaving it to wait out `REQUEST_TIMEOUT` (6 s — on an
    // Explorer thread, in the shell extension).
    if let Some(id) = crate::decode_frame_id(payload) {
        deliver(id, Err(ReplyNotUnderstood { id }));
        return Ok(RoutedFrame::UnreadableReply);
    }
    Err(format!(
        "could not decode frame as Response or Event ({} bytes)",
        payload.len()
    ))
}

// ---------------------------------------------------------------------------
// SyncPipeClient
// ---------------------------------------------------------------------------

pub struct SyncPipeClient {
    /// Monotonically-increasing request ID.
    next_id: AtomicU64,
    /// Locked writer — request() holds this while sending the frame.
    writer: Mutex<Box<dyn Write + Send + 'static>>,
    /// Pending requests waiting for a response.
    pending: Arc<Mutex<HashMap<u64, PendingTx>>>,
    /// Receiver end of the event channel.
    event_rx: Mutex<mpsc::Receiver<Event>>,
    /// Set to false by the reader thread when the pipe is closed/broken.
    alive: Arc<AtomicBool>,
    /// Reader thread handle (kept so we can join on close).
    _reader: Option<thread::JoinHandle<()>>,
}

impl SyncPipeClient {
    /// Create a client from arbitrary `Read + Send` / `Write + Send` streams.
    ///
    /// This constructor is cross-platform and used by tests.
    pub fn from_streams(
        reader: Box<dyn Read + Send + 'static>,
        writer: Box<dyn Write + Send + 'static>,
    ) -> Self {
        let pending: Arc<Mutex<HashMap<u64, PendingTx>>> = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let alive = Arc::new(AtomicBool::new(true));

        let pending_clone = Arc::clone(&pending);
        let alive_clone = Arc::clone(&alive);

        let handle = thread::Builder::new()
            .name("fauna-ipc-reader".into())
            .spawn(move || {
                reader_loop(reader, pending_clone, event_tx, alive_clone);
            })
            .expect("failed to spawn reader thread");

        SyncPipeClient {
            next_id: AtomicU64::new(1),
            writer: Mutex::new(writer),
            pending,
            event_rx: Mutex::new(event_rx),
            alive,
            _reader: Some(handle),
        }
    }

    /// Send a request and block until the matching Response arrives, the pipe
    /// breaks, or [`REQUEST_TIMEOUT`] elapses.
    ///
    /// Callers live inside `explorer.exe` (overlay `IsMemberOf`, context-menu
    /// `GetTitle` / `EnumSubCommands` / `Invoke`), so this **must** be bounded: a
    /// service that is alive but wedged never drops its end of the channel, and an
    /// unbounded `recv()` would hang the user's right-click — and, on the overlay
    /// path, Explorer's icon pump — forever.
    pub fn request(&self, method: RequestMethod) -> io::Result<Response> {
        self.request_with_timeout(method, REQUEST_TIMEOUT)
    }

    /// [`request`](Self::request) with an explicit bound (tests use a short one).
    pub fn request_with_timeout(
        &self,
        method: RequestMethod,
        timeout: std::time::Duration,
    ) -> io::Result<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request { id, method };

        // Register pending entry BEFORE sending so the reader thread can never
        // race and deliver the response before we're listening.
        let (tx, rx) = mpsc::sync_channel::<Result<Response, ReplyNotUnderstood>>(1);
        {
            let mut map = self.pending.lock().unwrap();
            map.insert(id, tx);
        }

        // Send the frame.
        let send_result = {
            let mut w = self.writer.lock().unwrap();
            write_frame(&mut *w, &req)
        };

        if let Err(e) = send_result {
            // Remove the pending entry we just inserted.
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }

        // Block until the reader delivers a response, the pipe closes, or we hit the
        // ceiling. On timeout the pending entry must be dropped, or a slow reply
        // arriving later would leak an orphaned sender into the map.
        match rx.recv_timeout(timeout) {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(unread)) => Err(io::Error::new(io::ErrorKind::InvalidData, unread)),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending.lock().unwrap().remove(&id);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "pipe closed before response arrived",
                ))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap().remove(&id);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no response within {timeout:?}"),
                ))
            }
        }
    }

    /// Block until the next server-pushed event arrives.
    pub fn recv_event(&self) -> io::Result<Event> {
        self.event_rx
            .lock()
            .unwrap()
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "event channel closed"))
    }

    /// Returns `true` if the reader thread is still running (pipe is healthy).
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Signal the client as closed.  The underlying OS handle will be dropped
    /// when the writer lock is released; the reader thread will exit on the
    /// next read error.
    pub fn close(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

#[cfg(windows)]
impl SyncPipeClient {
    /// Open a connection to the calling user's per-session fauna-sync named pipe
    /// (`\\.\pipe\fauna-sync.<SID>`) and return a client. Resolves the current
    /// process's token user SID at call time via
    /// [`crate::sync::current_user_pipe_name`]. This is the entry point all
    /// shell-extension handlers (overlay, context-menu, event-listener) use.
    pub fn connect_pipe() -> io::Result<Self> {
        let name = crate::sync::current_user_pipe_name()?;
        Self::connect_pipe_to(&name)
    }

    /// Open a connection to the named pipe `name` and return a client.
    ///
    /// `connect_pipe` is `connect_pipe_to(current_user_pipe_name())`; the
    /// explicit-name form lets an integration test point this *same* connect path
    /// at a unique test pipe served by `pipe_server::run_pipe_server`, exercising
    /// the real framing + reader thread rather than an in-memory `from_streams`
    /// fake.
    ///
    /// The handle is opened `FILE_FLAG_OVERLAPPED` and the reader/writer halves
    /// drive each direction with its own `OVERLAPPED` + event (see [`overlapped`]).
    /// A *synchronous* duplex handle would serialise the reader thread's pending
    /// read against `request()`'s write and deadlock — caught by the
    /// `fauna-sync-agent` `pipe_transport_integration` e2e.
    pub fn connect_pipe_to(name: &str) -> io::Result<Self> {
        let expected = crate::win_token::current_user_sid_string()
            .map_err(|e| io::Error::other(format!("resolve this process's own user SID: {e:#}")))?;
        Self::connect_pipe_to_expecting(name, &expected)
    }

    /// [`connect_pipe_to`](Self::connect_pipe_to) with the server SID to demand
    /// passed in rather than read from this process's token.
    ///
    /// Deliberately **not** public: the expected SID is a security parameter, and
    /// the only value production ever demands is our own. It exists so the
    /// refusal arm is testable on a single-account box — see the
    /// `server_identity` pins below.
    pub(crate) fn connect_pipe_to_expecting(name: &str, expected_sid: &str) -> io::Result<Self> {
        let file = open_pipe_handle(name)?;
        // BEFORE the client exists, so before anything can be written to it.
        verify_pipe_server_is(&file, name, expected_sid)?;
        let pipe = std::sync::Arc::new(overlapped::SharedPipe::new(file));
        let reader = overlapped::OverlappedReader::new(pipe.clone())?;
        let writer = overlapped::OverlappedWriter::new(pipe)?;

        Ok(Self::from_streams(Box::new(reader), Box::new(writer)))
    }
}

/// Open the named pipe `name` for the client half, retrying past a transient
/// `ERROR_PIPE_BUSY`.
#[cfg(windows)]
fn open_pipe_handle(name: &str) -> io::Result<std::fs::File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, SECURITY_IDENTIFICATION};

    let retry_deadline = std::time::Instant::now() + PIPE_BUSY_RETRY_DEADLINE;
    let file = loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED.0)
            // Cap what a pipe server may do with our token at *identify*.
            // A local named-pipe client defaults to `SECURITY_IMPERSONATION`,
            // which would let whoever is serving this machine-wide name act
            // as this user against anything on the box — including in the
            // window before `verify_pipe_server_is` has refused them. This
            // is a separate builder call on purpose: std masks the SQOS bits
            // out of `custom_flags` and applies them only from here, so
            // OR-ing `SECURITY_IDENTIFICATION` into the flags above would
            // read correct and set nothing (`security_qos_flags` also ORs in
            // `SECURITY_SQOS_PRESENT` itself).
            .security_qos_flags(SECURITY_IDENTIFICATION.0)
            .open(name)
        {
            Ok(f) => break f,
            // `pipe_transport::serve`'s accept loop is sequential — it spawns
            // the just-accepted connection's handler, THEN loops back to
            // `CreateNamedPipeW` the next listening instance — so a client
            // racing that narrow window sees ERROR_PIPE_BUSY even though the
            // server has no fixed instance cap (`PIPE_UNLIMITED_INSTANCES`).
            // Every call here is `endpoint.rs`'s documented "connect per
            // exchange", so concurrent callers (a convergence tick, a
            // user-triggered PullFolderNow, an e2e agent-command poke, …)
            // racing this window is expected, not exceptional — and with no
            // retry, `open()` fails the whole request outright. Measured
            // 2026-08-27: windows' `custodian_pull_run_now` TestAgent command
            // hit this racing the app's own convergence-tick traffic.
            // `WaitNamedPipeW` is the documented Win32 idiom: it blocks until
            // an instance frees up (or the deadline), rather than a blind
            // sleep-and-retry.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                let remaining = retry_deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(e);
                }
                wait_for_free_pipe_instance(name, remaining)?;
            }
            Err(e) => return Err(e),
        }
    };
    Ok(file)
}

/// Refuse `file` unless the process serving its named pipe runs as
/// `expected_sid`.
///
/// Called on the freshly-opened handle *before* a [`SyncPipeClient`] is built
/// around it, so a refused pipe never becomes an object anything can write a
/// frame to. Both arms — identified-and-wrong, and could-not-identify — produce
/// the same [`ServerIdentityRefused`]; see
/// [`win_token::pipe_server_user_sid_string`](crate::win_token::pipe_server_user_sid_string)
/// for why a failed probe is a refusal rather than a fallback.
#[cfg(windows)]
fn verify_pipe_server_is(file: &std::fs::File, name: &str, expected_sid: &str) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;

    let refuse = |reason: String| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            ServerIdentityRefused {
                pipe: name.to_string(),
                reason,
            },
        )
    };

    let served_by = crate::win_token::pipe_server_user_sid_string(HANDLE(file.as_raw_handle()))
        .map_err(|e| refuse(format!("could not identify the serving process ({e:#})")))?;

    if served_by != expected_sid {
        return Err(refuse(format!(
            "it is served by {served_by}, not {expected_sid}"
        )));
    }
    Ok(())
}

/// A named pipe answered, but the process serving it is not the user this client
/// is willing to talk to — or could not be identified at all.
///
/// A **distinct, terminal** error on purpose. The app pushes its nest bearer
/// (`RefreshBearer`) and a `SyncCapability` carrying the owner's `BackupKey`
/// (`ProvisionCapability`)
/// down this channel, so this is a refusal to hand secrets to a stranger — not
/// the ordinary *"the agent isn't up yet"* transport failure, and never to be
/// retried as one. See [`is_server_identity_refusal`].
#[derive(Debug)]
pub struct ServerIdentityRefused {
    /// The pipe name that was refused.
    pub pipe: String,
    /// Why — a mismatch, or the probe that could not answer.
    pub reason: String,
}

impl std::fmt::Display for ServerIdentityRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to speak to {}: {} — another local account may be serving \
             this name while your sync agent is down",
            self.pipe, self.reason
        )
    }
}

impl std::error::Error for ServerIdentityRefused {}

/// Whether `e` is a [`ServerIdentityRefused`] — the predicate a caller uses to
/// tell *"do not retry, and do not report this as a missing agent"* from an
/// ordinary connect failure.
///
/// The distinction is load-bearing for the ensure-the-agent path
/// (`sync-agent.md` § *A verb with NOTHING BEHIND IT TO RETRY*): that path
/// answers a failed connect by spawning the agent and re-probing for 10 s. Run
/// against a squatted name, the real agent's `FILE_FLAG_FIRST_PIPE_INSTANCE`
/// create fails every time, so the loop would burn its whole budget on
/// spawn → die → re-probe and then report *"agent unreachable"* — the one
/// diagnosis that is certainly wrong.
///
/// Always `false` off windows, where the transport is a `0700`-dir unix socket
/// whose parent is already per-user.
pub fn is_server_identity_refusal(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<ServerIdentityRefused>())
}

/// Win32 `ERROR_PIPE_BUSY` — every instance of the named pipe is currently
/// connected; distinct from `ERROR_FILE_NOT_FOUND` (the server hasn't created
/// the pipe at all yet, which this retry does NOT cover — that is a caller-level
/// "is the agent even up" concern, e.g. `ipc::wait_for_pipe` in the e2e harness).
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

/// Total budget a single [`SyncPipeClient::connect_pipe_to`] call spends
/// retrying past `ERROR_PIPE_BUSY` before giving up — well under
/// `sync_pipe_client::REQUEST_TIMEOUT`'s 6s so a genuinely wedged server still
/// fails a caller promptly rather than swallowing its whole budget here.
#[cfg(windows)]
const PIPE_BUSY_RETRY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Block (up to `timeout`) until an instance of the named pipe `name` is free
/// to connect, via the Win32 `WaitNamedPipeW` API — the documented idiom for
/// `ERROR_PIPE_BUSY`, and cheaper/more responsive than a blind sleep loop: it
/// returns the instant an instance frees up rather than on the next poll tick.
#[cfg(windows)]
fn wait_for_free_pipe_instance(name: &str, timeout: std::time::Duration) -> io::Result<()> {
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    let ok = unsafe { WaitNamedPipeW(windows::core::PCWSTR(wide.as_ptr()), timeout_ms) };
    if ok.as_bool() {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
impl SyncPipeClient {
    /// Connect to the per-user sync-agent unix socket at `path` and return a
    /// client — the unix sibling of [`connect_pipe_to`](Self::connect_pipe_to).
    ///
    /// A `UnixStream` is full-duplex, so unlike the Windows named pipe (which
    /// needs the `OVERLAPPED` dance to let a blocked reader and a concurrent
    /// writer coexist) we simply `try_clone` the stream: one handle is moved to
    /// the reader thread, the other stays for `request()`'s writes. Both impl
    /// blocking `Read`/`Write`, so the rest of `SyncPipeClient` (reader thread,
    /// `request()`, `recv_event()`) is unchanged and **no tokio runtime is
    /// required** — a GTK main loop or fauna-tui can call this directly.
    pub fn connect_socket(path: &std::path::Path) -> io::Result<Self> {
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(path)?;
        let reader = stream.try_clone()?;
        Ok(Self::from_streams(Box::new(reader), Box::new(stream)))
    }
}

/// Overlapped (async) pipe I/O presented as blocking `Read`/`Write`.
///
/// The shell-extension request clients keep a reader thread blocked in a read
/// while `request()` writes on the same connection. On a *synchronous* pipe
/// handle the OS serialises I/O per file object (`FO_SYNCHRONOUS_IO`), so the
/// pending read blocks the write → deadlock (a race the client usually but not
/// always wins; reproduced by the `pipe_transport_integration` e2e). Opening the
/// handle `FILE_FLAG_OVERLAPPED` and giving each direction its own `OVERLAPPED` +
/// event lets read and write proceed concurrently. We still expose *blocking*
/// `Read`/`Write` — each op issues the overlapped call then blocks on its own
/// event via `GetOverlappedResult(bWait=true)` — so the rest of `SyncPipeClient`
/// (the reader thread, `request()`, `recv_event()`) is unchanged.
#[cfg(windows)]
mod overlapped {
    use std::io::{self, Read, Write};
    use std::os::windows::io::AsRawHandle;
    use std::sync::Arc;

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_PENDING, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Threading::{CreateEventW, ResetEvent};
    use windows::core::{HRESULT, PCWSTR};

    fn to_io(e: windows::core::Error) -> io::Error {
        io::Error::other(e.to_string())
    }

    fn is_eof(hr: HRESULT) -> bool {
        hr == HRESULT::from_win32(ERROR_BROKEN_PIPE.0)
            || hr == HRESULT::from_win32(ERROR_HANDLE_EOF.0)
    }

    /// Owns the overlapped pipe file handle; closed when the last half (reader or
    /// writer `Arc` clone) drops. The handle is used only via overlapped ops, never
    /// `std::fs::File`'s own (synchronous) `Read`/`Write`.
    pub struct SharedPipe {
        file: std::fs::File,
    }

    impl SharedPipe {
        pub fn new(file: std::fs::File) -> Self {
            Self { file }
        }
        fn handle(&self) -> HANDLE {
            HANDLE(self.file.as_raw_handle())
        }
    }

    /// A per-direction manual-reset completion event, reset before each op and
    /// closed on drop.
    struct Event(HANDLE);

    impl Event {
        fn new() -> io::Result<Self> {
            let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(to_io)?;
            Ok(Event(h))
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    // The event handle is used only by its single owning half (reader xor writer),
    // each with its own `OVERLAPPED`, so moving the half to the reader thread is
    // sound. (The pipe handle is owned by `SharedPipe`'s `File`, already `Send`.)
    unsafe impl Send for Event {}

    /// After issuing an overlapped read/write, block on `ev` until it completes.
    /// `Ok(0)` on EOF / broken pipe so `read_exact` surfaces `UnexpectedEof`.
    unsafe fn finish(
        handle: HANDLE,
        ov: &OVERLAPPED,
        started: windows::core::Result<()>,
    ) -> io::Result<usize> {
        match started {
            Ok(()) => {} // completed synchronously — query the byte count below
            Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {}
            Err(e) if is_eof(e.code()) => return Ok(0),
            Err(e) => return Err(to_io(e)),
        }
        let mut transferred = 0u32;
        match unsafe { GetOverlappedResult(handle, ov, &mut transferred, true) } {
            Ok(()) => Ok(transferred as usize),
            Err(e) if is_eof(e.code()) => Ok(0),
            Err(e) => Err(to_io(e)),
        }
    }

    pub struct OverlappedReader {
        pipe: Arc<SharedPipe>,
        event: Event,
    }

    impl OverlappedReader {
        pub fn new(pipe: Arc<SharedPipe>) -> io::Result<Self> {
            Ok(Self {
                pipe,
                event: Event::new()?,
            })
        }
    }

    impl Read for OverlappedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            unsafe {
                ResetEvent(self.event.0).map_err(to_io)?;
                let mut ov = OVERLAPPED {
                    hEvent: self.event.0,
                    ..Default::default()
                };
                let handle = self.pipe.handle();
                let started = ReadFile(handle, Some(buf), None, Some(&mut ov));
                finish(handle, &ov, started)
            }
        }
    }

    pub struct OverlappedWriter {
        pipe: Arc<SharedPipe>,
        event: Event,
    }

    impl OverlappedWriter {
        pub fn new(pipe: Arc<SharedPipe>) -> io::Result<Self> {
            Ok(Self {
                pipe,
                event: Event::new()?,
            })
        }
    }

    impl Write for OverlappedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            unsafe {
                ResetEvent(self.event.0).map_err(to_io)?;
                let mut ov = OVERLAPPED {
                    hEvent: self.event.0,
                    ..Default::default()
                };
                let handle = self.pipe.handle();
                let started = WriteFile(handle, Some(buf), None, Some(&mut ov));
                finish(handle, &ov, started)
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Reader loop (runs on its own thread)
// ---------------------------------------------------------------------------

fn reader_loop(
    mut reader: Box<dyn Read + Send + 'static>,
    pending: Arc<Mutex<HashMap<u64, PendingTx>>>,
    event_tx: mpsc::Sender<Event>,
    alive: Arc<AtomicBool>,
) {
    loop {
        let payload = match read_frame(&mut reader) {
            Ok(p) => p,
            Err(_) => {
                // Pipe broken or EOF — wake all pending callers so they
                // unblock with an error.
                alive.store(false, Ordering::Relaxed);
                let mut map = pending.lock().unwrap();
                map.clear(); // drops all PendingTx senders → recv() returns Err
                return;
            }
        };

        // Errors from route_frame are non-fatal; skip the frame — an event of
        // a kind this build does not name is one record, never the connection.
        if let Err(e) = route_frame(&payload, &pending, &event_tx) {
            tracing::debug!("skipping an IPC frame: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{EventKind, FileStatus, ResponsePayload, ResponseResult};
    use std::io::Cursor;

    // Helper: encode a frame into a Vec<u8> using write_frame.
    fn encode<T: serde::Serialize>(msg: &T) -> Vec<u8> {
        let mut buf = Vec::new();
        write_frame(&mut buf, msg).unwrap();
        buf
    }

    /// A reader that never yields a byte and never hits EOF — a service that is
    /// alive (its end of the pipe stays open) but never answers.
    struct SilentReader {
        _keep_open: mpsc::Sender<()>,
        block_on: mpsc::Receiver<()>,
    }

    impl Read for SilentReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            // Blocks until `_keep_open` is dropped, i.e. until this struct dies.
            let _ = self.block_on.recv();
            Ok(0)
        }
    }

    /// A wedged-but-alive service must not hang the caller. `rx.recv()` only wakes
    /// when the *sender* is dropped (pipe closed), so an alive-and-silent service
    /// used to block `explorer.exe`'s right-click forever.
    #[test]
    fn request_times_out_against_an_alive_but_silent_service() {
        let (keep_open, block_on) = mpsc::channel();
        let reader: Box<dyn Read + Send + 'static> = Box::new(SilentReader {
            _keep_open: keep_open,
            block_on,
        });
        let writer: Box<dyn Write + Send + 'static> = Box::new(Vec::new());
        let client = SyncPipeClient::from_streams(reader, writer);

        let started = std::time::Instant::now();
        let err = client
            .request_with_timeout(
                RequestMethod::GetSyncStatus,
                std::time::Duration::from_millis(80),
            )
            .expect_err("a silent service must not return a response");

        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "request must give up promptly, took {:?}",
            started.elapsed()
        );
        // The timed-out request must not leak its pending entry.
        assert!(client.pending.lock().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // 1. route_frame_routes_response_to_pending
    // -----------------------------------------------------------------------
    #[test]
    fn route_frame_routes_response_to_pending() {
        let pending: Mutex<HashMap<u64, PendingTx>> = Mutex::new(HashMap::new());
        let (event_tx, _event_rx) = mpsc::channel::<Event>();

        // Register a pending request for id=42.
        let (tx, rx) = mpsc::sync_channel(1);
        pending.lock().unwrap().insert(42, tx);

        // Encode a Response with id=42.
        let resp = Response {
            id: 42,
            result: ResponseResult::Ok(ResponsePayload::Empty),
        };
        let payload = encode(&resp);
        // payload includes the 4-byte length prefix; read_frame strips it, so
        // route_frame receives only the dag-cbor body.
        let body = payload[4..].to_vec();

        let result = route_frame(&body, &pending, &event_tx).unwrap();
        assert_eq!(result, RoutedFrame::Response);

        let received = rx
            .recv()
            .expect("response should arrive")
            .expect("a readable reply");
        assert_eq!(received.id, 42);
        // Pending map should be empty now.
        assert!(pending.lock().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // 2. route_frame_routes_event_to_channel
    // -----------------------------------------------------------------------
    #[test]
    fn route_frame_routes_event_to_channel() {
        let pending: Mutex<HashMap<u64, PendingTx>> = Mutex::new(HashMap::new());
        let (event_tx, event_rx) = mpsc::channel::<Event>();

        let evt = Event {
            event: EventKind::FileStatusChanged {
                path: r"C:\Users\alice\Fauna\doc.txt".into(),
                status: FileStatus::Synced,
            },
        };
        let payload = encode(&evt);
        let body = payload[4..].to_vec();

        let result = route_frame(&body, &pending, &event_tx).unwrap();
        assert_eq!(result, RoutedFrame::Event);

        let received = event_rx.recv().expect("event should arrive");
        match received.event {
            EventKind::FileStatusChanged { path, status } => {
                assert_eq!(path, r"C:\Users\alice\Fauna\doc.txt");
                assert_eq!(status, FileStatus::Synced);
            }
            _ => panic!("wrong event kind"),
        }
    }

    // -----------------------------------------------------------------------
    // 3. read_frame_rejects_oversized
    // -----------------------------------------------------------------------
    #[test]
    fn read_frame_rejects_oversized() {
        // Write a length prefix that is MAX_FRAME_SIZE + 1.
        let oversized = MAX_FRAME_SIZE + 1;
        let mut data = Vec::new();
        data.extend_from_slice(&oversized.to_le_bytes());
        // No actual payload bytes — read_frame should reject before reading body.

        let mut cursor = Cursor::new(data);
        let err = read_frame(&mut cursor).expect_err("should reject oversized frame");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("frame too large"));
    }

    // -----------------------------------------------------------------------
    // 4. write_and_read_frame_roundtrip
    // -----------------------------------------------------------------------
    #[test]
    fn write_and_read_frame_roundtrip() {
        let req = Request {
            id: 7,
            method: RequestMethod::GetSyncStatus,
        };

        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();

        let mut cursor = Cursor::new(buf);
        let payload = read_frame(&mut cursor).unwrap();

        let decoded: Request = decode_payload(&payload).unwrap();
        assert_eq!(decoded.id, 7);
        assert!(matches!(decoded.method, RequestMethod::GetSyncStatus));
    }

    // -----------------------------------------------------------------------
    // 5. from_streams_routes_response
    // -----------------------------------------------------------------------
    /// A `Read` the test feeds frame-by-frame over a channel. `read` blocks until
    /// the next chunk is pushed and returns EOF once the sender drops — so the
    /// reader thread cannot observe a frame before the test has registered the
    /// matching pending entry. This removes the race a pre-loaded `Cursor` had
    /// (reader thread routing the Response before `pending` held id=99, dropping
    /// it, then the EOF-clear disconnecting the caller), which false-failed under
    /// dev-machine load with a `Disconnected` error rather than a real timeout.
    struct FedReader {
        rx: mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
        pos: usize,
    }

    impl Read for FedReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            while self.pos >= self.buf.len() {
                match self.rx.recv() {
                    Ok(chunk) => {
                        self.buf = chunk;
                        self.pos = 0;
                    }
                    Err(_) => return Ok(0), // sender dropped → EOF
                }
            }
            let n = std::cmp::min(out.len(), self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn from_streams_routes_response() {
        let resp = Response {
            id: 99,
            result: ResponseResult::Ok(ResponsePayload::Empty),
        };
        let evt = Event {
            event: EventKind::FileStatusChanged {
                path: r"C:\tmp\x.txt".into(),
                status: FileStatus::Syncing,
            },
        };
        let mut resp_frame = Vec::new();
        write_frame(&mut resp_frame, &resp).unwrap();
        let mut evt_frame = Vec::new();
        write_frame(&mut evt_frame, &evt).unwrap();

        // The reader is fed over a channel, so it cannot process the Response frame
        // before we register the pending entry for id=99 below (happens-before).
        let (feed_tx, feed_rx) = mpsc::channel::<Vec<u8>>();
        let reader: Box<dyn Read + Send + 'static> = Box::new(FedReader {
            rx: feed_rx,
            buf: Vec::new(),
            pos: 0,
        });
        let writer: Box<dyn Write + Send + 'static> = Box::new(Vec::new());
        let client = SyncPipeClient::from_streams(reader, writer);

        // Register the pending request BEFORE any byte is readable.
        let (tx, rx) = mpsc::sync_channel(1);
        client.pending.lock().unwrap().insert(99, tx);

        // Now release the two frames, then EOF.
        feed_tx.send(resp_frame).unwrap();
        feed_tx.send(evt_frame).unwrap();
        drop(feed_tx);

        // The response routes to our pending entry (10 s is a generous safety
        // bound — with the race gone it resolves in microseconds).
        let received_resp = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("response should route to the pending entry")
            .expect("a readable reply");
        assert_eq!(received_resp.id, 99);

        // ...and the event routes to the event channel.
        let received_evt = client.recv_event().expect("event should arrive");
        match received_evt.event {
            EventKind::FileStatusChanged { path, status } => {
                assert_eq!(path, r"C:\tmp\x.txt");
                assert_eq!(status, FileStatus::Syncing);
            }
            _ => panic!("wrong event kind"),
        }
    }

    // ── A newer agent's frames (transport.md § Rule 3 in full) ──
    //
    // The newer writer is modelled as test-only twins of the reply and event
    // envelopes carrying one variant this build does not name.

    #[derive(serde::Serialize)]
    enum NewerPayload {
        AddedInANewerAgent { n: u32 },
    }
    #[derive(serde::Serialize)]
    enum NewerResult {
        Ok(NewerPayload),
    }
    #[derive(serde::Serialize)]
    struct NewerResponse {
        id: u64,
        result: NewerResult,
    }
    #[derive(serde::Serialize)]
    enum NewerEventKind {
        AddedInANewerAgent { n: u32 },
    }
    #[derive(serde::Serialize)]
    struct NewerEvent {
        event: NewerEventKind,
    }

    /// A client over a fed reader, plus the feed.
    fn fed_client() -> (std::sync::Arc<SyncPipeClient>, mpsc::Sender<Vec<u8>>) {
        let (feed_tx, feed_rx) = mpsc::channel::<Vec<u8>>();
        let reader: Box<dyn Read + Send + 'static> = Box::new(FedReader {
            rx: feed_rx,
            buf: Vec::new(),
            pos: 0,
        });
        let writer: Box<dyn Write + Send + 'static> = Box::new(Vec::new());
        (
            std::sync::Arc::new(SyncPipeClient::from_streams(reader, writer)),
            feed_tx,
        )
    }

    /// Wait (bounded) until the client has a call pending, so a fed reply
    /// cannot arrive before anyone listens for it.
    fn wait_for_pending(client: &SyncPipeClient) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while client.pending.lock().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline, "no call ever pending");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// tier_1: a reply naming a payload variant this build does not know fails
    /// its ONE call at once, typed — never a wait for `REQUEST_TIMEOUT`, never
    /// "agent unreachable" — and the connection keeps serving the next call.
    #[test]
    fn an_unreadable_reply_fails_its_call_at_once_typed_and_the_connection_survives() {
        let (client, feed) = fed_client();

        let caller = std::sync::Arc::clone(&client);
        let call = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = caller.request_with_timeout(
                RequestMethod::GetSyncStatus,
                std::time::Duration::from_secs(30),
            );
            (result, started.elapsed())
        });
        wait_for_pending(&client);
        feed.send(encode(&NewerResponse {
            id: 1,
            result: NewerResult::Ok(NewerPayload::AddedInANewerAgent { n: 7 }),
        }))
        .unwrap();

        let (result, elapsed) = call.join().unwrap();
        let err = result.expect_err("an unreadable reply is not a Response");
        assert!(
            is_reply_not_understood(&err),
            "the failure is typed as an unreadable reply, got: {err}"
        );
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "failed at once, not on the timeout ({elapsed:?})"
        );
        assert!(client.is_alive(), "the connection survives the frame");

        // The next call on the same connection is answered normally.
        let caller = std::sync::Arc::clone(&client);
        let call = std::thread::spawn(move || {
            caller.request_with_timeout(RequestMethod::Pause, std::time::Duration::from_secs(30))
        });
        wait_for_pending(&client);
        feed.send(encode(&Response {
            id: 2,
            result: ResponseResult::Ok(ResponsePayload::Empty),
        }))
        .unwrap();
        let resp = call.join().unwrap().expect("the next reply is read");
        assert_eq!(resp.id, 2);
    }

    /// tier_1: an event of a kind this build does not name is skipped — the
    /// reader thread keeps going and the next event arrives.
    #[test]
    fn an_event_of_an_unknown_kind_is_skipped_with_the_connection_intact() {
        let (client, feed) = fed_client();
        feed.send(encode(&NewerEvent {
            event: NewerEventKind::AddedInANewerAgent { n: 1 },
        }))
        .unwrap();
        feed.send(encode(&Event {
            event: EventKind::FileStatusChanged {
                path: "after.txt".into(),
                status: FileStatus::Synced,
            },
        }))
        .unwrap();

        let evt = client
            .recv_event()
            .expect("the known event after it arrives");
        match evt.event {
            EventKind::FileStatusChanged { path, .. } => assert_eq!(path, "after.txt"),
            other => panic!("wrong event: {other:?}"),
        }
        assert!(
            client.is_alive(),
            "the connection survives the unknown event"
        );
    }

    /// tier_1: an unknown event routes nowhere and names no call.
    #[test]
    fn route_frame_skips_an_unknown_event_without_touching_a_pending_call() {
        let pending: Mutex<HashMap<u64, PendingTx>> = Mutex::new(HashMap::new());
        let (tx, _rx) = mpsc::sync_channel(1);
        pending.lock().unwrap().insert(1, tx);
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let body = encode(&NewerEvent {
            event: NewerEventKind::AddedInANewerAgent { n: 1 },
        })[4..]
            .to_vec();
        assert!(route_frame(&body, &pending, &event_tx).is_err());
        assert!(event_rx.try_recv().is_err(), "nothing forwarded");
        assert_eq!(pending.lock().unwrap().len(), 1, "no call failed");
    }
}

// ---------------------------------------------------------------------------
// Server-identity refusal pins (windows)
// ---------------------------------------------------------------------------

/// The `\\.\pipe\` namespace is **machine-wide**, so "the pipe with the right
/// name answered" is not evidence that the *sync agent* answered: any local
/// account can pre-create `\\.\pipe\fauna-sync.<victim SID>` while the victim's
/// agent is down. These pin the refusal from the client's side.
///
/// The cross-account case itself needs a second local account (admin-created),
/// which a headless run does not have — so the probe is structured the way the
/// production code actually decides: [`SyncPipeClient::connect_pipe_to`] resolves
/// **this** process's SID and hands it to
/// [`connect_pipe_to_expecting`](SyncPipeClient::connect_pipe_to_expecting), and
/// these drive that same seam with an *expected* SID of the test's choosing.
/// Refusing a pipe served by `S-1-5-18` when we expect our own SID exercises the
/// identical Win32 probe, comparison and refusal path a real squatter would hit;
/// only the value being compared against is injected.
#[cfg(all(test, windows))]
mod server_identity {
    use super::*;
    use std::io;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Pipes::CreateNamedPipeW;
    use windows::core::PCWSTR;

    /// A SID this test process is certainly not running as — `S-1-5-18` is
    /// `NT AUTHORITY\SYSTEM`, and the agent is ratified as never running as
    /// LocalSystem (`sync-agent.md` § Packaging + lifecycle).
    const FOREIGN_SID: &str = "S-1-5-18";

    /// A raw listening pipe instance owned by this test process — so its server
    /// SID *is* this user's, and the only thing distinguishing the accept arm
    /// from the refuse arm is the expected SID the client is given.
    struct TestPipe {
        handle: HANDLE,
        name: String,
    }

    impl TestPipe {
        fn create() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            use windows::Win32::Storage::FileSystem::{
                FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX,
            };
            use windows::Win32::System::Pipes::{
                PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
            };

            static N: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                r"\\.\pipe\fauna-sync-identity-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            );
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0),
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE,
                    PIPE_UNLIMITED_INSTANCES,
                    4096,
                    4096,
                    0,
                    None,
                )
            };
            assert!(
                !handle.is_invalid(),
                "create test pipe {name}: {:?}",
                io::Error::last_os_error()
            );
            Self { handle, name }
        }
    }

    impl Drop for TestPipe {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    /// **The defect this row closes.** A pipe served by a process that is not the
    /// expected user must be refused *before* the client exists at all — the app
    /// pushes its nest bearer (`RefreshBearer`) and a `SyncCapability` carrying
    /// **the owner's `BackupKey`**
    /// (`sync.rs` `ProvisionCapability`) down this channel, so a connected-then-
    /// checked client is already a client something could write to.
    ///
    /// Mutation-check: delete the verify call in `connect_pipe_to_expecting` and
    /// exactly this test reddens.
    #[test]
    fn a_pipe_whose_server_is_not_the_expected_user_is_refused() {
        let pipe = TestPipe::create();

        let err = match SyncPipeClient::connect_pipe_to_expecting(&pipe.name, FOREIGN_SID) {
            Ok(_) => panic!(
                "connected to {} while expecting server SID {FOREIGN_SID} — the client \
                 handed itself to whoever answered the name",
                pipe.name
            ),
            Err(e) => e,
        };

        assert!(
            is_server_identity_refusal(&err),
            "refusal must be the distinct server-identity error, not a generic \
             transport failure (the ensure-the-agent path retries those): {err}"
        );
    }

    /// The accept arm, on the same seam: the real agent serves from the user's
    /// own logon session, so the expected SID matches and the connect proceeds.
    /// Without this, "refuse everything" would pass the test above.
    #[test]
    fn a_pipe_served_by_this_user_is_accepted() {
        let pipe = TestPipe::create();
        let own = crate::win_token::current_user_sid_string().expect("own SID");

        SyncPipeClient::connect_pipe_to_expecting(&pipe.name, &own)
            .expect("a pipe served by this very process must be accepted");
    }

    /// Fail **closed**, not open: a squatter controls whether the identity probe
    /// can answer at all (a cross-user `OpenProcess` is itself `ACCESS_DENIED`),
    /// so "could not determine" must refuse exactly like "determined, and it is
    /// someone else". Driven through a handle that is not a named pipe, which is
    /// how the probe fails when it cannot answer.
    #[test]
    fn a_server_whose_identity_cannot_be_determined_is_refused() {
        let not_a_pipe =
            std::fs::File::open(std::env::current_exe().expect("exe path")).expect("open own exe");

        let err = crate::win_token::pipe_server_user_sid_string(handle_of(&not_a_pipe))
            .expect_err("a handle that is not a named pipe cannot yield a server SID");

        assert!(
            !format!("{err:#}").is_empty(),
            "the probe must report why it could not identify the server"
        );
    }

    /// The refusal above is the client's half; this is the other half of the
    /// same threat. A named-pipe server may **impersonate** its client, and a
    /// local client's default is `SECURITY_IMPERSONATION` — which would let a
    /// squatter that we connect to and then refuse act *as the user* for the
    /// instant the handle was open, against anything on the box that trusts the
    /// user's token. `SECURITY_IDENTIFICATION` lets the server learn who we are
    /// and nothing more.
    ///
    /// Asserted as the level the server actually observes, not as flag bits on
    /// the way in: std masks SQOS out of `custom_flags` and applies it only
    /// through `security_qos_flags`, so a plausible-looking
    /// `custom_flags(FILE_FLAG_OVERLAPPED | SECURITY_IDENTIFICATION)` would set
    /// nothing at all and still read correct.
    ///
    /// Mutation-check: drop the `.security_qos_flags(...)` line and this reddens
    /// with `SecurityImpersonation`.
    #[test]
    fn the_client_lets_the_server_identify_it_but_never_impersonate_it() {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::Security::{
            GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL,
            SecurityIdentification, TOKEN_QUERY, TokenImpersonationLevel,
        };
        use windows::Win32::System::Pipes::ImpersonateNamedPipeClient;
        use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

        use windows::Win32::Storage::FileSystem::ReadFile;

        let pipe = TestPipe::create();
        // The production open path, so the assertion is about what ships.
        let client = open_pipe_handle(&pipe.name).expect("open the test pipe");

        // A server may not impersonate until it has read from the pipe
        // (`ERROR_CANNOT_IMPERSONATE`), so play out the one exchange that makes
        // the question real: the client sends, as it does with its first request
        // frame, and the server reads before reaching for the identity.
        let shared = std::sync::Arc::new(overlapped::SharedPipe::new(client));
        let mut writer = overlapped::OverlappedWriter::new(shared).expect("build client writer");
        writer
            .write_all(b"x")
            .expect("client writes its first bytes");

        let mut byte = [0u8; 1];
        let mut got = 0u32;
        unsafe { ReadFile(pipe.handle, Some(&mut byte), Some(&mut got), None) }
            .expect("server reads the client's bytes");

        let level = unsafe {
            ImpersonateNamedPipeClient(pipe.handle).expect("server impersonates its client");

            let mut token = HANDLE::default();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token);
            let mut level = SECURITY_IMPERSONATION_LEVEL(0);
            let mut needed = 0u32;
            let read = opened.and_then(|()| {
                GetTokenInformation(
                    token,
                    TokenImpersonationLevel,
                    Some(&mut level as *mut _ as *mut _),
                    size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                    &mut needed,
                )
            });
            if !token.is_invalid() {
                let _ = CloseHandle(token);
            }
            // Revert BEFORE asserting: a panic must not leave this test thread
            // running on a borrowed identity.
            RevertToSelf().expect("revert to self");
            read.expect("read the impersonation token's level");
            level
        };

        assert_eq!(
            level, SecurityIdentification,
            "the server got impersonation level {level:?}; a pipe server must be \
             able to identify this client and nothing more"
        );
    }

    /// The real cross-account case, with **no second account and no privilege**.
    ///
    /// The tempting claim about this finding is that probing it needs an
    /// admin-created local account, and that claim is false: Windows already
    /// runs pipe servers under a *different* account that any ordinary user may
    /// open — the RPC endpoints served by `NT AUTHORITY\SYSTEM`. Pointing the
    /// production connect path at one is exactly the squatter's shape (a pipe
    /// answering under an account that is not ours), so the arm that matters
    /// most is pinned here rather than deferred to a human with an admin
    /// console. What a hand-made second account would add is only variety in
    /// *which* foreign account, not a different code path.
    ///
    /// Passes on either arm, because both are the same refusal and which one
    /// fires is Windows' choice, not ours: `OpenProcess` may answer
    /// (→ SID mismatch) or deny (→ could not identify). The assertion is that
    /// **no** foreign-account pipe is ever accepted.
    #[test]
    fn a_pipe_served_by_another_account_is_refused_with_no_privilege_needed() {
        // Several candidates so a future Windows retiring one does not silently
        // hollow this out; a run that can open none of them FAILS rather than
        // passing vacuously (`e2e-conventions.md` convention 7 — a skip is not
        // coverage). The list is `test_support`'s.
        const SYSTEM_SERVED: &[&str] = crate::test_support::SYSTEM_SERVED_PIPES;
        let own = crate::win_token::current_user_sid_string().expect("own SID");

        let mut reached = Vec::new();
        for leaf in SYSTEM_SERVED {
            let name = format!(r"\\.\pipe\{leaf}");
            // Only a pipe we can actually open tests anything; an unopenable one
            // is refused by Windows before our check is reached.
            if open_pipe_handle(&name).is_err() {
                continue;
            }
            match SyncPipeClient::connect_pipe_to_expecting(&name, &own) {
                Ok(_) => panic!(
                    "connected to {name}, served by another account, while expecting \
                     our own SID {own} — this is the finding, reproduced"
                ),
                Err(e) => {
                    assert!(
                        is_server_identity_refusal(&e),
                        "{name} must be refused as a server-identity failure, not as \
                         a generic transport error (which the ensure-the-agent path \
                         would retry): {e}"
                    );
                    reached.push(format!("{leaf}: {e}"));
                }
            }
        }

        assert!(
            !reached.is_empty(),
            "no SYSTEM-served pipe out of {SYSTEM_SERVED:?} could be opened, so this \
             test proved nothing — find a foreign-account pipe this user can open \
             rather than letting the cross-account arm go unpinned"
        );
        // Surfaced on `--nocapture` so a future reader can see WHICH arm real
        // cross-account traffic takes on their Windows build.
        println!("refused {} foreign-account pipe(s):", reached.len());
        for r in &reached {
            println!("  {r}");
        }
    }

    fn handle_of(f: &std::fs::File) -> HANDLE {
        use std::os::windows::io::AsRawHandle;
        HANDLE(f.as_raw_handle())
    }
}
