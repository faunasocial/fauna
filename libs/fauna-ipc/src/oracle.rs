//! The **conversations oracle's door** — the second `fauna-ipc` endpoint
//! (`docs/goal/behavior/conversations-oracle.md` § The custodian and the door).
//!
//! The agent's seam with the sides mirrored: the signed-in first-party app
//! hosting the account's conversations engine **serves**, and a device
//! principal (the cli's device shell) **connects**. Same length-prefixed
//! canonical dag-cbor frame as the agent's ([`crate::encode_frame`],
//! [`crate::MAX_FRAME_SIZE`]), same per-user isolation — a `0600` socket in
//! the agent socket's `0700` per-user dir on unix, a per-SID named pipe on
//! windows — and an endpoint name carrying a 16-hex-character fingerprint of
//! the account's actor id, so each account's door is its own.
//!
//! What lives here is the door's **vocabulary and transport**, no policy and no
//! cryptography: the handshake frames, the two classes' requests and replies,
//! the typed refusals, the endpoint resolver, the endpoint's lock
//! ([`EndpointLock`]), the bound listener ([`OracleListener`]) and the
//! server's per-connection loop, and a blocking client — on unix and windows
//! alike. On windows the client refuses a pipe this user does not serve
//! before its first frame, as the sync-pipe client does: the pipe namespace
//! is machine-wide. The custodian (the shared crate every desktop
//! app and tui links) supplies the possession proof's seal and the
//! per-operation authorize through [`OracleDoor`]; the principal supplies the
//! proof's open to [`OracleClient::connect`].
//!
//! **Architectural rule 1 of the owner doc, held by the vocabulary:** no frame
//! here carries MLS material of any kind. A request carries a room id, a
//! position, a query or a body; a reply carries the door's projection records
//! ([`OracleRoom`], [`OracleMessage`]) — text and display facts the page itself
//! renders — never an envelope, a key, a wrap or group state.
//!
//! **The projection is the door's own record, not the manager's render model.**
//! The door is a wire between two independently released programs, so it owes
//! the additive compatibility every wire does; the manager's `ThreadDetail` is
//! an internal render model that changes with every UI slice. The custodian
//! builds these records from the manager's projection, so what crosses is what
//! the page shows, and the render model stays free to move.

use std::io;
#[cfg(unix)]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The door protocol's version, carried in [`Hello`]. A custodian refuses a
/// version it does not speak with [`OracleRefusal::Unsupported`].
pub const ORACLE_PROTOCOL_VERSION: u32 = 1;

/// Messages per `conversation.read` page — one page is one operation against
/// the ceiling, so a room's recent history is a handful of operations and a
/// principal paging a whole community log meets the ceiling (the owner doc's
/// *The ceiling and the page*).
pub const ORACLE_PAGE_SIZE: u32 = 50;

/// The longest body `conversation.send` carries, in bytes.
pub const ORACLE_SEND_BODY_MAX: usize = 64 * 1024;

/// Results one search operation answers at most.
pub const ORACLE_SEARCH_LIMIT: u32 = 25;

/// Length of the nonce the possession proof seals to the principal's holder
/// key.
pub const ORACLE_NONCE_LEN: usize = 32;

/// The account fingerprint an endpoint name carries: the first 8 bytes of the
/// actor id as 16 lowercase hex characters. The actor id is already a digest
/// of the account's key, so its prefix is a fingerprint; the full id would
/// overrun macOS's 104-byte `sun_path`, and the handshake carries it whole.
#[must_use]
pub fn account_fingerprint(actor_id: &[u8; 32]) -> String {
    hex::encode(&actor_id[..8])
}

/// Where an account's oracle door listens on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleEndpoint {
    /// `<agent socket dir>/oracle-<fingerprint>.sock` — the agent socket's
    /// `0700` per-user dir, so every platform rule the agent's path obeys
    /// (Flatpak's shared app subdir, macOS's Application Support, the absolute
    /// home floor) holds for the door too.
    #[cfg(unix)]
    Unix(PathBuf),
    /// `\\.\pipe\fauna-oracle.<SID>.<fingerprint>`.
    #[cfg(windows)]
    Pipe(String),
}

impl OracleEndpoint {
    /// The door of the account `actor_id` for this OS user.
    pub fn for_account(actor_id: &[u8; 32]) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let agent = crate::unix_transport::default_socket_path()?;
            let path = unix_door_path(&agent, actor_id).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "the agent socket path has no parent",
                )
            })?;
            crate::unix_transport::check_socket_path_len(&path)?;
            Ok(Self::Unix(path))
        }
        #[cfg(windows)]
        {
            let sid = crate::win_token::current_user_sid_string()
                .map_err(|e| io::Error::other(e.to_string()))?;
            Ok(Self::Pipe(pipe_name_for(&sid, actor_id)))
        }
    }
}

