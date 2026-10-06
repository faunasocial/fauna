//! Per-channel conversation-history slice — the `history/<channel_hex>` path
//! family of the `__mls` reserved folder.
//!
//! Authority: `docs/goal/behavior/devices.md` § Cross-device MLS group-state
//! sync; at-rest shape `docs/goal/behavior/file-sync.md` § MLS state replica;
//! design tracked internally.
//!
//! [`ChannelHistorySlice`] carries what the nest-ordered log can NEVER give a
//! second device: the user's **own sent-message plaintext** (a sender cannot
//! MLS-decrypt its own application messages — `backends/fauna_mls.rs`
//! `poll_inbound_conv`), plus the **reaction and delete derived state** — the
//! folded aggregate on each message AND, beside it, the per-message reaction
//! event log and the tombstone set the live projection re-seeds from
//! ([`ChannelHistorySlice::reaction_log`],
//! [`ChannelHistorySlice::deleted_messages`], which say why the fold alone
//! will not do) — the per-channel ingest `watermark` (the highest channel
//! `seq` the slice reflects — informational: the resume cursor rides the
//! `provider` replica, and this is only its fallback for a `provider` blob
//! that holds no cursor yet — a device's first poll writes one), the fetch coordinates of the thread's
//! attachments (the resumed poll never re-walks the records that named them),
//! and a community room's **parked floor delete records**
//! ([`ChannelHistorySlice::parked_floor_deletes`] — walked past unjudged,
//! which the resumed poll likewise never re-walks).
//!
//! ⚠ The derived state is here because **stream replay cannot rebuild it**,
//! the reason the own plaintext is here: an own `Reaction`/`Delete` is
//! MLS-opaque to its own author off the log, and a restored device resumes
//! *past* every record it already folded. `ConversationsManager` is the only
//! writer that can fill it — the state is its memory, not the store's — so
//! [`ThreadStore::snapshot_channel_slice`] leaves the two fields empty and
//! `ConversationsManager::snapshot_channel_slice` fills them.
//!
//! Like `DraftsSnapshot`, the bytes here are canonical CBOR; the shared client
//! sync wrapper (slice 4) seals them under the owner's `BackupKey` before
//! upload. Compose state is deliberately **excluded** — the `__drafts` plane
//! owns it.
//!
//! Concurrent-device writes CAS on the nest; on conflict the client resolves
//! with the commutative [`merge_history_slices`] and retries: union by
//! `MessageId` (fauna-native ids are `conv:{channel_hex}:{seq}` — identical on
//! every device), same-id copies and the scalar fields taken from the
//! higher-watermark side (folds are deterministic replay of the seq-ordered
//! log, so the higher watermark's fold is a superset), watermark = max — with
//! the **monotone** state unioned rather than side-picked, because either
//! device may hold the only copy of a tombstone (the per-message `deleted`
//! flag and `deleted_messages`). The **reaction log** is unioned too, for a
//! different reason: its fold reads the SET of signed ops rather than the
//! sequence, so both devices' own reactions since the fork can survive
//! (`merge_reaction_state`, which also owns the one case that still cannot).

