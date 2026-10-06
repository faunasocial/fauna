//! Store-plane value types (charter: `account-data-plane.md` § Store logical
//! schema).
//!
//! Everything here is wasm-safe and key-less by construction (R7 (account-data-plane.md § The ratified decisions)): value and
//! block bytes are opaque `Vec<u8>`/CIDs — nothing in this module can demand
//! key material.

use anyhow::{Context, Result, bail};
use fauna_core::data::ContentHash;
use fauna_core::format::hex_full;
use std::collections::BTreeMap;

/// A writer's identity on the plane: a device principal's public key, or the
/// nest's key — the 32-byte writer key of the frontier vector (charter § The
/// frontier vector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WriterId(pub [u8; 32]);

impl WriterId {
    /// The reserved slot for **the nest that sequences a content scope**.
    ///
    /// A content scope is one-writer by construction: record order is the
    /// nest's, never a device's (charter § Feeds and cursors → *Multi-writer
    /// fit*: "a `__conv` channel scope keeps the home nest as its sequencing
    /// writer for content … so member replicas consume it as a one-writer
    /// feed"), and the same holds for the own-actor kinds, whose records exist
    /// because the nest appended them to a segment.
    ///
    /// So the frontier needs a slot for that writer — but **not** its key: W2.3 (account-data-plane.md § Workstreams)
    /// ruled that the nest-writer cursor rides the shipped scalar `since` and
    /// therefore "needs no nest-key discovery client-side"
    /// (`account-data-plane.md` § Implementation status → W2.3 ruling (a)). A
    /// replica has exactly one home nest per scope, so naming the slot is all
    /// that is required, and this constant is that name.
    ///
    /// It is ASCII on purpose. This value lands in at-rest frontier and journal
    /// rows, where the next reader meets it as 32 hex bytes; spelling it out
    /// means a hex dump decodes to what it is instead of looking like a device
    /// key nobody can account for. (An all-zero id would be shorter and say
    /// nothing.) A device principal's id is an Ed25519 public key — a
    /// compressed curve point — so this padded label is not one.
    pub const NEST_SEQUENCER: Self = Self(*b"fauna:nest-sequencer\0\0\0\0\0\0\0\0\0\0\0\0");

    pub fn to_hex(self) -> String {
        hex_full(&self.0)
    }
}

/// A journal row's operation — the closed set of charter § Store logical
/// schema component 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalOp {
    /// Class 1: a record's canonical block landed (item = its CID).
    RecordAdded,
    /// Class 2: a state entry changed (item = kind-scoped key + entry version).
    StatePut,
    /// Either class: the item is deleted. Tombstone rows persist until every
    /// enrolled device's frontier passes them (compaction is W2's concern).
    Tombstone,
}

impl JournalOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordAdded => "record-added",
            Self::StatePut => "state-put",
            Self::Tombstone => "tombstone",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "record-added" => Self::RecordAdded,
            "state-put" => Self::StatePut,
            "tombstone" => Self::Tombstone,
            other => bail!("unknown journal op {other:?}"),
        })
    }
}

/// What a journal row points at.
///
/// The row stores this in one opaque column (`item_ref`) via
/// [`ItemRef::encode`] — a versioned tag-length encoding, not dag-cbor,
/// because it never leaves the store: the *wire* shape of a feed row is W2's
/// contract (charter § Feeds and cursors), while this is purely at-rest and
/// covered by the store's `format_version` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemRef {
    /// Class 1/3: the canonical block CID (fixed 36 bytes,
    /// `serialization.md` § CID shape).
    Cid(ContentHash),
    /// Class 2: the kind-scoped entry key plus the entry version this row
    /// transports.
    ///
    /// `entry_version` has one meaning per provenance, and both are the
    /// version *on the authoring writer's log*:
    ///
    /// - **Locally authored** ([`crate::store::AccountStore::put_state`]): this
    ///   store's own monotonic counter for `(kind, key)`.
    /// - **Ingested from another writer**
    ///   ([`crate::store::AccountStore::ingest_state`]): the origin's
    ///   `writer_seq`. The origin's own counter is not on the wire (T14's
    ///   cleartext floor is closed and does not carry it), and the feed keeps
    ///   at most one live row per `(item, writer)` — so that writer's seq *is*
    ///   the item's version on its log. Deriving it from the wire rather than
    ///   locally is what makes a re-ingest byte-identical, and therefore an
    ///   idempotent replay instead of a spurious equivocation refusal.
    StateKey {
        kind: String,
        key: String,
        entry_version: u64,
    },
}