impl std::fmt::Display for OracleEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => write!(f, "{}", path.display()),
            #[cfg(windows)]
            Self::Pipe(name) => f.write_str(name),
        }
    }
}

/// The door's socket beside the agent's: same directory, the account's own
/// leaf. `None` for an agent path with no parent.
#[cfg(unix)]
fn unix_door_path(agent_socket: &std::path::Path, actor_id: &[u8; 32]) -> Option<PathBuf> {
    Some(
        agent_socket
            .parent()?
            .join(format!("oracle-{}.sock", account_fingerprint(actor_id))),
    )
}

/// The door's per-SID pipe name. Pure, testable on every platform.
#[must_use]
pub fn pipe_name_for(sid: &str, actor_id: &[u8; 32]) -> String {
    format!(
        r"\\.\pipe\fauna-oracle.{sid}.{}",
        account_fingerprint(actor_id)
    )
}

/// The named mutex a windows door's endpoint lock takes: the door's own
/// `Local\FaunaOracleDoor.<SID>.<fingerprint>`, keyed on the pipe name's leaf
/// past its `fauna-oracle.` prefix — never the agent's
/// [`crate::sync::mutex_name_for_pipe`], which keys on the leaf's last `.`
/// segment alone and would name every account's door
/// `Local\FaunaSyncAgent.<fingerprint>`. A `Local\` name may hold no further
/// backslash, which the leaf never does. Pure, testable on every platform.
#[must_use]
pub fn door_mutex_name(pipe_name: &str) -> String {
    let leaf = pipe_name.rsplit('\\').next().unwrap_or(pipe_name);
    let key = leaf.strip_prefix("fauna-oracle.").unwrap_or(leaf);
    format!(r"Local\FaunaOracleDoor.{key}")
}

/// An account's door endpoint, held: the agent's single-instance shape, per
/// (OS user, account), since the endpoint name carries both
/// (`conversations-oracle.md` § The custodian and the door: **one app serves
/// at a time**) — a `flock` on `<socket>.lock` on unix, the named mutex
/// [`door_mutex_name`] on windows. Released on drop; a crashed holder's is
/// released by the kernel.
pub struct EndpointLock {
    #[cfg(unix)]
    _lock: crate::unix_transport::InstanceLock,
    #[cfg(windows)]
    _lock: crate::pipe_transport::InstanceLock,
}

impl OracleEndpoint {
    /// Take this endpoint's lock — `AddrInUse` while another holder, in this
    /// process or another, has it.
    pub fn lock(&self) -> io::Result<EndpointLock> {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => Ok(EndpointLock {
                _lock: crate::unix_transport::InstanceLock::acquire(path)?,
            }),
            #[cfg(windows)]
            Self::Pipe(name) => Ok(EndpointLock {
                _lock: crate::pipe_transport::InstanceLock::acquire(&door_mutex_name(name))?,
            }),
        }
    }
}

/// The principal's first frame: who it claims to be, and to which account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// The principal's roster id (`third_party_principals.principal_id`).
    #[serde(with = "serde_bytes")]
    pub principal_id: Vec<u8>,
    /// The account's full actor id — the endpoint name carries only its
    /// fingerprint, so the custodian refuses a mismatch here.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
}

/// The custodian's answer to [`Hello`]: a fresh nonce HPKE-sealed to the
/// principal's attested holder key — or the refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Challenge {
    Sealed {
        #[serde(with = "serde_bytes")]
        enc: Vec<u8>,
        #[serde(with = "serde_bytes")]
        ciphertext: Vec<u8>,
    },
    Refused(OracleRefusal),
}

/// The principal's proof: the nonce, opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    #[serde(with = "serde_bytes")]
    pub nonce: Vec<u8>,
}

/// The custodian's verdict on [`Proof`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admission {
    Admitted,
    Refused(OracleRefusal),
}

/// One operation, after admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleRequest {
    pub id: u64,
    pub op: OracleOp,
}

/// The two classes' operations (`conversations-oracle.md` § The two classes).
/// There is no membership operation, and none may be added: invite, remove,
/// leave, rename, policy and transfer stay the page's gestures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OracleOp {
    /// `conversation.read` — the account's rooms.
    ListRooms,
    /// `conversation.read` — one page of a room's messages: the
    /// [`ORACLE_PAGE_SIZE`] messages before `before` (exclusive), or the newest
    /// page when `None`.
    ReadRoom {
        room_id: String,
        before: Option<u64>,
    },
    /// `conversation.read` — search the device's conversation index, or one
    /// community room through its home nest when `room_id` names one.
    Search {
        query: String,
        room_id: Option<String>,
    },
    /// `conversation.send` — a text message as the account into a room.
    Send { room_id: String, body: String },
}