use crate::address::{Rail, TypedAddress};
use crate::keying::ThreadKey;
use crate::message::{MessageId, MessageSnapshot};
use crate::reactions::StampedReactionEvent;
use crate::store::attachments::SealedBlobCoordinates;
use crate::store::threads::ThreadStore;
use crate::thread::{ThreadFlavor, ThreadId};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The serialised at-rest form of one channel's conversation history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChannelHistorySlice {
    /// The MLS channel this slice belongs to (lowercase hex, the same form
    /// `ThreadKey::Channel` keys on).
    pub channel_id_hex: String,
    /// Thread display label (carries rename effects).
    pub label: String,
    pub flavor: ThreadFlavor,
    pub participants: Vec<TypedAddress>,
    /// All messages in thread order — **including own sent plaintext**.
    pub messages: Vec<MessageSnapshot>,
    /// Highest channel `seq` folded into this slice; the restoring device
    /// resumes log ingest from here.
    pub watermark: i64,
    /// The channel's **home nest**, as the writing device knew it — `Some` for
    /// a cross-nest channel homed elsewhere, `None` for a same-nest one.
    ///
    /// The routing datum, persisted. `FaunaMlsBackend::channel_home` is a RAM
    /// map learned from the cross-nest Welcome envelope, so before this field
    /// existed a relaunch left it empty and every consumer of
    /// `channel_home_url` — the send door and all three inbound drains — read a
    /// foreign-homed channel as same-nest: sends blackholed on the member's own
    /// nest, and the drain fetched from a nest holding none of the channel's
    /// records.
    ///
    /// ⚠ `None` here is disambiguated by [`Self::home_same_nest`], the total
    /// encoding this field alone could not provide: `None` + `home_same_nest`
    /// means "the writer KNEW this channel is same-nest"; `None` +
    /// `!home_same_nest` means "the writer recorded no home" (unknown) —
    /// `channel_is_same_nest` writes `false` whenever the `channel_home` map
    /// lacks an entry for the channel. The restore leaves it absent from the
    /// routing map, the declared residual. A
    /// re-delivered Welcome re-teaches the home (each Welcome path records it
    /// above its idempotency guard as of 2026-08-30), but an inbox Welcome is
    /// acked once drained, so an established channel is normally never offered
    /// one — closing that population needs a datum durable independently of
    /// this slice.
    ///
    /// For a **shared folder** that datum exists and the launch now spends it:
    /// the member's own `ForeignFolder` row names the set's home, and
    /// `FaunaMlsBackend::seed_channel_homes_from_custody` seeds the routing map
    /// from it after the restore's slice loop — filling only holes, so a slice
    /// that DID carry a home still outranks it. the folder-key custody holds no such row
    /// for a **conversations** channel, so that half of the
    /// channels with no recorded home remains reachable only through a total encoding of this
    /// field's absence.
    ///
    /// ⚠ **Both** writers of `history/<ch>` must stamp this — the Rule-3 flush
    /// (`persist_channel_history`) and the debounced replica autosave
    /// (`snapshot_replica`). [`ThreadStore::snapshot_channel_slice`] cannot: the
    /// store knows threads, not nests. Only the autosave runs for a member who
    /// joined by cross-nest Welcome and has since only *received*, so while it
    /// left this `None` the routing datum was written away as absent on current
    /// binaries.
    #[serde(default)]
    pub home_nest_url: Option<String>,
    /// The **explicit same-nest marker** — the second half of the routing
    /// datum's total encoding, and a security boundary. `true` iff the writing device KNEW the channel
    /// drains locally (`home_nest_url` then `None`); it disambiguates the two
    /// meanings of a `None` `home_nest_url` (see that field).
    ///
    /// Why it matters: on restore, a `home_same_nest` slice re-establishes the
    /// `FaunaMlsBackend::channel_home` `SameNest` marker, so the unauthenticated
    /// pre-guard write (a re-delivered Welcome, run before MLS auth) cannot
    /// re-home a restored **local** channel to a peer-declared URL. A slice
    /// written before this marker existed decodes `false` (`serde(default)`),
    /// restores as **absent** from the map, and remains fillable — the declared,
    /// bounded residual (`federation.md` § Cross-nest → route (a)); the nest-side
    /// verified-origin resolution bounds even that plant to the sender's own
    /// verified nest, and a peer that declares **no** origin at all now writes
    /// nothing rather than pinning the entry — the bound is
    /// a bound on a URL, so it had nothing to say about a blank. Never `true`
    /// alongside a `Some(home_nest_url)` — a channel is foreign-homed or
    /// same-nest, never both.
    ///
    /// ⚠ A slice with `home_same_nest: true` written while a blank pre-guard
    /// wrote `SameNest` instead of nothing (the ~ten-hour window, 2026-08-30/31) still round-trips through this field
    /// today and is not healed by the fix — see
    /// [`crate::backends::fauna_mls::FaunaMlsBackend::record_channel_home_if_absent`]'s
    /// doc for the full residual and why no migration is planned.
    #[serde(default)]
    pub home_same_nest: bool,
    /// Where each of this channel's attachments rests and which key opens it,
    /// keyed by `blob_hash` — the attachment store's FaunaMls coordinates for
    /// the messages above, so a device restored from this slice can fetch an
    /// attachment again on its first render instead of rendering it declared
    /// forever: the restored poll never re-walks the records that named them
    /// (`conversations.md` § Attachments → *Retention*).
    ///
    /// Carried here, keyed by handle, rather than on `MessageSnapshot` or
    /// `AttachmentSnapshot`: those are UniFFI records every app renders, and a
    /// fetch coordinate is something no app renders. The channel is implicit —
    /// it is this slice's — so a slice can never point a restoring device at
    /// another channel's blobs.
    ///
    /// ⚠ **Both** writers of `history/<ch>` include this through
    /// `ConversationsManager::snapshot_channel_slice`, which reads the
    /// manager's attachment store; [`ThreadStore::snapshot_channel_slice`]
    /// cannot (the store knows threads, not where bytes rest — the same
    /// reason the two routing fields above are stamped outside it). A slice
    /// re-sealed to a room newcomer carries none (`deliver_history_slice`
    /// strips it): the newcomer holds no key for the epochs these name.
    ///
    /// Additive at rest: omitted when empty, so a slice with no attachments
    /// encodes byte-identically to one written before the field existed, and
    /// such a slice decodes empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attachment_coordinates: BTreeMap<String, SealedBlobCoordinates>,
    /// The participants whose handle **the owner's own gesture** named — the
    /// only rows a succession's tier-2 anchor may dial, persisted so a restored
    /// device anchors the way the writing device did
    /// (`identity-succession.md` § The succession statement → *which
    /// participant handles anchor tier 2*; the store half is
    /// `ThreadStore`'s anchor-grade marks). A row NOT listed here that carries
    /// a handle was named by the room home or copied at seat time: display
    /// only, never a dial target. An empty list is a slice with no
    /// anchor-grade row at all.
    ///
    /// **Required at rest** — every writer records it (the snapshot stamps
    /// this device's marks, an empty list included), so a blob without it is
    /// not one this software wrote and fails to decode
    /// ([`HistoryRestoreError::Decode`]). The pre-provenance reading of an
    /// absent field (every named row anchor-grade) was removed by the
    /// compat-remnant sweep: no slice written before the field exists.
    pub anchor_grade_handles: Vec<ActorId>,
    /// Every message of this channel the writing device holds **tombstoned**,
    /// as its projection decided: its own delete set *union* every inbound
    /// claim the fold admitted (`conversation-rooms.md` § Roles and
    /// authorization → *Delete any message — the mechanism*). The outcome, not
    /// the claims — the verdict was recorded once, at fold, against the policy
    /// the delete was made under, and is never re-asked; persisting the claims
    /// instead would durably re-open a decision that is settled.
    ///
    /// ⚠ **Both this set and the `deleted` flag on each [`MessageSnapshot`] in
    /// [`Self::messages`] carry the tombstone, and both are needed.** The flag
    /// is what an app renders. The set is what the live projection re-seeds from, and it is
    /// the only one that survives a restore onto a **non-empty** store:
    /// [`ThreadStore::restore_channel_slice`] dedups by message id and keeps
    /// the local copy, so a device that already holds the message would drop
    /// the flag and with it the tombstone.
    ///
    /// Additive at rest (absent ⇒ empty), so a slice for a channel with no
    /// tombstone is byte-identical to one written before the field existed.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub deleted_messages: BTreeSet<MessageId>,
    /// The per-message reaction **event log** — each op with its author-signed
    /// stamp, the shape the fold orders by
    /// ([`crate::reactions::fold_reactions`]) — for the messages this slice
    /// carries.
    ///
    /// ⚠ **The aggregate alone is not enough, which is why the raw events ride
    /// here beside it.** `ConversationsManager::thread_detail` *overwrites*
    /// `MessageSnapshot.reactions` with a fold of its in-memory log whenever
    /// that log holds anything for the message, and `toggle_reaction` resolves
    /// Add-vs-Remove by folding the same log. So on a restored device the
    /// aggregate is load-bearing for exactly as long as nobody reacts: the
    /// first post-restore reaction would re-*Add* an emoji the user already
    /// holds (it has no per-actor state to retract from) and the fold of that
    /// one event would wipe every restored pill it did not itself produce.
    /// Restoring the events instead makes the projection's overwrite correct
    /// by construction — the log it folds is the whole log again.
    ///
    /// The stamp-less `reaction_events` projection once written beside this
    /// field — a downgrade mirror for a pre-stamp build — was retired by the
    /// compat-remnant sweep (`version-compatibility.md` § Dimension 2, program
    /// 4): it is neither written nor read.
    ///
    /// Additive at rest (absent ⇒ empty).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reaction_log: BTreeMap<MessageId, Vec<crate::reactions::StampedReactionEvent>>,
    /// The community room's floor delete records this device has walked past
    /// but **not judged yet** — parked for the policy version they name or a
    /// name a succession could still lift (`conversation-rooms.md` § Roles and
    /// authorization → *Delete any message — the mechanism* → *Members verify
    /// what they paint*). A claim awaiting its verdict, deliberately unlike
    /// [`Self::deleted_messages`], which is verdicts only: nothing here paints
    /// anything until the room backend judges it under a chain this device
    /// anchored itself, on the first pass after a restore.
    ///
    /// Here because the log will not hand them back: the inbound walk advances
    /// the durable cursor past a record BEFORE it is judged, so a record still
    /// parked at quit was, before this field, gone for good on that account —
    /// the target painted for ever on a device where every other member shows
    /// it deleted, and silently, since the room's own unverified-moderation
    /// notice is derived off the same parked set. The record's bytes are public
    /// (an unsealed, signed envelope), so persisting the claim costs no secret,
    /// and re-judging one another device already honoured only re-applies a
    /// tombstone it already carries in `deleted_messages`. Bounded like the
    /// live set ([`MAX_PARKED_FLOOR_DELETES`]), in log order.
    ///
    /// Additive at rest (absent ⇒ empty), so a slice for a room with nothing
    /// parked is byte-identical to one written before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parked_floor_deletes: Vec<ParkedFloorDelete>,
}