const ITEM_REF_TAG_CID: u8 = 1;
const ITEM_REF_TAG_STATE_KEY: u8 = 2;

impl ItemRef {
    /// Stable at-rest encoding: one tag byte, then the variant's fields
    /// (length-prefixed where variable). Round-trip pinned by tests; evolving
    /// it is a store-format change (bump the meta version pair).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Cid(cid) => {
                let mut out = Vec::with_capacity(1 + 36);
                out.push(ITEM_REF_TAG_CID);
                out.extend_from_slice(cid.as_bytes());
                out
            }
            Self::StateKey {
                kind,
                key,
                entry_version,
            } => {
                let mut out = Vec::with_capacity(1 + 4 + kind.len() + 4 + key.len() + 8);
                out.push(ITEM_REF_TAG_STATE_KEY);
                out.extend_from_slice(&(kind.len() as u32).to_be_bytes());
                out.extend_from_slice(kind.as_bytes());
                out.extend_from_slice(&(key.len() as u32).to_be_bytes());
                out.extend_from_slice(key.as_bytes());
                out.extend_from_slice(&entry_version.to_be_bytes());
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (&tag, rest) = bytes
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("empty item_ref"))?;
        match tag {
            ITEM_REF_TAG_CID => {
                let arr: [u8; 36] = rest.try_into().map_err(|_| {
                    anyhow::anyhow!("cid item_ref: want 36 bytes, got {}", rest.len())
                })?;
                Ok(Self::Cid(ContentHash::from_bytes(arr)?))
            }
            ITEM_REF_TAG_STATE_KEY => {
                let (kind, rest) = take_lp_str(rest)
                    .ok_or_else(|| anyhow::anyhow!("state-key item_ref: truncated kind"))?;
                let (key, rest) = take_lp_str(rest)
                    .ok_or_else(|| anyhow::anyhow!("state-key item_ref: truncated key"))?;
                let arr: [u8; 8] = rest
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("state-key item_ref: truncated entry_version"))?;
                Ok(Self::StateKey {
                    kind,
                    key,
                    entry_version: u64::from_be_bytes(arr),
                })
            }
            other => bail!("unknown item_ref tag {other}"),
        }
    }
}

fn take_lp_str(bytes: &[u8]) -> Option<(String, &[u8])> {
    let (len, rest) = bytes.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    if rest.len() < len {
        return None;
    }
    let (s, rest) = rest.split_at(len);
    Some((String::from_utf8(s.to_vec()).ok()?, rest))
}

/// One journal row: `(writer_id, writer_seq, scope, op, item_ref)` — charter
/// § Store logical schema component 2. Rows are immutable once written;
/// `seq` is monotonic and gapless per *local* writer (other writers' rows are
/// stored verbatim and may legitimately arrive with gaps after the origin
/// compacted its own log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRow {
    pub writer: WriterId,
    pub seq: u64,
    pub scope: String,
    pub op: JournalOp,
    pub item: ItemRef,
}

/// The merged current value per `(kind, key)` — charter § Store logical
/// schema component 3. `value` is canonical dag-cbor bytes and MAY be sealed:
/// the store never inspects it (R7 key-less constraint); `merge_meta` is the
/// kind's merge-policy metadata, equally opaque at this layer (the
/// merge-policy seam dispatches on it at reading replicas, W2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEntry {
    pub kind: String,
    pub key: String,
    /// The scope whose feed carries this entry's `state-put` rows (the
    /// account-state scope in the charter's partition; kept explicit so the
    /// store never hard-codes scope naming ahead of W2).
    pub scope: String,
    pub value: Vec<u8>,
    pub merge_meta: Option<Vec<u8>>,
    /// Store-assigned, monotonic per `(kind, key)`; the version a `state-put`
    /// journal row transports.
    pub entry_version: u64,
    pub tombstone: bool,
}