impl OracleOp {
    /// The class the operation runs under — the name the grant's tuple and
    /// the record carry.
    #[must_use]
    pub fn class_name(&self) -> &'static str {
        match self {
            OracleOp::ListRooms | OracleOp::ReadRoom { .. } | OracleOp::Search { .. } => {
                "conversation.read"
            }
            OracleOp::Send { .. } => "conversation.send",
        }
    }

    /// The room the operation names, for the record's detail.
    #[must_use]
    pub fn room_id(&self) -> Option<&str> {
        match self {
            OracleOp::ListRooms => None,
            OracleOp::ReadRoom { room_id, .. } | OracleOp::Send { room_id, .. } => Some(room_id),
            OracleOp::Search { room_id, .. } => room_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleResponse {
    pub id: u64,
    pub result: Result<OracleReply, OracleRefusal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OracleReply {
    Rooms(Vec<OracleRoom>),
    Page(OracleMessagePage),
    Hits(Vec<OracleSearchHit>),
    Sent(OracleSent),
}

/// A room as the conversations page lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleRoom {
    pub room_id: String,
    /// The title the page shows.
    pub title: String,
    /// Who holds a key to the room's log: `end-to-end`, `community` or
    /// `transport-only` — the class the room's member set derives, which the
    /// oracle never moves.
    pub class: String,
    /// The members the page names, as it names them.
    pub members: Vec<String>,
    pub last_activity_ms: i64,
    pub unread: u32,
    /// Whether a send would leave (the page's reply bar).
    pub can_send: bool,
    /// Whether the room is searchable through its home nest (a community room).
    pub can_search_room: bool,
}

/// A message as the page renders it, with its position in the room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleMessage {
    /// 0 is the oldest message the device holds; positions only grow.
    pub position: u64,
    pub message_id: String,
    pub sender: String,
    pub sender_display: String,
    pub body: String,
    pub timestamp_ms: i64,
    pub is_own: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleMessagePage {
    pub room_id: String,
    /// Oldest first.
    pub messages: Vec<OracleMessage>,
    /// The `before` that reads the page preceding this one, or `None` at the
    /// room's start.
    pub earlier: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleSearchHit {
    pub room_id: String,
    /// The hit's position when the device holds the message.
    pub position: Option<u64>,
    pub excerpt: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleSent {
    pub room_id: String,
    pub message_id: String,
    pub position: u64,
}

/// Why the door refused — typed, so the principal relays it verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OracleRefusal {
    /// The handshake named an account this door does not serve.
    WrongAccount,
    /// No principal of this account has that id, or it attested no holder key.
    UnknownPrincipal,
    /// The proof did not open to the nonce.
    ProofFailed,
    /// The principal's live grant does not name the class.
    NotGranted { class: String },
    /// The oracle's ceiling: try again after the window rolls.
    CeilingReached { retry_after_secs: u64 },
    /// No such room on this device.
    NoSuchRoom,
    /// The page would refuse this too — its sentence.
    Refused { reason: String },
    /// A request the door cannot read or a version it does not speak.
    Unsupported,
    /// The custodian is not serving the account right now (signed out, or the
    /// conversations engine is held elsewhere).
    NotServing,
}

impl std::fmt::Display for OracleRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OracleRefusal::WrongAccount => {
                f.write_str("this Fauna app is signed in to a different account")
            }
            OracleRefusal::UnknownPrincipal => {
                f.write_str("this app is not one the account has approved")
            }
            OracleRefusal::ProofFailed => f.write_str("this app could not prove its key"),
            OracleRefusal::NotGranted { class } => {
                write!(f, "the account has not granted this app {class}")
            }
            OracleRefusal::CeilingReached { retry_after_secs } => write!(
                f,
                "too many conversation operations; try again in {retry_after_secs} seconds"
            ),
            OracleRefusal::NoSuchRoom => f.write_str("no such conversation on this device"),
            OracleRefusal::Refused { reason } => f.write_str(reason),
            OracleRefusal::Unsupported => {
                f.write_str("the Fauna app on this device does not support this request")
            }
            OracleRefusal::NotServing => {
                f.write_str("the Fauna app on this device is not serving this account right now")
            }
        }
    }
}

impl std::error::Error for OracleRefusal {}

/// The custodian's half of the door — what [`serve_connection`] asks of it.
/// Every method is the custodian's policy; this module only carries frames.
pub trait OracleDoor: Send + Sync + 'static {
    /// The admitted principal's identity, threaded to [`Self::perform`].
    type Principal: Send + Sync + 'static;

    /// Answer a [`Hello`]: seal a fresh nonce to the principal's holder key,
    /// returning the challenge and what the proof must open to.
    fn challenge<'a>(
        &'a self,
        hello: &'a Hello,
    ) -> DoorFuture<'a, ChallengeOutcome<Self::Principal>>;

    /// Perform one operation for an admitted principal. Authorize, ceiling and
    /// record are the custodian's, per call.
    fn perform<'a>(
        &'a self,
        principal: &'a Self::Principal,
        op: OracleOp,
    ) -> DoorFuture<'a, Result<OracleReply, OracleRefusal>>;
}