/// A community-room floor delete record waiting for the policy version it
/// names, or for a succession line that could still rank its author — the
/// room backend's parked set, live and at rest
/// ([`ChannelHistorySlice::parked_floor_deletes`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedFloorDelete {
    /// The record as the envelope carried it — verified for its signature and
    /// room before it was parked, and judged for its author's rank only under
    /// an anchored policy of the version it names.
    pub signed: fauna_mls::room_policy::SignedRoomFloorDelete,
    /// The record's own log position — [`crate::message::DeleteClaim::delete_seq`].
    pub delete_seq: Option<u64>,
}

/// The most floor delete records one room keeps parked, live or at rest. A
/// full room drops the oldest — which fails closed: that message stays painted.
pub const MAX_PARKED_FLOOR_DELETES: usize = 64;

/// Fold `more` into `held`, a parked set in log order: each record once (a
/// record is its own identity — two devices that parked the same envelope
/// hold equal bytes), and the oldest dropped past
/// [`MAX_PARKED_FLOOR_DELETES`]. The one union both the slice merge and a
/// restore onto a live session use, so neither can lose a record the other
/// side alone holds — a parked record is a tombstone that may yet paint, and
/// dropping it un-deletes for good.
pub fn union_parked_floor_deletes(
    held: &mut Vec<ParkedFloorDelete>,
    more: impl IntoIterator<Item = ParkedFloorDelete>,
) {
    for record in more {
        if !held.contains(&record) {
            held.push(record);
        }
    }
    held.sort_by_key(|record| record.delete_seq);
    if held.len() > MAX_PARKED_FLOOR_DELETES {
        held.drain(..held.len() - MAX_PARKED_FLOOR_DELETES);
    }
}

/// **The empty slice, and the shape every fixture builds on**. This type grows a field every few
/// months — four so far — and each growth used to mean editing every
/// hand-listed literal in the tree, which is both churn and a merge conflict
/// waiting for two branches that grow it independently. Fixtures name the
/// fields their assertion is about and take the rest from here.
///
/// ⚠ **The invariant: each defaulted field's value here MUST equal what
/// `serde` produces for that field when it is absent from a decoded blob** —
/// or, for a field required at rest (`anchor_grade_handles`), the value a
/// writer with nothing to record stamps (an empty list). So no fixture can
/// quietly assert against a shape no at-rest blob can have.
///
/// ⚠ **The two PRODUCTION constructors — [`ThreadStore::snapshot_channel_slice`]
/// and [`merge_history_slices`] — deliberately keep naming every field.** A
/// new field must be a compile error exactly where a real slice is written and
/// where two of them are reconciled, because those are the two places whose
/// answer is a decision (what does this device know? which side wins?) rather
/// than a blank. Only fixtures take the blank.
impl Default for ChannelHistorySlice {
    fn default() -> Self {
        Self {
            channel_id_hex: String::new(),
            label: String::new(),
            flavor: ThreadFlavor::OneToOne,
            participants: Vec::new(),
            messages: Vec::new(),
            watermark: 0,
            home_nest_url: None,
            home_same_nest: false,
            attachment_coordinates: BTreeMap::new(),
            anchor_grade_handles: Vec::new(),
            deleted_messages: BTreeSet::new(),
            reaction_log: BTreeMap::new(),
            parked_floor_deletes: Vec::new(),
        }
    }
}

/// A persisted history blob could not be restored. The caller treats this as
/// "no slice" and falls back to plain log replay from seq 0 (own-message
/// history is then absent until another device re-uploads) — a corrupt or
/// newer-shape blob must never crash the conversations surface.
#[derive(Debug, thiserror::Error)]
pub enum HistoryRestoreError {
    #[error("decode channel history slice: {0}")]
    Decode(String),
}

impl ChannelHistorySlice {
    /// Canonical CBOR bytes (byte-stable for equal logical state — message
    /// order is part of the state).
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        canonical_encode(self).map_err(|e| e.to_string())
    }

    /// Decode from [`Self::to_bytes`] output.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, HistoryRestoreError> {
        canonical_decode(bytes).map_err(|e| HistoryRestoreError::Decode(e.to_string()))
    }
}

impl ThreadStore {
    /// Capture the channel-bound thread `id` as a history slice. Returns
    /// `None` when the thread does not exist. `watermark` is supplied by the
    /// caller (the ingest loop owns the per-channel seq cursor). Compose state
    /// is not captured (the `__drafts` plane owns it).
    pub fn snapshot_channel_slice(
        &self,
        id: &ThreadId,
        channel_id_hex: &str,
        watermark: i64,
    ) -> Option<ChannelHistorySlice> {
        let detail = self.get(id)?;
        Some(ChannelHistorySlice {
            channel_id_hex: channel_id_hex.to_string(),
            label: detail.label,
            flavor: detail.flavor,
            participants: detail.participants,
            messages: detail.messages,
            watermark,
            // The store knows threads, not nests: the backend owns
            // `channel_home` and stamps both routing fields after the snapshot.
            home_nest_url: None,
            home_same_nest: false,
            // Nor where attachment bytes rest: the manager's attachment store
            // does, and `ConversationsManager::snapshot_channel_slice` fills this.
            attachment_coordinates: BTreeMap::new(),
            anchor_grade_handles: self.anchor_grade_actors(id),
            // Nor what the *projection* derived: tombstones and the reaction
            // event log are manager memory, and the store cannot see it.
            // `ConversationsManager::snapshot_channel_slice` — the one door
            // both `history/<ch>` writers take — folds and fills both.
            deleted_messages: BTreeSet::new(),
            reaction_log: BTreeMap::new(),
            // Nor what the room backend has parked: the manager holds that
            // set at rest for exactly this door, and fills it there.
            parked_floor_deletes: Vec::new(),
        })
    }

    /// Restore a slice into this store: find-or-create the channel-keyed
    /// thread, adopt the slice's label, and append every message (the store's
    /// id-level dedup makes this idempotent and safe on a non-empty store —
    /// an already-present `message_id` keeps its local copy). Returns the
    /// thread id so the backend can bind channel → thread.
    pub fn restore_channel_slice(&self, slice: &ChannelHistorySlice) -> ThreadId {
        let id = self.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: slice.channel_id_hex.clone(),
            },
            slice.participants.clone(),
            slice.flavor.clone(),
            Some(slice.label.clone()),
        );
        // A pre-existing thread keeps its stale label unless we adopt the
        // slice's (the slice's fold carries any rename effects).
        self.rename(&id, slice.label.clone());
        // Provenance: the recorded set of owner-typed rows.
        self.mark_anchor_grade(&id, slice.anchor_grade_handles.iter().copied());
        for m in &slice.messages {
            self.append_message(&id, m.clone());
        }
        id
    }
}

