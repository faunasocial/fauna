pub mod device;
pub mod sync;
pub mod sync_pipe_client;

// The unix-domain-socket transport (server + socket-path resolution) — the
// non-Windows sibling of the named-pipe path. The matching client lives on
// `SyncPipeClient::connect_socket` (sync_pipe_client.rs), so consumers use one
// client type on every platform.
#[cfg(unix)]
pub mod unix_transport;

// The named-pipe transport (pipe DACL + accept loop + client handler) — the
// server half of the windows path, whose client already lives in
// `sync_pipe_client`. The per-user sync agent serves through it, owner-only.
#[cfg(windows)]
pub mod pipe_transport;

// The shared "current process token's user" Win32 primitive — `sync`,
// `pipe_transport`, and `fauna-cfapi` (a cross-crate consumer) all need it;
// see the module docs for why it exists as one owner rather than three copies.
#[cfg(windows)]
pub mod win_token;

// The shared Windows SCM service-entry-point ceremony — both FaunaNest and
// FaunaBridge register a service and run an inner loop the same way; see the
// module docs for what's shared vs. left per-binary.
#[cfg(windows)]
pub mod scm_service;

// The length-prefixed dag-cbor frame loop both transports read and write with —
// and the ONE server-side enforcer of `MAX_FRAME_SIZE`.
#[cfg(any(unix, windows))]
mod frame_io;

// Connection-scoped values for those server loops: what lets the sync agent
// count an attached app for exactly as long as its connection is open.
#[cfg(any(unix, windows))]
pub mod conn_scope;

// The one platform-transport seam for the agent's local control plane: the
// per-user unix socket on linux/macOS, the per-SID named pipe on windows
// (`sync-agent.md` § Consumers). Clients hold an `AgentEndpoint` instead of a
// path, which is what lets the shared control client compile on both.
#[cfg(any(unix, windows))]
pub mod endpoint;

// The shared provisioning convergence loop every desktop control surface runs
// (linux GTK, FaunaKit, fauna-tui). Needs a tokio runtime, so it is gated to the
// platforms that have one — every real target is unix or windows.
#[cfg(any(unix, windows))]
pub mod convergence;

// The shared consumer loop for the agent's pushed events (the sibling of
// `convergence` on the read side): filter + blocking recv loop + self-healing
// listener thread. Platform shells inject only the notification surface.
pub mod events;

// The cross-process named-mutex test guard windows test suites (this crate's
// consumers) serialize on when touching shared, non-per-process OS state (a
// registry hive, a fixed-name Cloud Filter sync root). Gated like
// `endpoint`'s harness overrides: test/debug builds and forwarded
// `test-helpers`, never a plain release.
#[cfg(all(windows, any(test, debug_assertions, feature = "test-helpers")))]
pub mod test_support;

use std::io;

use serde::{Serialize, de::DeserializeOwned};

/// Encode a message as a length-prefixed canonical dag-cbor frame.
///
/// Frame: `[u32 little-endian payload length][canonical dag-cbor payload]`.
/// The payload is canonical IPLD dag-cbor (`docs/goal/architecture/serialization.md`),
/// the same encoder used everywhere else in Fauna.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
    let payload = fauna_cbor::encode_canonical(msg)?;
    let len = (payload.len() as u32).to_le_bytes();
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode a canonical dag-cbor payload (without the length prefix).
///
/// Uses strict decode (the pre-parse canonical validator runs before serde),
/// so non-canonical bytes are rejected rather than silently accepted.
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, fauna_cbor::DecodeError> {
    fauna_cbor::decode_strict(payload)
}

/// The `id` of a request or reply frame whose body this build cannot decode,
/// or `None` when the frame carries no readable `id` at all (an event frame,
/// or garbage).
///
/// Both ends of the app↔agent protocol can be different releases (an updated
/// app meeting a still-running older agent, an older shell extension meeting a
/// newer agent; `sync-agent.md` § Local agent health). A frame naming a
/// variant this build does not know still carries an `id` it can read, so the
/// one call that frame belongs to is answered — refused by the agent
/// ([`RefuseUndecodedRequest`]), failed at once by the app — and the
/// connection stays up (`transport.md` § Rule 3 in full: a request or reply
/// frame is skipped as one record).
pub(crate) fn decode_frame_id(payload: &[u8]) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct FrameId {
        id: u64,
    }
    decode_payload::<FrameId>(payload).ok().map(|f| f.id)
}

/// What a server answers a request frame whose method it cannot decode: a
/// reply to that frame's `id` refusing the one request, on a connection that
/// stays up. The server loop (`frame_io::handle_conn`) builds it from the id
/// [`decode_frame_id`] recovers.
pub trait RefuseUndecodedRequest {
    /// The refusal for request `id`.
    fn refuse_undecoded_request(id: u64) -> Self;
}

/// IPC protocol constants.
pub const PIPE_NAME: &str = r"\\.\pipe\fauna-service";
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024; // 16 MiB

/// Validate a frame's little-endian length prefix against [`MAX_FRAME_SIZE`],
/// returning the payload length to allocate. Shared by `frame_io::read_frame`
/// (async, tokio) and `sync_pipe_client::read_frame` (blocking,
/// `std::io::Read`) — the two read loops can't share code across I/O traits,
/// but the length-prefix policy the doc comment on `MAX_FRAME_SIZE` in
/// `frame_io` describes ("one reader, one owner") is a project decision, not
/// a trait difference, and belongs in exactly one place.
pub(crate) fn checked_frame_len(len_buf: [u8; 4]) -> io::Result<usize> {
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame too large: {len} bytes (max {MAX_FRAME_SIZE})"),
        ));
    }
    Ok(len as usize)
}
