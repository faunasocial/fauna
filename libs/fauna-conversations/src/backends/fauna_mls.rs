//! Fauna-native MLS conversation rail.
//!
//! All MLS crypto lives in `fauna-mls`; all nest I/O goes through the
//! object-safe [`ConversationsRpc`] seam (no `fauna-protocol` dependency here —
//! mirrors how [`crate::backend::OutboundMailSink`] keeps the WS-RPC call out of
//! this crate). The `ThreadId → ChannelId` binding is **backend-internal** —
//! clients never see a `ChannelId` (`docs/goal/ui/conversations.md` §
//! Architectural rules #2: no client-side MLS state).
//!
//! Track A wired `send` for a thread
//! already bound to its MLS channel. Track B added new-thread group bootstrap
//! ([`FaunaMlsBackend::bootstrap_group`]: key-package fetch → `create_group` →
//! bind → welcome deliver) and the inbound decrypt driver
//! ([`poll_inbound_conv`]). Track C added receiver-side welcome ingest
//! ([`ingest_welcome`]: join → materialize the channel-keyed thread → bind).
//! Membership changes (add/remove/rename) are Track D.

use crate::address::{Rail, TypedAddress};
use crate::backend::{
    BackendError, ChannelCursor, CommitGate, ConvRpcError, ConversationsRpc, CustodyCeremonySink,
    DomainEvidence, FolderCustodySink, GroupReceptionKeys, HistoryPersist, InboundBucket, MailFeed,
    ProviderPersist, RailBackend, RailInboundMessage, ResolveResult, RoomCeremonyRpc,
    RoomGenerationReader, RoomPolicyEdit, RoomPolicyRebuild, RoomPrincipalKind, RoomRosterEntry,
    RoomRosterRead, RoomRosterReader, RoomRosterReport, RoomRosterReportOutcome,
    RoomRosterReporter, RoomSeams, SchedulingSink, SelfAddress, SendOutcome, ShareEndpointsSink,
    SiblingGroupAdopter, SuccessionWitness, WelcomeChannelKind, classify_foreign_non_answer,
};
use crate::capabilities::{ThreadCapabilities, derive_capabilities};
use crate::compose::ComposeState;
use crate::manager::ConversationsManager;
use crate::message::{AttachmentSnapshot, BodyFormat, MessageBadges, MessageId};
use crate::room::{RoomInvitation, RoomSnapshot};
use crate::snapshot::ThreadDetail;
use crate::thread::ThreadId;
use async_trait::async_trait;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::store::{AttachmentCoordinates, AttachmentOpeningKey, SealedBlobCoordinates};
use fauna_core::attachment_limits::{INLINE_BLOB_BODY_LIMIT, MAX_ATTACHMENTS_PER_RECORD};
use fauna_core::crypto::GenerationKey;
use fauna_core::data::{ContentHash, Timestamp};
use fauna_core::identity::ActorId;
use fauna_mls::engine::MlsEngine;
use fauna_mls::error::MlsError;
use fauna_mls::room_policy::{RoomOwnershipOffer, RoomPolicyExtension};
use fauna_mls::types::{
    ChannelAttachment, ChannelEnvelope, ChannelId, ChannelMessage, ChannelMessageBody,
    GroupMetaMessage, ReactionOp,
};

/// The most a history-for-joiners slice may weigh on the wire
/// (`conversation-rooms.md` § History for joiners): one channel record, well
/// under the nest's per-record serve-page budget, so a room with a long
/// transcript hands a newcomer its most recent messages rather than a record
/// the nest refuses. Older messages are trimmed first
/// ([`RailBackend::deliver_history_slice`]).
const MAX_HISTORY_SLICE_BYTES: usize = 512 * 1024;

/// The roles table's policy rows (`conversation-rooms.md` § Roles and
/// authorization) for one editor gesture: the admin set and the transfer are
/// the owner's alone, the name and the two rules are any governor's.
/// Asked twice — before the gesture, for the product refusal, and inside the
/// rebuild at commit time, for the race a catch-up can fold in between.
fn edit_permitted(edit: &RoomPolicyEdit, role: fauna_mls::room_policy::RoomRole) -> bool {
    use fauna_mls::room_policy::RoomRole as WireRole;
    match edit {
        RoomPolicyEdit::AppointAdmin(_)
        | RoomPolicyEdit::DemoteAdmin(_)
        | RoomPolicyEdit::TransferOwnership(_) => role == WireRole::Owner,
        RoomPolicyEdit::Rename(_)
        | RoomPolicyEdit::JoinRule(_)
        | RoomPolicyEdit::HistoryPolicy(_) => role.is_admin_or_owner(),
    }
}

/// A safety net before this account publishes its own reception key to a
/// room (founding, accepting, or the tend pass's own-seat heal) — the same
/// [`fauna_mls::wrapped_blob::XWingPublicKey::parse_and_validate`] every nest
/// writer now runs. Never expected to fire in production: the key
/// is re-derived from a stored seed (`GroupReceptionKeyRecord::keypair`), and
/// ML-KEM-768 key generation yields a FIPS-203-valid key for every seed by
/// construction — this only catches a corrupted local derivation before it
/// becomes a stored key the room can never wrap to.
fn self_check_reception_pubkey(bytes: Vec<u8>) -> Result<Vec<u8>, BackendError> {
    fauna_mls::wrapped_blob::XWingPublicKey::parse_and_validate(&bytes).map_err(|e| {
        BackendError::Internal(format!(
            "this account's group-reception key is corrupt: {e}"
        ))
    })?;
    Ok(bytes)
}

/// The floor facts a synchronous render needs, projected once at read time so
/// `room_state` does no work per frame.
///
/// Deliberately not the whole [`RoomFloor`]: what a render asks of a floor is
/// the class, each participant's rank, and the policy the editor shows. Keeping
/// the projection here rather than the rows means a later reader cannot
/// accidentally start deriving a *fourth* fact from a snapshot that may be a
/// poll old.
#[derive(Clone, Debug, Default)]
pub(crate) struct CachedFloor {
    /// Every seated principal's kind — what [`crate::room::derive_room_class`]
    /// is a function of. Includes the home nest and any bridge principal, which
    /// are not participants any thread renders but *are* what the class is.
    kinds: Vec<crate::room::PrincipalKind>,
    /// Each seated principal's rank, for the member chips and the viewer's own.
    roles: HashMap<ActorId, crate::room::RoomRole>,
    /// The room's signed policy, already verified, as the editor renders it.
    /// `None` on a policy-less room, and on one whose policy this device could not
    /// verify — which are the same thing to a reader: no policy to stand behind.
    policy: Option<crate::room::RoomPolicySnapshot>,
    /// Whether the room's current generation wraps to its home nest — whether
    /// the nest reads it. `None` when the floor seats no nest, has no
    /// generation yet, or the nest's reply carried no answer.
    nest_read: Option<bool>,
    /// The room's verified labeler set, as `RoomSnapshot::labelers` renders
    /// it ([`floor_labelers`]).
    labelers: Option<Vec<String>>,
}

// A floor delete record waiting for the policy version it names
// (`FaunaMlsBackend::parked_floor_deletes`) is the shape the replica slice
// carries too, since a parked record has to survive a relaunch — so the type
// and its cap live beside the slice.
use crate::store::history::{MAX_PARKED_FLOOR_DELETES, ParkedFloorDelete};

/// The most policy versions one judgment fetches. A chain longer than this is
/// walked across passes — the verified prefix is kept and the record parked —
/// so one record never holds the inbound walk for an unbounded run of reads.
const MAX_POLICY_VERSION_FETCHES: u64 = 32;

/// How many of a policy's names one floor delete judgment may ask the
/// [`SuccessionWitness`] about **for the first time** — each may cost one
/// bounded dial, inline on the inbound poll. A name asked before dials nothing
/// (the witness memoizes its dial, and the no-anchor arm dials nothing) — save
/// a verified line's periodic re-ask, which spends this budget too
/// ([`LINE_REASK_PASSES`]). It is not free of I/O, though: a not-yet
/// name's re-ask consults the witness's anchor store, an account-store read that
/// is generation-gated while the store reads and backed off while it does not
/// (`fauna_client_recovery::UNREADABLE_STORE_BACKOFF_SECS` — one read per
/// interval, however many parked records re-ask). So a policy naming more
/// than this resolves across passes — the record parked meanwhile — rather
/// than holding one poll for every admin's home nest in turn.
const MAX_FIRST_SEAT_ASKS: u32 = 4;

/// How many of a room's inbound passes pass before a policy name whose line
/// this device verified — empty ("never succeeded") or positive — may be
/// asked again ([`RoomSeats::verified`]). A verified line is true only *so
/// far*: the identity may succeed later in the session, and so may the newest
/// holder of a positive line (A1 → A2 verified, then A2 → A3), and a line
/// settled for good would refuse the successor's seat until the app quit. A
/// re-ask may dial, so it spends the judgment's [`MAX_FIRST_SEAT_ASKS`] budget
/// like a first ask; this is the bound on how often: per room, at most one
/// re-dial per verified name every this-many passes, whatever drives the
/// passes.
pub const LINE_REASK_PASSES: u64 = 16;

/// The most policy names one room offers the peer-anchor harvest
/// ([`FaunaMlsBackend::offer_policy_names_to_harvest`]). A policy's admin set
/// is unbounded and its author is whoever founded a room this member chose to
/// join, while the anchor store the harvest fills is bounded for the whole
/// account (`identity-succession.md` § The succession statement → rule 5): an
/// honest line retires a handful of names, owners first.
///
/// **A fixed prefix, never a rolling window.** The eight are the FIRST eight
/// names the anchored chain carries, cut before the still-unanchored filter
/// rather than after it. Cut after, a settled name leaving the offer admitted
/// the ninth behind it, so a founder naming K same-nest identities seeded K
/// entries — and since the anchor store outlives the session while
/// [`RoomSeats`] does not, no in-memory budget could have bounded that either.
/// Chain order is fixed by the signed chain (version 1's names first, a later
/// version's appended), so one chain yields one prefix in every session and
/// the bound needs nothing persisted. The price: a name past the eighth in
/// chain order is never offered, and its records stay parked — the ruling's
/// residual (a), which already stands for a cross-nest name.
///
/// What this does NOT bound: the chain itself is re-anchored from the room's
/// home nest each session, so a founder who signs rival version 2s, served by
/// a home nest that hands out a different one per session, moves positions
/// two to eight — seven names a session instead of every name at once, from
/// an attacker who must hold the room's home nest as well as its founding
/// key. What it buys is still only the store's conceded residual (a full
/// store denies *future* anchoring, never a demotion).
const MAX_POLICY_NAME_OFFERS: usize = 8;

/// One community room's resolved seats: whom the names its policies carry have
/// since become, as far as this device has **verified**
/// (`conversation-rooms.md` § Roles and authorization → *A name designates its
/// verified line*). Resolved lazily — only a rank refusal a succession could
/// lift asks anything — so a room nobody succeeded in never dials.
#[derive(Default, Clone)]
struct RoomSeats {
    /// The verified lines; what the shared judge is handed.
    lines: fauna_mls::room_policy::SuccessionLines,
    /// Names whose line this device **verified** — positive (held in
    /// [`Self::lines`]) or empty ("never succeeded") — each with the
    /// [`Self::passes`] count it was last asked at. True only *so far*, so
    /// asked again once [`LINE_REASK_PASSES`] passes have gone by, and a held
    /// line only ever grows (`SuccessionLines::insert`). While a witness is
    /// registered a rank refusal therefore parks rather than settling as a
    /// member's claim — the one verdict no later pass would revisit.
    /// Deliberately NOT [`Self::not_yet`]: a verified name holds an anchor,
    /// so it is neither offered to the harvest nor a sign the room's
    /// moderation is unverifiable
    /// ([`FaunaMlsBackend::moderation_unverified`]).
    verified: HashMap<ActorId, u64>,
    /// This room's inbound passes since its first judgment asked anything —
    /// the clock [`Self::verified`] re-asks by (bumped by
    /// [`FaunaMlsBackend::retry_parked_floor_deletes`]).
    passes: u64,
    /// Names asked and not established *yet*. Asked again on the next judgment
    /// that needs them — no dial, and the anchor-store read behind it bounded
    /// (see [`MAX_FIRST_SEAT_ASKS`]) — which is how an anchor a later harvest
    /// seeds gets used.
    not_yet: HashSet<ActorId>,
    /// The next policy version, fetched but refused for a rank an unresolved
    /// name may yet lift. Kept so the parked record's retries do not read the
    /// home nest once per pass for the same bytes; unanchored, it grants
    /// nothing, and a relaunch forgets it.
    pending: Option<fauna_mls::room_policy::SignedRoomPolicy>,
    /// What each parked floor delete record is still waiting on: the
    /// [`Self::not_yet`] names **its own** judgment asked about, keyed by the
    /// record's signature (a record is its own identity). Set at every
    /// judgment and dropped when the record paints, is refused or leaves the
    /// parked set — so a name asked for an earlier record never speaks for a
    /// later one parked only for want of a version
    /// ([`FaunaMlsBackend::moderation_unverified`]).
    waiting: HashMap<Vec<u8>, Vec<ActorId>>,
}

impl RoomSeats {
    /// Which of `names` a judgment that parks now is left waiting on.
    fn still_waiting(&self, names: &[ActorId]) -> Vec<ActorId> {
        names
            .iter()
            .filter(|name| self.not_yet.contains(name))
            .copied()
            .collect()
    }
}

/// Every owner and admin name `policies` carry, each once, owners first.
fn policy_names(policies: &[&fauna_mls::room_policy::RoomPolicy]) -> Vec<ActorId> {
    let mut names = Vec::new();
    for policy in policies {
        for name in std::iter::once(&policy.owner).chain(&policy.admins) {
            if !names.contains(name) {
                names.push(*name);
            }
        }
    }
    names
}

/// What this device makes of a floor delete record
/// ([`FaunaMlsBackend::floor_delete_verdict`]).
enum FloorDeleteVerdict {
    /// The record is its author's, for this room, and the author holds this
    /// rank in the **anchored** policy of the version the record names.
    Ranked(crate::room::RoomRole),
    /// Final: a bad signature, another room's record, or a named version that
    /// does not anchor. Nothing is painted, now or later.
    Refused,
    /// The named version could not be fetched this pass. Nothing is painted
    /// *yet*.
    NotYet,
}

/// Why a community room's record did or did not open this pass
/// ([`FaunaMlsBackend::room_generation_key`]) — the receive walk's answer to
/// "may the cursor step past it?".
enum RoomKeyLookup {
    /// The generation's key, opened.
    Key(GenerationKey),
    /// This device holds keys in the room, but was never wrapped into this
    /// generation (one it predates under a `history_policy` that retains
    /// nothing for joiners). Final — step past it.
    NotForUs,
    /// Nothing to open with *yet*: the room's key material could not be read
    /// this pass, or this account holds no wrap in the room at all — a newcomer
    /// not yet keyed in. Stop before the record; the next pass retries.
    NotYet,
    /// No generation-read or reception-key seam registered — the declared
    /// absence. Step past it: the record stays on the log for a session that
    /// can open it, and a feed wedged behind it would read nothing else.
    Unkeyable,
}

/// What a room mint does with the home nest's read
/// ([`FaunaMlsBackend::mint_room_generation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NestRead {
    /// Whatever the room's current generation says — the members' standing
    /// choice. Every rotation that is not *about* the nest's read keeps it.
    Keep,
    /// Wrap to the nest — an owner or admin restoring the read the members
    /// withdrew. (Founding needs no explicit grant: a room's first mint keeps
    /// the "unknown" answer, which wraps to every target the floor seats.)
    Grant,
    /// Leave the nest out — the members withdrawing the grant.
    Revoke,
}

/// A channel's message-log home, as this device knows it — the total encoding
/// behind [`FaunaMlsBackend::channel_home`]. Its three states (this enum's two
/// variants plus **absent from the map**) are what let the unauthenticated
/// pre-guard write fill only a genuinely unknown channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChannelHome {
    /// This device knows the channel drains **locally** (it created the group,
    /// or joined via a same-nest Welcome, or restored a slice that recorded it
    /// so). An explicit marker, distinct from absence: the pre-guard must not
    /// overwrite it with a peer-declared URL.
    SameNest,
    /// The channel's log lives on the named **foreign** nest; the drain relays
    /// its `channel.fetch` there and sends route via `channel.send_remote`.
    Foreign(String),
}

pub struct FaunaMlsBackend {
    /// Shared MLS engine for the local actor (encrypt/decrypt, group state).
    engine: Arc<MlsEngine>,
    /// The **retirement event** for this rail: `false` until [`RailBackend::retire`]
    /// hands the engine's role over, `true` forever after.
    ///
    /// An event and not a re-read of [`MlsEngine::is_retired`] because the one
    /// consumer is a loop parked in a `select!`
    /// ([`crate::ConversationsSession::start_receive_loop`]), and the ruling it
    /// serves is that a loop serving an account ends on an event, never at its
    /// next tick (`account-scoping.md` § Implementation status → the
    /// `tui (in-memory)` ledger row). The engine keeps its cheap `AtomicBool`
    /// for `ensure_live`'s per-call guard; this is the same fact, awaitable.
    ///
    /// Native-only, exactly like [`RailBackend::retire`]'s override below: the
    /// role lock is a file-backed-store concept and the wasm build spawns no
    /// loop to wake.
    #[cfg(not(target_arch = "wasm32"))]
    retired: tokio::sync::watch::Sender<bool>,
    /// Nest transport seam (channel send/fetch, keypackages, welcome).
    rpc: Arc<dyn ConversationsRpc>,
    /// The local actor's canonical `<handle>@<domain>` — a live cell read at
    /// use time (`conversations.md` § State & data shape → *Self-address: live,
    /// never baked*): the `SendOutcome` sender attribution, the reply-all
    /// self-drop (`RailBackend::self_address`), and the domain the data plane
    /// compares a peer's against to tell a same-nest peer from a foreign one
    /// (cross-nest key-package fetch / Welcome route through the federation
    /// relay — `docs/goal/architecture/federation.md`). A session built before
    /// identity resolution starts empty and heals through the session's one
    /// setter; nothing here is rebuilt for it.
    self_address: Arc<SelfAddress>,
    /// The local actor id (MLS framing carries the authoritative sender leaf;
    /// this is the app-level sender stamped into `ChannelMessage`).
    self_actor: ActorId,
    /// `ThreadId → ChannelId`. Backend-internal: clients never see a `ChannelId`
    /// (conversations.md § Architectural rules #2). Populated by group-create
    /// (Track B) and welcome-join (Track C); seeded directly in tests.
    channels: Mutex<HashMap<ThreadId, ChannelId>>,
    /// Per-channel app-level send sequence. The MLS framing carries the real
    /// crypto epoch/generation; `ChannelMessage.sequence` is informational
    /// metadata inside the sealed payload.
    seqs: Mutex<HashMap<ChannelId, u64>>,
    /// Handle domains (lower-cased) this account has POSITIVE evidence host a
    /// Fauna nest: every `TypedAddress::Fauna` participant the manager has shown
    /// this rail (`RailBackend::observe_participants`) and every foreign domain
    /// whose nest answered a resolve this session. Together with the local
    /// actor's own domain this is the *known Fauna domain* set
    /// `resolve_foreign` consults when a peer does not answer
    /// (`docs/goal/architecture/federation.md` § Peer-auth model →
    /// *Discovery-failure semantics*).
    ///
    /// Session-scoped, and the participant harvest rebuilds it from the thread
    /// store on every probe — which is exactly why an EMPTY set is not an
    /// answer on its own. The store is itself empty until the replica restore
    /// has fetched this account's history from the home nest, so read this set
    /// only through [`Self::domain_evidence`], which reports that window as
    /// [`DomainEvidence::Unloaded`] rather than as an absence.
    known_domains: Mutex<HashSet<String>>,
    /// Has this account's conversation history been loaded into the process yet
    /// (`fauna_client_mls_sync::orchestration::restore_and_wire`, via
    /// [`Self::mark_conversations_loaded`])? Until it has, `known_domains`'
    /// participant half is empty for a reason that says nothing about the
    /// account, and [`Self::domain_evidence`] must not report an absence.
    ///
    /// One-way, and never reset: a restore that landed cannot un-land, and a
    /// later session loads afresh into a fresh backend.
    conversations_loaded: AtomicBool,
    /// Channels joined from a [`WelcomeChannelKind::Scheduling`] welcome — the
    /// mailbox-less CalDAV iMIP rail (`caldav-server.md` § Server-side
    /// auto-schedule, Half-1). These are **not** bound to a thread (`channels`):
    /// a scheduling channel carries an iMIP, not chat, so [`poll_inbound_scheduling`]
    /// drives its application messages to the [`SchedulingSink`] (calendar-apply)
    /// instead of [`poll_inbound_conv`] routing them into a conversation. Populated
    /// by [`ingest_scheduling_welcome`]; iterated by the session's scheduling poll.
    scheduling_channels: Mutex<HashSet<ChannelId>>,
    /// Channels joined from a [`WelcomeChannelKind::Folder`] welcome — a
    /// cross-user shared folder the recipient auto-joined (`docs/goal/ui/folders.md`
    /// § Sharing). Like [`Self::scheduling_channels`] these are **not** bound to a
    /// thread (`channels`): a shared folder is not a conversation, so the join
    /// only binds the MLS group in the engine (for content-key/chunk reads — the
    /// daemon syncs the content, not the chat rail) and never materializes a chat
    /// thread. Populated by [`join_folder_welcome`]; its sole in-crate use is the
    /// idempotency guard that stops a re-delivered Welcome attempting a second
    /// (init-key-spending) join.
    folder_channels: Mutex<HashSet<ChannelId>>,
    /// The member-side content-key **custody-ingest** seam (Phase 0 — the read
    /// leg), injected after construction by the FFI/wasm factory (which holds the
    /// folders client + the folder-key store). When set, [`join_folder_welcome`]
    /// and [`poll_inbound_folder`] fetch + open + merge the owner's content-key
    /// envelope into this member's own custody so a *member* can decrypt the set's
    /// content; when unset (`OnceLock` empty — tests, wasm, or a build without the
    /// seam) ingest is a no-op and a member can list but not decrypt. Set once via
    /// [`Self::set_folder_custody_sink`] — the custody twin of [`Self::commit_gate`].
    folder_custody: OnceLock<Arc<dyn FolderCustodySink>>,
    /// Channels whose content-key custody this session has already ingested —
    /// RAM-only, reseeded empty each launch like the folder poll cursors
    /// (`session.rs` `folder_cursors`). Gates the poll-cadence retry (design
    /// D2): a quiet poll (no epoch advance) re-fetches only for a channel absent
    /// here, so a member whose ingest failed (owner had not published, or a
    /// lagging epoch) retries next pass, while a member already holding custody
    /// skips the network until the next rotation re-publishes the envelope.
    ///
    /// **Membership means "custody is current", not "custody was ingested once"
    /// this session** — [`Self::maybe_ingest_folder_custody`] *clears* the
    /// entry on any attempt that does not merge a bundle. Marking on success
    /// only (the shape until 2026-07-24) silently made the retry above
    /// unreachable for the member it matters most for: one that ingested at
    /// join, then failed a rotation-commit ingest, and could never fetch again.
    ingested_folders: Mutex<HashSet<ChannelId>>,
    /// `ChannelId → `[`ChannelHome`]` — the channel's message-log routing.
    /// `Foreign(url)` names the nest whose log the drain relays `channel.fetch`
    /// to (`docs/goal/behavior/direct-messages.md` § Technical Flow — Cross-Nest,
    /// step 3), learned from the cross-nest Welcome envelope's `nest_url`
    /// ([`ingest_welcome`] / [`ingest_scheduling_welcome`]) or seeded from a
    /// durable foreign-set record. `SameNest` is an **explicit** marker that
    /// this device KNOWS the channel drains locally.
    ///
    /// **Total encoding, and it is a security boundary**: the three-state split — `Foreign`, `SameNest`,
    /// **absent** (unknown, not yet stamped by a bind) — is what confines the unauthenticated
    /// pre-guard write ([`Self::record_channel_home_if_absent`], run before any
    /// MLS authentication of a re-delivered Welcome). That writer may fill only
    /// an **absent** entry, so a channel this device created or joined under
    /// current code — carrying an explicit `SameNest` marker — cannot be
    /// re-homed to a peer-declared URL. Before this was total a same-nest
    /// channel was merely absent, so every ordinary local DM and group was a
    /// hole the pre-guard would fill. The **absent** state remains only for
    /// channels persisted by a binary before the marker existed (the declared,
    /// bounded residual — `federation.md` § Cross-nest → route (a)); the
    /// nest-side verified-origin resolution (`federation_handlers.rs`) bounds
    /// even that plant to the sender's own verified nest — a bound on a URL,
    /// which is why a peer declaring **no** origin is refused earlier instead:
    /// a blank writes nothing at all, so the entry stays
    /// absent and correctable rather than pinned.
    channel_home: Mutex<HashMap<ChannelId, ChannelHome>>,
    /// The **device-owned-epoch commit gate** ([`CommitGate`]), injected by the
    /// per-app leg (slice 5) *after* construction because the gate's catch-up
    /// holds a `Weak` back to this backend (the cycle break — design
    /// `2026-07-05-mls-cross-device-state-sync-design.md` §3). When **set**, the
    /// commit-producing paths ([`Self::add_participant`], [`Self::remove_participant`])
    /// stage-gate-merge their commit through the rebase loop and
    /// [`Self::post_app_message`] posts a takeover self-update before an
    /// application send in a foreign epoch; when **unset** (`OnceLock` empty),
    /// the gate-less staged path stands — stage → send → merge-on-accept, per
    /// `devices.md` Rule 1 (no multi-device plane / single-device client —
    /// additive/bidirectional compat). Set once per backend instance via
    /// [`Self::set_commit_gate`].
    commit_gate: OnceLock<Arc<dyn CommitGate>>,
    /// The cross-device **processed-seq cursor** seam ([`ChannelCursor`]),
    /// injected by the slice-5 leg alongside [`Self::commit_gate`]. The
    /// background inbound poll ([`poll_inbound_conv`] via the receive loop's
    /// [`crate::session::ConversationsSession::poll_conversations`] /
    /// `start_receive_loop`) resumes each channel from `resume_seq` (the restored
    /// `history/<ch>` watermark, not `0`) and reports the seqs it folds back with
    /// `advance` so the next replica save snapshots the right watermark. **Unset**
    /// (single-device / no multi-device plane) ⇒ the loop keeps a fresh cursor
    /// from `0` each launch — today's behavior (additive compat).
    channel_cursor: OnceLock<Arc<dyn ChannelCursor>>,
    /// The awaited **durable history flush** seam ([`HistoryPersist`]), injected
    /// by the slice-5 leg alongside [`Self::commit_gate`] — `devices.md`
    /// § Durability rules Rule 3 (durable-before-done). [`Self::bootstrap_group`]
    /// drives it so a durable `provider` never lists a chat channel without an
    /// accompanying `history/<ch>` blob, and the manager drives it (via
    /// [`RailBackend::persist_history`]) after every own store mutation so the
    /// action completes only once the user-irrecoverable state is durable.
    /// **Unset** (single-device / no multi-device plane) ⇒ every flush no-ops.
    history_persist: OnceLock<Arc<dyn HistoryPersist>>,
    /// The awaited **durable provider flush** seam ([`ProviderPersist`]) — the
    /// Rule-3 analogue of [`Self::history_persist`] for a **key-package mint**.
    /// [`RailBackend::ensure_keypackages`] / [`RailBackend::ensure_last_resort_keypackage`]
    /// await it **between the mint and the upload** (via
    /// [`Self::persist_provider_before_publish`]), so a package's fresh private
    /// init keys are durable in the replica before the package is fetchable —
    /// `devices.md` § Durability rules Rule 3 (save-before-publish). A non-`true`
    /// result means the launch gate has not lifted (a mint before the
    /// cross-device restore) and the publish is refused. **Unset** ⇒ refused
    /// while [`Self::replica_restore_pending`] (the plane is built, its restore
    /// has not wired this seam yet); otherwise (single-device / no multi-device
    /// plane) the mint publishes directly (no replica exists that a swap could
    /// restore over the init keys).
    provider_persist: OnceLock<Arc<dyn ProviderPersist>>,
    /// Has a launch leg declared that a cross-device replica restore is coming
    /// ([`Self::expect_replica_restore`]) that has neither injected
    /// [`Self::provider_persist`] yet nor been abandoned
    /// ([`Self::abandon_replica_restore`])? While it is, a key-package mint
    /// refuses to publish: the restore swaps the engine's whole provider, so a
    /// package minted before it ships an init key no device will hold. Unset for
    /// a client with no plane at all (single-device, tests) — the direct publish
    /// stands.
    replica_restore_pending: AtomicBool,
    /// The **mid-session sibling-group adoption** seam ([`SiblingGroupAdopter`]),
    /// injected by the launch leg beside the four above. The receive sweep runs
    /// it once at its start so a group another of the user's devices joined is
    /// imported and bound before `bound_channels()` is walked. **Unset**
    /// (single-device / no multi-device plane) ⇒ the sweep skips it.
    sibling_group_adopter: OnceLock<Arc<dyn SiblingGroupAdopter>>,
    /// Per-channel async serialization lock (design §3 / slice 5). The MLS engine
    /// cannot process an inbound `Commit` while a **gated** send has a commit
    /// staged-but-unmerged on the same group (openMLS rejects a staged-pending
    /// overlap), so this serializes, per channel, the background inbound poll
    /// ([`poll_inbound_conv`], driven by the receive loop) against the
    /// device-owned-epoch gated branches ([`RailBackend::add_participant`] /
    /// [`RailBackend::remove_participant`] and the takeover in
    /// [`Self::post_app_message`]). A `futures_util::lock::Mutex` — async and
    /// **wasm-safe** (this type builds on both targets; never `tokio::sync::Mutex`).
    /// ⚠ The gate's own catch-up ([`CommitGate`]'s rebase) drains
    /// [`poll_inbound_conv`] *inside* the gated critical section, so that inner
    /// call must **not** re-take this lock — only the background poll and the gated
    /// branches take it (never [`poll_inbound_conv`] itself). Lazily created per
    /// channel; unused (never contended) when no gate is injected.
    channel_locks: Mutex<HashMap<ChannelId, Arc<futures_util::lock::Mutex<()>>>>,
    /// Channels the Rule-2 heal already rewound this session — at most one
    /// rewind + re-walk per channel per process, the un-processable-commit loop
    /// guard (`devices.md` § Cross-device MLS group-state sync, Rule 2; see
    /// [`poll_inbound_conv`]'s heal block).
    rewound_channels: Mutex<HashSet<ChannelId>>,
    /// The in-group succession-statement verifier ([`SuccessionWitness`]),
    /// injected by the session layer because the anchor sources the
    /// verification rule needs (cached chain heads, the anchored chain walk)
    /// live above this crate. **Unset** ⇒ every statement degrades to the bare
    /// add, the same rendering an older client gets — never trusted.
    succession_witness: OnceLock<Arc<dyn SuccessionWitness>>,
    /// The **roster-report** seam ([`RoomRosterReporter`]): after every
    /// membership commit this device authors on a governed room, the
    /// resulting roster is reported to the room's home nest
    /// (`conversation-rooms.md` § The floor roster). **Unset** ⇒ the report
    /// is tallied and dropped — the declared gap until the nest's report
    /// kind exists.
    room_roster_reporter: OnceLock<Arc<dyn RoomRosterReporter>>,
    /// The **roster-read** seam ([`RoomRosterReader`]): the id-keyed handle
    /// read that names a member this device has never met
    /// (`conversation-rooms.md` § Implementation status today, the roster
    /// bullet). **Unset** ⇒ such a member keeps its elided actor id, which is
    /// the honest pre-existing fallback rather than a failure.
    room_roster_reader: OnceLock<Arc<dyn RoomRosterReader>>,
    /// The **generation-read** seam ([`RoomGenerationReader`]): a community
    /// room's key material, as far as this caller is entitled to see it.
    /// **Unset** ⇒ no `RoomSealed` record opens, which is the declared absence
    /// the class carried before any app called a room kind rather than a
    /// failure.
    room_generation_reader: OnceLock<Arc<dyn RoomGenerationReader>>,
    /// The **group-reception key** seam ([`GroupReceptionKeys`]): the
    /// account-plane keypairs whose secret halves open those wraps, and the
    /// write door that mints this account its first one. **Unset** ⇒ the same
    /// honest skip on the read, and a named refusal on
    /// [`FaunaMlsBackend::found_community_room`].
    group_reception_keys: OnceLock<Arc<dyn GroupReceptionKeys>>,
    /// The **room-ceremony** seam ([`RoomCeremonyRpc`]): founding a community
    /// room and publishing its generations. **Unset** ⇒ founding is refused
    /// by name — a room founded on a device that cannot key it would be a
    /// room nobody, its founder included, could ever send into.
    room_ceremony: OnceLock<Arc<dyn RoomCeremonyRpc>>,
    /// Opened room generation keys, per channel, keyed by generation id — the
    /// result of one `read_generations` + unwrap pass.
    ///
    /// Cached because a room's log is walked record by record and the wraps
    /// are the *same* for every record in a page: without this, a page of 50
    /// bubbles would be 50 nest round trips and 50 X-Wing decapsulations. A
    /// generation is immutable once minted (its id is content-derived from the
    /// mint core), so a cached key can never go stale — only *incomplete*,
    /// which is what [`FaunaMlsBackend::room_generation_key`]'s miss path
    /// refreshes.
    room_generations: Mutex<HashMap<ChannelId, HashMap<[u8; 32], GenerationKey>>>,
    /// The community rooms whose walk last stopped **waiting for its key-in**
    /// ([`RoomKeyLookup::NotYet`]) — what [`RoomSnapshot::awaiting_key`] paints,
    /// and how the walk recognises the first key after a wait.
    ///
    /// That recognition is load-bearing for the content index, not cosmetic:
    /// a room waiting for its key-in no longer holds the account's Conversation
    /// catch-up boundary open (`content-index-ingest.md` § Ingest triggers, v1
    /// → *A community room waiting for its key-in*), so by the time the key
    /// lands the boundary has usually closed — and everything behind the stop is
    /// the room's backlog, which must reach the index as catch-up rather than
    /// trickle. The walk therefore stops once more, before the first record the
    /// new key opens, so [`poll_inbound_conv`]'s caller can reopen the window
    /// before any of it is ingested ([`ConvPollOutcome::keyed_in`]).
    ///
    /// In-memory and per session, like the key cache beside it: a relaunch walks
    /// every room from its watermark inside the launch window anyway.
    awaiting_key_rooms: Mutex<HashSet<ChannelId>>,
    /// Per channel, what a **successful** roster read has said about each
    /// actor it was asked about ([`RosterAnswer`]). It bounds the read to
    /// membership events rather than to the poll tick: a member the read
    /// named is done for the session, a newly-seated member is unanswered and
    /// so asks immediately, and a member the roster *listed without a name*
    /// — one homed on another nest whose own home nest has not yet announced
    /// its handle (`conversation-rooms.md` § Implementation status today, the
    /// roster bullet) — is asked again at a widening gap of polls, because
    /// that name can still arrive: it rides the member's own next drain.
    ///
    /// A *failed* read records nothing, so a nest that was down when the
    /// member arrived is retried on the next poll rather than written off for
    /// the session.
    roster_reads: Mutex<HashMap<ChannelId, HashMap<ActorId, RosterAnswer>>>,
    /// What the floor roster says about a channel that has **no MLS group** —
    /// the only channels whose class this device cannot decide locally.
    ///
    /// `room_state` is synchronous (it renders a `ThreadDetail`), so it cannot
    /// ask the nest; and for a group-less channel it has nothing local to go
    /// on. "No group" is ambiguous exactly once: a **community** room has none
    /// by construction (it is born by `room.create`, never `bootstrap_group`),
    /// and so does an MLS room this device has not joined — and rendering the
    /// second as `Community` would tell the user their room's home nest reads
    /// it when it does not. The floor settles it, because it is the floor's
    /// *principal kinds* that define the class (§ Architectural rules, rule 1).
    ///
    /// Filled by [`FaunaMlsBackend::resolve_nameless_members`] on the poll
    /// pass, which already makes this read; absent until then, which renders
    /// as today's answer rather than a guess. Bounded to group-less channels,
    /// so an MLS room — whose class this device decides locally and correctly —
    /// costs no read at all.
    room_floors: Mutex<HashMap<ChannelId, CachedFloor>>,
    /// The invitations pending on each community room that this account may
    /// withdraw, as the home nest last served them
    /// ([`Self::refresh_pending_room_invites`]) — what
    /// [`RoomSnapshot::pending_invites`] renders. Absent until a list has been
    /// served, and again once the nest stops serving one; never an empty list
    /// standing in for an answer this device does not have. Beside
    /// [`Self::room_floors`] rather than on [`CachedFloor`] because the two are
    /// separate reads: a floor read replaces its whole projection, and a
    /// withdrawal re-lists without reading the floor.
    room_pending_invites: Mutex<HashMap<ChannelId, Vec<crate::backend::PendingRoomInvite>>>,
    /// Each community room's **anchored policy chain** — the signed policy
    /// versions this device has itself proven, from the founder's version 1
    /// (bound to the room id) link by link
    /// ([`fauna_mls::room_policy::CommunityPolicyChain`]). The only policy a
    /// floor delete record's author is ever ranked under
    /// ([`Self::floor_delete_verdict`]): [`Self::room_floors`]' policy is
    /// verified only as *somebody's* bytes, which renders a name and grants
    /// nothing. Session memory — a relaunch re-proves the chain.
    room_policy_chains: Mutex<HashMap<ChannelId, fauna_mls::room_policy::CommunityPolicyChain>>,
    /// What this device has learned, per community room, about whom its
    /// policies' names have since become ([`RoomSeats`]) — the succession
    /// lines the registered [`SuccessionWitness`] verified, which is the
    /// designation [`Self::floor_delete_verdict`] judges under. Session memory,
    /// like the chain beside it.
    room_seats: Mutex<HashMap<ChannelId, RoomSeats>>,
    /// Floor delete records this device could not judge **yet** — the named
    /// policy version did not arrive — kept so a later pass paints the
    /// tombstone the walk has already stepped past
    /// ([`Self::retry_parked_floor_deletes`]). Bounded per room
    /// ([`MAX_PARKED_FLOOR_DELETES`]); a record refused outright is never
    /// parked. **Not** session memory, unlike its three neighbours: the walk
    /// advanced the durable cursor past each of these before judging it, so a
    /// record still parked at quit would never be met again — the live set
    /// is mirrored at rest through the manager
    /// (`ConversationsManager::park_floor_delete` / `set_parked_floor_deletes`,
    /// stamped into `history/<ch>`), and a restored copy is drained back here
    /// on the first pass over the room.
    parked_floor_deletes: Mutex<HashMap<ChannelId, Vec<ParkedFloorDelete>>>,
    /// Every identity the peer-anchor harvest has **settled without an
    /// anchor** this session — found nothing, refused, or spent its budget —
    /// as announced through [`settle_parked_successions`]; a seed
    /// ([`redrive_parked_successions`]) is an anchor, and takes its name back
    /// out. What tells a parked floor delete record's *not yet* from its
    /// *cannot*: a name it waits on ([`RoomSeats::waiting`]) that the harvest
    /// settled without an anchor will not anchor before the session ends
    /// ([`Self::moderation_unverified`]). Session memory, like the harvest's
    /// own once-per-session guard.
    harvest_settled: Mutex<HashSet<ActorId>>,
    /// Every identity the peer-anchor harvest has **spoken for** this session
    /// — settled by ANY arm: seeded ([`redrive_parked_successions`]) or
    /// settled without an anchor ([`settle_parked_successions`]) — where
    /// [`Self::harvest_settled`] keeps only the anchorless half. What the
    /// folder commit walk's hold reads ([`FaunaMlsBackend::folder_walk_waits_on`]):
    /// once the sweep has spoken for a channel's recorded owner, nothing this
    /// session will change a still-refused statement's verdict, so the walk
    /// waits no longer. Session memory, like its neighbour.
    harvest_spoken_for: Mutex<HashSet<ActorId>>,
    /// Whether a peer-anchor harvest sweep runs this session
    /// ([`FaunaMlsBackend::arm_succession_harvest_wait`]) — the one condition
    /// under which the folder commit walk may wait behind a parked statement,
    /// because the sweep's settle is what ends the wait. Fails open like the
    /// witness's own wait: never armed, never held.
    harvest_armed: AtomicBool,
    /// Group-less channels a **confirmed** floor read has said this account
    /// holds no live seat on — a member this device removed, or was itself
    /// removed from. Distinct from [`Self::room_floors`], which a confirmed
    /// unseating never clears (the last-known roster still renders until this
    /// device re-navigates away): this is the one *seated?* fact
    /// [`Self::room_invitations`] can trust, because [`Self::tend_community_room`]
    /// only ever inserts here on [`crate::backend::RoomRosterRead::NoFloor`],
    /// never on [`crate::backend::RoomRosterRead::Unavailable`] (a transient
    /// read failure must never be mistaken for a confirmed removal), and
    /// removes the entry the moment a later read comes back seated again — a
    /// re-admission after a removal is exactly the case  exists for.
    /// Absent (the default for every channel this session has not yet tended)
    /// reads as "presumed still seated", which is today's behavior and the
    /// safe default: an unconfirmed guess must never unlock a settle this
    /// early return would otherwise perform.
    unseated_rooms: Mutex<HashSet<ChannelId>>,
    /// Polls until each community room's floor is read again
    /// ([`FaunaMlsBackend::tend_community_room`]); absent or `0` means due.
    /// A community room's membership changes on its home nest — an invitee
    /// accepting, a member leaving, another admin rotating the nest out — and
    /// no walk this device folds carries any of it, so the floor is the only
    /// place to see it. Counted in polls, not seconds
    /// (`e2e-conventions.md` convention 14).
    floor_refresh: Mutex<HashMap<ChannelId, u32>>,
    /// Channels this session has already weighed for a floor-roster backfill
    /// ([`FaunaMlsBackend::backfill_floor_roster`]) — the once-per-channel
    /// memo that keeps a room whose birth report never landed from asking on
    /// every poll forever. A channel is inserted BEFORE the read, so an
    /// answerless read costs one attempt, not one per poll — the same reason
    /// [`Self::tend_community_room`] restarts its countdown whether or not the
    /// read lands.
    floor_backfill: Mutex<HashSet<ChannelId>>,
    /// What became of the roster reports this session owed
    /// ([`RosterReportCounts`]).
    roster_reports: RosterReportTally,
    /// Ownership offers addressed to this identity, parked by the inbound poll
    /// for [`Self::complete_ownership_offer_locked`] — the incoming owner's
    /// half of the transfer ceremony (`conversation-rooms.md` § Roles and
    /// authorization → *Ownership transfer*). Parked rather than committed
    /// in place because a gated commit's own catch-up re-enters the poll;
    /// the driver completes it after the walk, under the same channel lock.
    /// Latest per channel wins: an offer supersedes an earlier one the owner
    /// abandoned, and the version check at completion drops a stale one.
    parked_ownership_offers: Mutex<HashMap<ChannelId, RoomOwnershipOffer>>,
    /// The **outgoing** owner's mirror of the above: the policy version this
    /// identity last offered the room at, per channel. The offer itself is
    /// parked only on the *incoming* owner's device
    /// ([`Self::park_ownership_offer`] returns unless the offered policy names
    /// that identity), so without this the one seat that must act on a refused
    /// hand-over — "the owner offers again" (`conversation-rooms.md` § Roles
    /// and authorization → *Ownership transfer*) — is the one seat holding no
    /// record that it offered at all. Written by [`Self::offer_ownership`] once
    /// the offer is really on the channel; read and cleared by
    /// [`notice_superseded_own_offer`] at this device's own commit fold.
    /// Session-scoped and RAM-only: an offer does not survive a relaunch, and
    /// neither should a notice about one.
    pending_own_offers: Mutex<HashMap<ChannelId, u64>>,
    /// What this session's inbound poll did with in-group succession statements
    /// ([`SuccessionStatementCounts`]) — the member-side half of a diagnosis the
    /// witness's own report cannot give, because a statement the poll never
    /// reached never reaches the witness either.
    succession_statements: SuccessionStatementTally,
    /// Witness-refused statements retained for the harvest re-drive
    /// (`identity-succession.md` § The succession statement → *the
    /// peer-profile harvest*). Keyed by the statement's `old_actor_id`, latest
    /// per thread; **admitted only for a current snapshot participant of the
    /// bound thread** (only a roster row can ever be re-pointed, and the bound
    /// caps what an in-group forger of arbitrary `old_actor_id`s
    /// can make this hold at roster size). Session-lifetime by design: an
    /// at-rest variant was considered and rejected in the ratification — it
    /// would hand a forged statement durable per-session re-drive work plus an
    /// expiry policy, to close a seconds-wide race whose harm is rendering
    /// continuity only.
    parked_successions:
        Mutex<HashMap<ActorId, HashMap<ThreadId, fauna_core::recovery::SignedIdentitySuccession>>>,
    /// The **folder rail's** park store — witness-refused statements a FOLDER
    /// channel carried ([`route_folder_succession`]), keyed by `old_actor_id`
    /// then channel (a folder channel has no thread; `federation.md` § Cross-nest
    /// shared folders + channel append → *The marker follows the owner's
    /// verified succession*). Admitted **only where the statement's
    /// `old_actor_id` IS the channel's recorded folder owner** — the one marker
    /// a re-drive could ever re-point, the folder twin of the participant-row
    /// bound above, so an in-group forger of arbitrary `old_actor_id`s holds
    /// this at one slot per channel.
    ///
    /// **Mirrored at rest, unlike the store above** — the engine's at-rest
    /// folder park (`MlsEngine::park_folder_succession`), drained into this map
    /// by each launch's first walk over the channel
    /// ([`Self::load_rested_folder_park`]), so the folder commit walk's hold
    /// behind a parked statement survives the session (`federation.md`
    /// § Cross-nest shared folders + channel append → *The folder commit walk
    /// inherits the harvest wait*). The conversations rail keeps its park
    /// session-lifetime (`identity-succession.md` § The succession statement →
    /// *the harvest wait* (e), "Parking keeps its roster bound and session
    /// lifetime"; the at-rest variant it rejected under *What a statement may
    /// cost the member who receives it* would have been forger-minted work
    /// every later session pays, plus an expiry policy, to close a
    /// seconds-wide race). The folder rail diverges because what its park
    /// guards is a forked seat, not that race, and neither cost carries over:
    /// the harvest speaking for the owner forgets the rested copy whatever it
    /// decides — that settle is the expiry — so a forgery costs at most one
    /// hold window per launch, and one slot per channel.
    parked_folder_successions:
        Mutex<HashMap<ActorId, HashMap<ChannelId, fauna_core::recovery::SignedIdentitySuccession>>>,
    /// Channels whose at-rest folder park this session has already drained
    /// into [`Self::parked_folder_successions`] — the once-per-channel memo
    /// (like [`Self::floor_backfill`]) that makes the drain the launch's first
    /// walk, never every walk.
    folder_park_loaded: Mutex<HashSet<ChannelId>>,
    /// Per-channel count of inbound commits this device **folded in** — one bump
    /// per [`CommitApplyOutcome::Advanced`] returned by [`apply_inbound_commit`],
    /// and nothing else bumps it (see [`Self::folded_commits`] for why that
    /// exclusivity is what the observable rests on). Session-scoped and RAM-only,
    /// like [`Self::rewound_channels`]: it answers "has this *process* folded a
    /// commit since I last looked", which is the only question a barrier asks.
    folded_commits: Mutex<HashMap<ChannelId, u64>>,
    /// The **custody-ceremony ingest** seam ([`CustodyCeremonySink`], W8.4 (account-data-plane.md § Workstreams) —
    /// `account-data-plane.md` § Replica posture → *The custody grant +
    /// ceremony*): the inbound poll hands every
    /// [`ChannelMessageBody::Custody`] record's verbatim bytes here as a
    /// thread effect, never a bubble. Injected by the glue layer (which owns
    /// the ceremony machine + the account store — the
    /// [`FolderCustodySink`] priority-#2 pattern); **unset** ⇒ the record is
    /// skipped and tallied, and the payload waits in the channel history for
    /// a capable session (the same degradation an older build's failed
    /// decode gives). Set once via [`Self::set_custody_ceremony_sink`].
    custody_sink: OnceLock<Arc<dyn CustodyCeremonySink>>,
    /// What this session's inbound poll did with custody-ceremony payloads
    /// ([`CustodyPayloadCounts`]) — the same convention-6 self-diagnosis the
    /// succession tally carries: every arm below degrades to a silent no-op,
    /// so which arm ran is the only diagnosis there is.
    custody_payloads: CustodyPayloadTally,
    /// The same tally for custody **receipts**, kept separate on purpose: a
    /// receipt that fails to verify is a different fact from a ceremony step
    /// that fails to capture, and one counter for both would hide a lying or
    /// misconfigured custodian behind a CAS-hiccup number.
    custody_receipts: CustodyPayloadTally,
    /// Ingest seam for share-set endpoint advertisements (slice F): the
    /// inbound poll hands every [`ChannelMessageBody::ShareEndpoints`]
    /// record's verbatim bytes here as a thread effect, never a bubble.
    /// Injected by the glue layer (which owns the account-plane write and the
    /// `fauna-peer-share` binding); **unset** ⇒ the record is skipped and
    /// tallied, and this session simply caches no peer candidates for that
    /// set — the nest stays the always-on source. Set once via
    /// [`Self::set_share_endpoints_sink`].
    share_endpoints_sink: OnceLock<Arc<dyn ShareEndpointsSink>>,
    /// What this session's inbound poll did with endpoint advertisements.
    /// Its own tally rather than a shared one: `uncaptured` here means an
    /// advertisement was **refused** — a member lying about who it is — and
    /// that is a security signal, not a storage hiccup. Collapsing it into a
    /// custody counter would bury exactly the number worth watching.
    share_endpoints: CustodyPayloadTally,
}

/// The receive-side tally of in-group succession statements, session-scoped.
///
/// Convention 6 ("failures must diagnose themselves") for a path with **no
/// rendered evidence of its own**: a statement that stalled behind an
/// un-incorporated Commit, one that failed to decrypt, one that arrived with no
/// witness registered and one a witness refused all leave the participant row
/// exactly as it was. Splitting them from outside used to need a rebuild — no
/// app installs a tracing subscriber, so the `debug!` lines on this path are
/// unreadable in production.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SuccessionStatementCounts {
    /// `GroupMetaMessage::Succession` bodies the poll decrypted. **Zero while
    /// the channel demonstrably grew is the decisive reading**: the statement
    /// never got as far as this arm (a stalled Commit walk, a failed decrypt),
    /// so nothing downstream of it is implicated.
    pub seen: u64,
    /// Bodies that did not decode as a `SignedIdentitySuccession` — a wire-shape
    /// break, not a trust decision.
    pub undecodable: u64,
    /// Bodies observed with no witness registered. Every one degrades to the
    /// bare add, correctly but silently; on an app that believes it wired a
    /// witness this is the whole bug.
    pub no_witness: u64,
    /// Statements a witness verified and this backend therefore re-pointed a
    /// row for — the success arm, counted so a green run proves the path ran
    /// rather than proving nothing happened. Re-drive re-points count here
    /// too: a re-point is a re-point, whenever the anchor arrived.
    pub repointed: u64,
    /// Statements parked for a later re-drive: cumulative parks this session,
    /// **both reasons** — the witness refused for now
    /// (`identity-succession.md` § The succession statement → *the
    /// peer-profile harvest*), or the roster-pair gate is still waiting for
    /// this group's remove-old commit. `parked > 0` with `repointed == 0`
    /// reads as "neither re-drive has fired yet"; the two arm counters below
    /// say which wait it is.
    pub parked: u64,
    /// Verified statements the roster-pair gate **held**: the successor is
    /// seated here but the predecessor's leaf has not gone yet — the honest
    /// ceremony's own midpoint, since the statement rides alongside the add
    /// and the remove-old commit lands after it. Transient by construction:
    /// the commit re-drive settles each one when that commit arrives, so a
    /// count that stays up while the channel grows is the ceremony stalling
    /// half-finished.
    pub awaiting_remove_old: u64,
    /// Verified statements **dropped** because the successor holds no leaf in
    /// this group at all — the ceremony never ran here, so the statement was
    /// replayed rather than carried. Not an error on its own (a member may
    /// forward a true statement anywhere), but the count a seed thief's
    /// replay shows up in, and it must never become a re-point.
    pub not_in_this_group: u64,
    /// Folder commit records the walk **held** — stopped before, undecrypted,
    /// unmemoized — because a parked statement naming the channel's recorded
    /// owner was still waiting on the harvest (`federation.md` § Cross-nest
    /// shared folders + channel append → *The marker follows the owner's
    /// verified succession*). One bump per held record per pass, so a count
    /// that climbs while `parked` stays up reads as "the set's commit rail is
    /// waiting on the sweep to settle its owner" — and a count that keeps
    /// climbing after the sweep should have spoken is the sweep never reaching
    /// that owner.
    pub held_commits: u64,
}

#[derive(Debug, Default)]
struct SuccessionStatementTally {
    seen: std::sync::atomic::AtomicU64,
    undecodable: std::sync::atomic::AtomicU64,
    no_witness: std::sync::atomic::AtomicU64,
    repointed: std::sync::atomic::AtomicU64,
    parked: std::sync::atomic::AtomicU64,
    awaiting_remove_old: std::sync::atomic::AtomicU64,
    not_in_this_group: std::sync::atomic::AtomicU64,
    held_commits: std::sync::atomic::AtomicU64,
}

impl SuccessionStatementTally {
    fn snapshot(&self) -> SuccessionStatementCounts {
        use std::sync::atomic::Ordering::Relaxed;
        SuccessionStatementCounts {
            seen: self.seen.load(Relaxed),
            undecodable: self.undecodable.load(Relaxed),
            no_witness: self.no_witness.load(Relaxed),
            repointed: self.repointed.load(Relaxed),
            parked: self.parked.load(Relaxed),
            awaiting_remove_old: self.awaiting_remove_old.load(Relaxed),
            not_in_this_group: self.not_in_this_group.load(Relaxed),
            held_commits: self.held_commits.load(Relaxed),
        }
    }
}

/// What a successful floor-roster read said about one actor this device asked
/// about — the per-actor state behind `FaunaMlsBackend::roster_reads`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RosterAnswer {
    /// The roster named the member, and the name is seated. Done **while the
    /// member stays named**: a named member is not nameless, so it never reaches
    /// the read again. It is NOT done for the session — a member removed and
    /// re-admitted is re-seated handle-less (the reconcile's add arm), and the
    /// read must then ask about them afresh, which is why a `Named` answer met
    /// for an actor rendering nameless is treated as stale.
    Named,
    /// The roster listed the member without a name — homed on another nest,
    /// its own home nest's announce not yet landed. Asked again once `skip`
    /// more polls have passed; `misses` counts the nameless answers so far
    /// and sets the next gap.
    Nameless { misses: u32, skip: u32 },
    /// The roster's reply did not mention the member AT ALL — never "no
    /// handle", since this device routinely folds a membership commit before
    /// the committing device's report of it reaches the nest
    /// (`resolve_nameless_members`, the `listed.get` match). The first
    /// omission is a free re-ask (`skip == 0`): the report is often seconds
    /// away, and paying a gap for it would slow the common case for no
    /// reason. Only the SECOND and later consecutive omissions open the same
    /// doubling gap a [`Self::Nameless`] answer uses — `misses` counts
    /// consecutive omissions, `skip` the polls left to sit out. Cleared
    /// wholesale for the channel on its next advanced commit
    /// (`FaunaMlsBackend::clear_omitted_roster_reads`, called from
    /// `reconcile_roster`), because that is when a stalled report is due.
    Omitted { misses: u32, skip: u32 },
}

impl RosterAnswer {
    /// The state after the `misses`-th consecutive nameless answer: the next
    /// read waits `2^(misses-1)` polls, capped at
    /// [`FaunaMlsBackend::NAMELESS_REASK_CAP`].
    fn nameless(misses: u32) -> Self {
        let skip = 1u32
            .checked_shl(misses.saturating_sub(1))
            .unwrap_or(FaunaMlsBackend::NAMELESS_REASK_CAP)
            .min(FaunaMlsBackend::NAMELESS_REASK_CAP);
        Self::Nameless { misses, skip }
    }

    /// The state after the `misses`-th consecutive omission: the first
    /// (`misses == 1`) is free (`skip == 0`); the next read then waits
    /// `2^(misses-2)` polls, capped at
    /// [`FaunaMlsBackend::NAMELESS_REASK_CAP`] — the same curve
    /// [`Self::nameless`] uses, started one omission later.
    fn omitted(misses: u32) -> Self {
        let skip = if misses <= 1 {
            0
        } else {
            1u32.checked_shl(misses - 2)
                .unwrap_or(FaunaMlsBackend::NAMELESS_REASK_CAP)
                .min(FaunaMlsBackend::NAMELESS_REASK_CAP)
        };
        Self::Omitted { misses, skip }
    }
}

/// What became of the floor-roster reports this session owed
/// (`conversation-rooms.md` § The floor roster → *End-to-end rooms*), the
/// [`SuccessionStatementCounts`] shape: a report is never rendered, so this
/// is its only evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RosterReportCounts {
    /// Membership commits this device authored on a governed room — each
    /// owes one report.
    pub owed: u64,
    /// Reports with no [`RoomRosterReporter`] registered — dropped. **This
    /// is the declared gap until the nest's report kind exists**; on a
    /// session that believes it wired the seam it is the whole bug.
    pub no_reporter: u64,
    /// Reports the home nest stored.
    pub delivered: u64,
    /// Reports the reporter could not deliver.
    pub undelivered: u64,
    /// Reports the home nest took but did **not** apply: its floor already
    /// held a report at or above the position this one named
    /// (`conversation-rooms.md` § The floor roster). Counted inside
    /// `delivered`, not beside it — the report reached the home; what it did
    /// not do is become the floor. Non-zero on a device whose own commit's
    /// report lost its position is the fingerprint worth reading.
    pub superseded: u64,
}

#[derive(Debug, Default)]
struct RosterReportTally {
    owed: std::sync::atomic::AtomicU64,
    no_reporter: std::sync::atomic::AtomicU64,
    delivered: std::sync::atomic::AtomicU64,
    undelivered: std::sync::atomic::AtomicU64,
    superseded: std::sync::atomic::AtomicU64,
}

impl RosterReportTally {
    fn snapshot(&self) -> RosterReportCounts {
        use std::sync::atomic::Ordering::Relaxed;
        RosterReportCounts {
            owed: self.owed.load(Relaxed),
            no_reporter: self.no_reporter.load(Relaxed),
            delivered: self.delivered.load(Relaxed),
            undelivered: self.undelivered.load(Relaxed),
            superseded: self.superseded.load(Relaxed),
        }
    }
}

/// The receive-side tally of custody-ceremony payloads, session-scoped —
/// convention 6 for a path with no rendered evidence of its own (a ceremony
/// payload is never a bubble), the [`SuccessionStatementCounts`] shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CustodyPayloadCounts {
    /// `ChannelMessageBody::Custody` bodies the poll decrypted. Zero while
    /// the counterpart demonstrably posted one means the record never got
    /// this far (stalled Commit walk, failed decrypt).
    pub seen: u64,
    /// Bodies observed with no sink registered — each waits in the channel
    /// history for a capable session; on an app that believes it wired the
    /// ceremony glue this is the whole bug.
    pub no_sink: u64,
    /// Payloads the sink durably captured — the success arm, counted so a
    /// green run proves the path ran.
    pub captured: u64,
    /// Payloads the sink could not capture (a transient account-store write
    /// failure, a refused/unverifiable payload). Healed by the per-launch
    /// re-walk and, ultimately, the ceremony's decay-to-re-offer.
    pub uncaptured: u64,
}

#[derive(Debug, Default)]
struct CustodyPayloadTally {
    seen: std::sync::atomic::AtomicU64,
    no_sink: std::sync::atomic::AtomicU64,
    captured: std::sync::atomic::AtomicU64,
    uncaptured: std::sync::atomic::AtomicU64,
}

impl CustodyPayloadTally {
    fn snapshot(&self) -> CustodyPayloadCounts {
        use std::sync::atomic::Ordering::Relaxed;
        CustodyPayloadCounts {
            seen: self.seen.load(Relaxed),
            no_sink: self.no_sink.load(Relaxed),
            captured: self.captured.load(Relaxed),
            uncaptured: self.uncaptured.load(Relaxed),
        }
    }
}

/// Resolves once the [`FaunaMlsBackend`] it was taken from has had its engine's
/// conversations-engine role handed over ([`RailBackend::retire`]) — the signal a
/// background task riding that engine ends on, at the hand-over rather than at
/// its next tick.
///
/// Distinct from [`crate::ConversationsSession::closed`] on purpose. That one
/// asks "has my holder let go of me", which a shell answers only on the path it
/// remembers; this one asks "is my engine still the one serving this account",
/// which the factory answers for every app at one seam
/// (`account-data-plane.md` § Multi-instance concurrency → *The role is HANDED
/// OVER in-process*). A failed successor build leaves the first question
/// answered "no" and the second answered "yes" — the strand this exists to end.
#[cfg(not(target_arch = "wasm32"))]
pub struct EngineRetired(tokio::sync::watch::Receiver<bool>);

#[cfg(not(target_arch = "wasm32"))]
impl EngineRetired {
    /// Wait for the hand-over; returns at once if it already happened.
    pub async fn wait(&mut self) {
        // `Err` means the backend itself is gone, which is a retirement by a
        // blunter route — either way the rider must stop, so both resolve.
        let _ = self.0.wait_for(|retired| *retired).await;
    }

    /// Whether the hand-over has already happened.
    pub fn is_retired(&self) -> bool {
        *self.0.borrow()
    }
}

impl FaunaMlsBackend {
    /// Standalone construction over a fixed address (tests, receive-only
    /// wirings). Session/manager wirings share one live cell across rails via
    /// [`Self::new_shared`].
    pub fn new(
        engine: Arc<MlsEngine>,
        rpc: Arc<dyn ConversationsRpc>,
        self_handle: impl Into<String>,
        self_actor: ActorId,
    ) -> Self {
        Self::new_shared(engine, rpc, SelfAddress::new(self_handle), self_actor)
    }

    /// An [`EngineRetired`] for this rail — take one per background task that
    /// rides the engine, and `select!` on it beside the task's own wake.
    ///
    /// The counterpart of [`crate::ConversationsSession::closed`]: that one fires
    /// when the *holder* lets go, this one when the *role* is handed over. A
    /// hand-over runs before the successor engine is built, so it happens whether
    /// or not that build then succeeds — which is the whole reason a loop needs
    /// both signals rather than the drop alone.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn retired_watch(&self) -> EngineRetired {
        EngineRetired(self.retired.subscribe())
    }

    /// Construction over the session's shared live cell — one
    /// `set_self_address` then heals this rail together with SMTP. The
    /// logged-in user's handle is canonical `localpart@domain`
    /// (`docs/goal/behavior/login.md` — a handle carries its `@domain`), so
    /// this nest's handle domain is derived from the cell on read.
    pub fn new_shared(
        engine: Arc<MlsEngine>,
        rpc: Arc<dyn ConversationsRpc>,
        self_address: Arc<SelfAddress>,
        self_actor: ActorId,
    ) -> Self {
        Self {
            engine,
            #[cfg(not(target_arch = "wasm32"))]
            retired: tokio::sync::watch::channel(false).0,
            rpc,
            self_address,
            self_actor,
            channels: Mutex::new(HashMap::new()),
            seqs: Mutex::new(HashMap::new()),
            known_domains: Mutex::new(HashSet::new()),
            conversations_loaded: AtomicBool::new(false),
            scheduling_channels: Mutex::new(HashSet::new()),
            folder_channels: Mutex::new(HashSet::new()),
            folder_custody: OnceLock::new(),
            ingested_folders: Mutex::new(HashSet::new()),
            channel_home: Mutex::new(HashMap::new()),
            commit_gate: OnceLock::new(),
            channel_cursor: OnceLock::new(),
            history_persist: OnceLock::new(),
            sibling_group_adopter: OnceLock::new(),
            provider_persist: OnceLock::new(),
            replica_restore_pending: AtomicBool::new(false),
            channel_locks: Mutex::new(HashMap::new()),
            rewound_channels: Mutex::new(HashSet::new()),
            succession_witness: OnceLock::new(),
            room_roster_reporter: OnceLock::new(),
            room_roster_reader: OnceLock::new(),
            room_generation_reader: OnceLock::new(),
            room_ceremony: OnceLock::new(),
            group_reception_keys: OnceLock::new(),
            room_generations: Mutex::new(HashMap::new()),
            awaiting_key_rooms: Mutex::new(HashSet::new()),
            roster_reads: Mutex::new(HashMap::new()),
            room_floors: Mutex::new(HashMap::new()),
            room_pending_invites: Mutex::new(HashMap::new()),
            room_policy_chains: Mutex::new(HashMap::new()),
            room_seats: Mutex::new(HashMap::new()),
            parked_floor_deletes: Mutex::new(HashMap::new()),
            harvest_settled: Mutex::new(HashSet::new()),
            harvest_spoken_for: Mutex::new(HashSet::new()),
            harvest_armed: AtomicBool::new(false),
            unseated_rooms: Mutex::new(HashSet::new()),
            floor_refresh: Mutex::new(HashMap::new()),
            floor_backfill: Mutex::new(HashSet::new()),
            roster_reports: RosterReportTally::default(),
            parked_ownership_offers: Mutex::new(HashMap::new()),
            pending_own_offers: Mutex::new(HashMap::new()),
            succession_statements: SuccessionStatementTally::default(),
            parked_successions: Mutex::new(HashMap::new()),
            parked_folder_successions: Mutex::new(HashMap::new()),
            folder_park_loaded: Mutex::new(HashSet::new()),
            folded_commits: Mutex::new(HashMap::new()),
            custody_sink: OnceLock::new(),
            custody_payloads: CustodyPayloadTally::default(),
            custody_receipts: CustodyPayloadTally::default(),
            share_endpoints_sink: OnceLock::new(),
            share_endpoints: CustodyPayloadTally::default(),
        }
    }

    /// Per-channel count of inbound commits this device has **folded in** this
    /// session, keyed by channel hex — `fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`
    /// owns the cross-app state-key contract; this getter owns the reasoning.
    ///
    /// **The question it answers.** "Has this device incorporated the epoch
    /// transition another member (or another of my own devices) just published?"
    /// Nothing else on a client can answer it. The obvious proxy — *did I render
    /// their message* — is unavailable in the one case that matters most, twin
    /// devices of the same actor: they share one leaf, and a sender cannot
    /// MLS-decrypt its own application messages
    /// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync,
    /// which is why own history rides the `history/<ch>` replica rather than log
    /// replay). So device A can *never* see device B's message text, however
    /// long it waits — but it does process B's **commit**, and that fold-in is
    /// exactly this count.
    ///
    /// **Contract, identical on every app that publishes it.** Starts empty for
    /// a fresh app process; a channel's entry only ever increases; a channel with
    /// no fold-in yet is absent (which a consumer reads as 0). One bump per
    /// [`CommitApplyOutcome::Advanced`] — i.e. per commit that actually advanced
    /// this device's epoch, whether by a plain `process_commit` of a foreign
    /// member's commit or by the own-leaf resync arm converging onto a sibling
    /// device's takeover. **Both rails count** (chat and folder share
    /// [`apply_inbound_commit`]), which is deliberate: the count means "this
    /// channel's ratchet moved under me", not "the chat poll ran".
    ///
    /// ⚠ **A `Stalled` outcome must NOT bump, and that is the whole soundness
    /// argument.** `Stalled` is precisely the case where the transition was
    /// *not* incorporated (an own-leaf commit whose resync failed, a future-epoch
    /// strand), and a barrier that released on it would hand the test the state
    /// it exists to rule out — a send that blind-appends at a stale epoch instead
    /// of taking the epoch over. Hence the single bump site in
    /// [`apply_inbound_commit`]'s wrapper: every arm's verdict flows through one
    /// comparison, so a future arm cannot quietly acquire a bump.
    ///
    /// **Why a count rather than the epoch number** (`MlsEngine::current_epoch`
    /// is right there): the epoch is a value a *restore* can also produce, so
    /// "epoch > baseline" cannot distinguish "I folded in their commit" from "I
    /// reloaded a replica that was already ahead". A count read before the
    /// trigger and waited past cannot false-pass that way — the value it must
    /// reach did not exist yet (`e2e-conventions.md` convention 14, the same
    /// argument `ACTIVATION_GESTURES_KEY` makes against a flag).
    pub fn folded_commits(&self) -> BTreeMap<String, u64> {
        self.folded_commits
            .lock()
            .unwrap()
            .iter()
            .map(|(ch, n)| (ch.to_string(), *n))
            .collect()
    }

    /// The one bump site for [`Self::folded_commits`], called by
    /// [`apply_inbound_commit`] and nowhere else.
    fn note_folded_commit(&self, channel_id: &ChannelId) {
        *self
            .folded_commits
            .lock()
            .unwrap()
            .entry(*channel_id)
            .or_insert(0) += 1;
    }

    /// What this session's inbound poll did with in-group succession statements
    /// — see [`SuccessionStatementCounts`] for how to read it.
    pub fn succession_statement_counts(&self) -> SuccessionStatementCounts {
        self.succession_statements.snapshot()
    }

    /// Park a witness-refused statement for the harvest re-drive. Latest per
    /// `(old_actor, thread)` wins — statements are idempotent and only the
    /// terminal pair matters, so one slot per row is enough.
    fn park_succession(
        &self,
        thread_id: ThreadId,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) {
        let old = statement.statement.old_actor_id;
        self.parked_successions
            .lock()
            .unwrap()
            .entry(old)
            .or_default()
            .insert(thread_id, statement);
        self.succession_statements
            .parked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Take every parked statement for `old_actor` out of the store. The
    /// re-drive re-inserts what the witness still refuses — fill-empty only,
    /// so a statement parked while the re-drive ran is never displaced by the
    /// older one it is re-considering.
    fn take_parked_for(
        &self,
        old_actor: &ActorId,
    ) -> Vec<(ThreadId, fauna_core::recovery::SignedIdentitySuccession)> {
        self.parked_successions
            .lock()
            .unwrap()
            .remove(old_actor)
            .map(|per_thread| per_thread.into_iter().collect())
            .unwrap_or_default()
    }

    /// Take every parked statement whose row lives in `thread_id`, across
    /// every predecessor — the **commit** re-drive's cut of the park store,
    /// where [`Self::take_parked_for`] is the **harvest**'s. The two re-drives
    /// answer different questions ("this peer's anchor arrived" vs "this
    /// group's roster moved"), so each cuts the store along its own axis.
    ///
    /// Empties a predecessor's map when its last row goes, so the store does
    /// not accumulate empty shells for peers whose statements all settled.
    fn take_parked_in_thread(
        &self,
        thread_id: &ThreadId,
    ) -> Vec<fauna_core::recovery::SignedIdentitySuccession> {
        let mut taken = Vec::new();
        self.parked_successions
            .lock()
            .unwrap()
            .retain(|_old, per_thread| {
                if let Some(statement) = per_thread.remove(thread_id) {
                    taken.push(statement);
                }
                !per_thread.is_empty()
            });
        taken
    }

    /// Re-insert a still-refused statement — but only into an empty slot (see
    /// [`Self::take_parked_for`]).
    fn repark_succession_if_empty(
        &self,
        thread_id: ThreadId,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) {
        let old = statement.statement.old_actor_id;
        self.parked_successions
            .lock()
            .unwrap()
            .entry(old)
            .or_default()
            .entry(thread_id)
            .or_insert(statement);
    }

    /// The folder rail's [`Self::park_succession`]: latest per
    /// `(old_actor, channel)` wins. Counted in the shared tally's `parked`.
    /// Mirrored to the engine's at-rest folder park (one slot per channel —
    /// the admission bound makes the owner the only `old_actor` one channel
    /// can park), so the hold survives the session.
    fn park_folder_succession(
        &self,
        channel_id: ChannelId,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) {
        match fauna_core::encoding::canonical_encode(&statement) {
            Ok(bytes) => self.engine.park_folder_succession(&channel_id, &bytes),
            Err(e) => tracing::warn!(
                channel = %channel_id,
                "folder succession statement not rested (encode failed): {e}"
            ),
        }
        let old = statement.statement.old_actor_id;
        self.parked_folder_successions
            .lock()
            .unwrap()
            .entry(old)
            .or_default()
            .insert(channel_id, statement);
        self.succession_statements
            .parked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// The harvest re-drive's cut of the folder park store
    /// ([`Self::take_parked_for`]'s twin).
    fn take_parked_folder_for(
        &self,
        old_actor: &ActorId,
    ) -> Vec<(ChannelId, fauna_core::recovery::SignedIdentitySuccession)> {
        self.parked_folder_successions
            .lock()
            .unwrap()
            .remove(old_actor)
            .map(|per_channel| per_channel.into_iter().collect())
            .unwrap_or_default()
    }

    /// The commit re-drive's cut of the folder park store
    /// ([`Self::take_parked_in_thread`]'s twin, keyed by channel).
    fn take_parked_folder_in_channel(
        &self,
        channel_id: &ChannelId,
    ) -> Vec<fauna_core::recovery::SignedIdentitySuccession> {
        let mut taken = Vec::new();
        self.parked_folder_successions
            .lock()
            .unwrap()
            .retain(|_old, per_channel| {
                if let Some(statement) = per_channel.remove(channel_id) {
                    taken.push(statement);
                }
                !per_channel.is_empty()
            });
        taken
    }

    /// Drain `channel_id`'s at-rest folder park into the RAM store — once per
    /// channel per session, ahead of the launch's first walk over it, and only
    /// into an empty slot (a statement this session parked itself is newer).
    /// An undecodable rested copy is forgotten: it can hold nothing.
    fn load_rested_folder_park(&self, channel_id: &ChannelId) {
        if !self.folder_park_loaded.lock().unwrap().insert(*channel_id) {
            return;
        }
        let Some(bytes) = self.engine.parked_folder_succession(channel_id) else {
            return;
        };
        match fauna_core::encoding::canonical_decode::<fauna_core::recovery::SignedIdentitySuccession>(
            &bytes,
        ) {
            Ok(statement) => self.repark_folder_succession_if_empty(*channel_id, statement),
            Err(_) => {
                self.engine.forget_parked_folder_succession(channel_id);
            }
        }
    }

    /// Forget every at-rest folder park naming `old_actor` — the harvest has
    /// spoken for that identity, so no later launch may hold on it again. An
    /// undecodable rested copy goes too.
    fn forget_rested_folder_parks_naming(&self, old_actor: &ActorId) {
        for (channel_id, bytes) in self.engine.parked_folder_successions() {
            if rested_statement_names(&bytes, old_actor) {
                self.engine.forget_parked_folder_succession(&channel_id);
            }
        }
    }

    /// Forget `channel_id`'s at-rest folder park if it names `old_actor` — the
    /// channel's walk dropped that statement, and a later owner's rested one
    /// (the slot is latest-wins) is not this drop's to forget.
    fn forget_rested_folder_park_naming(&self, channel_id: &ChannelId, old_actor: &ActorId) {
        if self
            .engine
            .parked_folder_succession(channel_id)
            .is_some_and(|bytes| rested_statement_names(&bytes, old_actor))
        {
            self.engine.forget_parked_folder_succession(channel_id);
        }
    }

    /// [`Self::repark_succession_if_empty`] for the folder park store.
    fn repark_folder_succession_if_empty(
        &self,
        channel_id: ChannelId,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) {
        let old = statement.statement.old_actor_id;
        self.parked_folder_successions
            .lock()
            .unwrap()
            .entry(old)
            .or_default()
            .entry(channel_id)
            .or_insert(statement);
    }

    /// Grant the Rule-2 cursor rewind for `channel` at most once per session:
    /// `true` exactly on the first call per channel. See [`poll_inbound_conv`]'s
    /// heal block for the full rationale.
    fn grant_cursor_rewind(&self, channel: &ChannelId) -> bool {
        self.rewound_channels
            .lock()
            .expect("lock poisoned")
            .insert(*channel)
    }

    /// Inject the [`CommitGate`] — the per-app leg (slice 5) calls this once,
    /// after both this backend and the gate are constructed (the gate's catch-up
    /// holds a `Weak` back here, so the backend must exist first — the cycle
    /// break). Idempotent: a second call is ignored (the `OnceLock` keeps the
    /// first). A backend with no gate set drives the gate-less staged commit
    /// path (stage → send → merge-on-accept, `devices.md` Rule 1).
    ///
    /// The one-way latch is deliberate and load-bearing: "engaged ⇒ stays engaged" is what makes a not-engaged gate probe a
    /// safe negative signal for the folder removal seam — do NOT make this
    /// re-settable. The discard is warned so a mis-injected second gate leaves
    /// a trace instead of vanishing silently.
    pub fn set_commit_gate(&self, gate: Arc<dyn CommitGate>) {
        if self.commit_gate.set(gate).is_err() {
            tracing::warn!(
                "set_commit_gate: a CommitGate is already latched for this backend; \
                 ignoring the second injection (the OnceLock keeps the first — \
                 backends are per-session, a second set means a wiring bug)"
            );
        }
    }

    /// The injected [`CommitGate`], or `None` when unset (single-device / no
    /// multi-device plane) — then the commit paths take the gate-less staged
    /// route (stage → send → merge-on-accept, `devices.md` Rule 1).
    /// `pub` so the folder removal seam (`fauna-client-folders`'s
    /// `FolderRemovalGate` adapter on `Arc<FaunaMlsBackend>`) can route the
    /// owner's Remove commit through the same gate the chat rail uses
    /// (`devices.md` § Cross-device MLS group-state sync, Rule 1).
    pub fn commit_gate(&self) -> Option<&Arc<dyn CommitGate>> {
        self.commit_gate.get()
    }

    /// Inject the [`ChannelCursor`] seam — the slice-5 leg calls this once,
    /// alongside [`Self::set_commit_gate`], after the `MlsStateSync` it wraps has
    /// `load`ed (so `resume_seq` already reflects the restored watermarks).
    /// Idempotent (the `OnceLock` keeps the first). Unset ⇒ the poll resumes from
    /// `0` each launch.
    pub fn set_channel_cursor(&self, cursor: Arc<dyn ChannelCursor>) {
        let _ = self.channel_cursor.set(cursor);
    }

    /// Register the member content-key **custody-ingest** seam (Phase 0 — the read
    /// leg). Injected once after construction by the FFI/wasm factory (which holds
    /// the folders client + folder-key store); idempotent (the `OnceLock`
    /// keeps the first). A backend with none never ingests custody — a member lists
    /// a shared set but cannot decrypt its bytes (the pre-Phase-0 behaviour).
    pub fn set_folder_custody_sink(&self, sink: Arc<dyn FolderCustodySink>) {
        let _ = self.folder_custody.set(sink);
    }

    /// Register the in-group succession-statement verifier
    /// ([`SuccessionWitness`] — see its doc for the trust rule). Set once per
    /// backend instance by the session layer (first registration wins, like the
    /// other seams). A backend with none renders every succession pair as the
    /// bare add — a backend with no witness wired — never as trusted continuity.
    pub fn set_succession_witness(&self, witness: Arc<dyn SuccessionWitness>) {
        let _ = self.succession_witness.set(witness);
    }

    /// Register the floor-roster report seam ([`RoomRosterReporter`]). Set
    /// once by the session layer (first registration wins, like the other
    /// seams). A backend with none tallies every owed report as
    /// [`RosterReportCounts::no_reporter`] and drops it — the declared gap
    /// until the nest's report kind exists.
    pub fn set_room_roster_reporter(&self, reporter: Arc<dyn RoomRosterReporter>) {
        let _ = self.room_roster_reporter.set(reporter);
    }

    /// The widest gap, in polls, between two reads about a member the floor
    /// roster keeps listing without a name. The gap starts at one poll and
    /// doubles per nameless answer, so a foreign member whose home nest never
    /// announces costs one read per this many polls in the
    /// steady state, while one whose first drain — the announce's carrier —
    /// comes late is named within a bounded number of polls after it. An
    /// event count, deliberately not a wall-clock interval
    /// (`e2e-conventions.md` convention 14).
    pub const NAMELESS_REASK_CAP: u32 = 32;

    /// Register the floor-roster **read** seam ([`RoomRosterReader`]) — the
    /// id-keyed handle read that names a member this device has never met.
    /// Set once by the session layer (first registration wins, like the other
    /// seams). A backend with none leaves such members rendering as their
    /// elided actor id, which is the honest fallback the read improves on
    /// rather than a gap it repairs.
    pub fn set_room_roster_reader(&self, reader: Arc<dyn RoomRosterReader>) {
        let _ = self.room_roster_reader.set(reader);
    }

    /// Register the community class's **generation-read** seam
    /// ([`RoomGenerationReader`]). Set once by the session layer, like the
    /// other seams. A backend with none opens no `RoomSealed` record — the
    /// declared absence the class carried before any app called a room kind.
    pub fn set_room_generation_reader(&self, reader: Arc<dyn RoomGenerationReader>) {
        let _ = self.room_generation_reader.set(reader);
    }

    /// Register the **group-reception key** seam
    /// ([`GroupReceptionKeys`]) — the account-plane keypairs whose secret
    /// halves open a room's generation wraps. Set once by the session layer.
    pub fn set_group_reception_keys(&self, keys: Arc<dyn GroupReceptionKeys>) {
        let _ = self.group_reception_keys.set(keys);
    }

    /// Register the **room-ceremony** seam ([`RoomCeremonyRpc`]). Set once by
    /// the session layer, like the other seams.
    pub fn set_room_ceremony(&self, ceremony: Arc<dyn RoomCeremonyRpc>) {
        let _ = self.room_ceremony.set(ceremony);
    }

    /// Register all four nest-backed room seams at once ([`RoomSeams`]) —
    /// the one call every glue site makes, so none can wire a subset.
    pub fn set_room_seams(&self, seams: RoomSeams) {
        let RoomSeams {
            reporter,
            reader,
            generations,
            ceremony,
        } = seams;
        self.set_room_roster_reporter(reporter);
        self.set_room_roster_reader(reader);
        self.set_room_generation_reader(generations);
        self.set_room_ceremony(ceremony);
    }

    /// Found a **community room** and bind it to `thread_id` — the class's
    /// birth ceremony as an app performs it
    /// (`conversation-rooms.md` § Implementation status today, the
    /// *A community room can be founded* bullet).
    ///
    /// Four acts, in this order, and the order is the whole design:
    ///
    /// 1. **Resolve this account's wrap target**, minting and persisting one
    ///    when the account holds none. Persisting comes first because the
    ///    public half is about to go out in the birth record: a crash between
    ///    handing it over and persisting the secret would leave the room
    ///    keying itself to a key this account will not have after the next
    ///    launch (`AccountRuntimeHandle::put_group_reception_key`'s own
    ///    record-then-act rule). An account that already holds one reuses its
    ///    newest — the *current* wrap target, and reusing it is what lets one
    ///    account be a member of many rooms without a key per room.
    /// 2. **`room.create`** with a fresh random salt and this device's signed
    ///    initial policy. The reply's room id is **re-derived and checked**
    ///    rather than trusted: the id commits to whose key founded the room
    ///    (`fauna_mls::room_policy::derive_room_id`), so accepting a different
    ///    one would bind this thread to a room the founder's own key does not
    ///    name.
    /// 3. **Key it.** A founded room has no generation at all — the nest
    ///    admits mints and never performs one — so a room left unkeyed here
    ///    could not be sent into by anybody, its founder included. The roster
    ///    read back is what the mint wraps to: the founder and the home nest,
    ///    the latter being the materialization grant the members can later
    ///    revoke by rotating it out (§ *The home nest's read, and its
    ///    revoke*). First mint, so no parent.
    /// 4. **Bind, home and persist**, exactly as [`Self::bootstrap_group`]
    ///    does for the end-to-end class: the channel is same-nest by
    ///    construction, and the empty-string home marker is recorded rather
    ///    than left absent for that method's stated reason.
    ///
    /// The thread is *not* created here — the caller owns thread creation, as
    /// it does for a group the send path bootstraps. What this returns is the
    /// channel the room's log lives on, already bound.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered (a device that
    /// cannot reach the account plane cannot found a keyable room), when the
    /// reception key cannot be persisted, when the nest refuses the ceremony,
    /// or when the id it answers is not the one this founder's key derives.
    pub async fn found_community_room(
        &self,
        thread_id: ThreadId,
        name: Option<String>,
    ) -> Result<ChannelId, BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot found a community room — it has no room-ceremony seam".to_string(),
            )
        })?;
        let reception = self.current_reception_key().await?;

        // A binding salt: through the room id it commits the room to the room
        // signature on every policy version above 1, so its home nest can
        // never serve a version lifted from another room this founder owns
        // (`fauna_mls::room_policy::RoomBinding`).
        let mut entropy = [0u8; 24];
        getrandom::fill(&mut entropy)
            .map_err(|e| BackendError::Internal(format!("birth salt entropy: {e}")))?;
        let salt = fauna_mls::room_policy::binding_birth_salt(&entropy);
        let expected = fauna_mls::room_policy::derive_room_id(&self.self_actor, &salt)
            .map_err(|e| BackendError::Internal(format!("derive room id: {e}")))?;
        let policy = fauna_mls::room_policy::RoomPolicy::initial(self.self_actor, name);
        let signed = self
            .engine
            .sign_room_policy_community(&expected, &policy)
            .map_err(|e| BackendError::Internal(format!("sign room policy: {e}")))?;
        let policy_bytes = fauna_core::encoding::canonical_encode(&signed)
            .map_err(|e| BackendError::Internal(format!("encode room policy: {e}")))?;
        let reception_pubkey =
            self_check_reception_pubkey(reception.reception_pubkey().map_err(|e| {
                BackendError::Internal(format!(
                    "this account's group-reception key is corrupt: {e}"
                ))
            })?)?;
        let room_hex = ceremony
            .room_create(hex::encode(salt), policy_bytes, reception_pubkey)
            .await?;
        let answered = hex::decode(&room_hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .ok_or_else(|| {
                BackendError::Internal(format!(
                    "the room ceremony answered a malformed room id: {room_hex}"
                ))
            })?;
        if answered != expected {
            return Err(BackendError::Internal(
                "the room ceremony answered an id this founder's own key does not derive"
                    .to_string(),
            ));
        }
        let channel_id = ChannelId(expected);

        // The home marker goes down BEFORE the mint, not with the binding: the
        // mint's roster read makes the same-nest-vs-relay pick off it, and a
        // room founded by this device is same-nest by construction. Recording
        // it explicitly rather than leaving it absent is `bootstrap_group`'s
        // own reason — an absent entry reads the same as "unknown", which a
        // hostile re-delivered Welcome's pre-guard write could then re-home.
        // Harmless if the ceremony below fails: a home marker for a channel
        // nothing is bound to is inert.
        self.record_channel_home(channel_id, "");

        // Key the room before binding it: a bound-but-unkeyed room renders as
        // a thread whose every send fails, which is worse than a founding that
        // failed outright and can be retried.
        // `Keep` on a room with no tip yet wraps to every target the floor
        // seats, the home nest included — the grant founding a community room
        // makes. Not `Grant`, which is the toggle's explicit act and refuses a
        // floor that seats no nest; a founding wraps to whoever is there.
        self.mint_room_generation(&channel_id, Vec::new(), NestRead::Keep)
            .await?;

        self.bind_channel(thread_id, channel_id);
        if let Some(persist) = self.history_persist.get()
            && let Err(e) = persist.persist_channel(channel_id).await
        {
            tracing::warn!("founding history persist failed (the debounced autosave retries): {e}");
        }
        Ok(channel_id)
    }

    /// This account's **current** group-reception keypair — its wrap target —
    /// minting and durably persisting one when the account holds none.
    ///
    /// The newest held record is the current target
    /// (`AccountRuntimeHandle::group_reception_keys` serves newest first), and
    /// reusing it is deliberate: the scheme addresses wraps to *the account*
    /// rather than to a membership, so one key covers every room this account
    /// sits in and a rotation is one act rather than one per room.
    ///
    /// A failed persist is a hard error rather than a degrade, because the
    /// alternative is handing a room a public half whose secret never reached
    /// disk.
    async fn current_reception_key(
        &self,
    ) -> Result<fauna_core::group_generation::GroupReceptionKeyRecord, BackendError> {
        let keys = self.group_reception_keys.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot key a community room — it has no group-reception key seam"
                    .to_string(),
            )
        })?;
        if let Some(existing) = keys.reception_keys().await.into_iter().next() {
            return Ok(existing);
        }
        let minted = fauna_core::group_generation::GroupReceptionKeyRecord::mint(
            Timestamp::now_millis() as i64,
        );
        if !keys.put_reception_key(minted.clone()).await {
            return Err(BackendError::Internal(
                "this device could not persist its group-reception keypair — the room seating is \
                 abandoned rather than keyed to a secret that never reached disk"
                    .to_string(),
            ));
        }
        Ok(minted)
    }

    /// Read the room's floor, or fail with the reason a keying act cannot
    /// proceed without it.
    ///
    /// Every keying act reads the floor **now** rather than trusting anything
    /// this device remembers: coverage is judged at the nest against the floor
    /// as it stands, so a minter wrapping to a cached roster is refused the
    /// moment a membership changes under it.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the roster-read seam is unregistered or
    /// the floor cannot be read. An *empty* floor is not an error — it is a
    /// legitimate answer, and the callers that cannot use one say so
    /// themselves.
    async fn read_room_floor(
        &self,
        channel_id: &ChannelId,
    ) -> Result<crate::backend::RoomFloor, BackendError> {
        let reader = self.room_roster_reader.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot read a room's floor — it has no roster-read seam".to_string(),
            )
        })?;
        reader
            .read_roster(channel_id.to_string(), self.channel_home_url(channel_id))
            .await
            .or_absent()
            .ok_or_else(|| {
                BackendError::Internal(
                    "this room's floor roster could not be read — a keying act has to cover the \
                     floor as it stands, so there is nothing safe to wrap to"
                        .to_string(),
                )
            })
    }

    /// Invite `invitee` into the community room `channel_id` hosts, as a plain
    /// member or an admin (`conversation-rooms.md` § Join rules and invites).
    ///
    /// The invitation is **this device's own signed act**, and nothing else
    /// happens here: an invite does not seat anybody, which is the one place
    /// the room plane deliberately diverges from the group plane. The invitee
    /// accepts, the home nest writes the roster row, and only then can the
    /// inviter key them in ([`Self::key_in_room_member`]) — three steps that
    /// cannot be collapsed, because a wrap names the roster entry the
    /// *seating* derived and that entry does not exist until acceptance.
    ///
    /// `invitee_node` is the invitee's home nest as this device knows it;
    /// `None` means this nest.
    ///
    /// The nest binds the signature to this authenticated caller, judges the
    /// caller's rank against the room's join rule, refuses an admin invitation
    /// the current policy does not already name, and gates the invitee's own
    /// reach policy. None of that is pre-checked here: they are the floor's
    /// decisions, and a device that guessed at them would refuse invitations
    /// the room would have allowed. What *is* checked here is the record's own
    /// shape — `RoomInvite::validate` refuses the `Owner` role, a room having
    /// exactly one owner that moves by transfer rather than by invitation.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the ceremony seam is unregistered or
    /// the invitation does not sign; the nest's own refusal otherwise.
    pub async fn invite_to_room(
        &self,
        channel_id: &ChannelId,
        invitee: ActorId,
        role: fauna_mls::room_policy::RoomRole,
        invitee_node: Option<String>,
    ) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot invite into a room — it has no room-ceremony seam".to_string(),
            )
        })?;
        // The version an invitation records is "the one I read the join rule
        // under", so it comes from the floor read that *served* the roles —
        // never from a cache, which could name a policy under which this
        // inviter's own rank was different. A policy-less room carries none; 0 is
        // no version at all (they start at 1) and reads as "unstated", which
        // is what it is.
        let policy_version = self
            .read_room_floor(channel_id)
            .await?
            .policy_version
            .unwrap_or(0);
        let signed = self
            .engine
            .sign_room_invite(&fauna_mls::room_policy::RoomInvite {
                room_id: channel_id.0.to_vec(),
                invitee,
                role,
                policy_version,
            })
            .map_err(|e| BackendError::Internal(format!("sign room invite: {e}")))?;
        let bytes = fauna_core::encoding::canonical_encode(&signed)
            .map_err(|e| BackendError::Internal(format!("encode room invite: {e}")))?;
        // The room's recorded home routes the act, as it routes every other
        // ceremony of a room this account is a foreign member of (the leave,
        // the roster relays): a foreign home means this device issues through
        // its own nest's relay, under `member-invite`, and the room's home
        // judges it (`conversation-rooms.md` § Join rules and invites → *A
        // cross-nest invitation*, the foreign-inviter leg).
        ceremony
            .room_invite(
                bytes,
                invitee_node.unwrap_or_default(),
                self.channel_home_url(channel_id),
            )
            .await?;
        Ok(())
    }

    /// Every invitation standing for this account, **verified** — the read that
    /// makes a room joinable at all (`conversation-rooms.md` § Join rules and
    /// invites). Each answers the room id [`Self::accept_room_invite`] takes, so
    /// no human ever reads one out of a log.
    ///
    /// **The signature is checked here, at the reader.** The nest binds an
    /// invitation's signer to the authenticated caller on the way in, but this
    /// record is what crossed the nest boundary, and it is signed precisely so
    /// the crossing does not have to be trusted. An invitation whose signature
    /// does not verify under the inviter it names — or that does not decode, or
    /// whose shape `RoomInvite::validate` refuses — is **dropped, not
    /// surfaced**: it names a room and an inviter, and rendering either before
    /// the bytes are proven would let the delivery path put words in a
    /// principal's mouth. Dropping is honest rather than lossy: the standing
    /// truth is the home nest's own `room_invites` row, so a genuine invitation
    /// survives a re-delivery, while a forged one has nothing behind it.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the ceremony seam is unregistered; the
    /// nest's own refusal otherwise. A malformed invitation is not an error —
    /// one bad record must not hide the good ones beside it.
    pub async fn pending_room_invitations(&self) -> Result<Vec<RoomInvitation>, BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot discover room invitations — it has no room-ceremony seam"
                    .to_string(),
            )
        })?;
        let mut out = Vec::new();
        for pending in ceremony.room_pending_invitations().await? {
            let Ok(signed) = fauna_core::encoding::canonical_decode::<
                fauna_mls::room_policy::SignedRoomInvite,
            >(&pending.signed_invite) else {
                continue;
            };
            if signed.verify_signature().is_err() {
                continue;
            }
            // `verify_signature` runs `RoomInvite::validate` first, so the id is
            // known to be 32 bytes by the time we are here.
            let Ok(room_id) = <[u8; 32]>::try_from(signed.invite.room_id.as_slice()) else {
                continue;
            };
            out.push(RoomInvitation {
                id: pending.id,
                room_id,
                inviter: signed.inviter,
                role: signed.invite.role.into(),
                policy_version: signed.invite.policy_version,
                room_node: pending.room_node,
            });
        }
        Ok(out)
    }

    /// Settle one standing invitation — after accepting it, or on a decline.
    ///
    /// Declining is exactly this and nothing more: the invitation stops
    /// standing for this account. It does not tell the room, because a refusal
    /// the inviter could read would make declining an act with an audience;
    /// what the room holds is a pending row its own members can see, and an
    /// invitee who never accepts leaves it pending.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the ceremony seam is unregistered; the
    /// nest's own refusal otherwise.
    pub async fn settle_room_invitation(&self, id: i64) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal("this app has no room-ceremony seam".to_string())
        })?;
        ceremony.room_settle_invitation(id).await?;
        Ok(())
    }

    /// Accept an invitation into the community room `room_id` names, and bind
    /// it to `thread_id` — the act that **seats** this account on the floor.
    ///
    /// The mirror of [`Self::found_community_room`], in the same order and for
    /// the same reason: this account's wrap target is resolved and durably
    /// persisted **before** its public half goes out, because the roster row
    /// and the wrap target are one fact. A seating that recorded a key this
    /// device cannot open would be a membership nothing could ever read, and
    /// re-admission would not repair it — a re-admitted member returns on a
    /// *fresh* roster entry, so wraps addressed to the old one stay bound to a
    /// slot it no longer holds.
    ///
    /// It deliberately does **not** key the room: a joiner has no key
    /// authority, and an add never mints
    /// (`account-data-taxonomy.md` § The recipient-set scheme → *Mint
    /// triggers*). What covers a newcomer is the inviter's own
    /// [`Self::key_in_room_member`], so a freshly accepted room reads nothing
    /// until that lands. That is honest rather than broken: how much of the
    /// room's past a newcomer may read is the `history_policy`'s decision, not
    /// the joiner's.
    ///
    /// `room_node` is the invitation's own ([`RoomInvitation::room_node`]):
    /// `Some(url)` names a room homed on **another nest**, and then the
    /// acceptance is relayed through this account's own nest to that home and
    /// the channel is recorded as foreign-homed there, so every later read,
    /// send and roster relay of this room picks its `_remote` kind off the one
    /// `ChannelHome` signal (`conversation-rooms.md` § Join rules and invites
    /// → *A cross-nest invitation*).
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered or the reception
    /// key cannot be persisted; the nest's own refusal otherwise (no pending
    /// invitation, one overtaken by a removal, or one that LAPSED — the nest
    /// judges an invitation again at accept and consumes one its inviter could
    /// no longer issue, `conversation-rooms.md` § Join rules and invites).
    pub async fn accept_room_invite(
        &self,
        thread_id: ThreadId,
        room_id: [u8; 32],
        room_node: Option<String>,
    ) -> Result<ChannelId, BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot join a community room — it has no room-ceremony seam".to_string(),
            )
        })?;
        let reception = self.current_reception_key().await?;
        let reception_pubkey =
            self_check_reception_pubkey(reception.reception_pubkey().map_err(|e| {
                BackendError::Internal(format!(
                    "this account's group-reception key is corrupt: {e}"
                ))
            })?)?;
        let channel_id = ChannelId(room_id);
        let room_node = room_node.filter(|u| !u.is_empty());
        ceremony
            .room_accept_invite(channel_id.to_string(), reception_pubkey, room_node.clone())
            .await?;
        // The home is recorded explicitly either way, for
        // `found_community_room`'s reason: a blank URL is an explicit
        // same-nest mark, and the invitation's `room_node` — which the
        // acceptance just rode to — is the foreign home every later relay of
        // this room (fetch, send, roster, generations, leave) is routed by.
        self.record_channel_home(channel_id, room_node.as_deref().unwrap_or(""));
        self.bind_channel(thread_id, channel_id);
        if let Some(persist) = self.history_persist.get()
            && let Err(e) = persist.persist_channel(channel_id).await
        {
            tracing::warn!("join history persist failed (the debounced autosave retries): {e}");
        }
        Ok(channel_id)
    }

    /// Cover a newly seated member of `channel_id` with the room's **tip**
    /// generation — the add side of the scheme's mint triggers.
    ///
    /// Called by the **inviter** once the invitee's acceptance has put them on
    /// the floor. Until it lands the newcomer holds no wrap and reads nothing,
    /// which is the honest state rather than a failure.
    ///
    /// **The tip alone, deliberately.** A room's `history_policy` decides how
    /// much of the retained bundle a newcomer may have, and no client can read
    /// a room's policy: the roster read serves roles and a version, and the
    /// room plane has no policy-get door at all. The tip is the one generation
    /// every policy authorizes, and it is what "the newcomer can read this
    /// room from now on" needs. A `full`-history room therefore backfills less
    /// than it could until a policy read exists — visible as a newcomer whose
    /// scrollback starts at their seating, never as a refusal, because a batch
    /// naming one unauthorized generation is refused **whole** and would leave
    /// the newcomer with nothing at all.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered, this device
    /// holds no key for the room's tip (it cannot hand out what it cannot
    /// open), or the target is not a keyable member of the floor; the nest's
    /// own refusal otherwise.
    pub async fn key_in_room_member(
        &self,
        channel_id: &ChannelId,
        target: ActorId,
    ) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal("this app has no room-ceremony seam".to_string())
        })?;
        let (generation_id, key) = self.room_tip_generation(channel_id).await.ok_or_else(|| {
            BackendError::Internal(
                "this device holds no key for the room's current generation — it cannot key \
                 anybody else in"
                    .to_string(),
            )
        })?;
        let row = self
            .read_room_floor(channel_id)
            .await?
            .wrap_targets()
            .into_iter()
            .find(|m| m.member_actor == target)
            .ok_or_else(|| {
                BackendError::Internal(
                    "that principal is not a keyable member of this room's floor — an invitation \
                     is not a seating, and a member the room seated without a wrap target cannot \
                     be covered yet"
                        .to_string(),
                )
            })?;
        let record = self
            .engine
            .build_room_topup_wrap(&key, &row, &generation_id, Timestamp::now_millis() as i64)
            .map_err(|e| BackendError::Internal(format!("build room top-up wrap: {e}")))?;
        let bytes = fauna_core::encoding::canonical_encode(&record)
            .map_err(|e| BackendError::Internal(format!("encode room top-up wrap: {e}")))?;
        ceremony
            .room_backfill_generations(channel_id.to_string(), hex::encode(target.0), vec![bytes])
            .await?;
        Ok(())
    }

    /// Remove `target` from the room's floor **and rotate its generation** —
    /// one act, because either half alone is wrong.
    ///
    /// Unseating is not the severance. A removed member still holds every
    /// generation key it was ever wrapped into, and a room's ciphertext is
    /// fetchable by anyone the relay serves — so what actually ends its read is
    /// a mint whose wrap set no longer names it ("a removal rotates the
    /// generation, so a removed member who can still fetch ciphertext through a
    /// relay reads nothing new", `conversation-rooms.md` § The three classes →
    /// *Community*, reason 4). The nest never mints, so this is the client's
    /// job, and a `room.remove` door that did not carry it would leave every
    /// caller silently believing in a severance that had not happened.
    ///
    /// **The order is the design and cannot be swapped.** Coverage is judged at
    /// the nest against the floor *as it stands now*, so a mint built before
    /// the unseat would wrap the room's next key to the very member being
    /// removed — the rotation would hand out what it exists to withhold.
    ///
    /// **A rotation that fails leaves a named, recoverable residue.** The
    /// unseat has landed by then: the target is off the floor, cannot send, and
    /// cannot be keyed into anything future — but it can still open traffic
    /// sealed under the un-rotated tip. That is a real half-state, so the error
    /// says exactly that rather than reading as "the removal failed", and
    /// [`Self::rotate_room_key`] is the retry any owner or admin can run
    /// afterwards from any device. It is deliberately *not* rolled back by
    /// re-seating the target: re-admission returns a member on a **fresh**
    /// roster entry, so an undo would not restore the state that was there.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the ceremony seam is unregistered; the
    /// nest's own refusal for the unseat (not an owner or admin, the target is
    /// the room's owner, the target is the home nest — whose read is revoked by
    /// rotating it out rather than by unseating it); and the residue error
    /// above when the unseat landed but the rotation did not.
    pub async fn remove_room_member(
        &self,
        channel_id: &ChannelId,
        target: ActorId,
    ) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot remove a room member — it has no room-ceremony seam".to_string(),
            )
        })?;
        ceremony
            .room_remove(channel_id.to_string(), hex::encode(target.0))
            .await?;
        self.rotate_room_key(channel_id).await.map_err(|e| {
            BackendError::Internal(format!(
                "{} is off this room's floor, but the room's key was NOT rotated, so they can \
                 still read traffic sealed under the current generation — rotate again to \
                 complete the removal: {e}",
                hex::encode(target.0)
            ))
        })
    }

    /// Leave the room — unseat **this** account.
    ///
    /// It does **not** rotate, and that is not an omission. A leaver has no
    /// mint authority (key authority is the owner's and admins',
    /// `conversation-rooms.md` § Roles and authorization), and a rotation it
    /// could build would still wrap to itself, since coverage is judged against
    /// the floor and the leaver is on it until the unseat lands. What bounds a
    /// voluntary departure is that the leaver already read everything up to
    /// now; a room that wants a departed member sealed out of *new* traffic
    /// rotates from a remaining owner or admin ([`Self::rotate_room_key`]).
    ///
    /// Local state is left alone for the same reason: this device keeps the
    /// generations it already holds and the bubbles it already folded, because
    /// deleting them would destroy the user's own copy of a conversation they
    /// were legitimately part of, and would not un-read a single byte.
    ///
    /// A room homed on another nest departs through the relay, by the same
    /// [`Self::channel_home_url`] signal that routes `channel.send_remote`:
    /// the floor this leave retires a seat on lives on the room's home, and
    /// this device's own nest holds no room record for it.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the ceremony seam is unregistered; the
    /// nest's own refusal otherwise — notably the **owner's**, who transfers
    /// ownership first, because a room is never owner-less.
    pub async fn leave_room_by_ceremony(&self, channel_id: &ChannelId) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot leave a room — it has no room-ceremony seam".to_string(),
            )
        })?;
        ceremony
            .room_leave(channel_id.to_string(), self.channel_home_url(channel_id))
            .await?;
        Ok(())
    }

    /// Read again the invitations pending on `channel_id` that this account
    /// may withdraw, for the synchronous render
    /// ([`RoomSnapshot::pending_invites`]). Answers whether what is rendered
    /// moved, so the caller knows whether a repaint is owed.
    ///
    /// Quiet by contract, like the floor read it rides: a **refusal** clears
    /// the list — this account is off the floor or not admitted to the door,
    /// and either way nothing here still stands behind a painted row —
    /// while a read that merely did not land leaves the last answer as it was.
    ///
    /// A room homed on another nest is not asked: the two doors have no relay
    /// kind, so this account's own nest holds no floor to answer from and
    /// would refuse a member in good standing.
    async fn refresh_pending_room_invites(&self, channel_id: &ChannelId) -> bool {
        let Some(ceremony) = self.room_ceremony.get() else {
            return false;
        };
        if self.channel_home_url(channel_id).is_some() {
            return false;
        }
        let listed = match ceremony.room_list_invites(channel_id.to_string()).await {
            Ok(listed) => Some(listed),
            Err(ConvRpcError::Rejected { .. } | ConvRpcError::NeedsUpdate { .. }) => None,
            Err(e) => {
                tracing::debug!(
                    channel = %channel_id,
                    "a room's pending invitations were not listed this pass: {e:?}"
                );
                return false;
            }
        };
        let mut pending = self.room_pending_invites.lock().unwrap();
        if pending.get(channel_id) == listed.as_ref() {
            return false;
        }
        match listed {
            Some(listed) => pending.insert(*channel_id, listed),
            None => pending.remove(channel_id),
        };
        true
    }

    /// Withdraw the invitation pending for `invitee` on a community room —
    /// the row, its standing envelope and the envelope's quota charge, one act
    /// on the home nest (`conversation-rooms.md` § Join rules and invites →
    /// *Pending invitations are visible to whoever may withdraw them*).
    ///
    /// Nothing is judged here: whoever the nest served the row to may withdraw
    /// it, and the nest asks the same predicate again at this door. The
    /// invitee is told nothing, and an invitation that was no longer pending
    /// is a success — the caller wanted it gone and it is. Either way the list
    /// is read again, so the row leaves the screen with the act rather than a
    /// floor cadence later.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when the ceremony seam is unregistered; the
    /// nest's own refusal otherwise (off the floor, or not this caller's
    /// invitation to withdraw).
    pub async fn revoke_room_invite(
        &self,
        channel_id: &ChannelId,
        invitee: ActorId,
    ) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot withdraw a room invitation — it has no room-ceremony seam"
                    .to_string(),
            )
        })?;
        ceremony
            .room_revoke_invite(channel_id.to_string(), invitee.to_hex())
            .await?;
        self.refresh_pending_room_invites(channel_id).await;
        Ok(())
    }

    /// The room's stored policy, **verified** — the record a member renders,
    /// and the one a policy change is authored from.
    ///
    /// The roles on the floor are this policy's projection, which is what the
    /// nest enforces against; these are the bytes their author signed. The
    /// signature is checked here, at the reader, for the invitation's reason: a
    /// policy names an owner, an admin set and a room name, and rendering any
    /// of them on the delivery path's word would let the path put a rank in
    /// somebody's mouth. A policy that does not decode or does not verify reads
    /// as `None` — the same answer a policy-less room gives, and the honest one:
    /// this device knows of no policy it can stand behind.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when no roster seam is registered. A floor
    /// this device cannot read, and a room carrying no policy, are both `None`
    /// rather than errors.
    pub async fn read_room_policy(
        &self,
        channel_id: &ChannelId,
    ) -> Result<Option<fauna_mls::room_policy::SignedRoomPolicy>, BackendError> {
        let floor = self.read_room_floor(channel_id).await?;
        Ok(self.anchored_room_policy(channel_id, &floor).await)
    }

    /// Apply `edit` to the room's stored policy and store the result — the
    /// room-settings editor's act (`conversation-rooms.md` § Roles and
    /// authorization). Answers the version now stored.
    ///
    /// **A change is a replacement, so it is authored from the stored bytes**,
    /// never from what this device happens to know. That is the whole reason
    /// the read exists: a policy assembled locally would carry defaults for
    /// every field the editor did not show, and storing it would silently reset
    /// them — a rename that also turned history on. So the edit names only what
    /// changes ([`RoomPolicyEdit`]), and everything else survives byte for byte.
    ///
    /// The version is bumped by exactly one, which is also the nest's ratchet:
    /// two devices editing concurrently means the second is refused rather than
    /// overwriting the first, and the honest recovery is to re-read and re-apply
    /// — which is what a caller retrying this method does.
    ///
    /// Ownership is **not** settable here; the nest refuses it by name, because
    /// a hand-over moves the roster row as well as the bytes
    /// ([`Self::transfer_room_ownership`]).
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered, the room carries
    /// no policy this device can verify (a policy-less room has nothing to amend),
    /// or the edited policy does not sign; the nest's own refusal otherwise —
    /// notably an **admin** changing the admin set, which only the owner may do.
    pub async fn set_room_policy(
        &self,
        channel_id: &ChannelId,
        edit: RoomPolicyEdit,
    ) -> Result<u64, BackendError> {
        if let RoomPolicyEdit::TransferOwnership(new_owner) = edit {
            return self.transfer_room_ownership(channel_id, new_owner).await;
        }
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot change a room's policy — it has no room-ceremony seam".to_string(),
            )
        })?;
        self.refuse_unpermitted_edit(channel_id, &edit).await?;
        let mut policy = self.amendable_policy(channel_id).await?;
        match &edit {
            RoomPolicyEdit::Rename(name) => policy.name = Some(name.clone()),
            RoomPolicyEdit::JoinRule(rule) => policy.join_rule = (*rule).into(),
            RoomPolicyEdit::HistoryPolicy(history) => policy.history_policy = (*history).into(),
            RoomPolicyEdit::AppointAdmin(actor) => {
                let mut admins = policy.admins.clone();
                admins.push(*actor);
                policy.set_admins(admins);
            }
            RoomPolicyEdit::DemoteAdmin(actor) => {
                let admins: Vec<ActorId> = policy
                    .admins
                    .iter()
                    .copied()
                    .filter(|a| a != actor)
                    .collect();
                policy.set_admins(admins);
            }
            // Routed to its own door above, before anything was read.
            RoomPolicyEdit::TransferOwnership(_) => unreachable!("handled above"),
        }
        ceremony
            .room_set_policy(
                channel_id.to_string(),
                self.sign_policy(channel_id, &policy)?,
            )
            .await
            .map_err(Into::into)
    }

    /// Replace the transparent labelers a **community** room's home nest
    /// applies to its messages (`conversation-rooms.md` § The three classes →
    /// *What the home nest does with its read*, purpose 2). Answers the version
    /// now stored.
    ///
    /// Authored from the **stored** set, for the policy's reason: the version is
    /// the nest's strict ratchet, so the next one is exactly one past what the
    /// nest holds — never past what this device last saw, which would be
    /// refused, or overwrite a co-admin's change it never read. The set is
    /// replaced whole: it is one signed record.
    ///
    /// # Errors
    /// [`BackendError::Refusal`] for a plain member, before any round trip (the
    /// nest refuses anyway — rule 6, "owner or admin for the rest"; this is the
    /// refusal for the finger, in the policy editor's words);
    /// [`BackendError::Internal`] when a seam is unregistered or the stored set
    /// does not verify; the nest's own refusal otherwise — notably an id it
    /// does not publish as a `wasm` or `text-model` labeler.
    pub async fn set_room_labelers(
        &self,
        channel_id: &ChannelId,
        labelers: Vec<ActorId>,
    ) -> Result<u64, BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot name a room's labelers — it has no room-ceremony seam".to_string(),
            )
        })?;
        let floor = self.read_room_floor(channel_id).await?;
        if let Some(role) = floor
            .members
            .iter()
            .find(|m| m.actor == self.self_actor)
            .and_then(|m| m.role)
            && !fauna_mls::room_policy::RoomRole::from(role).is_admin_or_owner()
        {
            return Err(BackendError::Refusal(
                fauna_i18n::strings::error::send::ROOM_POLICY_NOT_PERMITTED.to_string(),
            ));
        }
        let stored = match floor.labelers.as_deref() {
            None => 0,
            Some(bytes) => {
                verified_room_labelers(bytes, channel_id)
                    .ok_or_else(|| {
                        BackendError::Internal(
                            "the room's stored labeler set does not verify".to_string(),
                        )
                    })?
                    .labelers
                    .version
            }
        };
        let signed = self
            .engine
            .sign_room_labelers(&fauna_mls::room_policy::RoomLabelers::new(
                channel_id.0,
                stored + 1,
                labelers,
            ))
            .map_err(|e| BackendError::Internal(format!("sign room labeler set: {e}")))?;
        let bytes = fauna_core::encoding::canonical_encode(&signed)
            .map_err(|e| BackendError::Internal(format!("encode room labeler set: {e}")))?;
        let version = ceremony
            .room_set_labelers(channel_id.to_string(), bytes)
            .await?;
        // The editor re-renders from the floor, so the next render should show
        // the set just stored rather than wait a whole refresh interval.
        self.refresh_floor_soon(channel_id);
        Ok(version)
    }

    /// The product-level role refusal, read off the **floor** — the community
    /// class's copy of the one the MLS path makes against the group context.
    ///
    /// It exists so the two classes refuse the same gesture with the same
    /// words: the nest enforces the roles table itself and would refuse anyway,
    /// but a user who is told "only the owner appoints admins" on one room and
    /// gets a generic transport error on another is meeting two products
    /// (priority #1). The nest's check remains the authority; this one is for
    /// the finger.
    async fn refuse_unpermitted_edit(
        &self,
        channel_id: &ChannelId,
        edit: &RoomPolicyEdit,
    ) -> Result<(), BackendError> {
        use fauna_i18n::strings::error::send;
        let Some(role) = self
            .read_room_floor(channel_id)
            .await?
            .members
            .iter()
            .find(|m| m.actor == self.self_actor)
            .and_then(|m| m.role)
        else {
            // A floor that names no role for this account is a policy-less room or a
            // read that could not resolve one. The nest decides; refusing here
            // on an absent role would block an edit the room might well permit.
            return Ok(());
        };
        if edit_permitted(edit, role.into()) {
            return Ok(());
        }
        Err(BackendError::Refusal(
            if matches!(
                edit,
                RoomPolicyEdit::AppointAdmin(_)
                    | RoomPolicyEdit::DemoteAdmin(_)
                    | RoomPolicyEdit::TransferOwnership(_)
            ) {
                send::ROOM_ADMINS_OWNER_ONLY.to_string()
            } else {
                send::ROOM_POLICY_NOT_PERMITTED.to_string()
            },
        ))
    }

    /// Hand the room to `new_owner` — the outgoing owner's own act. Answers the
    /// version now stored.
    ///
    /// Signed by **this** account, and that is what authorizes it: the signer's
    /// role in the *previous* version is what decides whether the owner field
    /// may have changed, and at signing time this account is still the owner.
    /// The incoming owner must already be a live user member — a room is never
    /// owned by the nest that reads it, and never by somebody who is not in it.
    ///
    /// The outgoing owner becomes an ordinary member from the transfer on, and
    /// so becomes removable like any other — which is the point of transferring
    /// rather than leaving. An owner who wants out does this and then
    /// [`Self::leave_room`]; a bare leave is refused, because a room is never
    /// owner-less.
    ///
    /// **The successor leaves the admin set** (`conversation-rooms.md` § Roles
    /// and authorization — "the next version; the successor leaves the admin
    /// set"). Not a convenience: exactly one owner exists and the owner is
    /// never *also* an admin, so a policy naming its owner among the admins is
    /// structurally invalid and would not sign. Everyone else's rank is left
    /// alone, the outgoing owner included — who becomes a plain member, and so
    /// becomes removable like any other.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered, the room carries
    /// no policy this device can verify, or the new policy does not sign; the
    /// nest's own refusal otherwise — this account not being the owner, or the
    /// named successor not being a live user member.
    pub async fn transfer_room_ownership(
        &self,
        channel_id: &ChannelId,
        new_owner: ActorId,
    ) -> Result<u64, BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal(
                "this app cannot transfer a room — it has no room-ceremony seam".to_string(),
            )
        })?;
        let mut policy = self.amendable_policy(channel_id).await?;
        policy.owner = new_owner;
        // The successor leaves the admin set — the owner is never also an
        // admin, and a policy that named it in both would not sign at all.
        let admins: Vec<ActorId> = policy
            .admins
            .iter()
            .copied()
            .filter(|a| *a != new_owner)
            .collect();
        policy.set_admins(admins);
        let version = ceremony
            .room_transfer_ownership(
                channel_id.to_string(),
                self.sign_policy(channel_id, &policy)?,
            )
            .await?;
        self.refresh_floor_soon(channel_id);
        Ok(version)
    }

    /// The room's verified policy at its **next** version — the base every
    /// amendment starts from, so the two policy doors cannot drift on what
    /// "amend" means.
    async fn amendable_policy(
        &self,
        channel_id: &ChannelId,
    ) -> Result<fauna_mls::room_policy::RoomPolicy, BackendError> {
        let signed = self.read_room_policy(channel_id).await?.ok_or_else(|| {
            BackendError::Internal(
                "this room carries no policy this device can verify, so there is nothing to \
                 amend — a policy-less room is labelled by its members rather than governed"
                    .to_string(),
            )
        })?;
        let mut policy = signed.policy;
        // Exactly `stored + 1`: the nest's ratchet, so a replayed older policy
        // can never be installed over a newer one — and so two devices editing
        // at once means the second is refused rather than silently winning.
        policy.version += 1;
        Ok(policy)
    }

    /// Sign an amended policy of the room `channel_id` as this account,
    /// canonically encoded for the wire.
    fn sign_policy(
        &self,
        channel_id: &ChannelId,
        policy: &fauna_mls::room_policy::RoomPolicy,
    ) -> Result<Vec<u8>, BackendError> {
        // The community signer: both these doors are the floor-authoritative
        // class's, and a community room may carry the `request` join rule the
        // end-to-end validation refuses. It binds the version to this room, so
        // it can never be served as another room's.
        let signed = self
            .engine
            .sign_room_policy_community(&channel_id.0, policy)
            .map_err(|e| BackendError::Internal(format!("sign room policy: {e}")))?;
        fauna_core::encoding::canonical_encode(&signed)
            .map_err(|e| BackendError::Internal(format!("encode room policy: {e}")))
    }

    /// Mint a fresh generation over the room's floor as it stands now — the
    /// **severance** move, and the retry when one did not land.
    ///
    /// Every principal the floor currently seats with a wrap target gets the
    /// new key; a member just unseated does not, and neither does a home nest
    /// whose read the members have withdrawn — a rotation keeps that choice
    /// rather than resetting it ([`NestRead::Keep`]). It is the same mint founding runs, aimed at an
    /// existing room: the room's current tip becomes the new generation's
    /// parent, which is also the check the nest admits it on.
    ///
    /// Idempotent in effect rather than in fact: running it twice mints two
    /// generations, which costs a wrap set and severs nothing extra. That is
    /// the right way round for a retry — a rotation that ran when it did not
    /// need to is harmless, one that did not run when it was needed is the bug
    /// [`Self::remove_room_member`] exists to avoid.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered, this device
    /// cannot see the room's tip (it holds no wrap for the room, so it cannot
    /// name the parent the nest requires), the floor names no wrap target, or
    /// the nest refuses to admit the mint — which it does for a caller the
    /// floor does not rank owner or admin.
    pub async fn rotate_room_key(&self, channel_id: &ChannelId) -> Result<(), BackendError> {
        let tip = self
            .room_tip_generation_id(channel_id)
            .await
            .ok_or_else(|| {
                BackendError::Internal(
                    "this device cannot see this room's current generation, so it cannot name \
                     the parent a rotation must build on"
                        .to_string(),
                )
            })?;
        self.mint_room_generation(channel_id, vec![tip], NestRead::Keep)
            .await
    }

    /// Grant or withdraw the home nest's read of a community room — a
    /// **rotation**, never a roster edit (`conversation-rooms.md`
    /// § Implementation status today, *The home nest's read, and its revoke*:
    /// "Rotation revokes without touching the member set").
    ///
    /// `false` mints the room's next generation wrapped to every member but the
    /// nest: the nest reports its read revoked and deletes every derived view
    /// of the room in the same act, and derives nothing further. `true` mints
    /// one that wraps to it again, so the nest reads — and indexes — from that
    /// generation on; what it deleted stays deleted, because it is rebuilt only
    /// from generations it holds a wrap for.
    ///
    /// Owner or admin only, like every mint; the nest refuses anyone else. The
    /// class does not move either way: the nest stays on the floor, so the room
    /// is still a community room whose home nest the members have chosen not
    /// to let read it.
    ///
    /// # Errors
    /// As [`Self::rotate_room_key`], plus a refusal when the room's floor seats
    /// no home nest at all — an end-to-end room has no read to grant.
    pub async fn rotate_room_nest_read(
        &self,
        channel_id: &ChannelId,
        reads: bool,
    ) -> Result<(), BackendError> {
        let tip = self
            .room_tip_generation_id(channel_id)
            .await
            .ok_or_else(|| {
                BackendError::Internal(
                    "this device cannot see this room's current generation, so it cannot name \
                     the parent a rotation must build on"
                        .to_string(),
                )
            })?;
        let choice = if reads {
            NestRead::Grant
        } else {
            NestRead::Revoke
        };
        self.mint_room_generation(channel_id, vec![tip], choice)
            .await
    }

    /// The id of the room's tip generation, without opening it.
    ///
    /// A rotation needs the parent's **name**, not its key: a mint chooses a
    /// fresh random generation, so nothing about building one requires reading
    /// the generation it replaces. Kept apart from
    /// [`Self::room_tip_generation`] — which does open it — precisely so a
    /// rotation is not gated on a read it does not need: an admin seated after
    /// the last mint and not yet backfilled holds no openable tip, and refusing
    /// its rotation would leave a room unable to sever the member it most wants
    /// to.
    async fn room_tip_generation_id(&self, channel_id: &ChannelId) -> Option<[u8; 32]> {
        let reader = self.room_generation_reader.get()?;
        let wraps = reader
            .read_generations(channel_id.to_string(), self.channel_home_url(channel_id))
            .await?;
        wraps
            .into_iter()
            .find(|w| w.is_tip)
            .map(|w| w.generation_id)
    }

    /// Mint one generation for `channel_id` over the room's **own floor**, and
    /// publish it — the client half of the recipient-set scheme's mint
    /// (`conversation-rooms.md` § The three classes → *Community*).
    ///
    /// `parents` is the tip this mint replaces, empty for a room's first. The
    /// wrap set comes from the roster read back rather than from anything this
    /// device remembers: roster coverage is judged at the nest against the
    /// floor as it stands *now*, so a minter that wrapped to a cached roster
    /// would be refused the moment a membership changed under it.
    ///
    /// Every principal the floor seats with both an entry id and a reception
    /// key is wrapped to, the home **nest** included unless `nest_read` says
    /// otherwise — that wrap is the materialization grant, and a mint that
    /// omits it is the revoke. A principal with neither is *unkeyable*, which
    /// coverage skips.
    ///
    /// ⚠ **An ordinary rotation keeps the members' choice ([`NestRead::Keep`]).**
    /// The nest is outside the coverage rule precisely so the grant can be
    /// withdrawn, which means nothing at the nest stops a mint from wrapping to
    /// it again — so a removal's severance rotation that wrapped to "every
    /// target it can see" would re-grant the withdrawn read on an honest
    /// device's ordinary rotation, unasked and unseen. The tip's own wrap set
    /// is the record of the choice, and the roster read says whether the nest
    /// is in it — the nest's own word, which is as far as the revoke reaches:
    /// the keep guards against honest drift, not against a home nest that
    /// misreports its own row, whose floor this class trusts by construction
    /// (`community-rooms.md` § Implementation status today, the sealing entry
    /// → *How far the revoke reaches*).
    ///
    /// # Errors
    /// [`BackendError::Internal`] when a seam is unregistered, the roster is
    /// unreadable, it names no wrap target at all, the mint does not assemble,
    /// or the nest refuses to admit it.
    async fn mint_room_generation(
        &self,
        channel_id: &ChannelId,
        parents: Vec<[u8; 32]>,
        nest_read: NestRead,
    ) -> Result<(), BackendError> {
        let ceremony = self.room_ceremony.get().ok_or_else(|| {
            BackendError::Internal("this app has no room-ceremony seam".to_string())
        })?;
        let floor = self.read_room_floor(channel_id).await?;
        let nest_row = floor
            .members
            .iter()
            .find(|m| m.kind == crate::backend::RoomPrincipalKind::Nest);
        let wrap_nest = match nest_read {
            NestRead::Grant | NestRead::Revoke if nest_row.is_none() => {
                return Err(BackendError::Internal(
                    "this room's floor seats no home nest, so there is no read to grant or \
                     withdraw"
                        .to_string(),
                ));
            }
            NestRead::Grant => true,
            NestRead::Revoke => false,
            // Unknown — a room's first mint, or a reply carrying no answer —
            // is the status quo: wrap to it, which is what founding a community
            // room grants and what every mint did before the roster could say.
            NestRead::Keep => nest_row.and_then(|m| m.tip_wrapped) != Some(false),
        };
        let targets: Vec<_> = floor
            .members
            .iter()
            .filter(|m| wrap_nest || m.kind != crate::backend::RoomPrincipalKind::Nest)
            .filter_map(|m| m.wrap_target())
            .collect();
        if targets.is_empty() {
            return Err(BackendError::Internal(
                "this room's floor names no wrap target — a generation nobody can open is not a \
                 key, it is a lockout"
                    .to_string(),
            ));
        }
        let built = self
            .engine
            .build_room_generation_mint(&targets, parents, Timestamp::now_millis() as i64)
            .map_err(|e| BackendError::Internal(format!("build room generation: {e}")))?;
        let record = fauna_core::encoding::canonical_encode(&built.record)
            .map_err(|e| BackendError::Internal(format!("encode room generation: {e}")))?;
        ceremony
            .room_publish_generation(channel_id.to_string(), record)
            .await?;
        // Cache the key we just minted: this device is a wrap target of its
        // own mint, so a read would answer the same key — but the send that
        // usually follows a founding should not pay a round trip to learn what
        // it just chose. The tip read in `send_room_message` still happens;
        // this only spares the unwrap.
        if let Ok(mut cache) = self.room_generations.lock() {
            cache
                .entry(*channel_id)
                .or_default()
                .insert(built.generation_id, built.gen_key);
        }
        // Who the new tip covers — the nest's read included — has moved.
        self.refresh_floor_soon(channel_id);
        Ok(())
    }

    /// The opened room generation key `generation` names on `channel`, or why
    /// this device cannot open it — and so whether the receive walk may step
    /// past the record ([`RoomKeyLookup`]).
    ///
    /// Cache first, then **one** refresh pass: a miss re-reads the room's
    /// wraps and unwraps every one this account's reception secrets open, so
    /// walking a page of bubbles across a rotation costs one round trip rather
    /// than one per record.
    ///
    /// Every unwrap re-checks the mint's key commitment inside
    /// `open_group_generation_key_as_entry`, so a substituted wrap is refused
    /// here rather than trusted from the wire.
    async fn room_generation_key(
        &self,
        channel_id: &ChannelId,
        generation: &[u8; 32],
    ) -> RoomKeyLookup {
        if let Some(key) = self.room_generations.lock().ok().and_then(|cache| {
            cache
                .get(channel_id)
                .and_then(|per_gen| per_gen.get(generation))
                .map(copy_generation_key)
        }) {
            return RoomKeyLookup::Key(key);
        }
        let (Some(reader), Some(secrets)) = (
            self.room_generation_reader.get(),
            self.group_reception_keys.get(),
        ) else {
            return RoomKeyLookup::Unkeyable;
        };
        let Some(wraps) = reader
            .read_generations(channel_id.to_string(), self.channel_home_url(channel_id))
            .await
        else {
            // A read that failed is not an answer about this record: the next
            // pass asks again, and the walk must still be before it then.
            return RoomKeyLookup::NotYet;
        };
        // Derive each keypair once, not once per wrap: an X-Wing keygen per
        // (generation × held key) would be quadratic on a room that has
        // rotated often.
        let secrets: Vec<_> = secrets
            .reception_keys()
            .await
            .iter()
            .filter_map(|record| record.keypair().ok())
            .collect();
        let mut opened = HashMap::new();
        for wrap in wraps {
            for pair in &secrets {
                if let Ok(key) = fauna_mls::wrapped_blob::group_generation_wraps::
                    open_group_generation_key_as_entry(
                        &wrap.wrap,
                        &pair.secret,
                        &wrap.generation_id,
                        &wrap.entry_id,
                        &wrap.key_commitment,
                    )
                {
                    opened.insert(wrap.generation_id, key);
                    break;
                }
            }
        }
        let found = opened.get(generation).map(copy_generation_key);
        let holds_any = if let Ok(mut cache) = self.room_generations.lock() {
            let held = cache.entry(*channel_id).or_default();
            held.extend(opened);
            !held.is_empty()
        } else {
            false
        };
        match found {
            Some(key) => RoomKeyLookup::Key(key),
            // Keyed into the room, just not into this generation — one it
            // predates under a `history_policy` that retains nothing for
            // joiners. That is final, so the walk steps past it.
            None if holds_any => RoomKeyLookup::NotForUs,
            // No wrap in this room at all: a newcomer the room has not keyed in
            // yet. Acceptance seats before the inviter's device covers the
            // tip, and the tip is the one generation every history policy
            // authorizes — so what is sealed under it IS this member's to read,
            // later.
            None => RoomKeyLookup::NotYet,
        }
    }

    /// Record that `channel_id`'s walk stopped waiting for its key-in
    /// ([`Self::awaiting_key_rooms`]). `true` when the room was not already
    /// waiting — the transition the snapshot has to repaint for.
    fn begin_key_wait(&self, channel_id: &ChannelId) -> bool {
        self.awaiting_key_rooms.lock().unwrap().insert(*channel_id)
    }

    /// End `channel_id`'s wait, if it had one. `true` exactly once per wait —
    /// which is what makes [`ConvPollOutcome::keyed_in`] a one-shot stop rather
    /// than a new way to wedge a walk.
    fn end_key_wait(&self, channel_id: &ChannelId) -> bool {
        self.awaiting_key_rooms.lock().unwrap().remove(channel_id)
    }

    /// Whether `channel_id` is a community room whose walk is waiting for its
    /// key-in — [`RoomSnapshot::awaiting_key`].
    pub fn is_awaiting_key(&self, channel_id: &ChannelId) -> bool {
        self.awaiting_key_rooms.lock().unwrap().contains(channel_id)
    }

    /// Send one message into a **community room** — the class's whole send
    /// path (`conversation-rooms.md` § The three classes → *Community*).
    ///
    /// Three deliberate sames and one difference. Same storage shape (the
    /// ordinary `channel.send`, a `RoomSealed` envelope), same read feed, same
    /// `conv:<channel>:<seq>` message id — "one storage shape and one read
    /// feed for both classes, differing only in which key the reader holds" is
    /// literal here. The difference is the seal: the body is **authored** and
    /// sealed under the room's tip generation rather than encrypted to a group
    /// ratchet.
    ///
    /// **Always the tip, resolved now.** Sealing under a generation the room
    /// has rotated past would produce a message the home nest refuses to index
    /// and a removed member could still read — the exact two things a rotation
    /// is for — so this pays a read per send rather than trusting a cache.
    ///
    /// An attachment seals under the room's **attachment content kind** off the
    /// same tip generation ([`fauna_mls::room_message::seal_room_attachment`]) —
    /// a second per-kind key, never the message kind's, and openable by every
    /// holder of the generation's wrap, the home nest included: the class, not
    /// the kind, decides who reads (`community-rooms.md` § The three classes →
    /// *Attachments — the second content kind*, ratified
    /// 2026-09-10). Everything else is the end-to-end path's, reused: the blob
    /// is uploaded to the room's home nest before the send so the message never
    /// names a blob a peer cannot yet fetch, and its content address is listed
    /// in plaintext `attachment_refs` so the nest pins it past the blob GC.
    /// `ChannelAttachment::epoch` is written `0` on this class — the reader
    /// takes the generation from the envelope, which the author's signature
    /// binds.
    async fn send_room_message(
        &self,
        channel_id: &ChannelId,
        compose: &ComposeState,
        attachments: &[crate::backend::ResolvedAttachment],
    ) -> Result<SendOutcome, BackendError> {
        let (generation, key) = self.room_tip(channel_id).await?;
        let (body, attachment_refs, attachment_coordinates) = if attachments.is_empty() {
            (
                ChannelMessageBody::Text(compose.body_draft.clone()),
                Vec::new(),
                Vec::new(),
            )
        } else {
            let mut items = Vec::with_capacity(attachments.len());
            let mut attachment_refs = Vec::with_capacity(attachments.len());
            let mut attachment_coordinates = Vec::with_capacity(attachments.len());
            for att in attachments {
                let sealed =
                    fauna_mls::room_message::seal_room_attachment(&key, &generation, &att.bytes)
                        .map_err(|e| {
                            BackendError::Internal(format!("seal room attachment: {e}"))
                        })?;
                let (item, blob) = self
                    .upload_sealed_attachment(
                        channel_id,
                        att,
                        sealed,
                        AttachmentOpeningKey::RoomGeneration { generation },
                    )
                    .await?;
                attachment_refs.push(blob.sealed_cid_hex.clone());
                attachment_coordinates.push((
                    att.blob_hash.clone(),
                    AttachmentCoordinates::FaunaMls {
                        channel: *channel_id,
                        blob,
                    },
                ));
                items.push(item);
            }
            (
                ChannelMessageBody::Attachments {
                    body: compose.body_draft.clone(),
                    attachments: items,
                },
                attachment_refs,
                attachment_coordinates,
            )
        };
        let (seq, sent_at_ms, plane_ref) = self
            .post_room_body(channel_id, &generation, &key, body, attachment_refs)
            .await?;
        Ok(SendOutcome {
            message_id: MessageId(format!("conv:{channel_id}:{seq}")),
            timestamp_ms: sent_at_ms,
            sender: TypedAddress::Fauna {
                handle: self.self_address.get(),
                actor_id: self.self_actor,
            },
            plane_ref,
            attachment_coordinates,
        })
    }

    /// [`Self::room_tip_generation`], or the class's one "cannot send" refusal.
    async fn room_tip(
        &self,
        channel_id: &ChannelId,
    ) -> Result<([u8; 32], GenerationKey), BackendError> {
        self.room_tip_generation(channel_id).await.ok_or_else(|| {
            BackendError::Internal(
                "this room has no generation key on this device — it has not been keyed, or this \
                 device holds no wrap for its current generation"
                    .to_string(),
            )
        })
    }

    /// Seal `body` under the room's generation key, sign it as this author, and
    /// append it in a `RoomSealed` envelope — the one door every community
    /// body takes, a message and the sender's own delete or reaction alike
    /// (`conversation-rooms.md` § Roles and authorization → *Delete any message
    /// — the mechanism* → *Community rooms*: they "ride the same path", so the
    /// floor cannot tell one from another and is never asked to). Returns the
    /// allocated `seq`, the signed stamp, and the record's plane identity.
    async fn post_room_body(
        &self,
        channel_id: &ChannelId,
        generation: &[u8; 32],
        key: &GenerationKey,
        body: ChannelMessageBody,
        attachment_refs: Vec<String>,
    ) -> Result<(i64, i64, Option<crate::message::PlaneRef>), BackendError> {
        let sent_at_ms = Timestamp::now_millis() as i64;
        let ciphertext = self
            .engine
            .seal_room_message(&channel_id.0, generation, key, sent_at_ms, body)
            .map_err(|e| BackendError::Internal(format!("seal room message: {e}")))?;
        let envelope = ChannelEnvelope::RoomSealed {
            generation: generation.to_vec(),
            ciphertext,
        }
        .to_bytes()
        .map_err(|e| BackendError::Internal(format!("encode room envelope: {e}")))?;
        // The record's account-data-plane identity, derived from the very bytes
        // the nest files (`crate::plane`) — the send is one of the two places
        // that hold the sealed envelope (`account-sync-plane.md` § Built — T1's
        // reporting half → *The identity*). Unlike the MLS path a community
        // sender CAN open its own record, but its own poll never ingests it —
        // the walk skips a record whose id the Sent copy already holds — so
        // this is the sending device's only chance to learn it all the same.
        // Not a second truth: the same derivation the receiver's ingest runs,
        // over the same bytes.
        let plane_ref = crate::plane::plane_ref(&channel_id.to_string(), &envelope);
        let seq = self
            .send_on_channel(channel_id, envelope, None, attachment_refs)
            .await?;
        Ok((seq, sent_at_ms, plane_ref))
    }

    /// A bodiless side-effect send — the sender's own `Delete`, a `Reaction` —
    /// routed by class exactly as [`ConversationBackend::send`] routes a
    /// message: no MLS group on a bound channel is the community class's local
    /// signature, so the body seals under the room's tip generation; every
    /// other channel takes the MLS application path.
    async fn post_side_effect(
        &self,
        channel_id: &ChannelId,
        body: ChannelMessageBody,
    ) -> Result<(), BackendError> {
        if self.engine.has_group(channel_id) {
            self.post_app_message(channel_id, body, Vec::new()).await?;
        } else {
            let (generation, key) = self.room_tip(channel_id).await?;
            self.post_room_body(channel_id, &generation, &key, body, Vec::new())
                .await?;
        }
        Ok(())
    }

    /// The room generation new content seals under on `channel` — the **tip**,
    /// which is what a send uses (`conversation-rooms.md` § The three classes
    /// → *Community*: "content seals under the tip").
    ///
    /// Always a fresh read rather than a cache hit: the tip moves on every
    /// rotation, and sealing under a generation the room has rotated past
    /// would produce a message the home nest refuses to index and a removed
    /// member could still read — the exact two things the rotation was for.
    async fn room_tip_generation(
        &self,
        channel_id: &ChannelId,
    ) -> Option<([u8; 32], GenerationKey)> {
        let reader = self.room_generation_reader.get()?;
        let secrets = self.group_reception_keys.get()?;
        let wraps = reader
            .read_generations(channel_id.to_string(), self.channel_home_url(channel_id))
            .await?;
        let tip = wraps.into_iter().find(|w| w.is_tip)?;
        let records = secrets.reception_keys().await;
        for record in &records {
            let Ok(pair) = record.keypair() else { continue };
            if let Ok(key) =
                fauna_mls::wrapped_blob::group_generation_wraps::open_group_generation_key_as_entry(
                    &tip.wrap,
                    &pair.secret,
                    &tip.generation_id,
                    &tip.entry_id,
                    &tip.key_commitment,
                )
            {
                if let Ok(mut cache) = self.room_generations.lock() {
                    cache
                        .entry(*channel_id)
                        .or_default()
                        .insert(tip.generation_id, copy_generation_key(&key));
                }
                return Some((tip.generation_id, key));
            }
        }
        None
    }

    /// What became of this session's owed floor-roster reports — see
    /// [`RosterReportCounts`] for how to read it.
    pub fn roster_report_counts(&self) -> RosterReportCounts {
        self.roster_reports.snapshot()
    }

    /// Report `channel_id`'s roster to the room's home nest after a commit
    /// this device authored — *report, never guess* (`conversation-rooms.md`
    /// § The floor roster). Owed by every end-to-end room, the room's birth
    /// included ([`Self::send`] reports after the first post on a channel it
    /// just bootstrapped): roles are the policy's when the room carries one
    /// and absent when it does not — a 1:1, or a policy-less group — which is the
    /// wire's and the report door's own model (`RoomRosterEntryWire::role`,
    /// `RoomRosterReportRequest::policy_version`). Best-effort and tallied,
    /// never a failure of the gesture that owed it — the commit is already on
    /// the log, and the report's absence costs routing and custody-serving
    /// precision, never confidentiality.
    ///
    /// `commit_seq` is the log position the commit landed at — what its send
    /// answered — and it is what lets the home nest order this report against
    /// others that arrive out of order (§ The floor roster): without it, a
    /// report that got there after a later commit's would roll membership and
    /// roles back. `None` only for the birth report: the group's creation is
    /// not on the room log, so there is no position to name, and the home
    /// nest's bootstrap bound admits a self-naming first report on an empty
    /// floor.
    async fn report_roster(&self, channel_id: &ChannelId, commit_seq: Option<i64>) {
        use std::sync::atomic::Ordering::Relaxed;
        if !self.engine.has_group(channel_id) {
            return;
        }
        let policy = match self.engine.room_policy(channel_id) {
            Some(Ok(policy)) => Some(policy),
            // A room with no policy has no roles to report, not no roster.
            None => None,
            Some(Err(_)) => return,
        };
        self.roster_reports.owed.fetch_add(1, Relaxed);
        let Some(reporter) = self.room_roster_reporter.get() else {
            self.roster_reports.no_reporter.fetch_add(1, Relaxed);
            return;
        };
        let members = self
            .engine
            .group_members(channel_id)
            .into_iter()
            .map(|actor| RoomRosterEntry {
                actor,
                role: policy.as_ref().map(|p| p.role_of(&actor).into()),
            })
            .collect();
        let report = RoomRosterReport {
            channel_hex: channel_id.to_string(),
            members,
            policy_version: policy.as_ref().map(|p| p.signed.policy.version),
            commit_seq,
            // The room's home, when it is not this device's own nest — the
            // signal that routes this channel's `channel.fetch`, `channel.send`
            // and roster READ relays, and now its report: a floor roster lives
            // on the room's home nest alone (§ The home nest), so a report
            // sent to the reporter's own nest would never reach it.
            home_nest_url: self.channel_home_url(channel_id),
        };
        match reporter.report(report).await {
            RoomRosterReportOutcome::Stored => {
                self.roster_reports.delivered.fetch_add(1, Relaxed);
            }
            // Delivered, and the floor kept someone else's answer for this
            // position. Never silent: a device that cannot see this cannot
            // tell its own report from the one that replaced it — which is
            // the half of the ordering rule that used to fail quietly on both
            // sides (`conversation-rooms.md` § The floor roster).
            // Ordinary when this device is catching a stale report up; worth
            // a look when the position it names is the one this device just
            // committed at.
            RoomRosterReportOutcome::Superseded { by } => {
                self.roster_reports.delivered.fetch_add(1, Relaxed);
                self.roster_reports.superseded.fetch_add(1, Relaxed);
                tracing::warn!(
                    channel = %channel_id,
                    reported = ?commit_seq,
                    superseded_by = ?by,
                    "floor-roster report not applied: the room's home holds a report at or \
                     above this position"
                );
            }
            RoomRosterReportOutcome::Undelivered => {
                self.roster_reports.undelivered.fetch_add(1, Relaxed);
            }
        }
    }

    /// **Leave an end-to-end room** — the self-scoped leave door
    /// (`conversation-rooms.md` § Roles and authorization → *Leaving — the
    /// mechanism*, amended 2026-09-23). The final-roster-report fallback this
    /// door replaced for a home nest older than it was removed by the
    /// compat-remnant sweep's fourth ratified exception
    /// (`version-compatibility.md` § Dimension 2): no such home remains.
    ///
    /// The door stamps this account's floor row departed and no one else's.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when no room-ceremony seam is registered —
    /// never true for a production glue site, which registers all four room
    /// seams as one bundle ([`Self::set_room_seams`]); the home nest's own
    /// refusal otherwise, surfaced to the leaver verbatim.
    async fn leave_end_to_end_room(&self, channel_id: &ChannelId) -> Result<(), BackendError> {
        self.leave_room_by_ceremony(channel_id).await?;
        // The floor moved, so this device's own render should show it on the
        // next poll rather than wait out a refresh interval.
        self.refresh_floor_soon(channel_id);
        Ok(())
    }

    /// How many polls a community room's floor is trusted before it is read
    /// again ([`Self::tend_community_room`]). An event count, deliberately not
    /// a wall-clock interval (`e2e-conventions.md` convention 14): a newcomer
    /// who accepted is keyed in within this many polls of an owner or admin
    /// device, and a room costs one roster read per this many.
    pub const FLOOR_REFRESH_POLLS: u32 = 4;

    /// Make `channel_id`'s floor due on the very next poll — after this
    /// device's own room act, whose effect the next render should show rather
    /// than wait a whole refresh interval for.
    fn refresh_floor_soon(&self, channel_id: &ChannelId) {
        self.floor_refresh.lock().unwrap().insert(*channel_id, 0);
    }

    /// Whether `channel_id`'s floor is due for a read this poll, spending one
    /// poll of its countdown when it is not.
    fn floor_refresh_due(&self, channel_id: &ChannelId) -> bool {
        let mut refresh = self.floor_refresh.lock().unwrap();
        match refresh.get_mut(channel_id) {
            None | Some(0) => true,
            Some(left) => {
                *left -= 1;
                false
            }
        }
    }

    /// Store what a group-less channel's floor says for the synchronous render
    /// ([`CachedFloor`]) — the class, the ranks, the policy and whether the
    /// home nest reads. The policy is the one this device anchored
    /// ([`Self::anchored_room_policy`]), never the floor's bytes as served: it
    /// is what the settings editor seeds the join rule and history policy from.
    async fn cache_floor(&self, channel_id: &ChannelId, floor: &crate::backend::RoomFloor) {
        let policy = self.anchored_room_policy(channel_id, floor).await;
        let cached = CachedFloor {
            kinds: floor.members.iter().map(|m| m.render_kind()).collect(),
            roles: floor
                .members
                .iter()
                .filter_map(|m| Some((m.actor, m.role?)))
                .collect(),
            policy: policy.map(|signed| (&signed.policy).into()),
            nest_read: floor
                .members
                .iter()
                .find(|m| m.kind == RoomPrincipalKind::Nest)
                .and_then(|m| m.tip_wrapped),
            labelers: floor_labelers(floor, channel_id),
        };
        self.room_floors.lock().unwrap().insert(*channel_id, cached);
    }

    /// Judge one **floor delete record** — an owner's or admin's delete of
    /// another member's message in a community room — as a member does
    /// (`conversation-rooms.md` § Roles and authorization → *Delete any message
    /// — the mechanism* → *Members verify what they paint*): the record is its
    /// author's, for this room, and the author is owner or admin in the policy
    /// **of the version the record names** — a policy this device has anchored
    /// itself, never one it was merely served.
    ///
    /// The anchor is the chain ([`fauna_mls::room_policy::CommunityPolicyChain`]):
    /// version 1 proves itself the founder's against the room id, and every
    /// later version proves it follows the one before. The home nest serves the
    /// links ([`RoomRosterReader::read_policy_version`]) and can withhold one —
    /// the record then stays unpainted — but cannot mint one, which is what
    /// keeps "the nest cannot forge a tombstone alone" true.
    ///
    /// A name designates its **verified line** (same section → *A name
    /// designates its verified line*): the name, and every successor after it
    /// that the registered [`SuccessionWitness`] verified by its own anchored
    /// walk ([`RoomSeats`]). Nothing is asked until a rank refusal a succession
    /// could lift; with no witness a name stands for itself and the judgment
    /// fails closed — refused outright. With one, a rank refusal parks: every
    /// verified line ([`RoomSeats::verified`], asked again every
    /// [`LINE_REASK_PASSES`] passes) may yet grow, and a name nothing anchors
    /// may yet be anchored.
    async fn floor_delete_verdict(
        &self,
        channel_id: &ChannelId,
        signed: &fauna_mls::room_policy::SignedRoomFloorDelete,
    ) -> FloorDeleteVerdict {
        let mut seats = self
            .room_seats
            .lock()
            .unwrap()
            .get(channel_id)
            .cloned()
            .unwrap_or_default();
        let mut waiting = Vec::new();
        let verdict = self
            .floor_delete_verdict_under(channel_id, signed, &mut seats, &mut waiting)
            .await;
        if matches!(verdict, FloorDeleteVerdict::NotYet) && !waiting.is_empty() {
            seats.waiting.insert(signed.signature.clone(), waiting);
        } else {
            seats.waiting.remove(&signed.signature);
        }
        self.room_seats.lock().unwrap().insert(*channel_id, seats);
        verdict
    }

    /// Ask the witness about one more of `names` — whichever could still lift a
    /// rank refusal — and say whether `seats` learned a line (the only news
    /// worth judging again for). `first_asks` is the judgment's dial budget
    /// ([`MAX_FIRST_SEAT_ASKS`]).
    async fn resolve_more_seats(
        &self,
        seats: &mut RoomSeats,
        names: &[ActorId],
        first_asks: &mut u32,
    ) -> bool {
        let Some(witness) = self.succession_witness() else {
            return false;
        };
        for name in names {
            let recheck = match seats.verified.get(name) {
                // Not due: the verified line stands for this pass.
                Some(asked_at) if seats.passes < asked_at + LINE_REASK_PASSES => continue,
                Some(_) => true,
                None => false,
            };
            // A first ask and a re-ask of a verified line may each dial; a
            // not-yet name's ask dials nothing and so spends no budget — its
            // anchor-store read is bounded by the witness's backoff instead
            // (see [`MAX_FIRST_SEAT_ASKS`]).
            if recheck || !seats.not_yet.contains(name) {
                if *first_asks == 0 {
                    continue;
                }
                *first_asks -= 1;
            }
            let line = if recheck {
                witness.recheck_line(name).await
            } else {
                witness.succession_line(name).await
            };
            match line {
                crate::backend::SuccessionLine::Verified(successors) => {
                    seats.not_yet.remove(name);
                    seats.verified.insert(*name, seats.passes);
                    // A line only grows: an answer no longer than the held
                    // line — the same line again, a shorter one, a fork — is
                    // no news.
                    if seats.lines.insert(*name, &successors) {
                        return true;
                    }
                }
                // A re-ask that reached nothing leaves the held answer
                // standing, due again a full interval on.
                crate::backend::SuccessionLine::NotYet if recheck => {
                    seats.verified.insert(*name, seats.passes);
                }
                crate::backend::SuccessionLine::NotYet => {
                    seats.not_yet.insert(*name);
                }
            }
        }
        false
    }

    /// [`Self::floor_delete_verdict`] over the room's [`RoomSeats`], which the
    /// caller loads and stores. A record parked on a rank refusal leaves the
    /// names it is waiting on in `waiting` ([`RoomSeats::waiting`]); one parked
    /// for want of a version waits on none.
    async fn floor_delete_verdict_under(
        &self,
        channel_id: &ChannelId,
        signed: &fauna_mls::room_policy::SignedRoomFloorDelete,
        seats: &mut RoomSeats,
        waiting: &mut Vec<ActorId>,
    ) -> FloorDeleteVerdict {
        use fauna_mls::room_policy::RoomRole;
        let mut first_asks = MAX_FIRST_SEAT_ASKS;
        if let Err(e) = signed.verify(&channel_id.0) {
            tracing::warn!("floor delete record refused on {channel_id}: {e}");
            return FloorDeleteVerdict::Refused;
        }
        let named = signed.record.policy_version;
        let chain = match self
            .anchor_policy_chain(channel_id, named, seats, &mut first_asks, waiting)
            .await
        {
            Ok(chain) => chain,
            Err(verdict) => return verdict,
        };
        let Some(policy) = chain.policy_at(named) else {
            return FloorDeleteVerdict::Refused;
        };
        let names = policy_names(&[policy]);
        loop {
            let role = fauna_mls::room_policy::community_role_of(
                policy,
                &signed.record.author,
                &seats.lines,
            );
            // A member's rank is the one a succession could still lift.
            if role != RoomRole::Member {
                return FloorDeleteVerdict::Ranked(role.into());
            }
            if !self
                .resolve_more_seats(seats, &names, &mut first_asks)
                .await
            {
                // With no witness a name stands for itself, for good. With
                // one, no line is ever settled: every verified line may yet
                // grow (its newest holder may succeed in turn) and a name
                // nothing anchors may yet be anchored, while a member's claim
                // is a verdict no later pass revisits — so the record parks
                // (bounded by the park cap) and is judged again.
                return if self.succession_witness().is_none() {
                    FloorDeleteVerdict::Ranked(role.into())
                } else {
                    *waiting = seats.still_waiting(&names);
                    FloorDeleteVerdict::NotYet
                };
            }
        }
    }

    /// The room's current policy as its floor names it — but **as this device
    /// anchored it**, never as the floor served it (`conversation-rooms.md`
    /// § Roles and authorization → *A fetched version is anchored, never
    /// believed*). The one policy a device renders in the editor and amends.
    ///
    /// The floor is the party serving the current policy, so its word for
    /// which room those bytes are is no check: the served bytes only say which
    /// version to walk to ([`Self::anchor_policy_chain`]), and the answer is
    /// the version the chain holds there. So a version another room's owner
    /// signed and the nest stripped of its room signature, or one no rank in
    /// this room ever signed, reads as `None` — as does a chain the nest
    /// withholds a link of, or one longer than a pass walks (the next read
    /// walks on from the proven prefix). A served blob that differs from the
    /// anchored version is ignored: the chain proved first stands.
    async fn anchored_room_policy(
        &self,
        channel_id: &ChannelId,
        floor: &crate::backend::RoomFloor,
    ) -> Option<fauna_mls::room_policy::SignedRoomPolicy> {
        let version = verified_room_policy(floor, channel_id)?.policy.version;
        let mut seats = self
            .room_seats
            .lock()
            .unwrap()
            .get(channel_id)
            .cloned()
            .unwrap_or_default();
        // What a rank park waits on is a floor delete record's to track; an
        // amend base that cannot be proven yet is simply not there yet.
        let (mut first_asks, mut waiting) = (MAX_FIRST_SEAT_ASKS, Vec::new());
        let chain = self
            .anchor_policy_chain(
                channel_id,
                version,
                &mut seats,
                &mut first_asks,
                &mut waiting,
            )
            .await;
        self.room_seats.lock().unwrap().insert(*channel_id, seats);
        chain.ok()?.signed_at(version).cloned()
    }

    /// Walk the room's anchored chain ([`Self::room_policy_chains`]) until it
    /// reaches version `through`: fetch each missing link from the home nest
    /// ([`RoomRosterReader::read_policy_version`]), anchor version 1 to the
    /// room id under the served birth salt, and extend by the one judge. The
    /// proven prefix is kept whatever the outcome. `Err` is why the chain does
    /// not reach `through` this pass — [`FloorDeleteVerdict::NotYet`] for a
    /// link not reachable yet (or the pass's fetch cap), `Refused` for one
    /// that does not anchor.
    async fn anchor_policy_chain(
        &self,
        channel_id: &ChannelId,
        through: u64,
        seats: &mut RoomSeats,
        first_asks: &mut u32,
        waiting: &mut Vec<ActorId>,
    ) -> std::result::Result<fauna_mls::room_policy::CommunityPolicyChain, FloorDeleteVerdict> {
        use fauna_mls::room_policy::{CommunityPolicyChain, SignedRoomPolicy};
        let named = through;
        let Some(reader) = self.room_roster_reader.get() else {
            return Err(FloorDeleteVerdict::Refused);
        };
        let mut chain = self
            .room_policy_chains
            .lock()
            .unwrap()
            .get(channel_id)
            .cloned();
        let mut verdict = None;
        let mut fetched = 0u64;
        while chain.as_ref().is_none_or(|c| c.head_version() < named) {
            if fetched == MAX_POLICY_VERSION_FETCHES {
                verdict = Some(FloorDeleteVerdict::NotYet);
                break;
            }
            fetched += 1;
            let want = chain.as_ref().map_or(1, |c| c.head_version() + 1);
            if let Some(held) = chain.as_mut()
                && let Some(link) = seats.pending.take_if(|p| p.policy.version == want)
            {
                match self
                    .extend_resolving(channel_id, held, link, seats, first_asks, waiting)
                    .await
                {
                    Ok(()) => continue,
                    Err(refused) => {
                        verdict = Some(refused);
                        break;
                    }
                }
            }
            let (policy, birth_salt) = match reader
                .read_policy_version(
                    channel_id.to_string(),
                    self.channel_home_url(channel_id),
                    want,
                )
                .await
            {
                crate::backend::RoomPolicyVersionRead::Served { policy, birth_salt } => {
                    (policy, birth_salt)
                }
                crate::backend::RoomPolicyVersionRead::NotHeld => {
                    verdict = Some(FloorDeleteVerdict::Refused);
                    break;
                }
                crate::backend::RoomPolicyVersionRead::Unavailable => {
                    verdict = Some(FloorDeleteVerdict::NotYet);
                    break;
                }
            };
            let Ok(link) = fauna_core::encoding::canonical_decode::<SignedRoomPolicy>(&policy)
            else {
                verdict = Some(FloorDeleteVerdict::Refused);
                break;
            };
            let extended = if let Some(held) = chain.as_mut() {
                match self
                    .extend_resolving(channel_id, held, link, seats, first_asks, waiting)
                    .await
                {
                    Ok(()) => continue,
                    Err(refused) => {
                        verdict = Some(refused);
                        break;
                    }
                }
            } else if let Some(salt) = birth_salt {
                CommunityPolicyChain::anchor(&channel_id.0, &salt, link)
                    .map(|anchored| chain = Some(anchored))
                    .map_err(|e| e.to_string())
            } else {
                Err("the room's birth salt was not served".to_string())
            };
            if let Err(e) = extended {
                tracing::warn!("policy version {want} does not anchor on {channel_id}: {e}");
                verdict = Some(FloorDeleteVerdict::Refused);
                break;
            }
        }
        // Whatever prefix this pass proved stays proven — the next record, or
        // this one's retry, starts from it.
        if let Some(chain) = &chain {
            let mut chains = self.room_policy_chains.lock().unwrap();
            if chains
                .get(channel_id)
                .is_none_or(|held| held.head_version() < chain.head_version())
            {
                chains.insert(*channel_id, chain.clone());
            }
        }
        if let Some(verdict) = verdict {
            return Err(verdict);
        }
        chain.ok_or(FloorDeleteVerdict::Refused)
    }

    /// Extend `chain` by `link`, resolving successions only as a rank refusal
    /// asks for them. `Err` is the judgment's verdict: a rank refusal parks
    /// ([`FloorDeleteVerdict::NotYet`], the link kept as [`RoomSeats::pending`])
    /// while a witness is registered — every line may yet grow — and is
    /// refused outright with none; every other refusal is final. A park leaves
    /// the names it waits on in `waiting` — both policies' — which
    /// [`Self::moderation_unverified`] cuts to the anchored chain's.
    async fn extend_resolving(
        &self,
        channel_id: &ChannelId,
        chain: &mut fauna_mls::room_policy::CommunityPolicyChain,
        link: fauna_mls::room_policy::SignedRoomPolicy,
        seats: &mut RoomSeats,
        first_asks: &mut u32,
        waiting: &mut Vec<ActorId>,
    ) -> std::result::Result<(), FloorDeleteVerdict> {
        use fauna_mls::room_policy::PolicyStepRefusal;
        // Both policies' names: a step may carry a seat across under its
        // successor's name, or back under its predecessor's, and the floor
        // admits either as the same seat.
        let names = policy_names(&[chain.head(), &link.policy]);
        loop {
            let refusal = match chain.try_extend(link.clone(), &seats.lines) {
                Ok(()) => return Ok(()),
                Err(refusal) => refusal,
            };
            if matches!(refusal, PolicyStepRefusal::Rank(_))
                && self.resolve_more_seats(seats, &names, first_asks).await
            {
                continue;
            }
            let version = link.policy.version;
            if matches!(refusal, PolicyStepRefusal::Rank(_)) && self.succession_witness().is_some()
            {
                *waiting = seats.still_waiting(&names);
                seats.pending = Some(link);
                return Err(FloorDeleteVerdict::NotYet);
            }
            tracing::warn!("policy version {version} does not anchor on {channel_id}: {refusal}");
            return Err(FloorDeleteVerdict::Refused);
        }
    }

    /// Fold one floor delete record off the room log: judged
    /// ([`Self::floor_delete_verdict`]), then recorded as a delete claim with
    /// the rank its author held — or parked, or dropped. **Unverified, nothing
    /// is painted.** The claim's projection still decides
    /// ([`crate::message::DeleteClaim::admits`]): a plain member's record the
    /// floor somehow admitted is recorded as a member's and tombstones nothing.
    ///
    /// Repaints the room when its [`Self::moderation_unverified`] turns — a
    /// fact no ingest carries — and only then.
    async fn fold_floor_delete(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
        parked: ParkedFloorDelete,
    ) {
        let was = self.moderation_unverified(channel_id);
        self.judge_floor_delete(manager, channel_id, parked).await;
        self.announce_if_turned(manager, channel_id, was);
    }

    /// [`Self::fold_floor_delete`] without the announcement — for
    /// [`Self::retry_parked_floor_deletes`], which re-parks a room's records
    /// one by one and announces once, for the whole pass.
    async fn judge_floor_delete(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
        parked: ParkedFloorDelete,
    ) {
        let verdict = self.floor_delete_verdict(channel_id, &parked.signed).await;
        self.offer_policy_names_to_harvest(manager, channel_id);
        match verdict {
            FloorDeleteVerdict::Ranked(role) => manager.apply_inbound_delete_claim(
                MessageId(format!(
                    "conv:{channel_id}:{}",
                    parked.signed.record.target_seq
                )),
                crate::message::DeleteClaim {
                    claimant: parked.signed.record.author,
                    delete_seq: parked.delete_seq,
                    role: Some(role),
                },
            ),
            FloorDeleteVerdict::Refused => {}
            FloorDeleteVerdict::NotYet => {
                let held: HashSet<Vec<u8>> = {
                    let mut all = self.parked_floor_deletes.lock().unwrap();
                    let room = all.entry(*channel_id).or_default();
                    if room.len() == MAX_PARKED_FLOOR_DELETES {
                        room.remove(0);
                    }
                    room.push(parked.clone());
                    room.iter().map(|r| r.signed.signature.clone()).collect()
                };
                // A record the cap just dropped waits on nothing any more.
                if let Some(seats) = self.room_seats.lock().unwrap().get_mut(channel_id) {
                    seats
                        .waiting
                        .retain(|signature, _| held.contains(signature));
                }
                // And at rest, so a quit before the next pass does not lose
                // it: the walk has already stepped past it for good
                // ([`Self::parked_floor_deletes`]).
                manager.park_floor_delete(*channel_id, parked);
            }
        }
    }

    /// Whether `channel_id` carries moderation this device could not verify —
    /// [`crate::room::RoomSnapshot::moderation_unverified`]: a parked floor
    /// delete record ([`Self::parked_floor_deletes`]) is still waiting on a
    /// name ([`RoomSeats::waiting`]) that the chain this device **anchored**
    /// for the room carries ([`Self::room_policy_chains`]) and that the harvest
    /// has settled this session without an anchor ([`Self::harvest_settled`])
    /// — so the wait is not going to end before the session does.
    ///
    /// - **The record's own names**, never the room's running tally: a name
    ///   asked for an earlier record says nothing about a later one parked
    ///   only for want of a version, which is *not yet* judged.
    /// - **The anchored chain's names.** A park also waits on the names of the
    ///   served, still-unanchored next version — bytes the home nest chose —
    ///   and the harvest's roster walk may settle one of them; that is the
    ///   nest's say-so, never the room's, so it cannot raise the statement.
    ///   The full chain, not the first [`MAX_POLICY_NAME_OFFERS`] the harvest
    ///   is offered: a later name settled without an anchor is as unverifiable.
    /// - **Any** such name, not every: the served version's names never count,
    ///   so a rule waiting on all of them would keep a room silent for ever.
    pub fn moderation_unverified(&self, channel_id: &ChannelId) -> bool {
        let parked: Vec<Vec<u8>> = match self.parked_floor_deletes.lock().unwrap().get(channel_id) {
            Some(room) => room.iter().map(|r| r.signed.signature.clone()).collect(),
            None => return false,
        };
        if parked.is_empty() {
            return false;
        }
        let anchored = match self.room_policy_chains.lock().unwrap().get(channel_id) {
            Some(chain) => policy_names(&chain.policies().collect::<Vec<_>>()),
            None => return false,
        };
        let settled = self.harvest_settled.lock().unwrap().clone();
        let seats = self.room_seats.lock().unwrap();
        let Some(seats) = seats.get(channel_id) else {
            return false;
        };
        parked
            .iter()
            .filter_map(|signature| seats.waiting.get(signature))
            .flatten()
            .any(|name| anchored.contains(name) && settled.contains(name))
    }

    /// Every room whose [`Self::moderation_unverified`] holds now — what a
    /// harvest settle compares before and after, to repaint only on a turn.
    fn moderation_unverified_rooms(&self) -> HashSet<ChannelId> {
        let rooms: Vec<ChannelId> = self
            .parked_floor_deletes
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect();
        rooms
            .into_iter()
            .filter(|room| self.moderation_unverified(room))
            .collect()
    }

    /// Repaint `channel_id` if its [`Self::moderation_unverified`] is no longer
    /// `was` — either way, since the statement going is as much news as its
    /// coming, and never otherwise: a re-park that changes nothing says
    /// nothing.
    fn announce_if_turned(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
        was: bool,
    ) {
        if self.moderation_unverified(channel_id) != was {
            manager.notify_room_state_changed();
        }
    }

    /// Offer the peer-anchor harvest the policy names this room still wants an
    /// anchor for (`identity-succession.md` § The succession statement → *a
    /// community policy's names join the harvest's walk*): the names a rank
    /// refusal asked the witness about and found **no anchor yet** for.
    ///
    /// **Only names an anchored version carries.** [`RoomSeats::not_yet`] also
    /// holds the names of the version still waiting on the refusal — bytes the
    /// room's home nest served and nobody on a verified line signed — and
    /// offering those would let that nest choose whom this device fetches and
    /// spend its bounded anchor store. Chain order, so an owner precedes the
    /// admins and version 1's names precede a later version's; bounded to the
    /// chain's first [`MAX_POLICY_NAME_OFFERS`] names — the cut comes BEFORE
    /// the `not_yet` filter, see the constant. Nothing is fetched here: the
    /// sweep harvests on its own next pass, and the parked record is judged
    /// again on the next poll, exactly as for a thread peer.
    fn offer_policy_names_to_harvest(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
    ) {
        let not_yet = match self.room_seats.lock().unwrap().get(channel_id) {
            Some(seats) => seats.not_yet.clone(),
            None => return,
        };
        let mut names = match self.room_policy_chains.lock().unwrap().get(channel_id) {
            Some(chain) => policy_names(&chain.policies().collect::<Vec<_>>()),
            None => Vec::new(),
        };
        // Truncate, THEN filter: the other order is a rolling window.
        names.truncate(MAX_POLICY_NAME_OFFERS);
        names.retain(|name| not_yet.contains(name));
        manager.set_policy_anchor_wants(*channel_id, names);
    }

    /// Judge again every floor delete record an earlier pass had to park
    /// ([`Self::parked_floor_deletes`]) — this session's, and the ones the
    /// replica restored into the manager from a session that quit with them
    /// still parked. Free when the room parked none.
    ///
    /// The at-rest copy stays a superset while the pass runs (each re-park is
    /// a union into it) and is narrowed to the survivors only at the end, so
    /// a slice written mid-pass carries at worst a record the pass was about
    /// to judge anyway, never one short.
    async fn retry_parked_floor_deletes(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
    ) {
        // One inbound pass on this room: the clock a verified line's re-ask
        // runs by. Only a room some judgment already asked about has one.
        if let Some(seats) = self.room_seats.lock().unwrap().get_mut(channel_id) {
            seats.passes += 1;
        }
        let was = self.moderation_unverified(channel_id);
        let mut parked = self
            .parked_floor_deletes
            .lock()
            .unwrap()
            .remove(channel_id)
            .unwrap_or_default();
        crate::store::history::union_parked_floor_deletes(
            &mut parked,
            manager.parked_floor_deletes(channel_id),
        );
        if parked.is_empty() {
            return;
        }
        for record in parked {
            self.judge_floor_delete(manager, channel_id, record).await;
        }
        let survivors = self
            .parked_floor_deletes
            .lock()
            .unwrap()
            .get(channel_id)
            .cloned()
            .unwrap_or_default();
        manager.set_parked_floor_deletes(*channel_id, survivors);
        self.announce_if_turned(manager, channel_id, was);
    }

    /// Whether a **confirmed** floor read has said this account holds no live
    /// seat on `channel_id` — the one fact [`Self::room_invitations`] trusts
    /// to tell a genuinely spent invitation from a re-invitation after a
    /// removal (). `false` for a channel this session has never
    /// tended (presumed still seated, today's default) and for one whose last
    /// confirmed read seated this device.
    fn confirmed_unseated(&self, channel_id: &ChannelId) -> bool {
        self.unseated_rooms.lock().unwrap().contains(channel_id)
    }

    /// Whether `channel_id` is still owed a backfill attempt this session,
    /// spending its one attempt when it is.
    fn floor_backfill_due(&self, channel_id: &ChannelId) -> bool {
        self.floor_backfill.lock().unwrap().insert(*channel_id)
    }

    /// Give a room whose birth report never landed the floor roster it is
    /// owed (`conversation-rooms.md` § The floor roster → *End-to-end rooms*:
    /// "every room has a nest-side roster on its home nest").
    ///
    /// The birth report fires once, inside the send that bootstraps the group
    /// ([`Self::send`]), and it is best-effort: [`Self::report_roster`] returns
    /// silently when no reporter is wired, when the report goes undelivered
    /// (transport down) or when the policy read fails, and a crash between the
    /// first post and the report loses it outright. A room never bootstraps
    /// again, so such a room gets a floor only from its next membership or
    /// policy commit — and a **1:1 never gets one at all**, because it carries
    /// no such commit (its add-participant forks a new group). Those rooms
    /// leave the nest's custody serve door failing closed and a Welcome-seated
    /// peer's handle read refused on every poll. A current build still
    /// produces the shape, so this is crash and delivery recovery, not a
    /// pre-sweep remnant — ruled LIVE 2026-10-02 under the compat-remnant
    /// sweep (`compat-remnant-sweep.md`).
    ///
    /// This is the mirror of [`Self::tend_community_room`], which serves the
    /// group-LESS channels: this one serves the channels this device holds a
    /// group for, and each returns early on the other's case.
    ///
    /// ⚠ **The ordering trap, and why the read is the gate.** The report this
    /// sends names no log position — the group's creation is not on the room
    /// log, so there is nothing to name — and `commit_seq: None` is exactly
    /// what makes the home nest skip its supersede check and take the report
    /// wholesale. Sent to a room whose floor is already NEWER, that is a
    /// rollback. So the backfill is gated on a roster read that came back
    /// [`RoomRosterRead::NoFloor`] — a *confirmed* "no floor exists" or "not a
    /// member of it", indistinguishable here by design (the door answers a
    /// clean `permission_denied` to both), and both are safe: the first is the
    /// gap this closes, and the second the nest refuses on its own, because a
    /// report against an existing floor is admitted only from a live member.
    /// [`RoomRosterRead::Unavailable`] — the read simply failing to reach an
    /// answer — is NOT this case and must not be treated as one: a transient
    /// failure is not a confirmed-empty floor, and reporting on its strength
    /// is exactly 's rollback (`Self::floor_backfill_due`'s attempt
    /// is un-spent so a later poll gets a real answer instead).
    ///
    /// No carve-out in [`Self::report_roster`] is needed or wanted: it already
    /// accepts a policy-less room and reports a role-less roster for it. What
    /// was missing was only a trigger.
    ///
    /// ⚠ **Outside the channel lock**, like its two neighbours and for their
    /// reason: it awaits nest round trips. Best-effort by contract, and at most
    /// one attempt per channel per session ([`Self::floor_backfill_due`]) — a
    /// room that confirms it has no floor is not asked again this session, and
    /// one that gains a floor answers [`RoomRosterRead::Floor`] on the next
    /// launch and is skipped for good.
    pub async fn backfill_floor_roster(&self, channel_id: &ChannelId) {
        // Only a device that HOLDS the group can report its roster; a
        // group-less channel is `tend_community_room`'s case, not this one.
        if !self.engine.has_group(channel_id) {
            return;
        }
        if self.room_roster_reader.get().is_none() || self.room_roster_reporter.get().is_none() {
            return;
        }
        // Spend the attempt BEFORE the read, so an answerless room costs one
        // read per session rather than one per poll. Un-spent again below if
        // the read comes back `Unavailable` rather than a confirmed answer.
        if !self.floor_backfill_due(channel_id) {
            return;
        }
        let Some(reader) = self.room_roster_reader.get() else {
            return;
        };
        match reader
            .read_roster(channel_id.to_string(), self.channel_home_url(channel_id))
            .await
        {
            RoomRosterRead::Floor(_) => {
                // A floor exists and this device is on it — the room is
                // already served, and an unpositioned report here would roll
                // it back.
                return;
            }
            RoomRosterRead::Unavailable => {
                // The read failed rather than confirming an empty floor —
                // leave the attempt unspent so a later poll, not this
                // transient blip, decides whether to report.
                self.floor_backfill.lock().unwrap().remove(channel_id);
                return;
            }
            RoomRosterRead::NoFloor => {}
        }
        self.report_roster(channel_id, None).await;
    }

    /// Tend a **community room** after its walk — the per-poll step the class
    /// needs and an end-to-end room does not, because its membership changes
    /// happen on the home nest rather than in a commit this device folds.
    ///
    /// On a bounded cadence ([`Self::FLOOR_REFRESH_POLLS`], sooner after this
    /// device's own room act) it re-reads the floor and:
    ///
    /// 1. refreshes what the thread header, the chips and the editor render —
    ///    class, ranks, policy, and whether the home nest reads;
    /// 2. seats the floor's users as the thread's participants and names them
    ///    from the same read. An invitation seats nobody; acceptance does, on
    ///    the nest, where no walk sees it — so an invitee becomes a chip once
    ///    they are a member and not before;
    /// 3. when this device ranks owner or admin, **keys in** every user the
    ///    floor seated with no wrap for the room's current generation — the
    ///    inviter's half of the join (`conversation-rooms.md` § Join rules and
    ///    invites: "the inviter's device wraps the generation bundle to the
    ///    newcomer"), which until this ran nowhere. Any owner or admin device
    ///    may do it, and a second one covering the same newcomer is harmless:
    ///    the nest stores a top-up wrap idempotently.
    ///
    /// ⚠ **Outside the channel lock**, like [`Self::resolve_nameless_members`]
    /// and for its reason: it awaits nest round trips. Best-effort by contract
    /// — a read or a key-in that fails is retried on a later poll, and nothing
    /// is surfaced to the user from a background pass.
    pub async fn tend_community_room(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
    ) {
        // An MLS room decides its class and membership locally, off commits it
        // folds; only a group-less channel needs the floor.
        if self.engine.has_group(channel_id) {
            return;
        }
        let Some(thread_id) = self.thread_for_channel(channel_id) else {
            return;
        };
        let Some(reader) = self.room_roster_reader.get() else {
            return;
        };
        if !self.floor_refresh_due(channel_id) {
            return;
        }
        // The countdown restarts whether or not the read lands: a room this
        // account has LEFT stays bound to its thread (the device keeps the
        // bubbles it holds), and its floor answers "not a member" for good — a
        // retry on every poll would be a refused read per poll forever.
        self.floor_refresh
            .lock()
            .unwrap()
            .insert(*channel_id, Self::FLOOR_REFRESH_POLLS);
        let floor = match reader
            .read_roster(channel_id.to_string(), self.channel_home_url(channel_id))
            .await
        {
            RoomRosterRead::Floor(floor) => floor,
            // A CONFIRMED "not a member" — this device's own removal, or a
            // room that never existed. Either way, record it: `room_invitations`
            // trusts this fact (never `Self::room_floors`, which a removal never
            // clears) to tell "still spent" from "worth offering again"
            // (). A later poll clears the mark the moment the floor
            // seats this device again — re-admission is exactly the case this
            // exists for.
            RoomRosterRead::NoFloor => {
                self.unseated_rooms.lock().unwrap().insert(*channel_id);
                return;
            }
            // The read failed rather than confirming anything — leave whatever
            // this device last knew (seated or not) exactly as it was; a
            // transient blip must never flip either side of that fact.
            RoomRosterRead::Unavailable => return,
        };
        self.unseated_rooms.lock().unwrap().remove(channel_id);
        self.cache_floor(channel_id, &floor).await;

        // 0. This device's OWN seat first: keyable, and keyed to this
        //    account's current wrap target. A seat the floor holds keyless
        //    (a succession's successor, a member seated before the sealing
        //    plane) or under a key this account has rotated past is healed
        //    here, before anything else — a governing seat that cannot open
        //    the room cannot key anybody else into it either.
        if let Some(own) = floor
            .members
            .iter()
            .find(|m| m.actor == self.self_actor && m.kind == RoomPrincipalKind::User)
            && let Err(e) = self.heal_own_room_seat(channel_id, own).await
        {
            tracing::warn!(
                channel = %channel_id,
                "supplying this account's wrap target for its community-room seat failed; \
                 retried on a later poll: {e}"
            );
        }

        let users: Vec<ActorId> = floor
            .members
            .iter()
            .filter(|m| m.kind == RoomPrincipalKind::User)
            .map(|m| m.actor)
            .collect();
        manager.apply_inbound_roster(thread_id.clone(), &users, &[]);
        let named: Vec<(ActorId, String)> = floor
            .members
            .iter()
            .filter_map(|m| Some((m.actor, m.qualified_handle()?)))
            .collect();
        if !named.is_empty() {
            manager.apply_resolved_handles(thread_id, &named);
        }

        // What stands pending on the room, on the floor's own cadence: every
        // seated member is served a list (the nest scopes it), and an
        // invitation accepted, lapsed or withdrawn from another device leaves
        // this one's screen within the same interval the roster follows at.
        if self.refresh_pending_room_invites(channel_id).await {
            manager.notify_room_state_changed();
        }

        let governs = floor
            .members
            .iter()
            .find(|m| m.actor == self.self_actor)
            .and_then(|m| m.role)
            .is_some_and(|role| role.is_admin_or_owner());
        if !governs {
            return;
        }
        for member in &floor.members {
            let owed = member.kind == RoomPrincipalKind::User
                && member.actor != self.self_actor
                && member.tip_wrapped == Some(false)
                && member.wrap_target().is_some();
            if !owed {
                continue;
            }
            match self.key_in_room_member(channel_id, member.actor).await {
                // Read the floor again next poll, so the covered newcomer stops
                // reading as owed and the render follows.
                Ok(()) => self.refresh_floor_soon(channel_id),
                Err(e) => {
                    tracing::warn!(
                        channel = %channel_id,
                        "keying a newly seated member into a community room failed; retried on a \
                         later poll: {e}"
                    );
                }
            }
        }
    }

    /// Make this account's own seat on `channel_id`'s floor **keyable, and
    /// keyed to its current wrap target** — the client half of
    /// `fauna.conversations.room.set_reception_key`
    /// (`community-rooms.md` § Implementation status today, *A seat gains or
    /// rotates its wrap target*). `own` is this account's row as the floor
    /// read just served it.
    ///
    /// Three seats reach here owing something, and one act serves all three:
    ///
    /// - **A successor's seat.** The succession ceremony seats it with no key —
    ///   the predecessor's belongs to the material the ceremony retired — so
    ///   it governs at once and opens nothing minted after it. This account's
    ///   plane is its own (the predecessor's reception records are sealed to a
    ///   `BackupKey` schedule this identity does not hold), so the current key
    ///   here is a **fresh** one, minted on the first pass and never the
    ///   predecessor's: exactly the fresh published half the scheme's
    ///   fleet-severance rule asks for.
    /// - **A seat from before the sealing plane**, which has no key and no
    ///   entry; the door mints the entry with the key.
    /// - **A rotated account**: the floor holds the key of an earlier moment.
    ///   The scheme's wrap target is the member's *current* key, so the seat
    ///   is re-bound — on the same entry, since the retained keys still open
    ///   what was sealed to it.
    ///
    /// Then, when this seat ranks **owner or admin**: if the door answers a
    /// tip it holds no wrap for, it mints a fresh generation parented on that
    /// tip — the severance mint a succession's `Removed` row calls for
    /// (`account-data-taxonomy.md` § The recipient-set scheme → *Mint
    /// triggers*, trigger 1), and also how a seat that is the room's only key
    /// authority ever reads again, since nobody else can back it in; the tip
    /// comes from the door's own answer because `room.generations` shows an
    /// uncovered seat nothing. If instead the door reports a **rotation** of
    /// a covered seat, it rotates the room ([`Self::rotate_room_key`]) —
    /// trigger (2), the member's fleet-severance signal, so new traffic seals
    /// to the current key. A plain member mints in neither case: an owner's or
    /// admin's [`Self::tend_community_room`] keys an uncovered one in on its
    /// next pass, the way it covers any seated-but-unwrapped newcomer, and a
    /// plain member's rotation waits for a key authority's next mint.
    ///
    /// A seat that is already current and covered costs nothing here — no
    /// round trip. Returns whether the door was called.
    ///
    /// # Errors
    /// [`BackendError::Internal`] when this device could not persist a freshly
    /// minted key (the seat is left keyless rather than keyed to a secret that
    /// never reached disk), the door's refusal, or a mint refused by the nest.
    /// An app with no reception-key or ceremony seam — web's declared absence
    /// — answers `Ok(false)` and leaves the seat for another device.
    async fn heal_own_room_seat(
        &self,
        channel_id: &ChannelId,
        own: &crate::backend::RoomRosterKnownMember,
    ) -> Result<bool, BackendError> {
        let (Some(keys), Some(ceremony)) =
            (self.group_reception_keys.get(), self.room_ceremony.get())
        else {
            return Ok(false);
        };
        let governs = own.role.is_some_and(|role| role.is_admin_or_owner());
        // Minting a first key only when the account holds none — the ordinary
        // "already keyed" pass must not cost a write.
        let current = match keys.reception_keys().await.into_iter().next() {
            Some(held) => held,
            None => self.current_reception_key().await?,
        };
        let current_pubkey =
            self_check_reception_pubkey(current.reception_pubkey().map_err(|e| {
                BackendError::Internal(format!(
                    "this account's group-reception key is corrupt: {e}"
                ))
            })?)?;
        let keyed_current = own.reception_pubkey.as_deref() == Some(current_pubkey.as_slice());
        // A governing seat the tip is known not to cover re-asks the door even
        // when its key is current: a mint that failed on an earlier pass (a
        // concurrent rotation moved the tip) has no other way to learn the
        // parent it must name now.
        let owes_own_mint = governs && own.tip_wrapped == Some(false);
        if keyed_current && !owes_own_mint {
            return Ok(false);
        }
        let bound = ceremony
            .room_set_reception_key(channel_id.to_string(), current_pubkey)
            .await?;
        // The floor's own row has moved — re-read it sooner than the cadence.
        self.refresh_floor_soon(channel_id);
        if governs {
            if let Some(tip) = bound.uncovered_tip {
                // Uncovered: the tip's name comes from the door, because the
                // generations read shows this seat nothing.
                self.mint_room_generation(channel_id, vec![tip], NestRead::Keep)
                    .await?;
            } else if bound.rotated {
                // Covered under the key of an earlier moment: the scheme's
                // trigger (2) — a published rotation re-mints, so new traffic
                // seals to the current key and a device that held the old
                // secret is severed from it. The seat still holds the old
                // wrap, so the ordinary rotation can name the tip itself.
                self.rotate_room_key(channel_id).await?;
            }
        }
        Ok(true)
    }

    /// Name the seated members of `channel_id`'s thread that this device has
    /// **never met** — the id-keyed handle read
    /// (`conversation-rooms.md` § Implementation status today, the roster
    /// bullet). The network counterpart of
    /// [`ConversationsManager::seat_address_for`]'s device-local scan: that
    /// one resolves a member already rendered in another thread here, this one
    /// resolves everybody else.
    ///
    /// ⚠ **Call it OUTSIDE the channel lock, never inside the walk.** It
    /// awaits a nest round trip, and `poll_inbound_conv` runs under the
    /// caller-held [`Self::channel_lock`]; holding that across a network read
    /// would stall every concurrent gated commit on this channel. It is a
    /// pure display refinement — nothing downstream waits on it — so the
    /// caller drops the guard first and calls this after
    /// (`session::poll_bound`).
    ///
    /// Best-effort and silent by contract: no reader registered, no thread
    /// bound, no nameless member, a nest that cannot answer, or a room whose
    /// members are all foreign — every one of those leaves the row rendering
    /// its elided actor id, which is the honest fallback
    /// ([`crate::address::TypedAddress::display`]) rather than an error worth
    /// showing anyone.
    pub async fn resolve_nameless_members(
        &self,
        manager: &ConversationsManager,
        channel_id: &ChannelId,
    ) {
        let Some(reader) = self.room_roster_reader.get() else {
            return;
        };
        let Some(thread_id) = self.thread_for_channel(channel_id) else {
            return;
        };
        // Only actors still rendering as an elided id, minus the ones a
        // successful read has answered for — a named member for good, a
        // listed-but-nameless one until its gap of polls has elapsed (the
        // gap is spent here, one poll per call) — so the steady state of a
        // room whose foreign member's home nest never announces a handle
        // is one read per `NAMELESS_REASK_CAP` polls, not one per poll.
        let pending: Vec<ActorId> = {
            let mut reads = self.roster_reads.lock().unwrap();
            let answers = reads.entry(*channel_id).or_default();
            manager
                .nameless_participants(&thread_id)
                .into_iter()
                .filter(|a| match answers.get_mut(a) {
                    None => true,
                    // Stale by construction: `pending` draws only on actors
                    // rendering nameless NOW, so a member cached `Named` who is
                    // nameless again was re-seated since that read — removed,
                    // then re-admitted, and the reconcile's add arm seats a
                    // re-admission handle-less. Filtering them out pinned the
                    // re-admitted member elided for the rest of the session.
                    Some(RosterAnswer::Named) => true,
                    Some(RosterAnswer::Nameless { skip, .. })
                    | Some(RosterAnswer::Omitted { skip, .. }) => {
                        if *skip == 0 {
                            true
                        } else {
                            *skip -= 1;
                            false
                        }
                    }
                })
                .collect()
        };
        // A group-less channel whose floor this device has never read still
        // needs one read: its CLASS is unknowable locally, and `room_state`
        // renders that class on the thread header. Bounded to once per such
        // channel per session — an MLS room decides its own class locally, and
        // a cached floor is not re-read here (a membership change reaches the
        // render through the same walk that seats it).
        let floor_needed = !self.engine.has_group(channel_id)
            && !self.room_floors.lock().unwrap().contains_key(channel_id);
        if pending.is_empty() && !floor_needed {
            return;
        }
        // The home pick is made HERE, where the channel's recorded home is
        // known — the `roster_holds` precedent. A room homed on another nest
        // holds its floor roster there and nowhere else
        // (`conversation-rooms.md` § The home nest), so this device's own nest
        // has no roster to answer with: without the URL the read comes back
        // `permission_denied` ("not a member of this room") and every
        // co-member on a foreign-homed room stays elided forever.
        let Some(floor) = reader
            .read_roster(channel_id.to_string(), self.channel_home_url(channel_id))
            .await
            .or_absent()
        else {
            // A read that failed records nothing: the next poll retries.
            return;
        };
        // The third consumer of this one read, in the spirit the comment below
        // states: the class, the ranks and the policy a synchronous render
        // needs. Stored for a group-less channel only — an MLS room's class is
        // decided locally and correctly, and caching a mirror roster's roles
        // over the group context's would be a downgrade.
        if !self.engine.has_group(channel_id) {
            self.cache_floor(channel_id, &floor).await;
        }

        let members = floor.members;
        // Record only the actors the roster actually SPOKE ABOUT — the ones it
        // listed, whether or not it had a handle for them. An actor the reply
        // omitted was not answered "no handle"; it was not answered at all,
        // because the floor roster is a member-reported mirror and this device
        // routinely folds a membership commit BEFORE the committing device's
        // report of it reaches the nest. Marking such an actor answered would
        // pin the newest member — the one this whole path exists for — as
        // permanently elided on the strength of a roster that predates them.
        //
        // A listed actor WITH a handle is named for the session. One listed
        // WITHOUT is "not yet": a member homed on another nest is named by an
        // announce its own home nest makes on that member's next drain, which
        // may simply not have happened yet — so it is asked again, at a gap
        // that doubles per nameless answer up to `NAMELESS_REASK_CAP` polls.
        // An actor the reply omits ENTIRELY gets the same doubling gap, one
        // omission later: the first miss is free (the committing device
        // usually reports within seconds), and only the second and later
        // consecutive omissions back off — `RosterAnswer::omitted`. Bounded
        // either way: the gap keeps a never-announcing home nest (a non-conforming
        // one) or a never-reporting one from costing a read per poll, and
        // the retry keeps a member who first drains late from staying elided
        // for the rest of this session.
        // The user-only narrowing lives HERE rather than in the glue's
        // projection, which used to do it for every consumer: a room's nest
        // and bridge principals hold no handle and are not participants any
        // thread renders, but they ARE what the class derives from and what a
        // generation mint wraps to, so the read carries them and each consumer
        // takes the rows it is about.
        let listed: HashMap<ActorId, bool> = members
            .iter()
            .filter(|m| m.kind == RoomPrincipalKind::User)
            .map(|m| (m.actor, m.qualified_handle().is_some()))
            .collect();
        {
            let mut reads = self.roster_reads.lock().unwrap();
            let answers = reads.entry(*channel_id).or_default();
            for actor in &pending {
                match listed.get(actor) {
                    Some(true) => {
                        answers.insert(*actor, RosterAnswer::Named);
                    }
                    Some(false) => {
                        let misses = match answers.get(actor) {
                            Some(RosterAnswer::Nameless { misses, .. }) => misses + 1,
                            _ => 1,
                        };
                        answers.insert(*actor, RosterAnswer::nameless(misses));
                    }
                    // Not "no handle" — not answered at all (the comment
                    // above `listed`). Its own doubling gap, one omission
                    // later than the listed-but-nameless case: see
                    // `RosterAnswer::Omitted`.
                    None => {
                        let misses = match answers.get(actor) {
                            Some(RosterAnswer::Omitted { misses, .. }) => misses + 1,
                            _ => 1,
                        };
                        answers.insert(*actor, RosterAnswer::omitted(misses));
                    }
                }
            }
        }
        let resolved: Vec<(ActorId, String)> = members
            .iter()
            .filter_map(|m| Some((m.actor, m.qualified_handle()?)))
            .collect();
        if resolved.is_empty() {
            return;
        }
        manager.apply_resolved_handles(thread_id, &resolved);
    }

    /// Clear `channel_id`'s [`RosterAnswer::Omitted`] entries — called from
    /// [`reconcile_roster`] on every advanced inbound commit, whatever it
    /// changed. The reconcile's own add arm cannot do this: it skips every
    /// actor already seated (`ConversationsManager::apply_inbound_roster`),
    /// which is exactly the state an omitted member is in after their first
    /// appearance, so a per-actor "reset on re-seat" never fires for them
    /// again. A channel-wide reset on every advance does, and it is the
    /// right trigger anyway — the committing device typically reports within
    /// seconds of ITS OWN commit, so any fresh commit on the channel is
    /// exactly when a gap built from stale omissions should not be trusted
    /// to still describe the room. `Named` and `Nameless` entries are left
    /// alone: neither backs off on a commit-driven cadence — a `Named`
    /// answer is stale only when the member renders nameless again, and a
    /// `Nameless` one waits on that member's own home nest, not on this
    /// channel's membership events.
    fn clear_omitted_roster_reads(&self, channel_id: &ChannelId) {
        if let Some(answers) = self.roster_reads.lock().unwrap().get_mut(channel_id) {
            answers.retain(|_, answer| !matches!(answer, RosterAnswer::Omitted { .. }));
        }
    }

    /// The governed room's policy for `channel_id`, or the product refusal a
    /// policy-less room answers a policy gesture with.
    fn governed_policy(&self, channel_id: &ChannelId) -> Result<RoomPolicyExtension, BackendError> {
        match self.engine.room_policy(channel_id) {
            Some(Ok(policy)) => Ok(policy),
            Some(Err(e)) => Err(BackendError::Internal(format!(
                "the room's policy extension does not decode: {e}"
            ))),
            None => Err(BackendError::Refusal(
                fauna_i18n::strings::error::send::ROOM_POLICY_UNAVAILABLE.to_string(),
            )),
        }
    }

    /// The roles table's invite row (`conversation-rooms.md` § Roles and
    /// authorization), asked BEFORE any commit is authored so an honest app
    /// never posts an Add every other member refuses. A policy-less room (no
    /// policy) is open.
    fn ensure_may_invite(&self, channel_id: &ChannelId) -> Result<(), BackendError> {
        let Some(Ok(policy)) = self.engine.room_policy(channel_id) else {
            return Ok(());
        };
        let role = policy.role_of(&self.self_actor);
        if role.is_admin_or_owner()
            || policy.signed.policy.join_rule == fauna_mls::room_policy::JoinRule::MemberInvite
        {
            return Ok(());
        }
        Err(BackendError::Refusal(
            fauna_i18n::strings::error::send::ROOM_INVITE_NOT_PERMITTED.to_string(),
        ))
    }

    /// The roles table's remove row: owner and admins remove, nobody removes
    /// the owner. Asked before any commit is authored, like
    /// [`Self::ensure_may_invite`].
    fn ensure_may_remove(
        &self,
        channel_id: &ChannelId,
        target: &ActorId,
    ) -> Result<(), BackendError> {
        let Some(Ok(policy)) = self.engine.room_policy(channel_id) else {
            return Ok(());
        };
        use fauna_i18n::strings::error::send;
        if !policy.role_of(&self.self_actor).is_admin_or_owner() {
            return Err(BackendError::Refusal(
                send::ROOM_REMOVE_NOT_PERMITTED.to_string(),
            ));
        }
        if policy.role_of(target) == fauna_mls::room_policy::RoomRole::Owner {
            return Err(BackendError::Refusal(
                send::ROOM_OWNER_NOT_REMOVABLE.to_string(),
            ));
        }
        Ok(())
    }

    /// Stage + distribute + merge a room-policy commit installing what
    /// `rebuild` derives from the channel's current policy — the
    /// gated/gate-less twin of [`Self::evict_leaf_locked`] for a
    /// GroupContextExtensions commit, with the same Rule-1 discipline (merge
    /// only once the nest accepted). **The caller must already hold
    /// [`Self::channel_lock`]**, and must have judged the edit against the
    /// viewer's role: every other member will. The gated path re-runs
    /// `rebuild` on every rebase attempt ([`RoomPolicyRebuild`]); the
    /// gate-less path runs it once, on the policy it holds now. Returns the
    /// commit's log position.
    async fn commit_room_policy_locked(
        &self,
        channel_id: &ChannelId,
        rebuild: RoomPolicyRebuild,
    ) -> Result<i64, BackendError> {
        if let Some(gate) = self.commit_gate() {
            return gate.gated_set_room_policy(*channel_id, rebuild).await;
        }
        let current = self.governed_policy(channel_id)?;
        let next = rebuild(&current)
            .map_err(|e| BackendError::Internal(format!("rebuild room policy: {e}")))?;
        let commit_bytes = self
            .engine
            .set_room_policy_staged(channel_id, &next)
            .map_err(|e| BackendError::Internal(format!("stage room policy commit: {e}")))?;
        self.persist_engine_state();
        let envelope = ChannelEnvelope::Commit(commit_bytes)
            .to_bytes()
            .map_err(BackendError::Internal)?;
        match self
            .send_on_channel(channel_id, envelope, None, Vec::new())
            .await
        {
            Ok(seq) => {
                self.engine.merge_pending_commit(channel_id).map_err(|e| {
                    BackendError::Internal(format!("merge room policy commit: {e}"))
                })?;
                self.persist_engine_state();
                Ok(seq)
            }
            Err(e) => {
                if let Err(clear_err) = self.engine.clear_pending_commit(channel_id) {
                    tracing::warn!(
                        error = %clear_err,
                        "clearing the staged room policy commit failed after a send failure"
                    );
                }
                self.persist_engine_state();
                Err(e.into())
            }
        }
    }

    /// The outgoing owner's half of the ownership transfer ceremony
    /// (`conversation-rooms.md` § Roles and authorization → *Ownership
    /// transfer*): build the policy that names `new_owner`, countersign it as
    /// the owner **bound to this room**, and post it as an offer every member
    /// decrypts and only the named member acts on. Refused before anything
    /// leaves the device unless the viewer owns the room and `new_owner` is
    /// another current member — the roles table's transfer row, applied once
    /// here so an honest app never offers what no device could complete.
    async fn offer_ownership(
        &self,
        channel_id: &ChannelId,
        new_owner: ActorId,
    ) -> Result<(), BackendError> {
        use fauna_i18n::strings::error::send;
        use fauna_mls::room_policy::RoomRole as WireRole;
        let current = self.governed_policy(channel_id)?;
        if current.role_of(&self.self_actor) != WireRole::Owner {
            return Err(BackendError::Refusal(
                send::ROOM_TRANSFER_OWNER_ONLY.to_string(),
            ));
        }
        if new_owner == self.self_actor
            || self
                .engine
                .find_leaf_by_identity(channel_id, &new_owner)
                .is_none()
        {
            return Err(BackendError::Refusal(
                send::ROOM_TRANSFER_NOT_A_MEMBER.to_string(),
            ));
        }
        let mut policy = current.signed.policy.clone();
        policy.version += 1;
        policy.owner = new_owner;
        // The owner is never listed in the admin set (`RoomPolicy::validate`);
        // `set_admins` drops the incoming owner from it.
        let admins = policy.admins.clone();
        policy.set_admins(admins);
        let countersignature = self
            .engine
            .countersign_ownership_transfer(channel_id, &policy)
            .map_err(|e| BackendError::Internal(format!("countersign ownership transfer: {e}")))?;
        let offered_version = policy.version;
        let offer = RoomOwnershipOffer {
            policy,
            countersignature,
        };
        let bytes = offer
            .to_bytes()
            .map_err(|e| BackendError::Internal(format!("encode ownership offer: {e}")))?;
        self.post_app_message(
            channel_id,
            ChannelMessageBody::GroupMeta(GroupMetaMessage::OwnershipOffer(bytes)),
            Vec::new(),
        )
        .await?;
        // Only once the offer is really on the channel: an offer that never
        // posted is not one the owner should later be told was superseded.
        // Latest wins, mirroring `parked_ownership_offers`.
        self.pending_own_offers
            .lock()
            .unwrap()
            .insert(*channel_id, offered_version);
        Ok(())
    }

    /// Park an inbound ownership offer for
    /// [`Self::complete_ownership_offer_locked`] when it names this identity
    /// as the incoming owner. Every other member's copy is dropped here: the
    /// offer is addressed to one member and carried to all by MLS, and it
    /// changes nothing until the named member's commit lands.
    fn park_ownership_offer(&self, channel_id: &ChannelId, bytes: &[u8]) {
        let offer = match RoomOwnershipOffer::from_bytes(bytes) {
            Ok(offer) => offer,
            Err(e) => {
                tracing::warn!(channel = %channel_id, error = %e, "undecodable ownership offer; skipped");
                return;
            }
        };
        if offer.policy.owner != self.self_actor {
            return;
        }
        self.parked_ownership_offers
            .lock()
            .unwrap()
            .insert(*channel_id, offer);
    }

    /// The incoming owner's half of the transfer ceremony: sign the offered
    /// policy as this identity and commit it carrying the outgoing owner's
    /// countersignature, so every member admits the change with both
    /// signatures (`fauna_mls::room_policy::judge_commit`'s transfer arm).
    /// **The caller must hold [`Self::channel_lock`]** — the inbound drivers
    /// call this right after their walk, inside the lock they already hold,
    /// which is also why the poll parks instead of committing: a gated
    /// commit's catch-up re-enters the walk. A no-op with nothing parked. An
    /// offer the channel's policy has moved past (its version no longer
    /// advances by one, or the room already changed hands) is dropped with a
    /// log line and never retried — the owner offers again.
    pub async fn complete_ownership_offer_locked(&self, channel_id: &ChannelId) {
        let Some(offer) = self
            .parked_ownership_offers
            .lock()
            .unwrap()
            .remove(channel_id)
        else {
            return;
        };
        let me = self.self_actor;
        if let Ok(current) = self.governed_policy(channel_id)
            && let Err(e) = offer.verify_against(&current, &channel_id.0, &me)
        {
            tracing::info!(channel = %channel_id, reason = %e, "ownership offer dropped");
            return;
        }
        let engine = self.engine.clone();
        let channel = *channel_id;
        let rebuild: RoomPolicyRebuild = Box::new(move |current: &RoomPolicyExtension| {
            offer.verify_against(current, &channel.0, &me)?;
            let mut signed = engine.sign_room_policy(&offer.policy)?;
            signed.countersignature = Some(offer.countersignature.clone());
            Ok(RoomPolicyExtension {
                signed,
                successions: current.successions.clone(),
            })
        });
        match self.commit_room_policy_locked(channel_id, rebuild).await {
            Ok(commit_seq) => {
                tracing::info!(channel = %channel_id, "ownership transfer completed — this identity now owns the room");
                self.report_roster(channel_id, Some(commit_seq)).await;
            }
            Err(e) => {
                tracing::warn!(channel = %channel_id, error = %e, "ownership offer not completed");
            }
        }
    }

    /// Register the custody-ceremony ingest seam ([`CustodyCeremonySink`] —
    /// W8.4). Injected once by the glue layer (which owns the ceremony
    /// machine + account store); idempotent (the `OnceLock` keeps the
    /// first). A backend with none skips + tallies every ceremony payload —
    /// it waits in the channel history for a capable session.
    pub fn set_custody_ceremony_sink(&self, sink: Arc<dyn CustodyCeremonySink>) {
        let _ = self.custody_sink.set(sink);
    }

    /// The registered ceremony sink, if any — consumed by
    /// [`poll_inbound_conv`]'s custody arm.
    fn custody_ceremony_sink(&self) -> Option<&Arc<dyn CustodyCeremonySink>> {
        self.custody_sink.get()
    }

    /// Register the share-endpoint ingest seam ([`ShareEndpointsSink`] —
    /// slice F). Injected once by the glue layer (which owns the binding and
    /// the account-plane write); idempotent (the `OnceLock` keeps the first).
    /// A backend with none skips + tallies every advertisement and dials no
    /// peer for the set — the nest-mediated path is unaffected.
    pub fn set_share_endpoints_sink(&self, sink: Arc<dyn ShareEndpointsSink>) {
        let _ = self.share_endpoints_sink.set(sink);
    }

    /// The registered share-endpoint sink, if any — consumed by
    /// [`poll_inbound_conv`]'s advertisement arm.
    fn share_endpoints_sink(&self) -> Option<&Arc<dyn ShareEndpointsSink>> {
        self.share_endpoints_sink.get()
    }

    /// What this session's inbound poll did with endpoint advertisements —
    /// the same shape as [`Self::custody_payload_counts`]. Read `uncaptured`
    /// as **refused or unwritable**: a member whose advertisement did not
    /// bind to its channel-proven identity is counted here, and a climbing
    /// number is a security signal rather than a storage one.
    pub fn share_endpoints_counts(&self) -> CustodyPayloadCounts {
        self.share_endpoints.snapshot()
    }

    /// Post this device's own endpoint advertisement to a shared set's
    /// channel (slice F) — the verbatim canonical
    /// `fauna_core::share_endpoints::ShareEndpoints` bytes, sealed as a
    /// [`ChannelMessageBody::ShareEndpoints`] application message (no Commit,
    /// no epoch change — the `send_custody_payload` shape). Deliberately not
    /// on [`RailBackend`]: no other rail carries discovery, and it is never a
    /// user "send".
    ///
    /// ⚠ **"no Commit" describes the BODY, not the path — and the path does
    /// commit.** Every door here funnels through [`Self::post_app_message`],
    /// which with a [`CommitGate`] injected first runs the device-owned-epoch
    /// takeover (`devices.md` § Cross-device MLS group-state sync) — a
    /// self-`Update` **Commit**. That takeover is ADMITTED on a claimed folder
    /// channel since the 2026-08-24 roster-membership commit admission
    /// (`federation.md` § Cross-nest shared folders + channel append; a nest
    /// refuses the Commit of a member it has not admitted, so this send can
    /// fail on a pass — retrying is correct for any failure).
    /// Pinned by
    /// `gate_impl.rs::a_members_share_endpoint_advertisement_posts_a_commit`.
    pub async fn send_share_endpoints(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BackendError> {
        let channel_id = ChannelId::from_hex(channel_hex)
            .map_err(|e| BackendError::Internal(format!("share endpoints channel id: {e}")))?;
        self.post_app_message(
            &channel_id,
            ChannelMessageBody::ShareEndpoints(bytes),
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    /// What this session's inbound poll did with custody-ceremony payloads
    /// — see [`CustodyPayloadCounts`] for how to read it.
    pub fn custody_payload_counts(&self) -> CustodyPayloadCounts {
        self.custody_payloads.snapshot()
    }

    /// What this session's inbound poll did with custody **receipts** — the
    /// same shape as [`Self::custody_payload_counts`], deliberately its own
    /// counter (see the receive arm). `uncaptured` here means the attestation
    /// did not verify or could not be recorded, and coverage the owner cannot
    /// check must not be rendered as coverage.
    pub fn custody_receipt_counts(&self) -> CustodyPayloadCounts {
        self.custody_receipts.snapshot()
    }

    /// The registered witness, if any — consumed by [`poll_inbound_conv`]'s
    /// statement arm.
    fn succession_witness(&self) -> Option<&Arc<dyn SuccessionWitness>> {
        self.succession_witness.get()
    }

    /// Tell the registered witness that a peer-anchor harvest sweep runs this
    /// session ([`SuccessionWitness::harvest_armed`]). Called by whoever starts
    /// the sweep, before the first inbound poll — the native receive loop's
    /// prologue, web's session constructor — so no app owes the call. No
    /// witness registered → nothing to arm.
    pub async fn arm_succession_harvest_wait(&self) {
        self.note_harvest_sweep_armed();
        if let Some(witness) = self.succession_witness() {
            witness.harvest_armed().await;
        }
    }

    /// Record that a peer-anchor harvest sweep runs this session — the
    /// backend's own half of [`Self::arm_succession_harvest_wait`], for a
    /// caller that arms its witness directly (web's session constructor is
    /// synchronous and holds the concrete `ChainWitness`). What it arms is the
    /// folder commit walk's hold behind a parked succession statement
    /// ([`Self::folder_walk_waits_on`]); the witness's own wait is the
    /// witness's to arm.
    pub fn note_harvest_sweep_armed(&self) {
        self.harvest_armed.store(true, Ordering::SeqCst);
    }

    /// Whether the folder commit walk may still wait on the harvest to settle
    /// `owner` — a sweep runs this session and has not yet spoken for that
    /// identity. `false` is the fail-open answer: no sweep, or one that has
    /// settled the owner by any arm, and nothing else this session will change
    /// a refused statement's verdict.
    fn folder_walk_waits_on(&self, owner: &ActorId) -> bool {
        self.harvest_armed.load(Ordering::SeqCst)
            && !self.harvest_spoken_for.lock().unwrap().contains(owner)
    }

    /// The registered custody sink, if any — for the session's leave path to
    /// drop a foreign-set custody record alongside the local forget. Only
    /// consumed by `ConversationsSession`'s native-only leave/home-url methods
    /// (`#[cfg(not(target_arch = "wasm32"))]` in `session.rs`) — wasm's own
    /// `leave_folder` free-fn path doesn't route through here, so this
    /// getter is genuinely unused on wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn folder_custody_sink(&self) -> Option<Arc<dyn FolderCustodySink>> {
        self.folder_custody.get().cloned()
    }

    /// Best-effort **member content-key custody ingest** (Phase 0): fetch the
    /// owner's latest sealed content-key envelope for a joined folder channel,
    /// open it at **this** member's current group epoch, and fold the generations
    /// into their own folder-key custody through the registered
    /// [`FolderCustodySink`]. Driven at the two trigger points a member's custody
    /// can change — **join** ([`join_folder_welcome`]) and **rotation-commit
    /// receipt** ([`poll_inbound_folder`], once the poll advanced the epoch).
    ///
    /// `epoch_advanced` skips the network on a quiet poll: with no epoch advance
    /// and custody already ingested this session, nothing new is published to
    /// fetch (design D2) — unless the sink says the fetch is re-owed
    /// ([`FolderCustodySink::refetch_owed`]): the owner re-minted the set's
    /// nonce without advancing the epoch, which the nest's nonce echo or a
    /// row refused `signature_invalid` gives away
    /// (`writer-signed-change-records.md` ruling (11)(b)). A missing sink, an unpublished envelope
    /// (`not_published`), or an open failure (this member lags the current
    /// epoch — D4) all no-op and leave the member to retry on the next poll pass
    /// — by **clearing** this channel's `ingested_folders` mark, which is what
    /// makes that retry reachable at all for a member who already ingested once
    /// (see the inline note; this was the rotation leg's silent failure).
    /// **Never fails the caller** (best-effort; join/poll must not fail on ingest).
    /// The ordering is poll-then-fetch: at both trigger points the open runs at
    /// the member's *current* epoch, so a lagging member fails the open and the
    /// poll-cadence retry catches up once its commit heal lands (D4). On the
    /// **rotation** path that retry is load-bearing rather than a safety net:
    /// the owner cannot publish the re-sealed envelope before distributing the
    /// Remove commit (the post-rotation epoch does not exist until it merges),
    /// so a member polling on a short cadence lands in the gap by construction.
    async fn maybe_ingest_folder_custody(&self, channel_id: &ChannelId, epoch_advanced: bool) {
        let Some(sink) = self.folder_custody.get() else {
            return;
        };
        // Asked on every pass, so an attempt this pass makes anyway spends the
        // sink's owed signal rather than leaving it to buy a second fetch.
        let refetch_owed = sink.refetch_owed(&channel_id.0).await;
        if !should_attempt_custody_ingest(
            epoch_advanced,
            self.ingested_folders.lock().unwrap().contains(channel_id),
            refetch_owed,
        ) {
            return; // nothing changed and custody already held — no fetch
        }
        // Past the guard the mark means "custody is CURRENT", not "custody was
        // ingested at some point": every outcome below that did not merge a
        // bundle **clears** it, so the quiet-poll arm above stops
        // short-circuiting and the next pass genuinely retries.
        //
        // Clearing is what makes the "retry next pass" this fn's docs (and the
        // `ingested_folders` field doc) promise reachable **for a member who
        // already holds custody** — the case the rotation leg lives in. Marking
        // on success only, as this did until 2026-07-24, made the
        // rotation-commit trigger effectively one-shot: a member that polled
        // into the window between applying the owner's Remove commit and the
        // owner re-publishing the rotated envelope failed the open, stayed
        // marked from its join-time ingest, and so never fetched again — stuck
        // on the pre-rotation generation until an app restart or an unrelated
        // later commit, with every subsequent owner upload failing closed in
        // `content_open_roots`.
        //
        // That window cannot be closed by ordering the way the add path's is
        // (`mls-group-key-material.md` § M2 *Admitting a member*, Envelope
        // ordering: re-publish before delivering the Welcome). The
        // post-rotation epoch does not **exist** until the Remove commit
        // merges, so the owner cannot publish ahead of distributing it, and a
        // member polling on a short cadence necessarily lands in the gap. The
        // retry is the only thing that closes it.
        let merged = match sink.fetch_sealed_envelope(&channel_id.to_string()).await {
            // Not published / not readable / transient.
            None => false,
            // The sink verified the envelope's signature (none is ingested
            // without one — `writer-signed-change-records.md` ruling (11)(b));
            // only the owner this member's MLS state records — the marker a
            // witnessed succession re-points inside commit processing, never
            // the nest's roster — may move what it holds.
            Some(signed) => match self
                .engine
                .open_content_key_envelope_payload(channel_id, &signed.sealed)
            {
                Err(e) => {
                    tracing::debug!(
                        channel = %channel_id,
                        "folder custody ingest: envelope open failed \
                         (lagging epoch or pre-republish window — retrying next pass): {e}"
                    );
                    false
                }
                Ok(payload) => {
                    let may_move =
                        self.engine.folder_channel_owner(channel_id) == Some(signed.signer);
                    sink.merge_and_persist(&channel_id.0, payload, may_move)
                        .await
                }
            },
        };
        apply_custody_ingest_outcome(
            &mut self.ingested_folders.lock().unwrap(),
            channel_id,
            merged,
        );
    }

    /// The injected [`ChannelCursor`], or `None` when unset — then the background
    /// poll seeds each channel's cursor at `0` (today's single-device behavior).
    /// Consulted by the receive loop's pollers to resume from the restored
    /// watermark and report folded seqs back.
    pub fn channel_cursor(&self) -> Option<&Arc<dyn ChannelCursor>> {
        self.channel_cursor.get()
    }

    /// Inject the [`HistoryPersist`] seam — the slice-5 leg calls this once,
    /// alongside [`Self::set_commit_gate`] / [`Self::set_channel_cursor`], after
    /// the wrapped `MlsStateSync` has `load`ed (its save gate must be lifted for
    /// a flush to land). Idempotent (the `OnceLock` keeps the first). Unset ⇒
    /// every durable flush is a no-op (Rule 3 has no plane to persist into).
    pub fn set_history_persist(&self, persist: Arc<dyn HistoryPersist>) {
        let _ = self.history_persist.set(persist);
    }

    /// Inject the [`ProviderPersist`] seam — the launch leg calls this once,
    /// alongside [`Self::set_history_persist`], after the wrapped `MlsStateSync`
    /// has `load`ed (its save gate must be lifted for a provider save to land;
    /// before that the seam correctly reports `false` and a mint refuses to
    /// publish — the launch-window strand `devices.md` Rule 3 closes). Idempotent
    /// (the `OnceLock` keeps the first) and lifts [`Self::expect_replica_restore`].
    /// Unset ⇒ a key-package mint refuses while a restore is expected, and
    /// otherwise publishes directly (Rule 3 has no plane to persist into — no
    /// replica a swap could restore over the init keys).
    pub fn set_provider_persist(&self, persist: Arc<dyn ProviderPersist>) {
        let _ = self.provider_persist.set(persist);
        // Seam first, flag second: a mint racing this call either still sees the
        // flag (refuses) or already sees the seam (gates on it) — never neither.
        self.replica_restore_pending.store(false, Ordering::Release);
    }

    /// Declare that this session's launch leg has built the cross-device replica
    /// plane and a restore into this engine is coming. Every leg calls it at
    /// session build, before anything can mint: the shared tokio launcher (FFI
    /// native + tui), linux's session build, and the wasm constructor. From here
    /// until [`Self::set_provider_persist`] (the restore landed) or
    /// [`Self::abandon_replica_restore`] (it never will), a key-package mint
    /// refuses to publish (`devices.md` § Durability rules, Rule 3 —
    /// save-before-publish covers the window before the seam exists).
    pub fn expect_replica_restore(&self) {
        if self.provider_persist.get().is_none() {
            self.replica_restore_pending.store(true, Ordering::Release);
        }
    }

    /// The launch restore failed permanently and the session stays
    /// single-device (e.g. a nest without the `fauna.mls` plane): no swap is
    /// coming, so the pre-plane direct publish stands again.
    pub fn abandon_replica_restore(&self) {
        self.replica_restore_pending.store(false, Ordering::Release);
    }

    /// Is a key-package mint currently refused because a declared replica
    /// restore has not yet injected the persist seam (nor been abandoned)?
    pub fn replica_restore_pending(&self) -> bool {
        self.replica_restore_pending.load(Ordering::Acquire)
    }

    /// Inject the [`SiblingGroupAdopter`] seam — the launch leg calls this once
    /// beside [`Self::set_provider_persist`], after the wrapped `MlsStateSync`
    /// has `load`ed (the adopter is gated on the same launch gate as the saves).
    /// Idempotent (the `OnceLock` keeps the first). Unset ⇒ the receive sweep
    /// never asks; a group another device joined appears at the next launch.
    pub fn set_sibling_group_adopter(&self, adopter: Arc<dyn SiblingGroupAdopter>) {
        let _ = self.sibling_group_adopter.set(adopter);
    }

    /// The injected [`SiblingGroupAdopter`], or `None` when unset. Consulted by
    /// the receive sweep once at its start, before it walks `bound_channels()`.
    pub fn sibling_group_adopter(&self) -> Option<&Arc<dyn SiblingGroupAdopter>> {
        self.sibling_group_adopter.get()
    }

    /// Rule 3 gate for a key-package mint (`devices.md` § Durability rules —
    /// **save-before-publish**): await the injected [`ProviderPersist`] flush
    /// and return `Ok(())` only once the `provider` blob (the fresh init keys'
    /// durable home) has landed. A non-`true` flush — the launch gate not yet
    /// lifted, or (never, after a genuine mint) an unchanged provider — becomes a
    /// loud `Err`: the caller must NOT publish a package whose init key is not
    /// yet durable, because the imminent restore would wipe it while a peer holds
    /// the package. No seam injected yet
    /// but a restore expected ([`Self::expect_replica_restore`]) ⇒ the same loud
    /// `Err`: the restore that has not run would swap the keys away. No seam and
    /// no restore expected (single-device / no multi-device plane, or a restore
    /// abandoned) ⇒ `Ok(())`: there is no replica a swap could restore over the
    /// keys, so the pre-plane direct publish stands.
    async fn persist_provider_before_publish(&self, what: &str) -> Result<(), BackendError> {
        let Some(persist) = self.provider_persist.get() else {
            if self.replica_restore_pending.load(Ordering::Acquire) {
                return Err(BackendError::Internal(format!(
                    "refusing to publish {what}: the cross-device replica restore has not \
                     run yet, and it would replace the fresh private init keys; retry once \
                     the device has finished syncing"
                )));
            }
            return Ok(());
        };
        if !persist.persist_provider().await? {
            return Err(BackendError::Internal(format!(
                "refusing to publish {what}: the fresh private init keys are not yet \
                 durable in the cross-device replica (the launch restore has not \
                 completed); retry once the device has finished syncing"
            )));
        }
        Ok(())
    }

    /// Rule 3 for a **Welcome join** (`devices.md` § Durability rules —
    /// durable-before-done on a spent init key): await the injected
    /// [`ProviderPersist`] flush after the join, so the joined crypto state —
    /// which exists nowhere but this engine, with no re-Welcome for a member
    /// already in the group — is in the replica before the join is reported
    /// done. Rule-2-ordered by construction (the seam saves every slice first).
    /// **Best-effort**, unlike the mint's save-before-publish gate: the join
    /// has already happened and cannot be un-happened, so a transient failure
    /// is warn-logged and the debounced autosave is the retry; a gate-down
    /// no-op is a join before the launch restore, which the restore's own
    /// carry of local-only groups covers. No seam injected ⇒ nothing to do.
    async fn persist_provider_after_join(&self, what: &str) {
        let Some(persist) = self.provider_persist.get() else {
            return;
        };
        match persist.persist_provider().await {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                "{what}: the provider flush after the join was a no-op (launch gate down or \
                 unchanged) — the launch restore carries the join"
            ),
            Err(e) => tracing::warn!(
                "{what}: the provider flush after the join failed (the debounced autosave \
                 retries): {e}"
            ),
        }
    }

    /// The per-channel serialization lock (see [`Self::channel_locks`]), lazily
    /// created. The background inbound poll and the gated commit branches take it
    /// around their per-channel critical section; the gate's inner catch-up poll
    /// does **not** (it already runs inside the gated section holding this lock).
    pub fn channel_lock(&self, channel: &ChannelId) -> Arc<futures_util::lock::Mutex<()>> {
        Arc::clone(
            self.channel_locks
                .lock()
                .unwrap()
                .entry(*channel)
                .or_insert_with(|| Arc::new(futures_util::lock::Mutex::new(()))),
        )
    }

    /// Persist the engine's provider snapshot to its local store — the
    /// durability step of the gate-less membership path (`devices.md`
    /// § Durability rules Rule 1: the staged pending must be durable somewhere
    /// the restart path will find it). Mirrors
    /// `fauna_client_folders::mls_adapter::persist_group_state`: on wasm a
    /// no-op — a web engine's only persistence is the nest replica, whose
    /// crash-safety the *gated* plane provides, so the gate-less wasm residual
    /// stays declared in devices.md § Implementation status rather than closed
    /// here. Failures warn, never fail the operation — the wire outcome
    /// already happened; durability degrades to the pre-this-change posture
    /// (loud `OwnLeafCommit` stall on the next poll).
    fn persist_engine_state(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = self.engine.save_state() {
            tracing::warn!(
                error = %e,
                "persisting the MLS provider snapshot failed; a crash before the next \
                 save re-opens the gate-less accept-to-merge window"
            );
        }
    }

    /// The shared per-actor [`MlsEngine`] this backend drives. Handed out so the
    /// owner-side **folder** content-key orchestration
    /// (`fauna_client_folders::orchestration::FoldersAuthor`, via the
    /// `FolderGroupCrypto` adapter on `Arc<MlsEngine>`) reuses the SAME engine
    /// handle over the SAME `mls_state.db` as conversations — never a second
    /// `MlsEngine` racing on the one SQLite file (`apps/fauna-linux/src/mls.rs`:
    /// "one engine over one mls_state.db, never two racing on the same SQLite
    /// file"). A folder's MLS group thus lives on the same engine as the chat
    /// groups (welcome/commit reuse) and survives a crash.
    pub fn engine(&self) -> Arc<MlsEngine> {
        Arc::clone(&self.engine)
    }

    /// Record a channel's **home**, overwriting any prior state — the plain
    /// writer the three Welcome paths use *below* their idempotency guards
    /// (after an MLS-authenticated join) and [`Self::bootstrap_group`] uses for
    /// the same-nest channel it creates.
    ///
    /// A blank `home_nest_url` records an explicit [`ChannelHome::SameNest`]
    /// marker (a same-nest Welcome, or a locally-created channel) — the channel
    /// keeps draining locally ([`Self::channel_home_url`] still reports `None`),
    /// but it is now KNOWN local rather than merely absent, so the
    /// unauthenticated pre-guard cannot later re-home it.
    /// A non-blank URL records [`ChannelHome::Foreign`]. Idempotent for equal
    /// inputs.
    pub fn record_channel_home(&self, channel_id: ChannelId, home_nest_url: &str) {
        let home = if home_nest_url.is_empty() {
            ChannelHome::SameNest
        } else {
            ChannelHome::Foreign(home_nest_url.to_string())
        };
        self.channel_home.lock().unwrap().insert(channel_id, home);
    }

    /// Record a channel's home **only if no state is recorded yet** — the writer
    /// the three Welcome paths use *before* their idempotency guards.
    ///
    /// Each guard exists to skip the key-package-consuming *join*, and each one
    /// used to sit above the [`Self::record_channel_home`] call, so a
    /// **re-delivered** Welcome for a channel this device is already in taught it
    /// nothing: the guard returned first. That made the routing datum learnable
    /// exactly once, at first join, which is why a channel established before the
    /// datum was persisted had no way back. Re-recording a
    /// home the caller already handed us costs no init key, so the guards now sit
    /// *below* this.
    ///
    /// **`if_absent` against the TOTAL encoding is the security boundary**. This runs before any MLS
    /// authentication of the re-delivery — `home_nest_url` is peer-declared and
    /// the sender is unauthenticated (`welcome.deliver` is open-federation, and
    /// a Dm/Group Welcome is ungated on the client, as DM initiation requires).
    /// It may fill only a genuinely **absent** entry, so it can never overwrite
    /// an explicit [`ChannelHome::SameNest`] (a local channel this device made or
    /// joined under current code) nor a [`ChannelHome::Foreign`] some earlier,
    /// joined-and-validated Welcome established. The only entries it can still
    /// fill are absent ones — channels persisted before the marker existed (the
    /// re-delivery recovery target, and the declared residual).
    ///
    /// **A BLANK home writes NOTHING**. What bounds the residual above is the nest side resolving a
    /// declared origin against the sender's own verified nest — and that bound
    /// is *vacuous for a blank*, because there is no URL to resolve. A blank is
    /// wholly attacker-chosen: omitting or blanking `origin_nest_url` on the
    /// **federation** door (`federation_handlers.rs`, where the peer is foreign
    /// by construction) takes its no-declared-origin arm, relays `nest_url:
    /// None`, and the client blanks it into this call. Writing `SameNest` for
    /// that — as this did until 2026-08-31 — let one unauthenticated Welcome
    /// from any verified peer that knew a channel id pin a foreign-homed channel
    /// to `SameNest` **permanently**: the marker is deliberately immovable, the
    /// genuine cross-nest re-delivery then finds no hole, and the durable
    /// folder-record seed fills `Vacant` only, so neither recovery route could
    /// correct it. The property this restores is the general one: *no
    /// unauthenticated write may produce a routing state that a later
    /// authenticated datum cannot correct.* Absent stays correctable; a
    /// same-nest assertion this device did not make is not a datum.
    ///
    /// ⚠ **The trade, stated rather than silently taken.** A *local*
    /// channel whose marker is still absent is not upgraded absent → `SameNest`
    /// by a genuine same-nest re-delivery: it stays absent, hence still fillable by a later **non-blank**
    /// pre-guard write — the declared, bounded residual `federation.md` § Cross-nest
    /// already accepts, and one the nest-side verified-origin resolution really
    /// does cap. That is the deliberate exchange: a residual that is bounded and
    /// recoverable, in place of an unbounded write that was neither.
    ///
    /// ⚠ **A `SameNest` marker stays pinned once set.** The marker is
    /// deliberately immovable (this doc's paragraph above), so it durably
    /// round-trips through every autosave and restore unless the channel is
    /// re-joined outright. The loss is recoverability of a
    /// routing datum rather than confidentiality (`SameNest` and absent both
    /// route sends locally), which `nest/common.md` § Client-state
    /// recoverability → *Per-object remedies* accepts as the standing frame
    /// for exactly this shape of unrepairable per-object state.
    pub fn record_channel_home_if_absent(&self, channel_id: ChannelId, home_nest_url: &str) {
        if home_nest_url.is_empty() {
            return;
        }
        self.channel_home
            .lock()
            .unwrap()
            .entry(channel_id)
            .or_insert_with(|| ChannelHome::Foreign(home_nest_url.to_string()));
    }

    /// Seed `channel_home` from this member's own durable **foreign-set
    /// records** — the launch recovery for every cross-nest *folder* channel
    /// (`federation.md` § Cross-nest shared folders + channel
    /// append).
    ///
    /// The map's other writers cannot restore a folder channel's home after a
    /// relaunch. The three Welcome paths need a Welcome, and an inbox Welcome is
    /// acked once drained — so a channel established in an earlier session is
    /// never offered one again, and the join records its home in memory only.
    /// The launch restore reads `ChannelHistorySlice.home_nest_url`, but a
    /// folder channel is an engine group that never gets a slice, by design.
    /// This writer needs neither: the member's `ForeignFolder` record is
    /// written into their own folder-key custody on every cross-nest folder join and
    /// outlives both, so a folder channel recovers its routing here with **no
    /// Welcome re-delivery and no init key spent**.
    ///
    /// Scope is the folder population only, and deliberately so: the folder-key custody
    /// holds a record for a shared *set*, never for a conversations channel.
    /// Same-nest sets hold no record either, so nothing here can invent a home
    /// for a local channel.
    ///
    /// **Trust: the record is spent as written** — every record was written under the
    /// 2026-08-30 verified-origin constraint, and a seed has no handshake to
    /// verify against. Accepted as the inviter-binding TOFU residual — the
    /// record exists only where this user explicitly accepted the share, and a
    /// re-accept mints a fresh, verified-origin-resolved value
    /// (`federation.md` § Cross-nest shared folders + channel append; the
    /// declared-residual prose in the § Status bullet).
    ///
    /// `if_absent`, and called AFTER the restore's own slice loop: a channel's
    /// own at-rest record is the more specific datum and outranks the folder's.
    /// Returns the number of holes actually filled (a re-run fills none), which
    /// is what the restore logs.
    pub async fn seed_channel_homes_from_custody(&self) -> usize {
        let Some(sink) = self.folder_custody.get().cloned() else {
            return 0;
        };
        // Fetch first, lock second — the population read is a custody-store round
        // trip, and `channel_home`'s mutex may not be held across it.
        let homes = sink.foreign_homes().await;
        if homes.is_empty() {
            return 0;
        }
        let mut map = self.channel_home.lock().unwrap();
        let mut filled = 0usize;
        for (channel_id, home_nest_url) in homes {
            if home_nest_url.is_empty() {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) =
                map.entry(ChannelId(channel_id))
            {
                slot.insert(ChannelHome::Foreign(home_nest_url));
                filled += 1;
            }
        }
        filled
    }

    /// The home nest URL of a **foreign**-home channel, or `None` for a same-nest
    /// or not-yet-known channel (the common case). [`poll_inbound_conv`] /
    /// [`poll_inbound_scheduling`] pass it as the `home_nest_url` of `channel.fetch`
    /// so the drain relays to the channel's home nest; every existing consumer
    /// reads only this, so the [`ChannelHome`] total encoding is invisible to
    /// them — `SameNest` and absent both report `None` (a local fetch).
    pub fn channel_home_url(&self, channel_id: &ChannelId) -> Option<String> {
        match self.channel_home.lock().unwrap().get(channel_id) {
            Some(ChannelHome::Foreign(url)) => Some(url.clone()),
            Some(ChannelHome::SameNest) | None => None,
        }
    }

    /// Whether this device **explicitly knows** the channel is same-nest — the
    /// `SameNest` marker, distinct from an absent (unknown, not yet stamped) entry.
    /// The at-rest stamp reads this so a slice written from now on carries the
    /// marker durably, re-establishing it across a relaunch and keeping the
    /// pre-guard from re-homing a restored local channel.
    pub fn channel_is_same_nest(&self, channel_id: &ChannelId) -> bool {
        matches!(
            self.channel_home.lock().unwrap().get(channel_id),
            Some(ChannelHome::SameNest)
        )
    }

    /// Route a channel send by the channel's home — the one place the
    /// `send` vs `send_remote` pick is made (`direct-messages.md` § step 3b):
    /// a foreign-homed channel (a recorded Welcome `nest_url`, the same signal
    /// that drives the `channel.fetch` relay) sends via the distinct
    /// `channel.send_remote` kind so the member's own nest relays the envelope
    /// to the channel's home nest; a same-nest channel (the common case) uses
    /// `channel.send` unchanged. Every producer path (chat application sends,
    /// scheduling iMIP, add/remove-member commits) MUST go through this — a
    /// direct `rpc.channel_send` on a foreign-homed channel appends to the
    /// wrong nest's log, which no member ever fetches (the send blackhole).
    ///
    /// **`pub` because the commit gate sends through it too.** The gate runs a
    /// crate away (`fauna-client-mls-sync`) and used to call `channel_send`
    /// itself, so every gated commit on a foreign-homed channel — a cross-nest
    /// shared-folder member's device-owned-epoch takeover, and the add/remove
    /// member commits — blackholed here while the advertisement that followed
    /// routed correctly, leaving co-members an epoch behind and unable to
    /// decrypt it. `fauna_client_mls_sync::gate_impl::BackendChannelSend` is now
    /// the gate's only door and forwards straight here, so "every producer path"
    /// is literal.
    ///
    /// `attachment_refs` is the sender's plaintext list of the sealed
    /// attachment blobs the envelope names — the conversation kind's
    /// blob-reachability floor ([`ConversationsRpc::channel_send`] owns the
    /// contract); empty for every producer but a chat send with attachments.
    pub async fn send_on_channel(
        &self,
        channel_id: &ChannelId,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        match self.channel_home_url(channel_id) {
            Some(home_nest_url) => {
                self.rpc
                    .channel_send_remote(
                        channel_id.to_string(),
                        home_nest_url,
                        envelope,
                        expect_no_commit_since,
                        attachment_refs,
                    )
                    .await
            }
            None => {
                self.rpc
                    .channel_send(
                        channel_id.to_string(),
                        envelope,
                        expect_no_commit_since,
                        attachment_refs,
                    )
                    .await
            }
        }
    }

    /// Bind a thread to its MLS channel. Group-create (Track B) and welcome-join
    /// (Track C) call this in production; tests seed it directly.
    ///
    /// Also stamps the channel's **durable chat marker** in the engine's
    /// provider KV (`MlsEngine::mark_channel_chat`): the binding itself is
    /// RAM-only, so after a relaunch an un-rebound chat channel would
    /// otherwise be indistinguishable from a folder channel — and the
    /// background folder sweep applies commits while skipping application
    /// messages, which on a chat channel silently loses them to MLS forward
    /// secrecy. The marker rides the provider
    /// snapshot/replica beside the group, so the classification survives
    /// relaunch and cross-device restore.
    pub fn bind_channel(&self, thread_id: ThreadId, channel_id: ChannelId) {
        self.engine.mark_channel_chat(&channel_id);
        self.channels.lock().unwrap().insert(thread_id, channel_id);
    }

    fn channel_for(&self, thread_id: &ThreadId) -> Option<ChannelId> {
        self.channels.lock().unwrap().get(thread_id).copied()
    }

    /// Reverse of [`Self::channel_for`]: the thread bound to a channel. The
    /// inbound driver ([`poll_inbound_conv`]) routes a decrypted message into
    /// this thread. The binding is unique per channel, so the linear scan is
    /// over the local actor's handful of active threads.
    pub fn thread_for_channel(&self, channel_id: &ChannelId) -> Option<ThreadId> {
        self.channels
            .lock()
            .unwrap()
            .iter()
            .find_map(|(tid, cid)| (cid == channel_id).then(|| tid.clone()))
    }

    /// Every channel this backend has bound a thread to (group-bootstrap on the
    /// sender, welcome-ingest on the receiver). The client's inbound poll loop
    /// iterates these to drive [`poll_inbound_conv`] per channel; a channel binds
    /// before any message can arrive on it, so the set is always a superset of
    /// the channels with deliverable traffic.
    pub fn bound_channels(&self) -> Vec<ChannelId> {
        self.channels.lock().unwrap().values().copied().collect()
    }

    /// Record a channel as a **scheduling** delivery (a one-off CalDAV-iMIP
    /// channel; `caldav-server.md` § Server-side auto-schedule). Called by
    /// [`ingest_scheduling_welcome`] after joining the group from a
    /// [`WelcomeChannelKind::Scheduling`] welcome — deliberately *instead of*
    /// [`Self::bind_channel`], so the channel never materializes a chat thread.
    pub fn mark_scheduling_channel(&self, channel_id: ChannelId) {
        // Both marks: the in-memory set drives this run's scheduling poll; the
        // engine's durable marker (`MlsEngine::mark_channel_scheduling`) is what
        // keeps the channel classifying as scheduling after a relaunch empties
        // the set — the folder derivations and the eviction driver's seat
        // classification read it.
        self.engine.mark_channel_scheduling(&channel_id);
        self.scheduling_channels.lock().unwrap().insert(channel_id);
    }

    /// Whether `channel_id` was joined as a scheduling channel — the idempotency
    /// guard [`ingest_scheduling_welcome`] checks before the key-package-consuming
    /// join (mirroring [`Self::thread_for_channel`] for the chat path). Reads
    /// the in-memory set *or* the engine's durable marker, so the answer
    /// survives a relaunch and covers organizer-created one-offs the set never
    /// held.
    pub fn is_scheduling_channel(&self, channel_id: &ChannelId) -> bool {
        self.scheduling_channels
            .lock()
            .unwrap()
            .contains(channel_id)
            || self.engine.is_channel_scheduling(channel_id)
    }

    /// Every scheduling channel this backend has joined. The session's scheduling
    /// poll iterates these to drive [`poll_inbound_scheduling`] per channel — the
    /// scheduling twin of [`Self::bound_channels`] for the chat rail.
    pub fn scheduling_channels(&self) -> Vec<ChannelId> {
        self.scheduling_channels
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect()
    }

    /// Record a channel as a joined **folder** membership (a cross-user shared
    /// folder; `folders.md` § Sharing). Called by [`join_folder_welcome`]
    /// after joining the group from a [`WelcomeChannelKind::Folder`] welcome —
    /// deliberately *instead of* [`Self::bind_channel`], so the channel never
    /// materializes a chat thread (the folder twin of
    /// [`Self::mark_scheduling_channel`]).
    pub fn mark_folder_channel(&self, channel_id: ChannelId) {
        self.folder_channels.lock().unwrap().insert(channel_id);
    }

    /// Whether `channel_id` was joined as a folder membership — the idempotency
    /// guard [`join_folder_welcome`] checks before the key-package-consuming
    /// join (mirroring [`Self::is_scheduling_channel`]).
    pub fn is_folder_channel(&self, channel_id: &ChannelId) -> bool {
        self.folder_channels.lock().unwrap().contains(channel_id)
    }

    /// Whether `channel_id` rides the **folder rail** for *dispatch* purposes —
    /// derived from the engine exactly as [`Self::folder_poll_channels`] derives
    /// its sweep set (an MLS group that is neither a bound chat thread, nor a
    /// scheduling channel, nor durably chat-marked), and deliberately **not** read
    /// from the in-memory [`Self::mark_folder_channel`] marker.
    ///
    /// **Never route on the marker.** It is join-idempotency / leave bookkeeping:
    /// its only production writer is the *recipient's* [`join_folder_welcome`],
    /// and it empties on relaunch. But the only actor that ever *gates* a commit
    /// on a folder channel is the **owner**, removing a member — and the owner's
    /// backend never marks, because it creates the group through
    /// `FolderGroupCrypto for Arc<MlsEngine>`, which holds an `Arc<MlsEngine>`,
    /// not a `FaunaMlsBackend`, and so structurally cannot mark. Routing
    /// `CommitCatchUp::catch_up_after` on the marker therefore misrouted **every**
    /// contested owner-side removal to the chat poll, which no-ops on a
    /// thread-less channel: the cursor never advanced, the rebuilt commit re-sent
    /// on the same stale epoch, and the rebase spun to `RetriesExhausted` — a
    /// contested folder member removal could not converge. The engine persists
    /// its groups (native `mls_state.db`), so this derivation survives a relaunch,
    /// which is exactly why the *background* folder poll was never affected.
    ///
    /// The **durable chat marker** (`MlsEngine::is_channel_chat`, stamped by
    /// [`Self::bind_channel`], riding the provider snapshot/replica beside the
    /// group) closes the inverse hole: an un-*re*bound chat channel after a
    /// relaunch must never classify as folder (
    /// the sweep would apply its commits past unread chat messages, which MLS
    /// forward secrecy then makes permanently undecryptable). A folder
    /// channel is never chat-marked, so the owner-side dispatch above is
    /// unaffected.
    pub fn is_folder_rail(&self, channel_id: &ChannelId) -> bool {
        self.engine.has_group(channel_id)
            && !self
                .channels
                .lock()
                .unwrap()
                .values()
                .any(|c| c == channel_id)
            && !self.is_scheduling_channel(channel_id)
            && !self.engine.is_channel_chat(channel_id)
    }

    /// The **conversation** channels this account has joined — the membership
    /// half of the account plane's content-scope set
    /// (`fauna_sync_engine::scope_set`; charter
    /// `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors →
    /// *Scope partition*, "member scopes (each joined `__conv` channel)").
    ///
    /// Near-inverse of [`Self::folder_poll_channels`], and deliberately the
    /// *narrow* side of the same durable-marker split: a channel counts when it
    /// is either bound as a chat thread in this process or carries the durable
    /// chat marker (`MlsEngine::is_channel_chat`), never merely by being of
    /// unknown kind. Registering a scope only ever costs an empty feed read —
    /// no MLS state is touched, so the sweep's epoch hazard has no analogue
    /// here — but a `conv` scope minted for every folder channel would be
    /// permanent noise on every pump pass, whereas the one thing this misses
    /// self-heals.
    ///
    /// Residual, bounded and self-healing: a chat channel not yet bound in
    /// this process carries no marker until its
    /// next [`Self::bind_channel`] stamps the marker — i.e. until the app next
    /// opens that thread, which is also when its content first matters.
    pub fn conv_channels(&self) -> Vec<ChannelId> {
        let mut channels: std::collections::HashSet<ChannelId> =
            self.channels.lock().unwrap().values().copied().collect();
        channels.extend(
            self.engine
                .list_groups()
                .into_iter()
                .filter(|c| self.engine.is_channel_chat(c)),
        );
        // Sorted: the caller derives a scope set from this and compares it to
        // what it last derived, so a stable order keeps that comparison cheap.
        let mut channels: Vec<ChannelId> = channels.into_iter().collect();
        channels.sort_by_key(|c| c.0);
        channels
    }

    /// Every channel to drive the **folder commit poll**
    /// ([`poll_inbound_folder`]) over — the folder twin of
    /// [`Self::scheduling_channels`], but **derived from the engine** rather than
    /// the in-memory [`Self::mark_folder_channel`] set: every MLS group the
    /// engine holds that is neither a bound chat thread, nor a scheduling
    /// channel, nor durably chat-marked. The engine persists its groups (native
    /// `mls_state.db`), so this survives a process restart with no per-app
    /// re-marking — the marker set is join-idempotency/leave bookkeeping only
    /// and empties on relaunch.
    ///
    /// The derivation's old "over-approximates harmlessly" premise was
    /// **refuted**: the sweep applies commits while
    /// skipping application messages, so sweeping an un-*re*bound **chat**
    /// channel could advance the shared engine's epoch past an unread chat
    /// message — permanently undecryptable under MLS forward secrecy. The
    /// durable chat marker (`MlsEngine::is_channel_chat`, stamped at
    /// [`Self::bind_channel`], riding the provider snapshot/replica beside the
    /// group) excludes such channels even while unbound. Residual: a chat
    /// channel not yet bound in this process stays in the sweep until its
    /// next bind stamps it — an exposure that only narrows, never widens. Scheduling channels are likewise excluded durably
    /// (`MlsEngine::is_channel_scheduling`, stamped at both creation sites) —
    /// the relaunch over-approximation survives only for scheduling channels
    /// not yet stamped, where it stays harmless here (one-shot single-epoch
    /// deliveries, no membership commits on their log).
    pub fn folder_poll_channels(&self) -> Vec<ChannelId> {
        let bound: std::collections::HashSet<ChannelId> =
            self.channels.lock().unwrap().values().copied().collect();
        self.engine
            .list_groups()
            .into_iter()
            .filter(|c| {
                !bound.contains(c)
                    && !self.is_scheduling_channel(c)
                    && !self.engine.is_channel_chat(c)
            })
            .collect()
    }

    /// Locally **forget** a joined folder channel — the reverse of
    /// [`Self::mark_folder_channel`], driven by [`leave_folder`]. Forgets the
    /// MLS group in the engine (so `MlsEngine::has_group` → false and the B3
    /// member-visible list hides the set) and drops the folder + home-nest
    /// bookkeeping. Idempotent. Does **not** touch the nest roster — the
    /// complementary `fauna.folders.leave` self-drop is the caller's separate step.
    pub fn forget_folder_channel(&self, channel_id: ChannelId) -> Result<(), BackendError> {
        self.engine
            .forget_group(&channel_id)
            .map_err(|e| BackendError::Internal(format!("forget folder group: {e}")))?;
        self.folder_channels.lock().unwrap().remove(&channel_id);
        self.channel_home.lock().unwrap().remove(&channel_id);
        // The engine dropped the rested park with the group; the RAM one goes
        // too, and a rejoin this session starts with nothing to drain.
        self.take_parked_folder_in_channel(&channel_id);
        self.folder_park_loaded.lock().unwrap().remove(&channel_id);
        Ok(())
    }

    /// The peer actors of a thread (every Fauna participant that isn't us). The
    /// group-bootstrap path fetches a key package for each and adds them to the
    /// new MLS group.
    fn peer_actors(&self, thread: &ThreadDetail) -> Vec<(ActorId, Option<String>)> {
        thread
            .participants
            .iter()
            .filter_map(|a| match a {
                TypedAddress::Fauna { actor_id, handle } if *actor_id != self.self_actor => {
                    Some((*actor_id, self.peer_domain_for(handle)))
                }
                _ => None,
            })
            .collect()
    }

    /// The peer's **foreign** nest domain, or `None` for a same-nest peer. A
    /// `TypedAddress::Fauna` handle is canonical `localpart@domain`; a domain
    /// equal to ours (the live cell's domain, read here at routing time) — or
    /// an empty/absent one (welcome-ingested threads carry no handle) — routes
    /// same-nest (`None`), otherwise the data-plane call relays to that peer's
    /// nest.
    fn peer_domain_for(&self, handle: &str) -> Option<String> {
        let (_, d) = handle.rsplit_once('@')?;
        if d.is_empty() || d.eq_ignore_ascii_case(&self.self_address.domain()) {
            None
        } else {
            Some(d.to_string())
        }
    }

    /// Lazily bootstrap an MLS group for a thread that has no channel yet:
    /// fetch each peer's key package, `create_group`, bind the thread, and
    /// deliver the single Welcome to every peer. Returns the new `ChannelId`.
    /// (`docs/goal/ui/conversations.md` § Architectural rules #2 — all of this
    /// is shared-Rust MLS; the client never sees the `ChannelId`.)
    async fn bootstrap_group(&self, thread: &ThreadDetail) -> Result<ChannelId, BackendError> {
        let peers = self.peer_actors(thread);
        if peers.is_empty() {
            return Err(BackendError::Internal(
                "cannot start an MLS conversation with no Fauna peers".to_string(),
            ));
        }

        // Fetch + validate one key package per peer.
        let mut key_packages = Vec::with_capacity(peers.len());
        for (actor, peer_domain) in &peers {
            let actor_hex = hex::encode(actor.0);
            tracing::debug!(peer = %actor_hex, "bootstrap_group: keypackage_fetch");
            let kp_bytes = self
                .rpc
                .keypackage_fetch(actor_hex.clone(), peer_domain.clone())
                .await?
                .ok_or_else(|| {
                    BackendError::Internal(format!("no key package available for {actor_hex}"))
                })?;
            let kp = self
                .engine
                .key_package_from_bytes(&kp_bytes)
                .map_err(|e| BackendError::Internal(e.to_string()))?;
            key_packages.push(kp);
        }

        // A **group** is born governed — owned by its creator, invite-only,
        // no history for joiners (`conversation-rooms.md` § Roles and
        // authorization; `RoomPolicy::initial`). Every current app's key
        // package advertises the policy extension (`fauna_mls::room_policy`
        // § *Where the policy lives*), so a peer package that does not is a
        // non-conforming package, and `create_group_with_policy` refuses it
        // by name: the fork fails with that error rather than being born
        // policy-less (the older-app fallback that once minted a policy-less
        // group here was a compat remnant, removed 2026-09-25 under
        // `version-compatibility.md` § Dimension 2's fourth exception). A 1:1
        // is not a group (`conversation-rooms.md` § The room) and carries no
        // policy; the fork its add-participant makes is a group and does.
        let governed = peers.len() > 1;
        tracing::debug!(governed, "bootstrap_group: create_group");
        let (channel_id, welcome) = if governed {
            let policy = fauna_mls::room_policy::RoomPolicy::initial(self.self_actor, None);
            let signed = self
                .engine
                .sign_room_policy(&policy)
                .map_err(|e| BackendError::Internal(format!("sign room policy: {e}")))?;
            self.engine
                .create_group_with_policy(&key_packages, &RoomPolicyExtension::new(signed))
        } else {
            self.engine.create_group(&key_packages)
        }
        .map_err(|e| BackendError::Internal(format!("create group: {e}")))?;
        self.bind_channel(thread.thread_id.clone(), channel_id);
        // The creator's channel is same-nest by construction (its log lives on
        // this member's own nest). Record the explicit marker — not merely
        // leaving it absent — so a hostile re-delivered Welcome's pre-guard
        // write cannot re-home this local channel to a peer-declared URL, and
        // so the marker persists into the slice for after a relaunch.
        self.record_channel_home(channel_id, "");

        // One Welcome covers every added member; deliver it to each peer. A 1:1
        // is a DM welcome; anything larger carries the raw group id.
        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| BackendError::Internal(format!("serialize welcome: {e:?}")))?;
        let kind = if peers.len() > 1 {
            WelcomeChannelKind::Group {
                group_id_hex: self
                    .engine
                    .group_id_bytes(&channel_id)
                    .map(hex::encode)
                    .unwrap_or_default(),
            }
        } else {
            WelcomeChannelKind::Dm
        };
        for (actor, peer_domain) in &peers {
            tracing::debug!(peer = %hex::encode(actor.0), "bootstrap_group: welcome_deliver");
            self.rpc
                .welcome_deliver(
                    hex::encode(actor.0),
                    channel_id.to_string(),
                    welcome_bytes.clone(),
                    kind.clone(),
                    peer_domain.clone(),
                )
                .await?;
        }

        // Rule 3 (durable-before-done, `devices.md` § Durability rules):
        // durably persist the (still empty) `history/<ch>` slice BEFORE the
        // first send's takeover CAS-puts the `provider` (`post_app_message` →
        // `ensure_takeover`, which runs right after this returns). A durable
        // provider must never list a chat channel with no history blob — the
        // blob is what lets the next launch restore + re-bind the thread (and
        // what distinguishes a chat channel from a deliberately thread-less
        // scheduling/folder channel), so without it a crash mid-send leaves
        // the channel permanently invisible while peers keep posting into it.
        // Also Rule 2's order: history before provider. Best-effort — a
        // transient failure leaves exactly the pre-existing crash window, and
        // the post-append flush in `manager::send` persists on completion.
        tracing::debug!("bootstrap_group: persist_channel");
        if let Some(persist) = self.history_persist.get()
            && let Err(e) = persist.persist_channel(channel_id).await
        {
            tracing::warn!(
                "bootstrap history persist failed (the debounced autosave retries): {e}"
            );
        }

        Ok(channel_id)
    }

    /// Deliver a CalDAV scheduling iMIP (`REQUEST`/`REPLY`/`CANCEL`, as the raw
    /// RFC 5322 message the email rail would send) to a **mailbox-less** Fauna
    /// recipient over a one-off MLS channel — the WS-RPC sealed-delivery rail for
    /// an attendee with CalDAV enabled but email disabled
    /// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule,
    /// Half-1). No new standing-key surface: fetch the recipient's key package,
    /// `create_group` a fresh 1:1 group, deliver the Welcome tagged
    /// [`WelcomeChannelKind::Scheduling`] (so the recipient's receive loop routes
    /// the channel to the calendar-apply path, never the chat UI — Slice 4), then
    /// post the iMIP as the channel's first application message
    /// ([`ChannelMessageBody::Scheduling`]).
    ///
    /// Mirrors [`Self::bootstrap_group`] but for a 1:1, **non-chat** delivery: the
    /// channel is deliberately *not* bound to a thread ([`Self::bind_channel`]) —
    /// the organizer never receives on it (an attendee's RSVP comes back as its
    /// own iMIP dispatch, a separate one-off channel). `peer_domain` is
    /// `Some(domain)` for a recipient on a **foreign** nest (the home nest relays
    /// the key-package fetch + Welcome), `None` for same-nest — the same routing
    /// the chat rail uses.
    ///
    /// `imip_rfc5322` is `fauna_core::ical::ImipMessage.raw_rfc5322` verbatim, so
    /// the same bytes ride either transport (email or this WS-RPC rail) and the
    /// recipient extracts the `text/calendar` part identically (priority #2: one
    /// iMIP construction, two transports).
    pub async fn deliver_scheduling_imip(
        &self,
        recipient_actor: ActorId,
        peer_domain: Option<String>,
        imip_rfc5322: Vec<u8>,
    ) -> Result<(), BackendError> {
        let recipient_hex = hex::encode(recipient_actor.0);

        // Fetch the recipient's key package (same-nest or relayed).
        let kp_bytes = self
            .rpc
            .keypackage_fetch(recipient_hex.clone(), peer_domain.clone())
            .await?
            .ok_or_else(|| {
                BackendError::Internal(format!("no key package available for {recipient_hex}"))
            })?;

        // Seal the one-off scheduling delivery (a fresh group for just this
        // recipient — NOT bound to a thread, since the organizer never receives
        // on it — plus the iMIP as its first application message). This is the
        // SAME single-sourced builder the MDA server-side gateway drives with an
        // *ephemeral* engine, so the bytes a stock organizer's invite produces
        // are byte-identical to a Fauna app's (caldav-server.md § Server-side
        // auto-schedule). The organizer (this backend's actor) is the app-level
        // sender.
        let delivery = self
            .engine
            .build_scheduling_delivery(&kp_bytes, self.self_actor, imip_rfc5322)
            .map_err(|e| BackendError::Internal(format!("build scheduling delivery: {e}")))?;

        // Tag the Welcome `Scheduling` so the recipient's receive loop routes the
        // channel's application messages to the calendar-apply path, never the
        // chat UI (caldav-server.md § Server-side auto-schedule).
        self.rpc
            .welcome_deliver(
                recipient_hex,
                delivery.channel_id.to_string(),
                delivery.welcome_bytes,
                WelcomeChannelKind::Scheduling,
                peer_domain,
            )
            .await?;

        // The iMIP rides as the group's first application message — the same raw
        // RFC 5322 bytes the email rail carries.
        self.send_on_channel(
            &delivery.channel_id,
            delivery.app_envelope,
            None,
            Vec::new(),
        )
        .await?;

        Ok(())
    }

    /// Confirm a resolved actor is a reachable Fauna peer and wrap it in a
    /// `Resolved(Fauna)`. Both `resolve_address` forms (actor-id and handle)
    /// end here: a non-destructive `keypackage.count > 0` probe means there's a
    /// key package to add them to a group with. `count == 0` is
    /// reachable-but-exhausted, indistinguishable from non-existent over this
    /// probe and equally un-addable, so it keeps the manager's chain falling
    /// through (`NotFound`); a transport fault is `Error` — the home nest is a
    /// known Fauna domain by definition, the same rule `resolve_foreign` applies
    /// to a known foreign one (`federation.md` § Peer-auth model →
    /// *Discovery-failure semantics*), and `Error` is terminal for the chain.
    async fn resolve_reachable(&self, actor_id: ActorId, handle: String) -> ResolveResult {
        match self.rpc.keypackage_count(hex::encode(actor_id.0)).await {
            Ok(count) if count > 0 => {
                ResolveResult::Resolved(TypedAddress::Fauna { handle, actor_id })
            }
            Ok(_) => ResolveResult::NotFound,
            Err(e) => ResolveResult::Error(e.to_string()),
        }
    }

    /// The local actor's handle domain (lower-cased), or `None` while identity
    /// is unresolved (a bare or empty self address).
    fn self_domain(&self) -> Option<String> {
        handle_domain(&self.self_address.get())
    }

    /// Record positive evidence that `domain` hosts a Fauna nest.
    fn learn_domain(&self, domain: &str) {
        self.known_domains
            .lock()
            .unwrap()
            .insert(domain.to_ascii_lowercase());
    }

    /// Whether this account holds positive evidence that `domain` hosts a Fauna
    /// nest: the local actor's own domain, a `TypedAddress::Fauna` participant of
    /// some thread at that domain (`observe_participants`), or a nest there that
    /// answered a resolve this session (`learn_domain`).
    ///
    /// A bare `false` from this conflates "looked, and it is not there" with
    /// "have not looked yet" — the discovery decision needs
    /// [`Self::domain_evidence`], which separates them.
    fn is_known_fauna_domain(&self, domain: &str) -> bool {
        let wanted = domain.to_ascii_lowercase();
        self.self_domain().is_some_and(|d| d == wanted)
            || self.known_domains.lock().unwrap().contains(&wanted)
    }

    /// What this account's evidence says about `domain`, as the three-state
    /// verdict the discovery rule needs (`federation.md` § Peer-auth model →
    /// *Discovery-failure semantics*, case 2). The local actor's own domain and
    /// the harvested/answered set are positive evidence at any time; their
    /// **absence** only counts once the account's conversations have actually
    /// been loaded.
    ///
    /// `pub` so the launch seam that decides the state can be pinned directly:
    /// the difference between a restore that carried the account's evidence and
    /// one that carried nothing is a security-relevant fact, and asserting it
    /// through a UI picker would test the renderer instead of the rule.
    pub fn domain_evidence(&self, domain: &str) -> DomainEvidence {
        if self.is_known_fauna_domain(domain) {
            DomainEvidence::KnownFauna
        } else if self.conversations_loaded.load(Ordering::Acquire) {
            DomainEvidence::AbsentFromLoadedEvidence
        } else {
            DomainEvidence::Unloaded
        }
    }

    /// This account's conversation history is now in the thread store, so an
    /// empty participant harvest is a real absence from here on.
    ///
    /// Called once per launch by `fauna_client_mls_sync::orchestration::
    /// restore_and_wire`, right after it has restored every `history/<ch>`
    /// slice — the single funnel all four legs (FFI-native, tui, linux, web)
    /// reach the plane through. Before it, [`Self::domain_evidence`] answers
    /// [`DomainEvidence::Unloaded`] and a peer that does not answer is a failed
    /// lookup rather than an email fallthrough.
    ///
    /// ⚠ **The caller must have CARRIED the evidence, not merely run.** This
    /// says the account's conversations are loaded, and the discovery rule
    /// spends that claim on a plaintext-SMTP downgrade, so calling it after a
    /// restore that carried nothing re-opens the very window the three-state
    /// split closed — an *established* absence asserted over an empty store.
    /// `restore_and_wire` owns that test (a replica was read, or the engine
    /// holds no groups at all); it is stated here because this is the surface a
    /// future caller would reach for, and the mark is one-way.
    pub fn mark_conversations_loaded(&self) {
        self.conversations_loaded.store(true, Ordering::Release);
    }

    /// Resolve a typed `localpart@domain` whose `domain` is **foreign** (not this
    /// nest's) directly against that peer nest: anonymous `by_handle` over TLS,
    /// with reachability taken from the reply's `addressable` boolean — a foreign
    /// client cannot run a same-nest `keypackage.count` probe
    /// (`docs/goal/architecture/federation.md` § Key packages).
    ///
    /// **Discovery-failure semantics** (`federation.md` § Peer-auth model,
    /// ratified 2026-08-29) — two answers and one non-answer:
    ///
    /// - *A nest answered.* Found + addressable → `Resolved(Fauna)`. Found but
    ///   not addressable, unknown handle (`Ok(None)`), or a refusal from the
    ///   **closed, allowlisted** disowning set (`Rejected` — `domain_not_local`,
    ///   `handle.invalid`) → `NotFound`: a nest at this domain says "not a Fauna
    ///   recipient here", and the manager's chain falls through to email.
    ///   A version-incompatible nest (`NeedsUpdate`) is a Fauna nest we cannot
    ///   talk to → `Error`. An answer that came from a nest *serving* this domain
    ///   (found / not-found) also records the domain as a known Fauna domain.
    /// - *No nest answered* (`Transient` — DNS / connect / TLS / WS / timeout /
    ///   protocol fault, a transient wire refusal such as a rate limit, or any
    ///   refusal *outside* the allowlist above). The transport-error kind
    ///   is deliberately not consulted (the browser cannot see it; priority #1).
    ///   The domain is judged by positive evidence alone: a **known** Fauna
    ///   domain → `Error` ("Lookup failed — try again"; terminal for the chain,
    ///   so the SMTP rail can never re-read the string as an email address);
    ///   an unknown one → `NotFound` (first contact with an unreachable,
    ///   unadvertised nest is email by ruling — `bob@example.com` dials
    ///   `https://example.com` and fails exactly the same way).
    async fn resolve_foreign(&self, localpart: &str, domain: &str) -> ResolveResult {
        match self
            .rpc
            .actor_by_handle_remote(domain.to_string(), localpart.to_string())
            .await
        {
            Ok(Some(resolved)) if resolved.addressable => {
                self.learn_domain(domain);
                let Some(actor_id) = parse_actor_id_hex(&resolved.actor_id_hex) else {
                    return ResolveResult::Error(
                        "foreign nest returned a malformed actor id".to_string(),
                    );
                };
                // ⚠ The canonical handle is built from the **dialed** domain,
                // never from `resolved.echoed_domain`. The dial is the only
                // half of this answer anything vouches for: discovery ran over
                // authenticated TLS to `domain`, so the certificate binds
                // `domain` to the nest that answered. The echo is that nest's
                // own assertion about itself, and honouring it let a nest
                // serving `attacker.test` mint a participant whose stored,
                // rendered handle reads `bob@trusted.test`.
                //
                // Reading the dial is also the *honest* answer for a legitimate
                // multi-domain peer: this hop sends no `domain` qualifier, so
                // such a nest echoes its **primary** identity domain, and the
                // old code silently rewrote a user's typed `bob@domain2` to
                // `bob@primary`. The dial gives the user back the domain they
                // typed — which `mail-multidomain.md` § Resolution and login
                // report the live identity domain already calls the right
                // answer — and keeps `peer_domain_for`'s key-package route on
                // the TLS-verified domain rather than an attacker-chosen string.
                ResolveResult::Resolved(TypedAddress::Fauna {
                    handle: format!("{localpart}@{domain}"),
                    actor_id,
                })
            }
            Ok(_) => {
                // Not addressable, or no such actor: a nest serving this domain
                // answered, so the domain is known — and the answer is "no".
                self.learn_domain(domain);
                ResolveResult::NotFound
            }
            // Every non-answer goes through the one named rule
            // (`classify_foreign_non_answer`): a disowning refusal falls
            // through to email vouching for nothing, a version-incompatible
            // nest is a terminal error that proves the domain is Fauna, and
            // anything else is decided by positive evidence alone — never by
            // the fault kind, which a browser cannot see.
            //
            // ⚠ The `Rejected` arm is only ever a *disowning* refusal because
            // the seam guarantees it: `remote_by_handle_outcome` allowlists the
            // codes that disown a handle and hands every other rejection over
            // as a non-answer. Without that guarantee this arm would be reading
            // `RpcError::action()`'s OPEN default class as positive evidence of
            // a definite refusal, which is what let a throttled discovery probe
            // downgrade a known Fauna peer to plaintext SMTP.
            Err(e) => {
                let outcome = classify_foreign_non_answer(&e, self.domain_evidence(domain));
                if outcome.proves_fauna_domain {
                    self.learn_domain(domain);
                }
                if outcome.terminal {
                    ResolveResult::Error(e.to_string())
                } else {
                    ResolveResult::NotFound
                }
            }
        }
    }

    fn next_seq(&self, channel: &ChannelId) -> u64 {
        let mut seqs = self.seqs.lock().unwrap();
        let s = seqs.entry(*channel).or_insert(0);
        *s += 1;
        *s
    }

    /// Post one custody-ceremony payload to a channel (W8.4) — the verbatim
    /// canonical `CustodyCeremonyMessage` bytes, sealed as a
    /// [`ChannelMessageBody::Custody`] application message (no Commit, no
    /// epoch change — the `send_reaction` shape). Called by the ceremony
    /// glue, which addresses the channel by hex because that is what the
    /// durable ceremony state records; deliberately NOT on [`RailBackend`] —
    /// no other rail carries custody, and it is never a user "send".
    ///
    /// ⚠ **"no Commit" describes the BODY, not the path — and the path does
    /// commit.** Every door here funnels through [`Self::post_app_message`],
    /// which with a [`CommitGate`] injected first runs the device-owned-epoch
    /// takeover (`devices.md` § Cross-device MLS group-state sync) — a
    /// self-`Update` **Commit**. That takeover is ADMITTED on a claimed folder
    /// channel since the 2026-08-24 roster-membership commit admission
    /// (`federation.md` § Cross-nest shared folders + channel append; a nest
    /// refuses the Commit of a member it has not admitted, so this send can
    /// fail on a pass — retrying is correct for any failure).
    /// Pinned by
    /// `gate_impl.rs::a_members_share_endpoint_advertisement_posts_a_commit`.
    pub async fn send_custody_payload(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BackendError> {
        let channel_id = ChannelId::from_hex(channel_hex)
            .map_err(|e| BackendError::Internal(format!("custody payload channel id: {e}")))?;
        self.post_app_message(&channel_id, ChannelMessageBody::Custody(bytes), Vec::new())
            .await?;
        Ok(())
    }

    /// Post one custody **receipt** to a channel (W8.7 leg 2) — the verbatim
    /// signed `CustodyReceipt` envelope, sealed as a
    /// [`ChannelMessageBody::CustodyReceipt`] application message. Same
    /// no-Commit shape and same never-a-user-send reasoning as
    /// [`Self::send_custody_payload`]; a separate door because it is a
    /// separate body, and the bytes must reach the owner **unmodified** — the
    /// owner re-verifies the signature, so any re-encode on the way would
    /// present a lying custodian and an honest one identically.
    ///
    /// ⚠ **"no Commit" describes the BODY, not the path — and the path does
    /// commit.** Every door here funnels through [`Self::post_app_message`],
    /// which with a [`CommitGate`] injected first runs the device-owned-epoch
    /// takeover (`devices.md` § Cross-device MLS group-state sync) — a
    /// self-`Update` **Commit**. That takeover is ADMITTED on a claimed folder
    /// channel since the 2026-08-24 roster-membership commit admission
    /// (`federation.md` § Cross-nest shared folders + channel append; a nest
    /// refuses the Commit of a member it has not admitted, so this send can
    /// fail on a pass — retrying is correct for any failure).
    /// Pinned by
    /// `gate_impl.rs::a_members_share_endpoint_advertisement_posts_a_commit`.
    pub async fn send_custody_receipt(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BackendError> {
        let channel_id = ChannelId::from_hex(channel_hex)
            .map_err(|e| BackendError::Internal(format!("custody receipt channel id: {e}")))?;
        self.post_app_message(
            &channel_id,
            ChannelMessageBody::CustodyReceipt(bytes),
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    /// Encrypt a `ChannelMessage` carrying `body` and post it to the channel as
    /// an `Application` envelope. The shared encrypt→wrap→`channel_send` tail of
    /// both chat sends ([`RailBackend::send`]) and metadata changes
    /// ([`RailBackend::rename`]). Returns the server-assigned sequence.
    /// Seal one application message onto the channel and report what the nest
    /// made of it: the `seq` it allocated, and — derived from that seq and the
    /// envelope bytes it just sealed — the record's account-data-plane identity
    /// (`crate::plane`).
    ///
    /// The plane ref is produced *here*, not by the caller, because this is the
    /// only place the sealed envelope exists: `encode_body` hands over
    /// plaintext, and the ciphertext is built and consumed inside this call.
    /// Callers with no plane concern (rename, reactions, deletes) simply read
    /// [`AppendedRecord::seq`].
    ///
    /// `attachment_refs` — the sealed cids a [`ChannelMessageBody::Attachments`]
    /// body names, listed in plaintext beside the envelope so the nest can pin
    /// the blobs (the conversation kind's blob-reachability floor,
    /// [`ConversationsRpc::channel_send`]); empty for every other body.
    async fn post_app_message(
        &self,
        channel_id: &ChannelId,
        body: ChannelMessageBody,
        attachment_refs: Vec<String>,
    ) -> Result<AppendedRecord, BackendError> {
        // Device-owned-epoch takeover (design §3c): before the first application
        // send in an epoch this device did not author, post a self-`Update` commit
        // through the gate so the shared single leaf never forks a ratchet
        // generation. A no-op once this device owns the epoch; skipped entirely
        // with no [`CommitGate`] (single-device / no multi-device plane). All app
        // traffic funnels here (chat send, rename, reactions, deletes), so this is
        // the one takeover point.
        //
        // Hold the per-channel serialization lock ([`Self::channel_lock`]) across
        // the takeover *and* the `encrypt` below (gated path only) so a concurrent
        // background poll can't `process_commit` — advancing the epoch — between
        // the takeover commit and the `encrypt` that binds this message to it. The
        // takeover's own rebase drains `poll_inbound_conv` *inside* this held lock;
        // that inner poll must not re-take it (it doesn't — the lock lives on the
        // background poll + the gated branches, never in `poll_inbound_conv`). The
        // blind application append needs no lock (the ciphertext is already sealed
        // at the authored epoch), so the guard drops before the network send.
        // The lock `Arc` is owned for the whole body so the guard can outlive the
        // conditional (the background poll already creates this per-channel lock,
        // so acquiring it here on the ungated path costs nothing extra).
        let channel_lock = self.channel_lock(channel_id);
        let guard = match self.commit_gate() {
            Some(gate) => {
                // The two lines around the acquire are the only witness to a
                // send DEFERRED behind the background folder poll, which holds
                // this same per-channel lock across its whole inbound walk
                // (`session::poll_folder_feed`). Without them a queued
                // advertisement and a slow takeover are the same observation
                // from outside — the exact ambiguity that left the linux
                // journey's ~8 idle minutes unattributable.
                tracing::debug!(channel = %channel_id, "app send: awaiting the channel lock for the takeover");
                let g = channel_lock.lock().await;
                tracing::debug!(channel = %channel_id, "app send: channel lock held; ensuring the epoch takeover");
                gate.ensure_takeover(*channel_id).await?;
                tracing::debug!(channel = %channel_id, "app send: takeover settled; sealing at the authored epoch");
                Some(g)
            }
            None => None,
        };
        let message = ChannelMessage {
            sender: self.self_actor,
            sequence: self.next_seq(channel_id),
            // App-level metadata; the MLS framing carries the crypto epoch.
            channel_epoch: 0,
            body,
            timestamp: Timestamp::now(),
        };
        let ciphertext = self
            .engine
            .encrypt(channel_id, &message)
            .map_err(|e| BackendError::Internal(format!("encrypt application message: {e}")))?;
        // Ciphertext sealed at the authored epoch — release the channel lock
        // before the network append so the background poll isn't blocked on it.
        drop(guard);
        let envelope = ChannelEnvelope::Application(ciphertext)
            .to_bytes()
            .map_err(BackendError::Internal)?;
        // Application traffic is a blind append — the device-owned-epoch gate
        // is only for commits.
        let seq = self
            .send_on_channel(channel_id, envelope.clone(), None, attachment_refs)
            .await
            .map_err(BackendError::from)?;
        Ok(AppendedRecord {
            plane_ref: crate::plane::plane_ref(&channel_id.to_string(), &envelope),
            seq,
        })
    }

    /// Build the [`ChannelMessageBody`] for a chat send: a plain
    /// [`ChannelMessageBody::Text`] when there are no attachments, else a
    /// [`ChannelMessageBody::Attachments`] carrying the caption plus one
    /// [`ChannelAttachment`] per resolved attachment. Each attachment's
    /// plaintext bytes are sealed under the channel's current-epoch blob key
    /// ([`MlsEngine::seal_conversation_blob`] — the raw epoch secret never
    /// leaves the engine), the sealed blob is uploaded to the nest's
    /// content-addressed store ([`ConversationsRpc::blob_put`]), and only the
    /// reference (`sealed_cid` to GET it + the uniform plaintext `blob_hash`
    /// to render under + `epoch` to pick the open key) rides in the sealed
    /// channel message (`docs/goal/ui/conversations.md` § Attachments +
    /// § Encryption at rest). The seal+upload happens before `channel_send`, so
    /// the message never references a blob a peer can't yet fetch. This is the
    /// end-to-end class's half; a community room seals under its attachment
    /// content kind inside [`Self::send_room_message`], and both classes upload
    /// through [`Self::upload_sealed_attachment`].
    ///
    /// Also returns the sealed cids as the plaintext `attachment_refs` the
    /// send lists beside the envelope — the conversation kind's
    /// blob-reachability floor ([`ConversationsRpc::channel_send`]): the nest
    /// cannot read the body, so this list is what keeps the uploaded blobs
    /// alive past the GC's grace window. And returns, by handle, where each
    /// blob rests and the epoch that opens it — the sender's own coordinates
    /// ([`SendOutcome::attachment_coordinates`]).
    async fn encode_body(
        &self,
        channel_id: &ChannelId,
        compose: &ComposeState,
        attachments: &[crate::backend::ResolvedAttachment],
    ) -> Result<
        (
            ChannelMessageBody,
            Vec<String>,
            Vec<(String, AttachmentCoordinates)>,
        ),
        BackendError,
    > {
        if attachments.is_empty() {
            return Ok((
                ChannelMessageBody::Text(compose.body_draft.clone()),
                Vec::new(),
                Vec::new(),
            ));
        }
        let mut items = Vec::with_capacity(attachments.len());
        let mut attachment_refs = Vec::with_capacity(attachments.len());
        let mut attachment_coordinates = Vec::with_capacity(attachments.len());
        for att in attachments {
            let sealed = self
                .engine
                .seal_conversation_blob(channel_id, &att.bytes)
                .map_err(|e| BackendError::Internal(format!("seal attachment: {e}")))?;
            let (item, blob) = self
                .upload_sealed_attachment(
                    channel_id,
                    att,
                    sealed.sealed,
                    AttachmentOpeningKey::MlsEpoch {
                        epoch: sealed.epoch,
                    },
                )
                .await?;
            attachment_refs.push(blob.sealed_cid_hex.clone());
            attachment_coordinates.push((
                att.blob_hash.clone(),
                AttachmentCoordinates::FaunaMls {
                    channel: *channel_id,
                    blob,
                },
            ));
            items.push(item);
        }
        Ok((
            ChannelMessageBody::Attachments {
                body: compose.body_draft.clone(),
                attachments: items,
            },
            attachment_refs,
            attachment_coordinates,
        ))
    }

    /// Upload one already-sealed attachment blob to where the record will rest
    /// — the channel's **home** nest, by the same `ChannelHome` signal
    /// `send_on_channel` reads to pick `send` vs `send_remote`
    /// (`conversation-rooms.md` § The home nest → *Attachment bytes*) — and
    /// build the reference the sealed message carries. Shared by both classes,
    /// which differ only in the seal that produced `sealed` and so in the `key`
    /// that opens it: the end-to-end path seals under the MLS epoch blob key and
    /// stamps that `epoch`; the community path seals under the room's attachment
    /// content kind off a generation and writes `epoch = 0`
    /// (`community-rooms.md` § The three classes → *Attachments — the second
    /// content kind*). Returns the reference and where the blob now rests — the
    /// coordinates the receive loop remembers for a received attachment, whose
    /// content address is also the plaintext `attachment_refs` entry that pins
    /// the blob past the GC.
    async fn upload_sealed_attachment(
        &self,
        channel_id: &ChannelId,
        att: &crate::backend::ResolvedAttachment,
        sealed: Vec<u8>,
        key: AttachmentOpeningKey,
    ) -> Result<(ChannelAttachment, SealedBlobCoordinates), BackendError> {
        let epoch = match &key {
            AttachmentOpeningKey::MlsEpoch { epoch } => *epoch,
            AttachmentOpeningKey::RoomGeneration { .. } => 0,
            // Only a carried value is ever `Unknown`; a send never seals one.
            AttachmentOpeningKey::Unknown(_) => return Err(BackendError::NotSupported),
        };
        let size_bytes = att.bytes.len() as u64;
        // The content address IS the BLAKE3 of the sealed bytes — the nest's
        // store key and the `sealed_cid` the receiver GETs by.
        let sealed_cid = *blake3::hash(&sealed).as_bytes();
        let sealed_cid_hex = hex::encode(sealed_cid);
        self.rpc
            .blob_put(
                channel_id.to_string(),
                self.channel_home_url(channel_id),
                sealed_cid_hex.clone(),
                sealed,
            )
            .await?;
        let blob_hash = parse_content_hash_hex(&att.blob_hash).ok_or_else(|| {
            BackendError::Internal(format!(
                "attachment blob_hash is not 32-byte hex: {}",
                att.blob_hash
            ))
        })?;
        Ok((
            ChannelAttachment {
                blob_hash,
                sealed_cid: ContentHash::from_digest_raw(sealed_cid),
                filename: att.filename.clone(),
                mime_type: att.mime_type.clone(),
                size_bytes,
                is_image: att.is_image,
                epoch,
            },
            SealedBlobCoordinates {
                sealed_cid_hex,
                size_bytes,
                key,
            },
        ))
    }

    /// Does the nest's routing roster hold `actor_hex` for this channel?
    ///
    /// `Some(true)`/`Some(false)` are answers; **`None` means the roster could
    /// not be read**, never "absent" — a transport failure, or a **foreign-homed channel**
    /// (guard below). Callers must treat `None` as "refuse to act on
    /// membership", not as a negative ([`ConversationsRpc::channel_actors`]
    /// carries the same contract).
    ///
    /// The channel's HOME nest holds the whole roster — `actor_channels` for
    /// its own users plus `channel_foreign_members` for every member it
    /// relayed a Welcome to — and `channel.actors` answers their union. Any
    /// other nest holds rows for its own users only, so asking it would return
    /// a *partial* roster that reads `Some(false)` for a healthy member homed
    /// elsewhere — and the heal would evict + re-invite a working member. A
    /// foreign-homed channel (a recorded Welcome `nest_url`, the same signal
    /// that routes `channel.fetch`/`send` relays) therefore rides its home URL
    /// through the seam, which relays the read to the channel's home via the
    /// distinct kind `channel.actors_remote` →
    /// `fauna.federation.channel.actors` (`federation.md` § Cross-nest). A
    /// nest too old for the relay — on either side — degrades to `None`, the
    /// refuse arm, never a partial-roster answer (the reason it is a distinct
    /// kind, not an additive field).
    async fn roster_holds(
        &self,
        channel_id: &ChannelId,
        actor_hex: &str,
    ) -> Result<Option<bool>, BackendError> {
        Ok(self
            .rpc
            .channel_actors(channel_id.to_string(), self.channel_home_url(channel_id))
            .await?
            .map(|actors| actors.iter().any(|a| a.eq_ignore_ascii_case(actor_hex))))
    }

    /// Stage + distribute + merge an `add_member` commit, yielding the
    /// newcomer's Welcome bytes. **The caller must already hold
    /// [`Self::channel_lock`] for `channel_id`** — both branches below assume
    /// no inbound poll can interleave, and `add_participant`'s phantom heal
    /// needs one guard spanning its evict *and* this admit.
    ///
    /// With a [`CommitGate`] injected, stage-gate-merge through the
    /// device-owned-epoch rebase loop (rebasing on a
    /// `fauna.conversations.channel.stale` rejection); without one, the
    /// gate-less staged path — stage → send → merge only once the nest
    /// accepted, per `devices.md` § Cross-device MLS group-state sync **Rule 1
    /// (merge-ordering)**. The merge happens only after the commit bytes are
    /// durable on the nest log, exactly where a restart's log re-walk finds
    /// them. The old optimistic path (merge, then send) turned any send failure
    /// into a permanent whole-group strand: MLS cannot re-issue a commit for an
    /// already-merged transition, and the debounced replica autosave can
    /// durably capture a merged-but-undistributed epoch off ANY channel's
    /// activity. On a send failure the pending is cleared (returning the group
    /// to its pre-commit state) and the error surfaced — cleanly retryable. The
    /// staged pending is persisted BEFORE the send (`persist_engine_state`,
    /// Rule 1's durable-local-pending discipline — native only), so a crash at
    /// any point reconciles on relaunch: commit on the log → the poll's
    /// own-commit hash-match merges the reloaded pending; commit absent after a
    /// full walk → the resumed pending is cleared and the operation retried.
    ///
    /// Returns the commit's log position — what the send answered — beside
    /// the Welcome, for the roster report the commit owes
    /// ([`Self::report_roster`]).
    async fn admit_member_locked(
        &self,
        channel_id: &ChannelId,
        kp_bytes: Vec<u8>,
    ) -> Result<(i64, Vec<u8>), BackendError> {
        if let Some(gate) = self.commit_gate() {
            return gate.gated_add_member(*channel_id, kp_bytes).await;
        }
        let (commit_bytes, welcome_bytes) = self
            .engine
            .add_member_staged_from_bytes(channel_id, &kp_bytes)
            .map_err(|e| BackendError::Internal(format!("stage add commit: {e}")))?;
        self.persist_engine_state();
        let envelope = ChannelEnvelope::Commit(commit_bytes)
            .to_bytes()
            .map_err(BackendError::Internal)?;
        match self
            .send_on_channel(channel_id, envelope, None, Vec::new())
            .await
        {
            Ok(seq) => {
                self.engine
                    .merge_pending_commit(channel_id)
                    .map_err(|e| BackendError::Internal(format!("merge add commit: {e}")))?;
                self.persist_engine_state();
                Ok((seq, welcome_bytes))
            }
            Err(e) => {
                if let Err(clear_err) = self.engine.clear_pending_commit(channel_id) {
                    tracing::warn!(
                        error = %clear_err,
                        "clearing the staged add commit failed after a send failure"
                    );
                }
                self.persist_engine_state();
                Err(e.into())
            }
        }
    }

    /// Stage + distribute + merge a `remove_member` commit evicting `leaf` — an
    /// MLS Commit that re-keys the group so the removed member can't follow
    /// forward. No Welcome: the removed member simply falls off the epoch.
    /// **The caller must already hold [`Self::channel_lock`]** (see
    /// [`Self::admit_member_locked`] for the full gated/gate-less rationale,
    /// which is identical here). Returns the commit's log position.
    async fn evict_leaf_locked(
        &self,
        channel_id: &ChannelId,
        leaf: u32,
    ) -> Result<i64, BackendError> {
        if let Some(gate) = self.commit_gate() {
            return gate.gated_remove_member(*channel_id, leaf).await;
        }
        let commit_bytes = self
            .engine
            .remove_member_staged(channel_id, leaf)
            .map_err(|e| BackendError::Internal(format!("stage remove commit: {e}")))?;
        self.persist_engine_state();
        let envelope = ChannelEnvelope::Commit(commit_bytes)
            .to_bytes()
            .map_err(BackendError::Internal)?;
        match self
            .send_on_channel(channel_id, envelope, None, Vec::new())
            .await
        {
            Ok(seq) => {
                self.engine
                    .merge_pending_commit(channel_id)
                    .map_err(|e| BackendError::Internal(format!("merge remove commit: {e}")))?;
                self.persist_engine_state();
                Ok(seq)
            }
            Err(e) => {
                if let Err(clear_err) = self.engine.clear_pending_commit(channel_id) {
                    tracing::warn!(
                        error = %clear_err,
                        "clearing the staged remove commit failed after a send failure"
                    );
                }
                self.persist_engine_state();
                Err(e.into())
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RailBackend for FaunaMlsBackend {
    fn rail(&self) -> Rail {
        Rail::FaunaMls
    }

    /// The recorded owner of every folder channel this seat holds
    /// ([`Self::folder_poll_channels`] × `MlsEngine::folder_channel_owner`),
    /// this account's own identity excluded — a set this seat owns has no peer
    /// to harvest. Derived from the engine on every call, so a set joined or
    /// re-stamped mid-session is walked on the sweep's next pass.
    fn harvest_anchor_wants(&self) -> Vec<ActorId> {
        let mut wants = Vec::new();
        for channel in self.folder_poll_channels() {
            if let Some(owner) = self.engine.folder_channel_owner(&channel)
                && owner != self.self_actor
                && !wants.contains(&owner)
            {
                wants.push(owner);
            }
        }
        wants
    }

    fn capabilities(&self, thread: &ThreadDetail) -> ThreadCapabilities {
        let base = derive_capabilities(Rail::FaunaMls, thread.flavor.clone());
        match self.room_state(thread) {
            Some(room) => room.gate(base),
            None => base,
        }
    }

    /// The room a FaunaMls thread is (`conversation-rooms.md` § The room):
    /// every participant a user principal (this rail seats no nest and no
    /// bridge, so the class is end-to-end by derivation), the roles read off
    /// the bound channel's group-context policy through
    /// [`MlsEngine::room_policy`], the viewer's own role beside them. A thread
    /// with no bound channel, or a bound channel whose context carries no
    /// policy, is a **policy-less** room: class, no policy, no roles.
    ///
    /// A policy that is present but does not decode is agreed group state
    /// every member fails identically — every commit on that channel is
    /// refused (`fauna_mls::room_policy`) — and is rendered as policy-less
    /// here rather than hidden, with the failure logged once per read.
    fn room_state(&self, thread: &ThreadDetail) -> Option<RoomSnapshot> {
        use crate::room::{PrincipalKind, RoomMemberSnapshot, derive_room_class};
        let policy = self
            .channel_for(&thread.thread_id)
            .and_then(|channel| self.engine.room_policy(&channel))
            .and_then(|read| match read {
                Ok(policy) => Some(policy),
                Err(e) => {
                    tracing::warn!(
                        thread = %thread.thread_id.0,
                        error = %e,
                        "the room's policy extension does not decode; rendering it as policy-less"
                    );
                    None
                }
            });
        // The floor, for a channel with no MLS group to ask. A community room
        // IS one — it is born by `room.create` — and its class, ranks and
        // policy live on its home nest rather than in a group context this
        // device could read. Absent until the poll pass has filled it, which
        // renders as the local answer below rather than as a guess.
        let cached = self
            .channel_for(&thread.thread_id)
            .and_then(|channel| self.room_floors.lock().ok()?.get(&channel).cloned());

        let members: Vec<RoomMemberSnapshot> = thread
            .participants
            .iter()
            .map(|p| RoomMemberSnapshot {
                // A participant is always a user principal: the nest and bridge
                // principals a floor seats are not participants any thread
                // renders, which is why the class is derived from the FLOOR
                // below and never from this list. Keeping the list
                // participant-parallel is the contract every app's chip render
                // relies on (`RoomSnapshot::members`).
                kind: PrincipalKind::User,
                role: match (&policy, &cached, p.person_actor_id()) {
                    (Some(policy), _, Some(actor)) => Some(policy.role_of(&actor).into()),
                    (None, Some(floor), Some(actor)) => floor.roles.get(&actor).copied(),
                    _ => None,
                },
            })
            .collect();
        // **The class is a function of the member set, and the member set is
        // the floor's** (§ Architectural rules, rule 1) — so it is derived from
        // the floor's principal kinds when this device has them, and from the
        // participants only as the fallback for a room whose floor it has not
        // read. That fallback can never answer `Community`, which is correct:
        // the home nest is not a participant, so a room read that way is
        // end-to-end (or transport-only, once a bridge participant is
        // recognised) — the honest answer for a room this device knows nothing
        // else about.
        let class = match &cached {
            Some(floor) => derive_room_class(floor.kinds.iter().copied()),
            None => derive_room_class(members.iter().map(|m| m.kind)),
        };
        Some(RoomSnapshot {
            class,
            my_role: match (&policy, &cached) {
                (Some(policy), _) => Some(policy.role_of(&self.self_actor).into()),
                (None, Some(floor)) => floor.roles.get(&self.self_actor).copied(),
                _ => None,
            },
            policy: match (&policy, &cached) {
                (Some(policy), _) => Some((&policy.signed.policy).into()),
                (None, Some(floor)) => floor.policy.clone(),
                _ => None,
            },
            // Off the floor or not at all: an MLS room seats no nest, and a
            // community room's answer is the tip's wrap set, which only the
            // floor read carries.
            nest_read: cached.as_ref().and_then(|floor| floor.nest_read),
            // Off the floor for the same reason: only a community room has a
            // home nest that labels, and only its floor read carries the set.
            labelers: cached.as_ref().and_then(|floor| floor.labelers.clone()),
            // Off the walk: only a community room's walk waits for a key-in.
            awaiting_key: self
                .channel_for(&thread.thread_id)
                .is_some_and(|channel| self.is_awaiting_key(&channel)),
            // Off the parked set and the harvest: only a community room's
            // floor delete records are ever parked on a name.
            moderation_unverified: self
                .channel_for(&thread.thread_id)
                .is_some_and(|channel| self.moderation_unverified(&channel)),
            // Off its own read, beside the floor's: only a community room's
            // floor holds invitations, and only a list the nest served is
            // painted. Named by the handle the nest joined, else the elided id
            // — display only, like every name on this surface.
            pending_invites: self
                .channel_for(&thread.thread_id)
                .and_then(|channel| {
                    self.room_pending_invites
                        .lock()
                        .ok()?
                        .get(&channel)
                        .cloned()
                })
                .map(|pending| {
                    let display = |actor: ActorId, handle: &Option<String>| {
                        TypedAddress::Fauna {
                            actor_id: actor,
                            handle: handle.clone().unwrap_or_default(),
                        }
                        .display()
                    };
                    pending
                        .iter()
                        .map(|i| crate::room::RoomPendingInviteSnapshot {
                            invitee_actor_hex: i.invitee.to_hex(),
                            invitee_display: display(i.invitee, &i.invitee_handle),
                            inviter_display: display(i.inviter, &i.inviter_handle),
                            role: i.role,
                            invited_at_ms: i.invited_at_ms,
                            lapsed: !i.still_acceptable,
                        })
                        .collect()
                }),
            members,
        })
    }

    fn seated_on_room(&self, thread: &ThreadDetail) -> bool {
        match self.channel_for(&thread.thread_id) {
            // An end-to-end room is its MLS group: seated while the group is
            // active, unseated once this device has processed its removal.
            Some(channel) if self.engine.has_group(&channel) => {
                self.engine.is_group_active(&channel)
            }
            // A community room seats no group here; whether this device holds
            // the tip's wrap is decided where the post is sealed
            // (`room_post_seal_key`), against the nest's answer, not guessed
            // from a cache.
            _ => true,
        }
    }

    fn self_address(&self) -> Option<TypedAddress> {
        Some(TypedAddress::Fauna {
            actor_id: self.self_actor,
            handle: self.self_address.get(),
        })
    }

    /// Hand this rail's `MlsEngine` role over to the successor being built for
    /// the same account: flush and release the conversations-engine role lock on
    /// `mls_state.db` ([`MlsEngine::retire`]). The one rail that overrides
    /// [`RailBackend::retire`], because it is the one rail holding a
    /// process-exclusive resource.
    ///
    /// The engine `Arc` itself is deliberately *not* what is released here: it is
    /// shared with the session that owns this backend, with the FFI client's own
    /// stash, and — through UniFFI — with app-shell fields no Rust code can
    /// reach. Releasing the **role** instead makes the hand-over independent of
    /// all of them.
    /// `cfg`'d off-wasm because the role lock — and therefore `MlsEngine::retire`
    /// — exists only on the file-backed (`fauna-mls/native`) store; a wasm
    /// engine is in-memory and unshareable by construction, so the trait's
    /// no-op default is the correct behaviour there. Same target gate the rest of
    /// this file uses for native-only engine reach.
    #[cfg(not(target_arch = "wasm32"))]
    fn retire(&self) {
        self.engine.retire();
        // AFTER the engine's own retire, never before: the receive loop wakes on
        // this and exits, and a loop that woke first could still be mid-sweep
        // against an engine that has not yet flushed its provider snapshot.
        // `send` fails only when every receiver is gone, which is simply a
        // session whose loop already left - nothing to wake, nothing to report.
        let _ = self.retired.send(true);
    }

    /// Harvest the *known Fauna domain* evidence: the domain of every
    /// `TypedAddress::Fauna` participant this account converses with. Every
    /// such address was minted by a nest answering (a resolve, or a Welcome
    /// from a peer at that domain), so it is proof a Fauna nest serves the
    /// domain. Other rails' addresses vouch for nothing.
    fn observe_participants(&self, participants: &[TypedAddress]) {
        let mut known = self.known_domains.lock().unwrap();
        for p in participants {
            if let TypedAddress::Fauna { handle, .. } = p
                && let Some(domain) = handle_domain(handle)
            {
                known.insert(domain);
            }
        }
    }

    async fn resolve_address(&self, raw: &str) -> ResolveResult {
        // Form 1 — actor-id: a 64-hex string IS the 32-byte actor key, so
        // `TypedAddress::Fauna` needs no handle lookup. (Cross-nest /
        // WebFinger / DID resolution is a later slice.)
        if let Some(actor_id) = parse_actor_id_hex(raw) {
            return self
                .resolve_reachable(actor_id, raw.trim().to_string())
                .await;
        }

        // Form 2 — handle: a bare localpart (`alice`) or a `localpart@domain`
        // (`alice@nest.test`). Look it up on the logged-in nest via
        // `fauna.actor.by_handle` (the same-nest slice; cross-nest is deferred).
        // Shapes that belong to other rails (ActivityPub `@u@i`, Nostr `npub1…`,
        // Bluesky `did:…`) and junk report `NotFound`, so the manager's chain
        // falls through to those backends / the format-only parse.
        let Some((localpart, typed_domain)) = parse_fauna_handle(raw) else {
            return ResolveResult::NotFound;
        };
        // Same-nest probe first — it also yields this nest's handle domain, which
        // disambiguates a typed `localpart@domain`. A bare unknown handle is a
        // genuine not-found. A transport failure — the home nest not answering —
        // is `Error` for a bare handle, a handle typed at the local actor's own
        // domain, or while the self domain is still unknown (the home domain is
        // always a known Fauna domain, and pre-identity nothing can be
        // classified); a typed domain that is provably foreign still gets its
        // own probe — the peer may well be up (`federation.md` § Peer-auth model
        // → *Discovery-failure semantics*).
        let same = match self.rpc.actor_by_handle(localpart.clone()).await {
            Ok(opt) => opt,
            Err(e) => {
                if let Some(d) = typed_domain.as_deref()
                    && self
                        .self_domain()
                        .is_some_and(|own| !own.eq_ignore_ascii_case(d))
                {
                    return self.resolve_foreign(&localpart, d).await;
                }
                return ResolveResult::Error(e.to_string());
            }
        };
        // A typed domain that is NOT this nest's is a cross-nest Fauna handle,
        // resolved directly against that peer nest (a local handle of the same
        // localpart under a different domain is a distinct foreign actor —
        // `bob@other.test` ≠ the local `bob`). A bare handle, or a typed domain
        // matching this nest, promotes same-nest.
        //
        // The verdict comes from the shared owner rather than an inline match
        // here: the contacts Find User / knock-send path makes the identical
        // call to decide `fauna.inbox.send`'s `recipient_nest_url`, and two
        // copies of this three-case rule is how `bob@other.test` comes to mean
        // two different actors on two pages of one app (priorities #1/#3/#4).
        // `same`'s echoed `domain` IS this nest's handle domain; a `None` reply
        // volunteers none, which the rule reads as "probe the typed domain".
        let foreign = fauna_core::resolve::is_foreign_handle_domain(
            typed_domain.as_deref(),
            same.as_ref().map(|r| r.echoed_domain.as_str()),
        );
        if foreign {
            let d = typed_domain
                .as_deref()
                .expect("foreign resolution implies a typed domain");
            return self.resolve_foreign(&localpart, d).await;
        }
        match same {
            Some(resolved) => {
                let Some(actor_id) = parse_actor_id_hex(&resolved.actor_id_hex) else {
                    return ResolveResult::Error("nest returned a malformed actor id".to_string());
                };
                // Canonical `localpart@domain` display, regardless of which form
                // was typed (so a bare-handle chip still shows the full address).
                let canonical = format!("{localpart}@{}", resolved.echoed_domain);
                self.resolve_reachable(actor_id, canonical).await
            }
            // Bare handle with no local actor → fall through to other rails.
            None => ResolveResult::NotFound,
        }
    }

    fn bucket_inbound(
        &self,
        msg: RailInboundMessage,
        _mailbox: Option<MailFeed>,
    ) -> Result<InboundBucket, BackendError> {
        // Pure transform (mirrors `smtp::bucket_inbound`): the inbound driver
        // (`poll_inbound_conv`) has already decoded + MLS-decrypted the
        // `ChannelEnvelope`, so this just shapes the plaintext into the bucket.
        // FaunaMls routing is channel-keyed (the driver passes the bound thread
        // directly via `ingest_inbound_to_thread`), so `participants`/`subject`
        // here are informational only.
        // Compute is_own before msg.sender moves into the shared builder. A
        // multi-device self-echo: any Fauna address whose actor_id matches
        // self.self_actor is ours, regardless of which device sent it.
        let is_own = matches!(&msg.sender, TypedAddress::Fauna { actor_id, .. } if *actor_id == self.self_actor);
        let subject = msg.subject.clone();
        Ok(crate::backend::bucket_inbound_common(msg, subject, is_own))
    }

    async fn send(
        &self,
        thread: &ThreadDetail,
        compose: &ComposeState,
        // FaunaMls attachments seal as separate per-channel blobs — under the
        // group epoch key (`derive_blob_key(epoch_secret)`) on the end-to-end
        // class, under the room's attachment content kind on the community
        // class — and ride the nest's content-addressed blob store; the channel
        // message carries only the lightweight references
        // ([`ChannelAttachment`]). See `encode_body` / `send_room_message` +
        // `docs/goal/ui/conversations.md` § Attachments.
        attachments: &[crate::backend::ResolvedAttachment],
    ) -> Result<SendOutcome, BackendError> {
        // Existing thread → its bound channel; new thread → lazily bootstrap an
        // MLS group (fetch keypackages → create_group → bind → deliver Welcome).
        let (channel_id, born_here) = match self.channel_for(&thread.thread_id) {
            Some(c) => (c, false),
            None => (self.bootstrap_group(thread).await?, true),
        };

        // **A community room has no MLS group** — it is born by `room.create`,
        // never by `bootstrap_group`, and its content seals under the room's
        // generation key instead of a channel ratchet
        // (`conversation-rooms.md` § The three classes → *Community*). A bound
        // channel with no group is therefore the class's own local signature:
        // no round trip, no cached class projection, and true on any device
        // that gets this far. (A channel bound to a group this device has not
        // joined lands here too, and gets the community path's "no generation"
        // refusal — which names the actual problem better than the MLS path's
        // would.)
        if !self.engine.has_group(&channel_id) {
            return self
                .send_room_message(&channel_id, compose, attachments)
                .await;
        }

        tracing::debug!("send: encode_body");
        let (body, attachment_refs, attachment_coordinates) =
            self.encode_body(&channel_id, compose, attachments).await?;
        tracing::debug!("send: post_app_message");
        let appended = self
            .post_app_message(&channel_id, body, attachment_refs)
            .await?;
        tracing::debug!("send: post_app_message done");

        // The **birth report** (`conversation-rooms.md` § The floor roster →
        // *End-to-end rooms*): the roster the group's creation produced, to
        // the room's home nest — this device is the committing device of that
        // creation, and for a 1:1 this is the only report it will ever make
        // (its membership is fixed for life; an add-participant forks a new
        // group). After the first post and never before: that post is what
        // registers this actor on the channel's routing roster at the home
        // nest — the report door's first gate — and the creator's channel is
        // same-nest by construction. No position: the creation is not on the
        // room log, and the door's bootstrap bound admits a self-naming first
        // report on an empty floor.
        if born_here {
            self.report_roster(&channel_id, None).await;
        }

        Ok(SendOutcome {
            message_id: MessageId(format!("conv:{channel_id}:{}", appended.seq)),
            timestamp_ms: Timestamp::now_millis() as i64,
            sender: TypedAddress::Fauna {
                handle: self.self_address.get(),
                actor_id: self.self_actor,
            },
            // The sender's ONLY chance to learn it: a sender cannot MLS-decrypt
            // its own application message, so this record never comes back
            // through `poll_inbound_conv`.
            plane_ref: appended.plane_ref,
            attachment_coordinates,
        })
    }

    /// Add a participant to an **existing** (already-bound) MLS group — the
    /// in-place add path (`docs/goal/ui/conversations.md:30-33`). The 1:1→group
    /// fork is *not* routed here: the manager forks a fresh participant-keyed
    /// thread (`manager.rs` `add_participant_inner`) whose `send` lazily
    /// bootstraps the new group via [`Self::bootstrap_group`]; only a thread
    /// already bound to a channel reaches this method. Fetch the new member's
    /// key package, stage an add commit (one MLS Commit so every existing member
    /// ratchets forward, plus a Welcome for the newcomer), post the Commit so
    /// peers consume it via `poll_inbound_conv`'s `process_commit` arm, merge it
    /// once the nest accepted (devices.md Rule 1), and deliver the Welcome.
    ///
    /// **Idempotent + self-healing**, on the folder share path's ratified shape
    /// (`mls-group-key-material.md` § M2 *Admitting a member*). An identity
    /// already seated in the group is one of three things, discriminated on the
    /// nest's routing roster — which the nest writes at Welcome delivery, so its
    /// **absence** is exactly "the Welcome never landed" (⚠ the *presence*
    /// direction is weaker — see the note under the three arms):
    /// - **on the roster** → a healthy member; idempotent no-op. On the
    ///   channel's home nest this covers **cross-nest members too**: their
    ///   `channel_foreign_members` row (written when this nest relayed their
    ///   Welcome) is part of the `channel.actors` union.
    /// - **off the roster** → a post-crash *phantom leaf*; evict it and re-admit
    ///   with a fresh KeyPackage (a Welcome is mintable only inside the Add
    ///   commit, so re-admission is the only way to produce one).
    /// - **roster unreadable** (a transport failure, or a **foreign-homed channel** —
    ///   the caller's own nest cannot answer for the home's roster, see
    ///   [`Self::roster_holds`]) → refuse and say so; never guess at membership.
    ///
    /// The heal is authoritative when this member's own nest homes the channel
    /// (it bootstrapped the group); on a foreign-homed channel it refuses, by
    /// declaration, until the roster read relays to the channel's home — see
    /// the doc's Implementation-status note.
    ///
    /// ⚠ **Roster presence is not a Welcome witness on an unclaimed channel, and
    /// the on-roster arm's no-op trusts that it is.** `channel.fetch`/`send`
    /// auto-register any authenticated local caller on an unclaimed channel
    /// (ratified append-only/by-activity, `direct-messages.md` § Security
    /// Properties), so there `actor_channels` is an **activity log**. A phantom-leaf
    /// actor who fetches the channel thereby registers themselves, and the next add
    /// takes the idempotent no-op instead of healing — the UI reporting success.
    /// **Declared residual** (narrow-the-claim rather than add a witness): not naturally
    /// reachable (the client has no Welcome for that channel), no confidentiality
    /// impact, not third-party inducible (auto-register writes a row for *the
    /// caller*, so C cannot poison A's), and remove-then-add recovers it — the
    /// removal unseats the leaf, so the following add takes the fresh-admit path.
    /// The exact equivalence needs a Welcome-specific witness (a `welcomed_at`
    /// column, or a table distinct from the activity roster); do not "fix" it by
    /// suppressing auto-register on MLS-bearing channels without re-opening the
    /// by-activity ratification, which is a separate design call.
    async fn add_participant(
        &self,
        thread_id: ThreadId,
        addr: TypedAddress,
    ) -> Result<(), BackendError> {
        let actor_id = fauna_actor(&addr)?;
        // A foreign new member routes both the key-package fetch and the Welcome
        // through the home-nest federation relay (`nest_url`).
        let peer_domain = match &addr {
            TypedAddress::Fauna { handle, .. } => self.peer_domain_for(handle),
            _ => None,
        };
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot add a participant to an unbound thread".to_string())
        })?;

        // **Never route the local identity into the heal below.** Adding
        // yourself is a no-op gesture, but the owner's own leaf answers
        // `find_leaf_by_identity` exactly like a newcomer's would — and an
        // owner who has not yet posted is not on the roster either, so an
        // unguarded heal would read "phantom" and evict the caller from their
        // own group. Refuse up front rather than encode the exception downstream.
        if actor_id == self.self_actor {
            return Err(BackendError::Internal(
                "you are already a member of this group".to_string(),
            ));
        }

        // **A community room has no MLS group to add a leaf to.** Its
        // membership is the home nest's floor, and joining it is three acts:
        // this device's signed invitation, the invitee's acceptance, and a
        // key-in by an owner or admin, which `tend_community_room` runs once
        // the floor shows the newcomer. So the gesture here is the invitation,
        // and it seats nobody. The same local signature the send and policy
        // paths branch on — the apps never learn the class.
        if !self.engine.has_group(&channel_id) {
            self.invite_to_room(
                &channel_id,
                actor_id,
                fauna_mls::room_policy::RoomRole::Member,
                peer_domain,
            )
            .await?;
            self.refresh_floor_soon(&channel_id);
            return Ok(());
        }

        let actor_hex = hex::encode(actor_id.0);

        // **One channel-lock acquisition spans the whole gesture** — the
        // discriminator read, the phantom heal's evict, and the admit. The heal
        // is two sequential commits, and taking the lock per-commit would let
        // the background inbound poll (or a second add) land between them, in
        // the one window where the group is deliberately mid-repair. Everything
        // below therefore uses the `*_locked` commit drivers, which assume the
        // guard is already held. (Network I/O under this lock is the norm here:
        // the gated driver's own rebase loop does CAS-put + send + catch-up poll
        // inside it.)
        let channel_lock = self.channel_lock(&channel_id);
        let _guard = channel_lock.lock().await;

        // The roles table, before any commit exists to refuse
        // (`conversation-rooms.md` § Roles and authorization).
        self.ensure_may_invite(&channel_id)?;

        // Is this identity already seated? Then the gesture is one of three
        // things, and local MLS state alone cannot tell them apart — a healthy
        // member and a post-crash **phantom leaf** are byte-identical here. The
        // nest's routing roster is what separates them: it gains the member's
        // row at Welcome delivery, precisely the step a crash-window add never
        // reached (`mls-group-key-material.md` § M2 *Admitting a member*).
        let mut healed_phantom = false;
        if let Some(leaf) = self.engine.find_leaf_by_identity(&channel_id, &actor_id) {
            match self.roster_holds(&channel_id, &actor_hex).await? {
                // On the roster ⇒ a healthy member, and this is a duplicate
                // gesture (a stale overlay, a second device, a retry whose
                // first attempt actually landed). Idempotent no-op: their
                // membership is untouched and re-admitting would needlessly
                // cut them off and re-invite them.
                Some(true) => return Ok(()),
                // Seated but off the roster ⇒ the Welcome never landed. Evict
                // the ghost leaf through the ordinary removal machine (its
                // re-key is unnecessary when the Welcome provably never landed
                // but harmless — reusing the proven crash-safe path beats a
                // bespoke rotation-less evict), then fall through and admit
                // fresh below. A Welcome is mintable only inside the Add commit
                // itself, so re-admission is the *only* way to produce one.
                Some(false) => {
                    self.evict_leaf_locked(&channel_id, leaf).await?;
                    healed_phantom = true;
                }
                // Roster unreadable — a transport failure, or a
                // foreign-homed channel whose authoritative roster lives on its
                // home nest (`roster_holds`'s guard). Refuse to guess: evicting
                // a member we cannot vouch for is the one outcome strictly
                // worse than doing nothing. A specific, actionable error, never
                // MLS's raw duplicate-credential string.
                None => {
                    return Err(BackendError::Internal(format!(
                        "{actor_hex} is already in this group's MLS state, and this nest \
                         cannot confirm whether their invitation was delivered. If they \
                         never received it, remove them from the group and add them again."
                    )));
                }
            }
        }

        // A **fresh** key package per attempt: the phantom's add consumed the
        // previous one, and a Welcome cannot be re-minted for an already-seated
        // leaf — so the heal's re-admission needs its own.
        let kp_bytes = match self
            .rpc
            .keypackage_fetch(actor_hex.clone(), peer_domain.clone())
            .await
        {
            Ok(Some(kp)) => kp,
            Ok(None) => {
                return Err(evict_orphan_context(
                    healed_phantom,
                    BackendError::Internal(format!("no key package available for {actor_hex}")),
                ));
            }
            Err(e) => return Err(evict_orphan_context(healed_phantom, e.into())),
        };

        // The half-done state this composition introduces that neither
        // operation has alone: the evict landed, the re-admission did not. It
        // is deliberately *not* a new failure mode to recover from — it is
        // strictly better than the phantom it replaced, because the group is
        // now internally consistent (they are simply not a member) and a plain
        // retry takes the ordinary fresh-add path. Say exactly that — as a
        // localized product statement carrying the underlying `user_detail()`.
        let (commit_seq, welcome_bytes) =
            match self.admit_member_locked(&channel_id, kp_bytes).await {
                Ok(admitted) => admitted,
                Err(e) => return Err(evict_orphan_context(healed_phantom, e)),
            };

        // The newcomer joins a multi-member group, so the Welcome carries the
        // raw group id (matches `bootstrap_group`'s >1-peer branch).
        let group_id_hex = self
            .engine
            .group_id_bytes(&channel_id)
            .map(hex::encode)
            .unwrap_or_default();
        self.rpc
            .welcome_deliver(
                actor_hex,
                channel_id.to_string(),
                welcome_bytes,
                WelcomeChannelKind::Group { group_id_hex },
                peer_domain,
            )
            .await?;

        // *Report, never guess*: the roster this commit produced, to the
        // room's home nest (`conversation-rooms.md` § The floor roster).
        self.report_roster(&channel_id, Some(commit_seq)).await;

        Ok(())
    }

    /// The MLS engine's roster for the thread's bound channel — the
    /// authoritative answer to "who is seated here?", and the same read the
    /// succession sweep raises its unattested-member flags off
    /// (`fauna_client_recovery::group_sweep::unattested_members`). See the trait
    /// method for why the eviction driver may not use the snapshot instead.
    ///
    /// **A bound channel is what makes this rail the authority, and the only
    /// thing that does.** No binding means no authority *yet* — a group the
    /// owner composed but never bootstrapped has no MLS group to ask, and its
    /// membership is a local intention the snapshot is the whole truth of.
    ///
    /// A bound channel whose group the engine no longer holds (a set this device
    /// left or forgot) is deliberately **not** distinguished from a group that
    /// seats nobody: `group_members` answers the empty roster for both, and
    /// empty is the honest answer either way — nobody here can be evicted from
    /// it. Guarding that case with `has_group` and falling back to the snapshot
    /// reads as the more careful choice and is the worse one: the fallback would
    /// attempt a removal the engine cannot resolve (`find_leaf_by_identity` →
    /// "is not a member of this group"), record the failure, and block the
    /// verdict on a group that will never come back — stranding the item that
    /// § Propagation rule (4) exists to let close.
    fn authoritative_roster(&self, thread_id: &ThreadId) -> Option<Vec<ActorId>> {
        let channel_id = self.channel_for(thread_id)?;
        Some(self.engine.group_members(&channel_id))
    }

    /// The engine's thread-less seats of `person`, classified from the same
    /// sources the polls route on: the bound-channel map first (a bound channel
    /// is the thread loop's business, not this method's), then the scheduling
    /// marker ([`Self::mark_scheduling_channel`]), then the durable chat marker
    /// (`MlsEngine::is_channel_chat`), and the remainder is the folder rail —
    /// exactly [`Self::is_folder_rail`]'s derivation, membership then read
    /// off `group_members`, the read the flag itself was raised from.
    fn unbound_seats_of(&self, person: &ActorId) -> Vec<crate::backend::UnboundSeat> {
        use crate::backend::{UnboundChannelClass, UnboundSeat};
        let bound: std::collections::HashSet<_> =
            self.channels.lock().unwrap().values().copied().collect();
        self.engine
            .list_groups()
            .into_iter()
            .filter(|ch| !bound.contains(ch))
            .filter(|ch| self.engine.group_members(ch).contains(person))
            .map(|ch| {
                let class = if self.is_scheduling_channel(&ch) {
                    UnboundChannelClass::Scheduling
                } else if self.engine.is_channel_chat(&ch) {
                    UnboundChannelClass::Chat
                } else {
                    UnboundChannelClass::Folder
                };
                UnboundSeat {
                    channel_hex: ch.to_string(),
                    class,
                }
            })
            .collect()
    }

    /// Remove a participant from an MLS group: find their leaf, `remove_member`
    /// (an MLS Commit that re-keys the group so the removed member can't follow
    /// forward), and post the Commit so remaining members ratchet via
    /// `poll_inbound_conv`'s `process_commit` arm. No Welcome — the removed
    /// member simply falls off the epoch.
    async fn remove_participant(
        &self,
        thread_id: ThreadId,
        addr: TypedAddress,
    ) -> Result<(), BackendError> {
        let actor_id = fauna_actor(&addr)?;
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot remove a participant from an unbound thread".to_string())
        })?;

        // **The lock spans the lookup, not just the evict** — same rule, and the
        // same reason, as `add_participant`'s one-acquisition-per-gesture note.
        // `find_leaf_by_identity` hands back a leaf *index*, which is only
        // meaningful against the tree it was read from: let the background
        // inbound poll merge a Commit in between and the index is stale, so the
        // gesture either fails to find a member who is plainly there (no Commit
        // posted at all — the remove silently does nothing) or, worse, names
        // whichever leaf now sits at that position and evicts the wrong person.
        // Reading it under the guard makes both unrepresentable.
        let channel_lock = self.channel_lock(&channel_id);
        let _guard = channel_lock.lock().await;

        // The roles table, before any commit exists to refuse
        // (`conversation-rooms.md` § Roles and authorization).
        self.ensure_may_remove(&channel_id, &actor_id)?;

        let leaf = self
            .engine
            .find_leaf_by_identity(&channel_id, &actor_id)
            .ok_or_else(|| {
                BackendError::Internal(format!(
                    "{} is not a member of this group",
                    hex::encode(actor_id.0)
                ))
            })?;

        // The gated driver's inner catch-up poll does not re-take this lock.
        let commit_seq = self.evict_leaf_locked(&channel_id, leaf).await?;
        self.report_roster(&channel_id, Some(commit_seq)).await;
        Ok(())
    }

    /// Rename a group by posting an encrypted `GroupMeta::NameChanged`
    /// **application** message (not a Commit) — group metadata rides the same
    /// sealed channel as chat, so the new name is end-to-end encrypted. Peers
    /// surface it as a rename via `poll_inbound_conv` (which applies it to the
    /// bound thread). No MLS epoch change, so no Commit and no ratchet.
    async fn rename(&self, thread_id: ThreadId, new_label: String) -> Result<(), BackendError> {
        let channel_id = self
            .channel_for(&thread_id)
            .ok_or_else(|| BackendError::Internal("cannot rename an unbound thread".to_string()))?;
        // On a governed room the name is a field of the owner-signed policy
        // (`conversation-rooms.md` § Roles and authorization), so a rename is
        // a policy change — a commit every member judges by the renamer's
        // role. A policy-less room keeps the application-message rename.
        if matches!(self.engine.room_policy(&channel_id), Some(Ok(_))) {
            return self
                .update_room_policy(thread_id, RoomPolicyEdit::Rename(new_label))
                .await;
        }
        self.post_app_message(
            &channel_id,
            ChannelMessageBody::GroupMeta(GroupMetaMessage::NameChanged(new_label)),
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    /// Apply one policy edit to a governed room: re-derive the policy under
    /// the viewer's role, sign it as this identity, and commit the
    /// group-context change — the roles table
    /// (`conversation-rooms.md` § Roles and authorization) applied once here
    /// so an honest app never authors a policy commit every other member
    /// refuses.
    async fn update_room_policy(
        &self,
        thread_id: ThreadId,
        edit: RoomPolicyEdit,
    ) -> Result<(), BackendError> {
        use fauna_i18n::strings::error::send;
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot change the policy of an unbound thread".to_string())
        })?;

        // **A community room has no MLS group**, so there is no group context to
        // commit a policy into: its policy is stored on the home nest and
        // changed through `room.set_policy` / `room.transfer_ownership`. Same
        // local signature the send path branches on, and the same reason the
        // apps never learn it — one door, one edit vocabulary, two classes
        // underneath (priority #1).
        if !self.engine.has_group(&channel_id) {
            self.set_room_policy(&channel_id, edit).await?;
            self.refresh_floor_soon(&channel_id);
            return Ok(());
        }

        // The transfer is the owner's act but not the owner's commit: it posts
        // an OFFER (an application message, which takes the channel lock
        // itself) and the incoming owner's device commits — so it leaves
        // before the lock below.
        if let RoomPolicyEdit::TransferOwnership(new_owner) = edit {
            return self.offer_ownership(&channel_id, new_owner).await;
        }

        let channel_lock = self.channel_lock(&channel_id);
        let _guard = channel_lock.lock().await;

        let current = self.governed_policy(&channel_id)?;
        let my_role = current.role_of(&self.self_actor);
        if !edit_permitted(&edit, my_role) {
            let owner_only = matches!(
                edit,
                RoomPolicyEdit::AppointAdmin(_) | RoomPolicyEdit::DemoteAdmin(_)
            );
            return Err(BackendError::Refusal(if owner_only {
                send::ROOM_ADMINS_OWNER_ONLY.to_string()
            } else {
                send::ROOM_POLICY_NOT_PERMITTED.to_string()
            }));
        }

        // Applied against whatever policy the channel holds at commit time,
        // with the role re-read there ([`RoomPolicyRebuild`]): the product
        // refusal above is for the user's finger, this one is for the race
        // a catch-up can fold in between.
        let me = self.self_actor;
        let engine = self.engine.clone();
        let rebuild: RoomPolicyRebuild = Box::new(move |current: &RoomPolicyExtension| {
            if !edit_permitted(&edit, current.role_of(&me)) {
                return Err(MlsError::PolicyViolation(
                    "the viewer's role no longer permits this policy change".into(),
                ));
            }
            let mut policy = current.signed.policy.clone();
            policy.version += 1;
            match &edit {
                RoomPolicyEdit::Rename(name) => policy.name = Some(name.clone()),
                RoomPolicyEdit::JoinRule(rule) => policy.join_rule = (*rule).into(),
                RoomPolicyEdit::HistoryPolicy(history) => {
                    policy.history_policy = (*history).into();
                }
                RoomPolicyEdit::AppointAdmin(actor) => {
                    let mut admins = policy.admins.clone();
                    admins.push(*actor);
                    policy.set_admins(admins);
                }
                RoomPolicyEdit::DemoteAdmin(actor) => {
                    let admins: Vec<ActorId> = policy
                        .admins
                        .iter()
                        .copied()
                        .filter(|a| a != actor)
                        .collect();
                    policy.set_admins(admins);
                }
                RoomPolicyEdit::TransferOwnership(_) => {
                    return Err(MlsError::PolicyViolation(
                        "an ownership transfer is an offer, never a direct policy commit".into(),
                    ));
                }
            }
            let signed = engine.sign_room_policy(&policy)?;
            Ok(RoomPolicyExtension {
                signed,
                successions: current.successions.clone(),
            })
        });
        let commit_seq = self.commit_room_policy_locked(&channel_id, rebuild).await?;
        // The roles a policy commit rewrites are part of the roster this
        // device reports (`conversation-rooms.md` § The floor roster) —
        // report, never guess.
        self.report_roster(&channel_id, Some(commit_seq)).await;
        Ok(())
    }

    async fn found_room(
        &self,
        thread_id: ThreadId,
        name: Option<String>,
        invitees: &[TypedAddress],
    ) -> Result<(), BackendError> {
        // Every invitee resolved to an actor BEFORE the ceremony: a room
        // founded for a recipient it then cannot name is a room with nobody
        // else in it, which is worse than a refusal the user can act on.
        let invitees = invitees
            .iter()
            .map(|addr| {
                let actor = fauna_actor(addr)?;
                let node = match addr {
                    TypedAddress::Fauna { handle, .. } => self.peer_domain_for(handle),
                    _ => None,
                };
                Ok((actor, node))
            })
            .collect::<Result<Vec<_>, BackendError>>()?;
        let channel_id = self.found_community_room(thread_id, name).await?;
        self.refresh_floor_soon(&channel_id);
        for (actor, node) in invitees {
            if actor == self.self_actor {
                continue;
            }
            self.invite_to_room(
                &channel_id,
                actor,
                fauna_mls::room_policy::RoomRole::Member,
                node,
            )
            .await?;
        }
        Ok(())
    }

    async fn room_invitations(&self) -> Result<Vec<crate::room::RoomInvitation>, BackendError> {
        let mut standing = Vec::new();
        for invitation in self.pending_room_invitations().await? {
            // An invitation into a room this device holds a LIVE seat in is
            // spent — the settle after an accept did not land. Settle it now
            // rather than offer the user a room they are already in.
            //
            // Bound-but-unseated is NOT spent (): a removed member
            // keeps the thread binding (the device keeps its bubbles), so
            // "has a thread" alone cannot tell a genuinely spent invitation
            // from a legitimate re-invitation after a removal. Only a
            // CONFIRMED unseating (`Self::confirmed_unseated`, set by
            // `Self::tend_community_room` on a `NoFloor` read) overrides the
            // thread-bound presumption; a channel this session has never
            // tended presumes seated, same as today.
            let channel_id = ChannelId(invitation.room_id);
            if self.thread_for_channel(&channel_id).is_some()
                && !self.confirmed_unseated(&channel_id)
            {
                if let Err(e) = self.settle_room_invitation(invitation.id).await {
                    tracing::debug!("settling a spent room invitation failed (retried): {e}");
                }
                continue;
            }
            standing.push(invitation);
        }
        Ok(standing)
    }

    async fn accept_room_invitation(
        &self,
        thread_id: ThreadId,
        invitation: &crate::room::RoomInvitation,
    ) -> Result<(), BackendError> {
        let channel_id = self
            .accept_room_invite(thread_id, invitation.room_id, invitation.room_node.clone())
            .await?;
        self.refresh_floor_soon(&channel_id);
        // After the accept, never before: a settle that raced ahead of a failed
        // accept would lose the only record of the invitation.
        if let Err(e) = self.settle_room_invitation(invitation.id).await {
            tracing::warn!(
                "the accepted room invitation was not settled; the next listing settles it: {e}"
            );
        }
        Ok(())
    }

    async fn decline_room_invitation(
        &self,
        invitation: &crate::room::RoomInvitation,
    ) -> Result<(), BackendError> {
        self.settle_room_invitation(invitation.id).await
    }

    async fn withdraw_room_invite(
        &self,
        thread_id: ThreadId,
        invitee: ActorId,
    ) -> Result<(), BackendError> {
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot withdraw an invitation on an unbound thread".to_string())
        })?;
        self.revoke_room_invite(&channel_id, invitee).await
    }

    async fn leave_room(&self, thread: &ThreadDetail) -> Result<(), BackendError> {
        use crate::room::RoomClass;
        let channel_id = self
            .channel_for(&thread.thread_id)
            .ok_or_else(|| BackendError::Internal("cannot leave an unbound thread".to_string()))?;
        // The class picks the door (`conversation-rooms.md` § Roles and
        // authorization → *Leaving — the mechanism*). Matched exhaustively
        // rather than defaulted: a transport-only room is somebody else's
        // membership to model, and a room this device cannot classify is one
        // it should not guess a door for.
        match self.room_state(thread).map(|room| room.class) {
            Some(RoomClass::Community) => self.leave_room_by_ceremony(&channel_id).await,
            Some(RoomClass::EndToEnd) => self.leave_end_to_end_room(&channel_id).await,
            Some(RoomClass::TransportOnly) | None => Err(BackendError::NotSupported),
        }
    }

    async fn set_room_nest_read(
        &self,
        thread_id: ThreadId,
        reads: bool,
    ) -> Result<(), BackendError> {
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot change the nest read of an unbound thread".to_string())
        })?;
        self.rotate_room_nest_read(&channel_id, reads).await
    }

    async fn set_room_labelers(
        &self,
        thread_id: ThreadId,
        labelers: Vec<String>,
    ) -> Result<(), BackendError> {
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot name the labelers of an unbound thread".to_string())
        })?;
        let ids = labelers
            .iter()
            .map(|hex| {
                ActorId::from_hex(hex)
                    .map_err(|_| BackendError::Internal(format!("not a labeler id: {hex:?}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.set_room_labelers(&channel_id, ids).await.map(|_| ())
    }

    /// Post this device's history slice to the channel for a newcomer, under
    /// the room's `full` history rule (`conversation-rooms.md` § History for
    /// joiners): a `GroupMetaMessage::HistorySlice` application message in
    /// the epoch the newcomer just joined, trimmed oldest-first to fit one
    /// channel record. A member refuses to produce a slice the policy does
    /// not authorize — the check is here, not only at the caller.
    async fn deliver_history_slice(
        &self,
        thread_id: ThreadId,
        slice: &crate::store::history::ChannelHistorySlice,
    ) -> Result<(), BackendError> {
        let channel_id = self.channel_for(&thread_id).ok_or_else(|| {
            BackendError::Internal("cannot deliver history on an unbound thread".to_string())
        })?;
        let policy = self.governed_policy(&channel_id)?;
        if policy.signed.policy.history_policy != fauna_mls::room_policy::HistoryPolicy::Full {
            return Err(BackendError::Internal(
                "the room's history policy does not authorize a history slice".to_string(),
            ));
        }
        let mut slice = slice.clone();
        // A receiver refuses a slice WHOLE if any carried id is not
        // `conv:{this channel}:{seq}` ([`accept_joiner_slice`]). Every message
        // this rail appends is minted in that shape, so this drops nothing
        // today; it is here so an honest inviter can never author a slice the
        // newcomer throws away over one stray entry.
        let own_prefix = format!("conv:{channel_id}:");
        slice.messages.retain(|m| {
            m.message_id
                .0
                .strip_prefix(&own_prefix)
                .is_some_and(|tail| tail.parse::<i64>().is_ok())
        });
        let carried: std::collections::HashSet<_> = slice
            .messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect();
        slice.deleted_messages.retain(|id| carried.contains(id));
        slice.reaction_log.retain(|id, _| carried.contains(id));
        // The attachments' fetch coordinates are for the user's own devices: a
        // newcomer holds no key for the epochs they name, so they would only
        // eat the per-record budget below (`conversation-rooms.md` § History
        // for joiners).
        slice.attachment_coordinates.clear();
        let mut bytes = slice.to_bytes().map_err(BackendError::Internal)?;
        // Trim oldest-first until the record fits the channel's per-record
        // budget; an empty slice still carries the label and roster.
        while bytes.len() > MAX_HISTORY_SLICE_BYTES && !slice.messages.is_empty() {
            let dropped = slice.messages.remove(0);
            // ⚠ The derived state is keyed by message id, so it leaves with the
            // message it describes. Two reasons, and the first is why the loop
            // is still correct: an entry that outlives its message keeps
            // occupying the budget this loop is trying to free, so a slice
            // heavy with reactions could run `messages` empty and still post
            // over the ceiling. The second is that it would hand the newcomer
            // tombstones and per-actor reaction attribution for exactly the
            // history this loop just decided not to give them.
            slice.deleted_messages.remove(&dropped.message_id);
            slice.reaction_log.remove(&dropped.message_id);
            bytes = slice.to_bytes().map_err(BackendError::Internal)?;
        }
        self.post_app_message(
            &channel_id,
            ChannelMessageBody::GroupMeta(GroupMetaMessage::HistorySlice(bytes)),
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    /// Post a reaction to an existing message on the thread's MLS channel.
    /// Seals a `ChannelMessageBody::Reaction` application message (no Commit,
    /// no epoch change) — symmetric to `rename`. The thread must already be
    /// bound to a channel; returns `Other` if not (callers should guard on
    /// `capabilities.supports_reactions`).
    async fn send_reaction(
        &self,
        thread: &ThreadId,
        target_seq: u64,
        emoji: &str,
        op: ReactionOp,
    ) -> Result<(), BackendError> {
        let channel_id = self.channel_for(thread).ok_or_else(|| {
            BackendError::Internal("cannot react on an unbound thread".to_string())
        })?;
        self.post_side_effect(
            &channel_id,
            ChannelMessageBody::Reaction {
                target_seq,
                emoji: emoji.to_string(),
                op,
            },
        )
        .await
    }

    /// Post a delete marker for an existing message on the thread's channel.
    /// Seals a `ChannelMessageBody::Delete` — an MLS application message (no
    /// Commit, no epoch change), or in a community room a body on the room's
    /// sealed path ([`Self::post_side_effect`]). The thread must already be
    /// bound to a channel; returns `Other` if not.
    async fn send_delete(&self, thread: &ThreadId, target_seq: u64) -> Result<(), BackendError> {
        let channel_id = self.channel_for(thread).ok_or_else(|| {
            BackendError::Internal("cannot delete on an unbound thread".to_string())
        })?;
        self.post_side_effect(&channel_id, ChannelMessageBody::Delete { target_seq })
            .await
    }

    /// An owner's or admin's delete of another member's message. An end-to-end
    /// room seals the ordinary `Delete` (the engine's authenticated sender role
    /// is what admits it); a community room — no MLS group — files a signed,
    /// **unsealed floor delete record** under the policy version the floor
    /// holds, which the home nest judges without reading anything
    /// (`conversation-rooms.md` § Roles and authorization → *Delete any message
    /// — the mechanism* → *Community rooms*).
    ///
    /// The version is read fresh rather than taken from the render cache: the
    /// floor refuses a record naming any version but the one it holds, so a
    /// poll-old version would turn a policy change into a delete that bounces.
    async fn send_delete_any(
        &self,
        thread: &ThreadId,
        target_seq: u64,
    ) -> Result<(), BackendError> {
        let channel_id = self.channel_for(thread).ok_or_else(|| {
            BackendError::Internal("cannot delete on an unbound thread".to_string())
        })?;
        if self.engine.has_group(&channel_id) {
            return self
                .post_side_effect(&channel_id, ChannelMessageBody::Delete { target_seq })
                .await;
        }
        let reader = self.room_roster_reader.get().ok_or_else(|| {
            BackendError::Internal("no floor-roster reader is registered".to_string())
        })?;
        let policy_version = reader
            .read_roster(channel_id.to_string(), self.channel_home_url(&channel_id))
            .await
            .or_absent()
            .and_then(|floor| floor.policy_version)
            .ok_or_else(|| {
                BackendError::Internal(
                    "the room's policy version could not be read — no floor delete was filed"
                        .to_string(),
                )
            })?;
        let record = self
            .engine
            .sign_room_floor_delete(&channel_id.0, target_seq, policy_version)
            .and_then(|signed| signed.to_bytes())
            .map_err(|e| BackendError::Internal(format!("sign floor delete record: {e}")))?;
        let envelope = ChannelEnvelope::RoomFloorDelete(record)
            .to_bytes()
            .map_err(|e| BackendError::Internal(format!("encode floor delete envelope: {e}")))?;
        self.send_on_channel(&channel_id, envelope, None, Vec::new())
            .await?;
        Ok(())
    }

    /// Expose the thread's bound channel as hex so the manager can re-key a
    /// sender-bootstrapped thread to channel-keyed (see the trait default).
    fn channel_binding_hex(&self, thread_id: &ThreadId) -> Option<String> {
        self.channel_for(thread_id).map(|c| c.to_string())
    }

    /// Rule 3 (durable-before-done): route the manager's awaited post-mutation
    /// flush to the injected [`HistoryPersist`] seam. A thread with no bound
    /// channel yet (first send still bootstrapping) or no seam injected
    /// (single-device / no multi-device plane) is a no-op `Ok`.
    async fn persist_history(&self, thread: &ThreadId) -> Result<(), BackendError> {
        let (Some(channel), Some(persist)) = (self.channel_for(thread), self.history_persist.get())
        else {
            return Ok(());
        };
        persist.persist_channel(channel).await
    }

    /// Top up the local actor's key-package queue on the nest to `target`
    /// packages so peers can fetch one to add us to a group. Counts the
    /// remaining packages (`keypackage_count` for our own actor); if below
    /// `target`, generates the shortfall with the engine and uploads them.
    /// Idempotent — a no-op at or above `target`. Returns the number uploaded.
    ///
    /// The key-package *generation* needs the `MlsEngine` (which the manager
    /// stays free of, per `docs/goal/ui/conversations.md` § Architectural rules
    /// #2 — all MLS crypto in shared Rust); the backend owns both `engine` and
    /// `rpc`, and only opaque package bytes cross the seam. Clients reach it via
    /// the thin manager wrapper
    /// [`ConversationsManager::ensure_keypackages`](crate::ConversationsManager::ensure_keypackages),
    /// which routes through `dyn RailBackend` to here.
    async fn ensure_keypackages(&self, target: u64) -> Result<u64, BackendError> {
        let self_hex = hex::encode(self.self_actor.0);
        let have = self.rpc.keypackage_count(self_hex).await?;
        if have >= target {
            return Ok(0);
        }
        let shortfall = (target - have) as usize;
        let packages = self
            .engine
            .generate_key_packages_bytes(shortfall)
            .map_err(|e| BackendError::Internal(e.to_string()))?;
        // Rule 3 (durable-before-done / save-before-publish, `devices.md`
        // § Durability rules): the mint just wrote fresh private init keys into
        // the engine's `provider` storage. They are user-unreconstructable, so
        // they must be durable in the replica BEFORE the public package is
        // fetchable — else a provider swap (launch restore / `resync_provider` /
        // relaunch) wipes the init key while a peer already holds the package.
        // Await the durable flush and fail loudly rather than publish a doomed
        // package (the launch-window strand the debounced autosave could not
        // close).
        self.persist_provider_before_publish("key packages").await?;
        self.rpc
            .keypackage_upload(packages, false)
            .await
            .map_err(BackendError::from)
    }

    /// Publish the actor's mandatory **last-resort** key package (Spec Y2 —
    /// `docs/goal/architecture/federation.md` § Key packages — privacy &
    /// exhaustion): every actor publishes one reusable last-resort KP at
    /// onboarding so it stays reachable (`addressable`) after its one-time pool
    /// drains. The engine mints one with the MLS `last_resort` extension; the
    /// upload carries `last_resort = true`.
    ///
    /// **Idempotent on every login.** The nest keeps a *single* last-resort row
    /// per actor — `keypackage.upload { last_resort: true }` replaces any prior
    /// one (`bins/fauna-nest/src/db/channels.rs::put_last_resort_key_package`
    /// deletes the actor's existing last-resort rows before inserting). So this
    /// uploads unconditionally each call without accumulating duplicates; no
    /// count-check is needed (the one-time `count` gauge deliberately excludes
    /// last-resort KPs, so there is nothing to gate on here). Clients reach it
    /// via [`ConversationsManager::ensure_last_resort_keypackage`](crate::ConversationsManager::ensure_last_resort_keypackage).
    async fn ensure_last_resort_keypackage(&self) -> Result<(), BackendError> {
        let package = self
            .engine
            .generate_last_resort_key_package_bytes()
            .map_err(|e| BackendError::Internal(e.to_string()))?;
        // Rule 3 save-before-publish (see [`Self::ensure_keypackages`]): the
        // last-resort mint likewise writes a fresh private init key into the
        // provider, so its durability must precede publication.
        self.persist_provider_before_publish("the last-resort key package")
            .await?;
        self.rpc.keypackage_upload(vec![package], true).await?;
        Ok(())
    }
}

// ── Inbound conversation receive driver ───────────────────────────────

/// Per-page fetch size the poll fns request when the caller passes a
/// non-positive `page_limit` ("drain everything"): matches the nest's
/// serve-page ceiling (`SERVE_PAGE_MAX_RECORDS`,
/// `bins/fauna-nest/src/segments/mod.rs`). **Never send a non-positive
/// `limit` on the wire** — an already-deployed nest clamps
/// `limit.clamp(1, 500)`, which converts 0 into a ONE-record page (the Bug-A
/// starvation). The poll fns page
/// until an EMPTY page regardless of this value, so it only tunes round
/// trips, never completeness.
const DEFAULT_FETCH_PAGE_LIMIT: i64 = 500;

/// Resolve a caller `page_limit` to the wire fetch size (see
/// [`DEFAULT_FETCH_PAGE_LIMIT`]).
fn wire_fetch_limit(page_limit: i64) -> i64 {
    if page_limit > 0 {
        page_limit
    } else {
        DEFAULT_FETCH_PAGE_LIMIT
    }
}

/// Re-drive the statements the witness refused for `old_actor` while no anchor
/// was held — the convergence half of the peer-profile harvest
/// (`identity-succession.md` § The succession statement → *the peer-profile
/// harvest*). A consumed MLS application message cannot be re-decrypted
/// (forward secrecy), so the one delivery [`poll_inbound_conv`] parked is the
/// only copy this session will ever hold; when the harvest seeds the peer's
/// anchor on its own read-path schedule, this settles that copy through the
/// **ordinary** witness verify — tier 1 from the held head, no dial, and never
/// a profile fetch (rule 4's corollary: a statement's arrival must not
/// schedule, hasten or prioritize a harvest, and a re-drive fetches nothing).
///
/// Call it after a harvest for `old_actor` reports it seeded something new.
/// Statements the witness still refuses stay parked for the next harvest event
/// and die with the session. Returns how many rows were re-pointed.
///
/// A seed is also a **settle** — the peer's profile was read this session — so
/// this releases the harvest wait as well ([`settle_parked_successions`] is the
/// twin for a settle that seeded nothing).
pub async fn redrive_parked_successions(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    old_actor: &ActorId,
) -> u32 {
    redrive_after_harvest(backend, manager, old_actor, true).await
}

/// Re-drive the statements parked for `old_actor` after the harvest sweep
/// **settled that peer without seeding anything** — nothing new, a refusal, or
/// a spent retry budget (`identity-succession.md` § The succession statement →
/// *the harvest wait*). A statement the witness held back because this
/// session's harvest of the peer was still owed is released by exactly this
/// event: the un-demoted head it rests on settles it at tier 1, with no dial.
///
/// It announces the settle and **never a seed**: none landed, and the
/// witness's anchor-store read is bounded per seed generation — an
/// announcement per settled peer would turn that into a read per roster row.
pub async fn settle_parked_successions(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    old_actor: &ActorId,
) -> u32 {
    redrive_after_harvest(backend, manager, old_actor, false).await
}

/// What this group's own roster says about a **verified** succession — the
/// roster-pair gate (`succession-propagation.md` § Propagation → *MLS groups
/// (per group)*).
///
/// Verifying a statement proves the succession *happened*; it never proves it
/// happened **here**. A true statement is public — it rides the chain — and
/// this arm's own contract says the transport sender is not the authority, so
/// any member may carry one into any channel. Re-pointing on the signature
/// alone therefore re-points wherever the bytes are replayed, and the group
/// that never ran the ceremony is exactly where that is harmful: a seed thief
/// still holding the old leaf there replays the statement, and every member
/// renders the rightful successor while the thief's leaf stays live. That is
/// the "ceremony did not finish" condition — the succeeded credential still
/// has reach (`critical-alerts.md` § Feeders) — painted as continuity, the one
/// rendering that hides it.
///
/// The authority is therefore the **pair**, which the doc calls one logical
/// operation: the successor seated here and the predecessor's leaf gone. The
/// roster is read from the **MLS engine**, never from
/// `ThreadDetail::participants` — § Propagation → *Removing a flagged member*
/// rule (1): the snapshot is a local view written at join and by the owner's
/// own gestures that no inbound Commit reconciles, and a foreign-authored
/// membership Commit is precisely the adversarial case rather than a corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RosterPair {
    /// Both halves landed here: re-point the row.
    Complete,
    /// The successor is seated and the predecessor is still there — the honest
    /// ceremony's own midpoint, because the statement rides **alongside the
    /// add** (`fauna_mls::succession`; `fauna_client_recovery::group_sweep`
    /// posts it between the two commits) and the remove-old commit lands after
    /// it. Hold the statement for that commit.
    AwaitingRemoveOld,
    /// The successor holds no leaf here: this group never ran the ceremony, so
    /// the statement was replayed rather than carried. Drop it — holding it
    /// would mean keeping every public statement any member cares to forward,
    /// against a future that has its own carrier: when the ceremony does reach
    /// this group, the sweep posts the statement again beside its own add.
    NotHere,
}

fn roster_pair(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    old: &ActorId,
    new: &ActorId,
) -> RosterPair {
    let roster = backend.engine.group_members(channel_id);
    match (roster.contains(new), roster.contains(old)) {
        (true, false) => RosterPair::Complete,
        (true, true) => RosterPair::AwaitingRemoveOld,
        (false, _) => RosterPair::NotHere,
    }
}

/// Put a verified statement through [`roster_pair`] and act on the verdict —
/// the **one** place a verified statement is allowed to become a re-point, so
/// both arrival paths (the poll's own arm and either re-drive) share one gate
/// rather than growing two.
///
/// Returns the verdict so the caller can park, re-park or drop; the re-point
/// and its tally happen here.
fn settle_verified_succession(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    thread_id: &ThreadId,
    channel_id: &ChannelId,
    verified: &fauna_core::recovery::VerifiedSuccession,
) -> RosterPair {
    use std::sync::atomic::Ordering::Relaxed;
    let tally = &backend.succession_statements;
    let verdict = roster_pair(
        backend,
        channel_id,
        &verified.old_actor_id,
        &verified.new_actor_id,
    );
    match verdict {
        RosterPair::Complete => {
            manager.apply_inbound_succession(
                thread_id.clone(),
                &verified.old_actor_id,
                verified.new_actor_id,
            );
            tally.repointed.fetch_add(1, Relaxed);
        }
        RosterPair::AwaitingRemoveOld => {
            tally.awaiting_remove_old.fetch_add(1, Relaxed);
        }
        RosterPair::NotHere => {
            tally.not_in_this_group.fetch_add(1, Relaxed);
        }
    }
    verdict
}

/// Re-drive the statements `thread_id` is holding at the roster-pair gate, now
/// that an inbound commit moved this group's roster — the convergence half of
/// [`RosterPair::AwaitingRemoveOld`], and the reason holding is not dropping.
///
/// Driven from [`poll_inbound_conv`]'s `Advanced` arm, so the honest ceremony
/// settles inside the same walk that delivers it: the add commit, the
/// statement (held here), then the remove-old commit, whose fold-in brings us
/// straight back through this function with the pair complete.
///
/// Re-verifies through the witness rather than trusting the parked bytes: a
/// park is a delay, never a promotion, and the verify is tier 1 off a head
/// this session already holds (no dial, no fetch — the re-drive contract).
async fn redrive_parked_in_thread(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    thread_id: &ThreadId,
    channel_id: &ChannelId,
) {
    let parked = backend.take_parked_in_thread(thread_id);
    if parked.is_empty() {
        return;
    }
    let Some(witness) = backend.succession_witness() else {
        // The witness went away under us; put them back untouched rather than
        // let a roster move consume statements nothing can verify.
        for statement in parked {
            backend.repark_succession_if_empty(thread_id.clone(), statement);
        }
        return;
    };
    for statement in parked {
        // A local pre-filter ahead of the witness, off the statement's own
        // CLAIMED pair: this commit moved one group's roster, and most parked
        // statements are waiting on something else entirely. Reading unverified
        // bytes is sound here precisely because the filter can only ever skip
        // work — the re-point still goes through `settle_verified_succession`,
        // which gates on the pair the witness VERIFIED — so a forged claim buys
        // its author nothing but a verify this function would have run anyway.
        if roster_pair(
            backend,
            channel_id,
            &statement.statement.old_actor_id,
            &statement.statement.new_actor_id,
        ) != RosterPair::Complete
        {
            backend.repark_succession_if_empty(thread_id.clone(), statement);
            continue;
        }
        let verdict = match witness.verify(statement.clone()).await {
            Some(verified) => {
                settle_verified_succession(backend, manager, thread_id, channel_id, &verified)
            }
            // Refused now (the anchor was demoted, or never landed): that is
            // the harvest re-drive's wait, not ours. Keep holding it.
            None => RosterPair::AwaitingRemoveOld,
        };
        if verdict != RosterPair::Complete {
            backend.repark_succession_if_empty(thread_id.clone(), statement);
        }
    }
}

async fn redrive_after_harvest(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    old_actor: &ActorId,
    seed_landed: bool,
) -> u32 {
    // Settled WITHOUT an anchor: nothing more will be learned about this
    // identity before the session ends, so a floor delete record still parked
    // on its name is one the room says so about
    // (`FaunaMlsBackend::moderation_unverified`). A seed is the opposite — an
    // anchor, which the record's next judgment uses — so it records nothing,
    // and the room is never announced as unverifiable between the seed and
    // the pass that paints. Recorded before anything else (a witness-less
    // backend parks nothing, so the record costs it nothing); the rooms it
    // turns are repainted below, after the re-drive.
    let unverified_before = backend.moderation_unverified_rooms();
    {
        let mut settled = backend.harvest_settled.lock().unwrap();
        if seed_landed {
            settled.remove(old_actor);
        } else {
            settled.insert(*old_actor);
        }
    }
    // Either arm is the sweep SPEAKING for this identity: the folder commit
    // walk's hold ends here, whatever the re-drive below decides about the
    // statement (`FaunaMlsBackend::folder_walk_waits_on`). Recorded before the
    // witness is consulted, like the two announcements, and unconditionally —
    // the ordinary case is nothing parked, and the walk that stalls a minute
    // later must find the wait over.
    backend
        .harvest_spoken_for
        .lock()
        .unwrap()
        .insert(*old_actor);
    let Some(witness) = backend.succession_witness() else {
        return 0;
    };
    // Tell the witness the seed landed BEFORE asking it anything, and
    // unconditionally — the caller reaches here on every `Seeded` outcome, so
    // this is also the signal for a seed that arrives with nothing parked (the
    // ordinary case: the harvest sweep usually wins the race). A witness that
    // caches "no anchor held" has no other way to learn the store changed under
    // it — see `SuccessionWitness::anchor_seed_landed`.
    if seed_landed {
        witness.anchor_seed_landed().await;
    }
    // Then the settle, likewise before the parked set and likewise whether or
    // not anything is parked: the ordinary case is the sweep winning the race,
    // and the statement that arrives a minute later must find the wait over.
    // After the seed announcement, never before — a witness released ahead of
    // the re-read would settle on the head the seed just demoted.
    witness.harvest_settled(old_actor).await;
    let mut repointed = 0u32;
    for (thread_id, statement) in backend.take_parked_for(old_actor) {
        // The anchor arriving is only half the question: the statement still
        // has to have happened in THIS group ([`roster_pair`]). A row parked
        // here was refused by the witness, never gated, so the gate runs for
        // the first time now.
        let Some(channel_id) = backend.channel_for(&thread_id) else {
            // The thread unbound under us (a restore in flight): the gate
            // cannot be read, so nothing is decided and nothing is dropped.
            backend.repark_succession_if_empty(thread_id, statement);
            continue;
        };
        let verdict = match witness.verify(statement.clone()).await {
            Some(verified) => {
                settle_verified_succession(backend, manager, &thread_id, &channel_id, &verified)
            }
            None => RosterPair::AwaitingRemoveOld,
        };
        match verdict {
            RosterPair::Complete => repointed += 1,
            // Still waiting — on the anchor, or on this group's remove-old
            // commit. Either way the commit re-drive picks it up.
            RosterPair::AwaitingRemoveOld => {
                backend.repark_succession_if_empty(thread_id, statement)
            }
            RosterPair::NotHere => {}
        }
    }
    // The folder rail's park store, along the same axis: a statement a folder
    // channel carried while the witness held no anchor for this identity. A
    // statement still resting from an earlier launch is drained first — the
    // sweep may speak before this launch's first walk over its channel, and
    // the rested copy is forgotten below.
    for (channel_id, _) in backend.engine.parked_folder_successions() {
        backend.load_rested_folder_park(&channel_id);
    }
    for (channel_id, statement) in backend.take_parked_folder_for(old_actor) {
        match witness.verify(statement.clone()).await {
            Some(verified) => {
                if settle_folder_owner_succession(backend, &channel_id, &verified) {
                    repointed += 1;
                }
            }
            None => backend.repark_folder_succession_if_empty(channel_id, statement),
        }
    }
    // Spoken for, whatever was decided: no later launch holds on this owner
    // again (`federation.md`, the at-rest folder park — a forgery costs at
    // most one hold window per launch). The RAM re-park above stays for a
    // read-path seed this session and writes nothing at rest.
    backend.forget_rested_folder_parks_naming(old_actor);
    // The settle may have turned a room's parked moderation from *not yet*
    // into *unverified* — a fact no ingest carries, so say so; and only then.
    if backend.moderation_unverified_rooms() != unverified_before {
        manager.notify_room_state_changed();
    }
    repointed
}

/// The folder rail's arm for an in-group succession statement
/// (`GroupMetaMessage::Succession`) — the member side of *The marker follows
/// the owner's verified succession* (`federation.md` § Cross-nest shared
/// folders + channel append). Returns whether `cm` was
/// such a statement (every other body is not this arm's business).
///
/// The statement goes through the **same session-wired witness** and the
/// **same roster-pair door** the conversations rail uses
/// ([`settle_folder_owner_succession`]): verified, `old_actor_id` this
/// channel's recorded owner, successor seated here ⇒ the marker re-points to
/// the successor **at once, at the ceremony's midpoint** — the remove-old
/// commit that follows is authored by the successor's leaf, and it is this
/// re-stamp that admits it through the folder commit policy
/// (`MlsEngine::process_commit`). A statement the witness refuses for now is
/// **parked by channel** for the harvest re-drive
/// ([`redrive_parked_successions`]) and the channel's next commit
/// ([`redrive_parked_folder_in_channel`] — re-asked BEFORE that commit, which
/// the walk then holds while the sweep has yet to speak for the owner, so the
/// successor's remove-old is never refused and memoized against a marker the
/// re-drive would have moved) — admitted only where its `old_actor_id` is
/// this channel's recorded owner, the one marker a re-drive could re-point.
/// No witness, an undecodable body, and a refused statement naming anyone
/// else all degrade to nothing, tallied in [`SuccessionStatementCounts`] like
/// the conversations arm's.
async fn route_folder_succession(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    cm: &ChannelMessage,
) -> bool {
    let ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(bytes)) = &cm.body else {
        return false;
    };
    use std::sync::atomic::Ordering::Relaxed;
    let tally = &backend.succession_statements;
    tally.seen.fetch_add(1, Relaxed);
    let Some(witness) = backend.succession_witness() else {
        tally.no_witness.fetch_add(1, Relaxed);
        return true;
    };
    let Ok(statement) = fauna_core::encoding::canonical_decode::<
        fauna_core::recovery::SignedIdentitySuccession,
    >(bytes) else {
        tally.undecodable.fetch_add(1, Relaxed);
        return true;
    };
    match witness.verify(statement.clone()).await {
        Some(verified) => {
            settle_folder_owner_succession(backend, channel_id, &verified);
        }
        // Refused for now — and this delivery is the only copy this seat will
        // ever hold, so park it where a re-drive can still re-point something:
        // the channel whose recorded owner the statement names.
        None => {
            if backend.engine.folder_channel_owner(channel_id)
                == Some(statement.statement.old_actor_id)
            {
                backend.park_folder_succession(*channel_id, statement);
            }
        }
    }
    true
}

/// Put a verified statement through [`roster_pair`] on a FOLDER channel and
/// re-stamp the durable owner marker — the **one** place a verified statement
/// becomes a marker move, shared by the poll's own arm and both re-drives.
/// Returns whether the marker moved.
///
/// The re-stamp fires on [`RosterPair::AwaitingRemoveOld`] as well as on
/// [`RosterPair::Complete`], and that timing is load-bearing in both
/// directions (`federation.md`, the ruling): the honest ceremony arrives at
/// the midpoint — the statement rides beside the add, so the predecessor is
/// still seated — and the remove-old that completes it is the successor's
/// commit, which the policy admits only once the marker names the successor;
/// waiting for completion would wait for ever. Not on verification alone
/// either: a true statement is public and any member may carry one ahead of
/// the sweep, and a marker moved before the add-successor landed would refuse
/// that add — the old leaf's one constructive act — and strand the channel;
/// so a successor with no leaf here ([`RosterPair::NotHere`]) moves nothing,
/// and the sweep posts the statement again beside its own add. A verified
/// statement whose `old_actor_id` is **not** this channel's recorded owner
/// re-points nothing either: it is some member's succession, and the roster
/// itself carries that.
fn settle_folder_owner_succession(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    verified: &fauna_core::recovery::VerifiedSuccession,
) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let tally = &backend.succession_statements;
    match roster_pair(
        backend,
        channel_id,
        &verified.old_actor_id,
        &verified.new_actor_id,
    ) {
        RosterPair::NotHere => {
            tally.not_in_this_group.fetch_add(1, Relaxed);
            false
        }
        RosterPair::AwaitingRemoveOld | RosterPair::Complete => {
            if backend.engine.folder_channel_owner(channel_id) != Some(verified.old_actor_id) {
                return false;
            }
            backend
                .engine
                .mark_folder_channel_owner(channel_id, &verified.new_actor_id);
            tally.repointed.fetch_add(1, Relaxed);
            tracing::info!(
                channel = %channel_id,
                "folder-owner marker re-pointed to the verified successor"
            );
            true
        }
    }
}

/// Whether a rested folder-park value names `old_actor` — an undecodable one
/// counts as naming everyone, since it can hold nothing and should go.
fn rested_statement_names(bytes: &[u8], old_actor: &ActorId) -> bool {
    fauna_core::encoding::canonical_decode::<fauna_core::recovery::SignedIdentitySuccession>(bytes)
        .map_or(true, |s| s.statement.old_actor_id == *old_actor)
}

/// The folder rail's [`redrive_parked_in_thread`], run at **both** ends of a
/// commit: before it — the hold's own re-ask, so a witness that has gained an
/// anchor since the park releases the walk on the spot — and after one folds
/// in (a session with no sweep never holds, and the roster move is then the
/// re-drive's trigger, as on the conversations rail). A park is a delay, never
/// a promotion — the re-point still goes through the witness and
/// [`settle_folder_owner_succession`].
///
/// Returns whether the walk must **hold** the next commit on this channel: a
/// statement is still parked here, it still names the channel's recorded
/// owner, and the sweep has not yet spoken for that owner
/// ([`FaunaMlsBackend::folder_walk_waits_on`]). A parked statement whose
/// `old_actor_id` is no longer the recorded owner is dropped — some other
/// route moved the marker, and there is nothing left for it to re-point.
async fn redrive_parked_folder_in_channel(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
) -> bool {
    let parked = backend.take_parked_folder_in_channel(channel_id);
    if parked.is_empty() {
        return false;
    }
    let recorded_owner = backend.engine.folder_channel_owner(channel_id);
    let Some(witness) = backend.succession_witness() else {
        // The witness went away under us: keep the statements, but nothing can
        // verify them, so nothing may wait on them either.
        for statement in parked {
            backend.repark_folder_succession_if_empty(*channel_id, statement);
        }
        return false;
    };
    let mut hold = false;
    for statement in parked {
        let old = statement.statement.old_actor_id;
        if recorded_owner != Some(old) {
            // Dropped here, so dropped at rest too: the next launch must not
            // hold on a statement with nothing left to re-point.
            backend.forget_rested_folder_park_naming(channel_id, &old);
            continue;
        }
        match witness.verify(statement.clone()).await {
            Some(verified) => {
                settle_folder_owner_succession(backend, channel_id, &verified);
                backend.forget_rested_folder_park_naming(channel_id, &old);
            }
            // Refused still: that is the harvest's wait, not ours — and while
            // the sweep has not spoken for this owner, the commit behind the
            // statement waits with it.
            None => {
                hold |= backend.folder_walk_waits_on(&old);
                backend.repark_folder_succession_if_empty(*channel_id, statement);
            }
        }
    }
    hold
}

/// Drive the fauna-native MLS inbound feed for one channel: page the channel log
/// via `channel_fetch`, decode each `ChannelEnvelope`, MLS-decrypt `Application`
/// payloads (and `process_commit` membership `Commit`s), and ingest the
/// resulting messages into `manager`'s thread bound to this channel. The receive
/// counterpart to [`FaunaMlsBackend::send`], living in shared Rust so every
/// app reuses the same decode + decrypt + ingest path
/// (`docs/goal/ui/conversations.md` § Receiving into the conversations view).
///
/// `after_seq` is the paging cursor: advanced to the highest server sequence
/// seen, so the next call only fetches newer entries (the channel log is
/// monotonic, so the cursor alone dedups — no `seen` set needed). Routing is by
/// the backend's thread↔channel binding (populated by group bootstrap on the
/// sender and by welcome-ingest on the receiver, Track C); a channel with no
/// bound thread yields an empty outcome (the welcome that materializes the
/// thread hasn't been processed yet). Records that fail to decode or decrypt are
/// skipped, not fatal — one bad record can't stall the feed, and an actor cannot
/// MLS-decrypt its own application messages (already appended locally by `send`).
///
/// **The cursor never advances past an unincorporated commit** (a
/// [`CommitApplyOutcome::Stalled`] record): the walk stops there and reports
/// [`ConvPollOutcome::stalled`]. This is what makes the seq this poll leaves
/// behind safe to use as a gated-send `expect_no_commit_since` baseline —
/// advancing past a stalled commit would let a rebased commit be accepted for an
/// epoch the other members already left, a permanent fork (`devices.md` §
/// Cross-device MLS group-state sync, Rules 1–2 — "round toward the safe side").
/// The chat rail's one heal runs first: a *future-epoch* stall means this device
/// skipped the bridging commit, so the cursor rewinds to 0 and re-walks within
/// this same call (once per channel per session — a rewind cannot fill an
/// un-processable hole, and an own-leaf stall is never healable by one, since MLS
/// can never process an own commit). Only when that heal is unavailable or
/// already spent does the walk stop and report the stall. For the background feed
/// the stop is equally right: everything after an unincorporated epoch transition
/// is future-epoch-unprocessable anyway, and the next tick retries the heal.
///
/// The folder twin of this contract is [`poll_inbound_folder`] /
/// [`FolderPollOutcome`] — the two rails share [`apply_inbound_commit`] and must
/// never fork on cursor safety.
pub async fn poll_inbound_conv(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    channel_id: &ChannelId,
    after_seq: &mut i64,
    page_limit: i64,
) -> Result<ConvPollOutcome, BackendError> {
    // ⚠ A retired engine must not walk this log AT ALL, and the refusal has to
    // land here rather than at the `decrypt` below. The walk advances
    // `after_seq` past a record BEFORE decrypting it and skips every decrypt
    // failure with `continue` (correctly — an own message cannot be decrypted
    // by its sender) — and the session writes the advanced cursor to the
    // durable cross-device watermark. So an engine that merely failed each
    // decrypt would silently skip every record it walked, permanently, on every
    // device that later resumes from that watermark: strictly worse than the
    // ghost it replaces. Refusing before the cursor moves leaves the records on
    // the nest log for the successor, which holds the retire-point snapshot and
    // can still decrypt them.
    if backend.engine.is_retired() {
        return Err(BackendError::Internal(
            "the conversations-engine role was handed over — this engine must not \
             walk the inbound log (its cursor would advance past records it cannot \
             decrypt)"
                .into(),
        ));
    }
    let Some(thread_id) = backend.thread_for_channel(channel_id) else {
        return Ok(ConvPollOutcome::EMPTY);
    };
    let channel_hex = channel_id.to_string();
    // Floor delete records an earlier pass stepped past unjudged: the walk
    // will not meet them again, so they are re-judged here.
    backend
        .retry_parked_floor_deletes(manager, channel_id)
        .await;
    let mut ingested = 0usize;
    let started_at = *after_seq;
    let fetch_limit = wire_fetch_limit(page_limit);
    'pages: loop {
        let page_started_at = *after_seq;
        let entries = backend
            .rpc
            .channel_fetch(
                channel_hex.clone(),
                *after_seq,
                fetch_limit,
                backend.channel_home_url(channel_id),
            )
            .await?;
        if entries.is_empty() {
            break;
        }
        for crate::backend::FetchedRecord {
            seq,
            envelope,
            legal_takedown_ref,
            labels: server_labels,
            // The nest-attested author is the scheduling drain's alone: a
            // conversation authenticates its sender by the MLS credential.
            author: _,
        } in entries
        {
            // Where the cursor sat BEFORE this record. Every arm below may
            // advance past its record — except a membership `Commit` the local
            // group could not incorporate, which is the one record a Rule-2-safe
            // cursor must stop *before* (`CommitApplyOutcome::Stalled`). That arm
            // restores this value and ends the walk, so the returned seq is never
            // a baseline past an epoch transition this device never applied.
            let before_seq = *after_seq;
            if seq > *after_seq {
                *after_seq = seq;
            }
            // Legal-takedown tombstone (moderation.md § Categories & enforcement
            // item 1): the nest withheld the sealed envelope (empty) under a legal
            // obligation, so there is nothing to decode/decrypt — ingest a tombstone
            // carrying only the reference; the client renders
            // `legalTakedownTombstone(reference)` in place of the bubble. The store
            // dedups by `message_id` (`conv:<channel>:<seq>`), so if this device
            // already synced the real message before the takedown its copy stands —
            // the intended best-effort E2E boundary: the nest cannot recall a
            // message already delivered to a device.
            if let Some(reference) = legal_takedown_ref {
                let tombstone = legal_takedown_inbound(&channel_hex, seq, reference);
                if manager
                    .ingest_inbound_to_thread(thread_id.clone(), tombstone)
                    .is_ok()
                {
                    ingested += 1;
                }
                continue;
            }
            // The record's account-data-plane identity: the content hash of the
            // envelope the nest filed, derived through the nest's own mint
            // (`crate::plane`). Computed HERE, once per record, because this is
            // the only layer that still holds the sealed bytes — the decrypt
            // below consumes them, and the snapshot never carries them.
            let plane_ref = crate::plane::plane_ref(&channel_hex, &envelope);
            let Ok(env) = ChannelEnvelope::from_bytes(&envelope) else {
                continue; // undecodable record — skip, don't stall the feed
            };
            match env {
                // A **community room's** message, sealed under the room's
                // generation key rather than this channel's MLS ratchet
                // (`conversation-rooms.md` § The three classes → *Community*).
                // The two classes share one storage shape and one read feed,
                // "differing only in which key the reader holds" — which is
                // what this arm is: everything past the open is the `Text`
                // fold the MLS arm below performs.
                ChannelEnvelope::RoomSealed {
                    generation,
                    ciphertext,
                } => {
                    // Same dedup guard as the application arm, and for the
                    // same reason: a re-walk (restored history, a Rule-2 heal,
                    // an overlapping re-poll) must not pay for an unwrap it
                    // already folded. Thread-local, as there.
                    if manager.thread_holds_message(
                        &thread_id,
                        &MessageId(format!("conv:{channel_hex}:{seq}")),
                    ) {
                        continue;
                    }
                    let Ok(generation) = <[u8; 32]>::try_from(generation.as_slice()) else {
                        continue; // a malformed generation id names no wrap
                    };
                    // No key for this generation is the **honest skip** this
                    // arm inherited from its predecessor, not a failure: a
                    // member joined under a `history_policy` that retains
                    // nothing was never wrapped into the generations it
                    // predates, and a backend whose seams are unregistered
                    // holds no room key at all. Either way the record stays on
                    // the nest log for a session that can open it.
                    let key = match backend.room_generation_key(channel_id, &generation).await {
                        // The first key after a wait: stop before this record
                        // ONCE, ingesting nothing from it, so the caller can
                        // reopen the Conversation catch-up window before the
                        // room's backlog reaches the index seam
                        // ([`ConvPollOutcome::keyed_in`]). The wait is over
                        // either way — the next pass walks straight through.
                        RoomKeyLookup::Key(_) if backend.end_key_wait(channel_id) => {
                            manager.notify_room_state_changed();
                            *after_seq = before_seq;
                            return Ok(ConvPollOutcome {
                                ingested,
                                stalled: false,
                                awaiting_key: false,
                                keyed_in: true,
                            });
                        }
                        RoomKeyLookup::Key(key) => key,
                        RoomKeyLookup::NotForUs | RoomKeyLookup::Unkeyable => continue,
                        // Stop BEFORE this record, the unincorporated-commit
                        // arm's move: stepping past it would skip, for good, a
                        // message this member is about to be able to read —
                        // everything a newcomer's walk meets between its
                        // acceptance and the inviter's key-in, which is sealed
                        // under the very tip the key-in covers. The next pass
                        // retries; nothing after it opens either, since this
                        // member holds no key in the room at all.
                        RoomKeyLookup::NotYet => {
                            *after_seq = before_seq;
                            if backend.begin_key_wait(channel_id) {
                                manager.notify_room_state_changed();
                            }
                            return Ok(ConvPollOutcome {
                                ingested,
                                stalled: true,
                                awaiting_key: true,
                                keyed_in: false,
                            });
                        }
                    };
                    // Opens AND verifies the author's signature — the class's
                    // attribution, since a generation key every member holds
                    // authenticates nobody (`fauna_mls::room_message`). A
                    // refusal is a member sealing under another member's name,
                    // so it never becomes a bubble.
                    let signed = match fauna_mls::room_message::open_room_message(
                        &key,
                        &channel_id.0,
                        &generation,
                        &ciphertext,
                    ) {
                        Ok(signed) => signed,
                        Err(e) => {
                            tracing::warn!("room message did not open on {channel_hex}: {e}");
                            continue;
                        }
                    };
                    // The sender's own delete and reactions — side-effects on
                    // a prior message, never bubbles, exactly as in the MLS
                    // arm below. The claimant / reactor is the **verified
                    // signed author**; no role is recorded (`None`), so a
                    // sealed delete is admitted on the sender match alone — an
                    // owner's or admin's delete of another member's message is
                    // the floor delete record's, never this body's
                    // (`conversation-rooms.md` § Roles and authorization →
                    // *Delete any message — the mechanism* → *Community rooms*).
                    match &signed.core.body {
                        ChannelMessageBody::Reaction {
                            target_seq,
                            emoji,
                            op,
                        } => {
                            // The stamp comes off the SIGNED core, never the
                            // log: it is the one ordering token an author's
                            // signature covers, and this class's records are
                            // replayable at a fresh `seq` by any member or by
                            // the home nest (`community-rooms.md` § The three
                            // classes → *Community* → *Who wrote it*;
                            // `crate::reactions::fold_reactions`).
                            manager.apply_inbound_reaction(
                                MessageId(format!("conv:{channel_hex}:{target_seq}")),
                                signed.core.author,
                                emoji.clone(),
                                op.clone(),
                                signed.core.sent_at_ms,
                            );
                            continue;
                        }
                        ChannelMessageBody::Delete { target_seq } => {
                            manager.apply_inbound_delete_claim(
                                MessageId(format!("conv:{channel_hex}:{target_seq}")),
                                crate::message::DeleteClaim {
                                    claimant: signed.core.author,
                                    delete_seq: u64::try_from(seq).ok(),
                                    role: None,
                                },
                            );
                            continue;
                        }
                        _ => {}
                    }
                    // An attachment-bearing message: fetch + open + cache each
                    // blob under the room's ATTACHMENT content kind off the
                    // generation this envelope names — the same loop the MLS
                    // arm runs, differing only in the opener
                    // (`community-rooms.md` § The three classes →
                    // *Attachments — the second content kind*).
                    let attachments = match &signed.core.body {
                        ChannelMessageBody::Attachments { attachments, .. } => {
                            fetch_open_cache_attachments(
                                backend,
                                manager,
                                channel_id,
                                &channel_hex,
                                attachments,
                                AttachmentOpener::Room {
                                    key: &key,
                                    generation: &generation,
                                },
                            )
                            .await
                        }
                        _ => Vec::new(),
                    };
                    let Some(msg) =
                        room_message_to_inbound(&signed, &channel_hex, seq, plane_ref, attachments)
                    else {
                        continue; // another body kind — richer bodies are a follow-on
                    };
                    // The verdicts the room's home nest served beside the
                    // envelope — what the labelers the room names derived
                    // (`conversation-rooms.md` § The three classes → *What the
                    // home nest does with its read*, purpose 2) — merged into
                    // this device's own by the superset rule.
                    if manager
                        .ingest_inbound_with_server_labels(thread_id.clone(), msg, &server_labels)
                        .is_ok()
                    {
                        ingested += 1;
                    }
                }
                // An owner's or admin's floor delete record — the one unsealed
                // variant, so there is nothing to open: only to judge. Fails
                // closed — paints nothing — until it is verified against the
                // anchored policy of the version it names
                // ([`FaunaMlsBackend::floor_delete_verdict`]). Never a bubble.
                ChannelEnvelope::RoomFloorDelete(record) => {
                    match fauna_mls::room_policy::SignedRoomFloorDelete::from_bytes(&record) {
                        Ok(signed) => {
                            backend
                                .fold_floor_delete(
                                    manager,
                                    channel_id,
                                    ParkedFloorDelete {
                                        signed,
                                        delete_seq: u64::try_from(seq).ok(),
                                    },
                                )
                                .await;
                        }
                        Err(e) => tracing::warn!("floor delete record on {channel_hex}: {e}"),
                    }
                    continue;
                }
                ChannelEnvelope::Application(ct) => {
                    // Already folded (restored history / local echo / an earlier
                    // poll): skip the decrypt and — for attachment messages —
                    // the blob re-fetch. The store would dedup the append anyway
                    // (`ThreadStore::append_message`); checking first keeps a
                    // Rule-2 heal re-walk and any overlapping re-poll cheap.
                    // Effect-only messages (reactions/deletes/renames) are never
                    // stored under this id, so they re-apply idempotently below.
                    //
                    // ⚠ **Asked of THIS channel's thread, never of the whole
                    // store.** This skip runs BEFORE the decrypt, so whatever
                    // it steps over is never read — text, reaction, delete,
                    // succession statement or ownership offer alike — and the
                    // cursor has already moved past it. `seq` is minted by
                    // this channel's log, so only this channel's thread can
                    // honestly hold the id; the store-wide question let an id
                    // planted in ANY thread silence a record here.
                    if manager.thread_holds_message(
                        &thread_id,
                        &MessageId(format!("conv:{channel_hex}:{seq}")),
                    ) {
                        continue;
                    }
                    // The sender's role rides out of the same decrypt that
                    // authenticates the sender: it is read from the group
                    // context of the epoch the message was sealed in, which is
                    // only in hand at this moment (`conversation-rooms.md`
                    // § Roles and authorization → *Delete any message — the
                    // mechanism*). Only a `Delete` consumes it.
                    let Ok(fauna_mls::engine::DecryptedMessage {
                        message: cm,
                        sender_role,
                        epoch: sealed_in_epoch,
                    }) = backend.engine.decrypt_authenticated(channel_id, &ct)
                    else {
                        // Own message (a sender can't decrypt its own
                        // application messages) or a real decrypt failure.
                        continue;
                    };
                    // Group metadata changes ride the channel as app messages
                    // but apply as thread effects, not chat bubbles. A rename
                    // (`GroupMeta::NameChanged`, posted by `RailBackend::rename`)
                    // renames the bound thread.
                    if let ChannelMessageBody::GroupMeta(GroupMetaMessage::NameChanged(label)) =
                        &cm.body
                    {
                        manager.apply_inbound_rename(thread_id.clone(), label.clone());
                        continue;
                    }
                    // The in-group succession statement (identity-succession.md
                    // § Propagation → MLS groups) — a thread effect, not a
                    // bubble. The carried bytes are a CLAIM: only the witness
                    // (session-wired — it holds the anchor sources) may promote
                    // them, and only a verified pair re-points the thread's
                    // participant row so the add+remove ceremony renders as
                    // continuity. No witness, a malformed statement, and a
                    // failed or unanchorable verification all degrade to the
                    // bare add — the same rendering an older client (which
                    // cannot decode the variant and skips the record) gets —
                    // never to trusting the carried bytes or the transport
                    // sender.
                    if let ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(bytes)) =
                        &cm.body
                    {
                        // The tally beside each arm, not a summary at the end:
                        // every one of these degrades to the same silent
                        // no-op, so which arm ran is the only diagnosis there
                        // is (`SuccessionStatementCounts`).
                        use std::sync::atomic::Ordering::Relaxed;
                        let tally = &backend.succession_statements;
                        tally.seen.fetch_add(1, Relaxed);
                        let Some(witness) = backend.succession_witness() else {
                            tally.no_witness.fetch_add(1, Relaxed);
                            continue;
                        };
                        let Ok(statement) = fauna_core::encoding::canonical_decode::<
                            fauna_core::recovery::SignedIdentitySuccession,
                        >(bytes) else {
                            tally.undecodable.fetch_add(1, Relaxed);
                            continue;
                        };
                        let old_actor = statement.statement.old_actor_id;
                        match witness.verify(statement.clone()).await {
                            // Verified — but a verified statement is not yet a
                            // re-point: it must also have happened in THIS
                            // group ([`roster_pair`]). The honest ceremony
                            // lands here mid-flight (the statement rides with
                            // the add, so the predecessor is still seated), so
                            // that verdict parks for the remove-old commit
                            // rather than degrading.
                            Some(verified) => {
                                if settle_verified_succession(
                                    backend, manager, &thread_id, channel_id, &verified,
                                ) == RosterPair::AwaitingRemoveOld
                                {
                                    backend.park_succession(thread_id.clone(), statement);
                                }
                            }
                            // Refused for now — and this delivery is the only
                            // copy this session will ever hold (a consumed MLS
                            // application message cannot be re-decrypted), so
                            // if the named identity has a row in this thread —
                            // the only row a re-drive could ever re-point, and
                            // the bound that keeps a forger of arbitrary
                            // old_actor_ids at roster size — park it
                            // for the harvest re-drive
                            // (`redrive_parked_successions`).
                            None => {
                                let names_a_row = manager
                                    .thread_detail(thread_id.clone())
                                    .is_some_and(|d| {
                                        d.participants.iter().any(|p| matches!(
                                            p,
                                            TypedAddress::Fauna { actor_id, .. } if *actor_id == old_actor
                                        ))
                                    });
                                if names_a_row {
                                    backend.park_succession(thread_id.clone(), statement);
                                }
                            }
                        }
                        continue;
                    }
                    // An ownership offer (`conversation-rooms.md` § Roles and
                    // authorization → *Ownership transfer*): the outgoing
                    // owner's countersigned policy naming its successor. Every
                    // member decrypts it; only the named member acts, and not
                    // here — it is parked for the driver to complete after
                    // this walk (`complete_ownership_offer_locked`), because a
                    // gated commit's catch-up re-enters this very loop.
                    if let ChannelMessageBody::GroupMeta(GroupMetaMessage::OwnershipOffer(bytes)) =
                        &cm.body
                    {
                        backend.park_ownership_offer(channel_id, bytes);
                        continue;
                    }
                    // History for a joiner (`conversation-rooms.md` § History
                    // for joiners → *What a device accepts*). A slice is an
                    // application message, so MLS lets any member seal one at
                    // any time; [`accept_joiner_slice`] decides whether this
                    // one is the history THIS device is owed, and a refused
                    // slice folds nothing at all.
                    if let ChannelMessageBody::GroupMeta(GroupMetaMessage::HistorySlice(bytes)) =
                        &cm.body
                    {
                        match accept_joiner_slice(
                            backend,
                            channel_id,
                            &channel_hex,
                            seq,
                            &cm.sender,
                            sealed_in_epoch,
                            bytes,
                        ) {
                            Ok(mut slice) => {
                                // ⚠ The derived state, one step narrower than
                                // the messages. A `full`-history room already
                                // trusts the inviter for the past it supplies
                                // — they could omit or fabricate any
                                // pre-admission message — so a tombstone or a
                                // reaction pill on a message THIS slice is
                                // giving us is within that trust and is taken.
                                // A message this device already holds is not:
                                // that one arrived first-hand, and the inviter
                                // has no standing to tombstone it or restate
                                // who reacted to it — on the live path such a
                                // delete would have to clear
                                // `DeleteClaim::admits` (the sender match or a
                                // recorded governing role), and a slice
                                // carries the folded verdict with no claimant
                                // to re-judge. Decided BEFORE the fold, while
                                // "already holds" still means first-hand.
                                //
                                // The id-must-be-in-the-slice half of the
                                // bound is enforced for every caller by the
                                // manager's restore.
                                let held =
                                    |id: &MessageId| manager.thread_holds_message(&thread_id, id);
                                slice.deleted_messages.retain(|id| !held(id));
                                slice.reaction_log.retain(|id, _| !held(id));
                                manager.restore_joiner_history(&thread_id, &slice);
                            }
                            Err(refusal) => tracing::warn!(
                                channel = %channel_hex,
                                seq,
                                %refusal,
                                "history slice refused; nothing folded"
                            ),
                        }
                        continue;
                    }
                    // Reactions + cooperative deletes are side-effects on a prior message, not
                    // chat bubbles (conversations.md § Reactions & message delete). Apply to the
                    // manager's derived state keyed by the target message id; render no bubble.
                    if let ChannelMessageBody::Reaction {
                        target_seq,
                        emoji,
                        op,
                    } = &cm.body
                    {
                        let target = MessageId(format!("conv:{channel_hex}:{target_seq}"));
                        // The same stamp the community arm takes off its signed
                        // core, in the same unit — `ChannelMessage.timestamp`
                        // is MICROseconds (`fauna_core::data::Timestamp`), the
                        // signed `sent_at_ms` is milliseconds — so one fold
                        // serves both classes (priority #2). This class cannot
                        // be replayed (a consumed MLS ratchet key does not
                        // re-open a re-appended ciphertext), so the stamp buys
                        // uniformity here rather than a security property.
                        manager.apply_inbound_reaction(
                            target,
                            cm.sender,
                            emoji.clone(),
                            op.clone(),
                            i64::try_from(cm.timestamp.0 / 1000).unwrap_or(i64::MAX),
                        );
                        continue;
                    }
                    if let ChannelMessageBody::Delete { target_seq } = &cm.body {
                        let target = MessageId(format!("conv:{channel_hex}:{target_seq}"));
                        manager.apply_inbound_delete_claim(
                            target,
                            crate::message::DeleteClaim {
                                claimant: cm.sender,
                                delete_seq: u64::try_from(seq).ok(),
                                role: sender_role.map(Into::into),
                            },
                        );
                        continue;
                    }
                    // A custody-ceremony payload (W8.4) — a thread effect,
                    // never a bubble. The verbatim bytes go to the
                    // session-wired sink, which decodes + verifies (signer
                    // bound to the MLS-authenticated `cm.sender` — decrypt
                    // overwrote the self-asserted field with the leaf
                    // credential) and captures durably before answering
                    // `true`. No sink / capture failure: tallied and skipped
                    // — the payload waits in the channel history (per-launch
                    // 0-seeded re-walks re-feed it; ingest is idempotent per
                    // grant id), and the ceremony's decay-to-re-offer is the
                    // ultimate heal. The walk never stalls here — bubbles
                    // keep flowing past a custody CAS hiccup.
                    if let ChannelMessageBody::Custody(bytes) = &cm.body {
                        use std::sync::atomic::Ordering::Relaxed;
                        let tally = &backend.custody_payloads;
                        tally.seen.fetch_add(1, Relaxed);
                        match backend.custody_ceremony_sink() {
                            None => {
                                tally.no_sink.fetch_add(1, Relaxed);
                            }
                            Some(sink) => {
                                if sink.custody_payload(&channel_hex, cm.sender, bytes).await {
                                    tally.captured.fetch_add(1, Relaxed);
                                } else {
                                    tally.uncaptured.fetch_add(1, Relaxed);
                                }
                            }
                        }
                        continue;
                    }
                    // A custody receipt: same sink, same never-a-bubble
                    // handling, same never-stall rule — but its own body and
                    // its own tally, because a receipt that fails to verify is
                    // a different fact from a ceremony step that fails to
                    // capture, and collapsing them would hide a lying or
                    // misconfigured custodian inside a CAS-hiccup counter.
                    if let ChannelMessageBody::CustodyReceipt(bytes) = &cm.body {
                        use std::sync::atomic::Ordering::Relaxed;
                        let tally = &backend.custody_receipts;
                        tally.seen.fetch_add(1, Relaxed);
                        match backend.custody_ceremony_sink() {
                            None => {
                                tally.no_sink.fetch_add(1, Relaxed);
                            }
                            Some(sink) => {
                                if sink.custody_receipt(&channel_hex, cm.sender, bytes).await {
                                    tally.captured.fetch_add(1, Relaxed);
                                } else {
                                    tally.uncaptured.fetch_add(1, Relaxed);
                                }
                            }
                        }
                        continue;
                    }
                    // A share-set endpoint advertisement — the shared arm
                    // (`route_share_endpoints`), because folder channels carry
                    // the same body through their own poll and the two rails
                    // must not drift.
                    if route_share_endpoints(backend, &channel_hex, &cm).await {
                        continue;
                    }
                    // An attachment-bearing message: fetch + open + cache each
                    // blob, then ingest a bubble carrying the rendered
                    // `AttachmentSnapshot`s (the receive twin of `encode_body`).
                    if matches!(&cm.body, ChannelMessageBody::Attachments { .. }) {
                        if let Some(msg) = attachments_to_inbound(
                            backend,
                            manager,
                            channel_id,
                            &channel_hex,
                            &cm,
                            seq,
                            plane_ref,
                        )
                        .await
                            && manager
                                .ingest_inbound_to_thread(thread_id.clone(), msg)
                                .is_ok()
                        {
                            ingested += 1;
                        }
                        continue;
                    }
                    let Some(msg) = channel_message_to_inbound(&cm, &channel_hex, seq, plane_ref)
                    else {
                        continue; // other non-text body — richer bodies are a follow-on
                    };
                    if manager
                        .ingest_inbound_to_thread(thread_id.clone(), msg)
                        .is_ok()
                    {
                        ingested += 1;
                    }
                }
                ChannelEnvelope::Commit(cb) => {
                    match apply_inbound_commit(backend, channel_id, &cb).await {
                        // The transition is held (applied now). Safe to
                        // consume — the cursor advance above stands. A commit
                        // may have changed the room policy, whose name is the
                        // thread's label on a governed room
                        // (`conversation-rooms.md` § Roles and authorization):
                        // re-read it off the agreed group context. And it may
                        // have moved ONLY the policy — the roles and the
                        // role-gated capabilities every app paints — which no
                        // step here ticks for, so the observers are told the
                        // group context moved whatever else it did.
                        CommitApplyOutcome::Advanced => {
                            refresh_room_label(backend, manager, &thread_id, channel_id);
                            reconcile_roster(backend, manager, &thread_id, channel_id);
                            // After the reconcile, never before: it is what
                            // keeps a predecessor's row alive for the re-point
                            // while a successor on its chain is seated, and a
                            // re-drive ahead of it would re-point a row the
                            // reconcile had not decided yet.
                            redrive_parked_in_thread(backend, manager, &thread_id, channel_id)
                                .await;
                            notice_superseded_own_offer(backend, manager, channel_id);
                            manager.room_projection_moved(&thread_id);
                        }
                        // Already ours, or a record no member can apply.
                        CommitApplyOutcome::Skipped => {}
                        CommitApplyOutcome::Stalled { future_epoch } => {
                            // The Rule-2 heal (`devices.md` § Cross-device MLS
                            // group-state sync), for the future-epoch strand only:
                            // this device's cursor skipped the bridging commit —
                            // an un-processable hole ({provider, cursor}
                            // pairs sealed under Rule 2 cannot tear). Rewind the
                            // in-flight cursor to 0 and re-walk the log within this
                            // same call: already-applied commits quiet-skip
                            // (`PastEpochCommit`), already-held messages skip via
                            // the `thread_holds_message` guard above, and the skipped commit
                            // finally applies, so the walk lands on the head epoch.
                            // The rewind is TRANSIENT — the caller's durable cursor
                            // (`ChannelCursor::advance`, monotonic) only ever sees
                            // the final position, and the next provider save then
                            // writes a consistent {provider, cursor} pair, retiring
                            // the torn state. Guarded to at most one rewind per
                            // channel per session (an un-processable commit re-tears
                            // on every walk — rewinding cannot fill that hole) and
                            // skipped when the walk already began at 0. An own-leaf
                            // stall is never healable this way (MLS can never
                            // process an own commit), so it does not even try.
                            if future_epoch
                                && started_at > 0
                                && backend.grant_cursor_rewind(channel_id)
                            {
                                tracing::warn!(
                                    channel = %channel_id,
                                    "future-epoch commit: durable ingest cursor outran the \
                                     provider replica; rewinding to 0 and re-walking \
                                     (devices.md Rule 2 heal)"
                                );
                                *after_seq = 0;
                                continue 'pages;
                            }
                            // Unhealable in this pass. STOP BEFORE this record:
                            // restore the cursor to where it sat, so the next pass
                            // retries the heal and — decisively — a gated-send
                            // catch-up can never take a baseline past an epoch
                            // transition the local group never incorporated
                            // (`BackendCatchUp::catch_up_after` turns this stall
                            // into an error). Consuming it is what let a rebuilt
                            // commit be accepted for an epoch the other members had
                            // already left: every member quiet-skips it as
                            // `PastEpochCommit` while the sender merges and reports
                            // success — a silent permanent fork, and a
                            // `remove_participant` that "succeeds" while the removed
                            // member keeps decrypting.
                            *after_seq = before_seq;
                            tracing::error!(
                                channel = %channel_id,
                                future_epoch,
                                "commit not incorporated and unhealable this pass (own-leaf \
                                 resync pending, or a future-epoch hole with the Rule-2 rewind \
                                 spent/unavailable): stopping the walk before it — channel \
                                 stranded until the next resync/relaunch heals it"
                            );
                            return Ok(ConvPollOutcome {
                                ingested,
                                stalled: true,
                                awaiting_key: false,
                                keyed_in: false,
                            });
                        }
                    }
                }
            }
        }
        // The walk pages until an EMPTY page — never until a short one. A
        // short page does NOT mean the log is drained: the nest closes a page
        // early on the 2 MiB frame budget (`transport.md` § Max frame), and
        // an already-deployed nest clamps `limit: 0` to a one-record page.
        // Only the empty page proves the end of the log — which is exactly
        // what makes the arm-2 reconcile below sound (a "complete walk" must
        // reach the end, not the first short page). Progress guard: every
        // served seq exceeds the requested `after`, so a non-empty page that
        // advanced nothing is a server contract violation — stop, don't spin.
        if *after_seq <= page_started_at {
            // A non-empty page that advanced nothing is a server-contract
            // violation. The walk is INCOMPLETE — return `stalled: true` (like
            // the unhealable-stall arm) so the early return SKIPS the arm-2 clear
            // below AND `BackendCatchUp::catch_up_after` aborts a gated send
            // rather than take an `expect_no_commit_since` baseline from a
            // truncated walk. Falling through to a clean `stalled: false`
            // laundered positive evidence of nest misbehavior into the
            // destructive answer, at precisely the moment the premise should be
            // distrusted.
            tracing::error!(
                channel = %channel_id,
                cursor = *after_seq,
                "conv fetch page advanced no cursor — stopping the walk as INCOMPLETE (stalled)"
            );
            return Ok(ConvPollOutcome {
                ingested,
                stalled: true,
                awaiting_key: false,
                keyed_in: false,
            });
        }
    }
    // Gate-less crash reconcile, arm 2 (`devices.md` § Durability rules Rule 1):
    // a *resumed* staged pending — reloaded from the provider snapshot, its
    // authoring process dead — whose commit a complete walk of the whole log
    // (from 0, unstalled) never matched was provably never accepted by the
    // nest: clear it so the interrupted add/remove can be retried (openmls
    // forbids staging over an unmerged pending). Never touches a pending
    // staged by THIS process (its send may be in flight — the engine tracks
    // origin), and a landed commit never reaches here (the own-commit
    // hash-match in `apply_inbound_commit` merges it mid-walk). Runs under the
    // caller-held channel lock like the rest of the walk.
    if started_at == 0 && backend.engine.has_resumed_pending(channel_id) {
        tracing::warn!(
            channel = %channel_id,
            "a staged membership commit from a previous run never reached the channel \
             log; clearing the resumed pending — the interrupted operation was never \
             distributed and can be retried"
        );
        if let Err(e) = backend.engine.clear_pending_commit(channel_id) {
            tracing::warn!(error = ?e, "clearing the resumed pending failed");
        }
        backend.persist_engine_state();
    }
    Ok(ConvPollOutcome {
        ingested,
        stalled: false,
        awaiting_key: false,
        keyed_in: false,
    })
}

/// [`poll_inbound_conv`] for a caller with **no content-index catch-up window of
/// its own** — the app-driven sweeps (`ConversationsSession::poll_conversations`,
/// web's `pollConversations`): it walks straight past the one-shot
/// [`ConvPollOutcome::keyed_in`] stop by walking again, and adds up what both
/// passes ingested.
///
/// The stop exists for the receive loop, which reopens the Conversation window
/// before the re-walk (`session::poll_bound`). A caller that has no window to
/// reopen would only show the newly readable room one sweep later. If such a
/// sweep wins the race to the first key, the room's backlog stages as trickle —
/// the classification every room's backlog had before the stop existed, and never
/// a loss of content.
///
/// # Errors
/// Exactly [`poll_inbound_conv`]'s.
pub async fn poll_inbound_conv_past_key_in(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    channel_id: &ChannelId,
    after_seq: &mut i64,
    page_limit: i64,
) -> Result<ConvPollOutcome, BackendError> {
    let first = poll_inbound_conv(backend, manager, channel_id, after_seq, page_limit).await?;
    if !first.keyed_in {
        return Ok(first);
    }
    let mut again = poll_inbound_conv(backend, manager, channel_id, after_seq, page_limit).await?;
    again.ingested += first.ingested;
    Ok(again)
}

/// The result of one [`poll_inbound_conv`] pass — the chat-rail twin of
/// [`FolderPollOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvPollOutcome {
    /// Newly-ingested messages (the count this poll used to return on its own).
    pub ingested: usize,
    /// The walk stopped **before** a commit the local group has not incorporated
    /// and this pass could not heal ([`CommitApplyOutcome::Stalled`] — an
    /// own-leaf commit whose resync failed or ran gate-less, or a future-epoch
    /// strand whose Rule-2 rewind was spent or unavailable). The cursor was left
    /// pointing before that record (Rule 2, "round toward the safe side"), so the
    /// next pass retries it; a gated-send catch-up seeing `stalled` knows its
    /// `expect_no_commit_since` still sits before an unincorporated commit and
    /// its rebase cannot safely land.
    ///
    /// A **community room** stops the same way, for the same reason, before a
    /// `RoomSealed` record this account is not keyed into *yet* (a newcomer
    /// between its acceptance and the inviter's key-in, or a generation read
    /// that failed this pass): stepping past it would skip for good a message
    /// that is this member's to read. No gated send exists on that class, so
    /// the catch-up half of the contract has nothing to guard there.
    pub stalled: bool,
    /// The stop above was a community room's **wait for its key-in**, not a
    /// strand: an expected state for a newcomer, reported so the caller logs it
    /// quietly instead of as a fault. Never `true` unless `stalled` is.
    ///
    /// **Not an unfinished fold for the catch-up boundary.** The receive loop
    /// counts such a room as folded for now, so one unkeyed room cannot hold
    /// the account's whole Conversation catch-up open for as long as nobody
    /// keys it in (`content-index-ingest.md` § Ingest triggers, v1 → *A
    /// community room waiting for its key-in*).
    pub awaiting_key: bool,
    /// The walk met the **first key after a wait** and stopped before the first
    /// record it opens, ingesting nothing from it. The cursor is left before
    /// that record, and the room is no longer waiting; the next pass walks
    /// straight through.
    ///
    /// One pass, once per wait, so the receive loop can reopen the Conversation
    /// catch-up window *before* the room's backlog reaches the index seam:
    /// everything behind the wait is backlog, and staged after a closed
    /// boundary it would be trickle, which the lease never withholds. A caller
    /// with no window to reopen walks again at once
    /// ([`poll_inbound_conv_past_key_in`]). Never `true` together with
    /// `stalled`.
    pub keyed_in: bool,
}

impl ConvPollOutcome {
    /// Nothing ingested, nothing stalled — the channel had no bound thread yet.
    pub const EMPTY: Self = Self {
        ingested: 0,
        stalled: false,
        awaiting_key: false,
        keyed_in: false,
    };
}

/// How one inbound membership `Commit` landed on the local ratchet — the
/// [`apply_inbound_commit`] outcome the pollers dispatch on.
///
/// The distinction `Skipped` vs `Stalled` is load-bearing for **cursor
/// advancement** (`devices.md` § Cross-device MLS group-state sync, Rule 2 —
/// "round toward the safe side"): a cursor may advance past a `Skipped` record
/// (its state is already held, or the record is junk that can never apply), but
/// must **stop before** a `Stalled` one — the local group has NOT incorporated
/// that epoch transition, so treating it as consumed lets a later gate-send pass
/// `expect_no_commit_since` and append a forked commit for an epoch the other
/// members already advanced past. `Stalled::future_epoch` further distinguishes
/// the skipped-bridging-commit strand — the trigger for [`poll_inbound_conv`]'s
/// Rule-2 heal (rewind + re-walk, once per channel per session); an own-leaf
/// stall is NOT healable by a rewind (MLS can never process an own commit), so
/// the heal ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitApplyOutcome {
    /// The commit advanced this device's epoch (a processed foreign commit, or
    /// an own-leaf commit the resync arm converged onto).
    Advanced,
    /// Benign non-application: an already-held transition (past-epoch replay /
    /// own merged commit) or an intrinsically invalid record no member can
    /// ever apply ([`MlsError::InvalidCommit`] — the group's canonical state
    /// never advanced past it either). Safe to advance past; stalling on the
    /// invalid kind would let one garbage record from any in-group member
    /// wedge every other member's walk forever (a remote DoS).
    Skipped,
    /// The transition is NOT locally incorporated and could not be healed in
    /// this pass: an own-leaf commit whose resync failed (or ran gate-less), a
    /// future-epoch commit (a skipped-commit strand), or a commit that failed
    /// *locally* (storage/library/merge failure, group not yet held — the
    /// group may have advanced without this device, so Rule 2 rounds the
    /// ambiguity toward stalling). A cursor must not advance past it.
    Stalled {
        /// The stall is a future-epoch commit: this device's ingest cursor
        /// skipped the bridging commit (an un-processable
        /// hole). Healable by the Rule-2 rewind + re-walk.
        future_epoch: bool,
    },
}

/// Apply one inbound membership `Commit` to the MLS ratchet, dispatching on the
/// typed classification (devices.md § Cross-device MLS group-state sync, slice
/// 4c). Shared by [`poll_inbound_conv`]'s Commit arm and the folder commit
/// poll ([`poll_inbound_folder`]) so the two rails never fork. Returns the
/// [`CommitApplyOutcome`]; failures are logged, never fatal. The failure split
/// is Rule 2's two roundings: an *intrinsically invalid* record no member can
/// apply is `Skipped` (one bad record can't stall a feed — stalling would be a
/// remote DoS), while an unhealed own-leaf / future-epoch commit and any
/// *local* failure on a possibly-valid commit report
/// [`CommitApplyOutcome::Stalled`] so a Rule-2-safe cursor stops before it.
pub(crate) async fn apply_inbound_commit(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    commit_bytes: &[u8],
) -> CommitApplyOutcome {
    let outcome = classify_inbound_commit(backend, channel_id, commit_bytes).await;
    // The ONE bump site for `FaunaMlsBackend::folded_commits` (which owns the
    // reasoning). Every arm's verdict funnels through this single comparison, so
    // a future arm cannot quietly acquire a bump — and `Stalled`, the outcome
    // that means "NOT incorporated", provably cannot reach it.
    if outcome == CommitApplyOutcome::Advanced {
        backend.note_folded_commit(channel_id);
    }
    outcome
}

/// [`apply_inbound_commit`]'s classification proper — split out so the fold-in
/// bookkeeping above has exactly one place to observe the verdict.
async fn classify_inbound_commit(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    commit_bytes: &[u8],
) -> CommitApplyOutcome {
    match backend.engine.process_commit(channel_id, commit_bytes) {
        Ok(()) => {
            // Another member's commit advanced the group. Notified to the gate
            // for the record, but since 2026-08-24 this does NOT revoke epoch
            // authorship: the send right rides this device's own leaf's latest
            // commit, which another member's commit never touches — revoking
            // here made two actively-sending members re-take the epoch from
            // each other on every pump pass (see
            // `FaunaCommitGate::note_foreign_commit`). A same-account sibling
            // device's commit never reaches this arm (it classifies as the
            // `OwnLeafCommit` resync signal below, which does revoke).
            if let Some(gate) = backend.commit_gate() {
                gate.note_foreign_commit(*channel_id);
            }
            CommitApplyOutcome::Advanced
        }
        // Our own already-merged commit coming back around the poll, or a
        // replayed foreign commit — the state it would produce is already
        // held. Quiet skip.
        Err(MlsError::PastEpochCommit) => CommitApplyOutcome::Skipped,
        // A commit for an epoch AHEAD of ours: this device skipped the commit that
        // would have bridged the gap — MLS members cannot skip epochs and no
        // member can re-issue a past transition, so without intervention every
        // later commit on this channel lands here too. Report it as the heal
        // trigger: the chat-rail poll rewinds its cursor and re-walks the log
        // (once per channel per session — `poll_inbound_conv`'s heal block, per
        // `devices.md` § Cross-device MLS group-state sync Rule 2).
        Err(MlsError::FutureEpochCommit { epoch }) => {
            tracing::warn!(
                channel = %channel_id,
                commit_epoch = epoch,
                "future-epoch commit: this device's ingest cursor skipped the bridging commit \
                 (un-processable hole)"
            );
            CommitApplyOutcome::Stalled { future_epoch: true }
        }
        // An own-leaf commit (MLS can never process one). Two shapes:
        // (1) THIS device staged it and crashed (or lost the race) between the
        //     nest accepting the send and the local merge — the gate-less
        //     crash window (`devices.md` § Durability rules Rule 1). The
        //     durably-persisted staged pending IS the heal: if its stamped
        //     blake3 identity equals this logged commit's, merge it —
        //     identity-proven, so a sibling device's commit can never be
        //     merged in its place. Checked before (and independent of) the
        //     gate: a hash-matched pending is ours regardless of plane.
        // (2) Another of the user's devices took the epoch over: refetch the
        //     provider replica + reload the group via the gate.
        Err(MlsError::OwnLeafCommit { epoch }) => {
            let logged_commit_hash = *blake3::hash(commit_bytes).as_bytes();
            if backend.engine.pending_commit_hash(channel_id) == Some(logged_commit_hash) {
                match backend.engine.merge_pending_commit(channel_id) {
                    Ok(()) => {
                        backend.persist_engine_state();
                        tracing::info!(
                            channel = %channel_id,
                            "merged the reloaded staged pending for this device's own logged \
                             commit (gate-less crash-window heal, devices.md Rule 1)"
                        );
                        return CommitApplyOutcome::Advanced;
                    }
                    Err(e) => {
                        // Fall through to the resync/stall dispatch below —
                        // the pending stays for the next pass.
                        tracing::warn!(
                            error = ?e,
                            "merging the identity-matched staged pending failed"
                        );
                    }
                }
            }
            if let Some(gate) = backend.commit_gate() {
                // Bind the resync's crash-window merge to *this* commit's identity
                // (`logged_commit_hash` above): the restored step-2 pending is
                // merged only if it is the same commit as this logged record
                // (design §3 resync-identity hardening).
                if let Err(e) = gate
                    .resync_channel(*channel_id, epoch, logged_commit_hash)
                    .await
                {
                    tracing::warn!(error = ?e, "own-leaf commit resync failed; group ratchet stale until the next resync trigger");
                }
                // Converged iff the resync actually carried the group past the
                // logged commit's epoch (the restored replica merged/was ahead).
                // A resync that restored a same-or-older epoch without merging
                // (identity mismatch, no replica, transport failure) leaves the
                // transition un-incorporated — Stalled, so a Rule-2-safe cursor
                // stops before this commit and the next pass retries the heal.
                match backend.engine.current_epoch(channel_id) {
                    Ok(current) if current > epoch => CommitApplyOutcome::Advanced,
                    _ => CommitApplyOutcome::Stalled {
                        future_epoch: false,
                    },
                }
            } else {
                // No multi-device plane injected (single-device client): the launch-time replica load is the
                // remaining heal path.
                tracing::warn!(
                    "own-leaf foreign commit with no CommitGate injected; group ratchet stale until relaunch"
                );
                CommitApplyOutcome::Stalled {
                    future_epoch: false,
                }
            }
        }
        // Intrinsically invalid: no member can ever apply these bytes, so the
        // group's canonical state never advanced past them either — later
        // records still decrypt at the current epoch and the cursor may
        // consume this one (`MlsError::InvalidCommit`). Stalling here instead
        // would hand any in-group member a remote DoS: one garbage commit
        // would pin every other member's walk on this channel forever.
        // `CredentialBindingViolation` is the MLS-2 forged-leaf reject — the
        // same every honest member computes at this epoch, and an attack
        // signal worth its own loudness.
        Err(e @ (MlsError::InvalidCommit(_) | MlsError::CredentialBindingViolation(_))) => {
            tracing::warn!(
                error = ?e,
                channel = %channel_id,
                "intrinsically invalid inbound commit skipped (no member can apply it)"
            );
            CommitApplyOutcome::Skipped
        }
        // The folder commit policy's refusal (owner-managed roster,
        // `federation.md` § Cross-nest shared folders + channel append): a
        // proposal-carrying commit from a non-owner member. Deterministic for
        // every honest member holding the same owner marker — the canonical
        // group state never advances past it, so the cursor consumes it like
        // an intrinsically invalid record. Loud: a client posting one is
        // hostile or badly broken (the nest admits any rostered member's
        // commit; only members can weigh its content).
        Err(e @ MlsError::PolicyRefusedCommit { .. }) => {
            tracing::warn!(
                error = ?e,
                channel = %channel_id,
                "folder commit policy refused an inbound commit (non-owner roster change) — skipped"
            );
            CommitApplyOutcome::Skipped
        }
        // Everything else is local or ambiguous — storage, a library error, a
        // failed merge, a group this engine doesn't hold (yet): the group may
        // have incorporated this transition while this device did not, and
        // consuming it silently drops every later message sealed under the
        // new epoch (user-irrecoverable) and hands a gated send a forked
        // baseline. Rule 2 (`devices.md` § Cross-device MLS group-state sync)
        // rounds toward the safe side: stop the cursor before it — loud and
        // healable (next pass / relaunch / replica resync), unlike the loss
        // (observability.md category 3).
        Err(e) => {
            tracing::error!(
                error = ?e,
                channel = %channel_id,
                "MLS process_commit failed locally on an inbound commit; stalling the \
                 walk before it (Rule 2) — group ratchet stale until a retry or resync heals it"
            );
            CommitApplyOutcome::Stalled {
                future_epoch: false,
            }
        }
    }
}

/// Why [`accept_joiner_slice`] refused a member's history slice. Logged, never
/// surfaced: a refused slice is an absent one, and the newcomer's room simply
/// starts at its admission, as under `none`.
#[derive(Debug, PartialEq, Eq)]
enum JoinerSliceRefusal {
    Undecodable(String),
    OtherChannel,
    NotFullHistory,
    NotThisDevicesAdmission,
    ForeignOrFutureId(String),
}

impl std::fmt::Display for JoinerSliceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Undecodable(e) => write!(f, "undecodable: {e}"),
            Self::OtherChannel => f.write_str("it names another channel"),
            Self::NotFullHistory => {
                f.write_str("the room's policy in force does not give a newcomer history")
            }
            Self::NotThisDevicesAdmission => f.write_str(
                "not sealed by the account that admitted this device, in the epoch it \
                 joined at, as the first such slice",
            ),
            Self::ForeignOrFutureId(id) => {
                write!(
                    f,
                    "it carries {id:?}, which is not an earlier record of this channel"
                )
            }
        }
    }
}

/// The receiving half of history for joiners (`conversation-rooms.md`
/// § History for joiners → *What a device accepts*): whether the
/// `HistorySlice` record at `seq`, sealed by `sender` in `sealed_in_epoch`
/// (both as the engine authenticated them), is the history **this device** is
/// owed. Every other member's walk reaches this too and gets a refusal — that
/// is what makes "every other member folds a no-op" true.
///
/// The order is load-bearing. The room's policy and the slice's own shape are
/// judged first, and the admission is spent ([`MlsEngine::take_history_admission`])
/// only on a slice that names this channel under `full` from the right account
/// in the right epoch — so a stranger's slice, or the inviter's under `none`,
/// burns nothing. The carried ids are judged AFTER the spend: an inviter whose
/// one slice names a foreign or future id has used its one slice.
///
/// **Refused whole, never filtered.** An honest slice is a snapshot of this
/// channel's thread, whose every id is `conv:{channel}:{seq}` for a record
/// already on the log when the slice was sealed; one that carries anything
/// else was not made by the honest code path, and nothing in it is believed.
fn accept_joiner_slice(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    channel_hex: &str,
    seq: i64,
    sender: &ActorId,
    sealed_in_epoch: u64,
    bytes: &[u8],
) -> Result<crate::store::history::ChannelHistorySlice, JoinerSliceRefusal> {
    let slice = crate::store::history::ChannelHistorySlice::from_bytes(bytes)
        .map_err(|e| JoinerSliceRefusal::Undecodable(e.to_string()))?;
    if slice.channel_id_hex != channel_hex {
        return Err(JoinerSliceRefusal::OtherChannel);
    }
    // The policy in force as this device holds it NOW. The walk is in log
    // order, so that is the policy of the slice's epoch unless a commit landed
    // between the Add and the slice — and then the newer policy is the one to
    // obey: a room that has since said `none` owes this newcomer nothing.
    let full = backend.governed_policy(channel_id).is_ok_and(|policy| {
        policy.signed.policy.history_policy == fauna_mls::room_policy::HistoryPolicy::Full
    });
    if !full {
        return Err(JoinerSliceRefusal::NotFullHistory);
    }
    if !backend
        .engine
        .take_history_admission(channel_id, sender, sealed_in_epoch)
    {
        return Err(JoinerSliceRefusal::NotThisDevicesAdmission);
    }
    let own_prefix = format!("conv:{channel_hex}:");
    for m in &slice.messages {
        let earlier = m
            .message_id
            .0
            .strip_prefix(&own_prefix)
            .and_then(|tail| tail.parse::<i64>().ok())
            .is_some_and(|carried_seq| (0..seq).contains(&carried_seq));
        if !earlier {
            return Err(JoinerSliceRefusal::ForeignOrFutureId(
                m.message_id.0.clone(),
            ));
        }
    }
    Ok(slice)
}

/// After a foreign commit advanced `channel_id`, adopt the room name the
/// agreed group context now carries (`conversation-rooms.md` § Roles and
/// authorization: on a governed room the name is a field of the policy, so a
/// rename arrives as a commit, not as a `NameChanged` message). A policy-less
/// room, an unnamed policy, and an unchanged name are all no-ops.
fn refresh_room_label(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    thread_id: &ThreadId,
    channel_id: &ChannelId,
) {
    let Some(Ok(policy)) = backend.engine.room_policy(channel_id) else {
        return;
    };
    let Some(name) = policy.signed.policy.name.clone() else {
        return;
    };
    let current = manager.thread_detail(thread_id.clone()).map(|d| d.label);
    if current.as_deref() != Some(name.as_str()) {
        manager.apply_inbound_rename(thread_id.clone(), name);
    }
}

/// After an inbound commit advanced the epoch, hand the manager the group's
/// agreed roster so a member removed by SOMEONE ELSE leaves this device's
/// participant list too (`conversation-rooms.md` § The floor roster;
/// `ConversationsManager::apply_inbound_roster`). Governed rooms only: their
/// group context records every identity succession, which is what lets the
/// manager keep a predecessor's row for the re-point **while a real successor
/// on its chain is itself seated** — the manager retains a `superseded` pair
/// only on that condition, since the record is self-authored and never proves
/// its named successor exists; a
/// policy-less room has no such record, so its roster keeps today's behaviour (the
/// committing device alone edits it) rather than risk dropping a row a parked
/// succession statement still needs.
fn reconcile_roster(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    thread_id: &ThreadId,
    channel_id: &ChannelId,
) {
    let Some(Ok(extension)) = backend.engine.room_policy(channel_id) else {
        return;
    };
    // Every advanced commit — not only one that re-seats a pending actor —
    // clears this channel's omission back-off: the committing device usually
    // reports moments after its own commit, so a fresh commit is exactly
    // when a gap built from stale omissions is due to resolve
    // (`FaunaMlsBackend::clear_omitted_roster_reads`).
    backend.clear_omitted_roster_reads(channel_id);
    let roster = backend.engine.group_members(channel_id);
    // `(predecessor, a chain member)` — one pair per identity on the
    // predecessor's WHOLE forward chain, not only its resolved terminal.
    // Production records each hop's succession commit before that hop's
    // successor joins (`fauna_client_recovery::group_sweep`, step 0 then
    // step 1), so a multi-hop predecessor can outlive an intermediate holder
    // while the chain's terminal is still unseated; resolving straight to the
    // terminal dropped the row one commit early — at the SECOND hop's record
    // commit, before that hop's successor had even been added
    // (`ConversationsManager::apply_inbound_roster`, add arm).
    let superseded: Vec<(ActorId, ActorId)> = extension
        .successions
        .iter()
        .flat_map(|s| {
            extension
                .successor_chain(s.old)
                .into_iter()
                .map(move |successor| (s.old, successor))
        })
        .collect();
    manager.apply_inbound_roster(thread_id.clone(), &roster, &superseded);
}

/// Tell the **outgoing** owner its offered hand-over can no longer land
/// (`conversation-rooms.md` § Roles and authorization → *Ownership transfer*:
/// an offer the room has moved past is refused by every member and dropped by
/// the device holding it — "the owner offers again"). The drop happens on the
/// *incoming* owner's device, which is the only one that parks the offer, so
/// before this the offering owner learned of it only from the roles never
/// changing.
///
/// Superseded means: this device offered at version V, and the agreed policy
/// now stands at V or beyond **with this identity still the owner** — the room
/// advanced without the transfer, so the offer can never be the next version
/// again. The two other endings clear the record silently, because neither is
/// a refusal: the transfer landing (the owner is somebody else now, which is
/// what was asked for) and a policy still short of V (the offer is simply
/// still outstanding).
fn notice_superseded_own_offer(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    channel_id: &ChannelId,
) {
    let offered = backend
        .pending_own_offers
        .lock()
        .unwrap()
        .get(channel_id)
        .copied();
    let Some(offered_version) = offered else {
        return;
    };
    let Ok(current) = backend.governed_policy(channel_id) else {
        return;
    };
    let policy = &current.signed.policy;
    if policy.version < offered_version {
        return;
    }
    backend
        .pending_own_offers
        .lock()
        .unwrap()
        .remove(channel_id);
    if policy.owner == backend.self_actor {
        manager.apply_superseded_ownership_offer();
    }
}

/// Ingest an MLS Welcome on the receiver: join the group, materialize the
/// thread, and bind it so [`poll_inbound_conv`] can route subsequent ciphertext
/// into it. Fed by the client's `fauna.conversations.welcome.received` push
/// handler (glue) — the receive-side counterpart to
/// [`FaunaMlsBackend::bootstrap_group`]. `channel_id_hex` comes from the push
/// payload and lets us short-circuit a re-delivered Welcome *before* the
/// key-package-consuming join (a second join would fail, since the init key is
/// spent). `home_nest_url` is the cross-nest Welcome envelope's `nest_url` (the
/// channel's home nest, where its log lives) — recorded so a later
/// [`poll_inbound_conv`] of this channel relays its `channel.fetch` there; blank
/// for a same-nest Welcome (drains locally). Returns the new-or-existing thread id
/// (`docs/goal/ui/conversations.md` § MLS Welcome at-rest, § Architectural
/// rules #2).
pub async fn ingest_welcome(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    channel_id_hex: &str,
    welcome_bytes: &[u8],
    home_nest_url: &str,
) -> Result<ThreadId, BackendError> {
    // Learn the home BEFORE the guards below, not after them. Both of them
    // return on a channel this device is already in — which, after a relaunch,
    // is every restored channel (`restore_and_wire` rebinds thread↔channel, the
    // very map `thread_for_channel` reads) — so with the record below them a
    // re-delivered Welcome taught this session nothing at all, and the recovery
    // `federation.md` names could never run. The guards
    // exist to skip the init-key-consuming *join*; re-recording a home the
    // caller already handed us is free.
    if let Some(channel) = parse_channel_hex(channel_id_hex) {
        backend.record_channel_home_if_absent(channel, home_nest_url);
    }

    // Idempotency: if we already joined+bound this channel, skip the join.
    if let Some(channel) = parse_channel_hex(channel_id_hex)
        && let Some(existing) = backend.thread_for_channel(&channel)
    {
        return Ok(existing);
    }

    // Carry the engine's *not addressed to this device* verdict across the seam
    // as its own variant rather than folding it into a diagnostic: it is the
    // expected outcome on every device launched before the addressed package was
    // minted (`devices.md` § Cross-device MLS group-state sync → *Who may
    // consume a Welcome*), and the push arm's log level turns on telling it
    // apart from a genuine ingest fault.
    let channel_id = backend
        .engine
        .join_from_welcome_bytes(welcome_bytes)
        .map_err(|e| match e {
            MlsError::NotAddressedToThisDevice => BackendError::WelcomeNotAddressedHere,
            other => BackendError::Internal(format!("join welcome: {other}")),
        })?;

    // Cross-nest: remember the channel's home nest so the drain relays its fetch
    // there (same-nest ⇒ blank ⇒ recorded as nothing ⇒ local fetch). Above the
    // race guard for the same reason as the pre-guard record: a push whose hex
    // this build could not parse reaches the home only here, and the guard
    // below would swallow it.
    backend.record_channel_home(channel_id, home_nest_url);

    // A duplicate could have raced in between the check and the join.
    if let Some(existing) = backend.thread_for_channel(&channel_id) {
        return Ok(existing);
    }

    // Roster from the joined group. The Welcome carries no handles, so the
    // app-level ActorId is authoritative and the handle is resolved at the seat
    // through `ConversationsManager::seat_address_for` — the one path this site
    // shares with `apply_inbound_roster`, which reaches whatever this device
    // already knows and leaves the handle empty (rendering as a short id) when
    // it knows nothing.
    let participants: Vec<TypedAddress> = backend
        .engine
        .group_members(&channel_id)
        .into_iter()
        .filter(|a| *a != backend.self_actor)
        .map(|actor_id| manager.seat_address_for(actor_id))
        .collect();

    let thread_id = manager.materialize_conv_thread(channel_id.to_string(), participants);
    backend.bind_channel(thread_id.clone(), channel_id);
    // A governed room's name is a field of the owner-signed policy, and the
    // Welcome's group context already carries it — so the newcomer reads it
    // here rather than waiting for the next commit to refresh it. Nothing else
    // names the room for them: a member's history slice is never taken for its
    // label (`conversation-rooms.md` § History for joiners → *What a device
    // accepts*). A no-op for a policy-less room and for an unnamed one.
    refresh_room_label(backend, manager, &thread_id, &channel_id);
    // Rule 3 (durable-before-done, `devices.md` § Durability rules) — the
    // join-side twin of `bootstrap_group`'s persist. The bind above stamped
    // the durable chat marker, which makes this channel OWE a `history/<ch>`
    // slice at every provider put from now on; inside the autosave debounce
    // the commit gate's provider-only put, on ANY channel, would otherwise
    // list it slice-less. A device restoring that pairing holds the whole
    // account at `Unloaded` and cannot heal it (the slice is the thread: no
    // slice, no bind, no slice) — the one durable state `federation.md`
    // § Discovery-failure semantics says must be unrepresentable, closed at
    // its producer. Best-effort, exactly as at the bootstrap: a transient
    // failure leaves the debounced autosave as the retry.
    if let Some(persist) = backend.history_persist.get()
        && let Err(e) = persist.persist_channel(channel_id).await
    {
        tracing::warn!("welcome-join history persist failed (the debounced autosave retries): {e}");
    }
    // …and the provider: the join spent an init key, and the joined group's
    // state is user-irrecoverable until the replica lists it (the launch
    // swap carries a native store's copy; a web engine has no store to carry
    // from). After the slice, so the flush's own Rule-2 order holds.
    backend.persist_provider_after_join("welcome-join").await;
    Ok(thread_id)
}

/// Ingest a **scheduling** MLS Welcome on the recipient — the mailbox-less
/// CalDAV-iMIP twin of [`ingest_welcome`] (`docs/goal/behavior/caldav-server.md`
/// § Server-side auto-schedule, Half-1). Join the one-off group and record the
/// channel as scheduling ([`FaunaMlsBackend::mark_scheduling_channel`])
/// **instead of** materializing a chat thread + binding it: a scheduling delivery
/// is an iMIP, not a conversation, so [`poll_inbound_scheduling`] later drains its
/// application messages to the calendar-apply [`SchedulingSink`], never the chat
/// UI (caldav-server.md § Server-side auto-schedule — a scheduling delivery never
/// surfaces as a chat thread). `channel_id_hex` (from the push) lets us
/// short-circuit a re-delivered Welcome *before* the key-package-consuming join.
/// `home_nest_url` is the cross-nest Welcome envelope's `nest_url` (the channel's
/// home nest) — recorded so [`poll_inbound_scheduling`] relays this channel's
/// `channel.fetch` there; blank for a same-nest delivery (drains locally).
/// Returns the joined `ChannelId`.
pub async fn ingest_scheduling_welcome(
    backend: &FaunaMlsBackend,
    channel_id_hex: &str,
    welcome_bytes: &[u8],
    home_nest_url: &str,
) -> Result<ChannelId, BackendError> {
    // Above the guard, as in `ingest_welcome` and for the same reason — and
    // this path needs it just as much: `is_scheduling_channel` ORs in the
    // engine's **durable** marker, so the guard fires after a relaunch too.
    if let Some(channel) = parse_channel_hex(channel_id_hex) {
        backend.record_channel_home_if_absent(channel, home_nest_url);
    }

    // Idempotency: a re-delivered scheduling welcome for an already-joined
    // channel must not attempt a second (init-key-spending) join.
    if let Some(channel) = parse_channel_hex(channel_id_hex)
        && backend.is_scheduling_channel(&channel)
    {
        return Ok(channel);
    }

    let channel_id = backend
        .engine
        .join_from_welcome_bytes(welcome_bytes)
        .map_err(|e| BackendError::Internal(format!("join scheduling welcome: {e}")))?;
    backend.mark_scheduling_channel(channel_id);
    // Cross-nest: remember the channel's home nest so the scheduling drain relays
    // its fetch there (same-nest ⇒ blank ⇒ local fetch).
    backend.record_channel_home(channel_id, home_nest_url);
    // Rule 3 on the spent init key, as at every Welcome join (`ingest_welcome`).
    backend
        .persist_provider_after_join("scheduling welcome-join")
        .await;
    Ok(channel_id)
}

/// Join the MLS group of a **cross-user shared folder** from its Welcome — the
/// recipient AUTO/accept action of the contact-gate (`docs/goal/ui/folders.md`
/// § Sharing). The folder twin of [`ingest_scheduling_welcome`]: join the group
/// and record the channel as a folder membership
/// ([`FaunaMlsBackend::mark_folder_channel`]) **instead of** materializing a
/// chat thread + binding it ([`ingest_welcome`]) — a shared folder is *not* a
/// conversation. The join binds the group in the engine, which is all a later
/// content-key envelope open (`MlsEngine::open_content_key_envelope`) / chunk-key
/// export needs; the sync engine syncs the set's content, so there is no chat/iMIP
/// drain and thus no marker-iterated poll (unlike scheduling). `channel_id_hex`
/// (from the push / durable Welcome envelope) lets us short-circuit a re-delivered
/// Welcome *before* the key-package-consuming join — the SAME Welcome arrives via
/// both the best-effort push arm and the durable-inbox drain, so this idempotency
/// is load-bearing. `home_nest_url` is the cross-nest Welcome envelope's `nest_url`
/// (the group's home nest) — recorded so a later content fetch relays there; blank
/// for a same-nest share (drains locally). Since 2026-08-30 the envelope's value is
/// **verified-origin-resolved by this member's own nest** (a dial-proven address of
/// the handshake-verified origin, never the raw peer declaration), so the durable
/// `ForeignFolder` copy written below is constrained at the source. `access` is the home-nest-resolved
/// grant the relay carried (`"reader"`/`"writer"`; `None` from a non-conforming
/// relay ⇒ reader) — **advisory-for-UI only**, recorded so this client knows
/// whether to offer a folder binding. `home_nest_actor_id` is the home nest's
/// deployment identity (the byte-plane SPKI-pin trust root), carried on the same
/// relay and recorded on the foreign-set record. Returns the joined `ChannelId`.
///
/// The home-nest-resolved fields ride grouped in one
/// [`crate::session::FolderWelcomeContext`] (its `shared_by` is the gate's, not
/// read here). A cross-nest Welcome whose name is carried only sealed
/// (`set_name_seal`) is recorded nameless, then named from the seal once the
/// join's custody ingest holds the set's content keys.
pub async fn join_folder_welcome(
    backend: &FaunaMlsBackend,
    channel_id_hex: &str,
    welcome_bytes: &[u8],
    home_nest_url: &str,
    welcome_ctx: &crate::session::FolderWelcomeContext,
) -> Result<ChannelId, BackendError> {
    let set_name = welcome_ctx.set_name.as_deref();
    let access = welcome_ctx.access.as_deref();
    let home_nest_actor_id = welcome_ctx.home_nest_actor_id.as_deref();
    let owner_label = welcome_ctx
        .shared_by_handle
        .clone()
        .filter(|h| !h.is_empty())
        .zip(
            welcome_ctx
                .shared_by_domain
                .clone()
                .filter(|d| !d.is_empty()),
        );
    // Above the guard, as in `ingest_welcome` and for the same reason. This
    // path fails one step later than the other two rather than returning early:
    // `is_folder_channel` is RAM-only, so after a relaunch the guard misses and
    // the re-join below errors on the spent init key — never reaching the
    // record either. Learning the home first is what makes the re-delivery
    // useful on all three paths alike.
    if let Some(channel) = parse_channel_hex(channel_id_hex) {
        backend.record_channel_home_if_absent(channel, home_nest_url);
    }

    // Idempotency: a re-delivered folder welcome for an already-joined channel
    // must not attempt a second (init-key-spending) join.
    if let Some(channel) = parse_channel_hex(channel_id_hex)
        && backend.is_folder_channel(&channel)
    {
        return Ok(channel);
    }

    let channel_id = backend
        .engine
        .join_from_welcome_bytes(welcome_bytes)
        .map_err(|e| BackendError::Internal(format!("join folder welcome: {e}")))?;
    backend.mark_folder_channel(channel_id);
    // Arm the owner-managed-roster commit policy: a folder Welcome's
    // MLS-authenticated sender is the set's owner (the owner-only-Adds
    // invariant that same policy preserves — `federation.md` § Cross-nest
    // shared folders + channel append), so stamp them as the channel's durable
    // folder owner. Crypto-anchored (leaf-verified at join) and works
    // cross-nest, where the nest-asserted `shared_by` is absent. A join whose
    // sender the engine could not resolve stamps nothing — that channel keeps
    // open commit processing (today's behavior) rather than guessing an owner.
    if let Some(owner) = backend.engine.welcome_sender(&channel_id) {
        backend
            .engine
            .mark_folder_channel_owner(&channel_id, &owner);
    } else {
        tracing::warn!(
            channel = %channel_id,
            "folder welcome sender unresolved; owner-managed commit policy not armed here"
        );
    }
    // Cross-nest: remember the channel's home nest so a later content fetch relays
    // its request there (same-nest ⇒ blank ⇒ local fetch).
    backend.record_channel_home(channel_id, home_nest_url);
    // Cross-nest: durably record the foreign-set membership in this member's own
    // folder-key custody (`ForeignFolder` — identity + home-nest routing + display name;
    // Phase 2 client read-side). The member's own nest holds NO row for a
    // foreign set, so this record is what makes it listable and routes its
    // reads. Same-nest (blank `home_nest_url`) records nothing — the set
    // resolves via the member-visible roster. Best-effort like the custody
    // ingest below: a persist failure must not fail the join (the RAM-side
    // `channel_home` above already routes this session; a re-accept re-records).
    if !home_nest_url.is_empty()
        && let Some(sink) = backend.folder_custody.get()
    {
        let raw_group_id = backend
            .engine
            .group_id_bytes(&channel_id)
            .unwrap_or_default();
        if !sink
            .record_foreign_set(fauna_core::data::ForeignFolder {
                channel_id: channel_id.0,
                mls_group_id: raw_group_id,
                home_nest_url: home_nest_url.to_string(),
                set_name: set_name.map(str::to_string),
                // Advisory-only, nest-resolved on the set's HOME nest and
                // carried on the Welcome relay — what lets this client decide
                // whether to OFFER a folder binding. Never an authz input
                // (`federation.md` § Cross-nest → Recipient-side access
                // discovery); `None` from a non-conforming relay ⇒ reader.
                access: access.map(str::to_string),
                // The home nest's deployment identity (byte-plane SPKI-pin trust
                // root) + the owner-chosen cadence, both carried on the same relay.
                home_nest_actor_id: home_nest_actor_id.map(str::to_string),
                // The Welcome carries no floor; the first federated content-key
                // read stamps it (`custody::refresh_foreign_set_from_reply`).
                content_key_floor: None,
                // The cross-nest owner label, recorded only as the verified
                // pair this member's own nest forwarded (`federation.md`
                // § … *The cross-nest owner label*, join rule) — a bare handle
                // is a local user and never names a foreign set's owner.
                owner_handle: owner_label.as_ref().map(|(h, _)| h.clone()),
                owner_domain: owner_label.map(|(_, d)| d),
                ..Default::default()
            })
            .await
        {
            tracing::debug!(
                channel = %channel_id,
                "foreign-set record persist failed (re-recorded on next accept)"
            );
        }
    }
    // Member custody ingest (Phase 0 — the read leg): the join just put us at the
    // group's current epoch, so fetch + open + merge the owner's content-key
    // envelope now so this member can decrypt the set's content. Best-effort — a
    // not-yet-published or transient failure never fails the join; the folder
    // commit poll retries (design D2/D4). A fresh join always fetches
    // (`epoch_advanced = true`): the roster just changed, so the envelope was
    // re-published.
    backend.maybe_ingest_folder_custody(&channel_id, true).await;
    // A scrubbed cross-nest Welcome named the set only sealed, so the record
    // above is nameless — and a nameless foreign set has no engine binding and
    // no label. The ingest just put the set's content keys in custody (the
    // owner publishes the envelope before delivering the Welcome), so open the
    // seal now. Best-effort and gain-only, like the record itself.
    if !home_nest_url.is_empty()
        && set_name.is_none()
        && let Some(seal) = welcome_ctx.set_name_seal.as_ref()
        && let Some(sink) = backend.folder_custody.get()
        && !sink
            .name_foreign_set_from_seal(&channel_id.0, &seal.sealed, &seal.name_hash)
            .await
    {
        tracing::debug!(
            channel = %channel_id,
            "foreign set left nameless: its sealed name did not open (re-opened on next accept)"
        );
    }
    // Rule 3 on the spent init key, as at every Welcome join (`ingest_welcome`);
    // last, so the flushed provider carries the owner marker stamped above.
    backend
        .persist_provider_after_join("folder welcome-join")
        .await;
    Ok(channel_id)
}

/// **Leave** a joined cross-user shared folder — the reverse of
/// [`join_folder_welcome`], driving the recipient's `folder-leave-button`
/// (`docs/goal/ui/folders.md` § Sharing: "a `folder-leave-button` … to remove
/// yourself"). Addressed by the raw MLS `group_id` (hex) the member holds in their
/// B3 member-visible `FolderSummary` — the same value the nest-side
/// `fauna.folders.leave` self-drop takes — so a single input drives both halves.
/// Locally forgets the group ([`FaunaMlsBackend::forget_folder_channel`]), so the
/// set drops from the `has_group`-filtered list. Idempotent. The caller pairs this
/// with the nest roster self-drop (`FoldersClient::leave`) — off the roster, the
/// leaver stops receiving content-key rotations. A voluntary leave does **not**
/// rotate the owner's content key (`mls-group-key-material.md` § M2). Returns the
/// forgotten `ChannelId`.
pub fn leave_folder(
    backend: &FaunaMlsBackend,
    group_id_hex: &str,
) -> Result<ChannelId, BackendError> {
    let raw_group_id = hex::decode(group_id_hex.trim())
        .ok()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| {
            BackendError::Internal("leave folder: group_id must be non-empty hex".into())
        })?;
    let channel_id = ChannelId::from_group_id(&raw_group_id);
    backend.forget_folder_channel(channel_id)?;
    Ok(channel_id)
}

/// Drain a **scheduling** channel: page its log via `channel_fetch`, decode +
/// MLS-decrypt each [`ChannelEnvelope::Application`], and hand every
/// `ChannelMessageBody::Scheduling` payload (the raw RFC 5322 iMIP) to `sink` for
/// calendar-apply (`docs/goal/behavior/caldav-server.md` § Server-side
/// auto-schedule, Half-1). The scheduling twin of [`poll_inbound_conv`]: same
/// fetch + decode + decrypt + cursor machinery, but a scheduling channel binds no
/// thread, so the decrypted body routes to the [`SchedulingSink`] (CalDAV apply)
/// rather than a conversation. Non-`Scheduling` bodies and membership `Commit`s
/// are ignored — a one-off organizer→attendee channel carries only the iMIP.
///
/// `after_seq` is the monotonic paging cursor, advanced past every record seen
/// (exactly like [`poll_inbound_conv`]); a sink failure is **logged, not fatal**
/// (it can't stall the feed), and because the organizer delivers each iMIP on its
/// own fresh one-off channel a genuinely lost apply is re-delivered by the
/// organizer's next send, not by re-reading this drained channel. Returns the
/// number of iMIPs handed to the sink successfully.
pub async fn poll_inbound_scheduling(
    backend: &FaunaMlsBackend,
    sink: &dyn SchedulingSink,
    channel_id: &ChannelId,
    after_seq: &mut i64,
    page_limit: i64,
) -> Result<usize, BackendError> {
    // ⚠ A retired engine must not walk this log AT ALL, and the refusal has to
    // land here rather than at the `decrypt` below. The walk advances
    // `after_seq` past a record BEFORE decrypting it and skips every decrypt
    // failure with `continue` (correctly — an own message cannot be decrypted
    // by its sender) — and the session writes the advanced cursor to the
    // durable cross-device watermark. So an engine that merely failed each
    // decrypt would silently skip every record it walked, permanently, on every
    // device that later resumes from that watermark: strictly worse than the
    // ghost it replaces. Refusing before the cursor moves leaves the records on
    // the nest log for the successor, which holds the retire-point snapshot and
    // can still decrypt them.
    if backend.engine.is_retired() {
        return Err(BackendError::Internal(
            "the conversations-engine role was handed over — this engine must not \
             walk the inbound log (its cursor would advance past records it cannot \
             decrypt)"
                .into(),
        ));
    }
    let channel_hex = channel_id.to_string();
    let mut applied = 0usize;
    let fetch_limit = wire_fetch_limit(page_limit);
    loop {
        let page_started_at = *after_seq;
        let entries = backend
            .rpc
            .channel_fetch(
                channel_hex.clone(),
                *after_seq,
                fetch_limit,
                backend.channel_home_url(channel_id),
            )
            .await?;
        if entries.is_empty() {
            break;
        }
        // A scheduling channel is a one-off organizer→attendee iMIP delivery, not a
        // conversation surface — a legal takedown tombstones only chat threads, and
        // a withheld (empty) envelope here simply fails the decode below and is
        // skipped like any undecodable record.
        for crate::backend::FetchedRecord {
            seq,
            envelope,
            author,
            ..
        } in entries
        {
            if seq > *after_seq {
                *after_seq = seq;
            }
            // Only Application envelopes carry an iMIP; an undecodable record or a
            // membership Commit is skipped (a scheduling channel sees no commits).
            let Ok(ChannelEnvelope::Application(ct)) = ChannelEnvelope::from_bytes(&envelope)
            else {
                continue;
            };
            let Ok(cm) = backend.engine.decrypt(channel_id, &ct) else {
                // Own message (the organizer can't decrypt its own) or a real
                // decrypt failure — skip, don't stall the feed.
                continue;
            };
            // `cm.sender` is deliberately NOT the origin: the MDA gateway signs
            // with a per-delivery ephemeral identity, so on this rail the MLS
            // credential names nobody. What the sender cannot forge is the pair
            // below — the home nest's attested author + the home this client's
            // own nest stamped (`caldav-server.md` § Who may mutate an existing
            // event over the inbound rail).
            let ChannelMessageBody::Scheduling(imip) = cm.body else {
                continue; // a scheduling channel only carries Scheduling bodies
            };
            let origin = crate::backend::SchedulingOrigin {
                author,
                home_nest_url: backend.channel_home_url(channel_id).unwrap_or_default(),
            };
            match sink.apply_scheduling_imip(imip, origin).await {
                Ok(()) => applied += 1,
                Err(e) => tracing::error!("scheduling apply on {channel_hex}: {e}"),
            }
        }
        // Page until an EMPTY page (a short page does not mean drained — the
        // nest closes pages early on the frame budget, and an
        // already-deployed nest clamps `limit: 0` to one record); guard
        // against a no-progress page like [`poll_inbound_conv`]. Unlike the chat
        // and folder rails, this poll returns only a count — its outcome feeds
        // neither an arm-2 resumed-pending clear nor a gated-send
        // `catch_up_after` baseline — so a `break` here is a benign early stop
        // (retried next tick), with no `stalled` signal to misclassify.
        if *after_seq <= page_started_at {
            tracing::error!(
                channel = %channel_id,
                cursor = *after_seq,
                "scheduling fetch page advanced no cursor — stopping the walk"
            );
            break;
        }
    }
    Ok(applied)
}

/// Route one decrypted message to the [`ShareEndpointsSink`] IF it is a
/// share-set endpoint advertisement; answer whether it was one. Shared by
/// [`poll_inbound_conv`] and [`poll_inbound_folder`] — advertisements ride
/// the set's OWN channel (a folder channel for a shared set; the tier_1
/// carriage test drives a conversation-shaped channel), and the two rails
/// routing differently is exactly how the folder rail shipped skipping them
/// (found by the first app-level two-actor run, 2026-08-19).
///
/// Same never-a-bubble handling, same never-stall rule as the custody arm.
/// Its own tally because `uncaptured` means something different here — the
/// sink REFUSED an advertisement that did not bind to its channel-proven
/// sender, which is a member lying about who it is, not a CAS hiccup.
/// Unlike custody there is no re-drive owed: a lost advertisement costs
/// only peer candidates, and the nest-mediated path (the contract's
/// always-on source) is untouched.
async fn route_share_endpoints(
    backend: &FaunaMlsBackend,
    channel_hex: &str,
    cm: &ChannelMessage,
) -> bool {
    let ChannelMessageBody::ShareEndpoints(bytes) = &cm.body else {
        // Not an advertisement — a folder channel carries custody payloads and
        // receipts through this same door, so this is the common case and NOT a
        // fault. It is logged at trace only because the alternative is what bit
        // the linux journey's share-sink debugging: every exit below is a
        // silent counter, so a decrypted peer record that never reaches the
        // sink could leave by any of three doors and say nothing.
        tracing::trace!(
            channel = %channel_hex,
            "share sink: decrypted record is not an advertisement; not routed"
        );
        return false;
    };
    use std::sync::atomic::Ordering::Relaxed;
    let tally = &backend.share_endpoints;
    tally.seen.fetch_add(1, Relaxed);
    match backend.share_endpoints_sink() {
        None => {
            tally.no_sink.fetch_add(1, Relaxed);
            // The whole plane is inert on this seat, and until 2026-08-24 it
            // said so nowhere: the counter is read by tier_1 tests only, so at
            // tier_3 an unwired sink and a healthy-but-quiet plane were the
            // same observation. WARN, not debug — an advertisement crossed the
            // wire, was decrypted, and is being dropped on the floor.
            tracing::warn!(
                channel = %channel_hex,
                "share sink: an advertisement arrived but NO SINK is registered on this session —                  the peer-transfer plane cannot bind a dial row for this set"
            );
        }
        Some(sink) => {
            if sink.share_endpoints(channel_hex, cm.sender, bytes).await {
                tally.captured.fetch_add(1, Relaxed);
                tracing::debug!(
                    channel = %channel_hex,
                    "share sink: advertisement captured; a dial row is bound for this set"
                );
            } else {
                tally.uncaptured.fetch_add(1, Relaxed);
                tracing::debug!(
                    channel = %channel_hex,
                    "share sink: advertisement REFUSED by the sink (it did not bind to a                      channel-proven sender, or the write did not land)"
                );
            }
        }
    }
    true
}

/// The result of one [`poll_inbound_folder`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderPollOutcome {
    /// Commits that actually advanced this device's epoch.
    pub applied: usize,
    /// The walk stopped **before** a commit the local group has not incorporated
    /// and this pass could not heal ([`CommitApplyOutcome::Stalled`] — an
    /// own-leaf commit whose resync failed, or a future-epoch strand). The
    /// cursor was left pointing before that record (Rule-2 "round toward the
    /// safe side"), so the next pass retries it; a gated-send catch-up seeing
    /// `stalled` knows its `expect_no_commit_since` still sits before an
    /// unincorporated commit and its rebase cannot safely land.
    pub stalled: bool,
}

/// Drive the **folder commit poll** for one channel: page the channel log and
/// apply every membership `Commit` envelope to the MLS ratchet — the remaining
/// members' **epoch-advance liveness** for shared folders (5d(d),
/// `mls-group-key-material.md` § Rotate-on-removal). When the owner removes a
/// member, the orchestration posts the Remove commit to the set's channel
/// (`FoldersAuthor::drive_removal`); this poll is how every *remaining* member
/// advances to the post-removal epoch and can open the re-published content-key
/// envelope. The folder twin of [`poll_inbound_scheduling`], but commit-only:
/// a folder channel carries no chat/iMIP traffic, so `Application` envelopes
/// (and undecodable records) are skipped. Commit application is the same shared
/// dispatch as the chat rail ([`apply_inbound_commit`] — idempotent, past-epoch
/// commits quiet-skip, so re-walking a log from `after_seq = 0` is safe).
///
/// **The cursor never advances past an unincorporated commit** (a
/// [`CommitApplyOutcome::Stalled`] record): the walk stops there and reports
/// [`FolderPollOutcome::stalled`]. This is what makes the seq this poll returns
/// safe to use as a gated-send `expect_no_commit_since` baseline — advancing
/// past a stalled own-leaf commit would let a rebased commit be accepted for an
/// epoch other members already left, a permanent fork (`devices.md` § Cross-device
/// MLS group-state sync, Rules 1–2). For the background feed the stop is equally
/// right: everything after an unincorporated epoch transition is
/// future-epoch-unprocessable anyway, and the next tick retries the heal.
pub async fn poll_inbound_folder(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    after_seq: &mut i64,
    page_limit: i64,
) -> Result<FolderPollOutcome, BackendError> {
    // ⚠ A retired engine must not walk this log AT ALL, and the refusal has to
    // land here rather than at the `decrypt` below. The walk advances
    // `after_seq` past a record BEFORE decrypting it and skips every decrypt
    // failure with `continue` (correctly — an own message cannot be decrypted
    // by its sender) — and the session writes the advanced cursor to the
    // durable cross-device watermark. So an engine that merely failed each
    // decrypt would silently skip every record it walked, permanently, on every
    // device that later resumes from that watermark: strictly worse than the
    // ghost it replaces. Refusing before the cursor moves leaves the records on
    // the nest log for the successor, which holds the retire-point snapshot and
    // can still decrypt them.
    if backend.engine.is_retired() {
        return Err(BackendError::Internal(
            "the conversations-engine role was handed over — this engine must not \
             walk the inbound log (its cursor would advance past records it cannot \
             decrypt)"
                .into(),
        ));
    }
    // A statement parked by an earlier launch rests with the engine; this
    // launch re-walks from 0 and cannot re-read it (its decrypt consumed the
    // ratchet generation), so the first walk takes it back into the park
    // before any record — the hold below then fires as it did then.
    backend.load_rested_folder_park(channel_id);
    let channel_hex = channel_id.to_string();
    let mut applied = 0usize;
    let fetch_limit = wire_fetch_limit(page_limit);
    loop {
        let page_started_at = *after_seq;
        let entries = backend
            .rpc
            .channel_fetch(
                channel_hex.clone(),
                *after_seq,
                fetch_limit,
                backend.channel_home_url(channel_id),
            )
            .await?;
        if entries.is_empty() {
            break;
        }
        for crate::backend::FetchedRecord { seq, envelope, .. } in entries {
            // Membership commits AND share-endpoint advertisements matter on a
            // folder channel; every other Application body, undecodable or
            // withheld record is skipped without stalling the feed.
            match ChannelEnvelope::from_bytes(&envelope) {
                Ok(ChannelEnvelope::Commit(cb)) => {
                    // The hold behind a parked succession statement — the
                    // folder commit walk inherits the harvest wait
                    // (`federation.md` § Cross-nest shared folders + channel
                    // append → *The marker follows the owner's verified
                    // succession*). A statement the witness refused for now
                    // names this channel's recorded owner, and the commit in
                    // front of us may be the successor's remove-old: applied
                    // now it meets a marker still naming the predecessor, is
                    // refused by the folder commit policy and MEMOIZED — the
                    // decrypt consumed the committer's ratchet generation — so
                    // the re-drive that later re-stamps the marker could never
                    // re-admit it, and this seat forks from the group's epoch.
                    // So: re-ask the witness once (an anchor may have landed),
                    // and if the statement still refuses while the sweep has
                    // not yet spoken for the owner, stop BEFORE this record —
                    // nothing decrypted, nothing memoized, cursor behind it,
                    // the next pass retries. Bounded by the sweep's settle of
                    // that owner (whom `harvest_anchor_wants` puts on its
                    // walk), never a clock, and never in a session with no
                    // sweep: a forged statement can hold this rail exactly as
                    // long as the seat has not yet independently learned the
                    // owner's chain head this session, once.
                    if redrive_parked_folder_in_channel(backend, channel_id).await {
                        backend
                            .succession_statements
                            .held_commits
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::info!(
                            channel = %channel_id,
                            seq,
                            "folder walk: commit held behind a parked succession statement until the \
                             harvest settles the channel's recorded owner"
                        );
                        return Ok(FolderPollOutcome {
                            applied,
                            stalled: true,
                        });
                    }
                    match apply_inbound_commit(backend, channel_id, &cb).await {
                        CommitApplyOutcome::Advanced => {
                            applied += 1;
                            // This channel's roster moved: a succession statement
                            // parked on it may settle now (the folder twin of the
                            // conversations rail's commit re-drive; the hold
                            // verdict is moot after a fold).
                            redrive_parked_folder_in_channel(backend, channel_id).await;
                        }
                        CommitApplyOutcome::Skipped => {}
                        // Stop BEFORE this record — the cursor stays behind it, so
                        // the next pass (or the next rebase round's catch-up)
                        // retries the heal instead of silently consuming an epoch
                        // transition the local group never incorporated. No Rule-2
                        // rewind on this rail (future-epoch or not): the folder
                        // cursor is RAM-only, seeded to 0 each launch (`session.rs`
                        // `fs_cursors`), so a restart already re-walks from 0 — an
                        // in-session future-epoch here means an un-processable
                        // hole, which a rewind cannot fill.
                        CommitApplyOutcome::Stalled { .. } => {
                            return Ok(FolderPollOutcome {
                                applied,
                                stalled: true,
                            });
                        }
                    }
                }
                // The set's own discovery rail (`send_share_endpoints` posts
                // here) and the owner's succession statement: decrypt and
                // route those two bodies — a folder channel carries no chat,
                // so every other decrypted body drops. A failed decrypt is an
                // own message (a sender
                // cannot decrypt its own application posts) or a
                // ratchet-consumed record from a previous session's 0-seeded
                // re-walk — both skip; the dial rows a past session bound are
                // durable in the account store.
                Ok(ChannelEnvelope::Application(ct)) => {
                    match backend.engine.decrypt(channel_id, &ct) {
                        Ok(cm) => {
                            // Two bodies matter on a folder channel: the set's
                            // share-endpoint advertisement, and the owner's
                            // in-group succession statement, which re-points
                            // this seat's folder-owner marker
                            // (`route_folder_succession`). Every other
                            // decrypted body drops.
                            if !route_share_endpoints(backend, &channel_hex, &cm).await {
                                route_folder_succession(backend, channel_id, &cm).await;
                            }
                        }
                        // The skip is CORRECT for the two benign cases the arm
                        // was written for, but it must still NAME itself: this
                        // is the only door a peer's share-endpoint
                        // advertisement enters by, and until 2026-08-24 a
                        // failed decrypt was dropped with no log at all — so an
                        // own message, a ratchet-consumed re-walk, and a
                        // genuine epoch/ratchet desync were the SAME
                        // observation from outside.The linux journey
                        // sends its advertisements successfully, the receiving
                        // walk reads every record, and the sink still reports
                        // "no cached dial target" forever, with nothing
                        // anywhere saying why. Debug, not warn: on a healthy
                        // run this fires once per record this seat itself
                        // posted, which is ordinary weather — the error text is
                        // what separates that from a real desync.
                        Err(e) => tracing::debug!(
                            channel = %channel_id,
                            seq,
                            "folder walk: application record not opened, so nothing routed to the share sink                              (own post, a consumed ratchet, or a genuine epoch desync — the cause is in the error): {e}"
                        ),
                    }
                }
                _ => {}
            }
            if seq > *after_seq {
                *after_seq = seq;
            }
        }
        // Page until an EMPTY page (a short page does not mean drained — the
        // nest closes pages early on the frame budget, and an
        // already-deployed nest clamps `limit: 0` to one record); guard
        // against a no-progress page like [`poll_inbound_conv`].
        if *after_seq <= page_started_at {
            // Server-contract violation (non-advancing non-empty page): the walk
            // is INCOMPLETE, so report `stalled: true` — a gated-send
            // `catch_up_after` must NOT take an `expect_no_commit_since` baseline
            // from a truncated walk (same class as
            // `poll_inbound_conv`'s guard). This rail has no arm-2 clear, only the
            // gated-send baseline consumer.
            tracing::error!(
                channel = %channel_id,
                cursor = *after_seq,
                "folder fetch page advanced no cursor — stopping the walk as INCOMPLETE (stalled)"
            );
            return Ok(FolderPollOutcome {
                applied,
                stalled: true,
            });
        }
    }
    // Member custody ingest (Phase 0 — the read leg), on a CLEAN walk only. A
    // stalled walk returned earlier and deliberately skips this: ingesting at an
    // un-incorporated epoch would just fail the open (design D4), and the next
    // pass retries once the heal lands. Refresh custody always when a commit
    // advanced this member's epoch (the owner re-published the envelope), else
    // only when custody is not yet ingested this session (the D2 retry). No-op
    // when no custody sink is registered.
    backend
        .maybe_ingest_folder_custody(channel_id, applied >= 1)
        .await;
    Ok(FolderPollOutcome {
        applied,
        stalled: false,
    })
}

/// Whether this pass should fetch + open the content-key envelope, given
/// whether the poll advanced this member's epoch and whether the channel is
/// currently marked as holding **current** custody.
///
/// The D2 saving is the `false, true, false` case alone: a quiet poll for a
/// member whose custody is already current skips the network entirely.
/// Everything else attempts — including a quiet poll for a member whose *last*
/// attempt failed, which is the retry the rotation leg depends on
/// (see [`FaunaMlsBackend::maybe_ingest_folder_custody`]), and one the sink
/// says is re-owed ([`FolderCustodySink::refetch_owed`]): the owner re-minted
/// the set's nonce without advancing the epoch
/// (`writer-signed-change-records.md` ruling (11)(b)).
fn should_attempt_custody_ingest(
    epoch_advanced: bool,
    holds_current_custody: bool,
    refetch_owed: bool,
) -> bool {
    epoch_advanced || !holds_current_custody || refetch_owed
}

/// Record one ingest attempt's outcome in the "holds current custody" set.
///
/// `merged` — and only `merged` — marks the channel. A non-merge **clears** it,
/// which is what leaves the next quiet poll eligible to retry. Pure so the
/// invariant is pinned over the real `HashSet` without standing up an MLS
/// engine (the fetch/open half needs one; this bookkeeping half is the part
/// that regressed).
fn apply_custody_ingest_outcome(
    holds_current: &mut HashSet<ChannelId>,
    channel_id: &ChannelId,
    merged: bool,
) {
    if merged {
        holds_current.insert(*channel_id);
    } else {
        holds_current.remove(channel_id);
    }
}

/// Extract the Fauna [`ActorId`] from a membership-change target address. The
/// fauna-native MLS rail only ever names Fauna actors; a non-Fauna address is a
/// caller error (the manager routes by rail, so this is a guard, not a path).
/// The signed policy a floor read carries, **verified** for the room
/// `channel_id` — or `None` for a policy-less room, from a reply that omits the
/// field, or when the record does not verify, which to a reader are all the
/// same thing: no policy to stand behind. The one decoder every consumer of a
/// floor's policy bytes goes through.
///
/// A version above 1 whose room signature is missing or another room's is
/// refused. A floor read serves no chain, so this cannot tell whether the
/// version follows the room's own; what it answers only names the version to
/// walk to: the anchored chain ([`fauna_mls::room_policy::CommunityPolicyChain`]) is
/// what a device renders, amends ([`FaunaMlsBackend::anchored_room_policy`])
/// or grants a rank off.
fn verified_room_policy(
    floor: &crate::backend::RoomFloor,
    channel_id: &ChannelId,
) -> Option<fauna_mls::room_policy::SignedRoomPolicy> {
    let bytes = floor.policy.as_ref()?;
    let signed =
        fauna_core::encoding::canonical_decode::<fauna_mls::room_policy::SignedRoomPolicy>(bytes)
            .ok()?;
    signed
        .verify_community_for(&fauna_mls::room_policy::RoomBinding::new(channel_id.0))
        .ok()?;
    Some(signed)
}

/// The labeler set a floor serves, **verified**: it decodes, its signature
/// verifies under the signer it names, and it names THIS room — a set an admin
/// of two rooms signed for the other one must never render here, and the
/// signed room id is what makes lifting it detectable.
///
/// The record binds no policy version, so the signer's rank *at signing* is
/// the nest's to have checked (it refuses a set whose signer is not the owner
/// or an admin on the floor) and cannot be re-checked here; a set stays in
/// force after its signer loses the rank, as a policy version does.
fn verified_room_labelers(
    bytes: &[u8],
    channel_id: &ChannelId,
) -> Option<fauna_mls::room_policy::SignedRoomLabelers> {
    let signed =
        fauna_core::encoding::canonical_decode::<fauna_mls::room_policy::SignedRoomLabelers>(bytes)
            .ok()?;
    signed.verify_signature().ok()?;
    (signed.labelers.room_id.as_slice() == channel_id.0.as_slice()).then_some(signed)
}

/// What a floor says reads the room, as `RoomSnapshot::labelers` renders it:
/// the verified set's ids (lowercase hex); an empty set for a community room
/// that names none; `None` for a floor that seats no home nest (no nest reads,
/// so none labels) and for a stored set that does not verify.
fn floor_labelers(
    floor: &crate::backend::RoomFloor,
    channel_id: &ChannelId,
) -> Option<Vec<String>> {
    if !floor
        .members
        .iter()
        .any(|m| m.kind == RoomPrincipalKind::Nest)
    {
        return None;
    }
    match floor.labelers.as_deref() {
        None => Some(Vec::new()),
        Some(bytes) => Some(
            verified_room_labelers(bytes, channel_id)?
                .labelers
                .labelers
                .iter()
                .map(ActorId::to_hex)
                .collect(),
        ),
    }
}

fn fauna_actor(addr: &TypedAddress) -> Result<ActorId, BackendError> {
    match addr {
        TypedAddress::Fauna { actor_id, .. } => Ok(*actor_id),
        other => Err(BackendError::Internal(format!(
            "FaunaMls membership change needs a Fauna address, got {other:?}"
        ))),
    }
}

/// Annotate a failure that landed **after** `add_participant`'s phantom heal
/// already evicted the ghost leaf, so the message says what state the group is
/// actually in. A pass-through when `healed` is false.
///
/// The result is a [`BackendError::Refusal`] — a **product statement**, which is
/// what this sentence has always been: it tells the user the group is now
/// internally consistent and a plain retry takes the ordinary fresh-add path.
/// Two things changed when the send-slot taxonomy was ratified
/// (`conversations.md` § Errors & edge cases) and this composition had not
/// caught up: the sentence was **raw English**, violating § Architectural
/// rules 3 on an element rendered by all 7 apps, and it embedded `{e}` —
/// `Display`, which keeps variant tags and raw diagnostic payloads for logs and
/// is forbidden on any user surface. The underlying reason now rides
/// `user_detail()`, so a product statement survives and a diagnostic degrades to
/// the generic sentence instead of leaking.
///
/// It no longer re-wraps `Transport`. That variant's payload is the *seam's*
/// user string by [`ConvRpcError`](crate::backend::ConvRpcError)'s contract, so
/// a hand-built one here was always outside the contract; the retryability the
/// old comment defended was never routing (no consumer branches on the variant)
/// — it is carried by the sentence itself, which says to add them again.
fn evict_orphan_context(healed: bool, e: BackendError) -> BackendError {
    if !healed {
        return e;
    }
    // The raw error is the log's business, not the user's.
    tracing::warn!("re-invitation after a phantom-leaf heal failed: {e:?}");
    BackendError::Refusal(
        fauna_i18n::strings::conversations::unified::error_add_participant_after_heal(
            &e.user_detail(),
        ),
    )
}

/// Parse a lowercase-hex channel id back into a [`ChannelId`] (`None` on bad
/// length / non-hex).
fn parse_channel_hex(channel_id_hex: &str) -> Option<ChannelId> {
    ChannelId::from_hex(channel_id_hex).ok()
}

/// Parse a lowercase-hex 32-byte BLAKE3 digest into a [`ContentHash`] (the raw
/// blake3-256 CID). Used to lift a [`crate::backend::ResolvedAttachment`]'s
/// hex `blob_hash` (the manager's content handle) into the typed reference the
/// channel message carries. `None` on bad length / non-hex.
fn parse_content_hash_hex(hex_str: &str) -> Option<ContentHash> {
    fauna_core::hex32::decode(hex_str)
        .ok()
        .map(ContentHash::from_digest_raw)
}

/// Parse a typed recipient string as a Fauna [`ActorId`] (64 lowercase/uppercase
/// hex chars = the 32-byte actor key), or `None` if it isn't that shape. Used by
/// [`FaunaMlsBackend::resolve_address`]'s actor-id resolution form: a 64-hex
/// string carries the actor key verbatim, so it needs no handle→actor lookup.
fn parse_actor_id_hex(raw: &str) -> Option<ActorId> {
    ActorId::from_hex(raw).ok()
}

// The typed-handle parser behind [`FaunaMlsBackend::resolve_address`]'s
// handle→actor form lives in `fauna_core::resolve` since the public-folder
// follow (`fauna_client_folders::follow_ops`) started running the same chain —
// one parser, one same-nest-vs-cross-nest decision (priority #2).
use fauna_core::resolve::parse_fauna_handle;

/// The lower-cased domain of a canonical `localpart@domain` Fauna handle;
/// `None` for a bare handle (no `@`, e.g. a self address before identity
/// resolution) or junk. The key of the *known Fauna domain* set.
fn handle_domain(handle: &str) -> Option<String> {
    parse_fauna_handle(handle).and_then(|(_, d)| d.map(|d| d.to_ascii_lowercase()))
}

/// Which key opens a fetched attachment blob — the one point where the two
/// classes' receive paths differ (`community-rooms.md` § The three classes →
/// *Attachments — the second content kind*).
#[derive(Clone, Copy)]
enum AttachmentOpener<'a> {
    /// End-to-end: the channel's MLS blob key at the attachment's `epoch`
    /// ([`MlsEngine::open_conversation_blob`], grace-decrypt aware).
    MlsEpoch,
    /// Community: the room's attachment content kind off the generation the
    /// naming message was sealed under — the `RoomSealed` envelope's, which the
    /// author's signature binds; the attachment's own `epoch` is unread.
    Room {
        key: &'a GenerationKey,
        generation: &'a [u8; 32],
    },
}

/// Fetch, open and cache every attachment a message names, returning the
/// rendered snapshots — the receive twin of the two classes' seal + upload
/// (`encode_body` / `send_room_message`), shared so the two rails cannot
/// drift. For each [`ChannelAttachment`]: GET the sealed blob by `sealed_cid`
/// from the channel's home nest ([`crate::backend::ConversationsRpc::blob_get`]),
/// open it with `opener`, verify + cache the plaintext under the uniform
/// `blob_hash` so the bubble loads it via `ConversationsManager::attachment_bytes`,
/// and build the [`AttachmentSnapshot`]. A blob that is absent, fails to open
/// or lies about its hash is skipped — the bubble still shows its siblings; one
/// bad blob cannot stall the feed.
///
/// The walk is bounded by the reader itself, because the list is sealed and
/// its author's alone — neither the nest's per-record pin nor any send-time
/// check can see it ([`fauna_core::attachment_limits`];
/// `conversation-rooms.md` § The home nest → *The reader bounds what it
/// fetches*). At most [`MAX_ATTACHMENTS_PER_RECORD`] entries are walked; an
/// entry declaring more than [`INLINE_BLOB_BODY_LIMIT`] bytes is skipped
/// unfetched, and a fetched blob over it unopened, since no nest door accepts
/// one that large; and a plaintext that opens to a length other than its
/// declared `size_bytes` is a failed open.
async fn fetch_open_cache_attachments(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    channel_id: &ChannelId,
    channel_hex: &str,
    attachments: &[ChannelAttachment],
    opener: AttachmentOpener<'_>,
) -> Vec<AttachmentSnapshot> {
    let walked = if attachments.len() > MAX_ATTACHMENTS_PER_RECORD {
        tracing::warn!(
            named = attachments.len(),
            walked = MAX_ATTACHMENTS_PER_RECORD,
            "conv message names more attachments than a record may pin; walking the first"
        );
        &attachments[..MAX_ATTACHMENTS_PER_RECORD]
    } else {
        attachments
    };
    let mut snaps = Vec::with_capacity(walked.len());
    for att in walked {
        let sealed_cid_hex = hex::encode(att.sealed_cid.digest());
        let AttachmentFetch::Opened(plaintext) = fetch_and_open_attachment(
            backend,
            channel_id,
            channel_hex,
            &sealed_cid_hex,
            att.size_bytes,
            att.epoch,
            opener,
        )
        .await
        else {
            continue;
        };
        let blob_hash_hex = hex::encode(att.blob_hash.digest());
        // Probe before the move below — `detect_c2pa` reads the decrypted
        // bytes directly (the sender already stripped/sealed them, so there's
        // nothing to re-process here; `fauna_media::process::detect_c2pa` is
        // the probe-only half of the upload-side pipeline). Native builds get
        // a real reading via the `c2pa-detect` feature; the wasm32 stub always
        // answers `false` (`docs/goal/ui/conversations.md` § Attachments
        // "C2PA on-device").
        let c2pa = fauna_media::process::detect_c2pa(&att.mime_type, &plaintext);
        // `att.blob_hash` is SENDER-AUTHORED — it rides inside the sealed
        // `ChannelAttachment`, so opening the blob proves the sender is a member,
        // never that the handle names these bytes. An unverified key would let any
        // co-member overwrite that hash's bytes for every conversation at once
        // (one shared store), and swap a victim's staged outgoing file before
        // `send` re-resolves it. The door verifies; a lying attachment is skipped
        // whole — no snapshot either, so no bubble is left pointing at a handle
        // that resolves to nothing.
        if !manager.cache_attachment_bytes_checked(&blob_hash_hex, plaintext) {
            tracing::warn!(
                declared = %blob_hash_hex,
                filename = %att.filename,
                "conv attachment's bytes do not match its declared blob_hash; skipping"
            );
            continue;
        }
        // The store is a bounded cache (`conversations.md` § Attachments →
        // *Retention*): remember where these bytes rest and which key opens
        // them, so a render that misses them after eviction has them fetched
        // again by `refill_evicted_attachments` — the same fetch as above.
        manager.remember_attachment_coordinates(
            blob_hash_hex.clone(),
            AttachmentCoordinates::FaunaMls {
                channel: *channel_id,
                blob: SealedBlobCoordinates {
                    sealed_cid_hex,
                    size_bytes: att.size_bytes,
                    key: match opener {
                        AttachmentOpener::MlsEpoch => {
                            AttachmentOpeningKey::MlsEpoch { epoch: att.epoch }
                        }
                        AttachmentOpener::Room { generation, .. } => {
                            AttachmentOpeningKey::RoomGeneration {
                                generation: *generation,
                            }
                        }
                    },
                },
            },
        );
        snaps.push(AttachmentSnapshot {
            blob_hash: blob_hash_hex,
            filename: att.filename.clone(),
            mime_type: att.mime_type.clone(),
            size_bytes: att.size_bytes,
            is_image: att.is_image,
            c2pa,
        });
    }
    snaps
}

/// What one bounded fetch of a sealed attachment blob came to.
enum AttachmentFetch {
    /// Fetched, opened, and the plaintext is the declared size.
    Opened(Vec<u8>),
    /// Not coming back this way: absent on the nest, over the door's ceiling,
    /// failed to open, or opened to a different size than declared. A refill
    /// forgets the handle on this; the first receive skips the attachment.
    Gone,
    /// The read itself failed (transport). Worth asking again later.
    Transient,
}

/// Fetch one sealed attachment blob from where it rests — the channel's home
/// nest, a foreign home reached direct (`conversation-rooms.md` § The home
/// nest → *Attachment bytes*) — and open it with `opener`. The one bounded
/// fetch every FaunaMls attachment goes through, on first receive
/// ([`fetch_open_cache_attachments`]) and on a refill after eviction
/// ([`refill_evicted_attachments`]) alike, so the reader's bounds cannot drift
/// between the two: a `declared_size` over [`INLINE_BLOB_BODY_LIMIT`] is
/// refused before a byte is read, a fetched blob over it is refused unopened,
/// and a plaintext whose length is not `declared_size` is a failed open
/// (`conversation-rooms.md` § The home nest → *The reader bounds what it
/// fetches*). `epoch` is read by the MLS opener only.
async fn fetch_and_open_attachment(
    backend: &FaunaMlsBackend,
    channel_id: &ChannelId,
    channel_hex: &str,
    sealed_cid_hex: &str,
    declared_size: u64,
    epoch: u64,
    opener: AttachmentOpener<'_>,
) -> AttachmentFetch {
    if declared_size > INLINE_BLOB_BODY_LIMIT as u64 {
        tracing::warn!(
            declared = declared_size,
            "conv attachment declares more bytes than any blob door accepts; skipping unfetched"
        );
        return AttachmentFetch::Gone;
    }
    let sealed = match backend
        .rpc
        .blob_get(
            channel_hex.to_string(),
            backend.channel_home_url(channel_id),
            sealed_cid_hex.to_string(),
        )
        .await
    {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            tracing::warn!("conv attachment blob absent on nest; skipping that attachment");
            return AttachmentFetch::Gone;
        }
        Err(e) => {
            tracing::warn!(error = %e, "conv attachment blob fetch failed; skipping");
            return AttachmentFetch::Transient;
        }
    };
    if sealed.len() > INLINE_BLOB_BODY_LIMIT {
        tracing::warn!(
            fetched = sealed.len(),
            "conv attachment blob is larger than any blob door accepts; skipping unopened"
        );
        return AttachmentFetch::Gone;
    }
    let opened = match opener {
        AttachmentOpener::MlsEpoch => backend
            .engine
            .open_conversation_blob(channel_id, epoch, &sealed)
            .map_err(|e| e.to_string()),
        AttachmentOpener::Room { key, generation } => {
            fauna_mls::room_message::open_room_attachment(key, generation, &sealed)
                .map_err(|e| e.to_string())
        }
    };
    let plaintext = match opened {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "conv attachment decrypt failed; skipping");
            return AttachmentFetch::Gone;
        }
    };
    // The declared size is what every app shows beside the file; a
    // plaintext that disagrees with it is treated as a failed open.
    if plaintext.len() as u64 != declared_size {
        tracing::warn!(
            declared = declared_size,
            opened = plaintext.len(),
            "conv attachment opened to a different size than it declares; skipping"
        );
        return AttachmentFetch::Gone;
    }
    AttachmentFetch::Opened(plaintext)
}

/// Fetch again every attachment a render has missed since the last receive
/// cycle — the refill half of the attachment store's retention rule
/// (`docs/goal/ui/conversations.md` § Attachments → *Retention*;
/// `crate::store::attachments`). The store holds at most its budget and evicts
/// the least recently read plaintext; the bytes still rest on the room's home
/// nest, and the receive loop — or, for the sender's own attachments, the send
/// — remembered where (`ConversationsManager::remember_attachment_coordinates`),
/// so a miss is
/// repaired by repeating exactly the first receive's bounded fetch
/// ([`fetch_and_open_attachment`]), opened under the same key — the channel's
/// MLS blob key at the attachment's epoch, or the room's attachment content
/// kind off the naming generation ([`FaunaMlsBackend::room_generation_key`],
/// cached from the walk that first opened the message). Verified and cached
/// through the same door as any inbound attachment, then observers are
/// notified once so the render that missed asks again and hits.
///
/// Runs once per receive cycle, after the walk (native `conv_sweep!` /
/// `poll_conversations`, wasm `pollConversations`). A handle whose bytes are
/// gone for good — absent on the nest, or no longer openable — is forgotten,
/// so it renders declared (filename + size) from then on rather than being
/// asked for every cycle; a transport failure keeps it remembered for the next
/// render's ask. Returns how many attachments were cached again.
pub async fn refill_evicted_attachments(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
) -> usize {
    // This rail's wants only — a mail attachment's are the mail sweep's
    // (`backends::smtp::refill_evicted_mail_attachments`).
    let wanted = manager.take_wanted_sealed_blob_attachments();
    if wanted.is_empty() {
        return 0;
    }
    let mut refilled = 0usize;
    for (blob_hash, channel, coords) in wanted {
        let channel_hex = channel.to_string();
        let room_key = match &coords.key {
            AttachmentOpeningKey::MlsEpoch { .. } => None,
            // A key a newer build recorded: this build cannot open the blob.
            // Skipped, and its coordinates KEPT — they ride the next slice
            // re-upload for a build that can (an unknown arm never deletes).
            AttachmentOpeningKey::Unknown(_) => continue,
            AttachmentOpeningKey::RoomGeneration { generation } => {
                match backend.room_generation_key(&channel, generation).await {
                    RoomKeyLookup::Key(key) => Some(key),
                    // Could not read the room's key material this pass; the
                    // next render's miss asks again.
                    RoomKeyLookup::NotYet => continue,
                    // This device cannot open that generation — final.
                    _ => {
                        manager.forget_attachment_coordinates(&blob_hash);
                        continue;
                    }
                }
            }
        };
        let (opener, epoch) = match (&coords.key, &room_key) {
            (AttachmentOpeningKey::RoomGeneration { generation }, Some(key)) => {
                (AttachmentOpener::Room { key, generation }, 0)
            }
            (AttachmentOpeningKey::MlsEpoch { epoch }, _) => (AttachmentOpener::MlsEpoch, *epoch),
            // Unreachable by construction (a room key is always looked up
            // above); treated as gone rather than panicking a receive loop.
            _ => {
                manager.forget_attachment_coordinates(&blob_hash);
                continue;
            }
        };
        match fetch_and_open_attachment(
            backend,
            &channel,
            &channel_hex,
            &coords.sealed_cid_hex,
            coords.size_bytes,
            epoch,
            opener,
        )
        .await
        {
            AttachmentFetch::Opened(plaintext) => {
                if manager.cache_attachment_bytes_checked(&blob_hash, plaintext) {
                    refilled += 1;
                } else {
                    manager.forget_attachment_coordinates(&blob_hash);
                }
            }
            AttachmentFetch::Gone => manager.forget_attachment_coordinates(&blob_hash),
            AttachmentFetch::Transient => {}
        }
    }
    if refilled > 0 {
        manager.notify();
    }
    refilled
}

/// Map a decrypted attachment-bearing [`ChannelMessage`] onto a
/// [`RailInboundMessage`], fetching + opening + caching each attachment blob
/// along the way — the receive twin of [`FaunaMlsBackend::encode_body`]
/// (`docs/goal/ui/conversations.md` § Attachments). For each
/// [`ChannelAttachment`]: GET the sealed blob by `sealed_cid`
/// ([`crate::backend::ConversationsRpc::blob_get`]), open it under the message's
/// epoch blob key ([`MlsEngine::open_conversation_blob`]), cache the plaintext
/// under the uniform `blob_hash` so the bubble loads it via
/// `ConversationsManager::attachment_bytes`, and build the rendered
/// [`AttachmentSnapshot`]. A blob that's absent or fails to open is skipped (the
/// bubble still shows its siblings; one bad blob can't stall the feed). Returns
/// `None` only for a non-attachment body (a guard — the caller pre-checks).
/// The fetch/open/cache loop itself is [`fetch_open_cache_attachments`], shared
/// with the community class's `RoomSealed` arm.
async fn attachments_to_inbound(
    backend: &FaunaMlsBackend,
    manager: &ConversationsManager,
    channel_id: &ChannelId,
    channel_hex: &str,
    cm: &ChannelMessage,
    seq: i64,
    plane_ref: Option<crate::message::PlaneRef>,
) -> Option<RailInboundMessage> {
    let ChannelMessageBody::Attachments { body, attachments } = &cm.body else {
        return None;
    };
    let snaps = fetch_open_cache_attachments(
        backend,
        manager,
        channel_id,
        channel_hex,
        attachments,
        AttachmentOpener::MlsEpoch,
    )
    .await;
    Some(RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Fauna {
            handle: String::new(),
            actor_id: cm.sender,
        },
        recipients: Vec::new(),
        subject: None,
        body: body.clone(),
        // FaunaMls is markdown-capable, so the caption renders formatted (matches
        // the `Text` path + the sender's "Sent" copy).
        body_format: BodyFormat::Markdown,
        timestamp_ms: (cm.timestamp.0 / 1000) as i64,
        message_id: MessageId(format!("conv:{channel_hex}:{seq}")),
        in_reply_to: None,
        attachments: snaps,
        badges: MessageBadges::default(),
        legal_takedown_ref: None,
        plane_ref,
    })
}

/// Build a **legal-takedown tombstone** inbound for a withheld record
/// (`moderation.md` § Categories & enforcement item 1). The nest withheld the
/// sealed envelope (empty) under a legal obligation, so there is no plaintext and
/// **no attributable sender** (conv records persist none — the nest cannot
/// attribute a sealed E2E message to an author post-hoc, `moderation.md`
/// § Implementation status today), hence the zero [`ActorId`] and empty body: the
/// message carries only `legal_takedown_ref`, and every app renders the shared
/// `legalTakedownTombstone(reference)` in place of the bubble. It keeps its `seq`
/// slot via the `conv:<channel>:<seq>` id (the store append/dedup preserves order
/// and drops it if the real message was already synced).
fn legal_takedown_inbound(channel_hex: &str, seq: i64, reference: String) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Fauna {
            handle: String::new(),
            actor_id: ActorId([0u8; 32]),
        },
        recipients: Vec::new(),
        subject: None,
        body: String::new(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: Timestamp::now_millis() as i64,
        message_id: MessageId(format!("conv:{channel_hex}:{seq}")),
        in_reply_to: None,
        attachments: Vec::new(),
        badges: MessageBadges::default(),
        legal_takedown_ref: Some(reference),
        // No plane ref, and not for want of one: the nest withheld the sealed
        // envelope, so this record has no body to hand to a visible view. A
        // tombstone must never report a T1 observation — there is nothing
        // observed.
        plane_ref: None,
    }
}

/// What one application append produced: the nest-allocated `seq`, and the
/// record's account-data-plane identity derived from it.
struct AppendedRecord {
    seq: i64,
    /// `None` only if the channel id is not 32 hex bytes, which a bound channel
    /// never is (`crate::plane::plane_ref`).
    plane_ref: Option<crate::message::PlaneRef>,
}

/// Map a decrypted [`ChannelMessage`] onto a [`RailInboundMessage`]. Returns
/// `None` for non-text bodies (attachments / device-sync / group-meta) —
/// attachments are handled by [`attachments_to_inbound`]; the rest in later
/// slices. The sender's handle isn't carried in the ciphertext, so it's
/// left empty (the app-level `sender` ActorId is authoritative).
fn channel_message_to_inbound(
    cm: &ChannelMessage,
    channel_hex: &str,
    seq: i64,
    plane_ref: Option<crate::message::PlaneRef>,
) -> Option<RailInboundMessage> {
    let body = match &cm.body {
        ChannelMessageBody::Text(t) => t.clone(),
        _ => return None,
    };
    Some(RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Fauna {
            handle: String::new(),
            actor_id: cm.sender,
        },
        recipients: Vec::new(),
        subject: None,
        body,
        // FaunaMls is markdown-capable (`capabilities.supports_markdown`), so a
        // received conversation message is markdown — flag it so the bubble
        // renders it formatted, matching the sender's "Sent" copy
        // (`conversations.md` § Layout — `dm-message-text` rendered per
        // `body_format`).
        body_format: BodyFormat::Markdown,
        // `ChannelMessage.timestamp` is microseconds (`fauna_core::Timestamp`).
        timestamp_ms: (cm.timestamp.0 / 1000) as i64,
        message_id: MessageId(format!("conv:{channel_hex}:{seq}")),
        in_reply_to: None,
        attachments: Vec::new(),
        badges: MessageBadges::default(),
        legal_takedown_ref: None,
        plane_ref,
    })
}

/// A second live copy of a generation key.
///
/// [`GenerationKey`] is deliberately **not** `Clone` — key material should not
/// multiply by accident — and that constraint is worth keeping for the whole
/// codebase, so the room-generation cache spells its one legitimate copy out
/// here rather than weakening the type. Both copies are `Zeroizing`, so the
/// bytes are still wiped on drop; what is given up is only the *count* of live
/// copies, which for a per-channel cache the backend owns is exactly the point.
fn copy_generation_key(key: &GenerationKey) -> GenerationKey {
    GenerationKey::from_bytes(*key.as_bytes())
}

/// The conversations plane's answer to "which key opens this room's post"
/// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*): this
/// backend alone holds both key models — an end-to-end room's MLS group, and a
/// community member's generation wraps — so the feed asks it through
/// [`fauna_core::room_post::RoomPostKeys`] instead of holding either.
///
/// The class is read off what this device holds, not guessed: a channel this
/// engine keeps an MLS group for is an end-to-end room, and a community room is
/// exactly the class whose members keep no group.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl fauna_core::room_post::RoomPostKeys for FaunaMlsBackend {
    async fn room_post_seal_key(
        &self,
        room: [u8; 32],
    ) -> Result<
        (
            fauna_core::room_post::RoomPostSeal,
            zeroize::Zeroizing<[u8; 32]>,
        ),
        String,
    > {
        use fauna_core::room_post::RoomPostSeal;
        let channel = ChannelId(room);
        if self.engine.has_group(&channel) {
            let (epoch, secret) = self
                .engine
                .export_room_post_secret(&channel)
                .map_err(|e| format!("this room's epoch secret: {e}"))?;
            return Ok((
                RoomPostSeal::EndToEnd { epoch },
                zeroize::Zeroizing::new(secret),
            ));
        }
        // Always the tip, read now (the trait's rule): a post sealed under a
        // generation the room rotated past would reach the member the rotation
        // was for.
        let (generation, key) = self.room_tip_generation(&channel).await.ok_or_else(|| {
            "this device holds no key for this room's current generation".to_string()
        })?;
        Ok((
            RoomPostSeal::Community { generation },
            zeroize::Zeroizing::new(fauna_core::group_content::room_post_base_key(&key)),
        ))
    }

    async fn room_post_base_key(
        &self,
        room: [u8; 32],
        seal: fauna_core::room_post::RoomPostSeal,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
        use fauna_core::room_post::RoomPostSeal;
        let channel = ChannelId(room);
        match seal {
            RoomPostSeal::EndToEnd { epoch } => self
                .engine
                .room_post_secret_at(&channel, epoch)
                .map(zeroize::Zeroizing::new)
                .map_err(|e| format!("this room's epoch secret: {e}")),
            RoomPostSeal::Community { generation } => {
                match self.room_generation_key(&channel, &generation).await {
                    RoomKeyLookup::Key(key) => Ok(zeroize::Zeroizing::new(
                        fauna_core::group_content::room_post_base_key(&key),
                    )),
                    RoomKeyLookup::NotForUs => Err(
                        "this device was never keyed into the generation this post names".into(),
                    ),
                    RoomKeyLookup::NotYet => {
                        Err("this device is not keyed into this room yet".into())
                    }
                    RoomKeyLookup::Unkeyable => Err("this device holds no room keys".into()),
                }
            }
        }
    }

    /// The room's recorded home, from the same `ChannelHome` map that routes
    /// `channel.fetch`, `room.generations_remote` and `room.list_roster_remote`
    /// — one home for the routing signal, so a room post's verdict read cannot
    /// go to a different nest than the log it was derived from.
    async fn room_home_nest_url(&self, room: [u8; 32]) -> Option<String> {
        self.channel_home_url(&ChannelId(room))
    }
}

/// Fold one **verified** community-room message into the shared inbound shape
/// — the `RoomSealed` twin of [`channel_message_to_inbound`] and of
/// [`attachments_to_inbound`], and deliberately the same shape: past the open,
/// a community room's bubble and an end-to-end room's are the same thing. A
/// `Text` body is the bubble's text; an `Attachments` body is its caption, and
/// `attachments` are the snapshots the caller already fetched, opened and
/// cached ([`fetch_open_cache_attachments`]) — empty for a text message.
///
/// Two fields come from the *signed core* rather than from where the MLS twin
/// reads them, and both differences are the class's:
///
/// - **`sender`** is the signed `author`, not an MLS-authenticated leaf. The
///   caller must have verified it ([`fauna_mls::room_message::open_room_message`]
///   is the only door that produces a [`SignedRoomMessage`] worth folding);
///   this function trusts its argument exactly as its twin trusts `cm.sender`.
/// - **`timestamp_ms`** is the author's own signed stamp in **milliseconds**
///   — `ChannelMessage.timestamp` is microseconds and its twin divides, so a
///   copied `/ 1000` here would date every community bubble to 1970.
fn room_message_to_inbound(
    signed: &fauna_mls::room_message::SignedRoomMessage,
    channel_hex: &str,
    seq: i64,
    plane_ref: Option<crate::message::PlaneRef>,
    attachments: Vec<AttachmentSnapshot>,
) -> Option<RailInboundMessage> {
    let body = match &signed.core.body {
        ChannelMessageBody::Text(t) => t.clone(),
        ChannelMessageBody::Attachments { body, .. } => body.clone(),
        _ => return None,
    };
    Some(RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Fauna {
            handle: String::new(),
            actor_id: signed.core.author,
        },
        recipients: Vec::new(),
        subject: None,
        body,
        body_format: BodyFormat::Markdown,
        timestamp_ms: signed.core.sent_at_ms,
        message_id: MessageId(format!("conv:{channel_hex}:{seq}")),
        in_reply_to: None,
        attachments,
        badges: MessageBadges::default(),
        legal_takedown_ref: None,
        plane_ref,
    })
}

#[cfg(test)]
mod custody_ingest_retry_tests {
    use super::*;

    fn channel() -> ChannelId {
        ChannelId([7u8; 32])
    }

    /// The regression this pair exists to prevent: a member that ingested at
    /// join, then failed a rotation-commit ingest, must keep retrying on the
    /// ordinary quiet polls.
    ///
    /// Until 2026-07-24 the mark was only ever *inserted*, so after the failed
    /// attempt it stayed `true` and `should_attempt_custody_ingest(false, true, false)`
    /// short-circuited every later pass — the member never fetched again and
    /// sat on the pre-rotation generation until an app restart, with every
    /// subsequent owner upload failing closed in `content_open_roots`. The
    /// rotate-on-removal window that triggers it cannot be closed by ordering
    /// (the post-rotation epoch does not exist until the Remove commit merges),
    /// so this retry is the mechanism, not a safety net.
    #[test]
    fn a_failed_rotation_ingest_leaves_the_next_quiet_poll_retrying() {
        let ch = channel();
        let mut held: HashSet<ChannelId> = HashSet::new();

        // Join-time ingest succeeds.
        apply_custody_ingest_outcome(&mut held, &ch, true);
        assert!(
            !should_attempt_custody_ingest(false, held.contains(&ch), false),
            "a quiet poll with current custody must skip the network (design D2)"
        );

        // The owner's Remove commit advances the epoch, so this pass attempts…
        assert!(should_attempt_custody_ingest(
            true,
            held.contains(&ch),
            false
        ));
        // …and fails: the member polled before the owner re-published the
        // rotated envelope.
        apply_custody_ingest_outcome(&mut held, &ch, false);

        assert!(
            should_attempt_custody_ingest(false, held.contains(&ch), false),
            "after a FAILED ingest the next quiet poll must retry — this is the \
             rotation leg's only recovery path, and marking on success alone \
             made it unreachable"
        );

        // The retry lands once the owner has published.
        apply_custody_ingest_outcome(&mut held, &ch, true);
        assert!(!should_attempt_custody_ingest(
            false,
            held.contains(&ch),
            false
        ));
    }

    /// A member who has never ingested attempts on every pass (the pre-existing
    /// D2 arm), so the fix adds no new fetch class — a failed ingest simply
    /// rejoins the arm that was already retrying.
    #[test]
    fn a_member_without_custody_attempts_on_every_pass() {
        assert!(should_attempt_custody_ingest(false, false, false));
        assert!(should_attempt_custody_ingest(true, false, false));
    }

    /// `writer-signed-change-records.md` ruling (11)(b): a re-owed fetch
    /// attempts on a quiet poll over custody the last attempt merged — the
    /// one case neither an epoch advance nor a failed attempt reaches.
    #[test]
    fn a_reowed_fetch_attempts_on_a_quiet_poll_over_current_custody() {
        assert!(should_attempt_custody_ingest(false, true, true));
    }
}