/// Commutative two-way merge of concurrent history slices for the same
/// channel: message union by `MessageId` (same-id copies from the
/// higher-watermark side — its fold of the seq-ordered log is a superset),
/// scalars from the higher-watermark side, watermark = max. On equal
/// watermarks the tie-break is the canonical-encoding byte order (arbitrary
/// but symmetric, so `merge(a, b) == merge(b, a)` always holds).
pub fn merge_history_slices(
    a: &ChannelHistorySlice,
    b: &ChannelHistorySlice,
) -> ChannelHistorySlice {
    let hi = if a.watermark > b.watermark {
        a
    } else if b.watermark > a.watermark {
        b
    } else {
        // Equal watermarks: symmetric byte-order tie-break. Equal bytes ⇒
        // identical slices, either side serves.
        let ab = canonical_encode(a).unwrap_or_default();
        let bb = canonical_encode(b).unwrap_or_default();
        if ab >= bb { a } else { b }
    };
    let lo = if std::ptr::eq(hi, a) { b } else { a };

    // A recorded foreign home wins over an absent one in either direction (see
    // the field below); computed once so the same-nest marker can key off it.
    let home_nest_url = a.home_nest_url.clone().or_else(|| b.home_nest_url.clone());

    // The reaction log: a per-message union (see the field below).
    let reaction_log = merge_reaction_state(lo, hi);

    // Union: the winner's copies stand; add loser-only messages.
    let mut messages = hi.messages.clone();
    for m in &lo.messages {
        match messages.iter_mut().find(|x| x.message_id == m.message_id) {
            // ⚠ The tombstone is the ONE field the winner does not simply
            // decide. It is monotone — nothing un-deletes a message — and a
            // device can hold one the log cannot yet justify: an own delete is
            // MLS-opaque to its author, so the deleting device may carry the
            // lower watermark and still be the only side that knows. Taking
            // the winner's copy wholesale would silently un-delete it, which
            // is a user-visible regression of an act the user already
            // performed. OR, and it converges from either direction.
            Some(held) => held.deleted |= m.deleted,
            None => messages.push(m.clone()),
        }
    }
    // Deterministic thread order: channel seq when the id carries one
    // (`conv:{hex}:{seq}`), then timestamp, then id.
    messages.sort_by(|x, y| {
        (
            channel_seq_of(&x.message_id),
            x.timestamp_ms,
            &x.message_id.0,
        )
            .cmp(&(
                channel_seq_of(&y.message_id),
                y.timestamp_ms,
                &y.message_id.0,
            ))
    });

    ChannelHistorySlice {
        channel_id_hex: hi.channel_id_hex.clone(),
        label: hi.label.clone(),
        flavor: hi.flavor.clone(),
        participants: hi.participants.clone(),
        messages,
        watermark: a.watermark.max(b.watermark),
        // A recorded home wins over an absent one in either direction: `None`
        // is "same-nest or unknown" and can only lose information, so a merge
        // that dropped a known home would re-open the routing hole this field
        // closes.
        home_nest_url: home_nest_url.clone(),
        // The same-nest marker is meaningful only when no foreign home is
        // recorded (a channel is foreign-homed or same-nest, never both). A
        // known marker on either side survives — like a recorded home, it can
        // only add information (an explicit `SameNest` the pre-guard must not
        // overwrite); an absent (unknown) marker loses to a known one.
        home_same_nest: home_nest_url.is_none() && (a.home_same_nest || b.home_same_nest),
        // Union by handle, the higher-watermark side's entry winning a handle
        // both carry — the messages' own side-pick, so the merge stays
        // commutative. Two entries for one handle name the same bytes (the
        // handle is their BLAKE3); a stale one costs only a refill that forgets
        // it, while dropping one would leave that attachment declared.
        attachment_coordinates: {
            let mut coordinates = lo.attachment_coordinates.clone();
            coordinates.extend(
                hi.attachment_coordinates
                    .iter()
                    .map(|(hash, blob)| (hash.clone(), blob.clone())),
            );
            coordinates
        },
        // Provenance describes `participants`, so it follows the side whose
        // participants won — a set keyed to the other side's rows would vouch
        // for handles this slice does not carry.
        anchor_grade_handles: hi.anchor_grade_handles.clone(),
        // Monotone, like the per-message flag it mirrors: a union, never a
        // side-pick. Either device may hold the only copy of a tombstone.
        deleted_messages: a
            .deleted_messages
            .union(&b.deleted_messages)
            .cloned()
            .collect(),
        // Per message: a deduped UNION of the two stamped logs — see
        // [`merge_reaction_state`].
        reaction_log,
        // A union, never a side-pick, for the tombstone's reason: either
        // device may be the only one still holding a record the other has
        // already judged — and the judged one costs the restoring device one
        // idempotent re-judge, while a dropped one un-deletes for good.
        parked_floor_deletes: {
            let mut parked = lo.parked_floor_deletes.clone();
            union_parked_floor_deletes(&mut parked, hi.parked_floor_deletes.iter().cloned());
            parked
        },
    }
}

/// Where one stamped event sorts in a unioned reaction log — the canonical
/// order [`merge_reaction_state`] writes.
///
/// `(stamp, reactor, emoji, Add before Remove)`: inside one `(reactor, emoji)`
/// pair that pair's LAST entry in this order is its highest-stamped op with
/// `Remove` on a tie — exactly the op [`crate::reactions::fold_reactions`]
/// ranks highest, so the written log reads chronologically.
///
/// Keyed explicitly rather than by deriving `Ord` on
/// [`fauna_mls::types::ReactionOp`]: that is a wire enum, and an ordering
/// derived there would read as a promise the wire makes, when it is only this
/// merge's tie-break.
fn reaction_sort_key(ev: &StampedReactionEvent) -> (i64, [u8; 32], &str, (u8, &str)) {
    (
        ev.sent_at_ms,
        ev.reactor.0,
        ev.emoji.as_str(),
        // A carried op this build does not name sorts after both known ones,
        // by its own string — so the order stays total and the merge
        // symmetric whatever ops a newer build writes.
        match &ev.op {
            fauna_mls::types::ReactionOp::Add => (0, ""),
            fauna_mls::types::ReactionOp::Remove => (1, ""),
            fauna_mls::types::ReactionOp::Other(op) => (2, op.as_str()),
        },
    )
}