/// One live **relay row** — a feed row this replica can serve to a peer
/// verbatim (W2.6, charter § The peer leg: *"verbatim relay of other writers'
/// rows correct under R6"*).
///
/// The state-entry plane opens incoming envelopes and keeps only the merged
/// plaintext per `(kind, key)` — which cannot reconstruct another writer's
/// *row* (the sealed envelope + its coordinates), so a replica that serves
/// peers retains the wire row itself here: one live row per
/// `(scope, writer, item)`, exactly the nest's own collapse, superseded in
/// place when the same writer re-puts the same item. Everything in it is
/// wire-derived and store-opaque: `entry` is the sealed T14 envelope
/// byte-for-byte (this layer never opens it — the same R7 posture as
/// [`StateEntry::value`]), so the relay plane is equally serveable by a
/// future key-less custodian (R7's third witness, W8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRow {
    /// The scope whose feed carries this row.
    pub scope: String,
    /// The wire item-class discriminator (`state-entry` today; `record-cid`
    /// when the content-scope relay lands).
    pub item_class: String,
    /// The authoring writer.
    pub writer: WriterId,
    /// The row's coordinate on that writer's log — what a peer's frontier
    /// slot accounts.
    pub writer_seq: u64,
    /// The opaque item key: the blinded class-2 item key (or a record CID
    /// digest for `record-cid` rows). The collapse key, with `scope` +
    /// `writer`.
    pub item_key: Vec<u8>,
    /// The cleartext op (`state-put` / `tombstone` wire strings) — the
    /// cleartext floor a key-less relay may read; the sealed envelope carries
    /// the authoritative tombstone marker.
    pub op: String,
    /// The sealed T14 envelope, verbatim. `None` for item classes that carry
    /// no inline entry.
    pub entry: Option<Vec<u8>>,
    /// The row's coordinate in the scope's one sequencing nest's log
    /// (`SyncChange.seq`) — the serve order the feed's retention gate and
    /// its `retirable_through_seq` watermark are expressed in, which the
    /// reclamation pass compares to withhold a retire the gate would refuse
    /// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, clause (1) → *the gate's watermark*). Known for a row
    /// walked off the nest feed and for an own row the nest's put reply
    /// stamped; `None` for a row that arrived over a store-served leg (no
    /// cross-writer order), for an own row still unpublished, and for every
    /// row recorded before the column existed — the pass then asks, as it
    /// always did. Additive at rest (a nullable column); never a collapse or
    /// ordering key.
    pub feed_seq: Option<u64>,
}

/// One retire a bound plane sent (`fauna.account.state.retire`), by its exact
/// coordinates and belt, with the answer it met — an entry of the store's
/// **retire record** (`account-sync-plane.md` § The bind leg, ruling 5).
///
/// Whichever process sent the retire records it; the secondary leg, in the
/// co-located runtime that holds the seed-leg role, reads the record,
/// re-issues each entry at every linked nest still listing the row, and clears
/// what it read. Derived state: an entry lost delays its retire at a secondary
/// until a later pass re-makes the entry, or leaves a redundant row there
/// (ruling 6 of the same section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedRetire {
    /// The scope the row lives in.
    pub scope: String,
    pub item_key: [u8; 32],
    pub writer: WriterId,
    pub writer_seq: u64,
    pub no_rows_sealed_under: Option<[u8; 32]>,
    pub delete_escrow_wraps: bool,
    /// The row is off the nest's feed now (`Retired` or already gone).
    pub settled: bool,
}

/// One row of the **record index** — the always-present layer of the block
/// plane (charter § Store logical schema component 1: "the always-present layer
/// is the index, not the blocks").
///
/// Every class-1 record this replica knows of has an index row, whether or not
/// its bytes are held. Presence is deliberately **not** a field here: it is
/// derived from the block plane ([`crate::store::AccountStore::is_present`]),
/// so an index row and the bytes it describes cannot drift out of agreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordIndexEntry {
    /// The record's identity — its canonical block CID. `record_id == block
    /// CID` (the shipped post identity: `post_id` *is* the `__post` segment
    /// block's CID).
    pub cid: ContentHash,
    /// The scope whose feed carried this record.
    pub scope: String,
    /// The record's kind — the axis hydration policy is expressed on.
    pub kind: String,
    /// The block's byte length when known. Known from the feed row even for a
    /// record whose bytes this replica has never held, which is what lets a
    /// dehydrated replica show sizes and budget a hydration pass.
    pub size: Option<u64>,
}

