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
//! the typed refusals, the endpoint resolver, a blocking client, and the
//! server's per-connection loop. The custodian (the shared crate every desktop
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

/// Bind the account's door socket and serve connections until `shutdown`
/// flips. The caller holds the endpoint lock ([`crate::unix_transport::
/// InstanceLock`] on the same path) — which is what makes the stale-socket
/// unlink here safe, exactly as for the agent.
#[cfg(unix)]
pub async fn serve_unix<D: OracleDoor>(
    socket_path: &std::path::Path,
    door: std::sync::Arc<D>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    crate::unix_transport::prepare_socket_dir(socket_path)?;
    let _ = std::fs::remove_file(socket_path);
    let listener = tokio::net::UnixListener::bind(socket_path)?;
    crate::unix_transport::restrict_socket_perms(socket_path)?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                let door = door.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(stream, door).await {
                        tracing::debug!("oracle door connection ended: {e}");
                    }
                });
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(())
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
    #[error("the Fauna app's door failed: {0}")]
    Io(#[from] io::Error),
}

/// The principal's blocking client — one admitted connection.
pub struct OracleClient {
    #[cfg(unix)]
    stream: std::os::unix::net::UnixStream,
    next_id: u64,
}

impl OracleClient {
    /// Connect to `endpoint` and prove possession of the holder key: `open`
    /// is the principal's HPKE open of `(enc, ciphertext)` under its holder
    /// secret, answering the nonce or `None`.
    #[cfg(unix)]
    pub fn connect(
        endpoint: &OracleEndpoint,
        principal_id: &[u8],
        actor_id: &[u8; 32],
        open: impl FnOnce(&[u8], &[u8]) -> Option<zeroize::Zeroizing<Vec<u8>>>,
    ) -> Result<Self, ConnectError> {
        let OracleEndpoint::Unix(path) = endpoint;
        let stream =
            std::os::unix::net::UnixStream::connect(path).map_err(ConnectError::NoCustodian)?;
        let mut client = Self { stream, next_id: 1 };
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
    #[cfg(unix)]
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

    #[cfg(unix)]
    fn write<T: Serialize>(&mut self, msg: &T) -> io::Result<()> {
        use std::io::Write;
        self.stream.write_all(&frame(msg)?)
    }

    #[cfg(unix)]
    fn read<T: serde::de::DeserializeOwned>(&mut self) -> io::Result<T> {
        let payload = crate::sync_pipe_client::read_frame(&mut self.stream)?;
        crate::decode_payload(&payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }
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

    /// The door end to end over a real socket pair: a refused hello, a failed
    /// proof, then an admitted principal whose operations reach the door's
    /// `perform` with the principal it proved.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_handshake_admits_only_the_proven_principal() {
        struct Door;
        impl OracleDoor for Door {
            type Principal = Vec<u8>;
            fn challenge<'a>(
                &'a self,
                hello: &'a Hello,
            ) -> DoorFuture<'a, ChallengeOutcome<Vec<u8>>> {
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
                // A stand-in seal: the "ciphertext" is the nonce xor 0x5A.
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
                {
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
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oracle-test.sock");
        let (tx, rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn({
            let path = path.clone();
            async move { serve_unix(&path, std::sync::Arc::new(Door), rx).await }
        });
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let endpoint = OracleEndpoint::Unix(path.clone());
        let open = |_: &[u8], c: &[u8]| {
            Some(zeroize::Zeroizing::new(
                c.iter().map(|b| b ^ 0x5A).collect(),
            ))
        };

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
        let none = OracleClient::connect(
            &OracleEndpoint::Unix(dir.path().join("absent.sock")),
            b"p1",
            &ACTOR,
            open,
        )
        .err()
        .expect("no custodian");
        assert!(matches!(none, ConnectError::NoCustodian(_)));
        assert!(none.to_string().contains("open Fauna on this device"));

        tx.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    /// A refusal a newer door added fails the one call that met it, typed, and
    /// the connection still answers the next call — `OracleRefusal`'s `skip`
    /// line in `enum_ledger.txt` (`transport.md` § Rule 3 in full).
    #[cfg(unix)]
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

        let (stream, mut door) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut client = OracleClient { stream, next_id: 1 };
        let door_side = std::thread::spawn(move || {
            crate::sync_pipe_client::read_frame(&mut door).unwrap();
            let newer = NewerResponse {
                id: 1,
                result: Err(NewerRefusal::AddedInANewerDoor),
            };
            door.write_all(&crate::encode_frame(&newer).unwrap())
                .unwrap();
            crate::sync_pipe_client::read_frame(&mut door).unwrap();
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