/// The two devices' per-message reaction logs, merged: a deduped **union** of
/// the two logs for a message both sides carry, and the one side's log for a
/// message only one side carries.
///
/// # Why a union is available at all
///
/// A log is a fold of the seq-ordered stream, so the side that folded further
/// holds the superset of what *both* devices can see — which is why every
/// other same-id field here side-picks by watermark. Reactions are the
/// exception in both directions: the side-pick costs the losing device its
/// OWN reactions since the fork (an own `Reaction` is MLS-opaque to its
/// author, so the reacting device need not be the furthest-polled one), and
/// since every op carries its author's signed stamp the union is safe —
/// [`crate::reactions::fold_reactions`] reads the SET of distinct signed ops,
/// not the sequence, so two copies of one op collapse and the order the
/// events arrive in cannot change the answer. (This is the same property that
/// defeats replay on the community class: `community-rooms.md` § The three
/// classes → *Community* → *Who wrote it*.) Dedup key: the whole event —
/// `(reactor, emoji, op, sent_at_ms)` — since two events equal on all four
/// rank identically and the fold cannot tell them apart.
///
/// # Ordering
///
/// The union is written in [`reaction_sort_key`] order — deterministic, so
/// `merge(a, b)` and `merge(b, a)` agree down to the canonical bytes (the
/// equal-watermark tie-break reads them). The fold's display contract —
/// first-ADD appearance order (`../ui/conversations.md` § Reactions & message
/// delete) — then means *earliest-stamped add first*, which is the
/// chronological answer both devices would have reached had neither forked.
fn merge_reaction_state(
    lo: &ChannelHistorySlice,
    hi: &ChannelHistorySlice,
) -> BTreeMap<MessageId, Vec<StampedReactionEvent>> {
    // A message only one side holds a log for keeps it.
    let mut merged = lo.reaction_log.clone();
    for (message_id, hi_log) in &hi.reaction_log {
        let Some(lo_log) = lo.reaction_log.get(message_id) else {
            merged.insert(message_id.clone(), hi_log.clone());
            continue;
        };
        // Linear dedup, like the fold itself: a message's reaction count is
        // small, and this keeps the rule readable at the cost of nothing.
        let mut union: Vec<StampedReactionEvent> = Vec::new();
        for ev in lo_log.iter().chain(hi_log) {
            if !union.contains(ev) {
                union.push(ev.clone());
            }
        }
        union.sort_by(|x, y| reaction_sort_key(x).cmp(&reaction_sort_key(y)));
        merged.insert(message_id.clone(), union);
    }
    merged
}