/// Whether a kind's record bytes are kept locally (charter § Store logical
/// schema component 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hydration {
    /// Bytes are kept — the default for small-record kinds (posts, calendar,
    /// card).
    Always,
    /// Bytes may be absent and fetched on demand — bulky-record kinds (mail
    /// with attachments) "may placeholder exactly like class-3".
    OnDemand,
}

impl Hydration {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::OnDemand => "on-demand",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "always" => Self::Always,
            "on-demand" => Self::OnDemand,
            other => bail!("unknown hydration {other:?}"),
        })
    }
}

/// This replica's hydration policy: a default plus per-kind overrides
/// (charter § Store logical schema component 4 — policy lives in store meta).
///
/// **The store holds this as data and never authors it.** Which kinds are
/// "bulky" is registry knowledge, and this crate's dependency floor
/// deliberately excludes `fauna-protocol` (see the crate docs); a layer that
/// knows the kind registry sets the policy, and the store enforces it. That
/// split is why the type is a plain map rather than a lookup function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationPolicy {
    pub default: Hydration,
    pub per_kind: BTreeMap<String, Hydration>,
}

impl Default for HydrationPolicy {
    /// Hydrate everything — the conservative default, since a replica that
    /// keeps bytes it did not need wastes disk, while one that drops bytes it
    /// did need cannot serve them offline.
    fn default() -> Self {
        Self {
            default: Hydration::Always,
            per_kind: BTreeMap::new(),
        }
    }
}

impl HydrationPolicy {
    pub fn for_kind(&self, kind: &str) -> Hydration {
        self.per_kind.get(kind).copied().unwrap_or(self.default)
    }

    /// Stable at-rest encoding for the `store_meta` row: one line per entry,
    /// `default` first. Line-oriented rather than dag-cbor because store meta
    /// is a plain KV of bytes and this keeps a hand-inspected DB readable.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("default={}\n", self.default.as_str());
        for (kind, h) in &self.per_kind {
            out.push_str(&format!("{kind}={}\n", h.as_str()));
        }
        out.into_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).context("hydration policy: not utf-8")?;
        let mut policy = HydrationPolicy {
            default: Hydration::Always,
            per_kind: BTreeMap::new(),
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let (name, value) = line
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("hydration policy: malformed line {line:?}"))?;
            let hydration = Hydration::parse(value)?;
            if name == "default" {
                policy.default = hydration;
            } else {
                policy.per_kind.insert(name.to_string(), hydration);
            }
        }
        Ok(policy)
    }
}

/// Physical outcome of writing one journal row at its `(writer, seq)`
/// coordinates. The logical layer maps this to semantics: a local append
/// retries past `OccupiedByDifferent` (another process of the same replica
/// won the seq race), while ingest treats it as equivocation and refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    /// An identical row already sits at these coordinates (idempotent replay).
    IdenticalPresent,
    /// A *different* row sits at these coordinates; nothing was written.
    OccupiedByDifferent,
    /// Only from the two entry-and-row writes
    /// ([`crate::backend::StoreBackend::state_put_with_row`] and its group
    /// twin): the entry is no longer at the version the caller read — another
    /// instance on the same store wrote it in between — so nothing was
    /// written. The caller re-reads the version and tries again.
    EntryMoved,
}

/// Which drain leg an outbox intent belongs to. Recorded at append because the
/// composer is the one that knows how its intent must leave the device — the
/// generic drain must not guess (an MLS compose drained as a raw RPC would
/// bypass sealing entirely).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentDrainer {
    /// The generic nest-RPC drain: replay `payload` as a request of `kind`
    /// whose envelope carries `intent_id` as the idempotency key.
    Rpc,
    /// The MLS drain-through-the-gated-send-path (`devices.md` § Offline
    /// compose): drains only from a process hosting the account's
    /// conversations engine, which seals at the then-current epoch. The
    /// generic drain lists these but never sends them.
    Mls,
}

