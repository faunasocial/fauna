//! The generalized account-data feed's class-2 leg — `fauna.account.state.put`
//! plus the vocabulary the feed's additive fields speak (W2.3 (account-data-plane.md § Workstreams),
//! `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors).
//!
//! **What the nest holds.** A class-2 item is a *sealed entry* whose form is
//! frozen (§ The class-2 entry form, T14): `[form version][random nonce]
//! [ChaCha20-Poly1305]` over canonical dag-cbor, AAD-bound to `{form_version,
//! writer_id, writer_seq, scope, item_key}` and minted by
//! `fauna_core::account_entry_crypto::seal_entry`. The nest is a **relay**: it
//! stores those bytes opaque, echoes them verbatim through the feed, and holds
//! no key that opens one. The AAD is what makes that safe — no relay can splice
//! one entry's ciphertext under another's coordinates.
//!
//! **The cleartext floor is a ceiling.** § T14's *"Cleartext floor per entry"*
//! enumerates everything the plane may expose outside the seal — scope id,
//! writer id, writer seq, op discriminator, blinded item key, ciphertext size,
//! timing — and closes with *"Nothing else"*. So [`AccountStatePutRequest`]
//! carries exactly that list and no kind name or logical key: the **blinded**
//! item key (`keyed_hash(item_blind(kind), logical_key)`) is the only routing
//! handle, and it lands in the shipped `sync_changes.path_hash` slot rather than
//! a new column. The class-2 vocabulary (registered kinds × field names) is
//! enumerable, so an unkeyed hash would let any custodian dictionary *which
//! setting* changed — which is why these keys are blinded where file-sync's
//! floor `path_hash` is not.
//!
//! **Why the wire spells classes and ops as strings.** An enum deriving
//! `Deserialize` without a catch-all fails the *whole* surrounding struct when a
//! newer writer sends a variant this binary predates — the trap already recorded
//! against `KemSuiteId`. A feed row is exactly the shape
//! where that would turn one unknown item class into an undecodable page, so the
//! wire carries `&str` and [`ItemClass::from_wire`] answers `None` for anything
//! this binary does not know.

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

/// WS-RPC kind: append one sealed class-2 entry to a scope's feed (User-class —
/// the nest derives the owning actor from the authenticated connection, so the
/// request carries no `actor_id`).
pub const KIND_STATE_PUT: &str = "fauna.account.state.put";

/// The **account-state scope** — "exactly one per account"
/// (`account-sync-plane.md` § Feeds and cursors), realized nest-side as the
/// per-actor reserved folder `__state` (the shipped rail mechanism:
/// `db::snapshots::get_or_create_reserved_folder`).
///
/// Scope names are nest-gated exactly like [`crate::drafts::DRAFT_RAILS`]: a
/// client naming a scope the nest does not admit is refused, so a new scope
/// ships only once nests admit it. Content scopes and folder scopes (the other
/// two families) are **not** served by this kind at W2.3 — they keep their
/// shipped feeds.
pub const ACCOUNT_STATE_SCOPE: &str = "state";

/// The **fleet half of the A5 partition** (charter § The audience ladder +
/// § The scope string, landed with the R14 (account-data-plane.md § The ratified decisions) build design): every fleet-only
/// kind — machinery and data alike — seals into this sibling scope from its
/// first production row, while the frozen [`ACCOUNT_STATE_SCOPE`] string *is*
/// the delegable sub-scope (pure grandfathering: every production row ever
/// sealed there is delegable-rung, so nothing moves). A delegable grantee's
/// subscription names `state` and never observes fleet churn; custody grants
/// and admission verdicts enumerate both strings as ordinary scopes.
/// Kind→scope routing: `crate::merge_policy::home_scope_for_kind`.
pub const ACCOUNT_STATE_FLEET_SCOPE: &str = "state-fleet";

/// Whether `scope` is a scope this plane's write kind serves.
pub fn is_served_scope(scope: &str) -> bool {
    scope == ACCOUNT_STATE_SCOPE || scope == ACCOUNT_STATE_FLEET_SCOPE
}