/// The channel `seq` embedded in a fauna-native message id
/// ([`crate::message::MessageId::channel_position`]), or `i64::MAX` for
/// foreign-shaped ids so they sort after seq-bearing ones (stable via the
/// timestamp/id fallback keys).
fn channel_seq_of(id: &crate::message::MessageId) -> i64 {
    id.channel_position()
        .and_then(|(_, seq)| i64::try_from(seq).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{BodyFormat, MessageBadges, MessageId};

    fn actor(name: &str) -> TypedAddress {
        TypedAddress::Email {
            email_address: format!("{name}@example.com"),
        }
    }

    fn msg(id: &str, body: &str, is_own: bool, ts: i64) -> MessageSnapshot {
        MessageSnapshot {
            message_id: MessageId(id.to_string()),
            sender: actor(if is_own { "me" } else { "peer" }),
            sender_display: String::new(),
            body: body.to_string(),
            document: crate::message::document_for_message(body, BodyFormat::Markdown, &[]),
            timestamp_ms: ts,
            subject_line: None,
            badges: MessageBadges::default(),
            reply_to: None,
            reactions: vec![],
            deleted: false,
            is_own,
            legal_takedown_ref: None,
            labels: vec![],
            plane_ref: None,
            can_delete: false,
        }
    }

    const CH: &str = "aa11";

    fn slice(messages: Vec<MessageSnapshot>, watermark: i64, label: &str) -> ChannelHistorySlice {
        ChannelHistorySlice {
            channel_id_hex: CH.to_string(),
            label: label.to_string(),
            flavor: ThreadFlavor::OneToOne,
            participants: vec![actor("me"), actor("peer")],
            messages,
            watermark,
            ..Default::default()
        }
    }

    /// The slice-1 flow assertion, store half: device A's thread (own + peer
    /// messages) → slice bytes → fresh store restores → the full history
    /// including A's OWN messages is present, in order, under a channel-keyed
    /// thread.
    #[test]
    fn slice_round_trip_restores_own_messages_into_fresh_store() {
        let store_a = ThreadStore::new();
        let parts = vec![actor("me"), actor("peer")];
        let id = store_a.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: CH.to_string(),
            },
            parts,
            ThreadFlavor::OneToOne,
            Some("peer".to_string()),
        );
        store_a.append_message(&id, msg(&format!("conv:{CH}:1"), "hi from peer", false, 10));
        store_a.append_message(&id, msg(&format!("conv:{CH}:2"), "my own reply", true, 20));

        let captured = store_a.snapshot_channel_slice(&id, CH, 2).unwrap();
        let bytes = captured.to_bytes().unwrap();
        let decoded = ChannelHistorySlice::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, captured);
        assert_eq!(decoded.watermark, 2);

        let store_b = ThreadStore::new();
        let restored_id = store_b.restore_channel_slice(&decoded);
        let detail = store_b.get(&restored_id).unwrap();
        assert_eq!(detail.label, "peer");
        assert_eq!(detail.messages.len(), 2);
        assert!(detail.messages[1].is_own, "own message must survive");
        assert_eq!(detail.messages[1].body, "my own reply");

        // Restoring again is idempotent (id-level dedup).
        let again = store_b.restore_channel_slice(&decoded);
        assert_eq!(again, restored_id);
        assert_eq!(store_b.get(&restored_id).unwrap().messages.len(), 2);
    }

    #[test]
    fn merge_unions_messages_and_is_commutative() {
        let shared = msg(&format!("conv:{CH}:1"), "shared", false, 10);
        let a = slice(
            vec![
                shared.clone(),
                msg(&format!("conv:{CH}:2"), "only-a", true, 20),
            ],
            2,
            "peer",
        );
        let b = slice(
            vec![shared, msg(&format!("conv:{CH}:3"), "only-b", true, 30)],
            3,
            "renamed",
        );

        let ab = merge_history_slices(&a, &b);
        let ba = merge_history_slices(&b, &a);
        assert_eq!(ab, ba, "merge must be commutative");
        assert_eq!(ab.watermark, 3);
        assert_eq!(ab.label, "renamed", "higher-watermark side's scalars win");
        let ids: Vec<&str> = ab
            .messages
            .iter()
            .map(|m| m.message_id.0.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                format!("conv:{CH}:1"),
                format!("conv:{CH}:2"),
                format!("conv:{CH}:3")
            ],
            "union, ordered by channel seq"
        );
    }

    #[test]
    fn merge_same_id_takes_higher_watermark_copy() {
        // The same logged message, but the higher-watermark side has folded a
        // later Delete for it.
        let plain = msg(&format!("conv:{CH}:1"), "hello", false, 10);
        let mut deleted = plain.clone();
        deleted.deleted = true;

        let low = slice(vec![plain], 1, "peer");
        let high = slice(vec![deleted], 5, "peer");

        let merged = merge_history_slices(&low, &high);
        assert!(merged.messages[0].deleted, "higher-watermark fold wins");
        assert_eq!(merge_history_slices(&high, &low), merged);
    }

    #[test]
    fn merge_idempotent() {
        let a = slice(vec![msg(&format!("conv:{CH}:1"), "x", true, 1)], 1, "peer");
        assert_eq!(merge_history_slices(&a, &a), a);
    }

    /// **A tombstone is monotone, so the merge unions it instead of letting the
    /// higher watermark decide** — the one field where `merge_same_id_takes_
    /// higher_watermark_copy`'s rule would lose information rather than pick
    /// between two folds.
    ///
    /// The case is ordinary, not exotic: an own `Delete` is MLS-opaque to its
    /// author off the log, so the device that made it can be the ONLY one that
    /// knows, and it is under no obligation to also be the one that has polled
    /// furthest. A side-pick would silently un-delete a message the user
    /// already deleted — and `conversations.md` § Reactions & message delete
    /// has no un-delete for them to reach for.
    #[test]
    fn merge_unions_a_tombstone_the_higher_watermark_side_has_not_seen() {
        let id = MessageId(format!("conv:{CH}:1"));
        let mut deleted_copy = msg(&id.0, "hello", true, 10);
        deleted_copy.deleted = true;

        // The DELETING device is the one that has polled LESS far.
        let mut low = slice(vec![deleted_copy], 1, "peer");
        low.deleted_messages.insert(id.clone());
        let high = slice(vec![msg(&id.0, "hello", true, 10)], 5, "peer");

        for merged in [
            merge_history_slices(&low, &high),
            merge_history_slices(&high, &low),
        ] {
            assert!(
                merged.messages[0].deleted,
                "the tombstone survives whichever side won the watermark"
            );
            assert!(
                merged.deleted_messages.contains(&id),
                "and so does the set the restoring device re-seeds its projection from"
            );
        }
        assert_eq!(
            merge_history_slices(&low, &high),
            merge_history_slices(&high, &low),
            "still commutative"
        );
    }

    /// A parked floor delete record — a community room's moderation this
    /// device walked past but could not judge yet. Public, signed bytes; the
    /// signature is not verified by anything in this module, so a stand-in
    /// will do.
    fn parked(target_seq: u64, delete_seq: u64) -> ParkedFloorDelete {
        use fauna_mls::room_policy::{RoomFloorDelete, SignedRoomFloorDelete};
        ParkedFloorDelete {
            signed: SignedRoomFloorDelete {
                record: RoomFloorDelete {
                    room: vec![0x11; 32],
                    target_seq,
                    author: fauna_core::identity::ActorId([0x22; 32]),
                    policy_version: 3,
                },
                signature: vec![0x33; 64],
            },
            delete_seq: Some(delete_seq),
        }
    }

    /// `conversation-rooms.md` § Implementation status today, residual *(d)*:
    /// a record parked at quit is below the durable cursor and never walked
    /// again, so the parked set rides the slice — and a slice for a room with
    /// nothing parked is byte-identical to one written before the field.
    #[test]
    fn parked_floor_deletes_round_trip_and_an_empty_set_is_byte_identical_to_a_pre_field_slice() {
        let current = slice(vec![msg(&format!("conv:{CH}:1"), "x", true, 1)], 1, "peer");
        let bytes = current.to_bytes().unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("parked_floor_deletes"),
            "an empty parked set is omitted at rest"
        );

        let mut with = current.clone();
        with.parked_floor_deletes = vec![parked(1, 7), parked(2, 9)];
        let decoded = ChannelHistorySlice::from_bytes(&with.to_bytes().unwrap()).unwrap();
        assert_eq!(
            decoded, with,
            "the parked records round-trip through the at-rest bytes"
        );
        assert_eq!(
            ChannelHistorySlice::from_bytes(&bytes)
                .unwrap()
                .parked_floor_deletes,
            Vec::<ParkedFloorDelete>::new(),
            "absent reads as empty — the `Default` invariant"
        );
    }

    /// Two devices that fork with different parked sets converge on the
    /// union, in log order, each record once, and never past the live cap —
    /// a parked record is a tombstone that may yet paint, so the merge cannot
    /// let either side drop one the other holds alone.
    #[test]
    fn merge_unions_parked_floor_deletes_in_log_order_and_stays_commutative() {
        let mut low = slice(vec![], 1, "peer");
        low.parked_floor_deletes = vec![parked(1, 7), parked(3, 11)];
        let mut high = slice(vec![], 5, "peer");
        high.parked_floor_deletes = vec![parked(2, 9), parked(3, 11)];

        for merged in [
            merge_history_slices(&low, &high),
            merge_history_slices(&high, &low),
        ] {
            assert_eq!(
                merged.parked_floor_deletes,
                vec![parked(1, 7), parked(2, 9), parked(3, 11)],
                "the union, in log order, the shared record once"
            );
        }
        assert_eq!(
            merge_history_slices(&low, &high),
            merge_history_slices(&high, &low),
            "still commutative"
        );

        let mut held: Vec<ParkedFloorDelete> = (0..MAX_PARKED_FLOOR_DELETES as u64)
            .map(|i| parked(i, 100 + i))
            .collect();
        union_parked_floor_deletes(&mut held, [parked(999, 50), parked(1000, 5000)]);
        assert_eq!(
            held.len(),
            MAX_PARKED_FLOOR_DELETES,
            "capped like the live set"
        );
        assert_eq!(
            held.first().unwrap().delete_seq,
            Some(101),
            "the two oldest by log position — the late-added seq 50 and seq 100 — \
             are what a full set drops"
        );
        assert_eq!(held.last().unwrap().delete_seq, Some(5000));
    }

    /// One signed reaction op, as a current writer logs it.
    fn st(
        n: u8,
        emoji: &str,
        op: fauna_mls::types::ReactionOp,
        t: i64,
    ) -> crate::reactions::StampedReactionEvent {
        crate::reactions::StampedReactionEvent::new(ActorId([n; 32]), emoji.to_string(), op, t)
    }

    /// Record one message's reaction log on a slice, as
    /// `ConversationsManager::snapshot_channel_slice` writes it.
    fn log_on(
        slice: &mut ChannelHistorySlice,
        id: &MessageId,
        log: Vec<crate::reactions::StampedReactionEvent>,
    ) {
        slice.reaction_log.insert(id.clone(), log);
    }

    /// **Two devices that each reacted after a fork BOTH keep their reaction.**
    /// A watermark side-pick would cost the lower-watermark device its own
    /// reactions since the fork; every op carries its author's signed stamp,
    /// and the fold reads the SET
    /// ([`crate::reactions::fold_reactions`]), so a deduped union folds
    /// deterministically and that cost is gone.
    #[test]
    fn merge_unions_two_fully_stamped_reaction_logs() {
        use fauna_mls::types::ReactionOp::Add;
        let shared = MessageId(format!("conv:{CH}:1"));
        let me = ActorId([9; 32]);

        // The fork point both devices carry — deduped to one copy.
        let before = st(2, "👍", Add, 10);
        // Each device's OWN reaction after the fork: what the side-pick costs
        // whichever side ends up lower-watermark.
        let lo_own = st(5, "❤️", Add, 20);
        let hi_own = st(6, "🙏", Add, 30);

        let mut low = slice(vec![msg(&shared.0, "a", false, 1)], 1, "peer");
        log_on(&mut low, &shared, vec![before.clone(), lo_own.clone()]);
        let mut high = slice(vec![msg(&shared.0, "a", false, 1)], 5, "peer");
        log_on(&mut high, &shared, vec![before.clone(), hi_own.clone()]);

        for merged in [
            merge_history_slices(&low, &high),
            merge_history_slices(&high, &low),
        ] {
            assert_eq!(
                merged.reaction_log[&shared],
                vec![before.clone(), lo_own.clone(), hi_own.clone()],
                "the union, the shared op once, in canonical stamp order"
            );
            let pills: Vec<(String, u32)> =
                crate::reactions::fold_reactions(&merged.reaction_log[&shared], me)
                    .into_iter()
                    .map(|g| (g.emoji, g.count))
                    .collect();
            assert_eq!(
                pills,
                vec![
                    ("👍".to_string(), 1),
                    ("❤️".to_string(), 1),
                    ("🙏".to_string(), 1)
                ],
                "neither device's own reaction is lost to the other's watermark"
            );
        }

        assert_eq!(
            canonical_encode(&merge_history_slices(&low, &high)).unwrap(),
            canonical_encode(&merge_history_slices(&high, &low)).unwrap(),
            "commutative down to the canonical BYTES — the equal-watermark \
             tie-break reads them, so a merge that only agreed up to ordering \
             would make that tie-break direction-dependent"
        );
    }

    /// **`Remove` sorts after `Add` at an equal stamp**, and the union keeps a
    /// retraction only the lower-watermark device holds: within a
    /// `(reactor, emoji)` pair the merged log's LAST entry is the op the fold
    /// ranks highest.
    #[test]
    fn a_unioned_log_keeps_a_same_stamp_retraction() {
        use fauna_mls::types::ReactionOp::{Add, Remove};
        let shared = MessageId(format!("conv:{CH}:1"));
        let me = ActorId([9; 32]);

        // One reactor's add and its retraction inside the same millisecond,
        // each device holding only one of the two.
        let add = st(2, "👍", Add, 10);
        let remove = st(2, "👍", Remove, 10);

        let mut low = slice(vec![msg(&shared.0, "a", false, 1)], 1, "peer");
        log_on(&mut low, &shared, vec![remove.clone()]);
        let mut high = slice(vec![msg(&shared.0, "a", false, 1)], 5, "peer");
        log_on(&mut high, &shared, vec![add.clone()]);

        let merged = merge_history_slices(&low, &high);
        assert_eq!(
            merged.reaction_log[&shared],
            vec![add, remove],
            "Add before Remove at the equal stamp"
        );

        let stamped_fold = crate::reactions::fold_reactions(&merged.reaction_log[&shared], me);
        assert!(
            stamped_fold.is_empty(),
            "the retraction stands — and the side-pick would have lost it, \
             since the retracting device is the lower-watermark one (an own \
             reaction is MLS-opaque to its author, so it need not be the \
             furthest-polled side)"
        );
    }

    /// The **same-nest marker survives a merge**, in either direction — a
    /// defence-in-depth half of the routing datum's total encoding. A concurrent replica that recorded no marker (an absent
    /// marker) must never be able to strip a channel's known-`SameNest` status,
    /// or a plant window reopens on the next save; `save_history_cas` merges the
    /// slice being saved with whatever is at rest, so this is exactly that path.
    #[test]
    fn merge_preserves_the_same_nest_marker_over_an_absent_one() {
        let msgs = || vec![msg(&format!("conv:{CH}:1"), "x", true, 1)];
        let mut known = slice(msgs(), 1, "peer");
        known.home_same_nest = true;
        let unknown = slice(msgs(), 1, "peer"); // home_same_nest: false (no marker recorded)

        assert!(
            merge_history_slices(&known, &unknown).home_same_nest,
            "a known SameNest marker was lost merging with a marker-less slice"
        );
        assert!(
            merge_history_slices(&unknown, &known).home_same_nest,
            "merge must preserve the marker regardless of argument order"
        );
    }

    /// A recorded **foreign** home wins over a same-nest marker (a channel is
    /// foreign-homed or same-nest, never both), and the spurious marker is
    /// dropped so the merged slice stays a valid total encoding.
    #[test]
    fn merge_foreign_home_beats_a_same_nest_marker() {
        let msgs = || vec![msg(&format!("conv:{CH}:1"), "x", true, 1)];
        let mut foreign = slice(msgs(), 1, "peer");
        foreign.home_nest_url = Some("https://home.example".into());
        let mut same_nest = slice(msgs(), 1, "peer");
        same_nest.home_same_nest = true;

        let merged = merge_history_slices(&foreign, &same_nest);
        assert_eq!(
            merged.home_nest_url.as_deref(),
            Some("https://home.example")
        );
        assert!(
            !merged.home_same_nest,
            "a foreign home and a same-nest marker must not coexist after merge"
        );
    }

    fn blob(cid: &str, epoch: u64) -> SealedBlobCoordinates {
        SealedBlobCoordinates {
            sealed_cid_hex: cid.to_string(),
            size_bytes: 40,
            key: crate::store::attachments::AttachmentOpeningKey::MlsEpoch { epoch },
        }
    }

    /// The additive at-rest rule (`version-compatibility.md` § I4): a slice
    /// with no attachment coordinates encodes byte-for-byte as a slice written
    /// before the field existed, and such a blob decodes with none — so a
    /// replica sealed by an older binary still restores, and re-saving an
    /// unchanged attachment-free slice is no change.
    #[test]
    fn a_slice_without_coordinates_is_byte_identical_to_one_written_before_the_field() {
        #[derive(Serialize)]
        struct PreCoordinatesSlice {
            channel_id_hex: String,
            label: String,
            flavor: ThreadFlavor,
            participants: Vec<TypedAddress>,
            messages: Vec<MessageSnapshot>,
            watermark: i64,
            home_nest_url: Option<String>,
            home_same_nest: bool,
            // Required at rest, so every blob carries it.
            anchor_grade_handles: Vec<ActorId>,
        }
        let current = slice(vec![msg(&format!("conv:{CH}:1"), "x", true, 1)], 1, "peer");
        let pre = PreCoordinatesSlice {
            channel_id_hex: current.channel_id_hex.clone(),
            label: current.label.clone(),
            flavor: current.flavor.clone(),
            participants: current.participants.clone(),
            messages: current.messages.clone(),
            watermark: current.watermark,
            home_nest_url: None,
            home_same_nest: false,
            anchor_grade_handles: Vec::new(),
        };
        let pre_bytes = canonical_encode(&pre).unwrap();
        assert_eq!(current.to_bytes().unwrap(), pre_bytes);
        assert_eq!(
            ChannelHistorySlice::from_bytes(&pre_bytes).unwrap(),
            current
        );

        let mut with = current.clone();
        with.attachment_coordinates
            .insert("aa".into(), blob("cc", 3));
        assert_eq!(
            ChannelHistorySlice::from_bytes(&with.to_bytes().unwrap()).unwrap(),
            with,
            "the coordinates round-trip through the at-rest bytes"
        );
    }

    // ── handle provenance at rest ─────────────────────────────────────────
    //
    // `identity-succession.md` § The succession statement → *which participant
    // handles anchor tier 2*; the field's own doc owns the `None` reading.

    fn fauna(handle: &str, seed: u8) -> TypedAddress {
        TypedAddress::Fauna {
            handle: handle.to_string(),
            actor_id: ActorId([seed; 32]),
        }
    }

    fn fauna_thread(store: &ThreadStore, parts: Vec<TypedAddress>) -> ThreadId {
        store.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: CH.to_string(),
            },
            parts,
            ThreadFlavor::MlsGroup,
            Some("peers".to_string()),
        )
    }

    /// The owner-typed mark survives the slice round trip and the room-home
    /// name does not acquire one: a restored device anchors exactly as the
    /// writing device did.
    #[test]
    fn anchor_grade_marks_survive_the_slice_round_trip() {
        let alice = ActorId([1; 32]);
        let bob = ActorId([2; 32]);
        let store_a = ThreadStore::new();
        let id = fauna_thread(&store_a, vec![fauna("alice@home.example", 1), fauna("", 2)]);
        store_a.mark_anchor_grade(&id, [alice]);
        store_a.name_participants(&id, &[(bob, "bob@host.example".into())]);

        let slice = store_a.snapshot_channel_slice(&id, CH, 0).unwrap();
        assert_eq!(
            slice.anchor_grade_handles,
            vec![alice],
            "the writer records exactly the owner-typed rows"
        );
        let bytes = slice.to_bytes().unwrap();
        let restored = ChannelHistorySlice::from_bytes(&bytes).unwrap();
        assert_eq!(restored, slice);

        let store_b = ThreadStore::new();
        let id_b = store_b.restore_channel_slice(&restored);
        assert_eq!(
            store_b.anchor_grade_handle_for(&alice).as_deref(),
            Some("alice@home.example")
        );
        assert_eq!(
            store_b.anchor_grade_handle_for(&bob),
            None,
            "the room-home name restores as display only"
        );
        assert_eq!(store_b.anchor_grade_actors(&id_b), vec![alice]);
    }

    /// Provenance is required at rest: a slice with no anchor-grade row
    /// round-trips as an empty list and marks nothing — a named row is never
    /// read as anchor-grade without the record saying so — and a blob missing
    /// the field is refused rather than read under some default.
    #[test]
    fn provenance_is_required_and_an_empty_record_marks_nothing() {
        let alice = ActorId([1; 32]);
        let mut recorded = slice(vec![], 0, "peers");
        recorded.participants = vec![fauna("alice@home.example", 1), fauna("", 2)];
        assert!(recorded.anchor_grade_handles.is_empty());
        let round = ChannelHistorySlice::from_bytes(&recorded.to_bytes().unwrap()).unwrap();
        assert_eq!(round, recorded);
        let store = ThreadStore::new();
        store.restore_channel_slice(&round);
        assert_eq!(
            store.anchor_grade_handle_for(&alice),
            None,
            "a recorded slice with no anchor-grade row marks nothing"
        );

        // Strip the field from the encoded map: the decode refuses it.
        let mut value: fauna_cbor::Value =
            fauna_cbor::decode_strict(&recorded.to_bytes().unwrap()).unwrap();
        let fauna_cbor::Value::Map(entries) = &mut value else {
            panic!("a slice encodes as a map");
        };
        assert!(entries.remove("anchor_grade_handles").is_some());
        let stripped = fauna_cbor::encode_canonical(&value).unwrap();
        assert!(matches!(
            ChannelHistorySlice::from_bytes(&stripped),
            Err(HistoryRestoreError::Decode(_))
        ));
    }

    /// The merge keeps the provenance of the side whose participants won, in
    /// either argument order — a set keyed to the other side's rows would
    /// vouch for handles the merged slice does not carry.
    #[test]
    fn merge_keeps_the_provenance_of_the_side_whose_participants_won() {
        let alice = ActorId([1; 32]);
        let mut lo = slice(vec![msg(&format!("conv:{CH}:1"), "x", true, 1)], 1, "peers");
        lo.participants = vec![fauna("alice@home.example", 1)];
        lo.anchor_grade_handles = vec![alice];
        let mut hi = slice(vec![msg(&format!("conv:{CH}:2"), "y", true, 2)], 2, "peers");
        hi.participants = vec![fauna("alice@host.example", 1)];
        hi.anchor_grade_handles = vec![];

        for merged in [
            merge_history_slices(&lo, &hi),
            merge_history_slices(&hi, &lo),
        ] {
            assert_eq!(merged.participants, hi.participants);
            assert_eq!(
                merged.anchor_grade_handles,
                Vec::<ActorId>::new(),
                "the winner's rows carry the winner's provenance"
            );
        }
    }

    /// A merge unions the coordinates, the higher-watermark side's entry
    /// winning a handle both carry, in either argument order — so a CAS retry
    /// never writes away an attachment another device could fetch.
    #[test]
    fn merge_unions_attachment_coordinates_commutatively() {
        let msgs = || vec![msg(&format!("conv:{CH}:1"), "x", true, 1)];
        let mut low = slice(msgs(), 1, "peer");
        low.attachment_coordinates
            .insert("only-low".into(), blob("01", 1));
        low.attachment_coordinates
            .insert("both".into(), blob("02", 1));
        let mut high = slice(msgs(), 2, "peer");
        high.attachment_coordinates
            .insert("only-high".into(), blob("03", 2));
        high.attachment_coordinates
            .insert("both".into(), blob("04", 2));

        let merged = merge_history_slices(&low, &high);
        assert_eq!(merged, merge_history_slices(&high, &low));
        assert_eq!(
            merged
                .attachment_coordinates
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["both", "only-high", "only-low"]
        );
        assert_eq!(merged.attachment_coordinates["both"], blob("04", 2));
    }
}