impl IntentDrainer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Mls => "mls",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "rpc" => Self::Rpc,
            "mls" => Self::Mls,
            other => bail!("unknown intent drainer {other:?}"),
        })
    }
}

/// An outbox intent's durable state. Two values on purpose: a *drained*
/// intent has no state at all (completion-is-deletion), and retryable failure
/// is not a state — it is `retry_count` on a still-`Pending` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentStatus {
    /// Undrained; the drain may attempt it.
    Pending,
    /// Permanently failed — parked, user-visible, never re-attempted without
    /// user action. Parking one intent parks its whole scope's remainder
    /// behind it (FIFO is never reordered around a failure).
    Failed,
}

impl IntentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "failed" => Self::Failed,
            other => bail!("unknown intent status {other:?}"),
        })
    }
}

/// What a composer appends to the outbox: everything the drain needs to
/// replay the mutation later, identified by the client-generated idempotency
/// key. The store assigns `channel_seq` (per-scope FIFO position) and
/// `created_at` at append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxIntent {
    /// The client-generated idempotency key — the intent's identity here, on
    /// the wire envelope at drain, and in the nest's durable idempotency
    /// table. 16 bytes, matching the WS-RPC envelope field.
    pub intent_id: [u8; 16],
    /// The RPC kind the intent replays as.
    pub kind: String,
    /// The ordering channel — the scope the effect targets (a `conv` scope
    /// for a channel send; the account-state scope for account-plane kinds).
    /// FIFO holds within a scope, never across scopes.
    pub scope: String,
    /// The intent body: canonical dag-cbor of the request payload for `Rpc`
    /// intents; the plaintext compose for `Mls` intents (sealed only at
    /// drain — never pre-built ciphertext).
    pub payload: Vec<u8>,
    pub drainer: IntentDrainer,
}

/// One undrained outbox intent — charter § The offline-mutation contract (the
/// W4 phase-0 ruling: the outbox is its own durable component, holding
/// `OfflineQueued` intents only). An undrained intent is the only copy of the
/// user's pending write: **no store operation may destroy one** — the outbox
/// is deliberately outside every scope-keyed plane, so `drop_scope` cannot
/// reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxIntent {
    pub intent_id: [u8; 16],
    pub kind: String,
    pub scope: String,
    pub payload: Vec<u8>,
    pub drainer: IntentDrainer,
    /// Store-assigned per-scope FIFO position (append order).
    pub channel_seq: u64,
    pub status: IntentStatus,
    /// Attempts that did not conclude (the backoff pair's counter half —
    /// `transfer_queue` discipline).
    pub retry_count: u32,
    /// Seconds since epoch at append.
    pub created_at: u64,
    /// Seconds since epoch of the last inconclusive attempt (the backoff
    /// pair's stamp half); `None` before the first attempt.
    pub last_attempt_at: Option<u64>,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn cid() -> ContentHash {
        ContentHash::of_raw(b"account-store item-ref test block")
    }

    #[test]
    fn item_ref_cid_round_trips() {
        let r = ItemRef::Cid(cid());
        assert_eq!(ItemRef::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn item_ref_state_key_round_trips() {
        let r = ItemRef::StateKey {
            kind: "fauna.settings".into(),
            key: "notify/quiet-hours".into(),
            entry_version: 42,
        };
        assert_eq!(ItemRef::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn item_ref_decode_rejects_garbage() {
        assert!(ItemRef::decode(&[]).is_err());
        assert!(ItemRef::decode(&[9, 1, 2]).is_err());
        // truncated CID
        assert!(ItemRef::decode(&[ITEM_REF_TAG_CID, 0, 1, 2]).is_err());
        // truncated state-key length prefix
        assert!(ItemRef::decode(&[ITEM_REF_TAG_STATE_KEY, 0, 0]).is_err());
    }

    #[test]
    fn journal_op_round_trips() {
        for op in [
            JournalOp::RecordAdded,
            JournalOp::StatePut,
            JournalOp::Tombstone,
        ] {
            assert_eq!(JournalOp::parse(op.as_str()).unwrap(), op);
        }
        assert!(JournalOp::parse("frobnicate").is_err());
    }
}