/// The op discriminator of a class-1 record's arrival on a content scope's
/// feed (charter § Feeds and cursors: content-scope "feed items are
/// `record-added` (CID) and `tombstone`"). Shares [`OP_TOMBSTONE`] with the
/// class-2 ops — a deletion is a deletion on either plane, and one spelling
/// keeps a reader from having to know which plane a row came from to read its
/// op.
pub const OP_RECORD_ADDED: &str = "record-added";

/// The op discriminator of an ordinary class-2 value write.
pub const OP_STATE_PUT: &str = "state-put";
/// The op discriminator of a class-2 deletion. A tombstone is an **ordinary
/// sealed entry** whose payload carries the tombstone marker (§ T14); this
/// cleartext op exists only so key-less custodians can apply tombstone
/// retention, and a relay cannot fabricate one that opens.
pub const OP_TOMBSTONE: &str = "tombstone";

/// Whether `op` is one of the two class-2 op discriminators.
pub fn is_state_op(op: &str) -> bool {
    op == OP_STATE_PUT || op == OP_TOMBSTONE
}

/// The feed row's explicit item-class discriminator
/// (`account-sync-plane.md` § Feeds and cursors → *Feed row + wire
/// evolution*). Absent on a row means the shipped
/// file-row semantics, which is what the current file-sync pull sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemClass {
    /// A chunked file manifest — the shipped file-sync row.
    ChunkManifest,
    /// A raw blob referenced directly (the reserved rails' shape).
    DirectBlob,
    /// A class-1 record addressed by its block CID.
    RecordCid,
    /// A class-2 sealed state entry, served inline on the feed row.
    StateEntry,
}

impl ItemClass {
    /// The wire spelling — the vocabulary `account-sync-plane.md` § Feeds and
    /// cursors → *Feed row + wire evolution* fixes.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::ChunkManifest => "chunk-manifest",
            Self::DirectBlob => "direct-blob",
            Self::RecordCid => "record-cid",
            Self::StateEntry => "state-entry",
        }
    }

    /// Parse a wire spelling. `None` for any class this binary does not know —
    /// deliberately not an error, so a newer writer's row degrades to "opaque
    /// to me" instead of failing the page (see the module header).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "chunk-manifest" => Some(Self::ChunkManifest),
            "direct-blob" => Some(Self::DirectBlob),
            "record-cid" => Some(Self::RecordCid),
            "state-entry" => Some(Self::StateEntry),
            _ => None,
        }
    }
}

/// Hard ceiling on one sealed class-2 entry's ciphertext, refused before any
/// write. Sized for the kinds this plane carries — settings keys, read/ack
/// markers, relationship rows, device-endpoint entries, seen-set batches — each
/// a canonical dag-cbor value plus merge metadata, none of which is a document.
pub const MAX_STATE_ENTRY_BYTES: usize = 64 * 1024;

/// Hard ceiling on the number of **live** entries one actor may hold in one
/// scope (an entry being one `(item_key, writer)` pair; superseded predecessors
/// do not count, and a tombstone is an entry).
///
/// **Why a count cap is the bound here, and metering is not.** The two shipped
/// rulings on reserved-rail growth closed their
/// planes by *closing the enumeration* — the retired `__config` rail's one fixed path,
/// [`crate::drafts::DRAFT_RAILS`]'s three — because the unbounded axis is the
/// **count** of client-chosen keys, never one write's size. That remedy is
/// unavailable here by name: class-2 item keys are **blinded**, so the nest
/// cannot know the legal set and must not be able to (§ T14 — being able to
/// enumerate them is the exact disclosure the blinding prevents). It is
/// available by count, which is what this constant is: the enumeration stays
/// secret while the plane stays bounded at `MAX_STATE_ENTRIES_PER_SCOPE ×
/// MAX_STATE_ENTRY_BYTES` per writer.
///
/// Metering against the storage quota is refused for the reasons — the
/// wire carries no `device_id` the metered path wants, and reserved rails are a
/// ratified exemption from the `path_sealed` discipline it assumes — plus one
/// of this plane's own: charging a user's settings and read markers against the
/// quota that meters their *files* is a product change, not a bound.
pub const MAX_STATE_ENTRIES_PER_SCOPE: i64 = 4096;