/// What [`OracleDoor::challenge`] answers: the sealed challenge and what its
/// proof must open to, or the refusal.
pub type ChallengeOutcome<P> = Result<(Challenge, PendingProof<P>), OracleRefusal>;

/// The boxed future an [`OracleDoor`] method answers with.
pub type DoorFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a sealed challenge expects back: the nonce, and the principal it
/// admits when the proof matches.
pub struct PendingProof<P> {
    pub nonce: zeroize::Zeroizing<[u8; ORACLE_NONCE_LEN]>,
    pub principal: P,
}

use std::future::Future;

/// Serve one door connection: the handshake, then operations until the
/// principal closes it. Returns when the connection ends. Generic over the
/// stream so the unix socket and the windows pipe share it.
pub async fn serve_connection<S, D>(stream: S, door: std::sync::Arc<D>) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    D: OracleDoor,
{
    use tokio::io::AsyncWriteExt;
    let (mut reader, mut writer) = tokio::io::split(stream);

    let hello: Hello = read_typed(&mut reader).await?;
    let pending = if hello.version != ORACLE_PROTOCOL_VERSION {
        Err(OracleRefusal::Unsupported)
    } else {
        door.challenge(&hello).await
    };
    let pending = match pending {
        Ok((challenge, pending)) => {
            writer.write_all(&frame(&challenge)?).await?;
            pending
        }
        Err(refusal) => {
            writer
                .write_all(&frame(&Challenge::Refused(refusal))?)
                .await?;
            return Ok(());
        }
    };
    let proof: Proof = read_typed(&mut reader).await?;
    if !constant_time_eq(&proof.nonce, &pending.nonce[..]) {
        writer
            .write_all(&frame(&Admission::Refused(OracleRefusal::ProofFailed))?)
            .await?;
        return Ok(());
    }
    writer.write_all(&frame(&Admission::Admitted)?).await?;
    let principal = pending.principal;

    loop {
        let payload = match crate::frame_io::read_frame(&mut reader).await {
            Ok(payload) => payload,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let response = match crate::decode_payload::<OracleRequest>(&payload) {
            Ok(req) => OracleResponse {
                id: req.id,
                result: door.perform(&principal, req.op).await,
            },
            // A request this build cannot read is refused by its id and the
            // connection keeps serving (`transport.md` § Rule 3 in full).
            Err(_) => match crate::decode_frame_id(&payload) {
                Some(id) => OracleResponse {
                    id,
                    result: Err(OracleRefusal::Unsupported),
                },
                None => return Ok(()),
            },
        };
        writer.write_all(&frame(&response)?).await?;
    }
}

/// An account's door, bound: the `0600` socket on unix, the pipe name's
/// first instance under the owner-only DACL on windows
/// ([`crate::pipe_transport::PipeListener`]). Bound by the endpoint lock's
/// holder ([`OracleEndpoint::lock`]) — which is what makes the unix
/// stale-socket unlink safe, exactly as for the agent — so the door is
/// reachable from the moment [`bind`](Self::bind) answers.
pub struct OracleListener {
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    listener: crate::pipe_transport::PipeListener,
}

impl OracleListener {
    /// Bind `endpoint`. Needs a tokio runtime. On windows a name another
    /// server holds — this user's app in another logon session, or another
    /// account squatting it — fails the first-instance create, and this
    /// answers that failure.
    pub fn bind(endpoint: &OracleEndpoint) -> io::Result<Self> {
        match endpoint {
            #[cfg(unix)]
            OracleEndpoint::Unix(path) => {
                crate::unix_transport::prepare_socket_dir(path)?;
                let _ = std::fs::remove_file(path);
                let listener = tokio::net::UnixListener::bind(path)?;
                crate::unix_transport::restrict_socket_perms(path)?;
                Ok(Self {
                    listener,
                    path: path.clone(),
                })
            }
            #[cfg(windows)]
            OracleEndpoint::Pipe(name) => Ok(Self {
                listener: crate::pipe_transport::PipeListener::bind(name)
                    .map_err(|e| io::Error::other(format!("{e:#}")))?,
            }),
        }
    }

    /// Serve connections, each on a task of its own, until `shutdown` flips.
    pub async fn serve<D: OracleDoor>(
        mut self,
        door: std::sync::Arc<D>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> io::Result<()> {
        loop {
            tokio::select! {
                accepted = self.accept(&door) => accepted?,
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
        Ok(())
    }

    /// Accept one connection and serve it on its own task. Cancel-safe: both
    /// accepts are, and nothing after them awaits.
    #[cfg(unix)]
    async fn accept<D: OracleDoor>(&mut self, door: &std::sync::Arc<D>) -> io::Result<()> {
        let (stream, _addr) = self.listener.accept().await?;
        spawn_connection(stream, door.clone());
        Ok(())
    }

    #[cfg(windows)]
    async fn accept<D: OracleDoor>(&mut self, door: &std::sync::Arc<D>) -> io::Result<()> {
        let pipe = self
            .listener
            .accept()
            .await
            .map_err(|e| io::Error::other(format!("{e:#}")))?;
        spawn_connection(pipe, door.clone());
        Ok(())
    }
}

fn spawn_connection<S, D>(stream: S, door: std::sync::Arc<D>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    D: OracleDoor,
{
    tokio::spawn(async move {
        if let Err(e) = serve_connection(stream, door).await {
            tracing::debug!("oracle door connection ended: {e}");
        }
    });
}

fn frame<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    crate::encode_frame(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

async fn read_typed<R, T>(reader: &mut R) -> io::Result<T>
where
    R: tokio::io::AsyncReadExt + Unpin,
    T: serde::de::DeserializeOwned,
{
    let payload = crate::frame_io::read_frame(reader).await?;
    crate::decode_payload(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Why a principal could not reach or enter the door.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// Nothing serves the account's door: no signed-in Fauna app hosts the
    /// account's conversations on this device.
    // public-text: cli-answer-strings-public
    #[error("open Fauna on this device to let this app reach your conversations")]
    // public-text-end: cli-answer-strings-public
    NoCustodian(#[source] io::Error),
    /// The custodian refused the handshake.
    #[error("{0}")]
    Refused(OracleRefusal),
    /// The challenge did not open under this principal's holder key.
    #[error("this app could not open the Fauna app's challenge")]
    CannotOpen,
    /// windows: the process serving the door's pipe name is not this user, or
    /// could not be identified. The pipe namespace is machine-wide, so another
    /// local account can hold the name while no app of this user serves it;
    /// refused before the first frame, and never retried as a missing
    /// custodian.
    #[error(
        "refusing the Fauna app's door at {pipe}: {reason}; another account on this device may be holding its name"
    )]
    ServedByAnother { pipe: String, reason: String },
    #[error("the Fauna app's door failed: {0}")]
    Io(#[from] io::Error),
}

/// The two blocking halves of one door connection.
type Halves = (
    Box<dyn std::io::Read + Send + 'static>,
    Box<dyn std::io::Write + Send + 'static>,
);

/// The principal's blocking client — one admitted connection. Requests and
/// replies alternate, one at a time, so it needs no reader thread.
pub struct OracleClient {
    reader: Box<dyn std::io::Read + Send + 'static>,
    writer: Box<dyn std::io::Write + Send + 'static>,
    next_id: u64,
}

impl OracleClient {
    /// Connect to `endpoint` and prove possession of the holder key: `open`
    /// is the principal's HPKE open of `(enc, ciphertext)` under its holder
    /// secret, answering the nonce or `None`.
    pub fn connect(
        endpoint: &OracleEndpoint,
        principal_id: &[u8],
        actor_id: &[u8; 32],
        open: impl FnOnce(&[u8], &[u8]) -> Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> Result<Self, ConnectError> {
        let (reader, writer) = open_endpoint(endpoint)?;
        Self::handshake(reader, writer, principal_id, actor_id, open)
    }

    fn handshake(
        reader: Box<dyn std::io::Read + Send + 'static>,
        writer: Box<dyn std::io::Write + Send + 'static>,
        principal_id: &[u8],
        actor_id: &[u8; 32],
        open: impl FnOnce(&[u8], &[u8]) -> Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> Result<Self, ConnectError> {
        let mut client = Self {
            reader,
            writer,
            next_id: 1,
        };
        client.write(&Hello {
            version: ORACLE_PROTOCOL_VERSION,
            principal_id: principal_id.to_vec(),
            actor_id: actor_id.to_vec(),
        })?;
        let (enc, ciphertext) = match client.read::<Challenge>()? {
            Challenge::Sealed { enc, ciphertext } => (enc, ciphertext),
            Challenge::Refused(r) => return Err(ConnectError::Refused(r)),
        };
        let nonce = open(&enc, &ciphertext).ok_or(ConnectError::CannotOpen)?;
        client.write(&Proof {
            nonce: nonce.to_vec(),
        })?;
        match client.read::<Admission>()? {
            Admission::Admitted => Ok(client),
            Admission::Refused(r) => Err(ConnectError::Refused(r)),
        }
    }

    /// Perform one operation.
    pub fn call(
        &mut self,
        op: OracleOp,
    ) -> Result<Result<OracleReply, OracleRefusal>, ConnectError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&OracleRequest { id, op })?;
        let response: OracleResponse = self.read()?;
        if response.id != id {
            return Err(ConnectError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "the door answered another request",
            )));
        }
        Ok(response.result)
    }

    fn write<T: Serialize>(&mut self, msg: &T) -> io::Result<()> {
        self.writer.write_all(&frame(msg)?)
    }

    fn read<T: serde::de::DeserializeOwned>(&mut self) -> io::Result<T> {
        let payload = crate::sync_pipe_client::read_frame(&mut self.reader)?;
        crate::decode_payload(&payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }
}

/// Reach the door: nothing answering is [`ConnectError::NoCustodian`].
#[cfg(unix)]
fn open_endpoint(endpoint: &OracleEndpoint) -> Result<Halves, ConnectError> {
    let OracleEndpoint::Unix(path) = endpoint;
    let stream =
        std::os::unix::net::UnixStream::connect(path).map_err(ConnectError::NoCustodian)?;
    let reader = stream.try_clone()?;
    Ok((Box::new(reader), Box::new(stream)))
}

/// Reach the door, and refuse it unless this very user serves it — before
/// the first frame, so a [`Hello`] never reaches another account
/// (`conversations-oracle.md` § The custodian and the door; the check is the
/// sync-pipe client's, `sync-agent.md` § Control plane split).
#[cfg(windows)]
fn open_endpoint(endpoint: &OracleEndpoint) -> Result<Halves, ConnectError> {
    let OracleEndpoint::Pipe(name) = endpoint;
    let expected = crate::win_token::current_user_sid_string().map_err(|e| {
        ConnectError::Io(io::Error::other(format!(
            "resolve this process's own user SID: {e:#}"
        )))
    })?;
    open_pipe_expecting(name, &expected)
}

/// [`open_endpoint`] with the server SID to demand passed in. Deliberately not
/// public: the only value production demands is this process's own, and this
/// exists so the refusal arm is testable on a one-account box.
#[cfg(windows)]
fn open_pipe_expecting(name: &str, expected_sid: &str) -> Result<Halves, ConnectError> {
    use crate::sync_pipe_client::overlapped::{OverlappedReader, OverlappedWriter, SharedPipe};

    let file =
        crate::sync_pipe_client::open_pipe_handle(name).map_err(ConnectError::NoCustodian)?;
    crate::sync_pipe_client::pipe_server_is(&file, expected_sid).map_err(|reason| {
        ConnectError::ServedByAnother {
            pipe: name.to_string(),
            reason,
        }
    })?;
    let pipe = std::sync::Arc::new(SharedPipe::new(file));
    let reader = OverlappedReader::new(pipe.clone())?;
    let writer = OverlappedWriter::new(pipe)?;
    Ok((Box::new(reader), Box::new(writer)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR: [u8; 32] = [0xAB; 32];

    #[test]
    fn the_fingerprint_is_the_actor_ids_first_eight_bytes_in_hex() {
        assert_eq!(account_fingerprint(&ACTOR), "abababababababab");
        assert_eq!(account_fingerprint(&ACTOR).len(), 16);
    }

    #[test]
    fn the_windows_door_is_a_per_sid_per_account_pipe() {
        assert_eq!(
            pipe_name_for("S-1-5-21-1-2-3-1001", &ACTOR),
            r"\\.\pipe\fauna-oracle.S-1-5-21-1-2-3-1001.abababababababab"
        );
    }

    /// The door's lock is its own per (OS user, account) — never the agent's
    /// mutex, which the agent's derivation would name for every account.
    #[test]
    fn the_windows_door_locks_a_mutex_of_its_own_per_user_and_account() {
        let pipe = pipe_name_for("S-1-5-21-1-2-3-1001", &ACTOR);
        assert_eq!(
            door_mutex_name(&pipe),
            r"Local\FaunaOracleDoor.S-1-5-21-1-2-3-1001.abababababababab"
        );
        assert_ne!(
            door_mutex_name(&pipe),
            crate::sync::mutex_name_for_pipe(&pipe)
        );
        assert_ne!(
            door_mutex_name(&pipe),
            door_mutex_name(&pipe_name_for("S-1-5-21-1-2-3-1001", &[0xCD; 32]))
        );
        // A test pipe's leaf keys the lock whole, with no backslash past
        // `Local\`.
        assert_eq!(
            door_mutex_name(r"\\.\pipe\fauna-oracle-test-7-handover"),
            r"Local\FaunaOracleDoor.fauna-oracle-test-7-handover"
        );
    }

    /// An endpoint of this test's own: a socket in `dir` on unix, a pipe name
    /// unique to this process on windows.
    #[cfg_attr(windows, allow(unused_variables))]
    fn test_endpoint(dir: &std::path::Path, name: &str) -> OracleEndpoint {
        #[cfg(unix)]
        return OracleEndpoint::Unix(dir.join(format!("{name}.sock")));
        #[cfg(windows)]
        return OracleEndpoint::Pipe(format!(
            r"\\.\pipe\fauna-oracle-test-{}-{name}",
            std::process::id()
        ));
    }

    #[tokio::test]
    async fn one_holder_takes_an_endpoints_lock_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = test_endpoint(dir.path(), "lock");
        let held = endpoint.lock().expect("first come");
        assert_eq!(
            endpoint.lock().err().map(|e| e.kind()),
            Some(io::ErrorKind::AddrInUse)
        );
        drop(held);
        endpoint.lock().expect("free once dropped");
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_door_sits_beside_the_agent_socket() {
        let agent = std::path::Path::new("/run/user/1000/fauna/sync-agent.sock");
        assert_eq!(
            unix_door_path(agent, &ACTOR),
            Some(PathBuf::from(
                "/run/user/1000/fauna/oracle-abababababababab.sock"
            ))
        );
    }

    #[test]
    fn every_op_names_its_class_and_none_is_a_membership_verb() {
        let ops = [
            OracleOp::ListRooms,
            OracleOp::ReadRoom {
                room_id: "r".into(),
                before: None,
            },
            OracleOp::Search {
                query: "q".into(),
                room_id: None,
            },
            OracleOp::Send {
                room_id: "r".into(),
                body: "b".into(),
            },
        ];
        let classes: Vec<&str> = ops.iter().map(OracleOp::class_name).collect();
        assert_eq!(
            classes,
            [
                "conversation.read",
                "conversation.read",
                "conversation.read",
                "conversation.send"
            ]
        );
    }

    /// A door whose challenge is a stand-in seal (the "ciphertext" is the
    /// nonce xor 0x5A) and whose one operation names the principal it saw.
    struct Door;
    impl OracleDoor for Door {
        type Principal = Vec<u8>;
        fn challenge<'a>(&'a self, hello: &'a Hello) -> DoorFuture<'a, ChallengeOutcome<Vec<u8>>> {
            Box::pin(async move { self.sync_challenge(hello) })
        }
        fn perform<'a>(
            &'a self,
            principal: &'a Vec<u8>,
            op: OracleOp,
        ) -> DoorFuture<'a, Result<OracleReply, OracleRefusal>> {
            Box::pin(async move { self.sync_perform(principal, op) })
        }
    }
    impl Door {
        fn sync_challenge(&self, hello: &Hello) -> ChallengeOutcome<Vec<u8>> {
            if hello.actor_id != ACTOR {
                return Err(OracleRefusal::WrongAccount);
            }
            let nonce = [7u8; ORACLE_NONCE_LEN];
            Ok((
                Challenge::Sealed {
                    enc: vec![0; 32],
                    ciphertext: nonce.iter().map(|b| b ^ 0x5A).collect(),
                },
                PendingProof {
                    nonce: zeroize::Zeroizing::new(nonce),
                    principal: hello.principal_id.clone(),
                },
            ))
        }
        fn sync_perform(
            &self,
            principal: &[u8],
            op: OracleOp,
        ) -> Result<OracleReply, OracleRefusal> {
            match op {
                OracleOp::ListRooms => Ok(OracleReply::Rooms(vec![OracleRoom {
                    room_id: hex::encode(principal),
                    title: "t".into(),
                    class: "end-to-end".into(),
                    members: vec![],
                    last_activity_ms: 0,
                    unread: 0,
                    can_send: true,
                    can_search_room: false,
                }])),
                _ => Err(OracleRefusal::NoSuchRoom),
            }
        }
    }

    /// The stand-in seal's open.
    fn open(_: &[u8], c: &[u8]) -> Option<zeroize::Zeroizing<Vec<u8>>> {
        Some(zeroize::Zeroizing::new(
            c.iter().map(|b| b ^ 0x5A).collect(),
        ))
    }

    /// Bind `endpoint` and serve [`Door`] on it; the shutdown switch and the
    /// server task. The door is reachable once this answers.
    fn serve_door(
        endpoint: &OracleEndpoint,
    ) -> (
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let listener = OracleListener::bind(endpoint).expect("bind the test door");
        let (tx, rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(listener.serve(std::sync::Arc::new(Door), rx));
        (tx, server)
    }

    /// The door end to end over the platform's real transport — a unix
    /// socket, a windows named pipe: a refused hello, a failed proof, then an
    /// admitted principal whose operations reach the door's `perform` with
    /// the principal it proved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_handshake_admits_only_the_proven_principal() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = test_endpoint(dir.path(), "handshake");
        let (tx, server) = serve_door(&endpoint);

        let outcome = tokio::task::spawn_blocking({
            let endpoint = endpoint.clone();
            move || {
                let wrong = OracleClient::connect(&endpoint, b"p1", &[0; 32], open).err();
                let bad_proof = OracleClient::connect(&endpoint, b"p1", &ACTOR, |_, _| {
                    Some(zeroize::Zeroizing::new(vec![0; ORACLE_NONCE_LEN]))
                })
                .err();
                let mut ok = OracleClient::connect(&endpoint, b"p1", &ACTOR, open).unwrap();
                let rooms = ok.call(OracleOp::ListRooms).unwrap();
                let missing = ok
                    .call(OracleOp::ReadRoom {
                        room_id: "nope".into(),
                        before: None,
                    })
                    .unwrap();
                (wrong, bad_proof, rooms, missing)
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            outcome.0,
            Some(ConnectError::Refused(OracleRefusal::WrongAccount))
        ));
        assert!(matches!(
            outcome.1,
            Some(ConnectError::Refused(OracleRefusal::ProofFailed))
        ));
        let Ok(OracleReply::Rooms(rooms)) = outcome.2 else {
            panic!("{:?}", outcome.2)
        };
        assert_eq!(
            rooms[0].room_id,
            hex::encode(b"p1"),
            "perform saw the proven principal"
        );
        assert_eq!(outcome.3, Err(OracleRefusal::NoSuchRoom));

        // Nothing serves a door no app opened: the one-sentence gap.
        let absent = test_endpoint(dir.path(), "absent");
        let none = tokio::task::spawn_blocking(move || {
            OracleClient::connect(&absent, b"p1", &ACTOR, open).err()
        })
        .await
        .unwrap()
        .expect("no custodian");
        assert!(matches!(none, ConnectError::NoCustodian(_)));
        assert!(none.to_string().contains("open Fauna on this device"));

        tx.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    /// The windows door's client speaks to no one but this user: a pipe served
    /// by anyone else — here, demanded of `S-1-5-18`, the probe and the
    /// comparison a squatting account meets — is refused before the client
    /// exists, so no [`Hello`] is ever written to it; and the same pipe,
    /// demanded of this user, is accepted (the sync-pipe client's
    /// `server_identity` pins, on the door's seam).
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_windows_client_refuses_a_door_this_user_does_not_serve() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = test_endpoint(dir.path(), "identity");
        let (tx, server) = serve_door(&endpoint);
        let OracleEndpoint::Pipe(name) = endpoint.clone();

        let (foreign, own) = tokio::task::spawn_blocking(move || {
            let foreign = open_pipe_expecting(&name, "S-1-5-18").err();
            let own_sid = crate::win_token::current_user_sid_string().expect("own SID");
            let own = open_pipe_expecting(&name, &own_sid)
                .and_then(|(reader, writer)| {
                    OracleClient::handshake(reader, writer, b"p1", &ACTOR, open)
                })
                .and_then(|mut client| client.call(OracleOp::ListRooms));
            (foreign, own)
        })
        .await
        .unwrap();
        match foreign {
            Some(ConnectError::ServedByAnother { pipe, reason }) => {
                assert!(pipe.contains("fauna-oracle-test"), "{pipe}");
                assert!(reason.contains("S-1-5-18"), "{reason}");
            }
            other => panic!("a pipe this user does not serve is refused typed: {other:?}"),
        }
        assert!(
            matches!(own, Ok(Ok(OracleReply::Rooms(_)))),
            "the same door, served by this user, admits: {own:?}"
        );

        tx.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    /// A refusal a newer door added fails the one call that met it, typed, and
    /// the connection still answers the next call — `OracleRefusal`'s `skip`
    /// line in `enum_ledger.txt` (`transport.md` § Rule 3 in full).
    #[test]
    fn an_unreadable_response_fails_its_call_typed_and_the_connection_survives() {
        use std::io::Write;

        #[derive(Serialize)]
        enum NewerRefusal {
            AddedInANewerDoor,
        }
        #[derive(Serialize)]
        struct NewerResponse {
            id: u64,
            result: Result<OracleReply, NewerRefusal>,
        }

        let (mut door_reads, client_writes) = std::io::pipe().unwrap();
        let (client_reads, mut door) = std::io::pipe().unwrap();
        let mut client = OracleClient {
            reader: Box::new(client_reads),
            writer: Box::new(client_writes),
            next_id: 1,
        };
        let door_side = std::thread::spawn(move || {
            crate::sync_pipe_client::read_frame(&mut door_reads).unwrap();
            let newer = NewerResponse {
                id: 1,
                result: Err(NewerRefusal::AddedInANewerDoor),
            };
            door.write_all(&crate::encode_frame(&newer).unwrap())
                .unwrap();
            crate::sync_pipe_client::read_frame(&mut door_reads).unwrap();
            let known = OracleResponse {
                id: 2,
                result: Err(OracleRefusal::NoSuchRoom),
            };
            door.write_all(&crate::encode_frame(&known).unwrap())
                .unwrap();
        });

        match client.call(OracleOp::ListRooms) {
            Err(ConnectError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
            other => panic!("an unreadable response is a typed failure, got {other:?}"),
        }
        let next = client
            .call(OracleOp::ListRooms)
            .expect("the next call is answered");
        assert_eq!(next, Err(OracleRefusal::NoSuchRoom));
        door_side.join().unwrap();
    }
}
