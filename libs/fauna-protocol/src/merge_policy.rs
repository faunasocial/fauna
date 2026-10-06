//! The class-2 **kind registry** — a kind's merge policy and its audience rung,
//! plus the reader-side application of one incoming entry (W2.4 (account-data-plane.md § Workstreams)).
//!
//! Authority for the policies:
//! `docs/goal/architecture/account-data-plane.md` § Merge-policy seam — a
//! **closed set of five**, one per kind, with the per-field CRDT
//! merges of the account's preference records as the precedent. Authority for the rungs: the same charter's
//! § The audience ladder (R13 (account-data-plane.md § The ratified decisions)). This module is the inventory + the dispatcher; it
//! does not restate why a policy or a rung exists.
//!
//! The two columns are declared together because they are declared *at the same
//! moment* — registration — and both freeze with the kind string. They answer
//! entirely different questions (how it merges / who can open it) and the charter
//! is explicit that conflating them ships credentials to grantees; [`POLICIES`]
//! carries that argument at the table itself.
//!
//! # Why the lookup returns `Option` where `offline_class` is total
//!
//! [`crate::offline_class`] classifies all 596 *registered WS-RPC kinds* and is
//! bijection-tested against the registry. Class-2 **item kinds** are a
//! different vocabulary — the names a sealed entry carries inside its payload
//! (`fauna_core::account_entry_crypto::EntryPlaintext::kind`), which are also
//! the per-kind key-schedule inputs. That vocabulary grows one kind at a time
//! as the plane's kinds land, so the honest answer for a name this build does
//! not know is [`None`] ("not on the plane here"), never a default policy: a
//! guessed policy silently merges data under rules its author never chose.
//!
//! **A kind string is frozen the moment anything seals under it** — it is the
//! `keyed_hash` input of both `entry_key(kind)` and `item_blind(kind)`
//! (`owner-key-material.md` § Path A-sibling-2), so renaming one orphans every
//! entry already sealed under the old name, at rest and on every custodian.
//! Add rows; never edit one.
//!
//! # The reader is the only merger
//!
//! `apply_class2` runs at **reading replicas only** (charter § Replica posture,
//! R7: "class-2 merge happens only at reading replicas"). A custodian holds
//! sealed entries it cannot open and never reaches this code.

use std::borrow::Cow;
use std::collections::BTreeMap;

use fauna_core::account_entry_crypto::EntryPlaintext;
use fauna_core::crypto::{AccountStateKeySchedule, AudienceRung, SealingEpoch};
use serde::{Deserialize, Serialize};

use crate::ext_kind::ExtKind;

/// How a class-2 kind reconciles two concurrent values — the charter's closed
/// set (§ Merge-policy seam). Every kind on the plane declares exactly one;
/// `Immutable` is present for totality (class-1 records), not because a class-2
/// kind is expected to want it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePolicy {
    /// Class-1 records: identity *is* the content hash, so the first value
    /// under a key is the only value.
    Immutable,
    /// Irrecoverable class-2 (settings keys, the seen-set): merge field-wise /
    /// by union, never wholesale — losing a field a peer set is data loss.
    /// Shipped precedent: the per-record `merge` functions of the E3 kinds
    /// (`fauna_core::data`).
    CrdtPerField,
    /// Recreatable class-2: the newer stamp wins outright
    /// ([`LwwStamp`]). Precedent: the reserved rails' LWW.
    LatestWins,
    /// Text/file bodies: three-way merge with loser retention
    /// (`docs/goal/behavior/conflicts.md` owns the machinery).
    ThreeWay,
    /// Nest-arbitrated kinds: the nest serialized the writes before they ever
    /// reached the feed, so the feed carries applied truth in nest order.
    NestCas,
}

impl MergePolicy {
    /// Whether a kind under this policy may carry a **tombstone**.
    ///
    /// Part of the policy contract, declared here rather than discovered per
    /// kind (`account-data-plane.md` § Merge-policy seam). It is enforced at
    /// the *writer* door ([`crate`] has no writer; the plane's
    /// `write_local_and_publish` is it), because a tombstone a reader cannot
    /// merge is not a merge failure — it is a row that should never have been
    /// sealed, and the writing replica is the only party that can still refuse
    /// it cheaply.
    ///
    /// * [`Self::LatestWins`] and [`Self::NestCas`] — **yes.** A deletion is
    ///   just another value on a totally-ordered sequence: the stamp rank
    ///   resolves it for the former, nest order for the latter, and both
    ///   replicas reach the same answer.
    /// * [`Self::CrdtPerField`] — **no**, and [`apply_class2`] refuses it
    ///   loudly at the reader for the same reason ([`MergeError::
    ///   TombstoneOnCrdtKind`]): degrading a tombstone to a stamp comparison
    ///   lets a concurrent-and-newer value win wholesale on one replica while
    ///   the other keeps its merge, and the two never converge. Per-field
    ///   deletion is a design slice.
    /// * [`Self::Immutable`] — **no**, and this is the arm that had no stated
    ///   answer. A tombstone would reach [`apply_class2`]'s identity arm and
    ///   come back `KeepCurrent`: the delete silently would not happen, and the
    ///   caller would be told it succeeded. Refusing is not a claim that
    ///   immutable records can never be deleted — it is a refusal to *infer* a
    ///   deletion primitive for content-addressed items from an arm written to
    ///   mean "a second value under this key is a different item".
    /// * [`Self::ThreeWay`] — **no**: delete-versus-edit is the conflict rule
    ///   that machinery exists to decide, and no kind has wired a resolver.
    pub fn admits_tombstone(self) -> bool {
        match self {
            MergePolicy::LatestWins | MergePolicy::NestCas => true,
            MergePolicy::CrdtPerField | MergePolicy::Immutable | MergePolicy::ThreeWay => false,
        }
    }

    /// Whether a kind under this policy carries an [`LwwStamp`] in
    /// [`EntryPlaintext::merge_meta`] — including on a tombstone, where the
    /// stamp is the *only* thing ordering the deletion against a concurrent
    /// write.
    pub fn requires_stamp(self) -> bool {
        matches!(self, MergePolicy::LatestWins)
    }
}

// 🪦 `fauna.state.user-config` — the whole-record `UserConfig` kind — was
// RETIRED at E0 of the `__config` dissolution schedule (2026-08-12;
// `account-data-plane.md` § Migration + compatibility → *The `__config`
// dissolution schedule*). Registered fleet-only at W2.4 as the plane's first
// CRDT kind, **nothing ever sealed under it** (the R14 gate refused every
// fleet-only production seal from birth), and a whole-record kind spanning
// both audience rungs is exactly what R13 dissolved — so the registration was
// removed rather than left as a sealable hazard for the day the generation
// schedule lands. The string is **retired-never-reuse**: do not re-register
// it at any shape (`the_retired_user_config_kind_is_not_registered` pins
// this); the dissolution's per-field kinds land beside it under their own
// strings. (The blob rail whose merge this record once fed retired at closure
// step (6); see `docs/goal/architecture/config-dissolution.md`.)

/// Class-2 kind: the account's `moderation` preference sub-record
/// A kind whose canonical payload type travels WITH its name at the type
/// level — the kind→type binding the Secret-free conformance check needs to
/// be airtight (2026-08-17: the pin table maps a kind *string* to a
/// pin for a *specific* payload type, and without this constant nothing tied
/// the two — `load_record::<T>(handle, KIND)` took `T` and `KIND` as
/// unrelated parameters, so a call site could swap a delegable kind's payload
/// type with every pin still green).
///
/// The contract: the generic plane doors
/// (`fauna_sync_engine::preference_surfaces`) take their payload type FROM
/// one of these constants, never beside it — and the pin table's const bind
/// block (`crate::secret_free`) checks each constant's parameter against its
/// pin, so substituting a payload type means editing the constant here, which
/// fails the bind (and then the new type's exhaustive pin) to compile. The
/// derived `KIND_*` string constants below stay the string-context spelling;
/// they are `.name` of these, so the two can never drift.
pub struct RecordKind<T> {
    /// The frozen kind string (a key-schedule input, like every `KIND_*`).
    pub name: &'static str,
    _payload: std::marker::PhantomData<fn() -> T>,
}

impl<T> RecordKind<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _payload: std::marker::PhantomData,
        }
    }
}

/// The typed constants for the delegable preference-cluster kinds — one per
/// `KIND_*` string below, carrying the canonical payload type. See
/// [`RecordKind`] for the binding contract.
pub mod records {
    use super::RecordKind;
    use fauna_core::data::{
        DelegationConfig, ModerationConfig, PersonalizationConfig, SyncPrefsConfig,
    };
    use fauna_core::read_marker::ReadMarker;
    use fauna_core::seen_set::SeenScopeSet;

    pub const MODERATION: RecordKind<ModerationConfig> = RecordKind::new("fauna.state.moderation");
    pub const SYNC_PREFS: RecordKind<SyncPrefsConfig> = RecordKind::new("fauna.state.sync-prefs");
    pub const PERSONALIZATION: RecordKind<PersonalizationConfig> =
        RecordKind::new("fauna.state.personalization");
    pub const DELEGATION: RecordKind<DelegationConfig> = RecordKind::new("fauna.state.delegation");
    pub const SEEN_SET: RecordKind<SeenScopeSet> = RecordKind::new("fauna.state.seen-set");
    pub const READ_MARKER: RecordKind<ReadMarker> = RecordKind::new("fauna.state.read-marker");
}

/// (`fauna_core::data::ModerationConfig`) — the **first delegable-rung kind**,
/// and the first piece of the W2.5 preference cluster to ride the plane.
///
/// **Frozen** (see the module header): this string is a key-schedule input.
/// The typed spelling is [`records::MODERATION`]; this string is derived from
/// it, so the two cannot drift.
///
/// Registered ahead of its three cluster siblings (W2.5 item 0) because the
/// plane needed one kind it could actually seal: every other kind registered
/// that day was fleet-only and therefore R14-gated, which would have left the
/// sync plane provable by no test at all. The siblings below joined with W2.5
/// item 1 (their plane write is `fauna_sync_engine::preference_put`).
pub const KIND_MODERATION: &str = records::MODERATION.name;

/// The logical key of the singleton [`KIND_MODERATION`] item — one per account,
/// like every preference sub-record.
pub const MODERATION_KEY: &str = "self";

/// Class-2 kind: the account's `sync_prefs` preference sub-record
/// (`fauna_core::data::SyncPrefsConfig` — the user-global file-sync defaults).
///
/// **Frozen** (see the module header): this string is a key-schedule input.
/// The charter's `UserConfig` disposition table places it in the argued
/// delegable set (secret-free, recreatable, merging independently of its
/// siblings); W2.5 item 1's registration, per that table.
pub const KIND_SYNC_PREFS: &str = records::SYNC_PREFS.name;

/// Class-2 kind: the account's `personalization` preference sub-record
/// (`fauna_core::data::PersonalizationConfig` — the sealed trained-factor
/// registry; the model *blobs* stay in `personalization_models`, which is not
/// on this plane).
///
/// **Frozen** (see the module header): this string is a key-schedule input.
pub const KIND_PERSONALIZATION: &str = records::PERSONALIZATION.name;

/// Class-2 kind: the account's `delegation` preference sub-record
/// (`fauna_core::data::DelegationConfig` — the per-task-kind pins).
///
/// **Frozen** (see the module header): this string is a key-schedule input.
pub const KIND_DELEGATION: &str = records::DELEGATION.name;

/// The logical key every preference-cluster singleton uses — one item per
/// account per kind, so the key is a constant ([`MODERATION_KEY`] spelled the
/// same way for the same reason).
pub const PREFERENCE_KEY: &str = "self";

/// Class-2 kind: the fleet seen-set (charter § The replica boundary, R1) —
/// the plane's **first delegable CRDT kind**, and the union-merge exemplar
/// (W2.5 item 2).
///
/// **Frozen** (see the module header): this string is a key-schedule input.
///
/// **Not a singleton**: the logical key is the *referenced scope* — one entry
/// per scope whose observations it records — and the value is
/// [`fauna_core::seen_set::SeenScopeSet`] (per-writer watermarks + itemized
/// scope-feed coordinates; the representation ruling and the A4
/// compactability law live on that module).
///
/// The rung ruling (2026-08-12, recorded in the charter's § The audience
/// ladder): **delegable**, on the argued need the default-narrow rule
/// requires — R9's materialization tier is a *grant*, and only
/// delegable-rung kinds are grant-mintable at all
/// (`encryption-at-rest.md` § Capability tiering), so a fleet-only seen-set
/// would foreclose materialized read-state (unread views, cross-device
/// resume) permanently. The payload is references — coordinates and
/// watermarks, never content, never credentials — pinned Secret-free in
/// `crate::secret_free` in the same edit as this row.
pub const KIND_SEEN_SET: &str = records::SEEN_SET.name;

/// Class-2 kind: how far the user has read in one fauna-native conversation
/// channel (`docs/goal/behavior/conversation-read-state.md` § The read-marker
/// record) — the product-level read marker the seen-set deliberately is not.
///
/// **Frozen** (see the module header): this string is a key-schedule input.
///
/// **Not a singleton**: the logical key is
/// [`fauna_core::read_marker::channel_key`] (`conv:<channel id hex>`), one
/// entry per channel, and the value is
/// [`fauna_core::read_marker::ReadMarker`] — a max-register, so the merge is
/// its own `CrdtPerField` join rather than a stamp comparison that could move
/// a marker backwards. Entries are never shed (no tombstone is wanted: a
/// marker for a left channel is inert, and resumes on re-join).
///
/// The rung ruling (2026-09-21, recorded in `account-data-taxonomy.md`
/// § The audience ladder → *The read-marker rung*): **delegable**, on the
/// need the seen-set's ruling already argued — materialized unread views are
/// a grant, and only delegable-rung kinds are grant-mintable. The payload is
/// a reference and a counter, pinned Secret-free in `crate::secret_free` in
/// the same edit as this row.
pub const KIND_READ_MARKER: &str = records::READ_MARKER.name;

/// Class-2 kind: one device's dial candidates — the peer leg's registry row
/// (charter § The peer leg → *Discovery*, T5; W2.5 item 3). The value is
/// [`fauna_core::device_endpoints::DeviceEndpoints`].
///
/// **Frozen** (see the module header): this string is a key-schedule input.
///
/// **Not a singleton**: the logical key is the publishing device's writer id
/// (hex), so each device owns exactly one row and writers never collide on a
/// key — which is also why whole-record LWW is safe here (only the owning
/// device ever supersedes its row; a removal tombstone is stamped and
/// orderable).
///
/// The rung ruling (2026-08-12, recorded in the charter's § The audience
/// ladder): **fleet-only** — default-narrow holds, no argued need exists (no
/// grant-shaped consumer dials your devices; materialization wants derived
/// views, never dial candidates), and endpoint entries are location data.
/// Consequence, deliberate: the R14 gate refuses production origination of
/// this kind until the generation schedule lands — and for location data the
/// gate is a *feature* (generation keying severs a removed, possibly stolen,
/// device from the fleet's future addresses), so the explicitly-recreatable
/// exception was considered and rejected on the merits, not just for gate
/// hygiene. W2.6's store↔store proofs stage rows door-lessly (the sanctioned
/// pattern `conformance_account_state_walk.rs` documents); production peer
/// discovery sequences behind the generation schedule.
pub const KIND_DEVICE_ENDPOINTS: &str = "fauna.state.device-endpoints";

/// Class-2 kind: one custody a device HOLDS for another account — the
/// custodian-side registry row on the custodian's **own** plane (W8, T13:
/// "the custodian stores the witness (plus the owner's pinned NodeIds and
/// dial candidates) in a fleet-only class-2 kind of its own account plane").
/// The value is [`fauna_core::custodies_held::CustodyHeld`]; the logical key
/// is the custody grant id (hex), so one row per custody and writers never
/// collide. **Frozen** (see the module header).
///
/// Rung: **fleet-only**, and the epoch is **generation-tip**, for exactly
/// [`KIND_DEVICE_ENDPOINTS`]'s reason — the value carries the owner fleet's
/// NodeIds and dial candidates (location data), so generation keying is the
/// removal severance those entries want.
pub const KIND_CUSTODIES_HELD: &str = "fauna.state.custodies-held";

/// Class-2 kind: one custodian an account has GRANTED custody to — the
/// owner-side registry row (W8, T13's ceremony step 3: "the owner's fleet
/// records the custodian's NodeId + dial candidates as a fleet-only class-2
/// entry, so every fleet replica learns whom to serve and how to dial it").
/// The value is [`fauna_core::custodian_endpoints::CustodianEndpoints`]; the
/// logical key is the custody grant id (hex). **Frozen** (see the module
/// header).
///
/// Rung: **fleet-only** (the fleet-only `device-endpoints` kind never
/// reaches a non-fleet peer, and neither does this — the custodian learns
/// the owner's candidates over the ceremony channel instead); epoch
/// **generation-tip** (location data — same severance reasoning as its two
/// tip-sealed siblings).
pub const KIND_CUSTODIAN_ENDPOINTS: &str = "fauna.state.custodian-endpoints";

/// Class-2 kind: one member of one **shared file set**, as this fleet last
/// learned to reach them — the W8 share twin's discovery cache (slice F;
/// `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
/// *Discovery* + `p2p-shared-set-build.md` § *Build contract* → *Discovery carriage is its own
/// slice*). The value is [`fauna_core::share_endpoints::ShareEndpoints`];
/// the logical key is `<channel-id-hex>:<member-actor-hex>`
/// ([`fauna_core::share_endpoints::share_entry_key`]). **Frozen** (see the
/// module header).
///
/// Rung: **fleet-only** — the third member of the location-data family, for
/// its two siblings' reason exactly: the row holds *another user's* dial
/// candidates, learned over the set's own MLS-authenticated channel, and
/// must never travel onward to a non-fleet peer. Epoch **generation-tip**:
/// removing a device from this fleet must sever its future access to where
/// the account's counterparties can be reached.
///
/// **Deliberately NOT cfg-gated behind `p2p-share`,** though the plane it
/// serves is: a flavor-dependent at-rest kind table is the worse failure —
/// a store-safe replica must carry a row it does not understand *forward*
/// rather than drop it (no-data-loss). The excision claim the store-safe
/// witness pins is the `fauna.peer.share.` **wire** family, which this
/// at-rest kind does not widen.
pub const KIND_SHARE_ENDPOINTS: &str = "fauna.state.share-endpoints";

// ── The R14 generation machinery (charter § The generation machinery, build
// design ratified 2026-08-13). Six kinds, all fleet-only rung +
// [`SealingEpoch::Gen0`] — the bootstrap stratification: a fresh enrolled
// device reads the whole mint DAG under root-derivable material before it
// holds any generation key. Value types + the lattice joins live in
// `fauna_core::generation`; all six seal into the `state-fleet` scope
// (the A5 partition — `crate::account_state::ACCOUNT_STATE_FLEET_SCOPE`).

/// Class-2 kind: the fleet-membership truth for generation admissibility and
/// wrap targeting — one entry per device id, per-id monotone lattice with
/// `Removed` absorbing (`fauna_core::generation::DeviceSetRecord` owns the
/// join + its laws). **Frozen** (see the module header).
pub const KIND_DEVICE_SET: &str = "fauna.state.device-set";

/// Class-2 kind: one immutable-by-lattice entry per generation, logical key =
/// the content-derived generation id; the shred marker is the in-value
/// absorbing `Shredded` state (`fauna_core::generation::GenerationMintRecord`).
/// **Frozen** (see the module header).
pub const KIND_GENERATION_MINT: &str = "fauna.state.generation-mint";

/// Class-2 kind: the top-up / re-escrow wrap vehicle, logical key =
/// `"<generation-id-hex>/<target-device-id-hex>"`; whole-record LWW (any
/// valid wrap serves; the tombstone is the succession burn vehicle).
/// **Frozen** (see the module header).
pub const KIND_GENERATION_WRAP: &str = "fauna.state.generation-wrap";

/// Class-2 kind: an identity's published X-Wing escrow target, written once
/// per identity by a seed-holding surface (deterministic from the seed, so
/// concurrent writers agree byte-for-byte — Immutable), at the per-identity
/// key [`escrow_target_identity_key`] (`identity/<actor-id-hex>`): a successor
/// publishes its own row beside the predecessor's (the succession rider,
/// 2026-09-28). Additional holder targets land additively as sibling rows
/// keyed by holder id.
/// **Frozen** (see the module header).
pub const KIND_ESCROW_TARGET: &str = "fauna.state.escrow-target";

/// The per-identity logical keys of the two escrow kinds — the registry's
/// word for the grammar `fauna_core::generation` owns (`identity/<actor-id-hex>`
/// for a target row; `<generation>/<holder>/<actor-id-hex>` for a receipt).
pub use fauna_core::generation::{escrow_receipt_cell_key, escrow_target_identity_key};

/// Class-2 kind: one holder-signed durable escrow receipt per
/// (generation id, holder, identity), logical key =
/// [`escrow_receipt_cell_key`] — **the row the writer door's tip
/// resolution checks**, counting only receipts whose signed target key is the
/// observer's own identity's (escrow status is merged plane state, never a
/// live nest query, so the check works offline and no-nest identically).
/// Holder-generic by contract: `holder_id` selects the verification profile
/// (v1: the pinned nest deployment identity), never assumed.
/// **Frozen** (see the module header).
pub const KIND_ESCROW_RECEIPT: &str = "fauna.state.escrow-receipt";

/// Class-2 kind: the target-authored "cannot key generation G" signal (
/// the cure for the durable in-member corrupted-inline-wrap partition),
/// logical key = `"<generation-id-hex>/<target-device-id-hex>"`, one cell per
/// (generation, target), authored ONLY by the target: rows verify under the
/// target's own device key, ranked (verifies-at-cell, signed stamp, bytes)
/// (`fauna_core::generation::GenerationUnkeyableRecord` owns the record, the
/// anti-churn evidence contract, and the join).
/// **Frozen** (see the module header).
pub const KIND_GENERATION_UNKEYABLE: &str = "fauna.state.generation-unkeyable";

/// Class-2 kind: the target-authored "I hold these generations" statement —
/// the possession evidence the fleet-scope reclamation ruling rests on
/// (`account-data-taxonomy.md` § The generation machinery → the reach kind's
/// bullet + *Fleet-scope reclamation*). Logical key = the device id hex, one
/// cell per device, authored ONLY by that device: rows verify under the
/// device's own key, ranked (verifies-at-cell, signed stamp, bytes)
/// (`fauna_core::generation::DeviceReachRecord` owns the record and the
/// join). Coverage evidence for the top-up pass, the healer's licence to
/// retire its own cells. **Frozen** (see the module header).
pub const KIND_DEVICE_REACH: &str = "fauna.state.device-reach";

/// Class-2 kind: the remover's "seal nothing more under G" statement
/// (`account-data-taxonomy.md` § The generation machinery → the closed kind's
/// bullet + *The mint protocol, trigger (b)*). Logical key = the generation
/// id hex, one cell per generation, written by the device that writes
/// another device's `Removed` row. **The row's presence is the statement**:
/// nothing in the value is consulted
/// (`fauna_core::generation::GenerationClosedRecord` is audit only), so the
/// kind carries no signature and merges by byte-order max. The tip resolver
/// takes no generation a row names as a sealing candidate.
/// **Frozen** (see the module header).
pub const KIND_GENERATION_CLOSED: &str = "fauna.state.generation-closed";

/// Class-2 kind: the member's T20 **group-reception keypair** — the secret
/// half of the cross-account wrap target every storage group seals to
/// (`fauna_core::group_generation::GroupReceptionKeyRecord`; scheme owner:
/// `key-material-hierarchy.md` § Audience: a storage group). Logical key =
/// a digest of the public half; one Immutable row per keypair — rotation (on
/// the member's own fleet mint) writes a fresh row, and old rows are
/// retained because old generations' wraps still target old keys. An
/// ACCOUNT-plane kind on purpose: custody, device-removal severance, and
/// escrow of the reception secret ride the member's own R14 machinery —
/// which is also why it is the schedule's second `GenerationTip` kind, never
/// `Gen0` (a stolen device must lose reach at the next fleet mint). The
/// group plane's own kinds live in `crate::group_state`, not here.
/// **Frozen** (see the module header).
pub const KIND_GROUP_RECEPTION_KEY: &str = "fauna.state.group-reception-key";

/// Class-2 kind: a member's **held group machinery roots** — one row per
/// storage group this account belongs to, logical key = the scope id hex
/// (`fauna_core::group_generation::GroupHeldRootRecord`; custody ruling on
/// the record's doc + `key-material-hierarchy.md` § Audience: a storage
/// group). Immutable — a machinery root never rotates. Fleet-only +
/// tip-sealed for exactly the reception-key kind's reason: the root must be
/// readable by the member's whole fleet and fall out of a stolen device's
/// reach at the next fleet mint. Held group *generation* keys deliberately
/// have no kind here — their wraps rest in the group plane and re-open on
/// demand. **Frozen** (see the module header).
pub const KIND_GROUP_MACHINERY_ROOT: &str = "fauna.state.group-machinery-root";

/// Class-2 kind: the **private contact overlay** — the owner's own nickname,
/// notes and labels on another person, which nobody else ever sees
/// (`fauna_core::contact_overlay::ContactOverlay` owns the record and its
/// per-register join; `contacts.md` § The private overlay owns the concept).
/// Logical key = the other person's lowercase 64-hex actor id
/// (`fauna_core::contact_overlay::overlay_key`), one item per person, NOT
/// gated on a contact edge. Fleet-only and tip-sealed on the merits
/// (`account-data-taxonomy.md` § The audience ladder → *The contact-overlay
/// rung*): no grant-shaped consumer exists, and notes are exactly what a
/// stolen device should lose reach to at the next fleet mint.
/// **Frozen** (see the module header).
pub const KIND_CONTACT_OVERLAY: &str = "fauna.state.contact-overlay";

/// Class-2 kind: this account's **offline group-share ceremony record** —
/// its in-flight and completed co-present share initiations, both sides
/// (`fauna_core::group_ceremony::GroupShareConfig` owns the record and its
/// per-(scope, counterparty) join; `p2p.md` § Offline share initiation owns
/// the ceremony). **One item per ceremony side-record, never one per
/// account** (`config-dissolution.md` § Phases and gates → *Bounded rows*:
/// a record's size is fixed by the crypto suite, their number grows with
/// use). Key grammar (`fauna_core::group_ceremony::GroupShareRowKey` owns
/// it): `initiated/<scope hex32>/<recipient actor hex32>` holds one
/// `InitiatedGroupShare`, `invited/<scope hex32>` one `InvitedGroupShare`;
/// any other key is `BadValue`, and so is a value naming a different
/// ceremony than its key. The composite `GroupShareConfig` is the READ fold
/// over the account's rows.
/// The E3 lead slice of the `__config` dissolution
/// (`config-dissolution.md` § The `__config` dissolution schedule — the
/// kinds table's first row): born plane-only. Fleet-only
/// (`account-data-taxonomy.md` § The audience ladder → *The `UserConfig`
/// disposition*: an initiated record holds the scope's machinery root until
/// its plane row lands, so no grantee may ever open it) and tip-sealed
/// (`owner-key-material.md` § Path A-sibling-2: no fleet-only production
/// entry seals under the root-derivable-forever branch) — the same tip that
/// seals the ceremony's own [`KIND_GROUP_MACHINERY_ROOT`] row, so the offline
/// ceremony gains no new tip precondition. **Frozen** (see the module
/// header).
pub const KIND_GROUP_SHARE_CEREMONY: &str = "fauna.state.group-share-ceremony";

/// Class-2 kind: this account's **backup-destination state** — the
/// destination list and the unattested marks the succession aftermath raised
/// on carried-across destinations (`fauna_core::backup_state` owns the
/// records, their key grammar and their join; `backup-destinations.md` owns
/// the destinations, `succession-aftermath.md` § Re-key scope the marks). One
/// kind because they share one merge-time invariant (`config-dissolution.md`
/// P2): a `Removed` mark prunes the destination it names. Key grammar
/// (`fauna_core::backup_state::BackupRowKey` owns it, *Bounded rows*):
/// `destinations/<source nest hex64>` holds ONE destination-list row per
/// source box (a list is a fact about one box — `backup-destinations.md`
/// § *Destination data model*) — whole-record latest-wins on its embedded
/// stamp, its text fields and encoded size capped, so it is bounded by
/// construction — and `mark/<destination id>/<predecessor hex64>` one mark
/// each (an adjudicated mark is kept for good, the ledger ruling's reason for
/// per-mark rows). Any other key is `BadValue`, and so is a row whose value
/// names another key (a list another box's, a mark another mark's). The
/// composite `BackupState` is the READ fold for one box, which prunes that
/// box's list against every mark of the account. Fleet-only
/// (`account-data-taxonomy.md` § The audience ladder → *The `UserConfig`
/// disposition*) and tip-sealed (`owner-key-material.md` § Path
/// A-sibling-2): a destination row is exactly what a seed thief plants, so a
/// stolen device must fall out of its reach at the next fleet mint.
/// **Frozen** (see the module header).
pub const KIND_BACKUP: &str = "fauna.state.backup";

/// Class-2 kind: this account's **DNS management record** — the held
/// DNS-provider credentials, the "Fauna controls DNS" opt-ins, the ACME
/// account, the renewal delegations and opt-outs, the withdraw-aware publish
/// memory and the in-flight manual issuance (`fauna_core::data::DnsConfig`
/// owns the value; `dns-management.md` owns the concept, `tls-certificates.md`
/// the issuance half). **One item per account, at logical key `self`**
/// (`config-dissolution.md` § Phases and gates → *Bounded rows*: a settings
/// cluster whose size its own shape fixes, pinned by a size test under half
/// the per-entry cap). Whole-record latest-wins (`theirs_wins`, the stamp
/// being the entry's `LwwStamp`) —
/// and tolerant decode: LWW adopts the bytes verbatim, so a newer writer's
/// field survives an older replica. Fleet-only on the charter's own proof
/// (the [`POLICIES`] doc: provider credentials are secret, whatever their merge
/// axis says) and tip-sealed (`owner-key-material.md` § Path A-sibling-2: no
/// fleet-only production entry seals under the root-derivable-forever branch;
/// a stolen device must lose reach to the provider credentials at the next
/// fleet mint). An E3 slice of the `__config` dissolution
/// (`config-dissolution.md` § The `__config` dissolution schedule — the kinds
/// table's `fauna.state.dns` row). **Frozen** (see the module header).
pub const KIND_DNS: &str = "fauna.state.dns";

/// The one logical key a [`KIND_DNS`] row lives at.
pub const DNS_ROW_KEY: &str = "self";

/// Class-2 kind: this account's **minted ATProto app credentials** — the
/// client-custodied half of each credential, its secret among them, which the
/// settings page re-reveals (`fauna_core::data::AtprotoAppCredential` owns the
/// value; `atproto-pds-full.md` owns the concept). **One item per credential,
/// at logical key `<credential_id>`, never one per account**
/// (`config-dissolution.md` § Phases and gates → *Bounded rows*: the list grows
/// by one per mint with no cap, and every entry carries key material, so each
/// credential is its own bounded row); `AtprotoConfig` is the READ fold over
/// the account's rows, and a row's value must name its own key.
/// Latest-wins per credential, ordered by the entry's `LwwStamp`: a mint is a
/// put, a revoke a stamped tombstone. That is whole-record
/// `theirs_wins` narrowed to the credential — a revoke still
/// propagates, and two devices minting concurrently no longer lose one
/// another's secret. Tolerant decode: LWW adopts the bytes verbatim, so a
/// newer writer's field survives an older replica. Fleet-only on the
/// charter's own proof (the [`POLICIES`] doc: the secrets authenticate to the
/// account's PDS) and tip-sealed (`owner-key-material.md` § Path A-sibling-2:
/// a stolen device must lose reach to the secrets at the next fleet mint). An
/// E3 slice of the `__config` dissolution (`config-dissolution.md` § The
/// `__config` dissolution schedule — the kinds table's `fauna.state.atproto`
/// row). **Frozen** (see the module header).
pub const KIND_ATPROTO: &str = "fauna.state.atproto";

/// Class-2 kind: this account's **ATProto identity custody** — the
/// client-only-resident PLC rotation keys (each a P-256 scalar the user's
/// devices alone hold, the senior key of every DID the account minted), the
/// user's tombstone consents and contest intents, and the DIDs the nest ever
/// named (`fauna_core::data::AtprotoIdentityConfig` owns the value;
/// `atproto-pds-bridge.md` owns the concept). **One item per list element,
/// never one per account** (`config-dissolution.md` § Phases and gates →
/// *Bounded rows*: a rotation key per mint, a consent per retirement, an
/// intent per contest, a DID per box claim — four lists that grow with use,
/// one carrying key material): keys `key/<pubkey_did_key>`, `consent/<did>`,
/// `intent/<did>/<contested_op_cid>`, `named/<did>`, the grammar
/// `fauna_core::atproto_identity_rows` owns; the composite is the READ fold
/// over the account's rows, and a row's value must name its own key.
/// Per-field CRDT: the arm is the per-element half of
/// `AtprotoIdentityConfig::merge` (the shipped union — a rotation key is
/// irrecoverable, so every distinct key survives; bindings union, the earlier
/// instant wins); nothing is ever removed, so the kind has no tombstone.
/// Decode refuses an unknown field (P4). Fleet-only on the charter's own
/// proof (the [`POLICIES`] doc: the scalars ARE the account's recovery
/// authority over its DIDs) and tip-sealed (`owner-key-material.md` § Path
/// A-sibling-2: a stolen device must lose reach to the keys at the next fleet
/// mint). An E3 slice of the `__config` dissolution (`config-dissolution.md`
/// § The `__config` dissolution schedule — the kinds table's
/// `fauna.state.atproto-identity` row). **Frozen** (see the module header).
pub const KIND_ATPROTO_IDENTITY: &str = "fauna.state.atproto-identity";

/// Class-2 kind: this account's **mail custody** — the MLS Storage Encryption
/// Key (irrecoverable: losing it loses stored mail, `key-material-hierarchy.md`
/// § Path B), its cap-2 grace window with retirements, the succession burns,
/// the rotation sentinel, the three enablement flags, and one row per MUA
/// credential with its raw secret (`fauna_core::mail_rows` owns the rows,
/// their key grammar and their joins; `mail-credentials.md` owns the
/// concept). **Two row families, the key's first segment dispatching**
/// (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The mail
/// plane*): `self` → the ONE `MailStateRow` (bounded by construction; the
/// burns grow one per succession ceremony), `credential/<credential_id>` →
/// one `MailCredential`, the value naming its key — the credential list grows
/// with use and every row carries a secret, so each is its own row. Per-field
/// CRDT: the state row's arm is the MSEK present-wins / rotation-LWW, the
/// capped window and the burn min-union shared with `MailConfig::merge`, then
/// the recreatable four latest-wins on the row's own stamp; a credential
/// row's arm ORs the two monotone markers (burned, revoked — a marked join
/// carries an empty secret) and takes the remainder on its own stamp. No
/// deletion: a revoke is a marker, and the kind has no tombstone. Decode
/// refuses an unknown field (P4). Fleet-only on the charter's own proof (the
/// [`POLICIES`] doc: the MSEK and the MUA secrets are the account's mail
/// authority) and tip-sealed (`owner-key-material.md` § Path A-sibling-2: a
/// stolen device must lose reach to the MSEK and the MUA secrets at the next
/// fleet mint). An E3 slice of the `__config` dissolution
/// (`config-dissolution.md` § The `__config` dissolution schedule — the kinds
/// table's `fauna.state.mail` row). **Frozen** (see the module header).
pub const KIND_MAIL: &str = "fauna.state.mail";
/// Class-2 kind: this account's **custody-ceremony state** — the
/// record-then-act capture of every custody ceremony it runs, both sides: as
/// OWNER (granting custody: the signed offer, the host's accept, the mint and
/// deliver progress, the custodian's latest verified receipt) and as HOST
/// (holding custody: the offer, its own accept, the owner's deliver with the
/// witness, its latest minted receipt, the local decline/reclaim marks and
/// its Stop/budget) (`fauna_core::custody_ceremony` owns the records and
/// their join; `account-data-plane.md` § Replica posture → *The custody
/// grant + ceremony* owns the concept). **One item per ceremony side-record, never
/// one per account** (`config-dissolution.md` § Phases and gates → *Bounded
/// rows*: a record holds its signed envelopes verbatim and their number grows
/// with every custody granted or offered). Key grammar
/// (`fauna_core::custody_ceremony_rows::CustodyRowKey` owns it):
/// `granted/<grant id hex32>` holds one `GrantedCustody`, `held/<grant id
/// hex32>` one `HeldCustody`; any other key is `BadValue`, and so is a value
/// naming a different ceremony than its key, or one that does not re-encode to
/// its own bytes (the strict plane decode — the value types stay tolerant, so they
/// cannot take `deny_unknown_fields`). The composite
/// `CustodyConfig` is the READ fold over the account's rows. Per-field CRDT:
/// the arm is the per-record half of `CustodyConfig::merge` (the shipped
/// union — progress marks OR, envelopes non-empty-wins, the freshest receipt
/// with its own written/posted mark); nothing is ever removed, so the kind has
/// no tombstone. Fleet-only (`account-data-taxonomy.md` § The audience ladder
/// → *The `UserConfig` disposition*: which custodies the account runs, and
/// with whom, is no grantee's business) and tip-sealed (`owner-key-material.md`
/// § Path A-sibling-2: no fleet-only production entry seals under the
/// root-derivable-forever branch; the same tip already seals the ceremony's
/// registry rows, [`KIND_CUSTODIAN_ENDPOINTS`] and [`KIND_CUSTODIES_HELD`], so
/// the ceremony gains no new tip precondition). An E3 slice of the `__config`
/// dissolution (`config-dissolution.md` § The `__config` dissolution schedule
/// — the kinds table's `fauna.state.custody-ceremony` row). **Frozen** (see
/// the module header).
pub const KIND_CUSTODY_CEREMONY: &str = "fauna.state.custody-ceremony";

/// Class-2 kind: this account's **succession ledger** — the owner chain, the
/// signed capability-grant log and the grant / member / filter adjudication
/// marks a succession raises (`fauna_core::succession_ledger` owns the values,
/// their key grammar and their joins; `config-dissolution.md` § Phases and
/// gates → *Bounded rows* → *The ledger* owns the shape). **Five row families
/// under one kind, the key's first segment dispatching**
/// (`fauna_core::succession_ledger::SuccessionLedgerRowKey` owns the
/// grammar): `chain` — the ONE owner-chain row, the one row that grows (34 B
/// per succession ceremony); `event/<grant id hex32>/<sig hex128>` — one
/// signed `GrantEvent`, write-once; `grant-mark/<grant id hex32>/<predecessor
/// hex64>`, `member-item/<person hex64>/<predecessor hex64>/<reason>` (the
/// reason is the key's tail) and `filter-mark/<filter id decimal>/<predecessor
/// hex64>` — one mark each. Any other key is `BadValue`, and so is a value not
/// naming its key or not re-encoding to its own bytes. The composite
/// `SuccessionLedger::fold` is the READ fold, and the allowed-signer clause
/// runs there, over the folded chain.
/// The E3 third slice of the `__config` dissolution (`config-dissolution.md`
/// § The `__config` dissolution schedule — the kinds table's row): the rows join
/// through the per-row half of `SuccessionLedger::merge`. Fleet-only (`account-data-taxonomy.md` § The audience
/// ladder → *The `UserConfig` disposition*: the owner's forensic log and
/// adjudications are no grantee's business) and tip-sealed
/// (`owner-key-material.md` § Path A-sibling-2: no fleet-only production entry
/// seals under the root-derivable-forever branch; pre-succession rows stay
/// under the generations the succession rider re-escrows, never re-sealed).
/// **Frozen** (see the module header).
pub const KIND_SUCCESSION_LEDGER: &str = "fauna.state.succession-ledger";

/// Class-2 kind: this account's **subscription-tier period keys** — the
/// author-client-minted broadcast keys each tier's `KeyBlob` wraps to its
/// subscribers, and the staged subscriber-removal rotations
/// (`fauna_core::subscription_rows` owns the rows and their key grammar;
/// `fauna_core::data::SubscriptionsConfig` the composite;
/// `key-material-hierarchy.md` § Audience: an opaque set of subscriber
/// pubkeys owns the concept). **One item per period key and one per staged
/// removal, never one per account** (`config-dissolution.md` § Phases and
/// gates → *Bounded rows*: a tier's period history is uncapped by design and
/// every removal rotates, so a per-account row would outgrow the per-entry
/// cap). Key grammar: `period/<digest hex32>` holds one `TierPeriodRow`,
/// `removal/<digest hex32>` one `PendingRemovalRow`, the digest being the
/// record's own content digest (the whole period row; the removal without
/// its `settled` marker) — two devices rotating one tier concurrently mint
/// two periods at one version, and both keys must survive. Any other key is
/// `BadValue`, and so is a value whose digest is not its key. The composite
/// `SubscriptionsConfig` is the READ fold over the account's rows through the
/// shipped rule (`SubscriptionsConfig::merge`, the per-tier union); the plane
/// arm is its per-row half — a period row is write-once, a removal row ORs its
/// `settled` marker (a CRDT kind has no deletion, so a sentinel is settled,
/// never dropped). Fleet-only (`account-data-taxonomy.md` § The audience
/// ladder → *The `UserConfig` disposition*: the keys decrypt every paid post,
/// so no grantee may hold them) and tip-sealed (`owner-key-material.md`
/// § Path A-sibling-2: a stolen device must lose reach to the period keys at
/// the next fleet mint). An E3 slice of the `__config` dissolution
/// (`config-dissolution.md` § The `__config` dissolution schedule — the kinds
/// table's `fauna.state.subscriptions` row). **Frozen** (see the module
/// header).
pub const KIND_SUBSCRIPTIONS: &str = "fauna.state.subscriptions";

/// Class-2 kind: this account's **shared-folder content-key custody** — every
/// set's content-key generation history, its staged member-removal rotations
/// and the foreign sets it is a member of (`fauna_core::folder_key_rows` owns
/// the key grammar and the rows' decode; `FoldersConfig::merge` owns the join;
/// `config-dissolution.md` § Phases and gates → *Bounded rows* owns the
/// shape). **Four row families under one kind, the key's first segment
/// dispatching** (`fauna_core::folder_key_rows::FolderKeyRowKey`):
/// `set/nonce/<hex64>` or `set/channel/<hex64>` — one custody entry's metadata,
/// keyed by the merge's identity (the nonce, else the channel);
/// `gen/<digest hex64>` — ONE content-key generation and its set, write-once,
/// so the uncapped, never-prunable history is bounded per row;
/// `removal/<digest hex64>` — one staged removal and its monotone `settled`
/// marker (the subscriptions removal row's shape); `foreign/<channel hex64>` —
/// one foreign set. Any other key is `BadValue`, and so is a value not naming
/// its key or not re-encoding to its own bytes. The composite
/// `FoldersConfig::fold` is the READ fold (a settled removal leaves it).
/// The E3 slice of the `__config` dissolution (`config-dissolution.md` § The
/// `__config` dissolution schedule — the kinds table's row), born plane-only:
/// the rows join through `FoldersConfig::merge`. Fleet-only (`account-data-taxonomy.md` § The
/// audience ladder → *The `UserConfig` disposition*: content keys are the
/// chunk-crypto root of every shared set, no grantee's business) and
/// tip-sealed (`owner-key-material.md` § Path A-sibling-2).
/// **Frozen** (see the module header).
pub const KIND_FOLDER_KEYS: &str = "fauna.state.folder-keys";

/// Class-2 kind: this admin identity's **multi-nest deployment-seed
/// custody** — one entry per box it administers, holding the box's 32-byte
/// Ed25519 deployment seed, the one piece of a nest that cannot be regenerated
/// (`nest/box-recovery.md` § Trust & audience owns the concept;
/// `fauna_core::deployment_seed_rows` owns the rows, their key grammar and the
/// strict decode; `fauna_core::data::DeploymentSeedEntry` the value). **One
/// item per custodied box, never one per account** (`config-dissolution.md`
/// § Phases and gates → *Bounded rows*: the map grows with every box
/// administered and a rotated box's predecessor stays custodied, marked, so a
/// per-account row would be bounded by use, not by shape). Key: the box's
/// `nest_actor_id`, lowercase hex64; any other key is `BadValue`, and so is a
/// value that names another box, carries a seed that is not its id's
/// preimage, does not re-encode to its own bytes, or carries a field this
/// build does not know (the strict plane decode — the value type keeps its
/// `extra` catch-all, so it cannot take `deny_unknown_fields`).
/// The composite map is the READ fold over the account's rows through the
/// shipped rule (`DeploymentSeedEntry::merge_seed_map`, the per-box union); the
/// plane arm is its per-row half, `DeploymentSeedEntry::fold_from` (the label
/// join, present-wins supersession). Fleet-only (`account-data-taxonomy.md`
/// § The audience ladder → *The `UserConfig` disposition*: a seed re-creates a
/// box's identity, so no grantee may hold it) and tip-sealed
/// (`owner-key-material.md` § Path A-sibling-2: a stolen device must lose
/// reach to the seeds at the next fleet mint). An E3 slice of the `__config`
/// dissolution (`config-dissolution.md` § The `__config` dissolution schedule
/// — the kinds table's `fauna.state.deployment-seeds` row). **Frozen** (see
/// the module header).
pub const KIND_DEPLOYMENT_SEEDS: &str = "fauna.state.deployment-seeds";

/// Class-2 kind: this owner's **peer-anchor cache** — the chain heads and
/// harvested home domains the fleet holds for OTHER identities, the durable
/// anchors the succession witness checks a peer's statement against
/// (`identity-succession.md` § The succession statement → *the peer-profile
/// harvest* owns the concept; `fauna_core::peer_anchor_rows` owns the rows,
/// their key grammar and the strict decode; `fauna_core::data::PeerAnchors`
/// the composite). **One item per anchored actor per vector, never one per
/// account** (`config-dissolution.md` § Phases and gates → *Bounded rows*:
/// the two vectors run to `MAX_PEER_ANCHOR_ENTRIES` each, ~89 KiB at the
/// ceiling, over the per-entry cap). Keys: `head/<actor hex64>` holds one
/// `PeerChainHead`, `domain/<actor hex64>` one `PeerAnchorDomain`; any other
/// key is `BadValue`, and so is a value that names another actor, falls
/// outside the writers' shape (a 32-byte RecoveryKey; a non-empty host within
/// the hostname bound), or does not re-encode to its own bytes (the strict
/// plane decode — the value types stay tolerant). The composite
/// is the READ fold over the account's rows through the shipped rule
/// (`PeerAnchors::merge`), which is where the one ceiling and the one order
/// (P2) now hold; the plane arm is its per-actor half (`PeerChainHead::
/// join_from`: max `seq`, the byte-smaller key at an equal one, the same-head
/// `outrun` OR, min `first_seen`; `PeerAnchorDomain::join_from`: the
/// lexically smaller domain, min `first_seen`). Never LWW although every row
/// is recreatable: a latest-wins arm would let a behind replica rewind an
/// anchor, the reset the rewrite guard exists to refuse. Fleet-only
/// (`account-data-taxonomy.md` § The audience ladder → *The `UserConfig`
/// disposition*: who this owner talks to, and the anchors that decide whose
/// succession it believes, are no grantee's business) and tip-sealed
/// (`owner-key-material.md` § Path A-sibling-2: a stolen device must lose
/// reach to the anchors — whom this owner talks to — at the next fleet mint). An
/// E3 slice of the `__config` dissolution (`config-dissolution.md` § The
/// `__config` dissolution schedule — the kinds table's
/// `fauna.state.peer-anchors` row). **Frozen** (see the module header).
pub const KIND_PEER_ANCHORS: &str = "fauna.state.peer-anchors";

/// Class-2 kind: this owner's **per-nest blessing verdicts** — the "keep this
/// box's read grants renewed without me" choice (`docs/goal/ui/nests.md`
/// § Expiry / renewal → *Duration and blessing* owns the concept;
/// `fauna_core::blessed_nest_rows` owns the rows, their key grammar and the
/// strict decode; `fauna_core::data::BlessedNest` the value). **One item per
/// nest, never one per account** (`config-dissolution.md` § Phases and gates
/// → *Bounded rows*: the list grows by one entry per nest ever blessed or
/// un-blessed, and an un-blessed entry is kept so the newer verdict can win a
/// merge). Key: the lowercase hex64 of the nest's 32-byte id, holding one
/// `BlessedNest`; any other key is `BadValue`, and so is a value that names
/// another nest, holds an id that is not 32 bytes, or does not re-encode to
/// its own bytes (the strict plane decode — the value type stays tolerant).
/// The composite list is the READ fold over the account's rows
/// through the shipped rule (`merge_blessed_nests`); the plane arm is its
/// per-nest half (`BlessedNest::join`: the newer `at` wins, an equal `at`
/// goes to the UN-blessed side). Never LWW by row stamp: the rule's tie goes
/// to the more restrictive verdict, which a generic latest-wins arm cannot
/// express. Fleet-only (`account-data-taxonomy.md` § The audience ladder →
/// *The `UserConfig` disposition*: which boxes the owner trusts to renew
/// their own access is no grantee's business) and tip-sealed
/// (`owner-key-material.md` § Path A-sibling-2: a stolen device must lose
/// the ability to read which boxes are blessed — and to forge an
/// un-blessed→blessed flip seal — at the next fleet mint). An E3 slice of the
/// `__config` dissolution (`config-dissolution.md` § The `__config`
/// dissolution schedule — the kinds table's `fauna.state.blessed-nests`
/// row). **Frozen** (see the module header).
pub const KIND_BLESSED_NESTS: &str = "fauna.state.blessed-nests";

/// Class-2 kind: this account's **refused inbound scheduling changes** — the
/// notices the Events page lists when someone whose message may not change
/// the user's calendar tried to (`inbound-scheduling-authority.md`
/// § *Where the record rests* owns the record; `fauna_core::data::
/// RefusedSchedulingChanges` the value and its merge;
/// `fauna_core::refused_change_rows` the row, its key and the strict decode).
/// **One row per account, at key `self`**: the list is bounded by
/// construction — at most `MAX_REFUSED_SCHEDULING_CHANGES` rows, three per
/// author, every string at its byte ceiling, within the 32 KiB budget a
/// `const` assertion keeps honest — so the default key shape holds
/// (`config-dissolution.md` § Phases and gates → *Bounded rows*). Any other
/// key is `BadValue`, and so is a value that does not re-encode to its own
/// bytes, carries a row field this build does not know, or is not already
/// held under the ceilings (the strict plane decode — the row type keeps its
/// `extra` catch-all, so it cannot take `deny_unknown_fields`).
/// Per-field CRDT: the arm is `RefusedSchedulingChanges::merge` — the per-key union through the lexicographic
/// `RefusedSchedulingChange::absorb` (the later attempt wins whole, a tie
/// joins field by field), then the two ceilings; that per-key order is what
/// keeps the capped merge associative on bytes. No tombstone: a dismissal is a
/// marker on its row. Fleet-only (`account-data-taxonomy.md` § The audience
/// ladder → *The `UserConfig` disposition*: who tried to change the user's
/// calendar is no grantee's business) and tip-sealed (`owner-key-material.md`
/// § Path A-sibling-2: every E3 kind seals under the generation tip). An E3
/// slice of the `__config` dissolution (`config-dissolution.md` § The
/// `__config` dissolution schedule — the kinds table's
/// `fauna.state.refused-scheduling-changes` row). **Frozen** (see the module
/// header).
pub const KIND_REFUSED_SCHEDULING_CHANGES: &str = "fauna.state.refused-scheduling-changes";

/// Class-2 kind: this account's **followed public folders** — the entire
/// client-side state of a publicly-synced follow (`fauna_core::data::
/// FollowedFolder` owns the value; `folders.md` § Publicly-synced follow owns
/// the concept: the home nest holds no follower state, so this row IS the
/// follow). **One item per followed folder, at logical key
/// `FollowedFolder::plane_key` — the blake3 digest of `home_nest_url` (hex),
/// `/`, the `folder_id` — never one per account** (`config-dissolution.md`
/// § Phases and gates → *Bounded rows*: the list grows by one per follow with
/// no cap, so each follow is its own bounded row); `FollowsConfig` is the READ
/// fold over the account's rows, and a row's value must name its own key.
/// Latest-wins per follow, ordered by the entry's `LwwStamp`: a follow (or its
/// refresh) is a put, an unfollow a stamped tombstone. That is whole-record
/// `theirs_wins` narrowed to one folder — an unfollow
/// still propagates (a tombstone-less union would resurrect it), and two
/// devices following different folders concurrently no longer drop one of
/// them. Tolerant decode: LWW adopts the bytes verbatim, so a newer writer's
/// field survives an older replica. Fleet-only (the [`POLICIES`] rung default:
/// what the user follows is theirs, and no grantee is argued for) and
/// tip-sealed by the dissolution's own rule (`config-dissolution.md` § The
/// kinds: every E3 kind seals under `GenerationTip`). An E3 slice of the
/// `__config` dissolution (`config-dissolution.md` § The `__config`
/// dissolution schedule — the kinds table's `fauna.state.follows` row).
/// **Frozen** (see the module header).
pub const KIND_FOLLOWS: &str = "fauna.state.follows";

/// Class-2 kind: this account's **npub confirmation stamp** — when the owner
/// last confirmed, after a key succession, that the Nostr page shows their
/// npub (`nostr.md` § Key succession and rotation, leg 3;
/// `fauna_core::nostr_confirmation::NostrConfirmation` owns the value and its
/// join). **One item per account, at logical key `self`**
/// ([`NOSTR_CONFIRMATION_ROW_KEY`] — one monotone stamp, bounded by its
/// shape). Per-field CRDT delegating to `NostrConfirmation::merge`, the
/// **max** (`None` below every
/// `Some`); strict decode (`deny_unknown_fields` — a newer writer's field is
/// `BadValue`, re-presented on the next reconcile). Fleet-only (the
/// [`POLICIES`] rung default: the stamp is the owner's own witness state,
/// `account-data-taxonomy.md` § The audience ladder → *The `UserConfig`
/// disposition*, and no grantee is argued for) and tip-sealed by the
/// dissolution's own rule (`config-dissolution.md` § The kinds: every E3 kind
/// seals under `GenerationTip`; a stolen device must lose even the account's
/// succession-aftermath state at the next fleet mint). An E3 slice of the
/// `__config` dissolution (`config-dissolution.md` § The `__config`
/// dissolution schedule — the kinds table's `fauna.state.nostr-confirmation`
/// row). **Frozen** (see the module header).
pub const KIND_NOSTR_CONFIRMATION: &str = "fauna.state.nostr-confirmation";

/// The one logical key a [`KIND_NOSTR_CONFIRMATION`] row lives at.
pub use fauna_core::nostr_confirmation::NOSTR_CONFIRMATION_ROW_KEY;

/// **The admitted-kinds overlay's carriage** — one row per third-party
/// principal whose manifest a consenting device admitted
/// (`third-party-kinds.md` § The kinds vocabulary → *The registry overlay*).
/// Logical key = the metadata document's `client_id`; value =
/// [`crate::kind_manifest::KindManifestRecord`] (the compact JWS verbatim plus
/// the admitting device's `admitted_at`). Whole-record LWW: a re-consent
/// re-writes the row with the document's current manifest, and nothing a
/// replica holds is worth merging field-wise — every reader re-verifies the
/// JWS against the key's host, so the overlay is never built from a value it
/// did not check (`fauna_account_plane::kind_manifest_rows`). Fleet-only by
/// the default-narrow rule: no grantee needs it (a principal holds its own
/// manifest), and which apps the user connected is the user's alone.
/// Tip-sealed by the same reach argument: a stolen device must lose the
/// account's connected-app roster at the next fleet mint. An ACCOUNT row on
/// departure — the overlay serves every device, whichever one consented. A
/// revoked principal's row stays: the user's rows under its kinds stay
/// readable. **Frozen** (see the module header).
pub const KIND_KIND_MANIFEST: &str = "fauna.state.kind-manifest";

/// The registered class-2 kinds: **three frozen columns** per kind — how it
/// merges, who can open it (R13, charter § The audience ladder), and which key
/// material seals it (R14, charter § The generation machinery). Audience and
/// epoch are orthogonal on purpose: the machinery kinds below are fleet-only
/// **and** `Gen0`, which is what dissolves the apparent self-gating paradox.
///
/// Grows one row per kind as kinds land — deliberately **not** pre-populated for
/// kinds whose value shape nobody has built yet, because a row here is a claim
/// that [`apply_class2`] knows how to merge that kind's bytes.
///
/// **The two columns are orthogonal and must never be conflated.** The charter's
/// proof: DNS provider credentials are *recreatable* (merge axis: whole-record
/// LWW) **and** *secret* (audience axis: fleet-only) — reusing a merge table as
/// an audience split ships credentials to grantees. Both columns freeze with the
/// kind string, for the same reason: the rung selects the key branch, so changing
/// it orphans every entry already sealed under the old branch.
///
/// **The rung default is [`AudienceRung::FleetOnly`]** — delegable is the
/// deliberate, argued exception. The asymmetry forces it: a kind can be
/// re-registered one rung wider later (a new kind string plus a re-seal), but a
/// kind admitted too wide has had its plaintext sealed under a key grants can
/// reach, and there is no quiet withdrawal.
const POLICIES: &[(&str, MergePolicy, AudienceRung, SealingEpoch)] = &[
    // 🪦 The `fauna.state.user-config` row stood here (fleet-only,
    // CrdtPerField, never sealable under the R14 gate) until E0 of the
    // dissolution schedule retired it — see the tombstone comment above the
    // table's kinds. Rows are added, never edited; a *retirement* is possible
    // only because nothing ever sealed under the string.
    (
        KIND_MODERATION,
        // Whole-record LWW, per the re-scoped W2.5: one kind per preference
        // sub-record, each merging independently (no cross-record invariant
        // spans the preference cluster).
        MergePolicy::LatestWins,
        // The charter's `UserConfig` disposition table places `moderation` in
        // the argued delegable set: secret-free, recreatable, legitimate grant
        // material. Its payload type carries a Secret-free pin
        // (`crate::secret_free`), which is what licenses this row.
        AudienceRung::Delegable,
        // Delegable ⇒ Gen0, by construction (the branch never gains a
        // generation axis); the invariant test below pins the implication.
        SealingEpoch::Gen0,
    ),
    // The three cluster siblings (W2.5 item 1). Same shape as `moderation` for
    // the same reasons, and each licensed by its own Secret-free pin in
    // `crate::secret_free` — the registration and the pin land in the same
    // edit, and the tests there refuse either one alone.
    (
        KIND_SYNC_PREFS,
        MergePolicy::LatestWins,
        AudienceRung::Delegable,
        SealingEpoch::Gen0,
    ),
    (
        KIND_PERSONALIZATION,
        MergePolicy::LatestWins,
        AudienceRung::Delegable,
        SealingEpoch::Gen0,
    ),
    (
        KIND_DELEGATION,
        MergePolicy::LatestWins,
        AudienceRung::Delegable,
        SealingEpoch::Gen0,
    ),
    (
        KIND_SEEN_SET,
        // Grow-only union — the charter's merge table names the seen-set as
        // the union-CRDT row, and `admits_tombstone` is `false` by policy:
        // membership never shrinks (A4 — compaction shrinks *bytes*, inside
        // the join, never the set).
        MergePolicy::CrdtPerField,
        // Delegable, argued (the ruling is on `KIND_SEEN_SET`'s doc and in
        // the charter § The audience ladder): materialization (R9) is the
        // concrete grant-shaped consumer of read-state, and no grant can
        // reach a fleet-only branch — while a kind can never quietly narrow
        // later, it also can never quietly *widen*, and read-state's whole
        // purpose is to feed granted derived views. Secret-free pin:
        // `crate::secret_free::pin_seen_scope_set`. Generation 0, like the
        // preference cluster — the R14 gate binds fleet-only kinds and
        // content scopes, not this.
        AudienceRung::Delegable,
        SealingEpoch::Gen0,
    ),
    (
        KIND_READ_MARKER,
        // A max-register: no stamp could order it safely (a stale value with
        // a fresh stamp would un-read a thread fleet-wide), so it brings its
        // own join, and `admits_tombstone` being `false` costs it nothing —
        // entries are kept for the life of the account.
        MergePolicy::CrdtPerField,
        // Delegable, argued (`KIND_READ_MARKER`'s doc). Secret-free pin:
        // `crate::secret_free::pin_read_marker`.
        AudienceRung::Delegable,
        SealingEpoch::Gen0,
    ),
    (
        KIND_DEVICE_ENDPOINTS,
        // Whole-record LWW per T5: each device supersedes only its own row
        // (the key is its writer id), and a removal tombstone is stamped and
        // orderable.
        MergePolicy::LatestWins,
        // Fleet-only — default-narrow with no argued need, and R14-gated
        // deliberately: the ruling is on `KIND_DEVICE_ENDPOINTS`'s doc and in
        // the charter § The audience ladder. No Secret-free pin: pins are for
        // delegable kinds only (the reverse-direction test refuses one here).
        AudienceRung::FleetOnly,
        // The first tip-sealed kind: the schedule build's proof consumer —
        // production sealing waits for an admissible, escrow-acked
        // generation tip. (The two W8 custody kinds below joined it
        // 2026-08-15, same location-data reasoning.)
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_CUSTODIES_HELD,
        // Whole-record LWW: one row per custody grant id (the key), and only
        // the holding side's engine supersedes its row; revocation/lapse
        // removal is a stamped, orderable tombstone.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed — the value carries the OWNER fleet's
        // NodeIds/candidates (location data); reasoning on the kind const.
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_CUSTODIAN_ENDPOINTS,
        // Whole-record LWW: one row per custody grant id (the key); the
        // owner-side ceremony writer supersedes it on re-exchange, and a
        // revoke removes it by stamped tombstone.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed — the custodian's location data; reasoning
        // on the kind const.
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_SHARE_ENDPOINTS,
        // Whole-record LWW: one row per (set, member) — the key. A member's
        // later advertisement supersedes their earlier one, from whichever
        // of their devices sent it: the share leg dials the *actor* NodeId,
        // so every candidate is only a path hint and a superseded one costs
        // at most a stale hint (WAN + relay stand regardless). A leave or
        // evict removes the row by stamped tombstone.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed — a counterparty's location data;
        // reasoning on the kind const.
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    // ── The R14 generation machinery: fleet-only rung + Gen0 epoch, all five
    // (bootstrap stratification — see the kind consts' docs). Merge arms for
    // the two lattice kinds delegate to `fauna_core::generation`.
    (
        KIND_DEVICE_SET,
        // Per-id monotone lattice (`Removed` absorbing) — a CRDT join in
        // bytes, owned and law-tested by `fauna_core::generation`.
        MergePolicy::CrdtPerField,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_GENERATION_MINT,
        // Immutable-by-lattice: `Shredded` is the absorbing in-value shred
        // marker (the charter's build ruling — CrdtPerField refuses T14
        // tombstones, so deletion is a lattice phase, not a tombstone).
        MergePolicy::CrdtPerField,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_GENERATION_WRAP,
        // Per-healer three-segment cells (the hardening, 2026-08-15)
        // rank (verifies-under-the-cell's-
        // healer, signed stamp, bytes) — a `BackupKey` holder without the
        // healer's Ed25519 secret can never displace or pre-empt an honest
        // healer's row — and a key that is not a per-healer cell is refused.
        // The join lives in `fauna_core::generation` (the lattice kinds'
        // pattern). Burn-at-succession is owed as an authenticated absorbing
        // `Burned` variant, never a tombstone (CrdtPerField refuses those,
        // and an unauthenticated tombstone was itself a forgeable-suppression
        // vector).
        MergePolicy::CrdtPerField,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_ESCROW_TARGET,
        // Deterministic from the identity seed — concurrent seed-holding
        // writers produce identical bytes, so first-wins is also only-wins.
        MergePolicy::Immutable,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_ESCROW_RECEIPT,
        // A receipt is a fact: re-deposit is idempotent per (generation id,
        // wrap hash), and any valid receipt satisfies tip resolution.
        MergePolicy::Immutable,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_GENERATION_UNKEYABLE,
        // One honest author per cell — the target — with a verifying-preferred
        // rank ((verifies-at-cell, signed stamp, bytes)); the join lives in
        // `fauna_core::generation` (the lattice kinds' pattern), decode-or-
        // fail with first-contact strictness in the adoption arm, exactly
        // the per-healer wrap cells' shape and for the same reasons.
        MergePolicy::CrdtPerField,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_DEVICE_REACH,
        // One honest author per cell — the device itself — the unkeyable
        // kind's rank and adoption: verifying-preferred rank, first-contact
        // strictness in the adoption arm (its join stays total: the record
        // is a struct, not a closed-by-design enum). Gen0 like every machinery
        // kind: it must be readable before any tip resolves, since it is
        // what tells healers whom to stop healing.
        MergePolicy::CrdtPerField,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_GENERATION_CLOSED,
        // Presence is the statement and no reader consults the value, so the
        // join is the plain byte-order max and adoption asks only for a
        // generation-id key and a value that decodes — no signature to rank
        // by. Gen0 like every machinery kind: the resolver reads it before
        // any tip resolves, which is the whole of what it is for.
        MergePolicy::CrdtPerField,
        AudienceRung::FleetOnly,
        SealingEpoch::Gen0,
    ),
    (
        KIND_GROUP_RECEPTION_KEY,
        // One row per keypair at a pubkey-digest key: a second value under
        // the same key is a different keypair, never a newer one.
        MergePolicy::Immutable,
        // Fleet-only + tip-sealed — the whole point (the kind const's doc):
        // the reception secret must fall out of a stolen device's reach at
        // the member's next fleet mint, which is exactly what tip-sealing
        // buys and Gen0's derivable-forever posture would forfeit.
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_GROUP_MACHINERY_ROOT,
        // One row per scope at the scope-id key; the root never rotates, so
        // a second value is never a newer one.
        MergePolicy::Immutable,
        // The reception-key kind's argument, verbatim (the kind const's
        // doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_CONTACT_OVERLAY,
        // Per-register last-writer, labels per-label presence registers —
        // irrecoverable user-authored text, so never whole-record LWW (a
        // nickname on one device and notes on another must both survive).
        // The join lives in `fauna_core::contact_overlay`.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc; no Secret-free pin
        // — pins are delegable-only).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_GROUP_SHARE_CEREMONY,
        // Per-(scope, counterparty) union — booleans OR, envelopes and the
        // held root non-empty-wins, the scalar remainder on the record's own
        // stamp. Never whole-record LWW: a record is the only durable copy of
        // a consumed peer-channel frame. The join lives on
        // `fauna_core::group_ceremony::GroupShareConfig::merge`.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_BACKUP,
        // Per row: the destination list latest-wins on its own stamp, each
        // mark the verdict-precedence minimum; the `Removed` prune runs at
        // the read fold. The rule lives on
        // `fauna_core::backup_state::BackupState::merge`.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_DNS,
        // Whole-record LWW (`theirs_wins`) ordered by the
        // entry's stamp. Recreatable: the
        // admin re-enters a credential, the machine re-publishes.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed (the kind const's doc: provider
        // credentials).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_ATPROTO,
        // LWW per credential row — `theirs_wins`
        // narrowed to one credential, a revoke a stamped tombstone.
        // Recreatable: the user revokes and re-mints.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed (the kind const's doc: app-credential
        // secrets).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_SUCCESSION_LEDGER,
        // Per-row: the `chain` row is the gated `one_owner_chain` union (a
        // fork refused), an event row is write-once, a mark row is the
        // verdict-precedence minimum. Never whole-record LWW: the log and the
        // decided marks are never dropped. The joins live in
        // `fauna_core::succession_ledger`.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_SUBSCRIPTIONS,
        // Per-row halves of the per-tier union — a period row write-once, a
        // removal row's `settled` marker OR'd. Never LWW: a period key is
        // irrecoverable, and a concurrent rotation's loser must survive.
        // The rule lives on `fauna_core::data::SubscriptionsConfig::merge`.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc: the period keys).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_FOLDER_KEYS,
        // Per-row halves of `FoldersConfig::merge`: a set row joins
        // field-wise, a generation row is write-once, a staging folds per
        // field with its `settled` marker OR'd, a foreign record folds per
        // field. Never whole-record LWW: a content key, once
        // distributed, cannot be regenerated.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_DEPLOYMENT_SEEDS,
        // Per-box half of the per-`nest_actor_id` union — the label join and
        // present-wins supersession of `DeploymentSeedEntry::fold_from`.
        // Never LWW: a seed is a box's irrecoverable identity, and a device's
        // absence must never clobber a peer's entry.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc: the seeds).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_PEER_ANCHORS,
        // Per-actor half of the two unions — `PeerChainHead::join_from` /
        // `PeerAnchorDomain::join_from`; the ceiling is the read fold's.
        // Never LWW: a behind replica must never rewind an anchor.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc: the anchors).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_BLESSED_NESTS,
        // Per-nest half of `merge_blessed_nests` — `BlessedNest::join`.
        // Never LWW: a tie goes to the un-blessed verdict.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc: the blessings).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_ATPROTO_IDENTITY,
        // Per-element union — `AtprotoIdentityConfig::merge`'s halves, one
        // row per element. Irrecoverable: the scalar exists nowhere else.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc: the senior
        // rotation keys).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_MAIL,
        // Never LWW: the MSEK is irrecoverable and merges present-wins; a
        // credential row's burn and revoke are monotone markers.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc: the MSEK and the
        // MUA secrets).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_FOLLOWS,
        // LWW per followed-folder row — `theirs_wins`
        // narrowed to one folder, an unfollow a stamped tombstone.
        // Recreatable: the user follows again.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_CUSTODY_CEREMONY,
        // Per-record union — `CustodyConfig::merge`'s halves, one row per
        // ceremony side-record. Never LWW: a record is the only durable copy
        // of a consumed MLS payload.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_NOSTR_CONFIRMATION,
        // Max of the two stamps — `NostrConfirmation::merge`. Recreatable:
        // the owner confirms again.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_REFUSED_SCHEDULING_CHANGES,
        // `RefusedSchedulingChanges::merge` — the per-key union and the two
        // ceilings it holds. Never LWW: a device's absence must not
        // clobber a sibling's notice, nor a stale copy undo a dismissal.
        MergePolicy::CrdtPerField,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
    (
        KIND_KIND_MANIFEST,
        // Whole-record: the latest consent's manifest, re-verified on read.
        MergePolicy::LatestWins,
        // Fleet-only + tip-sealed (the kind const's doc).
        AudienceRung::FleetOnly,
        SealingEpoch::GenerationTip,
    ),
];

/// The policy for `kind`, or `None` when this build does not know the kind.
///
/// `None` is a *compat* answer, not an error: a newer writer may seal entries
/// of a kind this binary predates, and the reader's contract is to leave them
/// alone (they re-present on the next full-state reconcile — charter § Feeds
/// and cursors, backstop 2), never to guess.
pub fn merge_policy(kind: &str) -> Option<MergePolicy> {
    POLICIES
        .iter()
        .find(|(k, _, _, _)| *k == kind)
        .map(|(_, policy, _, _)| *policy)
}

/// The registered audience rung for `kind`, or `None` when this build does not
/// know the kind (same compat answer as [`merge_policy`]).
pub fn audience_rung(kind: &str) -> Option<AudienceRung> {
    POLICIES
        .iter()
        .find(|(k, _, _, _)| *k == kind)
        .map(|(_, _, rung, _)| *rung)
}

/// The registered sealing epoch for `kind`, or `None` when this build does not
/// know the kind (same compat answer as [`merge_policy`]).
///
/// This column is what the writer door routes on since the step-6 landing
/// (`fauna_sync_engine::account_state_plane::admit_origination` + its seal
/// dispatch): `Gen0` seals form v1 under the gen-0 branch, `GenerationTip`
/// resolves the admissible escrow-acked tip and seals form v2 under it —
/// refusing only when no tip resolves, which IS the R14 gate while no
/// generation exists.
pub fn sealing_epoch(kind: &str) -> Option<SealingEpoch> {
    POLICIES
        .iter()
        .find(|(k, _, _, _)| *k == kind)
        .map(|(_, _, _, epoch)| *epoch)
}

/// The scope a production entry of `kind` seals into — the A5 partition's
/// routing rule (charter § The audience ladder + § The scope string): the
/// frozen `state` string *is* the delegable sub-scope, and every fleet-only
/// kind seals into `state-fleet` from its first row, so a delegable grantee's
/// subscription never observes fleet churn.
///
/// A third-party kind (`ext.<publisher>.<name>`) seals into its own
/// `ext:<kind>` scope, derived **from the string alone** — no registry row,
/// no overlay (`third-party-kinds.md` § The `ext` sub-scope → *Home scope by
/// derivation*): the overlay ([`AdmittedKinds`]) answers only the kind's
/// policy, rung and epoch.
pub fn home_scope_for_kind(kind: &str) -> Option<Cow<'static, str>> {
    if let Ok(ext) = kind.parse::<ExtKind>() {
        return Some(Cow::Owned(crate::scope::ext_scope(&ext)));
    }
    audience_rung(kind).map(|rung| {
        Cow::Borrowed(match rung {
            AudienceRung::Delegable => crate::account_state::ACCOUNT_STATE_SCOPE,
            AudienceRung::FleetOnly => crate::account_state::ACCOUNT_STATE_FLEET_SCOPE,
        })
    })
}

/// **Whose a generation-sealed row is** — the departure axis of the
/// `GenerationTip` kinds (`account-data-taxonomy.md` § The generation
/// machinery → *Fleet-scope reclamation*, clauses (3)(d) and (4)): a device
/// leaving the fleet — signing out, or removed from the devices page — takes
/// its **own** rows with it, and nothing else.
///
/// The axis is what a row IS, never who wrote it. An account-level row is
/// written by whichever device happened to run the flow — the first
/// community-room seat mints the reception keypair, the ceremony driver holds
/// a group root, the custody ceremony's device records the registry rows —
/// and no other device ever re-writes it, so retiring it with its writer
/// loses it for the whole account (the no-user-data-loss invariant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipRowScope {
    /// The writing device's own state, keyed by that device's id: dead once
    /// the device is out of the fleet, and retired with it. Also let go,
    /// with no removal evidence, under a superseded generation once its
    /// writer is no verified member (`account-data-taxonomy.md` clause
    /// (3)(g), the let-go arm) — so a new device-scoped kind must be one a
    /// live writer re-derives on its own, safe to lose that way.
    Device,
    /// The account's state: kept, whichever device wrote it.
    Account,
}

/// The registered `GenerationTip` kinds, one variant each, so that the
/// departure classification ([`Self::scope`]) is an exhaustive `match` the
/// compiler owns: a new tip-sealed kind cannot build without choosing a side,
/// and `the_tip_sealed_kinds_are_exactly_the_registry_set` refuses a
/// `GenerationTip` row in the registry that this enum does not name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipSealedKind {
    /// [`KIND_DEVICE_ENDPOINTS`].
    DeviceEndpoints,
    /// [`KIND_CUSTODIES_HELD`].
    CustodiesHeld,
    /// [`KIND_CUSTODIAN_ENDPOINTS`].
    CustodianEndpoints,
    /// [`KIND_SHARE_ENDPOINTS`].
    ShareEndpoints,
    /// [`KIND_GROUP_RECEPTION_KEY`].
    GroupReceptionKey,
    /// [`KIND_GROUP_MACHINERY_ROOT`].
    GroupMachineryRoot,
    /// [`KIND_CONTACT_OVERLAY`].
    ContactOverlay,
    /// [`KIND_GROUP_SHARE_CEREMONY`].
    GroupShareCeremony,
    /// [`KIND_BACKUP`].
    Backup,
    /// [`KIND_DNS`].
    Dns,
    /// [`KIND_ATPROTO`].
    Atproto,
    /// [`KIND_ATPROTO_IDENTITY`].
    AtprotoIdentity,
    /// [`KIND_MAIL`].
    Mail,
    /// [`KIND_SUCCESSION_LEDGER`].
    SuccessionLedger,
    /// [`KIND_SUBSCRIPTIONS`].
    Subscriptions,
    /// [`KIND_FOLDER_KEYS`].
    FolderKeys,
    /// [`KIND_DEPLOYMENT_SEEDS`].
    DeploymentSeeds,
    /// [`KIND_PEER_ANCHORS`].
    PeerAnchors,
    /// [`KIND_BLESSED_NESTS`].
    BlessedNests,
    /// [`KIND_REFUSED_SCHEDULING_CHANGES`].
    RefusedSchedulingChanges,
    /// [`KIND_FOLLOWS`].
    Follows,
    /// [`KIND_CUSTODY_CEREMONY`].
    CustodyCeremony,
    /// [`KIND_NOSTR_CONFIRMATION`].
    NostrConfirmation,
    /// [`KIND_KIND_MANIFEST`].
    KindManifest,
}

impl TipSealedKind {
    /// Every registered `GenerationTip` kind, in registry order —
    /// `the_tip_sealed_kinds_are_exactly_the_registry_set` holds the list to
    /// the registry, so a consumer that walks it meets every tip-sealed kind.
    pub const ALL: [Self; 24] = [
        Self::DeviceEndpoints,
        Self::CustodiesHeld,
        Self::CustodianEndpoints,
        Self::ShareEndpoints,
        Self::GroupReceptionKey,
        Self::GroupMachineryRoot,
        Self::ContactOverlay,
        Self::GroupShareCeremony,
        Self::Backup,
        Self::Dns,
        Self::Atproto,
        Self::SuccessionLedger,
        Self::Subscriptions,
        Self::FolderKeys,
        Self::DeploymentSeeds,
        Self::PeerAnchors,
        Self::BlessedNests,
        Self::AtprotoIdentity,
        Self::Mail,
        Self::Follows,
        Self::CustodyCeremony,
        Self::NostrConfirmation,
        Self::RefusedSchedulingChanges,
        Self::KindManifest,
    ];

    /// The variant for a kind string, or `None` for any kind that is not a
    /// registered `GenerationTip` kind (a `Gen0` kind, or one this build does
    /// not know).
    #[must_use]
    pub fn of(kind: &str) -> Option<Self> {
        Some(match kind {
            KIND_DEVICE_ENDPOINTS => Self::DeviceEndpoints,
            KIND_CUSTODIES_HELD => Self::CustodiesHeld,
            KIND_CUSTODIAN_ENDPOINTS => Self::CustodianEndpoints,
            KIND_SHARE_ENDPOINTS => Self::ShareEndpoints,
            KIND_GROUP_RECEPTION_KEY => Self::GroupReceptionKey,
            KIND_GROUP_MACHINERY_ROOT => Self::GroupMachineryRoot,
            KIND_CONTACT_OVERLAY => Self::ContactOverlay,
            KIND_GROUP_SHARE_CEREMONY => Self::GroupShareCeremony,
            KIND_BACKUP => Self::Backup,
            KIND_DNS => Self::Dns,
            KIND_ATPROTO => Self::Atproto,
            KIND_ATPROTO_IDENTITY => Self::AtprotoIdentity,
            KIND_MAIL => Self::Mail,
            KIND_SUCCESSION_LEDGER => Self::SuccessionLedger,
            KIND_SUBSCRIPTIONS => Self::Subscriptions,
            KIND_FOLDER_KEYS => Self::FolderKeys,
            KIND_DEPLOYMENT_SEEDS => Self::DeploymentSeeds,
            KIND_PEER_ANCHORS => Self::PeerAnchors,
            KIND_BLESSED_NESTS => Self::BlessedNests,
            KIND_REFUSED_SCHEDULING_CHANGES => Self::RefusedSchedulingChanges,
            KIND_FOLLOWS => Self::Follows,
            KIND_CUSTODY_CEREMONY => Self::CustodyCeremony,
            KIND_NOSTR_CONFIRMATION => Self::NostrConfirmation,
            KIND_KIND_MANIFEST => Self::KindManifest,
            _ => return None,
        })
    }

    /// The kind string.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::DeviceEndpoints => KIND_DEVICE_ENDPOINTS,
            Self::CustodiesHeld => KIND_CUSTODIES_HELD,
            Self::CustodianEndpoints => KIND_CUSTODIAN_ENDPOINTS,
            Self::ShareEndpoints => KIND_SHARE_ENDPOINTS,
            Self::GroupReceptionKey => KIND_GROUP_RECEPTION_KEY,
            Self::GroupMachineryRoot => KIND_GROUP_MACHINERY_ROOT,
            Self::ContactOverlay => KIND_CONTACT_OVERLAY,
            Self::GroupShareCeremony => KIND_GROUP_SHARE_CEREMONY,
            Self::Backup => KIND_BACKUP,
            Self::Dns => KIND_DNS,
            Self::Atproto => KIND_ATPROTO,
            Self::AtprotoIdentity => KIND_ATPROTO_IDENTITY,
            Self::Mail => KIND_MAIL,
            Self::SuccessionLedger => KIND_SUCCESSION_LEDGER,
            Self::Subscriptions => KIND_SUBSCRIPTIONS,
            Self::FolderKeys => KIND_FOLDER_KEYS,
            Self::DeploymentSeeds => KIND_DEPLOYMENT_SEEDS,
            Self::PeerAnchors => KIND_PEER_ANCHORS,
            Self::BlessedNests => KIND_BLESSED_NESTS,
            Self::RefusedSchedulingChanges => KIND_REFUSED_SCHEDULING_CHANGES,
            Self::Follows => KIND_FOLLOWS,
            Self::CustodyCeremony => KIND_CUSTODY_CEREMONY,
            Self::NostrConfirmation => KIND_NOSTR_CONFIRMATION,
            Self::KindManifest => KIND_KIND_MANIFEST,
        }
    }

    /// Whose a row of this kind is — the classification a departure retires
    /// by. No wildcard arm, on purpose.
    #[must_use]
    pub fn scope(self) -> TipRowScope {
        match self {
            // Keyed by the publishing device's own writer id: each device
            // owns exactly one row, and it describes that device alone.
            Self::DeviceEndpoints => TipRowScope::Device,
            // Keyed by the custody grant id: the account's custodies, as
            // custodian and as owner — every fleet replica serves them.
            Self::CustodiesHeld | Self::CustodianEndpoints => TipRowScope::Account,
            // Keyed by (set, member): another user's dial hints, learned for
            // the account from whichever of its devices heard the
            // advertisement. Recreatable, but the account's.
            Self::ShareEndpoints => TipRowScope::Account,
            // The account's group-reception keypair and held group roots:
            // minted or held once, for the whole fleet, never rotated by
            // another device.
            Self::GroupReceptionKey | Self::GroupMachineryRoot => TipRowScope::Account,
            // Keyed by the other person's actor id: the user's own words
            // about them, written from whichever device — the account's.
            Self::ContactOverlay => TipRowScope::Account,
            // One row per ceremony side-record, all the account's: the
            // ceremonies are run from whichever device — and an initiated
            // record can be the only durable copy of a scope's machinery root.
            Self::GroupShareCeremony => TipRowScope::Account,
            // The account's destination list and its adjudications, edited
            // from whichever device — never the writing device's alone.
            Self::Backup => TipRowScope::Account,
            // The account's one DNS record: the credentials serve every
            // admin device, whichever one entered them.
            Self::Dns => TipRowScope::Account,
            // The account's app credentials: each one serves the account's
            // PDS, whichever device minted it.
            Self::Atproto => TipRowScope::Account,
            // The account's rotation keys: each is the senior key of an
            // account DID, whichever device minted it.
            Self::AtprotoIdentity => TipRowScope::Account,
            // The account's MSEK and MUA credentials: one mailbox, whichever
            // device enabled it or added a credential.
            Self::Mail => TipRowScope::Account,
            // The owner chain, the grant log and the marks: the account's
            // identity history, whichever device wrote a row.
            Self::SuccessionLedger => TipRowScope::Account,
            // The account's period keys: minted from whichever device, and
            // every device of the fleet mints the tier's blobs with them.
            Self::Subscriptions => TipRowScope::Account,
            // Every shared set's content keys: the account's, whichever device
            // minted or received a generation — irrecoverable, never retired
            // with a device.
            Self::FolderKeys => TipRowScope::Account,
            // The boxes the account administers: each seed serves every admin
            // device's recovery list, whichever device captured it.
            Self::DeploymentSeeds => TipRowScope::Account,
            // The fleet's anchors for other identities: whichever device
            // harvested or walked one, every device's witness checks against
            // it — never retired with a device.
            Self::PeerAnchors => TipRowScope::Account,
            // The user's per-nest verdicts: a blessing given on any device is
            // the user's, and every minting device renews by it.
            Self::BlessedNests => TipRowScope::Account,
            // The account's notices: whichever device drained the refused
            // message, the change it tried would have reached every device.
            Self::RefusedSchedulingChanges => TipRowScope::Account,
            // The account's followed folders: a follow made on any device is
            // the user's, and every device lists it.
            Self::Follows => TipRowScope::Account,
            // One row per ceremony side-record, all the account's: a ceremony
            // is driven from whichever device, and a record can be the only
            // durable copy of a consumed offer, accept or deliver — the
            // group-share ceremony's reasoning, and its registry rows' scope.
            Self::CustodyCeremony => TipRowScope::Account,
            // The owner's confirmation of the account's npub: made on any
            // device, it answers the aftermath's check on every device.
            Self::NostrConfirmation => TipRowScope::Account,
            // The admitted-kinds overlay: a consent made on any device admits
            // the kinds every device converges.
            Self::KindManifest => TipRowScope::Account,
        }
    }
}

/// Does a departing device's row of `kind` go with it? True only for a
/// device-scoped `GenerationTip` kind ([`TipRowScope::Device`]). Every other
/// kind is kept: an account-level one; a `Gen0` one (the reclamation pass
/// names a departed device's gen-0 rows one by one, never by filter); and a
/// kind this build does not know. The destructive side has no catch-all.
///
/// The same classification decides the let-go arm (`account-data-taxonomy.md`
/// clause (3)(g)): under a superseded generation, the generation's hander
/// retires a device-scoped row whose writer is no verified member, removal
/// evidence or none — so a kind on the device side must be safe to lose
/// whenever its writer is not, or not yet, a verified member.
#[must_use]
pub fn retired_with_its_writer(kind: &str) -> bool {
    TipSealedKind::of(kind).is_some_and(|k| k.scope() == TipRowScope::Device)
}

/// The seal/open keys for `kind`, routed to its registered branch.
///
/// **The one place the rung lookup happens.** Every production seal and open goes
/// through here rather than reaching for a branch directly, which is what makes
/// "the rung is enforced by which key branch seals the entry" true of the code
/// and not merely of the doc: a caller cannot pick the wrong branch, because it
/// never picks one.
pub fn kind_keys(
    schedule: &AccountStateKeySchedule,
    kind: &str,
) -> Option<fauna_core::crypto::AccountStateKindKeys> {
    // Gen0 routing only: every registered kind's gen-0 keys derive here, and
    // GenerationTip kinds gain their per-generation schedule with the mint
    // step (the tip's key material replaces `BackupKey` under the same
    // fleet-only context pair — `owner-key-material.md` § The schedule build
    // design). Until then the writer door refuses their production sealing,
    // so this routing is never reached for a tip-sealed production entry.
    audience_rung(kind).map(|rung| schedule.for_rung(rung, kind))
}

/// Every class-2 kind this build knows.
///
/// The reader's **trial-open set**: a feed row names its item only by the
/// blinded item key `keyed_hash(item_blind(kind), logical_key)`, which is
/// one-way, so a reader recovers the kind by attempting the open under each
/// kind's entry key and letting the AEAD tag arbitrate (the same shape as the
/// shipped epoch opener's trial chain, `owner-key-material.md` § Path
/// B-sibling-2). Keeping the set small is what keeps that cheap.
pub fn class2_kinds() -> impl Iterator<Item = &'static str> {
    POLICIES.iter().map(|(k, _, _, _)| *k)
}

/// The kinds registered at [`AudienceRung::Delegable`] — the grant-mintable set,
/// and the set the Secret-free conformance check ranges over
/// ([`secret_free`](crate::secret_free)).
pub fn delegable_kinds() -> impl Iterator<Item = &'static str> {
    POLICIES
        .iter()
        .filter(|(_, _, rung, _)| matches!(rung, AudienceRung::Delegable))
        .map(|(k, _, _, _)| *k)
}

/// A kind manifest's `merge` spellings — the five arms of [`MergePolicy`],
/// spelled once so a manifest can always be *parsed*
/// (`third-party-kinds.md` § The kinds vocabulary). Whether a parsed policy
/// is *admitted* from a manifest is [`MergePolicy::manifest_admitted`].
pub const MANIFEST_MERGE_SPELLINGS: [(&str, MergePolicy); 5] = [
    ("immutable", MergePolicy::Immutable),
    ("crdt-per-field", MergePolicy::CrdtPerField),
    ("latest-wins", MergePolicy::LatestWins),
    ("three-way", MergePolicy::ThreeWay),
    ("nest-cas", MergePolicy::NestCas),
];

impl MergePolicy {
    /// The policy a manifest's `merge` member names, or `None` for a
    /// spelling outside the closed five.
    pub fn from_manifest_spelling(spelling: &str) -> Option<Self> {
        MANIFEST_MERGE_SPELLINGS
            .iter()
            .find(|(s, _)| *s == spelling)
            .map(|(_, p)| *p)
    }

    /// The policy's manifest spelling.
    pub fn manifest_spelling(self) -> &'static str {
        MANIFEST_MERGE_SPELLINGS
            .iter()
            .find(|(_, p)| *p == self)
            .map(|(s, _)| *s)
            .expect("every MergePolicy arm has a manifest spelling")
    }

    /// May a manifest admit a kind under this policy? **`LatestWins` only**
    /// in v1 — the one policy an engine applies to a payload it cannot type
    /// (`apply_class2`'s LWW arm never looks at the kind), so the one a kind
    /// unseen at compile time converges under (`third-party-kinds.md` § The
    /// kinds vocabulary → *`merge`*, with each refusal's reason).
    pub fn manifest_admitted(self) -> bool {
        matches!(self, MergePolicy::LatestWins)
    }
}

/// Why [`AdmittedKinds::admit`] refused a kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    /// The policy cannot converge a payload the engine cannot type.
    #[error("merge policy {0:?} is not admitted from a manifest (latest-wins only)")]
    PolicyNotAdmitted(MergePolicy),
    /// The kind is already admitted under a different policy — a kind's
    /// policy freezes with its string, so a second answer is refused, never
    /// taken.
    #[error("kind {kind} is already admitted as {admitted:?}, not {offered:?}")]
    Conflicting {
        /// The kind.
        kind: String,
        /// Its admitted policy.
        admitted: MergePolicy,
        /// The policy offered now.
        offered: MergePolicy,
    },
}

/// **The admitted-kinds overlay** — the `ext.*` kinds this account's
/// consenting device admitted from verified manifests, each with its policy
/// (`third-party-kinds.md` § The kinds vocabulary → *The registry overlay*).
///
/// The registry lookups this type answers (`merge_policy`, `audience_rung`,
/// `sealing_epoch`, `kind_keys`) consult the compiled table FIRST and the
/// overlay second, so a first-party kind can never be shadowed. An admitted
/// kind is always [`AudienceRung::Delegable`] at [`SealingEpoch::Gen0`]: both
/// are consequences of being manifest-admitted, never data — the overlay
/// stores the policy alone, and a manifest can never widen who opens a kind.
/// An empty overlay answers exactly the free functions, so a plane with no
/// overlay is plane-native behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmittedKinds {
    kinds: BTreeMap<ExtKind, MergePolicy>,
}

impl AdmittedKinds {
    /// An empty overlay.
    pub const fn new() -> Self {
        Self {
            kinds: BTreeMap::new(),
        }
    }

    /// Admit `kind` under `policy`. Idempotent for the same answer.
    ///
    /// # Errors
    /// The policy is not manifest-admitted, or the kind is already admitted
    /// under another policy.
    pub fn admit(&mut self, kind: ExtKind, policy: MergePolicy) -> Result<(), AdmitError> {
        if !policy.manifest_admitted() {
            return Err(AdmitError::PolicyNotAdmitted(policy));
        }
        match self.kinds.get(&kind) {
            Some(admitted) if *admitted != policy => Err(AdmitError::Conflicting {
                kind: kind.to_string(),
                admitted: *admitted,
                offered: policy,
            }),
            _ => {
                self.kinds.insert(kind, policy);
                Ok(())
            }
        }
    }

    /// Is `kind` admitted?
    pub fn contains(&self, kind: &ExtKind) -> bool {
        self.kinds.contains_key(kind)
    }

    /// The admitted kinds, in order.
    pub fn kinds(&self) -> impl Iterator<Item = &ExtKind> {
        self.kinds.keys()
    }

    /// Is the overlay empty?
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    fn overlay_policy(&self, kind: &str) -> Option<MergePolicy> {
        let ext = kind.parse::<ExtKind>().ok()?;
        self.kinds.get(&ext).copied()
    }

    /// [`merge_policy`], then the overlay.
    pub fn merge_policy(&self, kind: &str) -> Option<MergePolicy> {
        merge_policy(kind).or_else(|| self.overlay_policy(kind))
    }

    /// [`audience_rung`], then the overlay (always delegable).
    pub fn audience_rung(&self, kind: &str) -> Option<AudienceRung> {
        audience_rung(kind).or_else(|| self.overlay_policy(kind).map(|_| AudienceRung::Delegable))
    }

    /// [`sealing_epoch`], then the overlay (always `Gen0`: the delegable
    /// branch never gains a generation axis).
    pub fn sealing_epoch(&self, kind: &str) -> Option<SealingEpoch> {
        sealing_epoch(kind).or_else(|| self.overlay_policy(kind).map(|_| SealingEpoch::Gen0))
    }

    /// [`kind_keys`], then the overlay — an admitted kind's keys are its
    /// delegable gen-0 pair, the pair a `content.read` grant wraps.
    pub fn kind_keys(
        &self,
        schedule: &AccountStateKeySchedule,
        kind: &str,
    ) -> Option<fauna_core::crypto::AccountStateKindKeys> {
        self.audience_rung(kind)
            .map(|rung| schedule.for_rung(rung, kind))
    }
}

/// The merge metadata of a stamped policy ([`MergePolicy::LatestWins`]) —
/// canonical dag-cbor in [`EntryPlaintext::merge_meta`].
///
/// Two fields because "latest" is not a total order on its own: two writers can
/// stamp the same millisecond, and a merge that is not deterministic across
/// replicas does not converge. `writer` breaks the tie by byte order —
/// arbitrary, but identical everywhere, which is the whole requirement.
///
/// **Exempt from transport.md rule 4's `extra` catch-all by construction, not
/// oversight** (`tools/check-additive-evolution/catch_all_baseline.txt`): a
/// nest never decodes this struct standalone — it lives only as opaque bytes
/// inside `EntryPlaintext::merge_meta` (a sealed, AEAD-encrypted field the
/// nest cannot open), and the merge/relay path always carries those bytes
/// forward verbatim rather than round-tripping through this struct's own
/// serde shape (`account-replica-posture.md` § The store device principal →
/// *Re-authoring preserves the LWW stamp and re-seals at publish*'s "re-puts
/// each distinct entry's CURRENT value with its `merge_meta` verbatim"). A
/// future field would never be silently dropped
/// here because nothing decode-then-re-encodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwStamp {
    /// Unix milliseconds at the authoring writer's clock. Advisory across
    /// devices (clocks skew); the tiebreak below is what makes the order total.
    pub at_ms: i64,
    /// The authoring writer's 32-byte id — the deterministic tiebreak.
    #[serde(with = "serde_bytes")]
    pub writer: [u8; 32],
}

impl LwwStamp {
    /// Canonical dag-cbor bytes for [`EntryPlaintext::merge_meta`].
    pub fn encode(&self) -> Result<Vec<u8>, fauna_core::error::Error> {
        fauna_core::encoding::canonical_encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_core::error::Error> {
        fauna_core::encoding::canonical_decode(bytes)
    }

    /// The total order: later millisecond wins, ties by writer bytes.
    ///
    /// Public because the order is part of the stamp's contract, not an
    /// implementation detail.
    ///
    /// The order itself lives in [`fauna_core::data::lww_rank`], below both this
    /// crate and `fauna-core`'s own stamped registers, which carry the same
    /// stamp; every one delegates, so no two can order a pair differently.
    pub fn rank(&self) -> ([u8; 8], [u8; 32]) {
        fauna_core::data::lww_rank(self.at_ms, self.writer)
    }
}

/// What a reading replica should do with one incoming entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The local value already reflects the incoming one — write nothing.
    /// Also the answer when a merge reproduces the current value exactly,
    /// which is what stops two replicas re-publishing merges at each other
    /// forever.
    KeepCurrent,
    /// Adopt the incoming value verbatim.
    Replace,
    /// Neither side alone: persist (and re-publish) this merged value.
    Merged(Box<EntryPlaintext>),
    /// The kind's resolution needs the three-way machinery
    /// (`docs/goal/behavior/conflicts.md`). No kind registers
    /// [`MergePolicy::ThreeWay`] yet; the first one lands with its resolver,
    /// and this variant is what the walk escalates to it.
    NeedsThreeWay,
}

/// Why an incoming entry could not be merged. Every variant is fail-closed:
/// the walk surfaces it rather than picking a side.
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("kind {kind:?} is not on the class-2 plane in this build")]
    UnknownKind { kind: String },
    #[error("kind mismatch merging class-2 entries: current {current:?}, incoming {incoming:?}")]
    KindMismatch { current: String, incoming: String },
    #[error("a {policy:?} entry of kind {kind:?} carries no LWW stamp")]
    MissingStamp { policy: MergePolicy, kind: String },
    #[error("malformed merge metadata on kind {kind:?}: {source}")]
    BadStamp {
        kind: String,
        #[source]
        source: fauna_core::error::Error,
    },
    #[error(
        "kind {kind:?} merges per-field and has no specified per-field deletion, \
         so a tombstone on it cannot be resolved convergently"
    )]
    TombstoneOnCrdtKind { kind: String },
    #[error("malformed value on kind {kind:?}: {source}")]
    BadValue {
        kind: String,
        #[source]
        source: fauna_core::error::Error,
    },
}

impl MergeError {
    /// Is this refusal a permanent property of the **row's own content** —
    /// as opposed to an inconsistency in this build or this store?
    ///
    /// The distinction decides what a walk does with the refusal
    /// (`fauna_sync_engine::account_state_plane`, the skip arm): the nest
    /// collapses per `(item_key, writer)`, so a row its author never
    /// supersedes is met on **every** walk and every reconcile forever — and
    /// any fleet-key holder (a removed device retains `BackupKey` forever,
    /// the stated R14 exposure) can seal one. A row-content refusal must
    /// therefore skip, never abort: aborting would let one hostile or
    /// corrupt row starve the replica of every other writer's rows — a
    /// client-causable unrecoverable state (`nest/common.md` § Client-state
    /// recoverability). A build/store inconsistency (an unknown kind that
    /// nonetheless opened, a kind mismatch inside the store) is OUR defect:
    /// fail loudly.
    pub fn is_row_content(&self) -> bool {
        match self {
            MergeError::TombstoneOnCrdtKind { .. }
            | MergeError::MissingStamp { .. }
            | MergeError::BadStamp { .. }
            | MergeError::BadValue { .. } => true,
            MergeError::UnknownKind { .. } | MergeError::KindMismatch { .. } => false,
        }
    }
}

/// Apply one incoming sealed-entry plaintext against the local current value.
///
/// Runs **only at reading replicas** (module header). `current` is this
/// replica's merged value for the same `(kind, key)`, `None` when the item is
/// new here.
///
/// Returns `Result` rather than the bare outcome the charter's sketch shows:
/// two of the five policies have to decode the value to merge it, and a value
/// that will not decode must fail loudly — silently keeping the local side
/// would drop a peer's write and report success.
pub fn apply_class2(
    policy: MergePolicy,
    current: Option<&EntryPlaintext>,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    if let Some(cur) = current
        && cur.kind != incoming.kind
    {
        return Err(MergeError::KindMismatch {
            current: cur.kind.clone(),
            incoming: incoming.kind.clone(),
        });
    }
    let Some(cur) = current else {
        // Nothing local to reconcile against — but adoption is NOT
        // unconditional: a first-contact row must satisfy the same
        // row-content contract a merge would demand of it, because an
        // adopted row becomes the `current` every later merge decodes.
        // Without this, arrival order decides truth: a replica that met a
        // junk / stampless / CRDT-tombstone row FIRST adopts it and then
        // (row-content refusals being skips) keeps it forever, while a
        // replica that met a valid row first keeps THAT — permanent
        // divergence, mintable by any fleet-key holder front-running a key
        // it never owned. Refusing here is convergent by construction: the
        // verdict is a pure function of the row's own bytes, identical at
        // every replica, and `reconcile` re-presents the row if a later
        // build learns to merge it.
        validate_adoptable(policy, incoming)?;
        return Ok(MergeOutcome::Replace);
    };
    if cur == incoming {
        return Ok(MergeOutcome::KeepCurrent);
    }

    match policy {
        // Identity is the content: a second value under the same key is a
        // different item, not a newer one.
        MergePolicy::Immutable => Ok(MergeOutcome::KeepCurrent),

        MergePolicy::LatestWins => {
            if stamp_of(policy, cur)?.rank() >= stamp_of(policy, incoming)?.rank() {
                Ok(MergeOutcome::KeepCurrent)
            } else {
                Ok(MergeOutcome::Replace)
            }
        }

        // The nest serialized these before they reached the feed, and the feed
        // relays applied truth in nest order — so the row we are looking at IS
        // the newer one. (The sealed CAS base echo is the reader's cross-check
        // and lands with the first NestCas kind; none is registered yet.)
        MergePolicy::NestCas => Ok(MergeOutcome::Replace),

        MergePolicy::ThreeWay => Ok(MergeOutcome::NeedsThreeWay),

        MergePolicy::CrdtPerField => {
            if cur.tombstone || incoming.tombstone {
                // Deliberately not degraded to a stamp comparison: a value
                // concurrent with (and newer than) a tombstone would then win
                // wholesale on one replica while the other kept its own merged
                // value, and the two would never converge. Per-field deletion
                // for a CRDT kind is a design slice (the charter's five
                // policies say nothing about it), not an implementation choice.
                return Err(MergeError::TombstoneOnCrdtKind {
                    kind: incoming.kind.clone(),
                });
            }
            match incoming.kind.as_str() {
                KIND_SEEN_SET => merge_seen_set_entry(cur, incoming),
                KIND_READ_MARKER => merge_read_marker_entry(cur, incoming),
                KIND_DEVICE_SET => merge_device_set_entry(cur, incoming),
                KIND_GENERATION_MINT => merge_two_phase_entry(
                    cur,
                    incoming,
                    fauna_core::generation::join_generation_mint,
                ),
                KIND_GENERATION_WRAP => merge_generation_wrap_entry(cur, incoming),
                KIND_GENERATION_UNKEYABLE => merge_generation_unkeyable_entry(cur, incoming),
                KIND_DEVICE_REACH => merge_device_reach_entry(cur, incoming),
                KIND_GENERATION_CLOSED => merge_generation_closed_entry(cur, incoming),
                KIND_CONTACT_OVERLAY => merge_contact_overlay_entry(cur, incoming),
                KIND_GROUP_SHARE_CEREMONY => merge_group_share_ceremony_entry(cur, incoming),
                KIND_ATPROTO_IDENTITY => merge_atproto_identity_entry(cur, incoming),
                KIND_MAIL => merge_mail_entry(cur, incoming),
                KIND_BACKUP => merge_backup_entry(cur, incoming),
                KIND_SUCCESSION_LEDGER => merge_succession_ledger_entry(cur, incoming),
                KIND_SUBSCRIPTIONS => merge_subscriptions_entry(cur, incoming),
                KIND_FOLDER_KEYS => merge_folder_keys_entry(cur, incoming),
                KIND_DEPLOYMENT_SEEDS => merge_deployment_seeds_entry(cur, incoming),
                KIND_PEER_ANCHORS => merge_peer_anchors_entry(cur, incoming),
                KIND_BLESSED_NESTS => merge_blessed_nests_entry(cur, incoming),
                KIND_REFUSED_SCHEDULING_CHANGES => {
                    merge_refused_scheduling_changes_entry(cur, incoming)
                }
                KIND_CUSTODY_CEREMONY => merge_custody_ceremony_entry(cur, incoming),
                KIND_NOSTR_CONFIRMATION => merge_nostr_confirmation_entry(cur, incoming),
                // The group plane's CRDT kinds (`crate::group_state` is
                // their registry; the joins live in `fauna_core::{group_scope,
                // group_generation}` — one dispatcher, one entry form).
                crate::group_state::KIND_GROUP_ROSTER => merge_group_roster_entry(cur, incoming),
                crate::group_state::KIND_GROUP_GENERATION_MINT => merge_two_phase_entry(
                    cur,
                    incoming,
                    fauna_core::group_generation::join_group_generation_mint,
                ),
                crate::group_state::KIND_GROUP_GENERATION_WRAP => {
                    merge_group_topup_entry(cur, incoming)
                }
                crate::group_state::KIND_GROUP_GENERATION_UNKEYABLE => {
                    merge_group_unkeyable_entry(cur, incoming)
                }
                crate::group_state::KIND_GROUP_AUTHORITY_REVOCATION => {
                    merge_group_authority_revocation_entry(cur, incoming)
                }
                other => Err(MergeError::UnknownKind {
                    kind: other.to_string(),
                }),
            }
        }
    }
}

/// The first-contact contract: could this row participate in a future merge?
///
/// Exactly the checks the policy's merge arm would run against the row —
/// single-sourced by running the same decoders/joins, never a second
/// implementation: a tombstone only where the policy can resolve one
/// ([`MergePolicy::admits_tombstone`]), a rankable stamp where the policy
/// orders by stamp, a decodable value where the merge must decode. Immutable /
/// NestCas / ThreeWay values stay opaque here because their merges never
/// decode them either.
fn validate_adoptable(policy: MergePolicy, incoming: &EntryPlaintext) -> Result<(), MergeError> {
    if incoming.tombstone && !policy.admits_tombstone() {
        return Err(MergeError::TombstoneOnCrdtKind {
            kind: incoming.kind.clone(),
        });
    }
    match policy {
        MergePolicy::LatestWins => {
            stamp_of(policy, incoming)?;
        }
        MergePolicy::CrdtPerField => {
            // The arm run against itself exercises exactly the merge's own
            // decode path (the joins never shortcut on equality) and nothing
            // else; the outcome is discarded.
            match incoming.kind.as_str() {
                KIND_SEEN_SET => {
                    merge_seen_set_entry(incoming, incoming)?;
                }
                KIND_READ_MARKER => {
                    merge_read_marker_entry(incoming, incoming)?;
                }
                // Deliberately PERMISSIVE about the self-signature (unlike the
                // reach/wrap/unkeyable arms): a two-phase lattice's first
                // contact asks only "could this row take part in a merge?" —
                // the key-aware join outranks a non-verifying row and the
                // fleet view refuses it, so adoption checks no signature and
                // a forgery merges in and loses (the group roster arm below
                // is the same shape).
                KIND_DEVICE_SET => {
                    validate_device_set_adoptable(incoming)?;
                }
                KIND_GENERATION_MINT => {
                    merge_two_phase_entry(
                        incoming,
                        incoming,
                        fauna_core::generation::join_generation_mint,
                    )?;
                }
                // Deliberately NOT the run-the-arm-against-itself pattern: the
                // per-healer join only demands that both sides DECODE (a
                // decoding forgery merges in and loses), so the strictness
                // lives here — first contact demands a row that verifies at
                // its cell (hardening).
                KIND_GENERATION_WRAP => {
                    validate_generation_wrap_adoptable(incoming)?;
                }
                // The same strictness split as the wrap kind, same rationale:
                // the join outranks a forgery, so first contact is where a
                // forged signal is refused outright.
                KIND_GENERATION_UNKEYABLE => {
                    validate_generation_unkeyable_adoptable(incoming)?;
                }
                KIND_DEVICE_REACH => {
                    validate_device_reach_adoptable(incoming)?;
                }
                // No signature to demand — the value is audit only — so first
                // contact asks for a generation-id key and a value that
                // decodes, and nothing else.
                KIND_GENERATION_CLOSED => {
                    validate_generation_closed_adoptable(incoming)?;
                }
                KIND_CONTACT_OVERLAY => {
                    merge_contact_overlay_entry(incoming, incoming)?;
                }
                KIND_GROUP_SHARE_CEREMONY => {
                    merge_group_share_ceremony_entry(incoming, incoming)?;
                }
                KIND_ATPROTO_IDENTITY => {
                    merge_atproto_identity_entry(incoming, incoming)?;
                }
                KIND_MAIL => {
                    merge_mail_entry(incoming, incoming)?;
                }
                KIND_BACKUP => {
                    merge_backup_entry(incoming, incoming)?;
                }
                KIND_SUCCESSION_LEDGER => {
                    merge_succession_ledger_entry(incoming, incoming)?;
                }
                KIND_SUBSCRIPTIONS => {
                    merge_subscriptions_entry(incoming, incoming)?;
                }
                KIND_FOLDER_KEYS => {
                    merge_folder_keys_entry(incoming, incoming)?;
                }
                KIND_DEPLOYMENT_SEEDS => {
                    merge_deployment_seeds_entry(incoming, incoming)?;
                }
                KIND_PEER_ANCHORS => {
                    merge_peer_anchors_entry(incoming, incoming)?;
                }
                KIND_BLESSED_NESTS => {
                    merge_blessed_nests_entry(incoming, incoming)?;
                }
                KIND_REFUSED_SCHEDULING_CHANGES => {
                    merge_refused_scheduling_changes_entry(incoming, incoming)?;
                }
                KIND_CUSTODY_CEREMONY => {
                    merge_custody_ceremony_entry(incoming, incoming)?;
                }
                KIND_NOSTR_CONFIRMATION => {
                    merge_nostr_confirmation_entry(incoming, incoming)?;
                }
                // The group plane's kinds, each holding its R14 sibling's
                // contract: the two-phase lattices run the arm against
                // themselves; the per-writer-cell kinds get first-contact
                // strictness.
                crate::group_state::KIND_GROUP_ROSTER => {
                    validate_group_roster_adoptable(incoming)?;
                }
                crate::group_state::KIND_GROUP_GENERATION_MINT => {
                    merge_two_phase_entry(
                        incoming,
                        incoming,
                        fauna_core::group_generation::join_group_generation_mint,
                    )?;
                }
                crate::group_state::KIND_GROUP_GENERATION_WRAP => {
                    validate_group_topup_adoptable(incoming)?;
                }
                crate::group_state::KIND_GROUP_GENERATION_UNKEYABLE => {
                    validate_group_unkeyable_adoptable(incoming)?;
                }
                crate::group_state::KIND_GROUP_AUTHORITY_REVOCATION => {
                    validate_group_authority_revocation_adoptable(incoming)?;
                }
                other => {
                    return Err(MergeError::UnknownKind {
                        kind: other.to_string(),
                    });
                }
            }
        }
        MergePolicy::Immutable | MergePolicy::NestCas | MergePolicy::ThreeWay => {}
    }
    Ok(())
}

/// The seen-set arm — delegating to the join `fauna_core::seen_set` owns and
/// proves, never a second implementation of it.
///
/// The delegate is a join *by construction* — elision lives inside it, so a
/// compacted side and an itemized side produce identical bytes whichever
/// replica merges — and its laws are asserted where it lives. That "asserted
/// where it lives" bar is the pattern every later CRDT arm holds
/// (established by the retired `fauna.state.user-config` arm's history: a
/// delegate that keeps a local preference makes each replica prefer itself
/// and re-publish forever — the first two-replica convergence test caught
/// exactly that, and the fix was making the delegate a join in bytes, never
/// ordering games in this dispatcher). The byte-equality echo-stop below is
/// the standing law: normal form makes equal membership encode equally, so a
/// converged pair stops publishing.
///
/// The same byte comparison against the served row is the adoption law
/// ([`crdt_join_outcome`]): a join that IS the incoming value answers
/// `Replace`, so a device that only takes a sibling's row whole authors no
/// row of its own (`delegable-scope-reclamation.md` § Delegable-scope
/// reclamation, part (1)).
fn merge_seen_set_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur: fauna_core::seen_set::SeenScopeSet =
        fauna_core::encoding::canonical_decode(&current.value).map_err(bad)?;
    let inc: fauna_core::seen_set::SeenScopeSet =
        fauna_core::encoding::canonical_decode(&incoming.value).map_err(bad)?;
    let bytes = fauna_core::encoding::canonical_encode(&cur.join(&inc)).map_err(bad)?;
    // Union needs no outer stamp: the value is its own merge state.
    Ok(crdt_join_outcome(current, incoming, bytes))
}

/// The read-marker arm — delegating to the max-register join
/// `fauna_core::read_marker` owns and law-tests (the seen-set arm's "asserted
/// where it lives" bar, the same byte-equality echo-stop, and the same
/// adoption law — [`crdt_join_outcome`]: a max-register's join always equals
/// one side, so this arm answers `KeepCurrent` or `Replace` and never
/// `Merged`).
fn merge_read_marker_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur: fauna_core::read_marker::ReadMarker =
        fauna_core::encoding::canonical_decode(&current.value).map_err(bad)?;
    let inc: fauna_core::read_marker::ReadMarker =
        fauna_core::encoding::canonical_decode(&incoming.value).map_err(bad)?;
    let bytes = fauna_core::encoding::canonical_encode(&cur.join(&inc)).map_err(bad)?;
    // The register is its own merge state; no outer stamp.
    Ok(crdt_join_outcome(current, incoming, bytes))
}

/// The outcome of a delegable CRDT arm's join, read off its canonical
/// `bytes` — the seen-set's and the read marker's, whose values are their own
/// merge state (no `merge_meta`):
///
/// - equal to the held value → `KeepCurrent`, the echo-stop (checked first,
///   so a served row equal to the held one is never re-adopted);
/// - equal to the served row's value → `Replace`: the walk takes the row
///   whole at its own coordinate, as it takes a latest-wins row that wins,
///   and authors none (`delegable-scope-reclamation.md` § Delegable-scope
///   reclamation, part (1) — re-authoring it made every device that had held
///   an older value a writer of the item at each advance);
/// - neither → `Merged`: a value no row carries yet, which only this
///   replica can publish.
///
/// Delegable kinds only: the fleet-scope CRDT arms keep answering `Merged`
/// for a join that differs from the held value, because their passes read
/// the device's own rows (`account-data-taxonomy.md` § The generation
/// machinery → *Fleet-scope reclamation*, clauses (3)(f) and (3)(g)).
fn crdt_join_outcome(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
    bytes: Vec<u8>,
) -> MergeOutcome {
    if bytes == current.value.as_slice() {
        return MergeOutcome::KeepCurrent;
    }
    if bytes == incoming.value.as_slice() {
        return MergeOutcome::Replace;
    }
    MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    }))
}

/// The contact-overlay arm — delegating to the per-register join
/// `fauna_core::contact_overlay` owns and law-tests (the seen-set arm's
/// "asserted where it lives" bar, and the same byte-equality echo-stop).
fn merge_contact_overlay_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur: fauna_core::contact_overlay::ContactOverlay =
        fauna_core::encoding::canonical_decode(&current.value).map_err(bad)?;
    let inc: fauna_core::contact_overlay::ContactOverlay =
        fauna_core::encoding::canonical_decode(&incoming.value).map_err(bad)?;
    let bytes = fauna_core::encoding::canonical_encode(&cur.join(&inc)).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // Every register carries its own stamp; no outer one.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The group-share ceremony arm — one row per ceremony side-record: the
/// key's first segment picks the record type (`initiated/` | `invited/`),
/// both sides decode as it and must name the key's ceremony
/// (`fauna_core::group_ceremony::decode_group_share_row`), and the join is
/// the per-record half `GroupShareConfig::merge` runs, with the same
/// byte-equality echo-stop as the seen-set arm.
fn merge_group_share_ceremony_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::group_ceremony::decode_group_share_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_group_share_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_group_share_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // Every record carries its own stamp; no outer one.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The backup-state arm — one row per record: the key picks the record type
/// (`destinations/<box>` | `mark/…`), both sides decode strictly as it and
/// the value must name its key (`fauna_core::backup_state::decode_backup_row`), and the
/// join is the per-row half `BackupState::merge` runs (the list's
/// latest-wins pick on its own stamp, a mark's verdict-precedence minimum),
/// with the same byte-equality echo-stop as the seen-set arm. The `Removed`
/// prune is the read fold's, never this arm's.
fn merge_backup_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::backup_state::decode_backup_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_backup_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_backup_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // The list row carries its own stamp and a mark needs none.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The succession-ledger arm — one row per entity plus the one `chain` row:
/// the key's first segment picks the record type, both sides decode as it,
/// re-encode to their own bytes and name the key
/// (`fauna_core::succession_ledger::decode_succession_ledger_row`), and the
/// join is the per-row half `SuccessionLedger::merge` runs. A `chain` pair the
/// `one_owner_chain` gate refuses (a fork) and two differing events under one
/// key are `BadValue`: row-content refusals the walk skips, so each replica
/// keeps its own. Same byte-equality echo-stop as the seen-set arm.
fn merge_succession_ledger_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::succession_ledger::decode_succession_ledger_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_succession_ledger_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_succession_ledger_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // Every row is its own merge state; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The subscription period-key arm — one row per period key or staged
/// removal: the key's first segment picks the row type (`period/` |
/// `removal/`), both sides decode as it and must carry the key's digest
/// (`fauna_core::subscription_rows::decode_subscriptions_row`), and the join
/// is the per-row half `SubscriptionsConfig::merge` folds through, with the
/// byte-equality echo-stop.
fn merge_subscriptions_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::subscription_rows::decode_subscriptions_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_subscriptions_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_subscriptions_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // A row is its own record; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The folder-keys arm — one row per entity: the key's first segment picks
/// the record type, both sides decode as it, re-encode to their own bytes and
/// name the key (`fauna_core::folder_key_rows::decode_folder_key_row`), and
/// the join is `FoldersConfig::merge` over the two rows' single-entity configs.
/// Two differing generations under one key are `BadValue` (a row-content refusal
/// the walk skips). Same byte-equality echo-stop as the seen-set arm.
fn merge_folder_keys_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::folder_key_rows::decode_folder_key_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_folder_key_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_folder_key_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // Every row is its own merge state; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The npub-confirmation arm — one row at `self`: both sides decode strictly
/// (`fauna_core::nostr_confirmation::decode_nostr_confirmation_row` refuses
/// any other key and any unknown field), and the join is
/// `NostrConfirmation::merge`, the max, with the byte-equality echo-stop.
fn merge_nostr_confirmation_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::nostr_confirmation::decode_nostr_confirmation_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_nostr_confirmation_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_nostr_confirmation_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).encode().map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // The stamp is its own merge state; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The deployment-seed custody arm — one row per custodied box: both sides
/// decode strictly and must be self-consistent rows for the key's box
/// (`fauna_core::deployment_seed_rows::decode_deployment_seed_row`), and the
/// join is `DeploymentSeedEntry::merge`, the per-box half of the seed-map union,
/// with the byte-equality echo-stop.
fn merge_deployment_seeds_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::deployment_seed_rows::decode_deployment_seed_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_deployment_seed_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_deployment_seed_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode_row()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // A row is its own record; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The peer-anchor arm — one row per anchored actor per vector: both sides
/// decode strictly as the key's family and must name the key's actor
/// (`fauna_core::peer_anchor_rows::decode_peer_anchor_row`), and the join is
/// the per-actor half of `PeerAnchors::merge`, with the byte-equality
/// echo-stop. The ceiling is not
/// the arm's: it is a cross-row count, held by the read fold.
fn merge_peer_anchors_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::peer_anchor_rows::decode_peer_anchor_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_peer_anchor_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_peer_anchor_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode_row()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // A row is its own record; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The blessed-nests arm — one row per nest: both sides decode strictly and
/// must name the key's nest (`fauna_core::blessed_nest_rows::
/// decode_blessed_nest_row`), and the join is the per-nest half of
/// `merge_blessed_nests` (`BlessedNest::join`), with the byte-equality echo-stop.
fn merge_blessed_nests_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::blessed_nest_rows::decode_blessed_nest_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_blessed_nest_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_blessed_nest_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode_row()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // A row is its own record; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The ATProto identity arm — one row per list element: the key's prefix
/// picks the element type, both sides decode as it exactly and must name the
/// key's element (`fauna_core::atproto_identity_rows::decode_atproto_identity_row`),
/// and the join is the per-element half `AtprotoIdentityConfig::merge` runs,
/// with the same byte-equality echo-stop as the ceremony arm.
fn merge_atproto_identity_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::atproto_identity_rows::decode_atproto_identity_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_atproto_identity_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_atproto_identity_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // No element carries an outer stamp; the join needs none.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The mail arm — two row families, the key's first segment dispatching
/// (`fauna_core::mail_rows::decode_mail_row`): both sides decode as the
/// key's family exactly, a credential must name its key's id and a marked
/// one carry no secret; the join is `MailStateRow::merge` (the MSEK, window
/// and burn halves `MailConfig::merge` runs, the recreatable four on the
/// row's stamp) or `MailCredential::merge` (the markers ORed, the remainder
/// on the row's stamp), with the byte-equality echo-stop.
fn merge_mail_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::mail_rows::decode_mail_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_mail_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_mail_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // Each row carries its own stamp; the join needs no outer one.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The custody-ceremony arm — one row per ceremony side-record: the key's
/// first segment picks the side, both sides decode as its record exactly and
/// must name the key's ceremony
/// (`fauna_core::custody_ceremony_rows::decode_custody_row`), and the join is
/// the per-record half `CustodyConfig::merge` runs, with the
/// byte-equality echo-stop.
fn merge_custody_ceremony_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::custody_ceremony_rows::decode_custody_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_custody_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_custody_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).and_then(|m| m.encode()).map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // Every record carries its own stamps; no outer one.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The refused-change arm — one row per account: both sides decode strictly
/// and must be held under the ceilings
/// (`fauna_core::refused_change_rows::decode_refused_changes_row`), and the
/// join is `RefusedSchedulingChanges::merge`, with the byte-equality echo-stop.
fn merge_refused_scheduling_changes_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::refused_change_rows::decode_refused_changes_row;
    let bad = |source: fauna_core::error::Error| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    };
    let cur = decode_refused_changes_row(&current.key, &current.value).map_err(bad)?;
    let inc = decode_refused_changes_row(&incoming.key, &incoming.value).map_err(bad)?;
    let bytes = cur.merge(&inc).encode_row().map_err(bad)?;
    if bytes == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Merged(Box::new(EntryPlaintext {
        kind: incoming.kind.clone(),
        key: incoming.key.clone(),
        // The row is its own record; no outer stamp.
        merge_meta: None,
        value: bytes.into(),
        tombstone: false,
    })))
}

/// The byte-level join signature the two-phase arms delegate to
/// (`fauna_core::generation::{join_device_set, join_generation_mint}`).
type TwoPhaseJoin = fn(&[u8], &[u8]) -> Result<Vec<u8>, fauna_core::error::Error>;

/// The two-phase-lattice arms (device-set, generation-mint) — delegating to
/// the byte-level joins `fauna_core::generation` owns and law-tests, never a
/// second implementation (the seen-set arm's "asserted where it lives" bar).
///
/// The join returns one of the two inputs **verbatim**, so the echo-stop is
/// byte equality with the current value and a converged pair stops
/// publishing; a `Merged` outcome is always the incoming bytes winning, which
/// `MergeOutcome::Replace` already expresses.
fn merge_two_phase_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
    join: TwoPhaseJoin,
) -> Result<MergeOutcome, MergeError> {
    let winner = join(&current.value, &incoming.value).map_err(|source| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    })?;
    if winner == current.value.as_slice() {
        return Ok(MergeOutcome::KeepCurrent);
    }
    Ok(MergeOutcome::Replace)
}

/// The `fauna.state.generation-wrap` arm (hardening) — every cell
/// is a per-healer cell, delegating to
/// [`fauna_core::generation::join_generation_wrap_per_healer`]: a decode-or-fail
/// byte-level join where a record verifying under the cell's healer beats any
/// bytes that do not, so a forged stamp displaces nothing.
///
/// A key that does not parse as a per-healer cell is not a cell of this kind
/// (the two-segment cell of the unattributed v1 shape was retired 2026-09-24
/// by the compat-remnant sweep, program 4): a row-content refusal
/// (skip-never-abort at the walk), identical at every replica.
fn merge_generation_wrap_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::generation::{
        WrapCellKey, join_generation_wrap_per_healer, parse_wrap_cell_key,
    };
    match parse_wrap_cell_key(&incoming.key) {
        Some(WrapCellKey {
            generation_id,
            target_device,
            healer,
        }) => {
            let winner = join_generation_wrap_per_healer(
                &generation_id,
                &target_device,
                &healer,
                &current.value,
                &incoming.value,
            )
            .map_err(|source| MergeError::BadValue {
                kind: incoming.kind.clone(),
                source,
            })?;
            if winner == current.value.as_slice() {
                Ok(MergeOutcome::KeepCurrent)
            } else {
                Ok(MergeOutcome::Replace)
            }
        }
        None => Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a generation-wrap logical key is three canonical hex32 segments, got {:?}",
                incoming.key
            )),
        }),
    }
}

/// First-contact contract for the wrap kind (hardening): a
/// per-healer row must verify at its cell — the join outranking but never
/// refusing a forgery that decodes (see the merge arm), adoption is where
/// forged rows are refused outright.
fn validate_generation_wrap_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::generation::{GenerationWrapRecordV2, WrapCellKey, parse_wrap_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    match parse_wrap_cell_key(&incoming.key) {
        Some(WrapCellKey {
            generation_id,
            target_device,
            healer,
        }) => {
            let record: GenerationWrapRecordV2 =
                fauna_core::encoding::canonical_decode(&incoming.value).map_err(|e| {
                    bad(format!("a per-healer generation-wrap row must decode: {e}"))
                })?;
            if !record.verifies_at(&generation_id, &target_device, &healer) {
                return Err(bad(
                    "a per-healer generation-wrap row must verify under its cell's healer \
                     at first contact"
                        .into(),
                ));
            }
            Ok(())
        }
        None => Err(bad(format!(
            "a generation-wrap logical key is three canonical hex32 segments, got {:?}",
            incoming.key
        ))),
    }
}

/// The `fauna.state.generation-unkeyable` arm — delegating to the byte-level
/// join `fauna_core::generation` owns and law-tests (the wrap cells' pattern:
/// verbatim winner, byte-equality echo-stop, `Replace` when incoming wins).
fn merge_generation_unkeyable_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::generation::{join_generation_unkeyable, parse_unkeyable_cell_key};
    let Some((generation_id, target_device)) = parse_unkeyable_cell_key(&incoming.key) else {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a generation-unkeyable logical key is two canonical hex32 segments, got {:?}",
                incoming.key
            )),
        });
    };
    let winner = join_generation_unkeyable(
        &generation_id,
        &target_device,
        &current.value,
        &incoming.value,
    )
    .map_err(|source| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    })?;
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the unkeyable kind: the row must verify under
/// its cell's target — the join outranking but never refusing a forgery
/// (see the merge arm), adoption is where a forged signal is refused
/// outright.
fn validate_generation_unkeyable_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::generation::{GenerationUnkeyableRecord, parse_unkeyable_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some((generation_id, target_device)) = parse_unkeyable_cell_key(&incoming.key) else {
        return Err(bad(format!(
            "a generation-unkeyable logical key is two canonical hex32 segments, got {:?}",
            incoming.key
        )));
    };
    let record: GenerationUnkeyableRecord = fauna_core::encoding::canonical_decode(&incoming.value)
        .map_err(|e| bad(format!("a generation-unkeyable row must decode: {e}")))?;
    if !record.verifies_at(&generation_id, &target_device) {
        return Err(bad(
            "a generation-unkeyable row must verify under its cell's target at first contact"
                .into(),
        ));
    }
    Ok(())
}

/// The `fauna.group.generation-wrap` arm — the per-healer wrap cells'
/// pattern at the group plane's actor-keyed, three-segment-only cells
/// (no legacy shape ever existed here, so an unparseable key is simply not a
/// cell of this kind).
fn merge_group_topup_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::group_generation::{join_group_topup, parse_group_topup_cell_key};
    let Some((generation_id, target_entry, healer)) = parse_group_topup_cell_key(&incoming.key)
    else {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a group generation-wrap logical key is three canonical hex32 segments, got {:?}",
                incoming.key
            )),
        });
    };
    let winner = join_group_topup(
        &generation_id,
        &target_entry,
        &healer,
        &current.value,
        &incoming.value,
    )
    .map_err(|source| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    })?;
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the group top-up kind: the row must verify
/// under its cell's healer actor — the join outranking but never refusing
/// a forgery, adoption is where a forged row is refused outright (the wrap
/// kind's split, unchanged).
fn validate_group_topup_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::group_generation::{GroupTopupRecord, parse_group_topup_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some((generation_id, target_entry, healer)) = parse_group_topup_cell_key(&incoming.key)
    else {
        return Err(bad(format!(
            "a group generation-wrap logical key is three canonical hex32 segments, got {:?}",
            incoming.key
        )));
    };
    let record: GroupTopupRecord = fauna_core::encoding::canonical_decode(&incoming.value)
        .map_err(|e| bad(format!("a group generation-wrap row must decode: {e}")))?;
    if !record.verifies_at(&generation_id, &target_entry, &healer) {
        return Err(bad(
            "a group generation-wrap row must verify under its cell's healer at first contact"
                .into(),
        ));
    }
    Ok(())
}

/// The `fauna.group.authority-revocation` arm — the group top-up arm's shape
/// over the two-segment (revoked device, revoker) cell
/// (`fauna_core::group_scope::join_authority_revocation`).
fn merge_group_authority_revocation_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::group_scope::{join_authority_revocation, parse_authority_revocation_cell_key};
    let Some((device, revoker)) = parse_authority_revocation_cell_key(&incoming.key) else {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a group authority-revocation logical key is two canonical hex32 segments, got {:?}",
                incoming.key
            )),
        });
    };
    let winner = join_authority_revocation(&device, &revoker, &current.value, &incoming.value)
        .map_err(|source| MergeError::BadValue {
            kind: incoming.kind.clone(),
            source,
        })?;
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// The `fauna.group.roster` arm — every cell is a per-writer cell
/// (`<entry>/<author>`), delegating to the key-aware
/// `fauna_core::group_scope::join_group_roster`: a row verifying at the cell
/// beats any bytes that do not, so a standing row is never displaced from
/// outside its writer's cell. A key that does not parse as a roster cell is
/// not a cell of this kind: a row-content refusal (skip-never-abort at the
/// walk), identical at every replica.
fn merge_group_roster_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::group_scope::{join_group_roster, parse_roster_cell_key};
    let Some((entry, author)) = parse_roster_cell_key(&incoming.key) else {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a group roster logical key is two canonical hex32 segments (<entry>/<author>), got {:?}",
                incoming.key
            )),
        });
    };
    let winner =
        join_group_roster(&entry, &author, &current.value, &incoming.value).map_err(|source| {
            MergeError::BadValue {
                kind: incoming.kind.clone(),
                source,
            }
        })?;
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the roster kind (the per-writer roster cell,
/// ruled 2026-09-27 — `account-data-taxonomy.md` § The recipient-set scheme
/// → *The roster kind* → *The per-writer roster cell*): the row must sit at
/// its OWN recomputed cell — entry segment = the content-derived id of its
/// core (or the signed entry id of a `Removed`), author segment = the device
/// its own embedded carriage names — and self-verify under that device
/// (`GroupRosterRecord::verifies_at`, the revocation kind's shape). So only
/// the named device can write a self-verifying row into its cell, no writer's
/// row is ever displaced by another writer's bytes, and the byte tie-break of
/// the key-aware join never crosses principals. Whether the author
/// speaks for the authority is the READER's check
/// (`fauna_core::group_scope::RosterView::build`): a merge never consults the
/// scope's birth record or its authority line.
fn validate_group_roster_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::group_scope::{GroupRosterRecord, parse_roster_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some((entry, author)) = parse_roster_cell_key(&incoming.key) else {
        return Err(bad(format!(
            "a group roster logical key is two canonical hex32 segments (<entry>/<author>), got {:?}",
            incoming.key
        )));
    };
    let record: GroupRosterRecord = fauna_core::encoding::canonical_decode(&incoming.value)
        .map_err(|e| bad(format!("a group roster row must decode: {e}")))?;
    if !record.verifies_at(&entry, &author) {
        return Err(bad(
            "a group roster row must sit at its own recomputed cell and self-verify under the \
             device its carriage names at first contact"
                .into(),
        ));
    }
    Ok(())
}

/// First-contact contract for the authority-revocation kind: the row must
/// verify under its cell's revoker — the join outranking but never refusing
/// a forgery, adoption is where a forged row is refused outright. Whether
/// the revoker
/// speaks for the authority is the READER's check
/// (`fauna_core::group_scope::GroupAuthority::build`): a merge never consults
/// the scope's birth record.
fn validate_group_authority_revocation_adoptable(
    incoming: &EntryPlaintext,
) -> Result<(), MergeError> {
    use fauna_core::group_scope::{AuthorityRevocationRecord, parse_authority_revocation_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some((device, revoker)) = parse_authority_revocation_cell_key(&incoming.key) else {
        return Err(bad(format!(
            "a group authority-revocation logical key is two canonical hex32 segments, got {:?}",
            incoming.key
        )));
    };
    let record: AuthorityRevocationRecord = fauna_core::encoding::canonical_decode(&incoming.value)
        .map_err(|e| bad(format!("a group authority-revocation row must decode: {e}")))?;
    if !record.verifies_at(&device, &revoker) {
        return Err(bad(
            "a group authority-revocation row must verify under its cell's revoker at first contact"
                .into(),
        ));
    }
    Ok(())
}

/// The `fauna.state.device-reach` arm — the unkeyable arm's shape over the
/// one-segment cell (`fauna_core::generation::join_device_reach`).
fn merge_device_reach_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::generation::{join_device_reach, parse_reach_cell_key};
    let Some(device_id) = parse_reach_cell_key(&incoming.key) else {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a device-reach logical key is one canonical hex32 segment, got {:?}",
                incoming.key
            )),
        });
    };
    let winner = join_device_reach(&device_id, &current.value, &incoming.value);
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the reach kind: the row must verify under its
/// cell's device — the join being total, adoption is where a forged
/// statement is refused outright.
fn validate_device_reach_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::generation::{DeviceReachRecord, parse_reach_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some(device_id) = parse_reach_cell_key(&incoming.key) else {
        return Err(bad(format!(
            "a device-reach logical key is one canonical hex32 segment, got {:?}",
            incoming.key
        )));
    };
    let record: DeviceReachRecord = fauna_core::encoding::canonical_decode(&incoming.value)
        .map_err(|e| bad(format!("a device-reach row must decode: {e}")))?;
    if !record.verifies_at(&device_id) {
        return Err(bad(
            "a device-reach row must verify under its cell's device at first contact".into(),
        ));
    }
    Ok(())
}

/// The `fauna.state.generation-closed` arm — the byte-order max
/// (`fauna_core::generation::join_generation_closed`) at a one-segment
/// generation-id cell.
fn merge_generation_closed_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::generation::{join_generation_closed, parse_closed_cell_key};
    if parse_closed_cell_key(&incoming.key).is_none() {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a generation-closed logical key is one canonical hex32 segment, got {:?}",
                incoming.key
            )),
        });
    }
    let winner = join_generation_closed(&current.value, &incoming.value);
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the closed kind: the key parses as a
/// generation id and the value decodes. Nothing in the value is consulted by
/// any reader, so there is no signature to check.
fn validate_generation_closed_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::generation::{GenerationClosedRecord, parse_closed_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    if parse_closed_cell_key(&incoming.key).is_none() {
        return Err(bad(format!(
            "a generation-closed logical key is one canonical hex32 segment, got {:?}",
            incoming.key
        )));
    }
    fauna_core::encoding::canonical_decode::<GenerationClosedRecord>(&incoming.value)
        .map_err(|e| bad(format!("a generation-closed row must decode: {e}")))?;
    Ok(())
}

/// The `fauna.state.device-set` arm — the reach arm's shape over the
/// one-segment cell, delegating to the key-aware
/// [`fauna_core::generation::join_device_set`] (the self-signed enrollment
/// ruling, 2026-09-16): the cell key is parsed here and handed to the join,
/// which is what lets a device's own signed row outrank a forgery filed at
/// its id. A non-canonical key is not a cell of this kind — the same
/// row-content refusal `FleetView` already applies at the read side, and
/// honest writers only ever write `hex32::encode(id)`.
fn merge_device_set_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::generation::{join_device_set, parse_device_set_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some(device_id) = parse_device_set_cell_key(&incoming.key) else {
        return Err(bad(format!(
            "a device-set logical key is one canonical hex32 segment, got {:?}",
            incoming.key
        )));
    };
    let winner =
        join_device_set(&device_id, &current.value, &incoming.value).map_err(|source| {
            MergeError::BadValue {
                kind: incoming.kind.clone(),
                source,
            }
        })?;
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the device-set kind: a canonical cell key and
/// a value the join can decode — and NOTHING about the self-signature, on
/// purpose (see the `validate_adoptable` arm): the join and the fleet view
/// are where a non-verifying row loses, so adoption stays a decode check.
fn validate_device_set_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    // The arm run against itself: exactly the merge's own key parse and
    // decode path, outcome discarded.
    merge_device_set_entry(incoming, incoming).map(|_| ())
}

/// The `fauna.group.generation-unkeyable` arm — the R14 unkeyable arm at the
/// group plane's actor-keyed cells.
fn merge_group_unkeyable_entry(
    current: &EntryPlaintext,
    incoming: &EntryPlaintext,
) -> Result<MergeOutcome, MergeError> {
    use fauna_core::group_generation::{join_group_unkeyable, parse_group_unkeyable_cell_key};
    let Some((generation_id, target_actor)) = parse_group_unkeyable_cell_key(&incoming.key) else {
        return Err(MergeError::BadValue {
            kind: incoming.kind.clone(),
            source: fauna_core::error::Error::Encoding(format!(
                "a group generation-unkeyable logical key is two canonical hex32 segments, \
                 got {:?}",
                incoming.key
            )),
        });
    };
    let winner = join_group_unkeyable(
        &generation_id,
        &target_actor,
        &current.value,
        &incoming.value,
    )
    .map_err(|source| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source,
    })?;
    if winner == current.value.as_slice() {
        Ok(MergeOutcome::KeepCurrent)
    } else {
        Ok(MergeOutcome::Replace)
    }
}

/// First-contact contract for the group unkeyable kind: the row must verify
/// under its cell's member actor.
fn validate_group_unkeyable_adoptable(incoming: &EntryPlaintext) -> Result<(), MergeError> {
    use fauna_core::group_generation::{GroupUnkeyableRecord, parse_group_unkeyable_cell_key};
    let bad = |msg: String| MergeError::BadValue {
        kind: incoming.kind.clone(),
        source: fauna_core::error::Error::Encoding(msg),
    };
    let Some((generation_id, target_actor)) = parse_group_unkeyable_cell_key(&incoming.key) else {
        return Err(bad(format!(
            "a group generation-unkeyable logical key is two canonical hex32 segments, got {:?}",
            incoming.key
        )));
    };
    let record: GroupUnkeyableRecord = fauna_core::encoding::canonical_decode(&incoming.value)
        .map_err(|e| bad(format!("a group generation-unkeyable row must decode: {e}")))?;
    if !record.verifies_at(&generation_id, &target_actor) {
        return Err(bad(
            "a group generation-unkeyable row must verify under its cell's member at \
             first contact"
                .into(),
        ));
    }
    Ok(())
}

fn stamp_of(policy: MergePolicy, entry: &EntryPlaintext) -> Result<LwwStamp, MergeError> {
    let Some(meta) = entry.merge_meta.as_ref() else {
        return Err(MergeError::MissingStamp {
            policy,
            kind: entry.kind.clone(),
        });
    };
    LwwStamp::decode(meta).map_err(|source| MergeError::BadStamp {
        kind: entry.kind.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamped(kind: &str, key: &str, value: &[u8], at_ms: i64, writer: u8) -> EntryPlaintext {
        EntryPlaintext {
            kind: kind.into(),
            key: key.into(),
            merge_meta: Some(
                LwwStamp {
                    at_ms,
                    writer: [writer; 32],
                }
                .encode()
                .unwrap()
                .into(),
            ),
            value: value.to_vec().into(),
            tombstone: false,
        }
    }

    // ── The generation-wrap arm (hardening) ───────────────────────

    /// A signed per-healer wrap entry (outer stamp carried too, as the writer
    /// dual-stamps for pre-hardening binaries).
    fn healer_wrap_entry(healer_seed: u8, at_ms: i64, wrap: &[u8]) -> ([u8; 32], EntryPlaintext) {
        let healer_key = ed25519_dalek::SigningKey::from_bytes(&[healer_seed; 32]);
        let healer = healer_key.verifying_key().to_bytes();
        let (g, t) = ([1u8; 32], [2u8; 32]);
        let record = fauna_core::generation::GenerationWrapRecordV2::Wrap {
            generation_id: g,
            target_device: t,
            healer,
            at_ms,
            wrap: wrap.to_vec(),
            healer_sig: fauna_core::generation::sign_topup_as_healer(
                &healer_key,
                &g,
                &t,
                at_ms,
                wrap,
            ),
        };
        let entry = stamped(
            KIND_GENERATION_WRAP,
            &fauna_core::generation::wrap_cell_key_per_healer(&g, &t, &healer),
            &fauna_core::encoding::canonical_encode(&record).unwrap(),
            at_ms,
            healer_seed,
        );
        (healer, entry)
    }

    /// **The pin at the seam**: a forged row — valid encoding,
    /// fabricated `i64::MAX` stamps inside and out, garbage signature — never
    /// displaces the cell healer's verifying row, in either merge order; and
    /// at first contact it is refused outright.
    #[test]
    fn a_forged_wrap_row_neither_displaces_nor_preempts_a_healers_cell() {
        let (healer, honest) = healer_wrap_entry(0x21, 7_000, b"honest-wrap-bytes");
        let forged_record = fauna_core::generation::GenerationWrapRecordV2::Wrap {
            generation_id: [1u8; 32],
            target_device: [2u8; 32],
            healer,
            at_ms: i64::MAX,
            wrap: vec![0xde; 24],
            healer_sig: vec![0xde; 64],
        };
        let forged = stamped(
            KIND_GENERATION_WRAP,
            &honest.key,
            &fauna_core::encoding::canonical_encode(&forged_record).unwrap(),
            i64::MAX,
            0xde,
        );

        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&honest), &forged).unwrap(),
            MergeOutcome::KeepCurrent,
            "a forged row never displaces the verifying one, whatever its stamps"
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&forged), &honest).unwrap(),
            MergeOutcome::Replace,
            "the verifying row displaces forged bytes already in its cell"
        );
        assert!(
            apply_class2(MergePolicy::CrdtPerField, None, &forged).is_err(),
            "first contact refuses a row that does not verify at its cell"
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &honest).unwrap(),
            MergeOutcome::Replace,
            "first contact adopts a verifying row"
        );
    }

    /// Within one healer's own re-publications the signed stamp ranks — and a
    /// replayed old row cannot claim freshness, because `at_ms` is inside the
    /// signature.
    #[test]
    fn a_healers_newer_publication_wins_its_own_cell() {
        let (_, older) = healer_wrap_entry(0x21, 7_000, b"first");
        let (_, newer) = healer_wrap_entry(0x21, 8_000, b"second");
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&older), &newer).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&newer), &older).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// The two-segment `"<generation>/<target>"` cell of the unattributed v1
    /// wrap shape was retired by the compat-remnant sweep (program 4): a
    /// stamped row at that key is no longer ranked by outer-stamp LWW — it is
    /// refused at first contact and at merge alike.
    #[test]
    fn a_two_segment_wrap_cell_is_refused() {
        let key = format!(
            "{}/{}",
            fauna_core::hex32::encode(&[1u8; 32]),
            fauna_core::hex32::encode(&[2u8; 32])
        );
        let older = stamped(KIND_GENERATION_WRAP, &key, b"older", 1_000, 1);
        let newer = stamped(KIND_GENERATION_WRAP, &key, b"newer", 2_000, 1);
        assert!(
            apply_class2(MergePolicy::CrdtPerField, None, &newer).is_err(),
            "first contact refuses a two-segment wrap cell"
        );
        assert!(
            apply_class2(MergePolicy::CrdtPerField, Some(&older), &newer).is_err(),
            "a two-segment wrap cell does not merge by outer stamp"
        );
    }

    /// The kind is CrdtPerField now, so the seam's E0 law covers it: no
    /// tombstone merges or adopts. The succession burn vehicle lands as an
    /// authenticated in-value variant instead (registry comment; charter §
    /// The generation machinery).
    #[test]
    fn wrap_kind_tombstones_are_refused_by_the_crdt_seam() {
        let (_, honest) = healer_wrap_entry(0x21, 7_000, b"w");
        let mut grave = honest.clone();
        grave.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&honest), &grave),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &grave),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
    }

    #[test]
    fn the_registered_kinds_and_the_policy_table_are_the_same_set() {
        let kinds: Vec<_> = class2_kinds().collect();
        assert_eq!(kinds.len(), POLICIES.len());
        for kind in kinds {
            assert!(merge_policy(kind).is_some(), "{kind} has no policy");
            assert!(audience_rung(kind).is_some(), "{kind} has no rung");
            assert!(sealing_epoch(kind).is_some(), "{kind} has no epoch");
        }
    }

    /// "Delegable kinds are `Gen0` by construction (their branch never gains a
    /// generation axis)" — charter § The generation machinery. A delegable
    /// `GenerationTip` row would rotate keys out from under standing grants.
    #[test]
    fn every_delegable_kind_seals_under_gen0() {
        for (kind, _, rung, epoch) in POLICIES {
            if matches!(rung, AudienceRung::Delegable) {
                assert_eq!(
                    *epoch,
                    SealingEpoch::Gen0,
                    "{kind}: a delegable kind must seal under generation 0"
                );
            }
        }
    }

    /// The bootstrap stratification (owner-key-material.md § The schedule
    /// build design): all five machinery kinds are fleet-only rung **and**
    /// gen-0 epoch, so a fresh enrolled device reads the whole mint DAG
    /// before holding any generation key.
    #[test]
    fn the_machinery_kinds_are_fleet_only_and_gen0() {
        for kind in [
            KIND_DEVICE_SET,
            KIND_GENERATION_MINT,
            KIND_GENERATION_WRAP,
            KIND_ESCROW_TARGET,
            KIND_ESCROW_RECEIPT,
        ] {
            assert_eq!(audience_rung(kind), Some(AudienceRung::FleetOnly), "{kind}");
            assert_eq!(sealing_epoch(kind), Some(SealingEpoch::Gen0), "{kind}");
        }
    }

    /// The tip-sealed set is a deliberate roster, not a drift surface: the
    /// schedule build's proof consumer, the two W8 custody kinds
    /// (2026-08-15) and the share leg's discovery cache (2026-08-17) — every
    /// member carries location data, which is exactly what generation
    /// keying's removal severance exists for — plus the T20 group-reception
    /// keypair (2026-08-17), whose argued reason is key-reach severance
    /// rather than location data: a stolen device must lose the reception
    /// secret at the member's next fleet mint, or every group holding the
    /// member keeps wrapping to a key the thief can reach — and the held
    /// machinery roots (2026-08-17, same session), which carry that same
    /// key-reach argument verbatim — and the private contact overlay
    /// (2026-09-26), whose argued reason is content severance: a person's
    /// notes are exactly what a stolen device should lose reach to at the next
    /// fleet mint (`account-data-taxonomy.md` → *The contact-overlay rung*) —
    /// and the E3 kinds of the `__config` dissolution, every one fleet-only
    /// and so tip-sealed by the schedule's own rule (`config-dissolution.md`
    /// § The kinds): the group-share ceremony record (2026-09-28, key reach:
    /// it can hold a scope's machinery root), the DNS record (2026-09-29,
    /// credential reach: a stolen device must lose the provider credentials),
    /// the ATProto app credentials (2026-09-29, credential reach: a stolen
    /// device must lose the app-credential secrets) and the subscription
    /// period keys (2026-09-29, key reach: a stolen device must lose the keys
    /// that decrypt every paid post) and the deployment seeds (2026-09-29,
    /// identity reach: a stolen device must lose the seeds that re-create
    /// every administered box's identity) — and the ATProto identity custody
    /// (2026-09-29, key reach: a stolen device must lose the senior rotation
    /// keys) — and the mail custody (2026-09-30, key reach: a stolen device
    /// must lose the MSEK and the MUA secrets) — and the followed public
    /// folders (2026-09-30, content severance: what the user follows says
    /// what they read, the overlay's argument) — and the custody-ceremony
    /// state (2026-09-30, location and relationship reach: a record carries
    /// both fleets' dial candidates and who holds the account's data, the
    /// same argument as its two registry rows) — and the peer-anchor cache
    /// (2026-09-30, social-graph reach: a stolen device must lose the list of
    /// every peer this owner anchors, the roster of whom it talks to) — and
    /// the npub confirmation stamp (2026-09-30, the schedule's rule rather
    /// than a reach argument of its own: one timestamp, but the account's
    /// succession-aftermath state, and no E3 kind seals root-derivable) — and
    /// the blessed nests (2026-09-30, key reach: a stolen device must lose
    /// which boxes renew the owner's read grants unattended, and with it the
    /// seal that would forge an un-blessed→blessed flip) — and the refused
    /// inbound scheduling changes (2026-09-30, relationship reach: a notice
    /// names who tried to change the user's calendar, and on which event) —
    /// and the admitted kind manifests (2026-10-02, relationship reach: the
    /// roster of third-party apps the user connected).
    /// A new `GenerationTip` kind extends this list in the same commit, with
    /// its own argued reason.
    #[test]
    fn the_tip_sealed_set_is_exactly_the_severance_needing_kinds() {
        let tip: Vec<_> = POLICIES
            .iter()
            .filter(|(_, _, _, epoch)| matches!(epoch, SealingEpoch::GenerationTip))
            .map(|(k, _, _, _)| *k)
            .collect();
        assert_eq!(
            tip,
            vec![
                KIND_DEVICE_ENDPOINTS,
                KIND_CUSTODIES_HELD,
                KIND_CUSTODIAN_ENDPOINTS,
                KIND_SHARE_ENDPOINTS,
                KIND_GROUP_RECEPTION_KEY,
                KIND_GROUP_MACHINERY_ROOT,
                KIND_CONTACT_OVERLAY,
                KIND_GROUP_SHARE_CEREMONY,
                KIND_BACKUP,
                KIND_DNS,
                KIND_ATPROTO,
                KIND_SUCCESSION_LEDGER,
                KIND_SUBSCRIPTIONS,
                KIND_FOLDER_KEYS,
                KIND_DEPLOYMENT_SEEDS,
                KIND_PEER_ANCHORS,
                KIND_BLESSED_NESTS,
                KIND_ATPROTO_IDENTITY,
                KIND_MAIL,
                KIND_FOLLOWS,
                KIND_CUSTODY_CEREMONY,
                KIND_NOSTR_CONFIRMATION,
                KIND_REFUSED_SCHEDULING_CHANGES,
                KIND_KIND_MANIFEST
            ]
        );
    }

    /// The departure classification covers the tip-sealed registry exactly:
    /// every `GenerationTip` row names a [`TipSealedKind`] that round-trips
    /// to its own string, and no other row names one. With `scope`'s
    /// exhaustive match, a new tip-sealed kind cannot land without choosing
    /// device-scoped or account-level.
    #[test]
    fn the_tip_sealed_kinds_are_exactly_the_registry_set() {
        for (kind, _, _, epoch) in POLICIES {
            let named = TipSealedKind::of(kind);
            assert_eq!(
                named.is_some(),
                matches!(epoch, SealingEpoch::GenerationTip),
                "{kind}: named by TipSealedKind iff it is tip-sealed"
            );
            if let Some(k) = named {
                assert_eq!(k.kind(), *kind, "{kind} round-trips");
            }
        }
        let registered: Vec<_> = POLICIES
            .iter()
            .filter(|(_, _, _, epoch)| matches!(epoch, SealingEpoch::GenerationTip))
            .map(|(k, _, _, _)| *k)
            .collect();
        let listed: Vec<_> = TipSealedKind::ALL.iter().map(|k| k.kind()).collect();
        assert_eq!(listed, registered, "`ALL` is the registry's tip-sealed set");
    }

    /// Only `device-endpoints` goes with the device that wrote it (clauses
    /// (3)(d) and (4) name reach and device-endpoints; reach is `Gen0` and
    /// retired by its own arm). The reception keypair, held group roots,
    /// custody registry rows and share endpoints are the account's; an
    /// unknown kind is kept.
    #[test]
    fn only_device_endpoints_is_retired_with_its_writer() {
        let retired: Vec<_> = POLICIES
            .iter()
            .map(|(k, _, _, _)| *k)
            .filter(|k| retired_with_its_writer(k))
            .collect();
        assert_eq!(retired, vec![KIND_DEVICE_ENDPOINTS]);
        assert!(!retired_with_its_writer(
            "fauna.state.a-kind-from-the-future"
        ));
    }

    /// The A5 routing rule: delegable kinds seal into the frozen `state`
    /// scope, fleet-only kinds into `state-fleet`, unknown kinds nowhere.
    #[test]
    fn home_scope_routes_by_rung() {
        use crate::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
        assert_eq!(
            home_scope_for_kind(KIND_MODERATION).as_deref(),
            Some(ACCOUNT_STATE_SCOPE)
        );
        assert_eq!(
            home_scope_for_kind(KIND_SEEN_SET).as_deref(),
            Some(ACCOUNT_STATE_SCOPE)
        );
        assert_eq!(
            home_scope_for_kind(KIND_DEVICE_SET).as_deref(),
            Some(ACCOUNT_STATE_FLEET_SCOPE)
        );
        assert_eq!(
            home_scope_for_kind(KIND_DEVICE_ENDPOINTS).as_deref(),
            Some(ACCOUNT_STATE_FLEET_SCOPE)
        );
        assert_eq!(
            home_scope_for_kind(KIND_CUSTODIES_HELD).as_deref(),
            Some(ACCOUNT_STATE_FLEET_SCOPE)
        );
        assert_eq!(
            home_scope_for_kind(KIND_CUSTODIAN_ENDPOINTS).as_deref(),
            Some(ACCOUNT_STATE_FLEET_SCOPE)
        );
        assert_eq!(
            home_scope_for_kind(KIND_SHARE_ENDPOINTS).as_deref(),
            Some(ACCOUNT_STATE_FLEET_SCOPE)
        );
        assert_eq!(home_scope_for_kind("fauna.no.such.kind"), None);
    }

    fn crdt_entry(kind: &str, key: &str, value: Vec<u8>) -> EntryPlaintext {
        EntryPlaintext {
            kind: kind.into(),
            key: key.into(),
            merge_meta: None,
            value: value.into(),
            tombstone: false,
        }
    }

    /// The watch item at the dispatcher: a `Removed` device-set value
    /// wins in both directions — an enrolled write arriving after the removal
    /// is `KeepCurrent`, never a resurrection. (The lattice laws themselves
    /// are asserted where the join lives, `fauna_core::generation`.)
    #[test]
    fn device_set_removal_absorbs_through_the_dispatcher() {
        use fauna_core::encoding::canonical_encode;
        use fauna_core::generation::DeviceSetRecord;
        let enrolled = canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![1; 8],
            authorization: vec![2; 8],
            enrolled_at_ms: 10,
            device_sig: Vec::new(),
        })
        .unwrap();
        let removed = canonical_encode(&DeviceSetRecord::Removed {
            removed_at_ms: 5,
            removed_by: [9; 32],
        })
        .unwrap();
        let dev = "aa".repeat(32);
        let cur = crdt_entry(KIND_DEVICE_SET, &dev, enrolled.clone());
        let grave = crdt_entry(KIND_DEVICE_SET, &dev, removed.clone());
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &grave).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&grave), &cur).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// The pin at the dispatcher: the device-set arm hands the
    /// parsed cell key to the key-aware join, so a device's own self-signed
    /// enrollment is `KeepCurrent` against a forgery carrying its cert and
    /// replaces one already merged — while adoption stays permissive about
    /// the signature on purpose: the join ranks a non-verifying row below the
    /// device's own, and the fleet view never admits it as a member.
    #[test]
    fn a_self_signed_enrollment_is_not_displaced_through_the_dispatcher() {
        use fauna_core::encoding::canonical_encode;
        use fauna_core::generation::{DeviceSetRecord, sign_device_enrollment};
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
        let cell = fauna_core::hex32::encode(&device.verifying_key().to_bytes());
        let honest = canonical_encode(&sign_device_enrollment(&device, vec![2; 8], 10)).unwrap();
        let forged = canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![0xFF; 8],
            authorization: vec![2; 8],
            enrolled_at_ms: 10,
            device_sig: vec![0xFF; 64],
        })
        .unwrap();
        let mine = crdt_entry(KIND_DEVICE_SET, &cell, honest.clone());
        let theirs = crdt_entry(KIND_DEVICE_SET, &cell, forged.clone());
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&mine), &theirs).unwrap(),
            MergeOutcome::KeepCurrent
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&theirs), &mine).unwrap(),
            MergeOutcome::Replace
        );
        // First contact: an unsigned row and the forgery are both adoptable
        // (the join and the view, not adoption, are where they lose); a
        // non-canonical cell key is not.
        let unsigned = canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![1; 8],
            authorization: vec![2; 8],
            enrolled_at_ms: 10,
            device_sig: Vec::new(),
        })
        .unwrap();
        for value in [unsigned, forged] {
            assert!(
                apply_class2(
                    MergePolicy::CrdtPerField,
                    None,
                    &crdt_entry(KIND_DEVICE_SET, &cell, value)
                )
                .is_ok()
            );
        }
        assert!(
            apply_class2(
                MergePolicy::CrdtPerField,
                None,
                &crdt_entry(KIND_DEVICE_SET, "AA", honest)
            )
            .is_err(),
            "a non-canonical device-set key is refused at first contact"
        );
    }

    /// A production-shaped roster fixture: authority device 0x41 under the
    /// authority root, the bound entry at its own two-segment cell, and the
    /// same device's authored removal of it.
    fn roster_fixture() -> (
        ed25519_dalek::SigningKey,
        String,
        fauna_core::group_scope::GroupRosterRecord,
        fauna_core::group_scope::GroupRosterRecord,
    ) {
        use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        use fauna_core::group_scope::{
            RosterEntryCore, roster_cell_key, sign_roster_enrollment, sign_roster_removal,
        };
        use fauna_core::identity::ActorKeypair;
        let authority = ActorKeypair::from_secret([0x21; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x41; 32]);
        let cert = DeviceAuthorization {
            actor_id: authority.actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&authority, &cert).unwrap();
        let authorization = canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
        let (entry_id, enrolled) = sign_roster_enrollment(
            &device,
            RosterEntryCore {
                scope_id: [1; 32],
                member_actor: fauna_core::identity::ActorId([2; 32]),
                admission_salt: [3; 32],
            },
            vec![0xE0; 8],
            authorization.clone(),
            10,
        )
        .unwrap();
        let (cell, removed) = sign_roster_removal(&device, entry_id, authorization, 20);
        assert_eq!(
            cell,
            roster_cell_key(&entry_id, &device.verifying_key().to_bytes())
        );
        (device, cell, enrolled, removed)
    }

    /// The group roster rides the same dispatcher (`crate::group_state` is
    /// its registry): within one writer's cell its authored `Removed` absorbs
    /// its `Enrolled` in both directions, and first contact adopts a row that
    /// verifies at its own cell and refuses junk.
    #[test]
    fn group_roster_removal_absorbs_through_the_dispatcher() {
        use fauna_core::encoding::canonical_encode;
        let (_, cell, enrolled, removed) = roster_fixture();
        let cur = crdt_entry(
            crate::group_state::KIND_GROUP_ROSTER,
            &cell,
            canonical_encode(&enrolled).unwrap(),
        );
        let grave = crdt_entry(
            crate::group_state::KIND_GROUP_ROSTER,
            &cell,
            canonical_encode(&removed).unwrap(),
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &grave).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&grave), &cur).unwrap(),
            MergeOutcome::KeepCurrent
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &grave).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &cur).unwrap(),
            MergeOutcome::Replace
        );
        let junk = crdt_entry(crate::group_state::KIND_GROUP_ROSTER, &cell, vec![0xFF; 4]);
        assert!(apply_class2(MergePolicy::CrdtPerField, None, &junk).is_err());
    }

    /// **The per-writer roster cell's first-contact contract**, the revocation kind's `verifies_at` shape at the
    /// roster arm: a row not sitting at its own recomputed cell — the retired
    /// one-segment key, another device's cell, another entry's cell — or one
    /// that does not self-verify under the device its carriage names (an
    /// unbound row, a re-keyed copy under a carried binding, a removal with a
    /// broken signature) is refused at adoption, whichever replica meets it
    /// first. So only the named device can write a self-verifying row into
    /// its cell, and no writer's row is ever displaced by another writer's
    /// bytes — the join's byte tie-break never crosses principals. Against
    /// an already-merged bound row, a re-keyed copy filed into the same cell
    /// still loses at the join (`KeepCurrent` / `Replace`).
    #[test]
    fn a_roster_row_not_at_its_own_cell_is_refused_at_first_contact() {
        use fauna_core::encoding::canonical_encode;
        use fauna_core::group_scope::{GroupRosterRecord, roster_cell_key};
        let (device, cell, enrolled, removed) = roster_fixture();
        let honest = canonical_encode(&enrolled).unwrap();
        let entry_id = enrolled.entry_id().unwrap();
        let GroupRosterRecord::Enrolled {
            core,
            authorization,
            authority_sig,
            enrolled_at_ms,
            ..
        } = enrolled
        else {
            unreachable!()
        };
        let forged = canonical_encode(&GroupRosterRecord::Enrolled {
            core,
            reception_pubkey: vec![0xFF; 8],
            authorization: authorization.clone(),
            authority_sig,
            enrolled_at_ms,
            binding_sig: Vec::new(),
        })
        .unwrap();
        let mine = crdt_entry(crate::group_state::KIND_GROUP_ROSTER, &cell, honest.clone());
        let theirs = crdt_entry(crate::group_state::KIND_GROUP_ROSTER, &cell, forged.clone());
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&mine), &theirs).unwrap(),
            MergeOutcome::KeepCurrent
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&theirs), &mine).unwrap(),
            MergeOutcome::Replace
        );
        // First contact: the bound row at its own cell adopts…
        assert!(apply_class2(MergePolicy::CrdtPerField, None, &mine).is_ok());
        // …and every displacement shape is refused outright.
        let refusals = [
            ("unbound / re-keyed copy", cell.clone(), forged),
            (
                "one-segment key",
                fauna_core::hex32::encode(&entry_id),
                honest.clone(),
            ),
            (
                "another device's cell",
                roster_cell_key(&entry_id, &[0xAA; 32]),
                honest.clone(),
            ),
            (
                "another entry's cell",
                roster_cell_key(&[0xBB; 32], &device.verifying_key().to_bytes()),
                honest,
            ),
            ("broken removal signature", cell.clone(), {
                let GroupRosterRecord::Removed {
                    entry_id,
                    authorization,
                    removed_at_ms,
                    ..
                } = removed
                else {
                    unreachable!()
                };
                canonical_encode(&GroupRosterRecord::Removed {
                    entry_id,
                    authorization,
                    removed_at_ms,
                    remover_sig: vec![0xFF; 64],
                })
                .unwrap()
            }),
        ];
        for (what, key, value) in refusals {
            let row = crdt_entry(crate::group_state::KIND_GROUP_ROSTER, &key, value);
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &row),
                    Err(MergeError::BadValue { .. })
                ),
                "{what} must be refused at first contact"
            );
        }
    }

    /// The group top-up kind's first-contact strictness: a row that does not
    /// verify under its cell's healer actor is refused at adoption (the join
    /// only outranks one that decodes).
    #[test]
    fn a_forged_group_topup_is_refused_at_first_contact() {
        use fauna_core::encoding::canonical_encode;
        use fauna_core::group_generation::{
            GroupTopupRecord, group_topup_cell_key, sign_group_topup_as_healer,
        };
        use fauna_core::identity::ActorKeypair;
        let healer = ActorKeypair::from_secret([0x31; 32]);
        let generation = [0x51; 32];
        let target = [0x52; 32];
        let wrap = vec![0xAB; 16];
        let cell = group_topup_cell_key(&generation, &target, &healer.actor_id().0);
        let honest = GroupTopupRecord::Wrap {
            generation_id: generation,
            target_entry: target,
            healer: healer.actor_id().0,
            at_ms: 7_000,
            wrap: wrap.clone(),
            healer_sig: sign_group_topup_as_healer(
                healer.signing_key(),
                &generation,
                &target,
                7_000,
                &wrap,
            ),
        };
        let ok = crdt_entry(
            crate::group_state::KIND_GROUP_GENERATION_WRAP,
            &cell,
            canonical_encode(&honest).unwrap(),
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &ok).unwrap(),
            MergeOutcome::Replace
        );
        // The same record at ANOTHER healer's cell does not verify there.
        let foreign_cell = group_topup_cell_key(
            &generation,
            &target,
            &ActorKeypair::from_secret([0x32; 32]).actor_id().0,
        );
        let squatting = crdt_entry(
            crate::group_state::KIND_GROUP_GENERATION_WRAP,
            &foreign_cell,
            canonical_encode(&honest).unwrap(),
        );
        assert!(apply_class2(MergePolicy::CrdtPerField, None, &squatting).is_err());
    }

    /// The authority-revocation kind through the dispatcher: a row verifying
    /// under its cell's revoker is adopted; junk that meets it is refused, never
    /// kept or ranked (decode-or-fail); the same row re-filed at another cell
    /// is refused at first contact.
    #[test]
    fn a_group_authority_revocation_is_cell_bound_through_the_dispatcher() {
        use fauna_core::encoding::canonical_encode;
        use fauna_core::group_scope::{authority_revocation_cell_key, sign_authority_revocation};
        let kind = crate::group_state::KIND_GROUP_AUTHORITY_REVOCATION;
        let revoker = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let (cell, record) =
            sign_authority_revocation(&revoker, vec![0xC0; 8], [0x51; 32], [0x41; 32], 5_000);
        let value = canonical_encode(&record).unwrap();
        let honest = crdt_entry(kind, &cell, value.clone());
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &honest).unwrap(),
            MergeOutcome::Replace
        );
        let junk = crdt_entry(kind, &cell, vec![0xFF; value.len() + 8]);
        assert!(apply_class2(MergePolicy::CrdtPerField, Some(&honest), &junk).is_err());
        assert!(apply_class2(MergePolicy::CrdtPerField, None, &junk).is_err());
        let refiled = crdt_entry(
            kind,
            &authority_revocation_cell_key(&[0x43; 32], &revoker.verifying_key().to_bytes()),
            value,
        );
        assert!(apply_class2(MergePolicy::CrdtPerField, None, &refiled).is_err());
    }

    /// **A merge never ranks a value it cannot decode** (`transport.md` §
    /// Schema and forward-compat discipline → *Rule 3 in full*, the
    /// `consensus` ground), pinned for the six verifying-preferred byte joins
    /// whose record types are closed by design. At an OCCUPIED cell, an
    /// incoming row that does not decode — junk, or a variant a later build
    /// writes — is a row-content refusal, never `KeepCurrent`: the walk's
    /// skip arm leaves it unaccounted (not journaled, not counted `kept`), so
    /// `reconcile` presents it again once a build can read it. Keeping it
    /// instead would journal the row as handled, and the store's idempotent
    /// ingest would then never let the later build apply it. The undecodable
    /// side as `current` refuses too: nothing ranks against what this build
    /// cannot read. (A decodable row that does not verify is still outranked
    /// — each kind's own forgery pin.)
    #[test]
    fn an_undecodable_row_at_an_occupied_cell_is_refused_never_kept() {
        use crate::group_state::{
            KIND_GROUP_AUTHORITY_REVOCATION, KIND_GROUP_GENERATION_UNKEYABLE,
            KIND_GROUP_GENERATION_WRAP, KIND_GROUP_ROSTER,
        };
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorKeypair;

        let wrap = healer_wrap_entry(0x21, 7_000, b"honest-wrap-bytes").1;

        let unkeyable = {
            use fauna_core::generation::{
                GenerationUnkeyableRecord, UNKEYABLE_VARIANT_SATISFIED, sign_unkeyable_as_target,
                unkeyable_cell_key,
            };
            let target_key = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
            let target = target_key.verifying_key().to_bytes();
            let generation = [0xAA; 32];
            let rec = GenerationUnkeyableRecord::Satisfied {
                generation_id: generation,
                target_device: target,
                asserted_at_ms: 9_000,
                target_sig: sign_unkeyable_as_target(
                    &target_key,
                    &generation,
                    UNKEYABLE_VARIANT_SATISFIED,
                    9_000,
                    &[],
                ),
            };
            crdt_entry(
                KIND_GENERATION_UNKEYABLE,
                &unkeyable_cell_key(&generation, &target),
                canonical_encode(&rec).unwrap(),
            )
        };

        let group_topup = {
            use fauna_core::group_generation::{
                GroupTopupRecord, group_topup_cell_key, sign_group_topup_as_healer,
            };
            let healer = ActorKeypair::from_secret([0x31; 32]);
            let (generation, target, wrap) = ([0x51; 32], [0x52; 32], vec![0xAB; 16]);
            let rec = GroupTopupRecord::Wrap {
                generation_id: generation,
                target_entry: target,
                healer: healer.actor_id().0,
                at_ms: 7_000,
                wrap: wrap.clone(),
                healer_sig: sign_group_topup_as_healer(
                    healer.signing_key(),
                    &generation,
                    &target,
                    7_000,
                    &wrap,
                ),
            };
            crdt_entry(
                KIND_GROUP_GENERATION_WRAP,
                &group_topup_cell_key(&generation, &target, &healer.actor_id().0),
                canonical_encode(&rec).unwrap(),
            )
        };

        let group_unkeyable = {
            use fauna_core::generation::UNKEYABLE_VARIANT_SATISFIED;
            use fauna_core::group_generation::{
                GroupUnkeyableRecord, group_unkeyable_cell_key, sign_group_unkeyable_as_target,
            };
            let target = ActorKeypair::from_secret([0x33; 32]);
            let generation = [0x53; 32];
            let rec = GroupUnkeyableRecord::Satisfied {
                generation_id: generation,
                target_actor: target.actor_id().0,
                asserted_at_ms: 2_000,
                target_sig: sign_group_unkeyable_as_target(
                    target.signing_key(),
                    &generation,
                    UNKEYABLE_VARIANT_SATISFIED,
                    2_000,
                    &[],
                ),
            };
            crdt_entry(
                KIND_GROUP_GENERATION_UNKEYABLE,
                &group_unkeyable_cell_key(&generation, &target.actor_id().0),
                canonical_encode(&rec).unwrap(),
            )
        };

        let roster = {
            let (_, cell, enrolled, _) = roster_fixture();
            crdt_entry(
                KIND_GROUP_ROSTER,
                &cell,
                canonical_encode(&enrolled).unwrap(),
            )
        };

        let revocation = {
            let revoker = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
            let (cell, rec) = fauna_core::group_scope::sign_authority_revocation(
                &revoker,
                vec![0xC0; 8],
                [0x51; 32],
                [0x41; 32],
                5_000,
            );
            crdt_entry(
                KIND_GROUP_AUTHORITY_REVOCATION,
                &cell,
                canonical_encode(&rec).unwrap(),
            )
        };

        // A variant no build of this major knows yet, canonically encoded.
        #[derive(serde::Serialize)]
        enum Later {
            NotYetInvented { at_ms: i64 },
        }
        let later = canonical_encode(&Later::NotYetInvented { at_ms: i64::MAX }).unwrap();

        for honest in [
            wrap,
            unkeyable,
            group_topup,
            group_unkeyable,
            roster,
            revocation,
        ] {
            let kind = honest.kind.clone();
            assert_eq!(
                apply_class2(MergePolicy::CrdtPerField, None, &honest).unwrap(),
                MergeOutcome::Replace,
                "{kind}: the honest row is adoptable — the cell is occupiable"
            );
            for value in [b"junk".to_vec(), later.clone()] {
                let undecodable = EntryPlaintext {
                    value: value.into(),
                    ..honest.clone()
                };
                for (current, incoming) in [(&honest, &undecodable), (&undecodable, &honest)] {
                    let err = apply_class2(MergePolicy::CrdtPerField, Some(current), incoming)
                        .expect_err("an undecodable side is refused, never ranked");
                    assert!(
                        err.is_row_content(),
                        "{kind}: a row-content refusal, so the walk skips the row \
                         unaccounted: {err}"
                    );
                }
            }
        }
    }

    /// The mint's shred marker is an in-value absorbing state (the build
    /// ruling): `Shredded` replaces `Minted` and a stale `Minted` never
    /// resurrects a shredded generation.
    #[test]
    fn mint_shred_absorbs_through_the_dispatcher() {
        use fauna_core::encoding::canonical_encode;
        use fauna_core::generation::{GenerationMintRecord, MintCore};
        let core = MintCore {
            parents: vec![[1; 32]],
            member_ids: vec![[2; 32]],
            minter: [2; 32],
            key_commitment: [3; 32],
            minted_at_ms: 1,
        };
        let minted = canonical_encode(&GenerationMintRecord::Minted {
            core: core.clone(),
            minter_sig: vec![0xAA; 64],
            wraps: Vec::new(),
        })
        .unwrap();
        let shredded = canonical_encode(&GenerationMintRecord::Shredded {
            core,
            shredded_at_ms: 2,
            shredded_by: [2; 32],
        })
        .unwrap();
        let gen_id = "bb".repeat(32);
        let live = crdt_entry(KIND_GENERATION_MINT, &gen_id, minted.clone());
        let shred = crdt_entry(KIND_GENERATION_MINT, &gen_id, shredded.clone());
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&live), &shred).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&shred), &live).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// T14 tombstones stay refused on the machinery CRDT kinds — deletion is
    /// a lattice phase there, never a tombstone (the build ruling; the E0
    /// convergence law is why).
    #[test]
    fn a_tombstone_on_a_machinery_crdt_kind_is_refused() {
        let dev = "cc".repeat(32);
        let cur = crdt_entry(KIND_DEVICE_SET, &dev, vec![1]);
        let mut grave = crdt_entry(KIND_DEVICE_SET, &dev, Vec::new());
        grave.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &grave),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
    }

    /// Tombstone admissibility is declared per policy, and the declaration
    /// agrees with what [`apply_class2`] will actually do — the drift this pins
    /// is a policy that says "yes" while the reader refuses, which is a wedge
    /// the writer door would then wave through.
    #[test]
    fn a_policy_admits_a_tombstone_only_where_the_reader_can_resolve_one() {
        for policy in [
            MergePolicy::Immutable,
            MergePolicy::CrdtPerField,
            MergePolicy::LatestWins,
            MergePolicy::ThreeWay,
            MergePolicy::NestCas,
        ] {
            let admits = policy.admits_tombstone();
            assert_eq!(
                admits,
                matches!(policy, MergePolicy::LatestWins | MergePolicy::NestCas),
                "{policy:?}: a deletion is orderable only where the policy already \
                 totally orders values — read `admits_tombstone` before changing this"
            );
            // A stamped policy's tombstone is ordered by its stamp alone, so the
            // two declarations have to be consistent: nothing may require a
            // stamp it is not allowed to carry a value for.
            if policy.requires_stamp() {
                assert!(admits, "{policy:?} orders by stamp but refuses a tombstone");
            }
        }
    }

    /// The one arm that had no stated answer before: an `Immutable` tombstone
    /// comes back `KeepCurrent`, i.e. the delete silently does not happen. That
    /// is *why* the policy refuses one at the writer door rather than leaving
    /// the caller to be told a lie.
    #[test]
    fn an_immutable_tombstone_would_silently_not_delete() {
        let cur = stamped("k", "k", b"v", 1, 1);
        let mut grave = cur.clone();
        grave.tombstone = true;
        grave.value = Vec::new().into();
        assert_eq!(
            apply_class2(MergePolicy::Immutable, Some(&cur), &grave).unwrap(),
            MergeOutcome::KeepCurrent
        );
        assert!(!MergePolicy::Immutable.admits_tombstone());
    }

    /// The compat contract: an unknown kind is `None`, never a default policy.
    #[test]
    fn an_unregistered_kind_has_no_policy() {
        assert_eq!(merge_policy("fauna.state.something-newer"), None);
        assert_eq!(merge_policy(""), None);
    }

    /// The E0 retirement pin (the dissolution schedule): the whole-record
    /// `UserConfig` kind is GONE from the plane and its string is
    /// retired-never-reuse — re-registering it at any shape re-creates the
    /// hazard E0 removed (a sealable whole-record kind spanning both rungs).
    #[test]
    fn the_retired_user_config_kind_is_not_registered() {
        assert_eq!(merge_policy("fauna.state.user-config"), None);
        assert_eq!(audience_rung("fauna.state.user-config"), None);
        assert!(!class2_kinds().any(|k| k == "fauna.state.user-config"));
    }

    /// Same freeze for the preference cluster — all four are key-schedule
    /// inputs the moment anything seals under them, and the singletons share
    /// one logical key.
    #[test]
    fn the_preference_cluster_kind_strings_are_frozen() {
        assert_eq!(KIND_MODERATION, "fauna.state.moderation");
        assert_eq!(KIND_SYNC_PREFS, "fauna.state.sync-prefs");
        assert_eq!(KIND_PERSONALIZATION, "fauna.state.personalization");
        assert_eq!(KIND_DELEGATION, "fauna.state.delegation");
        assert_eq!(MODERATION_KEY, "self");
        assert_eq!(PREFERENCE_KEY, "self");
    }

    /// The whole preference cluster is delegable whole-record LWW — the
    /// disposition table's row, held as a set so a future edit to one sibling
    /// is a deliberate divergence rather than a drift.
    #[test]
    fn the_preference_cluster_registers_uniformly() {
        for kind in [
            KIND_MODERATION,
            KIND_SYNC_PREFS,
            KIND_PERSONALIZATION,
            KIND_DELEGATION,
        ] {
            assert_eq!(merge_policy(kind), Some(MergePolicy::LatestWins), "{kind}");
            assert_eq!(audience_rung(kind), Some(AudienceRung::Delegable), "{kind}");
        }
    }

    #[test]
    fn a_valid_new_item_is_adopted_whatever_the_policy() {
        // The opaque-value policies adopt any bytes (their merges never
        // decode); the decoding policies adopt a row their own merge could
        // handle. CrdtPerField uses a real registered kind + value because
        // adoption runs the arm's decoder (`validate_adoptable`).
        for policy in [
            MergePolicy::Immutable,
            MergePolicy::LatestWins,
            MergePolicy::ThreeWay,
            MergePolicy::NestCas,
        ] {
            let incoming = stamped("k", "i", b"v", 1, 1);
            assert_eq!(
                apply_class2(policy, None, &incoming).unwrap(),
                MergeOutcome::Replace,
                "{policy:?}"
            );
        }
        let incoming = seen_entry("scope", &seen(&[], &[(1, 1)]));
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &incoming).unwrap(),
            MergeOutcome::Replace
        );
    }

    /// The first-contact contract: a row whose own content no merge could
    /// handle is refused at ADOPTION, not adopted-then-wedged. Without this,
    /// arrival order decides truth — a replica meeting the poisoned row
    /// first adopts it and keeps it forever (row-content refusals skip),
    /// while a replica meeting a valid row first keeps that: permanent
    /// divergence any fleet-key holder can mint by front-running a key.
    #[test]
    fn a_poisoned_first_contact_row_is_refused_not_adopted() {
        // Undecodable CRDT value.
        let junk = crdt_entry(KIND_DEVICE_SET, "aa", b"junk".to_vec());
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &junk),
            Err(MergeError::BadValue { .. })
        ));
        // CRDT tombstone — unresolvable by design.
        let mut ts = crdt_entry(KIND_SEEN_SET, "scope", Vec::new());
        ts.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &ts),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        // Unregistered CRDT kind — no arm could ever merge it.
        let unknown = crdt_entry("fauna.state.no-such-kind", "k", Vec::new());
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &unknown),
            Err(MergeError::UnknownKind { .. })
        ));
        // Stampless LatestWins — nothing could ever rank against it.
        let stampless = EntryPlaintext {
            kind: "k".into(),
            key: "i".into(),
            merge_meta: None,
            value: b"v".to_vec().into(),
            tombstone: false,
        };
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, None, &stampless),
            Err(MergeError::MissingStamp { .. })
        ));
        // Immutable tombstone — the policy cannot resolve one, and adopting
        // it would let a hostile writer squat a key with a permanent
        // deletion marker.
        let mut imm_ts = stamped("k", "i", b"", 1, 1);
        imm_ts.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::Immutable, None, &imm_ts),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
    }

    /// The reach kind's registry contract: fleet-only Gen0 CRDT registration,
    /// first-contact strictness (a forged row is refused at adoption, a
    /// verifying one adopted), and the arm delegating to the stamp-ranked
    /// join (a later verifying re-publication replaces, a replayed older one
    /// is kept out, junk is kept out).
    #[test]
    fn the_reach_kind_registers_and_adopts_verifying_rows_only() {
        use fauna_core::generation::{reach_cell_key, sign_device_reach};
        assert_eq!(
            merge_policy(KIND_DEVICE_REACH),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_DEVICE_REACH),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(sealing_epoch(KIND_DEVICE_REACH), Some(SealingEpoch::Gen0));

        let key = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
        let device = key.verifying_key().to_bytes();
        let cell = reach_cell_key(&device);
        let record = |at_ms: i64, holds: Vec<[u8; 32]>| {
            fauna_core::encoding::canonical_encode(&sign_device_reach(&key, at_ms, holds)).unwrap()
        };
        let forged = crdt_entry(KIND_DEVICE_REACH, &cell, b"junk".to_vec());
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &forged),
            Err(MergeError::BadValue { .. })
        ));
        let first = crdt_entry(KIND_DEVICE_REACH, &cell, record(1_000, vec![[7u8; 32]]));
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &first).unwrap(),
            MergeOutcome::Replace
        );
        let later = crdt_entry(
            KIND_DEVICE_REACH,
            &cell,
            record(2_000, vec![[7u8; 32], [8u8; 32]]),
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&first), &later).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&later), &first).unwrap(),
            MergeOutcome::KeepCurrent
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&later), &forged).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// The closed kind's registry contract: the frozen string, fleet-only
    /// Gen0 CRDT registration, adoption on a generation-id key and a value
    /// that decodes (no signature — nothing in the value is consulted), and
    /// the arm merging by byte-order max.
    #[test]
    fn the_closed_kind_registers_and_merges_by_byte_order_max() {
        use fauna_core::generation::{GenerationClosedRecord, closed_cell_key};
        assert_eq!(KIND_GENERATION_CLOSED, "fauna.state.generation-closed");
        assert_eq!(
            merge_policy(KIND_GENERATION_CLOSED),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_GENERATION_CLOSED),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_GENERATION_CLOSED),
            Some(SealingEpoch::Gen0)
        );

        let cell = closed_cell_key(&[0xAAu8; 32]);
        let record = |by: u8| {
            fauna_core::encoding::canonical_encode(&GenerationClosedRecord {
                closed_by: [by; 32],
                answers: [0x0B; 32],
                closed_at_ms: 1_000,
            })
            .unwrap()
        };

        // First contact: junk and a key that is no generation id are refused.
        let junk = crdt_entry(KIND_GENERATION_CLOSED, &cell, b"junk".to_vec());
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &junk),
            Err(MergeError::BadValue { .. })
        ));
        let misfiled = crdt_entry(KIND_GENERATION_CLOSED, "not-a-generation", record(1));
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &misfiled),
            Err(MergeError::BadValue { .. })
        ));
        let low = crdt_entry(KIND_GENERATION_CLOSED, &cell, record(1));
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &low).unwrap(),
            MergeOutcome::Replace
        );

        // Two removers closing one generation: the byte-order max, either way.
        let high = crdt_entry(KIND_GENERATION_CLOSED, &cell, record(2));
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&low), &high).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&high), &low).unwrap(),
            MergeOutcome::KeepCurrent
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&high), &high).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// The unkeyable kind's registry contract in one place: fleet-only Gen0
    /// CRDT registration, first-contact strictness (a forged signal is refused
    /// at adoption, a verifying one adopted), and the arm delegating to the
    /// stamp-ranked join (a later verifying `Satisfied` replaces the
    /// assertion; a replayed older assertion is kept out).
    #[test]
    fn the_unkeyable_kind_registers_and_adopts_verifying_rows_only() {
        use fauna_core::generation::{
            GenerationUnkeyableRecord, UNKEYABLE_VARIANT_ASSERTED, UNKEYABLE_VARIANT_SATISFIED,
            sign_unkeyable_as_target, unkeyable_cell_key,
        };
        assert_eq!(
            merge_policy(KIND_GENERATION_UNKEYABLE),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_GENERATION_UNKEYABLE),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_GENERATION_UNKEYABLE),
            Some(SealingEpoch::Gen0)
        );

        let target_key = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
        let target = target_key.verifying_key().to_bytes();
        let generation = [0xAAu8; 32];
        let cell = unkeyable_cell_key(&generation, &target);
        let record = |variant: u8, at_ms: i64, tried: Vec<[u8; 32]>| {
            let sig = sign_unkeyable_as_target(&target_key, &generation, variant, at_ms, &tried);
            let rec = if variant == UNKEYABLE_VARIANT_ASSERTED {
                GenerationUnkeyableRecord::Asserted {
                    generation_id: generation,
                    target_device: target,
                    asserted_at_ms: at_ms,
                    tried,
                    target_sig: sig,
                }
            } else {
                GenerationUnkeyableRecord::Satisfied {
                    generation_id: generation,
                    target_device: target,
                    asserted_at_ms: at_ms,
                    target_sig: sig,
                }
            };
            fauna_core::encoding::canonical_encode(&rec).unwrap()
        };

        // First contact: forged refused, verifying adopted.
        let forged = crdt_entry(KIND_GENERATION_UNKEYABLE, &cell, b"junk".to_vec());
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &forged),
            Err(MergeError::BadValue { .. })
        ));
        let asserted = crdt_entry(
            KIND_GENERATION_UNKEYABLE,
            &cell,
            record(UNKEYABLE_VARIANT_ASSERTED, 1_000, vec![[7u8; 32]]),
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, None, &asserted).unwrap(),
            MergeOutcome::Replace
        );

        // The arm ranks by the signed stamp: retraction wins, replay loses.
        let satisfied = crdt_entry(
            KIND_GENERATION_UNKEYABLE,
            &cell,
            record(UNKEYABLE_VARIANT_SATISFIED, 2_000, vec![]),
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&asserted), &satisfied).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&satisfied), &asserted).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    #[test]
    fn an_identical_entry_is_kept_whatever_the_policy() {
        for policy in [
            MergePolicy::Immutable,
            MergePolicy::CrdtPerField,
            MergePolicy::LatestWins,
            MergePolicy::ThreeWay,
            MergePolicy::NestCas,
        ] {
            let e = stamped("k", "i", b"v", 1, 1);
            assert_eq!(
                apply_class2(policy, Some(&e), &e).unwrap(),
                MergeOutcome::KeepCurrent,
                "{policy:?}"
            );
        }
    }

    #[test]
    fn latest_wins_compares_stamps_not_arrival_order() {
        let older = stamped("k", "i", b"old", 10, 1);
        let newer = stamped("k", "i", b"new", 20, 1);
        assert_eq!(
            apply_class2(MergePolicy::LatestWins, Some(&older), &newer).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::LatestWins, Some(&newer), &older).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// Same millisecond, two writers: both replicas must pick the SAME side,
    /// or they diverge permanently with nothing left to exchange.
    #[test]
    fn a_same_millisecond_tie_resolves_to_the_same_side_on_both_replicas() {
        let a = stamped("k", "i", b"a", 7, 0x01);
        let b = stamped("k", "i", b"b", 7, 0x02);
        // Replica holding `a` sees `b`: b's writer sorts higher → adopt.
        assert_eq!(
            apply_class2(MergePolicy::LatestWins, Some(&a), &b).unwrap(),
            MergeOutcome::Replace
        );
        // Replica holding `b` sees `a`: keep. Both end on `b`.
        assert_eq!(
            apply_class2(MergePolicy::LatestWins, Some(&b), &a).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// A pre-epoch stamp must sort below a positive one — the i64→u64 offset
    /// in `rank` is what makes that true for a big-endian byte comparison.
    #[test]
    fn a_negative_stamp_sorts_below_a_positive_one() {
        let before = stamped("k", "i", b"before", -5, 1);
        let after = stamped("k", "i", b"after", 5, 1);
        assert_eq!(
            apply_class2(MergePolicy::LatestWins, Some(&before), &after).unwrap(),
            MergeOutcome::Replace
        );
        assert_eq!(
            apply_class2(MergePolicy::LatestWins, Some(&after), &before).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    #[test]
    fn a_latest_wins_entry_without_a_stamp_fails_loudly() {
        let mut bare = stamped("k", "i", b"v", 1, 1);
        bare.merge_meta = None;
        let other = stamped("k", "i", b"w", 2, 1);
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&bare), &other),
            Err(MergeError::MissingStamp { .. })
        ));
    }

    #[test]
    fn malformed_stamp_bytes_fail_loudly() {
        let mut junk = stamped("k", "i", b"v", 1, 1);
        junk.merge_meta = Some(vec![0xff, 0xff, 0xff].into());
        let other = stamped("k", "i", b"w", 2, 1);
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&junk), &other),
            Err(MergeError::BadStamp { .. })
        ));
    }

    #[test]
    fn an_immutable_kinds_second_value_never_displaces_the_first() {
        let first = stamped("k", "i", b"first", 1, 1);
        let second = stamped("k", "i", b"second", 99, 1);
        assert_eq!(
            apply_class2(MergePolicy::Immutable, Some(&first), &second).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    #[test]
    fn a_nest_cas_row_is_applied_truth_and_replaces() {
        let cur = stamped("k", "i", b"a", 1, 1);
        let next = stamped("k", "i", b"b", 1, 1);
        assert_eq!(
            apply_class2(MergePolicy::NestCas, Some(&cur), &next).unwrap(),
            MergeOutcome::Replace
        );
    }

    #[test]
    fn a_three_way_kind_escalates_rather_than_guessing() {
        let cur = stamped("k", "i", b"a", 1, 1);
        let next = stamped("k", "i", b"b", 2, 1);
        assert_eq!(
            apply_class2(MergePolicy::ThreeWay, Some(&cur), &next).unwrap(),
            MergeOutcome::NeedsThreeWay
        );
    }

    #[test]
    fn a_kind_mismatch_between_the_two_sides_is_refused() {
        let cur = stamped("k1", "i", b"a", 1, 1);
        let next = stamped("k2", "i", b"b", 2, 1);
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&cur), &next),
            Err(MergeError::KindMismatch { .. })
        ));
    }

    // The plane-level `UserConfig` merge tests that stood here retired with
    // the E0 arm removal — their properties are held by the seen-set arm
    // tests below (convergence in bytes, echo-stop, tombstone refusal,
    // bad-value refusal) and by the per-record `merge` byte-law tests in
    // `fauna_core`, where the delegates live.

    #[test]
    fn a_crdt_kind_with_no_merge_arm_is_refused_rather_than_defaulted() {
        let cur = stamped("fauna.state.not-built", "i", b"a", 1, 1);
        let next = stamped("fauna.state.not-built", "i", b"b", 2, 1);
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &next),
            Err(MergeError::UnknownKind { .. })
        ));
    }

    #[test]
    fn lww_stamps_round_trip_through_canonical_dag_cbor() {
        let s = LwwStamp {
            at_ms: -1234,
            writer: [0x5a; 32],
        };
        assert_eq!(LwwStamp::decode(&s.encode().unwrap()).unwrap(), s);
    }

    // ── The seen-set arm ─────────────────────────────────────────────────────

    /// The kind string is a key-schedule input; its logical keys are scope
    /// names, not a singleton constant — both facts are part of the frozen
    /// registration.
    #[test]
    fn the_seen_set_kind_string_is_frozen() {
        assert_eq!(KIND_SEEN_SET, "fauna.state.seen-set");
    }

    /// The item-2 registration, held as a pair: union CRDT (the charter's
    /// merge-table row for the seen-set) at the delegable rung (the recorded
    /// ruling — materialization is the argued need). Changing either half is
    /// a re-registration under a new kind string, never an edit here.
    #[test]
    fn the_seen_set_registers_as_delegable_union_crdt() {
        assert_eq!(merge_policy(KIND_SEEN_SET), Some(MergePolicy::CrdtPerField));
        assert_eq!(audience_rung(KIND_SEEN_SET), Some(AudienceRung::Delegable));
    }

    /// The item-3 registration, held as a pair: whole-record LWW (T5 — each
    /// device supersedes only its own row) at the **fleet-only** rung
    /// (default-narrow, no argued need; endpoint entries are location data,
    /// and the R14 gate's refusal until the generation schedule is the
    /// removal-severance those entries want, not an oversight to route
    /// around). Its kind string is a key-schedule input, frozen like the
    /// rest.
    #[test]
    fn the_device_endpoints_register_as_fleet_only_lww() {
        assert_eq!(KIND_DEVICE_ENDPOINTS, "fauna.state.device-endpoints");
        assert_eq!(
            merge_policy(KIND_DEVICE_ENDPOINTS),
            Some(MergePolicy::LatestWins)
        );
        assert_eq!(
            audience_rung(KIND_DEVICE_ENDPOINTS),
            Some(AudienceRung::FleetOnly)
        );
        // LWW admits the stamped removal tombstone the peer registry needs.
        assert!(MergePolicy::LatestWins.admits_tombstone());
    }

    /// The W8 custody registry pair, held per the device-endpoints shape:
    /// whole-record LWW (one row per custody grant id, single-writer per
    /// row) at the fleet-only rung, tip-sealed (location data — the owner
    /// fleet's candidates on the held side, the custodian's on the owner
    /// side). Kind strings are key-schedule inputs, frozen.
    #[test]
    fn the_custody_registry_kinds_register_as_fleet_only_lww_tip_sealed() {
        assert_eq!(KIND_CUSTODIES_HELD, "fauna.state.custodies-held");
        assert_eq!(KIND_CUSTODIAN_ENDPOINTS, "fauna.state.custodian-endpoints");
        for kind in [KIND_CUSTODIES_HELD, KIND_CUSTODIAN_ENDPOINTS] {
            assert_eq!(merge_policy(kind), Some(MergePolicy::LatestWins), "{kind}");
            assert_eq!(audience_rung(kind), Some(AudienceRung::FleetOnly), "{kind}");
            assert_eq!(
                sealing_epoch(kind),
                Some(SealingEpoch::GenerationTip),
                "{kind}"
            );
        }
    }

    /// The share leg's discovery cache registers as the third member of the
    /// location-data family — same three properties as its two siblings, for
    /// the same reasons (the kind const carries them). Kind strings are
    /// key-schedule inputs, frozen.
    #[test]
    fn the_share_endpoints_kind_registers_as_fleet_only_lww_tip_sealed() {
        assert_eq!(KIND_SHARE_ENDPOINTS, "fauna.state.share-endpoints");
        assert_eq!(
            merge_policy(KIND_SHARE_ENDPOINTS),
            Some(MergePolicy::LatestWins)
        );
        assert_eq!(
            audience_rung(KIND_SHARE_ENDPOINTS),
            Some(AudienceRung::FleetOnly),
            "a counterparty's dial candidates must never travel past this fleet"
        );
        assert_eq!(
            sealing_epoch(KIND_SHARE_ENDPOINTS),
            Some(SealingEpoch::GenerationTip)
        );
    }

    fn seen_entry(scope_key: &str, s: &fauna_core::seen_set::SeenScopeSet) -> EntryPlaintext {
        EntryPlaintext {
            kind: KIND_SEEN_SET.into(),
            key: scope_key.into(),
            merge_meta: None,
            value: fauna_core::encoding::canonical_encode(s).unwrap().into(),
            tombstone: false,
        }
    }

    fn seen(watermarks: &[(u8, u64)], refs: &[(u8, u64)]) -> fauna_core::seen_set::SeenScopeSet {
        let mut s = fauna_core::seen_set::SeenScopeSet::new();
        for (writer, seq) in watermarks {
            s.raise_watermark([*writer; 32], *seq);
        }
        for (writer, seq) in refs {
            s.insert_ref([*writer; 32], *seq);
        }
        s
    }

    /// The arm merges through the join `fauna_core::seen_set` owns: both
    /// sides' members survive, and a watermark on one side elides the other
    /// side's covered refs in the merged bytes — A4's compaction, working
    /// through `apply_class2` itself.
    #[test]
    fn seen_set_merges_by_union_with_watermark_elision() {
        let cur = seen_entry("scope-x", &seen(&[], &[(1, 1), (1, 2), (2, 9)]));
        let inc = seen_entry("scope-x", &seen(&[(1, 5)], &[(3, 4)]));
        let MergeOutcome::Merged(entry) =
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &inc).unwrap()
        else {
            panic!("expected a merge");
        };
        let got: fauna_core::seen_set::SeenScopeSet =
            fauna_core::encoding::canonical_decode(&entry.value).unwrap();
        assert_eq!(got, seen(&[(1, 5)], &[(2, 9), (3, 4)]));
        assert!(
            got.contains(&[1u8; 32], 2),
            "the elided ref is still a member"
        );
    }

    /// The convergence property for the union arm — same requirement as the
    /// user-config pair test: roles reversed, identical bytes.
    #[test]
    fn seen_set_replicas_merge_to_identical_bytes() {
        let a = seen(&[(1, 3)], &[(2, 7)]);
        let b = seen(&[(2, 4)], &[(1, 8)]);
        let bytes = |cur: &fauna_core::seen_set::SeenScopeSet,
                     inc: &fauna_core::seen_set::SeenScopeSet| {
            match apply_class2(
                MergePolicy::CrdtPerField,
                Some(&seen_entry("s", cur)),
                &seen_entry("s", inc),
            )
            .unwrap()
            {
                MergeOutcome::Merged(e) => e.value.to_vec(),
                MergeOutcome::KeepCurrent => seen_entry("s", cur).value.to_vec(),
                MergeOutcome::Replace => seen_entry("s", inc).value.to_vec(),
                other => panic!("expected a merge, got {other:?}"),
            }
        };
        assert_eq!(bytes(&a, &b), bytes(&b, &a));
    }

    /// The echo-stop: a value that already absorbed the incoming one reports
    /// `KeepCurrent` — including when the incoming side is the *itemized*
    /// form of what the current side holds *compacted*, which is exactly the
    /// exchange a compaction would otherwise ping-pong on.
    #[test]
    fn an_absorbed_seen_set_reports_keep_current() {
        let compacted = seen_entry("s", &seen(&[(1, 4)], &[]));
        let itemized = seen_entry("s", &seen(&[], &[(1, 1), (1, 2), (1, 3), (1, 4)]));
        assert_eq!(
            apply_class2(MergePolicy::CrdtPerField, Some(&compacted), &itemized).unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// The adoption law (`delegable-scope-reclamation.md` § Delegable-scope
    /// reclamation, part (1)): a join that IS the served row's value answers
    /// `Replace` — the walk takes the row whole at its own coordinate and
    /// writes none of its own; a join equal to the held value is the
    /// echo-stop; only a join equal to neither side is a new value
    /// (`Merged`), here two sides each holding a reference the other lacks.
    #[test]
    fn a_seen_set_join_is_replace_keep_current_or_merged_by_which_side_it_equals() {
        let older = seen_entry("s", &seen(&[], &[(1, 1)]));
        let newer = seen_entry("s", &seen(&[], &[(1, 1), (2, 2)]));
        let other = seen_entry("s", &seen(&[], &[(3, 3)]));
        let merge = |cur: &EntryPlaintext, inc: &EntryPlaintext| {
            apply_class2(MergePolicy::CrdtPerField, Some(cur), inc).unwrap()
        };
        assert_eq!(merge(&older, &newer), MergeOutcome::Replace);
        assert_eq!(merge(&newer, &older), MergeOutcome::KeepCurrent);
        assert_eq!(merge(&newer, &newer), MergeOutcome::KeepCurrent);
        assert!(matches!(merge(&newer, &other), MergeOutcome::Merged(_)));
        // The compaction case: an itemized side meeting the compacted form
        // that covers it adopts the compacted row as served.
        let compacted = seen_entry("s", &seen(&[(1, 4)], &[]));
        let itemized = seen_entry("s", &seen(&[], &[(1, 1), (1, 2)]));
        assert_eq!(merge(&itemized, &compacted), MergeOutcome::Replace);
    }

    /// Two under-cap seen-sets whose plain union would seal past
    /// the per-entry cap merge, through `apply_class2`, to an entry every
    /// nest accepts — the budget fold inside the join
    /// (`fauna_core::seen_set` § The budget), reaching the arm. Roles
    /// reversed, identical bytes.
    #[test]
    fn two_under_cap_seen_sets_merge_under_the_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::seen_set::SeenScopeSet;

        let generation_sealed = matches!(
            sealing_epoch(KIND_SEEN_SET),
            Some(SealingEpoch::GenerationTip)
        );
        let sealed = |e: &EntryPlaintext| sealed_envelope_len(e, generation_sealed).unwrap();
        // About 49 bytes per ref (a 34-byte byte-string writer id and a
        // small seq): 900 refs seal to about 44 KB a side, under the cap and
        // under one writer's share; the two together would not fit.
        let (writer_a, writer_b) = ([0xA1u8; 32], [0xB2u8; 32]);
        let big = |writer: [u8; 32]| {
            let mut s = SeenScopeSet::new();
            for seq in 1..=900 {
                s.insert_ref(writer, seq);
            }
            s
        };
        let key = format!("content:conv:{}", "ab".repeat(32));
        let cur = seen_entry(&key, &big(writer_a));
        let inc = seen_entry(&key, &big(writer_b));
        assert!(
            sealed(&cur) <= MAX_STATE_ENTRY_BYTES && sealed(&inc) <= MAX_STATE_ENTRY_BYTES,
            "precondition: both sides pass the writer door on their own"
        );
        // What the unbounded union sealed to — the entry the walk journaled
        // and every nest refused, before the fold.
        let plain_union = SeenScopeSet {
            watermarks: Vec::new(),
            refs: big(writer_a)
                .refs
                .iter()
                .chain(&big(writer_b).refs)
                .copied()
                .collect(),
        };
        assert!(
            sealed(&seen_entry(&key, &plain_union)) > MAX_STATE_ENTRY_BYTES,
            "the fixture must outgrow the cap without the fold"
        );

        let MergeOutcome::Merged(entry) =
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &inc).unwrap()
        else {
            panic!("expected a merge");
        };
        let len = sealed(&entry);
        assert!(
            len <= MAX_STATE_ENTRY_BYTES,
            "the merged entry seals to {len} bytes, over the {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
        let got: SeenScopeSet = fauna_core::encoding::canonical_decode(&entry.value).unwrap();
        assert!(got.is_normal_form());
        for seq in 1..=600 {
            assert!(got.contains(&writer_a, seq) && got.contains(&writer_b, seq));
        }
        let MergeOutcome::Merged(reversed) =
            apply_class2(MergePolicy::CrdtPerField, Some(&inc), &cur).unwrap()
        else {
            panic!("expected a merge");
        };
        assert_eq!(
            reversed.value, entry.value,
            "roles reversed, identical bytes"
        );
    }

    /// The budget is sized against the cap, asserted where the cap lives: a
    /// FULL budget of the largest-encoding elements (u64 seqs at their
    /// widest), under a key far longer than any scope string, seals under
    /// `MAX_STATE_ENTRY_BYTES`. Grow the budget or the key space past this
    /// and the writer door is back in play for the seen-set.
    #[test]
    fn a_full_seen_set_budget_seals_under_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::seen_set::{SEEN_SET_ELEMENT_BUDGET, SeenRef, SeenScopeSet, SeenWatermark};

        let wm = u64::MAX - SEEN_SET_ELEMENT_BUDGET as u64;
        let full = SeenScopeSet {
            watermarks: vec![SeenWatermark {
                writer: [0xff; 32],
                seq: wm,
            }],
            refs: (1..SEEN_SET_ELEMENT_BUDGET as u64)
                .map(|i| SeenRef {
                    writer: [0xff; 32],
                    seq: wm + i,
                })
                .collect(),
        };
        assert!(full.is_normal_form(), "the fixture is a legal full budget");
        assert_eq!(full.element_count(), SEEN_SET_ELEMENT_BUDGET);
        let generation_sealed = matches!(
            sealing_epoch(KIND_SEEN_SET),
            Some(SealingEpoch::GenerationTip)
        );
        let entry = seen_entry(&"k".repeat(256), &full);
        let len = sealed_envelope_len(&entry, generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES,
            "a full budget seals to {len} bytes, over the {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    /// Union admits no deletion; the reader-side refusal is the same arm the
    /// user-config kind gets, asserted on this kind so the writer-door
    /// contract (`admits_tombstone`) and the reader agree here too.
    #[test]
    fn a_seen_set_tombstone_is_refused_at_the_reader() {
        let cur = seen_entry("s", &seen(&[(1, 4)], &[]));
        let mut dead = seen_entry("s", &seen(&[], &[]));
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
    }

    /// A seen-set value that will not decode — junk, or a *newer field* from
    /// a build past this one (`deny_unknown_fields` is deliberate on this
    /// type: a tolerant reader would silently strip the newer field from its
    /// re-encoded merge) — fails loudly instead of merging lossily.
    #[test]
    fn a_seen_set_value_that_will_not_decode_fails_loudly() {
        let cur = seen_entry("s", &seen(&[(1, 4)], &[]));
        let mut junk = seen_entry("s", &seen(&[], &[]));
        junk.value = vec![0xff, 0x00].into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&cur), &junk),
            Err(MergeError::BadValue { .. })
        ));
    }

    // ── The read marker (`conversation-read-state.md` § The read-marker record) ──

    fn marker_entry(through: u64) -> EntryPlaintext {
        EntryPlaintext {
            kind: KIND_READ_MARKER.into(),
            key: fauna_core::read_marker::channel_key("ab12"),
            merge_meta: None,
            value: fauna_core::encoding::canonical_encode(
                &fauna_core::read_marker::ReadMarker::new(through),
            )
            .unwrap()
            .into(),
            tombstone: false,
        }
    }

    /// The registration, held whole: the frozen string, its own CRDT join
    /// (never a stamp — a stale value with a fresh stamp must not un-read a
    /// thread), the delegable rung the taxonomy ruling argues, generation 0.
    #[test]
    fn the_read_marker_registers_as_a_delegable_gen0_crdt() {
        assert_eq!(KIND_READ_MARKER, "fauna.state.read-marker");
        assert_eq!(
            merge_policy(KIND_READ_MARKER),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_READ_MARKER),
            Some(AudienceRung::Delegable)
        );
        assert_eq!(sealing_epoch(KIND_READ_MARKER), Some(SealingEpoch::Gen0));
    }

    /// Roles reversed, identical bytes — and they are the higher position.
    #[test]
    fn read_marker_replicas_merge_to_the_higher_position_in_identical_bytes() {
        let bytes = |cur: u64, inc: u64| match apply_class2(
            MergePolicy::CrdtPerField,
            Some(&marker_entry(cur)),
            &marker_entry(inc),
        )
        .unwrap()
        {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => marker_entry(cur).value.to_vec(),
            MergeOutcome::Replace => marker_entry(inc).value.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        };
        assert_eq!(bytes(3, 9), bytes(9, 3));
        assert_eq!(bytes(3, 9), marker_entry(9).value.to_vec());
    }

    /// The echo-stop, and the property the kind exists for: an incoming
    /// marker at or below the held one changes nothing — a marker never
    /// moves backwards.
    #[test]
    fn a_lower_or_equal_read_marker_reports_keep_current() {
        for incoming in [0, 4, 9] {
            assert_eq!(
                apply_class2(
                    MergePolicy::CrdtPerField,
                    Some(&marker_entry(9)),
                    &marker_entry(incoming)
                )
                .unwrap(),
                MergeOutcome::KeepCurrent
            );
        }
    }

    /// The adoption law for the max-register: a higher incoming marker is
    /// the join itself, so it answers `Replace` (the walk adopts the served
    /// row and authors none); at or below the held one, `KeepCurrent`. A
    /// max-register's join always equals one side, so this arm never answers
    /// `Merged`.
    #[test]
    fn a_higher_read_marker_is_adopted_whole() {
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&marker_entry(3)),
                &marker_entry(9)
            )
            .unwrap(),
            MergeOutcome::Replace
        );
    }

    #[test]
    fn a_read_marker_tombstone_is_refused_at_the_reader() {
        let mut dead = marker_entry(0);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&marker_entry(9)), &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
    }

    /// Junk — or a newer field from a build past this one — fails loudly,
    /// at merge and at first contact alike, instead of merging lossily.
    #[test]
    fn a_read_marker_value_that_will_not_decode_fails_loudly() {
        let mut junk = marker_entry(0);
        junk.value = vec![0xff, 0x00].into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&marker_entry(9)), &junk),
            Err(MergeError::BadValue { .. })
        ));
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &junk),
            Err(MergeError::BadValue { .. })
        ));
    }

    fn overlay_entry(overlay: &fauna_core::contact_overlay::ContactOverlay) -> EntryPlaintext {
        crdt_entry(
            KIND_CONTACT_OVERLAY,
            &"ab".repeat(32),
            fauna_core::encoding::canonical_encode(overlay).unwrap(),
        )
    }

    fn nickname_overlay(
        nick: &str,
        at_ms: i64,
        writer: u8,
    ) -> fauna_core::contact_overlay::ContactOverlay {
        use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
        ContactOverlay {
            nickname: Register {
                stamp: Stamp::new(at_ms, [writer; 32]),
                value: Some(nick.into()),
            },
            ..Default::default()
        }
    }

    /// The registration, held whole (`contacts.md` § The private overlay +
    /// `account-data-taxonomy.md` → *The contact-overlay rung*): the frozen
    /// string, per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
    /// departure: the overlay is the user's, never the writing device's.
    #[test]
    fn the_contact_overlay_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_CONTACT_OVERLAY, "fauna.state.contact-overlay");
        assert_eq!(
            merge_policy(KIND_CONTACT_OVERLAY),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_CONTACT_OVERLAY),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_CONTACT_OVERLAY),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_CONTACT_OVERLAY));
        assert!(!delegable_kinds().any(|k| k == KIND_CONTACT_OVERLAY));
    }

    /// Two replicas, a nickname on one and notes on the other: roles
    /// reversed, both merge to identical bytes carrying both fields.
    #[test]
    fn contact_overlay_replicas_merge_per_field_in_identical_bytes() {
        use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
        let a = nickname_overlay("Mum", 10, 1);
        let b = ContactOverlay {
            notes: Register {
                stamp: Stamp::new(5, [2; 32]),
                value: Some("likes tea".into()),
            },
            ..Default::default()
        };
        let merged = |cur: &ContactOverlay, inc: &ContactOverlay| match apply_class2(
            MergePolicy::CrdtPerField,
            Some(&overlay_entry(cur)),
            &overlay_entry(inc),
        )
        .unwrap()
        {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        };
        assert_eq!(merged(&a, &b), merged(&b, &a));
        let joined: ContactOverlay =
            fauna_core::encoding::canonical_decode(&merged(&a, &b)).unwrap();
        assert_eq!(joined.nickname(), Some("Mum"));
        assert_eq!(joined.notes(), Some("likes tea"));
    }

    /// The echo-stop: an older value changes nothing.
    #[test]
    fn an_older_contact_overlay_reports_keep_current() {
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&overlay_entry(&nickname_overlay("New", 20, 1))),
                &overlay_entry(&nickname_overlay("Old", 10, 2)),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// Clearing is a write: a tombstone is refused at merge and at first
    /// contact alike, and so is a value that will not decode.
    #[test]
    fn a_contact_overlay_tombstone_or_junk_is_refused() {
        let mut dead = overlay_entry(&Default::default());
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let mut junk = overlay_entry(&Default::default());
        junk.value = vec![0xff, 0x00].into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &junk),
            Err(MergeError::BadValue { .. })
        ));
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&overlay_entry(&Default::default())),
                &junk
            ),
            Err(MergeError::BadValue { .. })
        ));
    }

    // ── The group-share ceremony record (`p2p.md` § Offline share initiation) ──

    fn initiated_record(
        scope: u8,
        updated: u64,
        offered: u64,
        deliver: &[u8],
        delivered: bool,
    ) -> fauna_core::group_ceremony::InitiatedGroupShare {
        fauna_core::group_ceremony::InitiatedGroupShare {
            scope_id: [scope; 32],
            recipient: fauna_core::identity::ActorId([0xB0; 32]),
            root: vec![0x11; 32].into(),
            offer: vec![0x0F, scope],
            offer_posted: true,
            deliver: deliver.to_vec(),
            delivered,
            offered_at: fauna_core::data::Timestamp(offered),
            updated_at: fauna_core::data::Timestamp(updated),
            ..Default::default()
        }
    }

    fn invited_record(
        scope: u8,
        updated: u64,
        initiator: u8,
        declined: bool,
    ) -> fauna_core::group_ceremony::InvitedGroupShare {
        fauna_core::group_ceremony::InvitedGroupShare {
            scope_id: [scope; 32],
            initiator: fauna_core::identity::ActorId([initiator; 32]),
            offer: vec![0x0A, scope],
            declined,
            updated_at: fauna_core::data::Timestamp(updated),
            ..Default::default()
        }
    }

    fn initiated_entry(r: &fauna_core::group_ceremony::InitiatedGroupShare) -> EntryPlaintext {
        crdt_entry(
            KIND_GROUP_SHARE_CEREMONY,
            &r.plane_key(),
            fauna_core::encoding::canonical_encode(r).unwrap().to_vec(),
        )
    }

    fn invited_entry(r: &fauna_core::group_ceremony::InvitedGroupShare) -> EntryPlaintext {
        crdt_entry(
            KIND_GROUP_SHARE_CEREMONY,
            &r.plane_key(),
            fauna_core::encoding::canonical_encode(r).unwrap().to_vec(),
        )
    }

    fn merged_group_share_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_GROUP_SHARE_CEREMONY, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    fn assert_join_on_bytes(key: &str, samples: &[Vec<u8>]) {
        let m = |a: &[u8], b: &[u8]| merged_group_share_row(key, a, b);
        for a in samples {
            assert_eq!(m(a, a), *a, "idempotent");
            for b in samples {
                assert_eq!(m(a, b), m(b, a), "commutative");
                for c in samples {
                    assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                }
            }
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.group-share-ceremony` row): the frozen string,
    /// per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
    /// departure: the ceremonies are the account's, never the writing
    /// device's.
    #[test]
    fn the_group_share_ceremony_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(
            KIND_GROUP_SHARE_CEREMONY,
            "fauna.state.group-share-ceremony"
        );
        assert_eq!(
            merge_policy(KIND_GROUP_SHARE_CEREMONY),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_GROUP_SHARE_CEREMONY),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_GROUP_SHARE_CEREMONY),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_GROUP_SHARE_CEREMONY));
        assert!(!delegable_kinds().any(|k| k == KIND_GROUP_SHARE_CEREMONY));
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.dns` row): the frozen string and row key,
    /// whole-record LWW, fleet-only, tip-sealed — and an ACCOUNT row on
    /// departure: the credentials serve every admin device.
    #[test]
    fn the_dns_registers_as_a_fleet_only_tip_sealed_latest_wins() {
        assert_eq!(KIND_DNS, "fauna.state.dns");
        assert_eq!(DNS_ROW_KEY, "self");
        assert_eq!(merge_policy(KIND_DNS), Some(MergePolicy::LatestWins));
        assert_eq!(audience_rung(KIND_DNS), Some(AudienceRung::FleetOnly));
        assert_eq!(sealing_epoch(KIND_DNS), Some(SealingEpoch::GenerationTip));
        assert_eq!(TipSealedKind::of(KIND_DNS), Some(TipSealedKind::Dns));
        assert!(!retired_with_its_writer(KIND_DNS));
        assert!(!delegable_kinds().any(|k| k == KIND_DNS));
    }

    /// The overlay's carriage, held whole (`third-party-kinds.md` § The kinds
    /// vocabulary → *The registry overlay*): the frozen string, whole-record
    /// LWW, fleet-only, tip-sealed, an ACCOUNT row on departure — and a
    /// first-party kind, never itself an `ext.*` string or overlay-admittable.
    #[test]
    fn the_kind_manifest_registers_as_a_fleet_only_tip_sealed_latest_wins() {
        assert_eq!(KIND_KIND_MANIFEST, "fauna.state.kind-manifest");
        assert_eq!(
            merge_policy(KIND_KIND_MANIFEST),
            Some(MergePolicy::LatestWins)
        );
        assert_eq!(
            audience_rung(KIND_KIND_MANIFEST),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_KIND_MANIFEST),
            Some(SealingEpoch::GenerationTip)
        );
        assert_eq!(
            TipSealedKind::of(KIND_KIND_MANIFEST).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
        assert!(!retired_with_its_writer(KIND_KIND_MANIFEST));
        assert!(!delegable_kinds().any(|k| k == KIND_KIND_MANIFEST));
        assert_eq!(
            home_scope_for_kind(KIND_KIND_MANIFEST).as_deref(),
            Some(crate::account_state::ACCOUNT_STATE_FLEET_SCOPE)
        );
    }

    /// The tolerant posture, pinned: the LWW path adopts a newer writer's
    /// bytes VERBATIM — a field this build's `DnsConfig` does not know
    /// survives the adoption (and so an older replica's re-seal) — while
    /// `DnsConfig` itself still decodes those bytes, dropping only the field
    /// it does not know. No arm decodes the value, so the pin is on both
    /// halves: the adoption and the type.
    #[test]
    fn a_newer_dns_field_is_adopted_verbatim_and_decodes_tolerantly() {
        use fauna_core::data::DnsConfig;
        let mut known = DnsConfig::default();
        known.managed_domains.insert("example.com".into());
        let known_bytes = fauna_core::encoding::canonical_encode(&known).unwrap();
        let mut map = match fauna_cbor::decode_strict::<fauna_cbor::Value>(&known_bytes).unwrap() {
            fauna_cbor::Value::Map(m) => m,
            other => panic!("DnsConfig encodes as a map, got {other:?}"),
        };
        map.insert(
            "a_field_from_a_newer_writer".into(),
            fauna_cbor::Value::Integer(7),
        );
        let newer = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map)).unwrap();

        let cur = stamped(KIND_DNS, DNS_ROW_KEY, &known_bytes, 1, 1);
        let inc = stamped(KIND_DNS, DNS_ROW_KEY, &newer, 2, 2);
        validate_adoptable(MergePolicy::LatestWins, &inc).unwrap();
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&cur), &inc).unwrap(),
            MergeOutcome::Replace
        ));

        let decoded: DnsConfig = fauna_core::encoding::canonical_decode(&newer).unwrap();
        assert_eq!(
            decoded, known,
            "the unknown field is dropped, the rest kept"
        );
    }

    /// A DNS record populated well past any real deployment: several provider
    /// credentials each covering several zones, many managed domains, the
    /// ACME account, delegations, opt-outs, all three publish memories and a
    /// manual issuance in flight.
    fn a_fully_populated_dns_record() -> fauna_core::data::DnsConfig {
        use fauna_core::data::{
            CnameDelegation, DnsConfig, DnsProviderCredential, DnsZoneRef, PendingChallengeRecord,
            PendingManualIssue, Timestamp,
        };
        let domains: Vec<String> = (0..16)
            .map(|i| format!("domain-{i:02}.example.org"))
            .collect();
        let names = |slot: &str| {
            domains
                .iter()
                .map(|d| {
                    let set = (0..3).map(|s| format!("{slot}{s}.{d}")).collect();
                    (d.clone(), set)
                })
                .collect()
        };
        DnsConfig {
            credentials: (0..6)
                .map(|p| DnsProviderCredential {
                    provider_id: format!("provider-{p}"),
                    fields: (0..3)
                        .map(|f| (format!("api-field-{f}"), "t".repeat(128).into()))
                        .collect(),
                    zones: domains
                        .iter()
                        .map(|d| DnsZoneRef {
                            id: "z".repeat(40),
                            name: d.clone(),
                        })
                        .collect(),
                    label: format!("Provider {p} (every domain)"),
                    created_at: u64::MAX,
                })
                .collect(),
            managed_domains: domains.iter().cloned().collect(),
            // A serialized ACME `AccountCredentials` is well under 1 KiB.
            acme_account: Some(vec![0xa5; 2048]),
            delegations: domains
                .iter()
                .map(|d| CnameDelegation {
                    domain: d.clone(),
                    target_name: format!("_acme-challenge.{d}.delegated-zone.example.net"),
                    target_zone: "delegated-zone.example.net".into(),
                })
                .collect(),
            auto_renew_off: domains.iter().cloned().collect(),
            dkim_published_names: names("sel-"),
            atproto_published_names: names("_atproto.handle-"),
            fauna_self_published_names: names("_fauna-"),
            pending_manual_issue: Some(PendingManualIssue {
                domain: domains[0].clone(),
                target_nest_id: vec![0xff; 32],
                challenges: (0..4)
                    .map(|_| PendingChallengeRecord {
                        name: format!("_acme-challenge.{}", domains[0]),
                        record_type: "TXT".into(),
                        value: "v".repeat(43),
                        ttl_seconds: u32::MAX,
                    })
                    .collect(),
                started_at: Timestamp(u64::MAX),
            }),
        }
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** the one row at `self` is bounded by the record's own shape,
    /// and a record populated well past any real deployment seals under HALF
    /// the per-entry cap — a join test never sees the cap; only the door and
    /// this pin do.
    #[test]
    fn a_fully_populated_dns_record_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        let value =
            fauna_core::encoding::canonical_encode(&a_fully_populated_dns_record()).unwrap();
        let entry = stamped(KIND_DNS, DNS_ROW_KEY, &value, i64::MAX, 0xff);
        let generation_sealed =
            matches!(sealing_epoch(KIND_DNS), Some(SealingEpoch::GenerationTip));
        let len = sealed_envelope_len(&entry, generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES / 2,
            "a fully populated DNS record seals to {len} bytes, over half the \
             {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the ACME account
    /// and the pending issuance's nest id ride as CBOR byte strings (major 2),
    /// never serde's default integer array.
    #[test]
    fn the_dns_records_bytes_fields_are_cbor_byte_strings() {
        use fauna_core::data::{DnsConfig, PendingManualIssue, Timestamp};
        let record = DnsConfig {
            acme_account: Some(vec![0xa5; 4]),
            pending_manual_issue: Some(PendingManualIssue {
                domain: String::new(),
                target_nest_id: vec![0x5a; 4],
                challenges: Vec::new(),
                started_at: Timestamp(0),
            }),
            ..Default::default()
        };
        let bytes = fauna_core::encoding::canonical_encode(&record).unwrap();
        for (field, byte) in [("acme_account", 0xa5), ("target_nest_id", 0x5a)] {
            assert!(
                bytes
                    .windows(5)
                    .any(|w| w == [0x44, byte, byte, byte, byte]),
                "{field} must encode as a 4-byte CBOR byte string"
            );
        }
        let back: DnsConfig = fauna_core::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, record);
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.atproto` row): the frozen string, LWW per
    /// credential row (a revoke is a tombstone), fleet-only, tip-sealed — and
    /// an ACCOUNT row on departure: each credential serves the account's PDS.
    #[test]
    fn the_atproto_registers_as_a_fleet_only_tip_sealed_latest_wins() {
        assert_eq!(KIND_ATPROTO, "fauna.state.atproto");
        assert_eq!(merge_policy(KIND_ATPROTO), Some(MergePolicy::LatestWins));
        assert!(MergePolicy::LatestWins.admits_tombstone(), "a revoke");
        assert_eq!(audience_rung(KIND_ATPROTO), Some(AudienceRung::FleetOnly));
        assert_eq!(
            sealing_epoch(KIND_ATPROTO),
            Some(SealingEpoch::GenerationTip)
        );
        assert_eq!(
            TipSealedKind::of(KIND_ATPROTO),
            Some(TipSealedKind::Atproto)
        );
        assert_eq!(TipSealedKind::Atproto.scope(), TipRowScope::Account);
        assert!(!retired_with_its_writer(KIND_ATPROTO));
        assert!(!delegable_kinds().any(|k| k == KIND_ATPROTO));
    }

    fn an_app_credential(id: &str, label: &str) -> fauna_core::data::AtprotoAppCredential {
        fauna_core::data::AtprotoAppCredential {
            credential_id: id.into(),
            label: label.into(),
            secret: fauna_core::secret::SecretByteBuf::new(b"abcd-efgh-ijkl-mnop".to_vec()),
            dm_allowed: true,
            created_at: u64::MAX,
        }
    }

    /// The tolerant posture, pinned on both halves (the DNS record's shape):
    /// the LWW path adopts a newer writer's credential row VERBATIM, and
    /// `AtprotoAppCredential` still decodes it, dropping only the field it
    /// does not know. A later stamped revoke (a tombstone) wins the same way.
    #[test]
    fn a_newer_atproto_credential_field_is_adopted_verbatim_and_decodes_tolerantly() {
        use fauna_core::data::AtprotoAppCredential;
        let known = an_app_credential("ivory", "Ivory");
        let known_bytes = fauna_core::encoding::canonical_encode(&known).unwrap();
        let mut map = match fauna_cbor::decode_strict::<fauna_cbor::Value>(&known_bytes).unwrap() {
            fauna_cbor::Value::Map(m) => m,
            other => panic!("AtprotoAppCredential encodes as a map, got {other:?}"),
        };
        map.insert(
            "a_field_from_a_newer_writer".into(),
            fauna_cbor::Value::Integer(7),
        );
        let newer = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map)).unwrap();

        let cur = stamped(KIND_ATPROTO, "ivory", &known_bytes, 1, 1);
        let inc = stamped(KIND_ATPROTO, "ivory", &newer, 2, 2);
        validate_adoptable(MergePolicy::LatestWins, &inc).unwrap();
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&cur), &inc).unwrap(),
            MergeOutcome::Replace
        ));
        let decoded: AtprotoAppCredential = fauna_core::encoding::canonical_decode(&newer).unwrap();
        assert_eq!(
            decoded, known,
            "the unknown field is dropped, the rest kept"
        );

        let mut revoke = stamped(KIND_ATPROTO, "ivory", &[], 3, 1);
        revoke.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&inc), &revoke).unwrap(),
            MergeOutcome::Replace
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per credential is bounded by the credential's own
    /// shape — a credential whose label and id run to 4 KiB each, far past
    /// the nest's 256-byte label cap, seals under HALF the per-entry cap.
    #[test]
    fn an_oversized_app_credential_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        let id = "i".repeat(4096);
        let value =
            fauna_core::encoding::canonical_encode(&an_app_credential(&id, &"l".repeat(4096)))
                .unwrap();
        let entry = stamped(KIND_ATPROTO, &id, &value, i64::MAX, 0xff);
        let generation_sealed = matches!(
            sealing_epoch(KIND_ATPROTO),
            Some(SealingEpoch::GenerationTip)
        );
        let len = sealed_envelope_len(&entry, generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES / 2,
            "an oversized app credential seals to {len} bytes, over half the \
             {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.follows` row): the frozen string, LWW per
    /// followed-folder row (an unfollow is a tombstone), fleet-only,
    /// tip-sealed — and an ACCOUNT row on departure: a follow is the user's,
    /// whichever device made it.
    #[test]
    fn the_follows_registers_as_a_fleet_only_tip_sealed_latest_wins() {
        assert_eq!(KIND_FOLLOWS, "fauna.state.follows");
        assert_eq!(merge_policy(KIND_FOLLOWS), Some(MergePolicy::LatestWins));
        assert!(MergePolicy::LatestWins.admits_tombstone(), "an unfollow");
        assert_eq!(audience_rung(KIND_FOLLOWS), Some(AudienceRung::FleetOnly));
        assert_eq!(
            sealing_epoch(KIND_FOLLOWS),
            Some(SealingEpoch::GenerationTip)
        );
        assert_eq!(
            TipSealedKind::of(KIND_FOLLOWS),
            Some(TipSealedKind::Follows)
        );
        assert_eq!(TipSealedKind::Follows.scope(), TipRowScope::Account);
        assert!(!retired_with_its_writer(KIND_FOLLOWS));
        assert!(!delegable_kinds().any(|k| k == KIND_FOLLOWS));
    }

    fn a_followed_folder(home: &str, name: &str) -> fauna_core::data::FollowedFolder {
        fauna_core::data::FollowedFolder {
            home_nest_url: home.into(),
            home_nest_actor_id: Some("ab".repeat(32)),
            owner_actor_id: "cd".repeat(32),
            owner_handle: Some("alice".into()),
            folder_id: i64::MAX,
            display_name: name.into(),
        }
    }

    /// The tolerant posture, pinned on both halves (the DNS record's shape):
    /// the LWW path adopts a newer writer's follow row VERBATIM, and
    /// `FollowedFolder` still decodes it, dropping only the field it does not
    /// know. A later stamped unfollow (a tombstone) wins the same way.
    #[test]
    fn a_newer_follow_field_is_adopted_verbatim_and_decodes_tolerantly() {
        use fauna_core::data::FollowedFolder;
        let known = a_followed_folder("https://peer.example", "site");
        let key = known.plane_key();
        let known_bytes = fauna_core::encoding::canonical_encode(&known).unwrap();
        let mut map = match fauna_cbor::decode_strict::<fauna_cbor::Value>(&known_bytes).unwrap() {
            fauna_cbor::Value::Map(m) => m,
            other => panic!("FollowedFolder encodes as a map, got {other:?}"),
        };
        map.insert(
            "a_field_from_a_newer_writer".into(),
            fauna_cbor::Value::Integer(7),
        );
        let newer = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map)).unwrap();

        let cur = stamped(KIND_FOLLOWS, &key, &known_bytes, 1, 1);
        let inc = stamped(KIND_FOLLOWS, &key, &newer, 2, 2);
        validate_adoptable(MergePolicy::LatestWins, &inc).unwrap();
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&cur), &inc).unwrap(),
            MergeOutcome::Replace
        ));
        let decoded: FollowedFolder = fauna_core::encoding::canonical_decode(&newer).unwrap();
        assert_eq!(
            decoded, known,
            "the unknown field is dropped, the rest kept"
        );

        let mut unfollow = stamped(KIND_FOLLOWS, &key, &[], 3, 1);
        unfollow.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::LatestWins, Some(&inc), &unfollow).unwrap(),
            MergeOutcome::Replace
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per followed folder is bounded by the record's own
    /// shape — a follow whose home URL, handle and display name run to 4 KiB
    /// each seals under HALF the per-entry cap, and its key stays a fixed
    /// 64-hex digest plus the id however long the URL runs.
    #[test]
    fn an_oversized_follow_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        let mut follow = a_followed_folder(&"u".repeat(4096), &"n".repeat(4096));
        follow.owner_handle = Some("h".repeat(4096));
        let key = follow.plane_key();
        assert!(key.len() <= 64 + 1 + 20, "the key is bounded: {key}");
        let value = fauna_core::encoding::canonical_encode(&follow).unwrap();
        let entry = stamped(KIND_FOLLOWS, &key, &value, i64::MAX, 0xff);
        let generation_sealed = matches!(
            sealing_epoch(KIND_FOLLOWS),
            Some(SealingEpoch::GenerationTip)
        );
        let len = sealed_envelope_len(&entry, generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES / 2,
            "an oversized follow seals to {len} bytes, over half the \
             {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the secret rides
    /// as a CBOR byte string (major 2), never serde's default integer array.
    #[test]
    fn the_app_credential_secret_is_a_cbor_byte_string() {
        let bytes =
            fauna_core::encoding::canonical_encode(&an_app_credential("ivory", "Ivory")).unwrap();
        // Major 2, length 19, then the secret itself.
        let mut want = vec![0x53];
        want.extend_from_slice(b"abcd-efgh-ijkl-mnop");
        assert!(
            bytes.windows(want.len()).any(|w| w == want.as_slice()),
            "the secret must encode as a 19-byte CBOR byte string"
        );
    }

    fn a_rotation_key(
        seed: u8,
        created: u64,
        dids: &[&str],
    ) -> fauna_core::data::AtprotoRotationKey {
        fauna_core::data::AtprotoRotationKey {
            secret_scalar: fauna_core::secret::SecretArray32::new([seed; 32]),
            pubkey_did_key: format!("did:key:zDnaeK{seed:02x}"),
            created_at: created,
            published_for_dids: dids.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    fn a_contest_intent(at: u64) -> fauna_core::data::AtprotoContestIntent {
        fauna_core::data::AtprotoContestIntent {
            did: "did:plc:ewvi7nxzyoun6zhxrhs64oiz".into(),
            contested_op_cid: "bafyreib2rxk3rh6kzwq".into(),
            requested_at: at,
        }
    }

    fn identity_entry(
        r: &fauna_core::atproto_identity_rows::AtprotoIdentityRecord,
    ) -> EntryPlaintext {
        crdt_entry(
            KIND_ATPROTO_IDENTITY,
            &r.plane_key().unwrap(),
            r.encode().unwrap(),
        )
    }

    fn merged_identity_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_ATPROTO_IDENTITY, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.atproto-identity` row): the frozen string,
    /// per-field CRDT (no tombstone: the union never removes), fleet-only,
    /// tip-sealed — and an ACCOUNT row on departure: every rotation key is
    /// the senior key of an account DID, whichever device minted it.
    #[test]
    fn the_atproto_identity_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_ATPROTO_IDENTITY, "fauna.state.atproto-identity");
        assert_eq!(
            merge_policy(KIND_ATPROTO_IDENTITY),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_ATPROTO_IDENTITY),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_ATPROTO_IDENTITY),
            Some(SealingEpoch::GenerationTip)
        );
        assert_eq!(
            TipSealedKind::of(KIND_ATPROTO_IDENTITY),
            Some(TipSealedKind::AtprotoIdentity)
        );
        assert_eq!(TipSealedKind::AtprotoIdentity.scope(), TipRowScope::Account);
        assert!(!retired_with_its_writer(KIND_ATPROTO_IDENTITY));
        assert!(!delegable_kinds().any(|k| k == KIND_ATPROTO_IDENTITY));
    }

    /// The join laws, ON BYTES through the plane arm, per element type:
    /// commutative, associative, idempotent — including the two diagnostic
    /// instants (`created_at`, `requested_at`), where a keep-mine fold would
    /// leave two replicas holding different bytes for ever.
    #[test]
    fn atproto_identity_merge_is_a_join_on_bytes() {
        use fauna_core::atproto_identity_rows::AtprotoIdentityRecord as R;
        let law = |samples: Vec<R>| {
            let key = samples[0].plane_key().unwrap();
            let bytes: Vec<Vec<u8>> = samples.iter().map(|r| r.encode().unwrap()).collect();
            let m = |a: &[u8], b: &[u8]| merged_identity_row(&key, a, b);
            for a in &bytes {
                assert_eq!(m(a, a), *a, "idempotent");
                for b in &bytes {
                    assert_eq!(m(a, b), m(b, a), "commutative");
                    for c in &bytes {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                    }
                }
            }
        };
        law(vec![
            R::RotationKey(a_rotation_key(1, 10, &[])),
            R::RotationKey(a_rotation_key(1, 5, &["did:plc:bbb"])),
            R::RotationKey(a_rotation_key(1, 20, &["did:plc:aaa", "did:plc:ccc"])),
            R::RotationKey(a_rotation_key(1, 10, &["did:plc:aaa"])),
        ]);
        law(vec![
            R::ContestIntent(a_contest_intent(30)),
            R::ContestIntent(a_contest_intent(10)),
            R::ContestIntent(a_contest_intent(20)),
        ]);
        law(vec![R::TombstoneConsent("did:plc:aaa".into())]);
        law(vec![R::NestNamedDid("did:plc:aaa".into())]);
        // And it is the union the rule promises: bindings union, the earlier
        // instant wins.
        let enc = |k: fauna_core::data::AtprotoRotationKey| R::RotationKey(k).encode().unwrap();
        let key = R::RotationKey(a_rotation_key(1, 0, &[]))
            .plane_key()
            .unwrap();
        let joined: fauna_core::data::AtprotoRotationKey =
            fauna_core::encoding::canonical_decode(&merged_identity_row(
                &key,
                &enc(a_rotation_key(1, 20, &["did:plc:ccc"])),
                &enc(a_rotation_key(1, 5, &["did:plc:aaa"])),
            ))
            .unwrap();
        assert_eq!(joined.published_for_dids, ["did:plc:aaa", "did:plc:ccc"]);
        assert_eq!(joined.created_at, 5);
    }

    /// The arm is the composite merge's per-element half, not a second rule:
    /// merging two composite records and splitting the result into rows gives
    /// exactly the rows the arm produces from the two sides' rows.
    #[test]
    fn the_atproto_identity_arm_agrees_with_the_composite_fold() {
        use fauna_core::data::AtprotoIdentityConfig;
        let a = AtprotoIdentityConfig {
            rotation_keys: vec![
                a_rotation_key(1, 10, &["did:plc:aaa"]),
                a_rotation_key(2, 30, &[]),
            ],
            tombstone_consents: vec!["did:plc:aaa".into()],
            contest_intents: vec![a_contest_intent(40)],
            nest_named_dids: vec!["did:plc:aaa".into()],
        };
        let b = AtprotoIdentityConfig {
            rotation_keys: vec![
                a_rotation_key(1, 8, &["did:plc:bbb"]),
                a_rotation_key(3, 20, &[]),
            ],
            tombstone_consents: vec!["did:plc:bbb".into()],
            contest_intents: vec![a_contest_intent(35)],
            nest_named_dids: vec!["did:plc:aaa".into(), "did:plc:bbb".into()],
        };
        let rows = |c: &AtprotoIdentityConfig| -> Vec<(String, Vec<u8>)> {
            c.rows()
                .unwrap()
                .into_iter()
                .map(|(k, r)| (k, r.encode().unwrap()))
                .collect()
        };
        let mut composite = rows(&a.merge(&b));
        composite.sort();
        let mut by_arm: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
        for (k, v) in rows(&a).into_iter().chain(rows(&b)) {
            let next = match by_arm.get(&k) {
                Some(cur) => merged_identity_row(&k, cur, &v),
                None => v,
            };
            by_arm.insert(k, next);
        }
        assert_eq!(by_arm.into_iter().collect::<Vec<_>>(), composite);
        assert_eq!(composite.len(), 3 + 2 + 1 + 2);
    }

    /// The echo-stop: a value the current one already covers changes nothing.
    #[test]
    fn a_covered_atproto_identity_row_reports_keep_current() {
        use fauna_core::atproto_identity_rows::AtprotoIdentityRecord as R;
        let current = R::RotationKey(a_rotation_key(1, 5, &["did:plc:aaa", "did:plc:bbb"]));
        let older = R::RotationKey(a_rotation_key(1, 10, &["did:plc:aaa"]));
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&identity_entry(&current)),
                &identity_entry(&older),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// No deletion exists: a tombstone is refused at merge and at first
    /// contact alike, and so is a value that will not decode, a key outside
    /// the grammar, a value filed under another element's key, and — the
    /// one refusal the composite merge cannot make — one pubkey carrying two
    /// scalars.
    #[test]
    fn an_atproto_identity_tombstone_junk_misfiled_or_forged_row_is_refused() {
        use fauna_core::atproto_identity_rows::AtprotoIdentityRecord as R;
        let rec = R::RotationKey(a_rotation_key(1, 10, &[]));
        let mut dead = identity_entry(&rec);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let bad = |e: EntryPlaintext| {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &e),
                    Err(MergeError::BadValue { .. })
                ),
                "{} must be refused",
                e.key
            );
        };
        let mut junk = identity_entry(&rec);
        junk.value = vec![0xff, 0x00].into();
        bad(junk);
        let mut self_key = identity_entry(&rec);
        self_key.key = "self".into();
        bad(self_key);
        let mut misfiled = identity_entry(&rec);
        misfiled.key = R::RotationKey(a_rotation_key(2, 10, &[]))
            .plane_key()
            .unwrap();
        bad(misfiled);
        let mut wrong_type = identity_entry(&R::NestNamedDid("did:plc:aaa".into()));
        wrong_type.key = "consent/did:plc:bbb".into();
        bad(wrong_type);

        let mut forged = a_rotation_key(1, 10, &[]);
        forged.secret_scalar = fauna_core::secret::SecretArray32::new([0xee; 32]);
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&identity_entry(&rec)),
                &identity_entry(&R::RotationKey(forged)),
            ),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// The P4 decode posture — an unknown field is refused, on the two
    /// struct-valued element types: a row carrying a field from a build past
    /// this one answers `BadValue` (it re-presents on the next reconcile)
    /// instead of merging with the field silently stripped. The value types
    /// stay tolerant; the row decode demands the value
    /// re-encode to its own bytes.
    #[test]
    fn a_newer_atproto_identity_field_is_refused_not_stripped() {
        use fauna_core::atproto_identity_rows::AtprotoIdentityRecord as R;
        for rec in [
            R::RotationKey(a_rotation_key(1, 10, &[])),
            R::ContestIntent(a_contest_intent(10)),
        ] {
            let current = identity_entry(&rec);
            let mut map =
                match fauna_cbor::decode_strict::<fauna_cbor::Value>(&current.value).unwrap() {
                    fauna_cbor::Value::Map(m) => m,
                    other => panic!("an element encodes as a map, got {other:?}"),
                };
            map.insert("from_the_future".into(), fauna_cbor::Value::Integer(1));
            let mut newer = current.clone();
            newer.value = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map))
                .unwrap()
                .into();
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, None, &newer),
                Err(MergeError::BadValue { .. })
            ));
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, Some(&current), &newer),
                Err(MergeError::BadValue { .. })
            ));
        }
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per element is bounded by the element's own shape.
    /// A rotation key is published senior for one DID (fresh-key-per-mint
    /// burns it thereafter); a key bound to 64 DIDs, and a contest intent
    /// whose DID and CID run to 4 KiB each, still seal under HALF the
    /// per-entry cap.
    #[test]
    fn an_oversized_atproto_identity_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::atproto_identity_rows::AtprotoIdentityRecord as R;
        let dids: Vec<String> = (0..64).map(|i| format!("did:plc:{i:0>24}")).collect();
        let dids: Vec<&str> = dids.iter().map(String::as_str).collect();
        let long = "x".repeat(4096);
        let generation_sealed = matches!(
            sealing_epoch(KIND_ATPROTO_IDENTITY),
            Some(SealingEpoch::GenerationTip)
        );
        for rec in [
            R::RotationKey(a_rotation_key(1, u64::MAX, &dids)),
            R::ContestIntent(fauna_core::data::AtprotoContestIntent {
                did: long.clone(),
                contested_op_cid: long.clone(),
                requested_at: u64::MAX,
            }),
            R::TombstoneConsent(long.clone()),
        ] {
            let key = rec.plane_key().unwrap();
            let entry = stamped(
                KIND_ATPROTO_IDENTITY,
                &key,
                &rec.encode().unwrap(),
                i64::MAX,
                0xff,
            );
            let len = sealed_envelope_len(&entry, generation_sealed).unwrap();
            assert!(
                len <= MAX_STATE_ENTRY_BYTES / 2,
                "{} seals to {len} bytes, over half the {MAX_STATE_ENTRY_BYTES}-byte cap",
                &key[..key.len().min(16)]
            );
        }
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the rotation
    /// key's scalar rides as a 32-byte CBOR byte string (major 2), never
    /// serde's default integer array.
    #[test]
    fn the_rotation_key_scalar_is_a_cbor_byte_string() {
        let bytes = fauna_core::encoding::canonical_encode(&a_rotation_key(0xab, 1, &[])).unwrap();
        let mut want = vec![0x58, 0x20];
        want.extend_from_slice(&[0xab; 32]);
        assert!(
            bytes.windows(want.len()).any(|w| w == want.as_slice()),
            "the scalar must encode as a 32-byte CBOR byte string"
        );
    }

    // ── `fauna.state.mail` (the mail custody) ──

    fn mail_key(seed: u8) -> fauna_core::secret::SecretArray32 {
        fauna_core::secret::SecretArray32::new([seed; 32])
    }

    fn mail_state(
        msek: Option<u8>,
        at: u64,
        f: impl FnOnce(&mut fauna_core::mail_rows::MailStateRow),
    ) -> fauna_core::mail_rows::MailStateRow {
        let mut s = fauna_core::mail_rows::MailStateRow {
            msek: msek.map(mail_key),
            mail_enabled: Some(true),
            updated_at: fauna_core::data::Timestamp(at),
            ..Default::default()
        };
        f(&mut s);
        s
    }

    fn mail_credential(
        at: u64,
        f: impl FnOnce(&mut fauna_core::data::MailCredential),
    ) -> fauna_core::data::MailCredential {
        let mut c = fauna_core::data::MailCredential {
            credential_id: "iphone-mail".into(),
            display_name: "iPhone Mail".into(),
            kind: fauna_core::data::MailCredentialKind::Plain,
            secret: b"correct horse battery".to_vec().into(),
            created_at: 1_800_000_000,
            updated_at: fauna_core::data::Timestamp(at),
            wrapped_under: None,
            revoked_at_unix: None,
            burned: None,
        };
        f(&mut c);
        c
    }

    fn a_mail_burn(p: u8, at: u64) -> fauna_core::data::MailSuccessionBurn {
        fauna_core::data::MailSuccessionBurn {
            predecessor: fauna_core::identity::ActorId([p; 32]),
            at_unix: at,
        }
    }

    fn mail_entry(r: &fauna_core::mail_rows::MailRecord) -> EntryPlaintext {
        crdt_entry(KIND_MAIL, &r.plane_key().unwrap(), r.encode().unwrap())
    }

    fn merged_mail_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_MAIL, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    fn mail_join_laws(samples: &[fauna_core::mail_rows::MailRecord]) {
        let key = samples[0].plane_key().unwrap();
        let bytes: Vec<Vec<u8>> = samples.iter().map(|r| r.encode().unwrap()).collect();
        let m = |a: &[u8], b: &[u8]| merged_mail_row(&key, a, b);
        for (i, a) in bytes.iter().enumerate() {
            assert_eq!(m(a, a), *a, "idempotent: sample {i}");
            for (j, b) in bytes.iter().enumerate() {
                assert_eq!(m(a, b), m(b, a), "commutative: samples {i}, {j}");
                for (k, c) in bytes.iter().enumerate() {
                    assert_eq!(
                        m(&m(a, b), c),
                        m(a, &m(b, c)),
                        "associative: samples {i}, {j}, {k}"
                    );
                }
            }
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.mail` row): the frozen string, per-field CRDT (no
    /// tombstone: a revoke is a marker), fleet-only, tip-sealed — and an
    /// ACCOUNT row on departure: one mailbox, whichever device wrote a row.
    #[test]
    fn the_mail_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_MAIL, "fauna.state.mail");
        assert_eq!(merge_policy(KIND_MAIL), Some(MergePolicy::CrdtPerField));
        assert_eq!(audience_rung(KIND_MAIL), Some(AudienceRung::FleetOnly));
        assert_eq!(sealing_epoch(KIND_MAIL), Some(SealingEpoch::GenerationTip));
        assert_eq!(TipSealedKind::of(KIND_MAIL), Some(TipSealedKind::Mail));
        assert_eq!(TipSealedKind::Mail.scope(), TipRowScope::Account);
        assert!(!retired_with_its_writer(KIND_MAIL));
        assert!(!delegable_kinds().any(|k| k == KIND_MAIL));
    }

    /// The join laws, ON BYTES through the plane arm, per row family:
    /// commutative, associative, idempotent. The state row's samples cover a
    /// rotation (a displaced MSEK with its retirement), two concurrent
    /// rotations tied on the stamp, an in-flight sentinel, a fresh device
    /// with no MSEK, the flags and the burns; the credential row's cover a
    /// re-wrap, a rename, a burn, a revoke at two instants, a re-minted secret
    /// under one id and a stamp tie.
    #[test]
    fn mail_merge_is_a_join_on_bytes() {
        use fauna_core::data::{MsekFingerprint, PriorMsekRetirement};
        use fauna_core::mail_rows::{MailRecord as R, MailRotationSentinel};
        let retired = |seed: u8, at: u64| PriorMsekRetirement {
            msek: mail_key(seed),
            retired_at_unix: at,
        };
        mail_join_laws(&[
            R::State(mail_state(None, 5, |s| s.mail_enabled = None)),
            R::State(mail_state(Some(1), 10, |_| {})),
            R::State(mail_state(Some(2), 20, |s| {
                s.prior_mseks = vec![mail_key(1)];
                s.prior_msek_retirements = vec![retired(1, 100)];
            })),
            R::State(mail_state(Some(3), 20, |s| {
                s.prior_mseks = vec![mail_key(1)];
                s.prior_msek_retirements = vec![retired(1, 90)];
                s.caldav_enabled = true;
            })),
            R::State(mail_state(Some(1), 15, |s| {
                s.pending_rotation = Some(MailRotationSentinel {
                    new_msek: mail_key(2),
                });
                s.succession_burns = vec![a_mail_burn(7, 50)];
            })),
            R::State(mail_state(Some(1), 12, |s| {
                s.mail_enabled = Some(false);
                s.carddav_enabled = true;
                s.succession_burns = vec![a_mail_burn(7, 40), a_mail_burn(8, 60)];
            })),
        ]);
        let fp = Some(MsekFingerprint::of(&mail_key(2)));
        mail_join_laws(&[
            R::Credential(mail_credential(10, |_| {})),
            R::Credential(mail_credential(20, |c| c.wrapped_under = fp)),
            R::Credential(mail_credential(20, |c| c.display_name = "Phone".into())),
            R::Credential(mail_credential(15, |c| {
                c.secret = b"a re-mint on another device".to_vec().into();
            })),
            R::Credential(mail_credential(30, |c| {
                c.revoked_at_unix = Some(1_800_000_300);
                c.secret = Default::default();
            })),
            R::Credential(mail_credential(12, |c| {
                c.revoked_at_unix = Some(1_800_000_200);
                c.secret = Default::default();
            })),
            R::Credential(mail_credential(11, |c| {
                c.burned = Some(a_mail_burn(7, 1_800_000_100));
                c.secret = Default::default();
            })),
        ]);
    }

    /// The state row's rule, pinned by value: a rotation's newer stamp takes
    /// the MSEK and the recreatable four together, `mail_enabled` stays
    /// present-wins beside the MSEK, the burns min-union, and the joined
    /// stamp is the later one.
    #[test]
    fn the_mail_state_row_joins_the_msek_halves_and_the_recreatable_four() {
        use fauna_core::mail_rows::{MailRecord as R, MailRotationSentinel, MailStateRow};
        let rotated = mail_state(Some(2), 20, |s| {
            s.prior_mseks = vec![mail_key(1)];
            s.prior_msek_retirements = vec![fauna_core::data::PriorMsekRetirement {
                msek: mail_key(1),
                retired_at_unix: 100,
            }];
            s.mail_enabled = None;
            s.succession_burns = vec![a_mail_burn(7, 50)];
        });
        let older = mail_state(Some(1), 10, |s| {
            s.caldav_enabled = true;
            s.pending_rotation = Some(MailRotationSentinel {
                new_msek: mail_key(2),
            });
            s.succession_burns = vec![a_mail_burn(7, 40)];
        });
        let joined: MailStateRow = fauna_core::encoding::canonical_decode(&merged_mail_row(
            "self",
            &R::State(older).encode().unwrap(),
            &R::State(rotated).encode().unwrap(),
        ))
        .unwrap();
        assert_eq!(joined.msek, Some(mail_key(2)));
        assert_eq!(joined.prior_mseks, vec![mail_key(1)]);
        assert_eq!(joined.pending_rotation, None, "the newer four win whole");
        assert!(!joined.caldav_enabled);
        assert_eq!(
            joined.mail_enabled,
            Some(true),
            "present-wins beside the MSEK"
        );
        assert_eq!(joined.succession_burns, vec![a_mail_burn(7, 40)]);
        assert_eq!(joined.updated_at, fauna_core::data::Timestamp(20));
    }

    /// The monotone markers: a revoke and a burn each survive a concurrent
    /// re-wrap stamped later, and the joined row carries no secret — while
    /// the re-wrap's remainder (the generation it names) still lands, which
    /// is exactly what makes the marked row owed a delete.
    #[test]
    fn a_mail_credential_marker_survives_a_later_rewrap() {
        use fauna_core::data::{MailCredential, MsekFingerprint};
        use fauna_core::mail_rows::MailRecord as R;
        let fp = Some(MsekFingerprint::of(&mail_key(2)));
        let rewrap = R::Credential(mail_credential(50, |c| c.wrapped_under = fp))
            .encode()
            .unwrap();
        for marked in [
            mail_credential(40, |c| {
                c.revoked_at_unix = Some(1_800_000_400);
                c.secret = Default::default();
            }),
            mail_credential(40, |c| {
                c.burned = Some(a_mail_burn(7, 1_800_000_400));
                c.secret = Default::default();
            }),
        ] {
            let joined: MailCredential = fauna_core::encoding::canonical_decode(&merged_mail_row(
                "credential/iphone-mail",
                &R::Credential(marked.clone()).encode().unwrap(),
                &rewrap,
            ))
            .unwrap();
            assert!(joined.is_marked(), "the marker survives the later stamp");
            assert_eq!(joined.revoked_at_unix, marked.revoked_at_unix);
            assert_eq!(joined.burned, marked.burned);
            assert!(joined.secret.is_empty(), "a marked join carries no secret");
            assert_eq!(joined.wrapped_under, fp, "the remainder is latest-wins");
            assert_eq!(joined.updated_at, fauna_core::data::Timestamp(50));
        }
        // Two revokes: the earliest instant wins, whichever stamp is later.
        let early = R::Credential(mail_credential(10, |c| {
            c.revoked_at_unix = Some(5);
            c.secret = Default::default();
        }));
        let late = R::Credential(mail_credential(90, |c| {
            c.revoked_at_unix = Some(9);
            c.secret = Default::default();
        }));
        let joined: MailCredential = fauna_core::encoding::canonical_decode(&merged_mail_row(
            "credential/iphone-mail",
            &late.encode().unwrap(),
            &early.encode().unwrap(),
        ))
        .unwrap();
        assert_eq!(joined.revoked_at_unix, Some(5));
    }

    /// The echo-stop: a value the current one already covers changes nothing.
    #[test]
    fn a_covered_mail_row_reports_keep_current() {
        use fauna_core::mail_rows::MailRecord as R;
        let current = R::Credential(mail_credential(20, |_| {}));
        let older = R::Credential(mail_credential(10, |c| c.display_name = "old".into()));
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&mail_entry(&current)),
                &mail_entry(&older),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
        let state = R::State(mail_state(Some(1), 20, |_| {}));
        let stale = R::State(mail_state(Some(1), 10, |s| s.caldav_enabled = true));
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&mail_entry(&state)),
                &mail_entry(&stale),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// No deletion exists: a tombstone is refused, at merge and first contact
    /// alike, and so is a value that will not decode, a key outside the
    /// grammar, a state row under a credential key (and the reverse), a
    /// credential filed under another id, and a marked credential still
    /// carrying a secret — the one shape the join never produces.
    #[test]
    fn a_mail_tombstone_junk_misfiled_or_secret_bearing_marked_row_is_refused() {
        use fauna_core::mail_rows::MailRecord as R;
        let cred = R::Credential(mail_credential(10, |_| {}));
        let mut dead = mail_entry(&cred);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let bad = |e: EntryPlaintext| {
            for current in [None, Some(&mail_entry(&cred))] {
                assert!(
                    matches!(
                        apply_class2(MergePolicy::CrdtPerField, current, &e),
                        Err(MergeError::BadValue { .. })
                    ),
                    "{} must be refused",
                    e.key
                );
            }
        };
        let mut junk = mail_entry(&cred);
        junk.value = vec![0xff, 0x00].into();
        bad(junk);
        let mut unkeyed = mail_entry(&cred);
        unkeyed.key = "mail".into();
        bad(unkeyed);
        let mut empty_id = mail_entry(&cred);
        empty_id.key = "credential/".into();
        bad(empty_id);
        let mut misfiled = mail_entry(&cred);
        misfiled.key = "credential/other-mail".into();
        bad(misfiled);
        let mut state_as_credential = mail_entry(&R::State(mail_state(Some(1), 1, |_| {})));
        state_as_credential.key = "credential/iphone-mail".into();
        bad(state_as_credential);
        let mut credential_as_state = mail_entry(&cred);
        credential_as_state.key = "self".into();
        bad(credential_as_state);
        bad(mail_entry(&R::Credential(mail_credential(10, |c| {
            c.revoked_at_unix = Some(3);
        }))));
    }

    /// The P4 decode posture — an unknown field is refused, never stripped:
    /// on the state row (`deny_unknown_fields`, and at depth, inside a
    /// burn), and on the credential row (strict by round-trip, since its
    /// value type stays tolerant). A row from a build past this one
    /// answers `BadValue` and re-presents on the next reconcile.
    #[test]
    fn a_newer_mail_field_is_refused_not_stripped() {
        use fauna_cbor::Value;
        use fauna_core::mail_rows::MailRecord as R;
        let as_map = |bytes: &[u8]| match fauna_cbor::decode_strict::<Value>(bytes).unwrap() {
            Value::Map(m) => m,
            other => panic!("a mail row encodes as a map, got {other:?}"),
        };
        let reencode = |m: std::collections::BTreeMap<String, Value>| -> Vec<u8> {
            fauna_cbor::encode_canonical(&Value::Map(m)).unwrap()
        };
        let state = mail_entry(&R::State(mail_state(Some(1), 1, |s| {
            s.succession_burns = vec![a_mail_burn(7, 1)];
        })));
        let credential = mail_entry(&R::Credential(mail_credential(1, |_| {})));
        let mut newers = Vec::new();
        for current in [&state, &credential] {
            let mut map = as_map(&current.value);
            map.insert("from_the_future".into(), Value::Integer(1));
            let mut newer = current.clone();
            newer.value = reencode(map).into();
            newers.push((current, newer));
        }
        // At depth: a field inside the state row's burn.
        let mut map = as_map(&state.value);
        let Some(Value::List(burns)) = map.get_mut("succession_burns") else {
            panic!("the burns encode as a list");
        };
        let Value::Map(burn) = &mut burns[0] else {
            panic!("a burn encodes as a map");
        };
        burn.insert("from_the_future".into(), Value::Integer(1));
        let mut deep = state.clone();
        deep.value = reencode(map).into();
        newers.push((&state, deep));
        for (current, newer) in newers {
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, None, &newer),
                Err(MergeError::BadValue { .. })
            ));
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, Some(current), &newer),
                Err(MergeError::BadValue { .. })
            ));
        }
    }

    /// **The size pins (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows* → *The mail plane*):** the state row is bounded by shape but for
    /// the burns, one per succession ceremony — a row with a full window
    /// (2 priors + 2 retirements), a sentinel, every flag and 512 burns seals
    /// under HALF the per-entry cap; a credential row with a 4 KiB id, a 4 KiB
    /// name, a 4 KiB secret, both markers and a fingerprint does too. A
    /// burn's instant is unix SECONDS, so it is pinned at `u32::MAX` (the year
    /// 2106) — at `u64::MAX` each burn's instant would take four bytes more on
    /// the wire, and 512 of them tip the row just over the half-cap (33 462
    /// bytes, measured 2026-09-30); every other field is at its widest.
    #[test]
    fn an_oversized_mail_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::data::{MsekFingerprint, PriorMsekRetirement};
        use fauna_core::mail_rows::{MailRecord as R, MailRotationSentinel};
        let generation_sealed =
            matches!(sealing_epoch(KIND_MAIL), Some(SealingEpoch::GenerationTip));
        let state = mail_state(Some(1), u64::MAX, |s| {
            s.prior_mseks = vec![mail_key(2), mail_key(3)];
            s.prior_msek_retirements = [2, 3]
                .map(|k| PriorMsekRetirement {
                    msek: mail_key(k),
                    retired_at_unix: u64::MAX,
                })
                .to_vec();
            s.pending_rotation = Some(MailRotationSentinel {
                new_msek: mail_key(4),
            });
            s.mail_enabled = Some(true);
            s.caldav_enabled = true;
            s.carddav_enabled = true;
            s.succession_burns = (0..512u32)
                .map(|i| {
                    let mut p = [0xff; 32];
                    p[..4].copy_from_slice(&i.to_be_bytes());
                    fauna_core::data::MailSuccessionBurn {
                        predecessor: fauna_core::identity::ActorId(p),
                        at_unix: u64::from(u32::MAX),
                    }
                })
                .collect();
        });
        let long = "x".repeat(4096);
        let credential = mail_credential(u64::MAX, |c| {
            c.credential_id = long.clone();
            c.display_name = long.clone();
            c.secret = vec![0xab; 4096].into();
            c.created_at = u64::MAX;
            c.wrapped_under = Some(MsekFingerprint::of(&mail_key(1)));
            c.revoked_at_unix = Some(u64::MAX);
            c.burned = Some(a_mail_burn(7, u64::MAX));
        });
        for rec in [R::State(state), R::Credential(credential)] {
            let key = rec.plane_key().unwrap();
            let entry = stamped(KIND_MAIL, &key, &rec.encode().unwrap(), i64::MAX, 0xff);
            let len = sealed_envelope_len(&entry, generation_sealed).unwrap();
            assert!(
                len <= MAX_STATE_ENTRY_BYTES / 2,
                "{} seals to {len} bytes, over half the {MAX_STATE_ENTRY_BYTES}-byte cap",
                &key[..key.len().min(16)]
            );
        }
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the credential
    /// secret (a `SecretByteBuf` since the 2026-09-30 re-cut — a `SecretBytes`
    /// encoded an integer array), the generation fingerprint, every MSEK
    /// carrier on the state row and the burn's predecessor id all ride as
    /// CBOR byte strings (major 2), never serde's default integer array.
    #[test]
    fn the_mail_credential_secret_is_a_cbor_byte_string() {
        use fauna_core::data::{MsekFingerprint, PriorMsekRetirement};
        use fauna_core::mail_rows::{MailRecord as R, MailRotationSentinel};
        let byte_string = |bytes: &[u8], payload: &[u8], what: &str| {
            let mut want = if payload.len() < 24 {
                vec![0x40 | payload.len() as u8]
            } else {
                vec![0x58, payload.len() as u8]
            };
            want.extend_from_slice(payload);
            assert!(
                bytes.windows(want.len()).any(|w| w == want.as_slice()),
                "{what} must encode as a CBOR byte string"
            );
        };
        let fp = MsekFingerprint::of(&mail_key(9));
        let cred = R::Credential(mail_credential(1, |c| {
            c.secret = vec![0xab; 32].into();
            c.wrapped_under = Some(fp);
        }))
        .encode()
        .unwrap();
        byte_string(&cred, &[0xab; 32], "the credential secret");
        byte_string(&cred, &fp.0, "the generation fingerprint");
        let state = R::State(mail_state(Some(0xa1), 1, |s| {
            s.prior_mseks = vec![mail_key(0xa2)];
            s.prior_msek_retirements = vec![PriorMsekRetirement {
                msek: mail_key(0xa2),
                retired_at_unix: 1,
            }];
            s.pending_rotation = Some(MailRotationSentinel {
                new_msek: mail_key(0xa3),
            });
            s.succession_burns = vec![a_mail_burn(0xa4, 1)];
        }))
        .encode()
        .unwrap();
        byte_string(&state, &[0xa1; 32], "the MSEK");
        byte_string(&state, &[0xa2; 32], "a prior MSEK and its retirement key");
        byte_string(&state, &[0xa3; 32], "the sentinel's incoming MSEK");
        byte_string(&state, &[0xa4; 32], "a burn's predecessor id");
    }

    // ── `fauna.state.subscriptions` (the subscription period keys) ──

    fn a_period(version: u64, key: u8, rotated_at: u64) -> fauna_core::data::TierPeriod {
        fauna_core::data::TierPeriod {
            version,
            key: [key; 32].into(),
            rotated_at,
            minted_by: Some(fauna_core::identity::ActorId([0xA1; 32])),
        }
    }

    fn a_removal(tier: &str, sub: u8, version: u64, key: u8) -> fauna_core::data::PendingRemoval {
        fauna_core::data::PendingRemoval {
            tier_name: tier.into(),
            subscriber_id: fauna_core::identity::ActorId([sub; 32]),
            new_period: a_period(version, key, version * 1000),
        }
    }

    fn subscriptions_entry(
        row: &fauna_core::subscription_rows::SubscriptionsRow,
    ) -> EntryPlaintext {
        crdt_entry(KIND_SUBSCRIPTIONS, &row.plane_key(), row.encode().unwrap())
    }

    fn merged_subscriptions_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_SUBSCRIPTIONS, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.subscriptions` row): the frozen string, per-field
    /// CRDT, fleet-only, tip-sealed — and an ACCOUNT row on departure: the
    /// period keys are the account's, never the writing device's.
    #[test]
    fn the_subscriptions_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_SUBSCRIPTIONS, "fauna.state.subscriptions");
        assert_eq!(
            merge_policy(KIND_SUBSCRIPTIONS),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_SUBSCRIPTIONS),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_SUBSCRIPTIONS),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_SUBSCRIPTIONS));
        assert!(!delegable_kinds().any(|k| k == KIND_SUBSCRIPTIONS));
        assert_eq!(
            TipSealedKind::of(KIND_SUBSCRIPTIONS).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    /// The join laws, ON BYTES through the plane arm, per row type:
    /// commutative, associative, idempotent. A period row's samples are the
    /// one record its key names (write-once); a removal row's are the one
    /// removal, settled and not.
    #[test]
    fn subscriptions_merge_is_a_join_on_bytes() {
        use fauna_core::subscription_rows::{PendingRemovalRow, SubscriptionsRow, TierPeriodRow};
        let check = |key: &str, samples: &[Vec<u8>]| {
            let m = |a: &[u8], b: &[u8]| merged_subscriptions_row(key, a, b);
            for a in samples {
                assert_eq!(m(a, a), *a, "idempotent");
                for b in samples {
                    assert_eq!(m(a, b), m(b, a), "commutative");
                    for c in samples {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                    }
                }
            }
        };
        let period = SubscriptionsRow::Period(TierPeriodRow {
            tier_name: "gold".into(),
            period: a_period(3, 0x33, 3000),
        });
        check(&period.plane_key(), &[period.encode().unwrap()]);
        let removal = |settled| {
            SubscriptionsRow::Removal(PendingRemovalRow {
                removal: a_removal("gold", 7, 4, 0x44),
                settled,
            })
        };
        let (live, done) = (removal(false), removal(true));
        check(
            &live.plane_key(),
            &[live.encode().unwrap(), done.encode().unwrap()],
        );
        // And it is the monotone marker the rule promises: settled wins from
        // either side.
        assert_eq!(
            merged_subscriptions_row(
                &live.plane_key(),
                &live.encode().unwrap(),
                &done.encode().unwrap()
            ),
            done.encode().unwrap()
        );
    }

    /// The arm is the fold's per-row half, not a second rule: joining two
    /// composites through the shipped `SubscriptionsConfig::merge` and
    /// splitting the result into rows gives exactly the rows the arm produces
    /// from the two sides' rows — including a concurrent rotation (two
    /// version-3 periods of one tier, both kept).
    #[test]
    fn the_subscriptions_arm_agrees_with_the_composite_fold() {
        use fauna_core::data::{SubscriptionsConfig, TierPeriodKeys};
        let a = SubscriptionsConfig {
            tiers: vec![TierPeriodKeys {
                tier_name: "gold".into(),
                current: a_period(3, 0x33, 3000),
                prior: vec![a_period(2, 0x22, 2000)],
            }],
            pending_removals: vec![a_removal("gold", 7, 4, 0x44)],
        };
        let b = SubscriptionsConfig {
            tiers: vec![
                TierPeriodKeys {
                    tier_name: "gold".into(),
                    current: a_period(3, 0x3B, 3001),
                    prior: vec![a_period(2, 0x22, 2000)],
                },
                TierPeriodKeys {
                    tier_name: "silver".into(),
                    current: a_period(1, 0x51, 1000),
                    prior: vec![],
                },
            ],
            pending_removals: vec![a_removal("gold", 7, 4, 0x44)],
        };
        let composite: Vec<(String, Vec<u8>)> = a
            .merge(&b)
            .rows()
            .into_iter()
            .map(|(k, r)| (k, r.encode().unwrap()))
            .collect();
        let mut by_arm: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
        for (k, r) in a.rows().into_iter().chain(b.rows()) {
            let v = r.encode().unwrap();
            let next = match by_arm.get(&k) {
                Some(cur) => merged_subscriptions_row(&k, cur, &v),
                None => v,
            };
            by_arm.insert(k, next);
        }
        assert_eq!(by_arm.into_iter().collect::<Vec<_>>(), composite);
        assert_eq!(composite.len(), 5, "four distinct periods, one removal");
    }

    /// The echo-stop: a row the current one already covers changes nothing.
    #[test]
    fn a_covered_subscriptions_row_reports_keep_current() {
        use fauna_core::subscription_rows::{PendingRemovalRow, SubscriptionsRow};
        let removal = |settled| {
            SubscriptionsRow::Removal(PendingRemovalRow {
                removal: a_removal("gold", 7, 4, 0x44),
                settled,
            })
        };
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&subscriptions_entry(&removal(true))),
                &subscriptions_entry(&removal(false)),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// No deletion exists for a row: a tombstone is refused at merge and at
    /// first contact alike, and so is a value that will not decode, a key
    /// outside the grammar, a value filed under another record's digest, a
    /// row of the other type, and two different periods meeting at one key.
    #[test]
    fn a_subscriptions_tombstone_junk_or_misfiled_row_is_refused() {
        use fauna_core::subscription_rows::{SubscriptionsRow, TierPeriodRow};
        let row = SubscriptionsRow::Period(TierPeriodRow {
            tier_name: "gold".into(),
            period: a_period(1, 0x11, 1000),
        });
        let mut dead = subscriptions_entry(&row);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let bad = |e: EntryPlaintext| {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &e),
                    Err(MergeError::BadValue { .. })
                ),
                "{} must be refused",
                e.key
            );
        };
        let mut junk = subscriptions_entry(&row);
        junk.value = vec![0xff, 0x00].into();
        bad(junk);
        let mut self_key = subscriptions_entry(&row);
        self_key.key = "self".into();
        bad(self_key);
        let other = SubscriptionsRow::Period(TierPeriodRow {
            tier_name: "gold".into(),
            period: a_period(1, 0x12, 1000),
        });
        let mut misfiled = subscriptions_entry(&row);
        misfiled.key = other.plane_key();
        bad(misfiled);
        let mut upper = subscriptions_entry(&row);
        upper.key = upper.key.to_uppercase().replace("PERIOD/", "period/");
        bad(upper);
        let mut wrong_type = subscriptions_entry(&row);
        wrong_type.key = wrong_type.key.replace("period/", "removal/");
        bad(wrong_type);
        // Two different periods at one key cannot both be honest: refused
        // at merge, never silently picked.
        let mut forged = subscriptions_entry(&other);
        forged.key = row.plane_key();
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&subscriptions_entry(&row)),
                &forged
            ),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// The P4 decode posture — `deny_unknown_fields`, on both row types and
    /// the period they carry: a row with a field from a build past this one
    /// answers `BadValue` (it re-presents on the next reconcile) instead of
    /// merging with the field silently stripped from the re-encoded row.
    #[test]
    fn a_newer_subscriptions_field_is_refused_not_stripped() {
        use fauna_core::subscription_rows::{PendingRemovalRow, SubscriptionsRow, TierPeriodRow};
        #[derive(serde::Serialize)]
        struct Newer<'a, T> {
            #[serde(flatten)]
            record: &'a T,
            from_the_future: u8,
        }
        fn with_extra_field<T: serde::Serialize>(record: &T) -> Vec<u8> {
            fauna_core::encoding::canonical_encode(&Newer {
                record,
                from_the_future: 1,
            })
            .unwrap()
            .to_vec()
        }
        let period_row = TierPeriodRow {
            tier_name: "gold".into(),
            period: a_period(1, 0x11, 1000),
        };
        let removal_row = PendingRemovalRow {
            removal: a_removal("gold", 7, 2, 0x22),
            settled: false,
        };
        // A newer field on the row itself.
        for (row, newer) in [
            (
                SubscriptionsRow::Period(period_row.clone()),
                with_extra_field(&period_row),
            ),
            (
                SubscriptionsRow::Removal(removal_row.clone()),
                with_extra_field(&removal_row),
            ),
        ] {
            let current = subscriptions_entry(&row);
            let mut incoming = current.clone();
            incoming.value = newer.into();
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, Some(&current), &incoming),
                Err(MergeError::BadValue { .. })
            ));
        }
        // And one nested in the period it carries.
        #[derive(serde::Serialize)]
        struct RowWithNewerPeriod<'a> {
            tier_name: &'a str,
            period: Newer<'a, fauna_core::data::TierPeriod>,
        }
        let current = subscriptions_entry(&SubscriptionsRow::Period(period_row.clone()));
        let mut incoming = current.clone();
        incoming.value = fauna_core::encoding::canonical_encode(&RowWithNewerPeriod {
            tier_name: &period_row.tier_name,
            period: Newer {
                record: &period_row.period,
                from_the_future: 1,
            },
        })
        .unwrap()
        .to_vec()
        .into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&current), &incoming),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per period key or staged removal is bounded by the
    /// record's own shape — with a 4 KiB tier name, either row seals under
    /// HALF the per-entry cap, however many periods the tier accumulates.
    #[test]
    fn an_oversized_subscriptions_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::subscription_rows::{PendingRemovalRow, SubscriptionsRow, TierPeriodRow};
        let name = "t".repeat(4096);
        let generation_sealed = matches!(
            sealing_epoch(KIND_SUBSCRIPTIONS),
            Some(SealingEpoch::GenerationTip)
        );
        for row in [
            SubscriptionsRow::Period(TierPeriodRow {
                tier_name: name.clone(),
                period: a_period(u64::MAX, 0xff, u64::MAX),
            }),
            SubscriptionsRow::Removal(PendingRemovalRow {
                removal: fauna_core::data::PendingRemoval {
                    new_period: a_period(u64::MAX, 0xff, u64::MAX),
                    ..a_removal(&name, 0xff, 1, 0xff)
                },
                settled: true,
            }),
        ] {
            let len = sealed_envelope_len(&subscriptions_entry(&row), generation_sealed).unwrap();
            assert!(
                len <= MAX_STATE_ENTRY_BYTES / 2,
                "an oversized subscriptions row seals to {len} bytes, over half the \
                 {MAX_STATE_ENTRY_BYTES}-byte cap"
            );
        }
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the period key
    /// and the subscriber id ride as CBOR byte strings (major 2, length 32),
    /// never serde's default integer array.
    #[test]
    fn the_subscriptions_key_material_is_a_cbor_byte_string() {
        use fauna_core::subscription_rows::{PendingRemovalRow, SubscriptionsRow};
        let bytes = SubscriptionsRow::Removal(PendingRemovalRow {
            removal: a_removal("gold", 0x77, 2, 0x44),
            settled: false,
        })
        .encode()
        .unwrap();
        for fill in [0x44u8, 0x77] {
            let mut want = vec![0x58, 0x20];
            want.extend_from_slice(&[fill; 32]);
            assert!(
                bytes.windows(want.len()).any(|w| w == want.as_slice()),
                "the {fill:#x} field must encode as a 32-byte CBOR byte string"
            );
        }
    }

    // ── `fauna.state.deployment-seeds` (the multi-nest deployment-seed custody) ──

    fn a_seed_entry(
        seed: u8,
        domain: Option<&str>,
        superseded_by: Option<u8>,
    ) -> fauna_core::data::DeploymentSeedEntry {
        fauna_core::data::DeploymentSeedEntry {
            nest_actor_id: fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(
                [seed; 32],
            ),
            seed: [seed; 32].into(),
            domain: domain.map(str::to_string),
            superseded_by: superseded_by
                .map(|s| fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed([s; 32])),
            ..Default::default()
        }
    }

    fn seed_entry(e: &fauna_core::data::DeploymentSeedEntry) -> EntryPlaintext {
        crdt_entry(
            KIND_DEPLOYMENT_SEEDS,
            &e.plane_key(),
            e.encode_row().unwrap(),
        )
    }

    fn merged_seed_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_DEPLOYMENT_SEEDS, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.deployment-seeds` row): the frozen string,
    /// per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
    /// departure: the seeds are the account's, never the capturing device's.
    #[test]
    fn the_deployment_seeds_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_DEPLOYMENT_SEEDS, "fauna.state.deployment-seeds");
        assert_eq!(
            merge_policy(KIND_DEPLOYMENT_SEEDS),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_DEPLOYMENT_SEEDS),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_DEPLOYMENT_SEEDS),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_DEPLOYMENT_SEEDS));
        assert!(!delegable_kinds().any(|k| k == KIND_DEPLOYMENT_SEEDS));
        assert_eq!(
            TipSealedKind::of(KIND_DEPLOYMENT_SEEDS).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    /// The join laws, ON BYTES through the plane arm: commutative,
    /// associative, idempotent over every shape one box's row takes — no
    /// label, two differing labels, unmarked, and marked by two differing
    /// successors (fork evidence) — and the rule's asymmetry holds from
    /// either side: a supersession mark is never undone.
    #[test]
    fn deployment_seeds_merge_is_a_join_on_bytes() {
        let samples: Vec<Vec<u8>> = [
            a_seed_entry(0x11, None, None),
            a_seed_entry(0x11, Some("b.example"), None),
            a_seed_entry(0x11, Some("a.example"), None),
            a_seed_entry(0x11, None, Some(0x12)),
            a_seed_entry(0x11, Some("b.example"), Some(0x13)),
        ]
        .iter()
        .map(|e| e.encode_row().unwrap())
        .collect();
        let key = a_seed_entry(0x11, None, None).plane_key();
        let m = |a: &[u8], b: &[u8]| merged_seed_row(&key, a, b);
        for a in &samples {
            assert_eq!(m(a, a), *a, "idempotent");
            for b in &samples {
                assert_eq!(m(a, b), m(b, a), "commutative");
                for c in &samples {
                    assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                }
            }
        }
        let (bare, marked) = (&samples[0], &samples[3]);
        assert_eq!(m(bare, marked), *marked, "a lagging copy never un-marks");
        assert_eq!(m(marked, bare), *marked);
    }

    /// The arm is the union's per-box half, not a second rule: joining two
    /// devices' maps through the shipped `DeploymentSeedEntry::merge_seed_map`
    /// and splitting the result into rows gives exactly the rows the arm
    /// produces from the two sides' rows.
    #[test]
    fn the_deployment_seeds_arm_agrees_with_the_composite_fold() {
        use fauna_core::data::DeploymentSeedEntry;
        use fauna_core::deployment_seed_rows::deployment_seed_rows;
        let a = vec![
            a_seed_entry(0x11, None, Some(0x12)),
            a_seed_entry(0x21, Some("a.example"), None),
        ];
        let b = vec![
            a_seed_entry(0x11, Some("old.example"), None),
            a_seed_entry(0x12, Some("new.example"), None),
        ];
        let want: Vec<(String, Vec<u8>)> =
            deployment_seed_rows(&DeploymentSeedEntry::merge_seed_map(&a, &b))
                .into_iter()
                .map(|(k, e)| (k, e.encode_row().unwrap()))
                .collect();
        let mut got = std::collections::BTreeMap::<String, Vec<u8>>::new();
        for (k, e) in deployment_seed_rows(&a)
            .into_iter()
            .chain(deployment_seed_rows(&b))
        {
            let v = e.encode_row().unwrap();
            let joined = match got.get(&k) {
                Some(cur) => merged_seed_row(&k, cur, &v),
                None => v,
            };
            got.insert(k, joined);
        }
        assert_eq!(got.into_iter().collect::<Vec<_>>(), want);
    }

    /// A covered row — the incoming side adds nothing — is `KeepCurrent`, so
    /// a converged pair stops publishing.
    #[test]
    fn a_covered_deployment_seed_row_reports_keep_current() {
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&seed_entry(&a_seed_entry(
                    0x11,
                    Some("a.example"),
                    Some(0x12)
                ))),
                &seed_entry(&a_seed_entry(0x11, None, None)),
            ),
            Ok(MergeOutcome::KeepCurrent)
        ));
    }

    /// A tombstone, junk bytes, a key outside the grammar (`self`, upper
    /// case), a value filed under another box's key, and a forged row whose
    /// seed is not its id's preimage are all refused — at first contact, and
    /// against a good current row.
    #[test]
    fn a_deployment_seed_tombstone_junk_misfiled_or_forged_row_is_refused() {
        let good = a_seed_entry(0x11, Some("a.example"), None);
        let mut dead = seed_entry(&good);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let mut junk = seed_entry(&good);
        junk.value = vec![0xff, 0x00].into();
        let mut self_key = seed_entry(&good);
        self_key.key = "self".into();
        let mut upper = seed_entry(&good);
        upper.key = upper.key.to_uppercase();
        let mut misfiled = seed_entry(&good);
        misfiled.key = a_seed_entry(0x22, None, None).plane_key();
        let forged = seed_entry(&fauna_core::data::DeploymentSeedEntry {
            seed: [0x77; 32].into(),
            ..good.clone()
        });
        for bad in [junk, self_key, upper, misfiled, forged] {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &bad),
                    Err(MergeError::BadValue { .. })
                ),
                "first contact must refuse the row at {:?}",
                bad.key
            );
            if bad.key == seed_entry(&good).key {
                assert!(matches!(
                    apply_class2(MergePolicy::CrdtPerField, Some(&seed_entry(&good)), &bad),
                    Err(MergeError::BadValue { .. })
                ));
            }
        }
    }

    /// **The decode-posture pin — strict on the plane, though the value type
    /// cannot take `deny_unknown_fields`** (its `#[serde(flatten)] extra`
    /// keeps its tolerance, and serde refuses the two together): a
    /// row with a field from a build past this one answers `BadValue` (it
    /// re-presents on the next reconcile) instead of merging with the field
    /// silently stripped — and so does a non-canonical encoding of a known
    /// row, which would not re-encode to its own bytes.
    #[test]
    fn a_newer_deployment_seed_field_is_refused_not_stripped() {
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a fauna_core::data::DeploymentSeedEntry,
            from_the_future: u8,
        }
        let good = a_seed_entry(0x11, Some("a.example"), None);
        let current = seed_entry(&good);
        let mut incoming = current.clone();
        incoming.value = fauna_core::encoding::canonical_encode(&Newer {
            record: &good,
            from_the_future: 1,
        })
        .unwrap()
        .to_vec()
        .into();
        for cur in [None, Some(&current)] {
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, cur, &incoming),
                Err(MergeError::BadValue { .. })
            ));
        }
        // The same row, its map header written in a non-minimal form.
        let canonical = good.encode_row().unwrap();
        assert_eq!(canonical[0] & 0xe0, 0xa0, "a short CBOR map header");
        let mut loose = vec![0xb8, canonical[0] & 0x1f];
        loose.extend_from_slice(&canonical[1..]);
        let mut non_canonical = current.clone();
        non_canonical.value = loose.into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &non_canonical),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per custodied box is bounded by the entry's own
    /// shape — with a 4 KiB domain label and a supersession mark, the row
    /// seals under HALF the per-entry cap, however many boxes the admin
    /// identity administers.
    #[test]
    fn an_oversized_deployment_seed_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        let generation_sealed = matches!(
            sealing_epoch(KIND_DEPLOYMENT_SEEDS),
            Some(SealingEpoch::GenerationTip)
        );
        let label = "d".repeat(4096);
        let row = a_seed_entry(0xff, Some(&label), Some(0xfe));
        let len = sealed_envelope_len(&seed_entry(&row), generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES / 2,
            "an oversized deployment-seed row seals to {len} bytes, over half the \
             {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the id, the
    /// seed and the successor id ride as CBOR byte strings (major 2, length
    /// 32), never serde's default integer array.
    #[test]
    fn the_deployment_seed_key_material_is_a_cbor_byte_string() {
        let e = a_seed_entry(0x11, None, Some(0x12));
        let bytes = e.encode_row().unwrap();
        for field in [e.nest_actor_id, e.seed.to_array(), e.superseded_by.unwrap()] {
            let mut want = vec![0x58, 0x20];
            want.extend_from_slice(&field);
            assert!(
                bytes.windows(want.len()).any(|w| w == want.as_slice()),
                "every 32-byte field must encode as a CBOR byte string"
            );
        }
    }

    // ── `fauna.state.custody-ceremony` (the custody ceremony's record-then-act state) ──

    fn a_granted(
        accept: &[u8],
        receipt: (&[u8], u64, bool),
        updated: u64,
        host: u8,
    ) -> fauna_core::custody_ceremony::GrantedCustody {
        fauna_core::custody_ceremony::GrantedCustody {
            grant_id: vec![0x1D; 16],
            host: [host; 32],
            channel_hex: "aa".repeat(32),
            offer: vec![0xF0, 0x1D],
            accept: accept.to_vec(),
            minted: !accept.is_empty(),
            latest_receipt: receipt.0.to_vec(),
            latest_receipt_at: fauna_core::data::Timestamp(receipt.1),
            receipt_row_written: receipt.2,
            offered_at: fauna_core::data::Timestamp(1_000),
            duration_secs: 3_600,
            updated_at: fauna_core::data::Timestamp(updated),
            ..Default::default()
        }
    }

    fn a_held(
        deliver: &[u8],
        mint: (&[u8], u64, bool, bool),
        knobs: Option<(u64, bool, u64)>,
        updated: u64,
    ) -> fauna_core::custody_ceremony::HeldCustody {
        fauna_core::custody_ceremony::HeldCustody {
            grant_id: vec![0x2E; 16],
            owner: [9u8; 32],
            channel_hex: "bb".repeat(32),
            offer: vec![0x0F, 0x2E],
            deliver: deliver.to_vec(),
            held_row_written: !deliver.is_empty(),
            receipt: mint.0.to_vec(),
            receipt_minted_at: fauna_core::data::Timestamp(mint.1),
            receipt_posted: mint.2,
            receipt_degraded: mint.3,
            host_knobs: knobs.map(
                |(cap, stopped, at)| fauna_core::custody_ceremony::HostKnobs {
                    retained_bytes_cap: cap,
                    stopped,
                    set_at: fauna_core::data::Timestamp(at),
                },
            ),
            updated_at: fauna_core::data::Timestamp(updated),
            ..Default::default()
        }
    }

    /// A held record whose deliver was captured at `at`, its runtime row
    /// `written` or owed — the deliver's freshest-wins clause.
    fn delivered_at(
        deliver: &[u8],
        at: u64,
        written: bool,
    ) -> fauna_core::custody_ceremony::HeldCustody {
        let mut h = a_held(deliver, (&[], 0, false, false), None, 1_000);
        h.deliver_at = fauna_core::data::Timestamp(at);
        h.held_row_written = written;
        h
    }

    fn custody_row(record: fauna_core::custody_ceremony_rows::CustodyRecord) -> (String, Vec<u8>) {
        (record.key().unwrap().render(), record.encode().unwrap())
    }

    fn custody_entry(record: fauna_core::custody_ceremony_rows::CustodyRecord) -> EntryPlaintext {
        let (key, value) = custody_row(record);
        crdt_entry(KIND_CUSTODY_CEREMONY, &key, value)
    }

    fn merged_custody_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_CUSTODY_CEREMONY, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.nostr-confirmation` row): the frozen string and
    /// row key, per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row
    /// on departure: the confirmation answers every device's check.
    #[test]
    fn the_nostr_confirmation_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_NOSTR_CONFIRMATION, "fauna.state.nostr-confirmation");
        assert_eq!(NOSTR_CONFIRMATION_ROW_KEY, "self");
        assert_eq!(
            merge_policy(KIND_NOSTR_CONFIRMATION),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_NOSTR_CONFIRMATION),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_NOSTR_CONFIRMATION),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_NOSTR_CONFIRMATION));
        assert!(!delegable_kinds().any(|k| k == KIND_NOSTR_CONFIRMATION));
        assert_eq!(
            TipSealedKind::of(KIND_NOSTR_CONFIRMATION).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    fn nostr_confirmation_row(at: Option<i64>) -> Vec<u8> {
        fauna_core::nostr_confirmation::NostrConfirmation { confirmed_at: at }
            .encode()
            .unwrap()
    }

    fn merged_nostr_confirmation(a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| {
            crdt_entry(
                KIND_NOSTR_CONFIRMATION,
                NOSTR_CONFIRMATION_ROW_KEY,
                v.to_vec(),
            )
        };
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The join laws, ON BYTES through the plane arm: commutative,
    /// associative, idempotent over an unconfirmed row and three stamps —
    /// and the join is the max.
    #[test]
    fn nostr_confirmation_merge_is_a_join_on_bytes() {
        let samples: Vec<Vec<u8>> = [None, Some(1), Some(1_700_000_000), Some(i64::MAX)]
            .into_iter()
            .map(nostr_confirmation_row)
            .collect();
        let m = merged_nostr_confirmation;
        for a in &samples {
            assert_eq!(m(a, a), *a, "idempotent");
            for b in &samples {
                assert_eq!(m(a, b), m(b, a), "commutative");
                for c in &samples {
                    assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                }
            }
        }
        assert_eq!(
            m(
                &nostr_confirmation_row(Some(9)),
                &nostr_confirmation_row(Some(5))
            ),
            nostr_confirmation_row(Some(9))
        );
        assert_eq!(
            m(
                &nostr_confirmation_row(None),
                &nostr_confirmation_row(Some(5))
            ),
            nostr_confirmation_row(Some(5))
        );
    }

    /// The strict posture, pinned (P4, `deny_unknown_fields`): a newer
    /// writer's field is `BadValue` at first contact and at merge — the entry
    /// re-presents on the next reconcile — and so is a row at any key but
    /// `self`.
    #[test]
    fn a_newer_nostr_confirmation_field_is_refused() {
        let known = nostr_confirmation_row(Some(7));
        let mut map = match fauna_cbor::decode_strict::<fauna_cbor::Value>(&known).unwrap() {
            fauna_cbor::Value::Map(m) => m,
            other => panic!("NostrConfirmation encodes as a map, got {other:?}"),
        };
        map.insert(
            "a_field_from_a_newer_writer".into(),
            fauna_cbor::Value::Integer(7),
        );
        let newer = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map)).unwrap();
        let entry = |key: &str, v: &[u8]| crdt_entry(KIND_NOSTR_CONFIRMATION, key, v.to_vec());
        assert!(matches!(
            validate_adoptable(
                MergePolicy::CrdtPerField,
                &entry(NOSTR_CONFIRMATION_ROW_KEY, &newer)
            ),
            Err(MergeError::BadValue { .. })
        ));
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&entry(NOSTR_CONFIRMATION_ROW_KEY, &known)),
                &entry(NOSTR_CONFIRMATION_ROW_KEY, &newer)
            ),
            Err(MergeError::BadValue { .. })
        ));
        assert!(matches!(
            validate_adoptable(MergePolicy::CrdtPerField, &entry("other", &known)),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.custody-ceremony` row): the frozen string,
    /// per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
    /// departure: a ceremony is the account's, never the driving device's.
    #[test]
    fn the_custody_ceremony_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_CUSTODY_CEREMONY, "fauna.state.custody-ceremony");
        assert_eq!(
            merge_policy(KIND_CUSTODY_CEREMONY),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_CUSTODY_CEREMONY),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_CUSTODY_CEREMONY),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_CUSTODY_CEREMONY));
        assert!(!delegable_kinds().any(|k| k == KIND_CUSTODY_CEREMONY));
        assert_eq!(
            TipSealedKind::of(KIND_CUSTODY_CEREMONY).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    /// The join laws, ON BYTES through the plane arm, per side: commutative,
    /// associative, idempotent over every shape one ceremony's record takes —
    /// captured or not, a receipt older, newer, or at the same instant with
    /// different bytes, written or not, and a tie on the record's own
    /// `updated_at` with a different scalar remainder — where a keep-mine
    /// would leave two replicas on different bytes.
    #[test]
    fn custody_ceremony_merge_is_a_join_on_bytes() {
        use fauna_core::custody_ceremony_rows::CustodyRecord::{Granted, Held};
        let granted: Vec<(String, Vec<u8>)> = [
            a_granted(&[], (&[], 0, false), 1_000, 2),
            a_granted(&[0xAC, 1], (&[], 0, false), 1_500, 2),
            a_granted(&[0xAC, 2], (&[0xB1], 5_000, true), 1_500, 3),
            a_granted(&[], (&[0xB2], 5_000, false), 2_000, 2),
            a_granted(&[0xAC, 1], (&[0xB1], 5_000, false), 2_000, 4),
            a_granted(&[], (&[0xB0], 7_000, false), 900, 2),
        ]
        .into_iter()
        .map(|r| custody_row(Granted(r)))
        .collect();
        let held: Vec<(String, Vec<u8>)> = [
            a_held(&[], (&[], 0, false, false), None, 1_000),
            a_held(
                &[0xDE],
                (&[0xC1], 5_000, true, false),
                Some((7, true, 20)),
                1_000,
            ),
            a_held(
                &[0xDF],
                (&[0xC2], 5_000, false, true),
                Some((9, false, 20)),
                1_000,
            ),
            a_held(
                &[],
                (&[0xC1], 5_000, false, true),
                Some((9, false, 30)),
                1_500,
            ),
            a_held(&[0xDE], (&[0xC0], 6_000, false, false), None, 800),
            // A superseding deliver: fresher (unwritten), the same instant
            // with different bytes, and the same deliver written through.
            delivered_at(&[0xDE], 10, true),
            delivered_at(&[0xDF], 20, false),
            delivered_at(&[0xDD], 20, true),
            delivered_at(&[0xDF], 20, true),
        ]
        .into_iter()
        .map(|r| custody_row(Held(r)))
        .collect();
        for samples in [granted, held] {
            let key = samples[0].0.clone();
            assert!(samples.iter().all(|(k, _)| *k == key));
            let m = |a: &[u8], b: &[u8]| merged_custody_row(&key, a, b);
            for (_, a) in &samples {
                assert_eq!(m(a, a), *a, "idempotent");
                for (_, b) in &samples {
                    assert_eq!(m(a, b), m(b, a), "commutative");
                    for (_, c) in &samples {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                    }
                }
            }
        }
    }

    /// The arm is the composite's per-record half, not a second rule: joining
    /// two devices' records through `CustodyConfig::merge` (the composite's
    /// statement) and splitting the result gives exactly the rows the arm
    /// produces from the two sides' rows.
    #[test]
    fn the_custody_ceremony_arm_agrees_with_the_composite_merge() {
        use fauna_core::custody_ceremony::CustodyConfig;
        let a = CustodyConfig {
            granted: vec![a_granted(&[0xAC, 1], (&[0xB1], 5_000, true), 2_000, 2)],
            held: vec![a_held(&[0xDE], (&[0xC1], 5_000, true, false), None, 1_000)],
        };
        let mut other_grant = a_granted(&[], (&[], 0, false), 1_000, 2);
        other_grant.grant_id = vec![0x3F; 16];
        let b = CustodyConfig {
            granted: vec![
                a_granted(&[], (&[0xB2], 7_000, false), 1_500, 3),
                other_grant,
            ],
            held: vec![a_held(
                &[],
                (&[], 0, false, false),
                Some((7, true, 20)),
                1_500,
            )],
        };
        let want: Vec<(String, Vec<u8>)> = a
            .merge(&b)
            .rows()
            .unwrap()
            .into_iter()
            .map(|(k, r)| (k, r.encode().unwrap()))
            .collect();
        let mut got = std::collections::BTreeMap::<String, Vec<u8>>::new();
        for (k, r) in a.rows().unwrap().into_iter().chain(b.rows().unwrap()) {
            let v = r.encode().unwrap();
            let joined = match got.get(&k) {
                Some(cur) => merged_custody_row(&k, cur, &v),
                None => v,
            };
            got.insert(k, joined);
        }
        let mut want_sorted = want;
        want_sorted.sort();
        assert_eq!(got.into_iter().collect::<Vec<_>>(), want_sorted);
    }

    /// A covered row — the incoming side adds nothing — is `KeepCurrent`, so
    /// a converged pair stops publishing.
    #[test]
    fn a_covered_custody_ceremony_row_reports_keep_current() {
        use fauna_core::custody_ceremony_rows::CustodyRecord::Granted;
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&custody_entry(Granted(a_granted(
                    &[0xAC, 1],
                    (&[0xB1], 5_000, true),
                    2_000,
                    2
                )))),
                &custody_entry(Granted(a_granted(&[], (&[], 0, false), 1_000, 2))),
            ),
            Ok(MergeOutcome::KeepCurrent)
        ));
    }

    /// A tombstone, junk bytes, a key outside the grammar (`self`, upper
    /// case), a record filed under another grant id, and a record filed
    /// under the other side are all refused — at first contact, and against
    /// a good current row.
    #[test]
    fn a_custody_ceremony_tombstone_junk_or_misfiled_row_is_refused() {
        use fauna_core::custody_ceremony_rows::CustodyRecord::Granted;
        let good = custody_entry(Granted(a_granted(&[], (&[], 0, false), 1_000, 2)));
        let mut dead = good.clone();
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let mut junk = good.clone();
        junk.value = vec![0xff, 0x00].into();
        let mut self_key = good.clone();
        self_key.key = "self".into();
        let mut upper = good.clone();
        upper.key = format!("granted/{}", "1D".repeat(16));
        let mut misfiled = good.clone();
        misfiled.key = format!("granted/{}", "3f".repeat(16));
        let mut other_side = good.clone();
        other_side.key = format!("held/{}", "1d".repeat(16));
        for bad in [junk, self_key, upper, misfiled, other_side] {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &bad),
                    Err(MergeError::BadValue { .. })
                ),
                "first contact must refuse the row at {:?}",
                bad.key
            );
            if bad.key == good.key {
                assert!(matches!(
                    apply_class2(MergePolicy::CrdtPerField, Some(&good), &bad),
                    Err(MergeError::BadValue { .. })
                ));
            }
        }
    }

    /// **The decode-posture pin — strict on the plane by round-trip, though
    /// the value types stay tolerant**: a row with a field from a build past this one
    /// answers `BadValue` (it re-presents on the next reconcile) instead of
    /// merging with the field silently stripped — and so does a
    /// non-canonical encoding of a known row.
    #[test]
    fn a_newer_custody_ceremony_field_is_refused_not_stripped() {
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a fauna_core::custody_ceremony::GrantedCustody,
            from_the_future: u8,
        }
        use fauna_core::custody_ceremony_rows::CustodyRecord::Granted;
        let good = a_granted(&[0xAC, 1], (&[0xB1], 5_000, true), 2_000, 2);
        let current = custody_entry(Granted(good.clone()));
        let mut incoming = current.clone();
        incoming.value = fauna_core::encoding::canonical_encode(&Newer {
            record: &good,
            from_the_future: 1,
        })
        .unwrap()
        .to_vec()
        .into();
        for cur in [None, Some(&current)] {
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, cur, &incoming),
                Err(MergeError::BadValue { .. })
            ));
        }
        // The same row, its map header written in a non-minimal form.
        let canonical = current.value.to_vec();
        assert_eq!(canonical[0] & 0xe0, 0xa0, "a short CBOR map header");
        let mut loose = vec![0xb8, canonical[0] & 0x1f];
        loose.extend_from_slice(&canonical[1..]);
        let mut non_canonical = current.clone();
        non_canonical.value = loose.into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &non_canonical),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the grant id,
    /// the counterparty and every verbatim envelope ride as CBOR byte strings,
    /// never serde's default integer array.
    #[test]
    fn the_custody_ceremony_bytes_are_cbor_byte_strings() {
        use fauna_core::custody_ceremony_rows::CustodyRecord::{Granted, Held};
        let (_, g) = custody_row(Granted(a_granted(
            &[0xAC, 1],
            (&[0xB1], 5_000, true),
            2_000,
            2,
        )));
        let (_, h) = custody_row(Held(a_held(
            &[0xDE],
            (&[0xC1], 5_000, true, false),
            None,
            1_000,
        )));
        let has = |bytes: &[u8], want: &[u8]| bytes.windows(want.len()).any(|w| w == want);
        let with_head = |head: &[u8], body: &[u8]| [head, body].concat();
        assert!(has(&g, &with_head(&[0x50], &[0x1D; 16])), "grant id");
        assert!(has(&g, &with_head(&[0x58, 0x20], &[2; 32])), "host");
        assert!(has(&g, &[0x42, 0xAC, 1]), "accept");
        assert!(has(&g, &[0x41, 0xB1]), "receipt");
        assert!(has(&h, &with_head(&[0x50], &[0x2E; 16])), "grant id");
        assert!(has(&h, &with_head(&[0x58, 0x20], &[9; 32])), "owner");
        assert!(has(&h, &[0x41, 0xDE]), "deliver");
        assert!(has(&h, &[0x41, 0xC1]), "receipt");
    }

    // ── `fauna.state.peer-anchors` (the succession witness's peer anchors) ──

    fn an_anchor_head(
        actor: u8,
        key: u8,
        seq: u64,
        first_seen: u64,
        outrun: bool,
    ) -> fauna_core::peer_anchor_rows::PeerAnchorRow {
        fauna_core::peer_anchor_rows::PeerAnchorRow::Head(fauna_core::data::PeerChainHead {
            actor: fauna_core::identity::ActorId([actor; 32]),
            recovery_pubkey: vec![key; 32],
            seq,
            first_seen: fauna_core::data::Timestamp(first_seen),
            outrun,
        })
    }

    fn an_anchor_domain(
        actor: u8,
        host: &str,
        first_seen: u64,
    ) -> fauna_core::peer_anchor_rows::PeerAnchorRow {
        fauna_core::peer_anchor_rows::PeerAnchorRow::Domain(fauna_core::data::PeerAnchorDomain {
            actor: fauna_core::identity::ActorId([actor; 32]),
            domain: host.to_string(),
            first_seen: fauna_core::data::Timestamp(first_seen),
        })
    }

    fn anchor_entry(row: &fauna_core::peer_anchor_rows::PeerAnchorRow) -> EntryPlaintext {
        crdt_entry(
            KIND_PEER_ANCHORS,
            &row.plane_key(),
            row.encode_row().unwrap(),
        )
    }

    fn merged_anchor_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_PEER_ANCHORS, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.peer-anchors` row): the frozen string, per-field
    /// CRDT, fleet-only, tip-sealed — and an ACCOUNT row on departure: the
    /// anchors serve every device's witness, whichever device wrote them.
    #[test]
    fn the_peer_anchors_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_PEER_ANCHORS, "fauna.state.peer-anchors");
        assert_eq!(
            merge_policy(KIND_PEER_ANCHORS),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_PEER_ANCHORS),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_PEER_ANCHORS),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_PEER_ANCHORS));
        assert!(!delegable_kinds().any(|k| k == KIND_PEER_ANCHORS));
        assert_eq!(
            TipSealedKind::of(KIND_PEER_ANCHORS).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    /// The join laws, ON BYTES through the plane arm, per family:
    /// commutative, associative, idempotent over every shape one actor's row
    /// takes — for a head, an advance, an equivocation at one height, the
    /// outrun mark on the same head and on a head an advance replaced, and
    /// differing first sightings; for a domain, two hosts and two sightings —
    /// and the monotonicity the kind exists for holds from either side: a
    /// behind replica never rewinds a head, and never re-marks an advanced one.
    #[test]
    fn peer_anchors_merge_is_a_join_on_bytes() {
        let families = [
            vec![
                an_anchor_head(1, 0x10, 3, 50, false),
                an_anchor_head(1, 0x10, 3, 40, true),
                an_anchor_head(1, 0x0f, 3, 60, false),
                an_anchor_head(1, 0x20, 5, 70, false),
                an_anchor_head(1, 0x20, 5, 90, true),
                an_anchor_head(1, 0x05, 1, 10, true),
            ],
            vec![
                an_anchor_domain(1, "b.example", 60),
                an_anchor_domain(1, "a.example", 80),
                an_anchor_domain(1, "c.example", 20),
            ],
        ];
        for samples in families {
            let key = samples[0].plane_key();
            let samples: Vec<Vec<u8>> = samples.iter().map(|r| r.encode_row().unwrap()).collect();
            let m = |a: &[u8], b: &[u8]| merged_anchor_row(&key, a, b);
            for a in &samples {
                assert_eq!(m(a, a), *a, "idempotent");
                for b in &samples {
                    assert_eq!(m(a, b), m(b, a), "commutative");
                    for c in &samples {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                    }
                }
            }
        }
        let advanced = an_anchor_head(1, 0x20, 5, 70, false).encode_row().unwrap();
        let stale_marked = an_anchor_head(1, 0x10, 3, 50, true).encode_row().unwrap();
        let key = an_anchor_head(1, 0, 0, 0, false).plane_key();
        let joined = merged_anchor_row(&key, &advanced, &stale_marked);
        assert_eq!(
            joined,
            an_anchor_head(1, 0x20, 5, 50, false).encode_row().unwrap(),
            "the advance holds, unmarked; only the earliest sighting moves"
        );
        assert_eq!(merged_anchor_row(&key, &stale_marked, &advanced), joined);
    }

    /// The arm is the unions' per-actor half, not a second rule: joining two
    /// devices' anchors through the shipped `PeerAnchors::merge` and
    /// splitting the result into rows gives exactly the rows the arm
    /// produces from the two sides' rows (below the ceiling, which is the
    /// read fold's, `fauna_core::peer_anchor_rows`).
    #[test]
    fn the_peer_anchors_arm_agrees_with_the_composite_merge() {
        use fauna_core::data::PeerAnchors;
        use fauna_core::peer_anchor_rows::PeerAnchorRow;
        let split = |rows: Vec<PeerAnchorRow>| {
            let mut a = PeerAnchors::default();
            for r in rows {
                match r {
                    PeerAnchorRow::Head(h) => a.chain_heads.push(h),
                    PeerAnchorRow::Domain(d) => a.anchor_domains.push(d),
                }
            }
            a
        };
        let a = split(vec![
            an_anchor_head(1, 0x10, 3, 50, false),
            an_anchor_head(2, 0x20, 1, 40, true),
            an_anchor_domain(1, "b.example", 60),
            an_anchor_domain(4, "d.example", 10),
        ]);
        let b = split(vec![
            an_anchor_head(1, 0x11, 5, 70, false),
            an_anchor_head(2, 0x20, 1, 30, false),
            an_anchor_head(3, 0x30, 7, 90, false),
            an_anchor_domain(1, "a.example", 80),
        ]);
        let want: Vec<(String, Vec<u8>)> = a
            .merge(&b)
            .rows()
            .into_iter()
            .map(|(k, r)| (k, r.encode_row().unwrap()))
            .collect();
        let mut got = std::collections::BTreeMap::<String, Vec<u8>>::new();
        for (k, r) in a.rows().into_iter().chain(b.rows()) {
            let v = r.encode_row().unwrap();
            let joined = match got.get(&k) {
                Some(cur) => merged_anchor_row(&k, cur, &v),
                None => v,
            };
            got.insert(k, joined);
        }
        assert_eq!(got.into_iter().collect::<Vec<_>>(), want);
    }

    /// A covered row — the incoming side adds nothing — is `KeepCurrent`, so
    /// a converged pair stops publishing.
    #[test]
    fn a_covered_peer_anchor_row_reports_keep_current() {
        for (current, incoming) in [
            (
                an_anchor_head(1, 0x20, 5, 40, true),
                an_anchor_head(1, 0x10, 3, 50, false),
            ),
            (
                an_anchor_domain(1, "a.example", 40),
                an_anchor_domain(1, "b.example", 50),
            ),
        ] {
            assert!(matches!(
                apply_class2(
                    MergePolicy::CrdtPerField,
                    Some(&anchor_entry(&current)),
                    &anchor_entry(&incoming),
                ),
                Ok(MergeOutcome::KeepCurrent)
            ));
        }
    }

    /// A tombstone, junk bytes, a key outside the grammar (`self`, a bare
    /// actor, upper case), a value filed under another actor's key or the
    /// other family's, and a value outside the writers' shape are all
    /// refused — at first contact, and against a good current row.
    #[test]
    fn a_peer_anchor_tombstone_junk_misfiled_or_misshapen_row_is_refused() {
        let good = an_anchor_head(0xab, 0x10, 3, 50, false);
        let mut dead = anchor_entry(&good);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let mut junk = anchor_entry(&good);
        junk.value = vec![0xff, 0x00].into();
        let mut self_key = anchor_entry(&good);
        self_key.key = "self".into();
        let mut bare = anchor_entry(&good);
        bare.key = bare.key.trim_start_matches("head/").to_string();
        let mut upper = anchor_entry(&good);
        upper.key = format!(
            "head/{}",
            upper.key.trim_start_matches("head/").to_uppercase()
        );
        let mut misfiled = anchor_entry(&good);
        misfiled.key = an_anchor_head(2, 0x10, 3, 50, false).plane_key();
        let mut other_family = anchor_entry(&good);
        other_family.key = an_anchor_domain(0xab, "a.example", 0).plane_key();
        let mut short = anchor_entry(&good);
        let fauna_core::peer_anchor_rows::PeerAnchorRow::Head(mut h) = good.clone() else {
            unreachable!()
        };
        h.recovery_pubkey.truncate(31);
        short.value = fauna_core::peer_anchor_rows::PeerAnchorRow::Head(h)
            .encode_row()
            .unwrap()
            .into();
        let long_host = anchor_entry(&an_anchor_domain(1, &"h".repeat(254), 0));
        for bad in [
            junk,
            self_key,
            bare,
            upper,
            misfiled,
            other_family,
            short,
            long_host,
        ] {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &bad),
                    Err(MergeError::BadValue { .. })
                ),
                "first contact must refuse the row at {:?}",
                bad.key
            );
            if bad.key == anchor_entry(&good).key {
                assert!(matches!(
                    apply_class2(MergePolicy::CrdtPerField, Some(&anchor_entry(&good)), &bad),
                    Err(MergeError::BadValue { .. })
                ));
            }
        }
    }

    /// **The decode-posture pin — strict on the plane, though the value types
    /// stay tolerant:** a row with a field from a build past this
    /// one answers `BadValue` (it re-presents on the next reconcile) instead
    /// of merging with the field silently stripped — and so does a
    /// non-canonical encoding of a known row, which would not re-encode to
    /// its own bytes.
    #[test]
    fn a_newer_peer_anchor_field_is_refused_not_stripped() {
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a fauna_core::data::PeerChainHead,
            from_the_future: u8,
        }
        let good = an_anchor_head(1, 0x10, 3, 50, false);
        let fauna_core::peer_anchor_rows::PeerAnchorRow::Head(inner) = &good else {
            unreachable!()
        };
        let current = anchor_entry(&good);
        let mut incoming = current.clone();
        incoming.value = fauna_core::encoding::canonical_encode(&Newer {
            record: inner,
            from_the_future: 1,
        })
        .unwrap()
        .to_vec()
        .into();
        for cur in [None, Some(&current)] {
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, cur, &incoming),
                Err(MergeError::BadValue { .. })
            ));
        }
        let canonical = good.encode_row().unwrap();
        assert_eq!(canonical[0] & 0xe0, 0xa0, "a short CBOR map header");
        let mut loose = vec![0xb8, canonical[0] & 0x1f];
        loose.extend_from_slice(&canonical[1..]);
        let mut non_canonical = current.clone();
        non_canonical.value = loose.into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &non_canonical),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per anchored actor is bounded by the entry's own
    /// shape — a head at its widest (every field set, the largest `seq` and
    /// stamp) and a domain at the hostname bound each seal under HALF the
    /// per-entry cap, however many peers the fleet anchors.
    #[test]
    fn a_widest_peer_anchor_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        let generation_sealed = matches!(
            sealing_epoch(KIND_PEER_ANCHORS),
            Some(SealingEpoch::GenerationTip)
        );
        let host = "d".repeat(fauna_core::web::MAX_HOSTNAME_BYTES);
        for row in [
            an_anchor_head(0xff, 0xff, u64::MAX, u64::MAX, true),
            an_anchor_domain(0xff, &host, u64::MAX),
        ] {
            let len = sealed_envelope_len(&anchor_entry(&row), generation_sealed).unwrap();
            assert!(
                len <= MAX_STATE_ENTRY_BYTES / 2,
                "a widest peer-anchor row seals to {len} bytes, over half the \
                 {MAX_STATE_ENTRY_BYTES}-byte cap"
            );
        }
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the actor id
    /// and the RecoveryKey ride as CBOR byte strings (major 2, length 32),
    /// never serde's default integer array.
    #[test]
    fn the_peer_anchor_ids_and_keys_are_cbor_byte_strings() {
        for (row, fields) in [
            (
                an_anchor_head(0x11, 0x22, 3, 50, true),
                vec![[0x11; 32], [0x22; 32]],
            ),
            (an_anchor_domain(0x33, "a.example", 50), vec![[0x33; 32]]),
        ] {
            let bytes = row.encode_row().unwrap();
            for field in fields {
                let mut want = vec![0x58, 0x20];
                want.extend_from_slice(&field);
                assert!(
                    bytes.windows(want.len()).any(|w| w == want.as_slice()),
                    "every 32-byte field must encode as a CBOR byte string"
                );
            }
        }
    }

    // ── `fauna.state.blessed-nests` (the user's per-nest blessing verdicts) ──

    fn a_blessing(nest: u8, blessed: bool, at: u64) -> fauna_core::data::BlessedNest {
        fauna_core::data::BlessedNest {
            nest_id: vec![nest; 32],
            blessed,
            at,
        }
    }

    fn blessing_entry(row: &fauna_core::data::BlessedNest) -> EntryPlaintext {
        crdt_entry(
            KIND_BLESSED_NESTS,
            &row.plane_key(),
            row.encode_row().unwrap(),
        )
    }

    fn merged_blessing_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_BLESSED_NESTS, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.blessed-nests` row): the frozen string, per-field
    /// CRDT, fleet-only, tip-sealed — and an ACCOUNT row on departure: a
    /// verdict given on any device is the user's, whichever device wrote it.
    #[test]
    fn the_blessed_nests_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_BLESSED_NESTS, "fauna.state.blessed-nests");
        assert_eq!(
            merge_policy(KIND_BLESSED_NESTS),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_BLESSED_NESTS),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_BLESSED_NESTS),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_BLESSED_NESTS));
        assert!(!delegable_kinds().any(|k| k == KIND_BLESSED_NESTS));
        assert_eq!(
            TipSealedKind::of(KIND_BLESSED_NESTS).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    /// The join laws, ON BYTES through the plane arm: commutative,
    /// associative, idempotent over every shape one nest's row takes — both
    /// verdicts at one stamp (the tie, which must un-bless from either
    /// side), a newer bless over an older un-bless and the reverse — and the
    /// rule's two halves hold from either side: the newer verdict wins, and
    /// an equal stamp goes to the un-blessed side.
    #[test]
    fn blessed_nests_merge_is_a_join_on_bytes() {
        let samples = [
            a_blessing(1, true, 100),
            a_blessing(1, false, 100),
            a_blessing(1, true, 120),
            a_blessing(1, false, 90),
            a_blessing(1, false, 120),
            a_blessing(1, true, 0),
        ];
        let key = samples[0].plane_key();
        let samples: Vec<Vec<u8>> = samples.iter().map(|r| r.encode_row().unwrap()).collect();
        let m = |a: &[u8], b: &[u8]| merged_blessing_row(&key, a, b);
        for a in &samples {
            assert_eq!(m(a, a), *a, "idempotent");
            for b in &samples {
                assert_eq!(m(a, b), m(b, a), "commutative");
                for c in &samples {
                    assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                }
            }
        }
        let blessed = a_blessing(1, true, 100).encode_row().unwrap();
        let unblessed = a_blessing(1, false, 100).encode_row().unwrap();
        assert_eq!(m(&blessed, &unblessed), unblessed, "a tie un-blesses");
        assert_eq!(m(&unblessed, &blessed), unblessed, "from either side");
        let newer = a_blessing(1, true, 101).encode_row().unwrap();
        assert_eq!(m(&unblessed, &newer), newer, "the newer verdict wins");
    }

    /// The arm is `merge_blessed_nests`' per-nest half, not a second rule:
    /// merging two devices' lists through the shipped function and splitting
    /// the result into rows gives exactly the rows the arm produces from the
    /// two sides' rows.
    #[test]
    fn the_blessed_nests_arm_agrees_with_the_composite_merge() {
        use fauna_core::blessed_nest_rows::blessed_nest_rows;
        use fauna_core::data::merge_blessed_nests;
        let a = vec![
            a_blessing(1, true, 100),
            a_blessing(2, true, 50),
            a_blessing(3, false, 90),
            a_blessing(4, true, 10),
        ];
        let b = vec![
            a_blessing(1, false, 120),
            a_blessing(2, false, 50),
            a_blessing(3, true, 80),
            a_blessing(5, false, 7),
        ];
        let want: Vec<(String, Vec<u8>)> = blessed_nest_rows(&merge_blessed_nests(&a, &b))
            .into_iter()
            .map(|(k, r)| (k, r.encode_row().unwrap()))
            .collect();
        let mut got = std::collections::BTreeMap::<String, Vec<u8>>::new();
        for (k, r) in blessed_nest_rows(&a)
            .into_iter()
            .chain(blessed_nest_rows(&b))
        {
            let v = r.encode_row().unwrap();
            let joined = match got.get(&k) {
                Some(cur) => merged_blessing_row(&k, cur, &v),
                None => v,
            };
            got.insert(k, joined);
        }
        assert_eq!(got.into_iter().collect::<Vec<_>>(), want);
    }

    /// A covered row — the incoming side adds nothing — is `KeepCurrent`, so
    /// a converged pair stops publishing.
    #[test]
    fn a_covered_blessed_nest_row_reports_keep_current() {
        for (current, incoming) in [
            (a_blessing(1, true, 120), a_blessing(1, false, 100)),
            (a_blessing(1, false, 100), a_blessing(1, true, 100)),
        ] {
            assert!(matches!(
                apply_class2(
                    MergePolicy::CrdtPerField,
                    Some(&blessing_entry(&current)),
                    &blessing_entry(&incoming),
                ),
                Ok(MergeOutcome::KeepCurrent)
            ));
        }
    }

    /// A tombstone, junk bytes, a key outside the grammar (`self`, upper
    /// case, a prefixed id), a value filed under another nest's key, and a
    /// nest id that is not 32 bytes are all refused — at first contact, and
    /// against a good current row.
    #[test]
    fn a_blessed_nest_tombstone_junk_misfiled_or_misshapen_row_is_refused() {
        let good = a_blessing(0xab, true, 100);
        let mut dead = blessing_entry(&good);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let mut junk = blessing_entry(&good);
        junk.value = vec![0xff, 0x00].into();
        let mut self_key = blessing_entry(&good);
        self_key.key = "self".into();
        let mut upper = blessing_entry(&good);
        upper.key = upper.key.to_uppercase();
        let mut prefixed = blessing_entry(&good);
        prefixed.key = format!("nest/{}", prefixed.key);
        let mut misfiled = blessing_entry(&good);
        misfiled.key = a_blessing(2, true, 100).plane_key();
        let mut short = blessing_entry(&good);
        let mut s = good.clone();
        s.nest_id.truncate(31);
        short.value = s.encode_row().unwrap().into();
        for bad in [junk, self_key, upper, prefixed, misfiled, short] {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &bad),
                    Err(MergeError::BadValue { .. })
                ),
                "first contact must refuse the row at {:?}",
                bad.key
            );
            if bad.key == blessing_entry(&good).key {
                assert!(matches!(
                    apply_class2(
                        MergePolicy::CrdtPerField,
                        Some(&blessing_entry(&good)),
                        &bad
                    ),
                    Err(MergeError::BadValue { .. })
                ));
            }
        }
    }

    /// **The decode-posture pin — strict on the plane, though the value type
    /// stays tolerant:** a row with a field from a build past
    /// this one answers `BadValue` (it re-presents on the next reconcile)
    /// instead of merging with the field silently stripped — and so does a
    /// non-canonical encoding of a known row, which would not re-encode to
    /// its own bytes.
    #[test]
    fn a_newer_blessed_nest_field_is_refused_not_stripped() {
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a fauna_core::data::BlessedNest,
            from_the_future: u8,
        }
        let good = a_blessing(1, true, 100);
        let current = blessing_entry(&good);
        let mut incoming = current.clone();
        incoming.value = fauna_core::encoding::canonical_encode(&Newer {
            record: &good,
            from_the_future: 1,
        })
        .unwrap()
        .to_vec()
        .into();
        for cur in [None, Some(&current)] {
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, cur, &incoming),
                Err(MergeError::BadValue { .. })
            ));
        }
        let canonical = good.encode_row().unwrap();
        assert_eq!(canonical[0] & 0xe0, 0xa0, "a short CBOR map header");
        let mut loose = vec![0xb8, canonical[0] & 0x1f];
        loose.extend_from_slice(&canonical[1..]);
        let mut non_canonical = current.clone();
        non_canonical.value = loose.into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &non_canonical),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** one row per nest is bounded by the entry's own shape — a
    /// row at its widest (the largest stamp) seals under HALF the per-entry
    /// cap, however many nests the user ever blessed.
    #[test]
    fn a_widest_blessed_nest_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        let generation_sealed = matches!(
            sealing_epoch(KIND_BLESSED_NESTS),
            Some(SealingEpoch::GenerationTip)
        );
        let widest = blessing_entry(&a_blessing(0xff, false, u64::MAX));
        let len = sealed_envelope_len(&widest, generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES / 2,
            "a widest blessed-nest row seals to {len} bytes, over half the \
             {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    /// **Bytes are byte strings** (the *Bounded rows* rider): the nest id
    /// rides as a CBOR byte string (major 2, length 32), never serde's
    /// default integer array.
    #[test]
    fn the_blessed_nest_id_is_a_cbor_byte_string() {
        let bytes = a_blessing(0x33, true, 50).encode_row().unwrap();
        let mut want = vec![0x58, 0x20];
        want.extend_from_slice(&[0x33; 32]);
        assert!(
            bytes.windows(want.len()).any(|w| w == want.as_slice()),
            "the 32-byte nest id must encode as a CBOR byte string"
        );
    }

    /// The join laws, ON BYTES through the plane arm, per record type:
    /// commutative, associative, idempotent — including a tie on the
    /// record's own `updated_at`, where a keep-mine remainder would leave two
    /// replicas holding different bytes for ever.
    #[test]
    fn group_share_ceremony_merge_is_a_join_on_bytes() {
        let enc_i = |r: &fauna_core::group_ceremony::InitiatedGroupShare| {
            fauna_core::encoding::canonical_encode(r).unwrap().to_vec()
        };
        let enc_v = |r: &fauna_core::group_ceremony::InvitedGroupShare| {
            fauna_core::encoding::canonical_encode(r).unwrap().to_vec()
        };
        let initiated = [
            initiated_record(1, 10, 3, b"", false),
            initiated_record(1, 20, 4, b"dlv-b", true),
            initiated_record(1, 20, 7, b"dlv-a", false),
            initiated_record(1, 5, 1, b"", false),
        ];
        assert_join_on_bytes(
            &initiated[0].plane_key(),
            &initiated.iter().map(enc_i).collect::<Vec<_>>(),
        );
        let invited = [
            invited_record(2, 10, 3, false),
            invited_record(2, 20, 4, false),
            invited_record(2, 20, 7, true),
            invited_record(2, 5, 1, false),
        ];
        assert_join_on_bytes(
            &invited[0].plane_key(),
            &invited.iter().map(enc_v).collect::<Vec<_>>(),
        );
        // And it is the union the rule promises: the OR'd markers, the
        // byte-smaller non-empty deliver, the higher stamp's remainder.
        let key = initiated[0].plane_key();
        let joined: fauna_core::group_ceremony::InitiatedGroupShare =
            fauna_core::encoding::canonical_decode(&merged_group_share_row(
                &key,
                &enc_i(&initiated[1]),
                &enc_i(&initiated[2]),
            ))
            .unwrap();
        assert!(joined.delivered);
        assert_eq!(joined.deliver, b"dlv-a");
        assert_eq!(joined.offered_at, fauna_core::data::Timestamp(7));
    }

    /// The arm is the fold's per-record half, not a second rule: joining two
    /// composite records and splitting the result into rows gives exactly
    /// the rows the arm produces from the two sides' rows.
    #[test]
    fn the_group_share_ceremony_arm_agrees_with_the_composite_fold() {
        use fauna_core::group_ceremony::GroupShareConfig;
        let a = GroupShareConfig {
            initiated: vec![initiated_record(1, 10, 3, b"", false)],
            invited: vec![invited_record(2, 20, 4, false)],
        };
        let b = GroupShareConfig {
            initiated: vec![
                initiated_record(1, 20, 4, b"dlv", true),
                initiated_record(3, 5, 1, b"", false),
            ],
            invited: vec![invited_record(2, 10, 3, true)],
        };
        let composite: Vec<(String, Vec<u8>)> = a
            .merge(&b)
            .rows()
            .into_iter()
            .map(|(k, r)| (k, r.encode().unwrap()))
            .collect();
        let mut by_arm: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
        for (k, r) in a.rows().into_iter().chain(b.rows()) {
            let v = r.encode().unwrap();
            let next = match by_arm.get(&k) {
                Some(cur) => merged_group_share_row(&k, cur, &v),
                None => v,
            };
            by_arm.insert(k, next);
        }
        let mut composite_sorted = composite.clone();
        composite_sorted.sort();
        assert_eq!(by_arm.into_iter().collect::<Vec<_>>(), composite_sorted);
        assert_eq!(composite.len(), 3);
    }

    /// The echo-stop: a value the current one already covers changes nothing.
    #[test]
    fn a_covered_group_share_ceremony_reports_keep_current() {
        let current = initiated_record(1, 20, 4, b"dlv", true);
        let older = initiated_record(1, 10, 3, b"", false);
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&initiated_entry(&current)),
                &initiated_entry(&older),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// No deletion exists for a record: a tombstone is refused at merge and
    /// at first contact alike, and so is a value that will not decode, a key
    /// outside the grammar, a value filed under another ceremony's key, and
    /// a record of the other side's type.
    #[test]
    fn a_group_share_ceremony_tombstone_junk_or_misfiled_row_is_refused() {
        let rec = initiated_record(1, 10, 3, b"", false);
        let mut dead = initiated_entry(&rec);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let bad = |e: EntryPlaintext| {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &e),
                    Err(MergeError::BadValue { .. })
                ),
                "{} must be refused",
                e.key
            );
        };
        let mut junk = initiated_entry(&rec);
        junk.value = vec![0xff, 0x00].into();
        bad(junk);
        let mut self_key = initiated_entry(&rec);
        self_key.key = "self".into();
        bad(self_key);
        let mut misfiled = initiated_entry(&rec);
        misfiled.key = initiated_record(2, 10, 3, b"", false).plane_key();
        bad(misfiled);
        let mut upper = initiated_entry(&rec);
        upper.key = upper.key.to_uppercase().replace("INITIATED/", "initiated/");
        bad(upper);
        let mut wrong_type = invited_entry(&invited_record(1, 10, 3, false));
        wrong_type.key = format!(
            "initiated/{}/{}",
            fauna_core::hex32::encode(&[1; 32]),
            fauna_core::hex32::encode(&[0xB0; 32])
        );
        bad(wrong_type);
    }

    /// The P4 decode posture — `deny_unknown_fields`, on both record types:
    /// a record carrying a field from a build past this one answers
    /// `BadValue` (it re-presents on the next reconcile) instead of merging
    /// with the field silently stripped from the re-encoded record.
    #[test]
    fn a_newer_group_share_ceremony_field_is_refused_not_stripped() {
        /// `record` with one field this build does not know.
        #[derive(serde::Serialize)]
        struct Newer<'a, T> {
            #[serde(flatten)]
            record: &'a T,
            from_the_future: u8,
        }
        fn with_extra_field<T: serde::Serialize>(record: &T) -> Vec<u8> {
            fauna_core::encoding::canonical_encode(&Newer {
                record,
                from_the_future: 1,
            })
            .unwrap()
            .to_vec()
        }
        let init = initiated_record(1, 10, 3, b"", false);
        let inv = invited_record(2, 10, 3, false);
        for (current, mut newer) in [
            (initiated_entry(&init), initiated_entry(&init)),
            (invited_entry(&inv), invited_entry(&inv)),
        ] {
            newer.value = if newer.key.starts_with("initiated/") {
                with_extra_field(&init)
            } else {
                with_extra_field(&inv)
            }
            .into();
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, None, &newer),
                Err(MergeError::BadValue { .. })
            ));
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, Some(&current), &newer),
                Err(MergeError::BadValue { .. })
            ));
        }
    }

    // ── The backup-destination state (`backup-destinations.md`) ──

    /// The source box every list row in these tests is filed under.
    const BACKUP_BOX: [u8; 32] = [0xA1; 32];

    fn backup_list_key() -> String {
        fauna_core::backup_state::destinations_key(&BACKUP_BOX)
    }

    fn backup_dest(id: &str) -> fauna_core::data::BackupDestination {
        fauna_core::data::BackupDestination {
            destination_id: id.to_string(),
            destination_nest_url: format!("https://{id}.example"),
            folder_name: "__mail".to_string(),
            ..Default::default()
        }
    }

    fn backup_mark(
        id: &str,
        pred: u8,
        verdict: fauna_core::data::UnattestedVerdict,
    ) -> fauna_core::data::DestinationUnattestedMark {
        fauna_core::data::DestinationUnattestedMark {
            destination_id: id.to_string(),
            predecessor: fauna_core::identity::ActorId([pred; 32]),
            verdict,
        }
    }

    fn destinations_row(ids: &[&str], at: u64) -> Vec<u8> {
        fauna_core::encoding::canonical_encode(&fauna_core::backup_state::BackupDestinationsRow {
            source_nest: BACKUP_BOX,
            backup: fauna_core::data::BackupConfig {
                destinations: ids.iter().map(|i| backup_dest(i)).collect(),
            },
            updated_at: fauna_core::data::Timestamp(at),
        })
        .unwrap()
    }

    fn merged_backup_row(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_BACKUP, key, v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.backup` row): the frozen string, per-field CRDT,
    /// fleet-only, tip-sealed — and an ACCOUNT row on departure.
    #[test]
    fn the_backup_state_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(KIND_BACKUP, "fauna.state.backup");
        assert_eq!(merge_policy(KIND_BACKUP), Some(MergePolicy::CrdtPerField));
        assert_eq!(audience_rung(KIND_BACKUP), Some(AudienceRung::FleetOnly));
        assert_eq!(
            sealing_epoch(KIND_BACKUP),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_BACKUP));
        assert!(!delegable_kinds().any(|k| k == KIND_BACKUP));
    }

    /// The join laws, ON BYTES through the plane arm, per row family — a tie
    /// on the list's own stamp included, and a newer build's verdict string.
    #[test]
    fn backup_state_merge_is_a_join_on_bytes() {
        use fauna_core::data::UnattestedVerdict as V;
        let lists = [
            destinations_row(&["a", "b"], 10),
            destinations_row(&["c"], 10),
            destinations_row(&["b"], 20),
            destinations_row(&[], 5),
        ];
        let marks: Vec<Vec<u8>> = [
            V::Open,
            V::Kept,
            V::Removed,
            V::Other("later".into()),
            V::Other("earlier".into()),
        ]
        .into_iter()
        .map(|v| fauna_core::encoding::canonical_encode(&backup_mark("a", 1, v)).unwrap())
        .collect();
        let mark_key = backup_mark("a", 1, V::Open).plane_key();
        let list_key = backup_list_key();
        for (key, samples) in [
            (list_key.as_str(), &lists[..]),
            (mark_key.as_str(), &marks[..]),
        ] {
            let m = |a: &[u8], b: &[u8]| merged_backup_row(key, a, b);
            for a in samples {
                assert_eq!(m(a, a), *a, "idempotent");
                for b in samples {
                    assert_eq!(m(a, b), m(b, a), "commutative");
                    for c in samples {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative");
                    }
                }
            }
        }
        // The rule it promises: the higher stamp's list; the restrictive
        // verdict.
        assert_eq!(merged_backup_row(&list_key, &lists[0], &lists[2]), lists[2]);
        assert_eq!(merged_backup_row(&mark_key, &marks[1], &marks[2]), marks[2]);
    }

    /// The arms are the composite's per-row halves: two replicas' rows,
    /// joined row by row through the arm and read through the fold, give
    /// exactly the state `BackupState::merge` gives.
    #[test]
    fn the_backup_state_arms_agree_with_the_composite_merge() {
        use fauna_core::backup_state::BackupState;
        use fauna_core::data::UnattestedVerdict as V;
        let state = |ids: &[&str], marks, at| BackupState {
            backup: fauna_core::data::BackupConfig {
                destinations: ids.iter().map(|i| backup_dest(i)).collect(),
            },
            marks,
            updated_at: fauna_core::data::Timestamp(at),
        };
        let a = state(
            &["a", "b"],
            vec![backup_mark("a", 1, V::Open), backup_mark("b", 2, V::Kept)],
            20,
        );
        let b = state(&["b", "c"], vec![backup_mark("a", 1, V::Removed)], 10);
        let mut by_arm: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
        for (k, r) in a.rows(&BACKUP_BOX).into_iter().chain(b.rows(&BACKUP_BOX)) {
            let v = r.encode().unwrap();
            let next = match by_arm.get(&k) {
                Some(cur) => merged_backup_row(&k, cur, &v),
                None => v,
            };
            by_arm.insert(k, next);
        }
        let folded = BackupState::from_rows(
            by_arm.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            &BACKUP_BOX,
        )
        .unwrap();
        assert_eq!(folded, a.merge(&b));
        let ids: Vec<_> = folded
            .backup
            .destinations
            .iter()
            .map(|d| d.destination_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["b"],
            "the newer list, with the removed destination pruned"
        );
    }

    /// The echo-stop: a value the current one already covers changes nothing.
    #[test]
    fn a_covered_backup_row_reports_keep_current() {
        let entry = |v: Vec<u8>| crdt_entry(KIND_BACKUP, &backup_list_key(), v);
        assert_eq!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&entry(destinations_row(&["a"], 20))),
                &entry(destinations_row(&["b"], 10)),
            )
            .unwrap(),
            MergeOutcome::KeepCurrent
        );
    }

    /// No deletion exists for a row: a tombstone is refused at merge and at
    /// first contact alike, and so is junk, a key outside the grammar (the
    /// retired default `self` and phase A's account-wide `destinations`
    /// included), a mark filed under another mark's key, a mark under a
    /// list's key, and a list filed under another box's key.
    #[test]
    fn a_backup_tombstone_junk_or_misfiled_row_is_refused() {
        use fauna_core::data::UnattestedVerdict as V;
        let m = backup_mark("a", 1, V::Open);
        let good = || {
            crdt_entry(
                KIND_BACKUP,
                &m.plane_key(),
                fauna_core::encoding::canonical_encode(&m).unwrap(),
            )
        };
        let mut dead = good();
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let bad = |e: EntryPlaintext| {
            assert!(
                matches!(
                    apply_class2(MergePolicy::CrdtPerField, None, &e),
                    Err(MergeError::BadValue { .. })
                ),
                "{} must be refused",
                e.key
            );
        };
        let mut junk = good();
        junk.value = vec![0xff, 0x00].into();
        bad(junk);
        let mut self_key = good();
        self_key.key = "self".into();
        bad(self_key);
        let mut misfiled = good();
        misfiled.key = backup_mark("b", 1, V::Open).plane_key();
        bad(misfiled);
        let mut wrong_type = good();
        wrong_type.key = backup_list_key();
        bad(wrong_type);
        bad(crdt_entry(
            KIND_BACKUP,
            "destinations",
            destinations_row(&["a"], 1),
        ));
        bad(crdt_entry(
            KIND_BACKUP,
            &fauna_core::backup_state::destinations_key(&[0xB2; 32]),
            destinations_row(&["a"], 1),
        ));
        assert!(apply_class2(MergePolicy::CrdtPerField, None, &good()).is_ok());
        assert!(
            apply_class2(
                MergePolicy::CrdtPerField,
                None,
                &crdt_entry(KIND_BACKUP, &backup_list_key(), destinations_row(&["a"], 1)),
            )
            .is_ok()
        );
    }

    /// The P4 decode posture — strict at every depth: a destination carrying
    /// a field from a build past this one answers `BadValue` (it re-presents
    /// on the next reconcile) instead of merging with the field stripped.
    #[test]
    fn a_newer_backup_field_is_refused_not_stripped() {
        #[derive(serde::Serialize)]
        struct NewerDest<'a> {
            #[serde(flatten)]
            base: &'a fauna_core::data::BackupDestination,
            from_the_future: u8,
        }
        #[derive(serde::Serialize)]
        struct NewerConfig<'a> {
            destinations: Vec<NewerDest<'a>>,
        }
        #[derive(serde::Serialize)]
        struct NewerRow<'a> {
            #[serde(with = "serde_bytes")]
            source_nest: [u8; 32],
            backup: NewerConfig<'a>,
            updated_at: fauna_core::data::Timestamp,
        }
        let d = backup_dest("a");
        let newer = crdt_entry(
            KIND_BACKUP,
            &backup_list_key(),
            fauna_core::encoding::canonical_encode(&NewerRow {
                source_nest: BACKUP_BOX,
                backup: NewerConfig {
                    destinations: vec![NewerDest {
                        base: &d,
                        from_the_future: 1,
                    }],
                },
                updated_at: fauna_core::data::Timestamp(1),
            })
            .unwrap(),
        );
        let current = crdt_entry(KIND_BACKUP, &backup_list_key(), destinations_row(&["a"], 1));
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &newer),
            Err(MergeError::BadValue { .. })
        ));
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, Some(&current), &newer),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// The size pin (*Bounded rows*): the largest destination-list row the
    /// bounds admit — at-cap entries, every text field at its cap and every
    /// optional field present, as many as fit under
    /// [`fauna_core::backup_state::MAX_BACKUP_DESTINATIONS_VALUE_BYTES`],
    /// its value then counted AT that ceiling — and a mark row with the
    /// longest destination id both seal, generation-sealed as the kind is,
    /// under HALF the per-entry cap the writer door enforces.
    #[test]
    fn a_backup_row_at_its_ceiling_seals_under_half_the_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::backup_state::*;
        let full = |i: usize| {
            let id = format!("{i:0>width$}", width = MAX_DESTINATION_ID_BYTES);
            fauna_core::data::BackupDestination {
                destination_id: id,
                destination_nest_url: "u".repeat(MAX_DESTINATION_URL_BYTES),
                destination_actor_pubkey: [0xEE; 32],
                folder_name: "f".repeat(MAX_DESTINATION_FOLDER_BYTES),
                added_at: u64::MAX,
                display_name: Some("n".repeat(MAX_DESTINATION_LABEL_BYTES)),
                kind: "k".repeat(MAX_DESTINATION_KIND_BYTES),
                custodian_device_id: Some("c".repeat(MAX_CUSTODIAN_DEVICE_ID_BYTES)),
                capacity_cap_bytes: Some(u64::MAX),
            }
        };
        let row_of = |n: usize| BackupDestinationsRow {
            source_nest: [0xAB; 32],
            backup: fauna_core::data::BackupConfig {
                destinations: (0..n).map(full).collect(),
            },
            updated_at: fauna_core::data::Timestamp(u64::MAX),
        };
        let n = (1..)
            .find(|n| row_of(*n).check_bounds().is_err())
            .expect("the byte ceiling binds")
            - 1;
        let row = row_of(n);
        row.check_bounds()
            .expect("the pinned row is inside the bounds");
        let mark = fauna_core::data::DestinationUnattestedMark {
            destination_id: "i".repeat(MAX_DESTINATION_ID_BYTES),
            predecessor: fauna_core::identity::ActorId([0xDD; 32]),
            verdict: fauna_core::data::UnattestedVerdict::Other("x".repeat(64)),
        };
        let generation_sealed = matches!(
            sealing_epoch(KIND_BACKUP),
            Some(SealingEpoch::GenerationTip)
        );
        let list_value = fauna_core::encoding::canonical_encode(&row).unwrap();
        // The envelope's overhead is fixed across one CBOR length class, so a
        // value AT the ceiling seals to this row's sealed length plus the
        // bytes it falls short by.
        let slack = MAX_BACKUP_DESTINATIONS_VALUE_BYTES - list_value.len();
        assert!(list_value.len() > 255 && MAX_BACKUP_DESTINATIONS_VALUE_BYTES < 65_536);
        for (key, value, pad) in [
            (destinations_key(&row.source_nest), list_value, slack),
            (
                mark.plane_key(),
                fauna_core::encoding::canonical_encode(&mark).unwrap(),
                0,
            ),
        ] {
            let sealed =
                sealed_envelope_len(&crdt_entry(KIND_BACKUP, &key, value), generation_sealed)
                    .unwrap()
                    + pad;
            eprintln!("size pin: {key} seals to {sealed} B");
            assert!(
                sealed <= MAX_STATE_ENTRY_BYTES / 2,
                "{key} seals to {sealed} B, over half the {MAX_STATE_ENTRY_BYTES} B cap"
            );
        }
        eprintln!("size pin: {n} at-cap entries fit under the ceiling");
    }

    // ── The succession ledger (`config-dissolution.md` → *The ledger*) ──

    mod succession_ledger {
        use super::*;
        use fauna_core::data::{
            FilterUnattestedMark, GrantUnattestedMark, MemberUnattestedItem,
            MemberUnattestedReason, UnattestedVerdict,
        };
        use fauna_core::grant_event::{GRANT_EVENT_SIGNATURE_LEN, GrantEvent, GrantEventKind};
        use fauna_core::identity::{ActorId, ActorKeypair};
        use fauna_core::succession_ledger::{
            CHAIN_KEY, ChainState, SuccessionLedger, SuccessionLedgerRecord,
        };

        fn kp(seed: u8) -> ActorKeypair {
            ActorKeypair::from_secret([seed; 32])
        }

        fn mint(kp: &ActorKeypair, grant: u8, at: u64) -> GrantEvent {
            GrantEvent {
                grant_id: vec![grant; 16],
                holder: vec![0xAA; 32],
                kind: GrantEventKind::Mint,
                scope: vec![],
                window_start: at,
                window_end: at + 1000,
                at,
                sig: vec![0u8; GRANT_EVENT_SIGNATURE_LEN],
            }
            .sign(kp.signing_key())
            .unwrap()
        }

        fn row(r: &SuccessionLedgerRecord) -> (String, Vec<u8>) {
            (r.key().unwrap().render(), r.encode().unwrap())
        }

        fn entry(key: &str, value: &[u8]) -> EntryPlaintext {
            crdt_entry(KIND_SUCCESSION_LEDGER, key, value.to_vec())
        }

        fn merged(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
            match apply_class2(
                MergePolicy::CrdtPerField,
                Some(&entry(key, a)),
                &entry(key, b),
            )
            .unwrap()
            {
                MergeOutcome::Merged(e) => e.value.to_vec(),
                MergeOutcome::KeepCurrent => a.to_vec(),
                other => panic!("expected a merge, got {other:?}"),
            }
        }

        fn assert_join_on_bytes(key: &str, samples: &[Vec<u8>]) {
            let m = |a: &[u8], b: &[u8]| merged(key, a, b);
            for a in samples {
                assert_eq!(m(a, a), *a, "idempotent at {key}");
                for b in samples {
                    assert_eq!(m(a, b), m(b, a), "commutative at {key}");
                    for c in samples {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative at {key}");
                    }
                }
            }
        }

        fn refused(e: &EntryPlaintext, current: Option<&EntryPlaintext>) -> bool {
            matches!(
                apply_class2(MergePolicy::CrdtPerField, current, e),
                Err(MergeError::BadValue { .. })
            )
        }

        const VERDICTS: [fn() -> UnattestedVerdict; 5] = [
            || UnattestedVerdict::Open,
            || UnattestedVerdict::Kept,
            || UnattestedVerdict::Removed,
            || UnattestedVerdict::Other("a-newer-verdict".into()),
            || UnattestedVerdict::Other("b-newer-verdict".into()),
        ];

        fn grant_mark(v: UnattestedVerdict) -> SuccessionLedgerRecord {
            SuccessionLedgerRecord::GrantMark(GrantUnattestedMark {
                grant_id: vec![1; 16],
                predecessor: ActorId([2; 32]),
                verdict: v,
            })
        }

        fn member_item(v: UnattestedVerdict) -> SuccessionLedgerRecord {
            SuccessionLedgerRecord::MemberItem(MemberUnattestedItem {
                person: ActorId([7; 32]),
                predecessor: ActorId([2; 32]),
                reason: MemberUnattestedReason::CompromiseWindow,
                verdict: v,
            })
        }

        fn filter_mark(v: UnattestedVerdict) -> SuccessionLedgerRecord {
            SuccessionLedgerRecord::FilterMark(FilterUnattestedMark {
                filter_id: 42,
                predecessor: ActorId([2; 32]),
                verdict: v,
            })
        }

        /// The registration, held whole (`config-dissolution.md` — the kinds
        /// table's `fauna.state.succession-ledger` row): the frozen string,
        /// per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
        /// departure.
        #[test]
        fn the_succession_ledger_registers_as_a_fleet_only_tip_sealed_crdt() {
            assert_eq!(KIND_SUCCESSION_LEDGER, "fauna.state.succession-ledger");
            assert_eq!(
                merge_policy(KIND_SUCCESSION_LEDGER),
                Some(MergePolicy::CrdtPerField)
            );
            assert_eq!(
                audience_rung(KIND_SUCCESSION_LEDGER),
                Some(AudienceRung::FleetOnly)
            );
            assert_eq!(
                sealing_epoch(KIND_SUCCESSION_LEDGER),
                Some(SealingEpoch::GenerationTip)
            );
            assert_eq!(
                TipSealedKind::of(KIND_SUCCESSION_LEDGER).map(TipSealedKind::scope),
                Some(TipRowScope::Account)
            );
            assert!(!retired_with_its_writer(KIND_SUCCESSION_LEDGER));
            assert!(!delegable_kinds().any(|k| k == KIND_SUCCESSION_LEDGER));
        }

        /// The join laws, ON BYTES through the plane arm, per row type: the
        /// chain's gated union (every sample one owner chain), the mark rows'
        /// verdict minimum over all five verdict shapes (two differing
        /// unnameable ones included), the event row's write-once idempotence.
        #[test]
        fn succession_ledger_merge_is_a_join_on_bytes() {
            let (s, p, q) = (
                ActorId([0x50; 32]),
                ActorId([0x10; 32]),
                ActorId([0x20; 32]),
            );
            let chains: Vec<Vec<u8>> = [vec![], vec![p], vec![q], vec![p, q]]
                .into_iter()
                .map(|prior| {
                    row(&SuccessionLedgerRecord::Chain(ChainState {
                        actor_id: s,
                        prior_actor_ids: prior,
                    }))
                    .1
                })
                .collect();
            assert_join_on_bytes(CHAIN_KEY, &chains);
            for make in [grant_mark, member_item, filter_mark] {
                let samples: Vec<_> = VERDICTS.iter().map(|v| row(&make(v())).1).collect();
                assert_join_on_bytes(&row(&make(UnattestedVerdict::Open)).0, &samples);
            }
            let (key, event) = row(&SuccessionLedgerRecord::Event(mint(&kp(1), 1, 10)));
            assert_join_on_bytes(&key, &[event]);

            // The successor's id survives either direction, and the chain
            // keeps the predecessor.
            let pred = row(&SuccessionLedgerRecord::Chain(ChainState {
                actor_id: p,
                prior_actor_ids: vec![q],
            }))
            .1;
            let succ = row(&SuccessionLedgerRecord::Chain(ChainState {
                actor_id: s,
                prior_actor_ids: vec![p],
            }))
            .1;
            let joined = merged(CHAIN_KEY, &pred, &succ);
            assert_eq!(joined, merged(CHAIN_KEY, &succ, &pred));
            let chain: ChainState = fauna_core::encoding::canonical_decode(&joined).unwrap();
            assert_eq!(chain.actor_id, s);
            assert_eq!(chain.prior_actor_ids, vec![p, q]);
            // And a decided mark beats an open one whichever side holds it.
            let (key, open) = row(&grant_mark(UnattestedVerdict::Open));
            let kept = row(&grant_mark(UnattestedVerdict::Kept)).1;
            assert_eq!(merged(&key, &open, &kept), kept);
        }

        /// The arm is the ledger join's per-row half, not a second rule:
        /// joining two whole ledgers and splitting the result into rows gives
        /// exactly the rows the arm produces from the two sides' rows.
        #[test]
        fn the_succession_ledger_arm_agrees_with_the_composite_merge() {
            let (me, pred) = (kp(1), kp(2));
            let a = SuccessionLedger {
                actor_id: me.actor_id(),
                prior_actor_ids: vec![pred.actor_id()],
                grant_events: vec![mint(&me, 1, 10), mint(&pred, 2, 5)],
                unattested_grant_marks: vec![GrantUnattestedMark {
                    grant_id: vec![2; 16],
                    predecessor: pred.actor_id(),
                    verdict: UnattestedVerdict::Open,
                }],
                unattested_member_items: vec![],
                unattested_filter_marks: vec![FilterUnattestedMark {
                    filter_id: 9,
                    predecessor: pred.actor_id(),
                    verdict: UnattestedVerdict::Kept,
                }],
            };
            let b = SuccessionLedger {
                actor_id: me.actor_id(),
                prior_actor_ids: vec![],
                grant_events: vec![mint(&me, 1, 10), mint(&me, 3, 20)],
                unattested_grant_marks: vec![GrantUnattestedMark {
                    grant_id: vec![2; 16],
                    predecessor: pred.actor_id(),
                    verdict: UnattestedVerdict::Removed,
                }],
                unattested_member_items: vec![MemberUnattestedItem {
                    person: ActorId([7; 32]),
                    predecessor: pred.actor_id(),
                    reason: MemberUnattestedReason::CompromiseWindow,
                    verdict: UnattestedVerdict::Open,
                }],
                unattested_filter_marks: vec![],
            };
            let mut composite: Vec<(String, Vec<u8>)> = a
                .merge(&b)
                .rows()
                .unwrap()
                .iter()
                .map(|(_, r)| row(r))
                .collect();
            composite.sort();
            let mut by_arm: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
            for (_, r) in a.rows().unwrap().into_iter().chain(b.rows().unwrap()) {
                let (k, v) = row(&r);
                let next = match by_arm.get(&k) {
                    Some(cur) => merged(&k, cur, &v),
                    None => v,
                };
                by_arm.insert(k, next);
            }
            assert_eq!(by_arm.into_iter().collect::<Vec<_>>(), composite);
            assert_eq!(composite.len(), 7, "chain + 3 events + 3 marks");
        }

        /// The two row-content refusals — a forked chain and a differing
        /// event under one key — and the grammar's: a tombstone, junk, a key
        /// outside the grammar, a value filed under another row's key, a
        /// record of another family's type.
        #[test]
        fn a_forked_chain_a_rewritten_event_or_a_misfiled_row_is_refused() {
            let (p, s, t) = (ActorId([1; 32]), ActorId([2; 32]), ActorId([3; 32]));
            let succ = row(&SuccessionLedgerRecord::Chain(ChainState {
                actor_id: s,
                prior_actor_ids: vec![p],
            }));
            let thief = row(&SuccessionLedgerRecord::Chain(ChainState {
                actor_id: t,
                prior_actor_ids: vec![p],
            }));
            assert!(
                refused(&entry(&thief.0, &thief.1), Some(&entry(&succ.0, &succ.1))),
                "two successors of one predecessor is a fork"
            );
            assert!(
                !refused(&entry(&thief.0, &thief.1), None),
                "alone, a chain row is adoptable"
            );

            // Write-once: same key, a different value (a tampered window).
            let good = mint(&kp(1), 1, 10);
            let mut tampered = good.clone();
            tampered.window_end += 1;
            let (key, good_bytes) = row(&SuccessionLedgerRecord::Event(good));
            let tampered_bytes = SuccessionLedgerRecord::Event(tampered).encode().unwrap();
            assert!(refused(
                &entry(&key, &tampered_bytes),
                Some(&entry(&key, &good_bytes))
            ));
            assert_eq!(
                apply_class2(
                    MergePolicy::CrdtPerField,
                    Some(&entry(&key, &good_bytes)),
                    &entry(&key, &good_bytes)
                )
                .unwrap(),
                MergeOutcome::KeepCurrent,
                "a byte-equal event is the echo-stop"
            );

            let mut dead = entry(&key, &good_bytes);
            dead.tombstone = true;
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, None, &dead),
                Err(MergeError::TombstoneOnCrdtKind { .. })
            ));
            assert!(refused(&entry(&key, &[0xff, 0x00]), None));
            assert!(refused(&entry("self", &good_bytes), None));
            let other_key = row(&SuccessionLedgerRecord::Event(mint(&kp(1), 2, 10))).0;
            assert!(refused(&entry(&other_key, &good_bytes), None));
            assert!(refused(&entry(&key.to_uppercase(), &good_bytes), None));
            let (mark_key, _) = row(&grant_mark(UnattestedVerdict::Open));
            assert!(refused(&entry(&mark_key, &good_bytes), None));
            assert!(refused(&entry(CHAIN_KEY, &good_bytes), None));
        }

        /// The P4 decode posture, per row type: a value carrying a field from
        /// a build past this one answers `BadValue` (it re-presents on the
        /// next reconcile) instead of merging with the field silently
        /// stripped from the re-encoded row.
        #[test]
        fn a_newer_succession_ledger_field_is_refused_not_stripped() {
            #[derive(serde::Serialize)]
            struct Newer<'a, T> {
                #[serde(flatten)]
                record: &'a T,
                from_the_future: u8,
            }
            fn with_extra_field<T: serde::Serialize>(record: &T) -> Vec<u8> {
                fauna_core::encoding::canonical_encode(&Newer {
                    record,
                    from_the_future: 1,
                })
                .unwrap()
            }
            let records = [
                SuccessionLedgerRecord::Chain(ChainState::of(ActorId([2; 32]))),
                SuccessionLedgerRecord::Event(mint(&kp(1), 1, 10)),
                grant_mark(UnattestedVerdict::Open),
                member_item(UnattestedVerdict::Open),
                filter_mark(UnattestedVerdict::Open),
            ];
            for record in &records {
                let (key, current) = row(record);
                let newer = match record {
                    SuccessionLedgerRecord::Chain(r) => with_extra_field(r),
                    SuccessionLedgerRecord::Event(r) => with_extra_field(r),
                    SuccessionLedgerRecord::GrantMark(r) => with_extra_field(r),
                    SuccessionLedgerRecord::MemberItem(r) => with_extra_field(r),
                    SuccessionLedgerRecord::FilterMark(r) => with_extra_field(r),
                };
                assert!(refused(&entry(&key, &newer), None), "{key}: first contact");
                assert!(
                    refused(&entry(&key, &newer), Some(&entry(&key, &current))),
                    "{key}: merge"
                );
            }
        }

        /// Every `Vec<u8>` on the row types is a CBOR byte string, never
        /// serde's integer array — the size pins' premise (`serde_bytes`;
        /// `ActorId` is one by its own impl).
        #[test]
        fn succession_ledger_rows_carry_byte_strings() {
            fn has_int_array(v: &crate::Value) -> bool {
                match v {
                    crate::Value::List(items) => {
                        items.iter().all(|i| matches!(i, crate::Value::Integer(_)))
                            && !items.is_empty()
                            || items.iter().any(has_int_array)
                    }
                    crate::Value::Map(m) => m.values().any(has_int_array),
                    _ => false,
                }
            }
            let records = [
                SuccessionLedgerRecord::Chain(ChainState {
                    actor_id: ActorId([2; 32]),
                    prior_actor_ids: vec![ActorId([1; 32])],
                }),
                SuccessionLedgerRecord::Event(mint(&kp(1), 1, 10)),
                grant_mark(UnattestedVerdict::Open),
                member_item(UnattestedVerdict::Open),
                filter_mark(UnattestedVerdict::Open),
            ];
            for record in &records {
                let value: crate::Value = crate::decode_strict(&record.encode().unwrap()).unwrap();
                assert!(
                    !has_int_array(&value),
                    "{record:?} encodes an integer array"
                );
            }
        }
    }

    // ── Shared-folder content keys (`config-dissolution.md` → *Bounded rows*) ──

    mod folder_keys {
        use super::*;
        use fauna_core::data::{
            FolderKeyCustody, FolderPendingRemoval, FoldersConfig, ForeignFolder,
        };
        use fauna_core::folder_key_rows::{
            FolderGenerationRow, FolderKeyRecord, FolderRemovalRow, FolderSetRecord, SetIdentity,
            decode_folder_key_row,
        };
        use fauna_core::folder_keys::{ContentKeyGeneration, FolderContentKeys};
        use fauna_core::identity::ActorId;

        fn generation(version: u64, key: u8) -> ContentKeyGeneration {
            ContentKeyGeneration {
                version,
                key: [key; 32].into(),
                rotated_at: version * 1_000,
            }
        }

        fn row(r: &FolderKeyRecord) -> (String, Vec<u8>) {
            (r.plane_key().unwrap(), r.encode().unwrap())
        }

        fn entry(key: &str, value: &[u8]) -> EntryPlaintext {
            crdt_entry(KIND_FOLDER_KEYS, key, value.to_vec())
        }

        fn merged(key: &str, a: &[u8], b: &[u8]) -> Vec<u8> {
            match apply_class2(
                MergePolicy::CrdtPerField,
                Some(&entry(key, a)),
                &entry(key, b),
            )
            .unwrap()
            {
                MergeOutcome::Merged(e) => e.value.to_vec(),
                MergeOutcome::KeepCurrent => a.to_vec(),
                other => panic!("expected a merge, got {other:?}"),
            }
        }

        fn assert_join_on_bytes(key: &str, samples: &[Vec<u8>]) {
            let m = |a: &[u8], b: &[u8]| merged(key, a, b);
            for a in samples {
                assert_eq!(m(a, a), *a, "idempotent at {key}");
                for b in samples {
                    assert_eq!(m(a, b), m(b, a), "commutative at {key}");
                    for c in samples {
                        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative at {key}");
                    }
                }
            }
        }

        fn refused(e: &EntryPlaintext, current: Option<&EntryPlaintext>) -> bool {
            matches!(
                apply_class2(MergePolicy::CrdtPerField, current, e),
                Err(MergeError::BadValue { .. })
            )
        }

        fn set_record(retired_at: Option<u64>, channel: u8) -> FolderKeyRecord {
            FolderKeyRecord::Set(FolderSetRecord {
                channel_id: (channel != 0).then_some([channel; 32]),
                set_nonce: Some([0x01; 32]),
                name: Some("photos".into()),
                created_at: 10,
                retired_at,
                ..Default::default()
            })
        }

        fn lifted_record(retired_at: u64, lifted_at: u64) -> FolderKeyRecord {
            let FolderKeyRecord::Set(set) = set_record(Some(retired_at), 0) else {
                unreachable!()
            };
            FolderKeyRecord::Set(FolderSetRecord {
                lifted_at: Some(lifted_at),
                ..set
            })
        }

        fn pending(commit: Option<Vec<u8>>, gated_attempted: bool) -> FolderKeyRecord {
            settled_pending(commit, gated_attempted, false)
        }

        fn settled_pending(
            commit: Option<Vec<u8>>,
            gated_attempted: bool,
            settled: bool,
        ) -> FolderKeyRecord {
            FolderKeyRecord::Removal(FolderRemovalRow {
                removal: FolderPendingRemoval {
                    channel_id: [0xC1; 32],
                    name: "photos".into(),
                    removed_member: ActorId([0x55; 32]),
                    new_generation: generation(3, 0x33),
                    commit,
                    gated_attempted,
                },
                settled,
            })
        }

        fn foreign(url: &str, access: Option<&str>, floor: Option<u64>) -> FolderKeyRecord {
            FolderKeyRecord::Foreign(ForeignFolder {
                channel_id: [0x77; 32],
                mls_group_id: vec![0x78; 16],
                home_nest_url: url.into(),
                home_nest_actor_id: None,
                set_name: Some("photos".into()),
                access: access.map(str::to_string),
                content_key_floor: floor,
                ..Default::default()
            })
        }

        fn config() -> FoldersConfig {
            FoldersConfig {
                sets: vec![FolderKeyCustody {
                    channel_id: Some([0xC1; 32]),
                    keys: Some(FolderContentKeys {
                        current: generation(2, 0x22),
                        prior: vec![generation(1, 0x11)],
                    }),
                    set_nonce: Some([0x01; 32]),
                    name: Some("photos".into()),
                    created_at: 10,
                    ..Default::default()
                }],
                pending_removals: vec![],
                foreign_sets: vec![],
            }
        }

        /// The registration, held whole (`config-dissolution.md` — the kinds
        /// table's `fauna.state.folder-keys` row): the frozen string,
        /// per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
        /// departure.
        #[test]
        fn the_folder_keys_register_as_a_fleet_only_tip_sealed_crdt() {
            assert_eq!(KIND_FOLDER_KEYS, "fauna.state.folder-keys");
            assert_eq!(
                merge_policy(KIND_FOLDER_KEYS),
                Some(MergePolicy::CrdtPerField)
            );
            assert_eq!(
                audience_rung(KIND_FOLDER_KEYS),
                Some(AudienceRung::FleetOnly)
            );
            assert_eq!(
                sealing_epoch(KIND_FOLDER_KEYS),
                Some(SealingEpoch::GenerationTip)
            );
            assert_eq!(
                TipSealedKind::of(KIND_FOLDER_KEYS).map(TipSealedKind::scope),
                Some(TipRowScope::Account)
            );
            assert!(!retired_with_its_writer(KIND_FOLDER_KEYS));
            assert!(!delegable_kinds().any(|k| k == KIND_FOLDER_KEYS));
        }

        /// The join laws, ON BYTES through the plane arm, per row type: the
        /// set row's field-wise join (tombstone, bound-over-served channel),
        /// the staging's commit / attempt fold, the foreign record's per-field
        /// fold, and the generation row's write-once idempotence.
        #[test]
        fn folder_keys_merge_is_a_join_on_bytes() {
            let sets: Vec<Vec<u8>> = [
                set_record(None, 0),
                set_record(Some(50), 0),
                set_record(Some(40), 0xC1),
                set_record(None, 0xC2),
                lifted_record(40, 40),
            ]
            .iter()
            .map(|r| row(r).1)
            .collect();
            assert_join_on_bytes(&row(&set_record(None, 0)).0, &sets);

            let stagings: Vec<Vec<u8>> = [
                pending(None, false),
                pending(Some(vec![1, 2]), false),
                pending(Some(vec![0, 9]), true),
                pending(None, true),
                settled_pending(None, false, true),
                settled_pending(Some(vec![5]), true, true),
            ]
            .iter()
            .map(|r| row(r).1)
            .collect();
            assert_join_on_bytes(&row(&pending(None, false)).0, &stagings);

            let foreigns: Vec<Vec<u8>> = [
                foreign("https://a.example", Some("writer"), None),
                foreign("https://b.example", Some("reader"), Some(3)),
                foreign("https://a.example", None, Some(7)),
            ]
            .iter()
            .map(|r| row(r).1)
            .collect();
            assert_join_on_bytes(&row(&foreign("x", None, None)).0, &foreigns);

            let (key, g) = row(&FolderKeyRecord::Generation(FolderGenerationRow {
                set: SetIdentity::Nonce([0x01; 32]),
                generation: generation(1, 0x11),
            }));
            assert_join_on_bytes(&key, &[g]);

            // The fail-safe folds survive either direction: the later
            // tombstone (ruling (l)(v) — a lift covers only the stamp it
            // names, so a later delete outlives it), the lesser grant, the
            // higher floor.
            let joined = merged(&row(&set_record(None, 0)).0, &sets[1], &sets[2]);
            let rec: FolderSetRecord = fauna_core::encoding::canonical_decode(&joined).unwrap();
            assert_eq!(rec.retired_at, Some(50));
            let joined = merged(&row(&set_record(None, 0)).0, &sets[2], &sets[4]);
            let rec: FolderSetRecord = fauna_core::encoding::canonical_decode(&joined).unwrap();
            assert!(
                rec.to_entry().is_live(),
                "a stale tombstone cannot undo the lift"
            );
            let joined = merged(&row(&set_record(None, 0)).0, &sets[1], &sets[4]);
            let rec: FolderSetRecord = fauna_core::encoding::canonical_decode(&joined).unwrap();
            assert!(
                !rec.to_entry().is_live(),
                "the later delete outlives the lift"
            );
            let key = row(&foreign("x", None, None)).0;
            let f: ForeignFolder =
                fauna_core::encoding::canonical_decode(&merged(&key, &foreigns[0], &foreigns[1]))
                    .unwrap();
            assert_eq!(f.access.as_deref(), Some("reader"));
            assert_eq!(f.content_key_floor, Some(3));
        }

        /// The arm is `FoldersConfig::merge`'s per-row half, not a second
        /// rule: merging two whole configs and splitting the result into rows
        /// gives exactly the rows the arm produces from the two sides' rows —
        /// a concurrent rotation to one version keeping both keys.
        #[test]
        fn the_folder_keys_arm_agrees_with_the_composite_merge() {
            let a = config();
            let mut b = config();
            b.sets[0].keys = Some(FolderContentKeys {
                current: generation(2, 0x2B),
                prior: vec![generation(1, 0x11)],
            });
            b.sets[0].retired_at = Some(90);
            b.pending_removals.push(FolderPendingRemoval {
                channel_id: [0xC1; 32],
                name: "photos".into(),
                removed_member: ActorId([0x55; 32]),
                new_generation: generation(3, 0x33),
                commit: None,
                gated_attempted: true,
            });
            b.foreign_sets.push(ForeignFolder {
                channel_id: [0x77; 32],
                mls_group_id: vec![0x78; 16],
                home_nest_url: "https://home.example".into(),
                home_nest_actor_id: None,
                set_name: None,
                access: None,
                content_key_floor: Some(2),
                ..Default::default()
            });
            let mut composite: Vec<(String, Vec<u8>)> = a
                .merge(&b)
                .rows()
                .unwrap()
                .iter()
                .map(|(_, r)| row(r))
                .collect();
            composite.sort();
            let mut by_arm: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
            for (_, r) in a.rows().unwrap().into_iter().chain(b.rows().unwrap()) {
                let (k, v) = row(&r);
                let next = match by_arm.get(&k) {
                    Some(cur) => merged(&k, cur, &v),
                    None => v,
                };
                by_arm.insert(k, next);
            }
            assert_eq!(by_arm.into_iter().collect::<Vec<_>>(), composite);
            assert_eq!(
                composite.len(),
                6,
                "set + three generations + staging + foreign"
            );
        }

        /// The row-content refusal — two differing generations under one key
        /// — and the grammar's: a tombstone, junk, a key outside the grammar
        /// (the retired `self` shape included), a value filed under another
        /// row's key, a record of another family's type.
        #[test]
        fn a_rewritten_generation_or_a_misfiled_row_is_refused() {
            let (key, good) = row(&FolderKeyRecord::Generation(FolderGenerationRow {
                set: SetIdentity::Nonce([0x01; 32]),
                generation: generation(1, 0x11),
            }));
            let other = FolderKeyRecord::Generation(FolderGenerationRow {
                set: SetIdentity::Nonce([0x01; 32]),
                generation: generation(1, 0x12),
            })
            .encode()
            .unwrap();
            assert!(refused(&entry(&key, &other), Some(&entry(&key, &good))));
            assert!(
                refused(&entry(&key, &other), None),
                "the value names its digest"
            );
            assert_eq!(
                apply_class2(
                    MergePolicy::CrdtPerField,
                    Some(&entry(&key, &good)),
                    &entry(&key, &good)
                )
                .unwrap(),
                MergeOutcome::KeepCurrent,
                "a byte-equal generation is the echo-stop"
            );

            let mut dead = entry(&key, &good);
            dead.tombstone = true;
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, None, &dead),
                Err(MergeError::TombstoneOnCrdtKind { .. })
            ));
            assert!(refused(&entry(&key, &[0xff, 0x00]), None));
            let whole = fauna_core::encoding::canonical_encode(&config()).unwrap();
            assert!(
                refused(&entry("self", &whole), None),
                "no one-row-per-account shape"
            );
            assert!(refused(&entry(&key.to_uppercase(), &good), None));
            let (set_key, set) = row(&set_record(None, 0));
            assert!(refused(&entry(&set_key, &good), None));
            assert!(refused(&entry(&key, &set), None));
            // A set record under another identity's key.
            let mut moved = set_record(None, 0);
            if let FolderKeyRecord::Set(r) = &mut moved {
                r.set_nonce = Some([0x02; 32]);
            }
            assert!(refused(&entry(&set_key, &row(&moved).1), None));
            let (fkey, _) = row(&foreign("x", None, None));
            let (pkey, pval) = row(&pending(None, false));
            assert!(refused(&entry(&fkey, &pval), None));
            assert!(refused(&entry(&pkey, &set), None));
        }

        /// The P4 decode posture, per row type: a value carrying a field from
        /// a build past this one answers `BadValue` (it re-presents on the
        /// next reconcile) instead of merging with the field silently
        /// stripped — `FoldersConfig`'s own serde is tolerant, the plane is
        /// not.
        #[test]
        fn a_newer_folder_keys_field_is_refused_not_stripped() {
            #[derive(serde::Serialize)]
            struct Newer<'a, T> {
                #[serde(flatten)]
                record: &'a T,
                from_the_future: u8,
            }
            fn with_extra_field<T: serde::Serialize>(record: &T) -> Vec<u8> {
                fauna_core::encoding::canonical_encode(&Newer {
                    record,
                    from_the_future: 1,
                })
                .unwrap()
            }
            let records = [
                set_record(None, 0xC1),
                FolderKeyRecord::Generation(FolderGenerationRow {
                    set: SetIdentity::Channel([0xC1; 32]),
                    generation: generation(1, 0x11),
                }),
                pending(Some(vec![1]), true),
                foreign("https://a.example", Some("reader"), Some(1)),
            ];
            for record in &records {
                let (key, current) = row(record);
                let newer = match record {
                    FolderKeyRecord::Set(r) => with_extra_field(r),
                    FolderKeyRecord::Generation(r) => with_extra_field(r),
                    FolderKeyRecord::Removal(r) => with_extra_field(r),
                    FolderKeyRecord::Foreign(r) => with_extra_field(r),
                };
                assert!(refused(&entry(&key, &newer), None), "{key}: first contact");
                assert!(
                    refused(&entry(&key, &newer), Some(&entry(&key, &current))),
                    "{key}: merge"
                );
                assert!(decode_folder_key_row(&key, &current).is_ok());
            }
        }

        /// Every byte field on the row types is a CBOR byte string, never
        /// serde's integer array — the size pins' premise.
        #[test]
        fn folder_keys_rows_carry_byte_strings() {
            fn has_int_array(v: &crate::Value) -> bool {
                match v {
                    crate::Value::List(items) => {
                        items.iter().all(|i| matches!(i, crate::Value::Integer(_)))
                            && !items.is_empty()
                            || items.iter().any(has_int_array)
                    }
                    crate::Value::Map(m) => m.values().any(has_int_array),
                    _ => false,
                }
            }
            let records = [
                set_record(Some(1), 0xC1),
                FolderKeyRecord::Generation(FolderGenerationRow {
                    set: SetIdentity::Nonce([0x01; 32]),
                    generation: generation(1, 0x11),
                }),
                pending(Some(vec![1, 2, 3]), false),
                foreign("https://a.example", Some("reader"), Some(1)),
            ];
            for record in &records {
                let value: crate::Value = crate::decode_strict(&record.encode().unwrap()).unwrap();
                assert!(
                    !has_int_array(&value),
                    "{record:?} encodes an integer array"
                );
            }
        }
    }

    // ── `fauna.state.refused-scheduling-changes` (the refused inbound scheduling changes) ──

    fn a_refusal(
        uid: u64,
        author: u64,
        at: i64,
        occurrences: u32,
        dismissed: u32,
    ) -> fauna_core::data::RefusedSchedulingChange {
        fauna_core::data::RefusedSchedulingChange {
            uid_hash: format!("{uid:064x}"),
            author: Some(format!("{:064x}", author + 1)),
            author_home_nest_url: String::new(),
            sender_address: String::new(),
            method: "CANCEL".into(),
            reason: "not_the_organizer".into(),
            summary: "Kickoff".into(),
            first_refused_at: at - 10,
            last_refused_at: at,
            occurrences,
            dismissed_through: dismissed,
            extra: Default::default(),
        }
    }

    /// `rows`, held under the ceilings — a value the plane accepts.
    fn held(
        rows: Vec<fauna_core::data::RefusedSchedulingChange>,
    ) -> fauna_core::data::RefusedSchedulingChanges {
        use fauna_core::data::RefusedSchedulingChanges;
        RefusedSchedulingChanges { rows }.merge(&RefusedSchedulingChanges::default())
    }

    fn refused_entry(list: &fauna_core::data::RefusedSchedulingChanges) -> EntryPlaintext {
        crdt_entry(
            KIND_REFUSED_SCHEDULING_CHANGES,
            "self",
            list.encode_row().unwrap(),
        )
    }

    fn merged_refused_row(a: &[u8], b: &[u8]) -> Vec<u8> {
        let entry = |v: &[u8]| crdt_entry(KIND_REFUSED_SCHEDULING_CHANGES, "self", v.to_vec());
        match apply_class2(MergePolicy::CrdtPerField, Some(&entry(a)), &entry(b)).unwrap() {
            MergeOutcome::Merged(e) => e.value.to_vec(),
            MergeOutcome::KeepCurrent => a.to_vec(),
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    /// The registration, held whole (`config-dissolution.md` — the kinds
    /// table's `fauna.state.refused-scheduling-changes` row): the frozen
    /// string, per-field CRDT, fleet-only, tip-sealed — and an ACCOUNT row on
    /// departure: a notice is the account's, whichever device drained it.
    #[test]
    fn the_refused_scheduling_changes_registers_as_a_fleet_only_tip_sealed_crdt() {
        assert_eq!(
            KIND_REFUSED_SCHEDULING_CHANGES,
            "fauna.state.refused-scheduling-changes"
        );
        assert_eq!(
            merge_policy(KIND_REFUSED_SCHEDULING_CHANGES),
            Some(MergePolicy::CrdtPerField)
        );
        assert_eq!(
            audience_rung(KIND_REFUSED_SCHEDULING_CHANGES),
            Some(AudienceRung::FleetOnly)
        );
        assert_eq!(
            sealing_epoch(KIND_REFUSED_SCHEDULING_CHANGES),
            Some(SealingEpoch::GenerationTip)
        );
        assert!(!retired_with_its_writer(KIND_REFUSED_SCHEDULING_CHANGES));
        assert!(!delegable_kinds().any(|k| k == KIND_REFUSED_SCHEDULING_CHANGES));
        assert_eq!(
            TipSealedKind::of(KIND_REFUSED_SCHEDULING_CHANGES).map(TipSealedKind::scope),
            Some(TipRowScope::Account)
        );
    }

    /// The join laws, ON BYTES through the plane arm — commutative,
    /// associative, idempotent — over lists the ceilings cut, including the
    /// case a cap makes hard: a row one merge cuts coming back through a
    /// third replica's newer attempt (`c` below re-sights `a`'s newest key).
    /// Red with the field-wise per-key join the merge used before the lift
    /// (`fauna_core::refused_change_rows::tests::the_capped_merge_is_a_join_on_bytes`,
    /// the fuzzed twin, went red on it first).
    #[test]
    fn refused_scheduling_changes_merge_is_a_join_on_bytes() {
        // `a`: seven authors, three rows each at stamps 100.. (its own global
        // ceiling already cuts the oldest); `b`: seven other authors, three
        // rows each, all newer — the union's global ceiling cuts every row of
        // `a`; `c`: a newer attempt on `a`'s newest key with a lower count and
        // no dismissal, and a dismissal on one of `b`'s rows.
        let a = held(
            (0..21)
                .map(|i| a_refusal(i, i / 3, 100 + i as i64, 5, 5))
                .collect(),
        );
        let b = held(
            (0..21)
                .map(|i| a_refusal(100 + i, 10 + i / 3, 200 + i as i64, 1, 0))
                .collect(),
        );
        let c = held(vec![
            a_refusal(20, 6, 300, 2, 0),
            a_refusal(100, 10, 200, 1, 1),
        ]);
        let samples: Vec<Vec<u8>> = [
            fauna_core::data::RefusedSchedulingChanges::default(),
            a,
            b,
            c,
            held(vec![a_refusal(5, 1, 105, 7, 2)]),
        ]
        .iter()
        .map(|l| l.encode_row().unwrap())
        .collect();
        let m = merged_refused_row;
        for x in &samples {
            assert_eq!(m(x, x), *x, "idempotent");
            for y in &samples {
                assert_eq!(m(x, y), m(y, x), "commutative");
                for z in &samples {
                    assert_eq!(m(&m(x, y), z), m(x, &m(y, z)), "associative");
                }
            }
        }
    }

    /// A covered row — the incoming side adds nothing — is `KeepCurrent`, so
    /// a converged pair stops publishing.
    #[test]
    fn a_covered_refused_changes_row_reports_keep_current() {
        let current = held(vec![a_refusal(1, 1, 100, 2, 2), a_refusal(2, 2, 110, 1, 0)]);
        let stale = held(vec![a_refusal(1, 1, 100, 2, 0)]);
        assert!(matches!(
            apply_class2(
                MergePolicy::CrdtPerField,
                Some(&refused_entry(&current)),
                &refused_entry(&stale),
            ),
            Ok(MergeOutcome::KeepCurrent)
        ));
    }

    /// A tombstone, junk bytes, a key other than `self`, and a list the
    /// ceilings would change (a fourth row for one author, a title past its
    /// byte ceiling) are all refused — at first contact, and against a good
    /// current row.
    #[test]
    fn a_refused_changes_tombstone_junk_misfiled_or_unheld_row_is_refused() {
        let good = held(vec![a_refusal(1, 1, 100, 1, 0)]);
        let mut dead = refused_entry(&good);
        dead.tombstone = true;
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &dead),
            Err(MergeError::TombstoneOnCrdtKind { .. })
        ));
        let mut junk = refused_entry(&good);
        junk.value = vec![0xff, 0x00].into();
        let mut other_key = refused_entry(&good);
        other_key.key = "other".into();
        let mut crowded_rows: Vec<_> = (0..4).map(|uid| a_refusal(uid, 1, 100, 1, 0)).collect();
        crowded_rows.sort_by_cached_key(fauna_core::data::RefusedSchedulingChange::key);
        let crowded = crdt_entry(
            KIND_REFUSED_SCHEDULING_CHANGES,
            "self",
            fauna_core::data::RefusedSchedulingChanges { rows: crowded_rows }
                .encode_row()
                .unwrap(),
        );
        let mut long = good.clone();
        long.rows[0].summary = "s".repeat(4096);
        let long = crdt_entry(
            KIND_REFUSED_SCHEDULING_CHANGES,
            "self",
            long.encode_row().unwrap(),
        );
        for bad in [junk, other_key, crowded, long] {
            for cur in [None, Some(&refused_entry(&good))] {
                assert!(
                    matches!(
                        apply_class2(MergePolicy::CrdtPerField, cur, &bad),
                        Err(MergeError::BadValue { .. })
                    ),
                    "the arm must refuse the row at {:?}",
                    bad.key
                );
            }
        }
    }

    /// **The decode-posture pin — strict on the plane, though the row type
    /// cannot take `deny_unknown_fields`** (its `#[serde(flatten)] extra`
    /// keeps its tolerance, and serde refuses the two together): a
    /// list whose row carries a field from a build past this one answers
    /// `BadValue` (it re-presents on the next reconcile) instead of merging
    /// with the field silently stripped — and so does a non-canonical
    /// encoding of a known list, which would not re-encode to its own bytes.
    #[test]
    fn a_newer_refused_change_field_is_refused_not_stripped() {
        #[derive(serde::Serialize)]
        struct Newer<'a> {
            #[serde(flatten)]
            record: &'a fauna_core::data::RefusedSchedulingChange,
            from_the_future: u8,
        }
        let good = held(vec![a_refusal(1, 1, 100, 1, 0)]);
        let current = refused_entry(&good);
        let mut incoming = current.clone();
        incoming.value = fauna_core::encoding::canonical_encode(&vec![Newer {
            record: &good.rows[0],
            from_the_future: 1,
        }])
        .unwrap()
        .to_vec()
        .into();
        for cur in [None, Some(&current)] {
            assert!(matches!(
                apply_class2(MergePolicy::CrdtPerField, cur, &incoming),
                Err(MergeError::BadValue { .. })
            ));
        }
        // The same list, its array header written in a non-minimal form.
        let canonical = good.encode_row().unwrap();
        assert_eq!(canonical[0] & 0xe0, 0x80, "a short CBOR array header");
        let mut loose = vec![0x98, canonical[0] & 0x1f];
        loose.extend_from_slice(&canonical[1..]);
        let mut non_canonical = current.clone();
        non_canonical.value = loose.into();
        assert!(matches!(
            apply_class2(MergePolicy::CrdtPerField, None, &non_canonical),
            Err(MergeError::BadValue { .. })
        ));
    }

    /// **The size pin (`config-dissolution.md` § Phases and gates → *Bounded
    /// rows*):** the one `self` row, FULL — `MAX_REFUSED_SCHEDULING_CHANGES`
    /// rows, three per author, every string at its byte ceiling and every
    /// number at its widest — seals under HALF the per-entry cap.
    #[test]
    fn a_full_refused_changes_row_seals_under_half_the_entry_cap() {
        use crate::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_core::account_entry_crypto::sealed_envelope_len;
        use fauna_core::data::{
            MAX_REFUSED_CHANGE_ADDRESS_BYTES, MAX_REFUSED_CHANGE_SUMMARY_BYTES,
            MAX_REFUSED_CHANGE_TOKEN_BYTES, MAX_REFUSED_CHANGE_URL_BYTES,
            MAX_REFUSED_CHANGES_PER_AUTHOR, MAX_REFUSED_SCHEDULING_CHANGES,
            RefusedSchedulingChange,
        };
        let generation_sealed = matches!(
            sealing_epoch(KIND_REFUSED_SCHEDULING_CHANGES),
            Some(SealingEpoch::GenerationTip)
        );
        let pad = |prefix: String, len: usize| format!("{prefix:x<len$}");
        let full = held(
            (0..MAX_REFUSED_SCHEDULING_CHANGES)
                .map(|i| RefusedSchedulingChange {
                    uid_hash: pad(format!("{i:02}"), MAX_REFUSED_CHANGE_TOKEN_BYTES),
                    author: Some(format!("{:064x}", 1 + i / MAX_REFUSED_CHANGES_PER_AUTHOR)),
                    author_home_nest_url: pad(
                        "https://nest.example/".into(),
                        MAX_REFUSED_CHANGE_URL_BYTES,
                    ),
                    sender_address: pad(format!("{i:02}@"), MAX_REFUSED_CHANGE_ADDRESS_BYTES),
                    method: pad("CANCEL".into(), MAX_REFUSED_CHANGE_TOKEN_BYTES),
                    reason: pad("not_the_organizer".into(), MAX_REFUSED_CHANGE_TOKEN_BYTES),
                    summary: pad(String::new(), MAX_REFUSED_CHANGE_SUMMARY_BYTES),
                    first_refused_at: i64::MIN,
                    last_refused_at: i64::MAX,
                    occurrences: u32::MAX,
                    dismissed_through: u32::MAX,
                    extra: Default::default(),
                })
                .collect(),
        );
        assert_eq!(
            full.rows.len(),
            MAX_REFUSED_SCHEDULING_CHANGES,
            "the list is full"
        );
        assert!(
            full.rows
                .iter()
                .all(|r| r.summary.len() == MAX_REFUSED_CHANGE_SUMMARY_BYTES
                    && r.sender_address.len() == MAX_REFUSED_CHANGE_ADDRESS_BYTES),
            "every field at its ceiling survives the bound"
        );
        let len = sealed_envelope_len(&refused_entry(&full), generation_sealed).unwrap();
        assert!(
            len <= MAX_STATE_ENTRY_BYTES / 2,
            "a full refused-changes row seals to {len} bytes, over half the \
             {MAX_STATE_ENTRY_BYTES}-byte cap"
        );
    }

    // ── the admitted-kinds overlay (third-party-kinds.md § The kinds vocabulary) ──

    fn ext(s: &str) -> ExtKind {
        s.parse().unwrap()
    }

    #[test]
    fn manifest_spellings_name_the_closed_five_and_admit_latest_wins_only() {
        for (spelling, policy) in MANIFEST_MERGE_SPELLINGS {
            assert_eq!(MergePolicy::from_manifest_spelling(spelling), Some(policy));
            assert_eq!(policy.manifest_spelling(), spelling);
            assert_eq!(
                policy.manifest_admitted(),
                policy == MergePolicy::LatestWins
            );
        }
        assert_eq!(MergePolicy::from_manifest_spelling("LatestWins"), None);
        assert_eq!(MergePolicy::from_manifest_spelling("lww"), None);
    }

    #[test]
    fn an_unadmitted_ext_kind_is_not_on_the_plane_and_an_admitted_one_is_delegable_gen0() {
        let notes = "ext.example.com.notes";
        let mut overlay = AdmittedKinds::new();
        // The compat answer before admission: not on the plane here.
        assert_eq!(merge_policy(notes), None);
        assert_eq!(overlay.merge_policy(notes), None);
        assert_eq!(overlay.audience_rung(notes), None);
        assert_eq!(overlay.sealing_epoch(notes), None);

        overlay.admit(ext(notes), MergePolicy::LatestWins).unwrap();
        assert_eq!(overlay.merge_policy(notes), Some(MergePolicy::LatestWins));
        assert_eq!(overlay.audience_rung(notes), Some(AudienceRung::Delegable));
        assert_eq!(overlay.sealing_epoch(notes), Some(SealingEpoch::Gen0));
        // The free functions stay the compiled table alone.
        assert_eq!(merge_policy(notes), None);
        // A sibling kind of the same publisher is not admitted by implication.
        assert_eq!(overlay.merge_policy("ext.example.com.other"), None);
    }

    #[test]
    fn the_overlay_admits_latest_wins_only_and_never_a_second_answer() {
        let mut overlay = AdmittedKinds::new();
        for refused in [
            MergePolicy::CrdtPerField,
            MergePolicy::ThreeWay,
            MergePolicy::NestCas,
            MergePolicy::Immutable,
        ] {
            assert_eq!(
                overlay.admit(ext("ext.example.com.notes"), refused),
                Err(AdmitError::PolicyNotAdmitted(refused))
            );
        }
        assert!(overlay.is_empty());
        overlay
            .admit(ext("ext.example.com.notes"), MergePolicy::LatestWins)
            .unwrap();
        // Idempotent for the same answer.
        overlay
            .admit(ext("ext.example.com.notes"), MergePolicy::LatestWins)
            .unwrap();
        assert_eq!(overlay.kinds().count(), 1);
    }

    /// The overlay never shadows the compiled table: a first-party kind
    /// answers its registered columns whatever the overlay holds.
    #[test]
    fn the_compiled_table_answers_first() {
        let overlay = AdmittedKinds::new();
        for kind in class2_kinds() {
            assert_eq!(overlay.merge_policy(kind), merge_policy(kind), "{kind}");
            assert_eq!(overlay.audience_rung(kind), audience_rung(kind), "{kind}");
            assert_eq!(overlay.sealing_epoch(kind), sealing_epoch(kind), "{kind}");
        }
    }

    #[test]
    fn an_ext_kind_homes_in_its_own_scope_from_the_string_alone() {
        assert_eq!(
            home_scope_for_kind("ext.example.com.notes").as_deref(),
            Some("ext:ext.example.com.notes")
        );
        assert_eq!(
            home_scope_for_kind(KIND_MODERATION).as_deref(),
            Some(crate::account_state::ACCOUNT_STATE_SCOPE)
        );
        assert_eq!(home_scope_for_kind("ext.example.com.*"), None);
    }

    /// An admitted kind's keys are its delegable branch — the pair a
    /// `content.read` grant wraps (`DelegableSchedule::for_kind`) — never a
    /// fleet pair.
    #[test]
    fn an_admitted_kind_keys_on_the_delegable_branch() {
        let schedule =
            AccountStateKeySchedule::derive(&fauna_core::crypto::BackupKey::from_bytes([9u8; 32]));
        let kind = "ext.example.com.notes";
        let mut overlay = AdmittedKinds::new();
        assert!(overlay.kind_keys(&schedule, kind).is_none());
        overlay.admit(ext(kind), MergePolicy::LatestWins).unwrap();
        let keys = overlay.kind_keys(&schedule, kind).unwrap();
        assert_eq!(
            keys.item_key(b"k"),
            schedule
                .for_rung(AudienceRung::Delegable, kind)
                .item_key(b"k")
        );
        assert_ne!(
            keys.item_key(b"k"),
            schedule
                .for_rung(AudienceRung::FleetOnly, kind)
                .item_key(b"k")
        );
    }
}