/// The most rows one [`AccountStatePutRequest::replaces`] list may name
/// (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part (2)).
/// The nest keeps one live row per `(item, writer)`, and a put names only rows
/// of its own item, so a list is as long as the number of other writers that
/// hold a live row of the item — a handful in every measured flow. The bound
/// keeps one put's transaction bounded; the nest refuses a longer list
/// `invalid_request`.
pub const MAX_REPLACED_ROWS_PER_PUT: usize = 64;

/// `fauna.account.state.put` — append one sealed class-2 entry to a scope's
/// feed. Every field is the cleartext floor's own list and nothing else.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccountStatePutRequest {
    /// The scope this entry belongs to — [`ACCOUNT_STATE_SCOPE`] today.
    pub scope: String,
    /// Hex 32-byte writer id: the authoring device principal's public key
    /// (§ The store device principal). Multi-master per R6 — a scope's rows
    /// carry one high-water per writer, never one shared counter.
    pub writer_id: String,
    /// The authoring writer's own log sequence for this entry, assigned by that
    /// writer and monotonic per writer. Bound into the entry's AAD, so it cannot
    /// be restated by a relay; the nest additionally refuses a value that does
    /// not advance this writer's high-water for the item.
    pub writer_seq: i64,
    /// The 32-byte **blinded** item key `keyed_hash(item_blind(kind),
    /// logical_key)` — stable for routing, supersession and latest-per-writer
    /// retention, opaque to every key-less position. Lands in the shipped
    /// `sync_changes.path_hash` slot.
    pub item_key: ByteBuf,
    /// [`OP_STATE_PUT`] or [`OP_TOMBSTONE`].
    pub op: String,
    /// The sealed T14 envelope, stored and echoed verbatim.
    pub entry: ByteBuf,
    /// **Nest-arbitrated kinds only** (§ T14 — *"The CAS base a nest arbitrates
    /// on rides the write RPC cleartext as a version counter"*): the
    /// [`Self::writer_seq`] this writer believes is
    /// the item's current head. When present the nest refuses the write unless
    /// it matches, so two devices racing a nest-arbitrated kind serialize
    /// instead of forking. Absent for the ordinary multi-master kinds, whose
    /// concurrent writes are the merge seam's input, not a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cas_base: Option<i64>,
    /// **The rows this put covers** (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, part (2)): rows of the put's own scope,
    /// each named by its cleartext coordinates. Inside the put's transaction,
    /// once the put has passed its own CAS and sequence checks, the nest marks
    /// superseded each named row that is live at exactly those coordinates,
    /// skips one that is not, and only then counts the cap — so a put that
    /// replaces a row needs no free pair. A refused put supersedes nothing. No
    /// retention gate applies: the row covering a replaced row lands in the
    /// same transaction. Delegable scope only — the nest refuses a list on
    /// [`ACCOUNT_STATE_FLEET_SCOPE`] `invalid_request`, as it does one longer
    /// than [`MAX_REPLACED_ROWS_PER_PUT`]. An empty list is not sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<ReplacedRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row an [`AccountStatePutRequest::replaces`] list names — the same
/// cleartext coordinates [`AccountStateRetireRequest`] names its row by.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReplacedRow {
    /// The 32-byte blinded item key of the row.
    pub item_key: ByteBuf,
    /// Hex 32-byte writer id of the row's author.
    pub writer_id: String,
    /// The row's own `writer_seq` — exact: a newer live row of the same
    /// `(item_key, writer)` is left alone.
    pub writer_seq: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`AccountStatePutRequest`] — the nest-log seq the entry landed at
/// (the nest-writer slot's coordinate, the same axis `since` walks).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccountStatePutReply {
    pub seq: i64,
    /// How many of the request's [`AccountStatePutRequest::replaces`] rows
    /// this put superseded — present (`Some(0)` included) whenever the request
    /// named any row, absent when it named none. The device forgets its relay
    /// copies of the rows it named once the put lands (part (2)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<u32>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// WS-RPC kind: retire one live class-2 entry from a scope's feed — mark it
/// superseded **without inserting anything** (`account-data-taxonomy.md`
/// § The generation machinery → *Fleet-scope reclamation*, clause (1)). The
/// one plane write that needs no live-entry headroom, which is what makes a
/// scope at [`MAX_STATE_ENTRIES_PER_SCOPE`] recoverable from an app. The nest
/// keeps the row's bytes and coordinate (its seq-reuse memory is intact) and
/// refuses the retire `not_yet_stable` while any marked walker holding a live
/// grant has not walked past the row, or `generation_in_use` while a live
/// form-v2 row still names [`AccountStateRetireRequest::no_rows_sealed_under`].
/// With [`AccountStateRetireRequest::delete_escrow_wraps`] beside that belt,
/// the retire that lands also deletes the holder's escrow wraps of the belted
/// generation, in the same transaction (clause (3e): a shredded, dataless
/// generation has nothing left to recover). User-class, actor from the
/// connection, exactly like [`KIND_STATE_PUT`]. Any other refusal is an
/// ordinary error to the caller.
pub const KIND_STATE_RETIRE: &str = "fauna.account.state.retire";

/// `fauna.account.state.retire` — the row named by its cleartext coordinates.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccountStateRetireRequest {
    /// The scope the row lives in.
    pub scope: String,
    /// The 32-byte blinded item key of the row.
    pub item_key: ByteBuf,
    /// Hex 32-byte writer id of the row's author.
    pub writer_id: String,
    /// The row's own `writer_seq` — the retire is exact: a newer live row of
    /// the same `(item_key, writer)` is left alone, and the request answers
    /// `retired: false`.
    pub writer_seq: i64,
    /// When present, the retire is refused `generation_in_use` while any live
    /// form-v2 row in the scope names this 32-byte generation id in its
    /// cleartext header — the nest-side belt behind a client's "this
    /// generation is dataless" finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_rows_sealed_under: Option<ByteBuf>,
    /// The escrow sweep rides the belt: when set, the retire that lands also
    /// deletes every escrow wrap this nest holds for the generation
    /// `no_rows_sealed_under` names, in the same transaction — after the belt
    /// has found the generation dataless, never otherwise. The flag has no
    /// generation of its own on purpose: the deletion cannot name a
    /// generation the belt did not check, so datalessness cannot be skipped.
    /// Set without `no_rows_sealed_under` the request is refused
    /// `invalid_request`. Additive 2026-09-16.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete_escrow_wraps: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`AccountStateRetireRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccountStateRetireReply {
    /// `true` when a live row at exactly those coordinates was marked
    /// superseded by this call; `false` when no such live row exists (already
    /// retired, superseded by a newer row, or never published) — idempotent,
    /// never an error.
    pub retired: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_class_wire_spellings_round_trip() {
        for c in [
            ItemClass::ChunkManifest,
            ItemClass::DirectBlob,
            ItemClass::RecordCid,
            ItemClass::StateEntry,
        ] {
            assert_eq!(ItemClass::from_wire(c.as_wire()), Some(c));
        }
    }

    /// The vocabulary is fixed by `account-sync-plane.md` § Feeds and cursors
    /// → *Feed row + wire evolution*; a rename here is a wire break, not a
    /// refactor.
    #[test]
    fn item_class_wire_spellings_are_the_ratified_ones() {
        assert_eq!(ItemClass::ChunkManifest.as_wire(), "chunk-manifest");
        assert_eq!(ItemClass::DirectBlob.as_wire(), "direct-blob");
        assert_eq!(ItemClass::RecordCid.as_wire(), "record-cid");
        assert_eq!(ItemClass::StateEntry.as_wire(), "state-entry");
    }

    /// An unknown class must degrade to `None`, never to a decode failure — the
    /// lesson this module's header records.
    #[test]
    fn an_unknown_item_class_is_none_rather_than_an_error() {
        assert_eq!(ItemClass::from_wire("some-future-class"), None);
        assert_eq!(ItemClass::from_wire(""), None);
    }

    #[test]
    fn only_the_account_state_scope_is_served_today() {
        assert!(is_served_scope(ACCOUNT_STATE_SCOPE));
        assert!(!is_served_scope("conv"));
        assert!(!is_served_scope(""));
    }

    /// Today's put request and reply, field for field, before `replaces` /
    /// `replaced` (2026-10-02) — the shape an older peer encodes and decodes.
    #[derive(Debug, Serialize, Deserialize)]
    struct PrePutRequest {
        scope: String,
        writer_id: String,
        writer_seq: i64,
        item_key: ByteBuf,
        op: String,
        entry: ByteBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cas_base: Option<i64>,
        #[serde(flatten, default)]
        extra: BTreeMap<String, Value>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct PrePutReply {
        seq: i64,
        #[serde(flatten, default)]
        extra: BTreeMap<String, Value>,
    }

    fn a_put() -> AccountStatePutRequest {
        AccountStatePutRequest {
            scope: ACCOUNT_STATE_SCOPE.to_string(),
            writer_id: "ab".repeat(32),
            writer_seq: 7,
            item_key: vec![3u8; 32].into(),
            op: OP_STATE_PUT.to_string(),
            entry: vec![9u8; 5].into(),
            ..Default::default()
        }
    }

    /// `replaces` and `replaced` are additive (`delegable-scope-reclamation.md`
    /// § Delegable-scope reclamation, *Compatibility*): a put that names no
    /// row and a reply that counts none are byte-identical to today's.
    #[test]
    fn a_put_naming_no_row_and_its_reply_encode_to_todays_bytes() {
        let put = a_put();
        let pre = PrePutRequest {
            scope: put.scope.clone(),
            writer_id: put.writer_id.clone(),
            writer_seq: put.writer_seq,
            item_key: put.item_key.clone(),
            op: put.op.clone(),
            entry: put.entry.clone(),
            cas_base: None,
            extra: Default::default(),
        };
        assert_eq!(
            crate::codec::encode_canonical(&put).unwrap(),
            crate::codec::encode_canonical(&pre).unwrap()
        );
        let reply = AccountStatePutReply {
            seq: 42,
            ..Default::default()
        };
        let pre = PrePutReply {
            seq: 42,
            extra: Default::default(),
        };
        assert_eq!(
            crate::codec::encode_canonical(&reply).unwrap(),
            crate::codec::encode_canonical(&pre).unwrap()
        );
    }

    /// The older-nest direction: a put that names rows decodes, field
    /// unknown, into today's request, and a reply counting rows into today's
    /// reply; the newer side reads both back whole.
    #[test]
    fn a_put_naming_rows_decodes_at_an_older_peer_and_round_trips() {
        let put = AccountStatePutRequest {
            replaces: vec![ReplacedRow {
                item_key: vec![3u8; 32].into(),
                writer_id: "cd".repeat(32),
                writer_seq: 4,
                extra: Default::default(),
            }],
            ..a_put()
        };
        let bytes = crate::codec::encode_canonical(&put).unwrap();
        let old: PrePutRequest = crate::codec::decode_strict(&bytes).unwrap();
        assert_eq!(old.writer_seq, 7);
        assert!(old.extra.contains_key("replaces"));
        let back: AccountStatePutRequest = crate::codec::decode_strict(&bytes).unwrap();
        assert_eq!(back, put);

        let reply = AccountStatePutReply {
            seq: 42,
            replaced: Some(0),
            extra: Default::default(),
        };
        let bytes = crate::codec::encode_canonical(&reply).unwrap();
        let old: PrePutReply = crate::codec::decode_strict(&bytes).unwrap();
        assert_eq!(old.seq, 42);
        let back: AccountStatePutReply = crate::codec::decode_strict(&bytes).unwrap();
        assert_eq!(back.replaced, Some(0));
        // And today's reply reads as one that counted nothing.
        let bytes = crate::codec::encode_canonical(&PrePutReply {
            seq: 1,
            extra: Default::default(),
        })
        .unwrap();
        let new: AccountStatePutReply = crate::codec::decode_strict(&bytes).unwrap();
        assert_eq!(new.replaced, None);
    }

    #[test]
    fn the_two_ops_are_the_only_state_ops() {
        assert!(is_state_op(OP_STATE_PUT));
        assert!(is_state_op(OP_TOMBSTONE));
        assert!(!is_state_op("create"));
        assert!(!is_state_op("delete"));
    }
}
