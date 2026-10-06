//! `fauna.segments.list` — list the source nest's live segments for an
//! actor+kind. Reply contains only `live_segments`; tombstoned segments
//! awaiting retention GC are not replicated (parent spec D7 / Plan 5
//! spec § D1).
//!
//! `fauna.segments.compact` — manual compaction trigger for an optional
//! `(kind, actor_id)` scope. Drops the periodic worker's 25% tombstone-
//! fraction gate to 0%; runs the same compaction code path under the same
//! advisory lock. Authorization: bearer must equal `actor_id` (owner)
//! for scoped requests, or be a nest admin (always — including whole-nest
//! unscoped requests). Pure-backup destinations refused with
//! `fauna.segments.pure_backup_destination`. Per
//! `docs/goal/architecture/message-segment-store.md` § Manual compact
//! endpoint.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_core::identity::ActorId;

use fauna_cbor::Value;

/// The WS-RPC kind [`SegmentsListRequest`] rides: enumerate one scope's live
/// segments of a kind at the source nest, with the source's saved counter.
pub const KIND_SEGMENTS_LIST: &str = "fauna.segments.list";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentsListRequest {
    /// A segment-store kind tag (`message-segment-store.md` § Layout is the
    /// authoritative list — `"mail"`, `"conv"`, `"calendar"`, `"card"`,
    /// `"post"`; there is no `"cal"`). Which kinds a given nest *serves*
    /// here is the list handler's wired set.
    pub kind: String,
    /// Owner actor; must equal the auth Bearer's actor or the handler
    /// returns `fauna.segments.not_owner`.
    pub actor_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The WS-RPC kind [`SegmentsCounterFloorRequest`] rides: raise one scope's
/// saved segment counter for one family (`segment-backup-protocol.md`
/// § Client-device custodian (pull) → *Restore* → *Recovery into the lived-in
/// nest that regressed*, part (0)).
pub const KIND_SEGMENTS_COUNTER_FLOOR: &str = "fauna.segments.counter_floor";

/// `fauna.segments.counter_floor` request — sent by the owner's device when
/// its audit pass accepts a source regression, with the generation it had
/// **pinned** (a ledger the source itself sealed, never the destination's
/// served number).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentsCounterFloorRequest {
    /// The serve tag of the family to floor — `"mail"` for the content,
    /// `"mail-placement"` for its journal ([`SegmentFamily::serve_kind`]); each
    /// family floors on its own key.
    pub kind: String,
    /// The scope — the owner actor, as for [`SegmentsListRequest::actor_id`];
    /// must equal the authenticated actor.
    pub actor_id: String,
    /// The floor: the scope's saved counter becomes at least this.
    pub floor: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.segments.counter_floor` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentsCounterFloorReply {
    /// The family's saved counter after the call — at least the floor, and
    /// higher when it already was.
    pub next_segment_id: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentsListReply {
    pub segments: Vec<SegmentRef>,
    /// The source's **saved** segment counter for this `(kind, scope)` — its
    /// `KindManifest::next_seg_id`, the id the next segment written takes.
    /// Never decreases for as long as the source's data directory lives:
    /// append, compaction and re-seed adoption all only raise it, and a
    /// compaction that yields nothing retires its inputs without minting, so
    /// this can sit **above** the highest live id plus one. That is exactly
    /// why the backup ledger carries it rather than deriving a counter from
    /// the live list ([`LiveManifestMirror::next_segment_id_seen`]): a device
    /// pinning "the highest generation I verified" must not alarm on an
    /// honest nest whose top segment compacted away. Required: a listing
    /// without it is refused, so the ledger never falls back to a derived
    /// counter an honest compaction can lower.
    pub next_segment_id: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One live segment as the source describes it — the `.dat` **and**, since
/// 2026-08-29, its `.meta` sidecar.
///
/// `Default` exists for fixtures (`..Default::default()` — the struct-update
/// shape that lets two branches grow a wire type without conflicting); a real
/// ref always comes from a nest's list handler.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SegmentRef {
    pub segment_id: u32,
    /// BLAKE3 of the framed segment file (header + payload + footer +
    /// trailer). 32 bytes hex-encoded.
    pub blake3_hex: String,
    /// `"YYYY-MM"`.
    pub bucket: String,
    pub record_count: u32,
    /// Count of tombstoned records *within* this live segment (not
    /// whole-segment tombstoning).
    pub tombstone_count: u32,
    pub size_bytes: u64,
    pub created_at_secs: u64,
    /// `true` for the currently-being-appended segment in its bucket.
    /// Per parent spec D7.
    pub is_open: bool,
    /// BLAKE3 of the segment's `.meta` sidecar, hex — the other half of the
    /// pair (`message-segment-store.md` § Segment file format: `record_order`
    /// and the per-record floor metadata live ONLY there, so a segment cannot
    /// be reopened without it). Carried so the backup corpus's
    /// `manifest.<kind>` mirror anchors **both** halves and a custodian can
    /// verify the sidecar in transit exactly as it verifies the `.dat`.
    ///
    /// Required: a ref without it is refused at decode, so no reader ever
    /// skips the sidecar check. A finalized pair is immutable (compaction
    /// mints a new id), so this never changes while `blake3_hex` stands still.
    pub meta_blake3_hex: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.segments.compact` — manual compaction request.
///
/// Scope semantics:
/// - `kind = Some("mail" | "calendar" | …)` restricts to one segment-store
///   kind; `None` runs every kind.
/// - `actor_id = Some(_)` restricts to one actor (bearer must be that
///   actor or a nest admin); `None` runs whole-nest (admin-only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactRequest {
    pub kind: Option<String>,
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub actor_id: Option<ActorId>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactReply {
    /// Number of segments physically rewritten by this run.
    pub segments_rewritten: u32,
    /// Number of per-segment compaction errors (logged; not surfaced
    /// individually). `0` on a clean run.
    pub errors: u32,
    /// Echo of the resolved scope (verbatim from the request).
    pub scope: CompactScopeEcho,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactScopeEcho {
    pub kind: Option<String>,
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub actor_id: Option<ActorId>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ────────────────────────────────────────────────────────────────────
// Destination path layout + the manifest mirror
// ────────────────────────────────────────────────────────────────────
//
// Moved here from `fauna_sync_engine::segment_backup` on 2026-09-28 (and
// re-exported there): the audit loop reads the mirror and the layout from
// wasm-clean shared Rust the sync engine cannot be a dependency of.

// ────────────────────────────────────────────────────────────────────
// Destination path layout
// ────────────────────────────────────────────────────────────────────

/// The destination-side path of one backed-up segment's `.dat`, inside the
/// owner's reserved custody-copy set.
///
/// This layout is a **contract between five parties that never share a call
/// stack** — the nest coordinator that writes it, the client-device custodian
/// that pulls it, the re-seed leg that delivers it, the audit loop that samples
/// it, and the removal teardown that tombstones it — so a divergence would not
/// fail a build, it would silently address different bytes. It lives here,
/// next to [`SegmentRef`] and [`LiveManifestMirror`] (and re-exported from
/// `fauna_sync_engine::segment_backup`, the shared leaf set both backup arms
/// reuse verbatim), because the audit loop that reads it is wasm-clean shared
/// Rust the sync engine cannot be a dependency of. The read side
/// of the contract is [`parse_segment_rel_path`] — never a hand-rolled
/// prefix/suffix match.
pub fn segment_rel_path(scope_hex: &str, segment_id: u32) -> String {
    format!("{scope_hex}/seg-{segment_id:08}.dat")
}

/// The destination-side path of one backed-up segment's `.meta` sidecar — the
/// sibling of [`segment_rel_path`], same set, same segment, `.meta` for `.dat`
/// exactly as on the source's disk.
///
/// An ordinary custody path in every respect: latest-per-path liveness, the
/// grace window T, GC and the custodian's cap all see one more path per
/// segment and nothing new. Introduced 2026-08-29 (`message-segment-store.md`
/// § Client-device custodian (pull) → *Restore*): without it the corpus held
/// `.dat` files nothing could reopen, because the record footers live only in
/// the sidecar.
pub fn segment_meta_rel_path(scope_hex: &str, segment_id: u32) -> String {
    format!("{scope_hex}/seg-{segment_id:08}.meta")
}

/// Which file of a segment pair a custody path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentHalf {
    /// `seg-NNNNNNNN.dat` — the CARv2 container.
    Dat,
    /// `seg-NNNNNNNN.meta` — the dag-cbor sidecar.
    Meta,
}

/// Read a custody path back into the `(segment_id, half)` that
/// [`segment_rel_path`] / [`segment_meta_rel_path`] derived it from for
/// `scope_hex`, or `None` for anything else (another scope, the mirror, a
/// folder-mirror row, a non-canonical spelling such as `seg-7.dat`).
///
/// Parse-then-re-derive, like `parse_reserved_backup_set_name`: whatever
/// this returns, feeding it back through the formatter reproduces the input
/// exactly, so the read side can never quietly accept a shape the write side
/// never emits. Before 2026-08-29 three consumers (the custodian pull, the
/// re-seed leg, the audit's stratum key) each hand-parsed this layout; a fourth
/// file per segment is exactly the change that would have drifted them.
pub fn parse_segment_rel_path(scope_hex: &str, path: &str) -> Option<(u32, SegmentHalf)> {
    let rest = path.strip_prefix(scope_hex)?.strip_prefix("/seg-")?;
    let (id_text, half) = match rest.strip_suffix(".dat") {
        Some(id) => (id, SegmentHalf::Dat),
        None => (rest.strip_suffix(".meta")?, SegmentHalf::Meta),
    };
    let id: u32 = id_text.parse().ok()?;
    let canonical = match half {
        SegmentHalf::Dat => segment_rel_path(scope_hex, id),
        SegmentHalf::Meta => segment_meta_rel_path(scope_hex, id),
    };
    (canonical == path).then_some((id, half))
}

/// Which half of a backed-up message kind a custody path belongs to.
///
/// **A backed-up message kind's corpus is its content segments AND its
/// placement journal** — the same pair a message-kind snapshot pins, for the
/// same reason: a content record holds only what its id determines, and
/// everything mutable about it (which mailbox, which flags, which UID) lives
/// in the journal. A corpus that carries the content alone restores mail that
/// sits in no mailbox. Owner of the rule:
/// `docs/goal/behavior/backup-destinations.md` § Third destination kind →
/// *Where restored mail lands*; of these paths:
/// `docs/goal/architecture/segment-backup-protocol.md` § Client-device
/// custodian (pull) → *Restore* → *The placement journal rides the set*.
///
/// Both families rest in the kind's **one** reserved set. The journal is
/// deliberately not a second entry in `BACKED_UP_KINDS`, and the reason is
/// structural rather than a preference:
///
/// - a client-device custodian's store qualifies a row by its set, and both
///   families share the kind's one set and number their segments from 1, so
///   within that set they need distinct paths whether or not they are
///   distinct kinds — hence the infix;
/// - the check-in's `high_water`, the status row's backlog and the pass count
///   are all stated per kind, and a journal has no head of its own worth
///   reporting. The kind's pass moves both halves and checks in once.
///
/// Every mover (the nest coordinator, the custodian pull, the re-seed leg, the
/// materialize verb) is parameterised by this one value rather than by a
/// second copy of each path function, so a fifth path per segment cannot drift
/// between the families the way three hand-parsers once drifted over the
/// sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SegmentFamily {
    /// The kind's content segments: `{scope_hex}/seg-NNNNNNNN.{dat,meta}` and
    /// the mirror `{scope_hex}/manifest.<kind>`. The layout every held corpus
    /// already uses; it never moves.
    Content,
    /// The kind's placement journal:
    /// `{scope_hex}/placement/seg-NNNNNNNN.{dat,meta}` and the mirror
    /// `{scope_hex}/placement/manifest.<kind>`.
    Placement,
}

impl SegmentFamily {
    /// The order every pass walks the families in: **content first, journal
    /// second**. A journal entry naming a record the copy does not hold yet is
    /// a row the read path never serves, and a record the journal does not
    /// name yet is unfiled until the next pass. Neither is a torn state, but
    /// content-first is the order under which the copy is never *missing* a
    /// record an older journal entry names.
    pub const PASS_ORDER: [SegmentFamily; 2] = [SegmentFamily::Content, SegmentFamily::Placement];

    /// The path prefix every file of this family shares inside the set,
    /// trailing slash included.
    fn prefix(self, scope_hex: &str) -> String {
        match self {
            SegmentFamily::Content => format!("{scope_hex}/"),
            SegmentFamily::Placement => format!("{scope_hex}/placement/"),
        }
    }

    /// The path of one segment's `.dat`.
    pub fn dat_path(self, scope_hex: &str, segment_id: u32) -> String {
        format!("{}seg-{segment_id:08}.dat", self.prefix(scope_hex))
    }

    /// The path of one segment's `.meta` sidecar.
    pub fn meta_path(self, scope_hex: &str, segment_id: u32) -> String {
        format!("{}seg-{segment_id:08}.meta", self.prefix(scope_hex))
    }

    /// The path of this family's live-segment mirror. `kind` is the **backed-up
    /// kind** (`mail`), never the journal's serve tag: the infix already says
    /// which half this is.
    pub fn mirror_path(self, scope_hex: &str, kind: &str) -> String {
        format!("{}manifest.{kind}", self.prefix(scope_hex))
    }

    /// Read a custody path back into the backed-up `kind` whose mirror
    /// [`Self::mirror_path`] derived it for, or `None` for anything else — a
    /// segment path, the other family's mirror, a kind the reserved-set
    /// derivation does not know.
    ///
    /// Parse-then-re-derive like [`Self::parse`], and additionally closed over
    /// [`BACKUP_SET_KINDS`]: a destination's custody row is the audited
    /// party's own text, so a path that merely *looks* like a mirror
    /// (`…/manifest.anything`) must not make the reader fetch and decode an
    /// arbitrary blob as the set's ledger.
    pub fn parse_mirror_path(self, scope_hex: &str, path: &str) -> Option<&'static str> {
        let kind = path
            .strip_prefix(self.prefix(scope_hex).as_str())?
            .strip_prefix("manifest.")?;
        let kind = BACKUP_SET_KINDS.iter().copied().find(|k| *k == kind)?;
        (self.mirror_path(scope_hex, kind) == path).then_some(kind)
    }

    /// Read a custody path back into the `(segment_id, half)` this family's
    /// formatters derived it from, or `None` for anything else — the other
    /// family's paths and both mirrors included.
    ///
    /// Parse-then-re-derive, the discipline [`parse_segment_rel_path`] set:
    /// whatever this returns, the formatter reproduces the input exactly.
    pub fn parse(self, scope_hex: &str, path: &str) -> Option<(u32, SegmentHalf)> {
        let rest = path
            .strip_prefix(self.prefix(scope_hex).as_str())?
            .strip_prefix("seg-")?;
        let (id_text, half) = match rest.strip_suffix(".dat") {
            Some(id) => (id, SegmentHalf::Dat),
            None => (rest.strip_suffix(".meta")?, SegmentHalf::Meta),
        };
        let id: u32 = id_text.parse().ok()?;
        let canonical = match half {
            SegmentHalf::Dat => self.dat_path(scope_hex, id),
            SegmentHalf::Meta => self.meta_path(scope_hex, id),
        };
        (canonical == path).then_some((id, half))
    }

    /// The kind tag this family of `kind` is listed and fetched under on the
    /// serve plane (`fauna.segments.list`, the byte routes, a
    /// `SegmentSource`), and the key its per-destination upload state is
    /// filed under. `None` when `kind` has no such family: a kind with no
    /// placement layer (`conv`, `post`) has no journal to move.
    ///
    /// The journal tags are the journals' own on-disk scope kinds, spelled out
    /// rather than derived: calendar's is `calendar-placement` while its
    /// manifest label is `cal-placement`, so a suffixing rule would be right
    /// for mail and wrong for the next kind to join the sweep.
    pub fn serve_kind(self, kind: &str) -> Option<&'static str> {
        let known = BACKUP_SET_KINDS.iter().copied().find(|k| *k == kind)?;
        match self {
            SegmentFamily::Content => Some(known),
            SegmentFamily::Placement => match known {
                "mail" => Some("mail-placement"),
                "calendar" => Some("calendar-placement"),
                "card" => Some("card-placement"),
                _ => None,
            },
        }
    }

    /// The inverse of [`Self::serve_kind`]: which family of which backed-up
    /// kind a serve tag names, or `None` for a tag nothing derives.
    ///
    /// A mover's per-destination upload state is filed under the serve tag, so
    /// anything that reads those rows back (the removal teardown) has only the
    /// tag in hand and needs both halves of what it meant: the family for the
    /// path, the kind for the set the custody rests in.
    pub fn from_serve_kind(tag: &str) -> Option<(SegmentFamily, &'static str)> {
        Self::PASS_ORDER.iter().find_map(|family| {
            BACKUP_SET_KINDS
                .iter()
                .copied()
                .find(|kind| family.serve_kind(kind) == Some(tag))
                .map(|kind| (*family, kind))
        })
    }
}

/// Every kind `reserved_backup_set_name` answers for.
///
/// Distinct from `BACKED_UP_KINDS`, which is what the *sweep* backs up (every
/// kind here but `conv`): this is what the **name derivation** knows, i.e.
/// which set names are legitimately reserved-backup-set-shaped. A kind reaches
/// `BACKED_UP_KINDS` long after its name is derivable.
pub const BACKUP_SET_KINDS: &[&str] = &["mail", "post", "calendar", "card", "conv"];

/// The destination-side path of the `manifest.<kind>` mirror — the same contract
/// as [`segment_rel_path`], for the one non-segment path in a backup set.
pub fn manifest_rel_path(scope_hex: &str, kind: &str) -> String {
    format!("{scope_hex}/manifest.{kind}")
}

// ────────────────────────────────────────────────────────────────────
// Manifest mirror
// ────────────────────────────────────────────────────────────────────

/// Destination-side mirror of the source's per-actor KindManifest
/// (`live` list). Uploaded as `{actor_hex}/manifest.{kind}` whenever
/// any segment was (re)uploaded or the canonical hash of the source's
/// `segments` reply changes.
///
/// Wire format: **canonical dag-cbor** (`fauna_core::encoding::canonical_encode`
/// / `canonical_decode`) — the spec allows either CBOR or postcard, and
/// canonical dag-cbor is the project's one at-rest encoder (serialization.md
/// § Goal). `SegmentRef` has a `serde(flatten)` `extra: BTreeMap` field;
/// `serde_ipld_dagcbor` round-trips flatten under strict canonical decode
/// (proven by fauna-cbor's `serde_flatten_btreemap_round_trips_canonically`),
/// which is why this no longer needs ciborium.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveManifestMirror {
    /// The source's saved segment counter at the moment this ledger was
    /// written — [`SegmentsListReply::next_segment_id`], its `KindManifest::
    /// next_seg_id`, which never decreases while the source's data directory
    /// lives.
    ///
    /// This is the ledger's **generation**: the client audit's inclusion arm
    /// remembers the highest value it verified per (destination, set, family)
    /// and treats a destination serving a lower one as a rollback to an older
    /// genuine ledger — unless the owner's own source nest confirms it is the
    /// one that regressed (`fauna_client_backup::audit`, the generation pin;
    /// `backup-restore.md` § Background Tasks → *Implementation status (audit
    /// loop)*, seventh bullet). Its original consumer, a destination-side
    /// churn refusal on compaction-driven shrink (Plan 5 spec § D2 step 5),
    /// was never built.
    pub next_segment_id_seen: u32,
    pub live: Vec<SegmentRef>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl LiveManifestMirror {
    /// Serialize to canonical dag-cbor bytes for destination upload.
    pub fn to_bytes(&self) -> fauna_core::error::Result<Vec<u8>> {
        fauna_core::encoding::canonical_encode(self)
    }

    /// Deserialize from canonical dag-cbor bytes — the custodian pull, the
    /// materialize verb and the audit loop's population anchor all read it.
    pub fn from_bytes(bytes: &[u8]) -> fauna_core::error::Result<Self> {
        fauna_core::encoding::canonical_decode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    /// The mirror parser is closed over the kinds the derivation knows and over
    /// its own family: a destination's custody path is the audited party's own
    /// text, so nothing that merely looks like a mirror may parse as one.
    #[test]
    fn a_mirror_path_parses_back_to_its_kind_and_nothing_else_does() {
        let scope = "ab".repeat(32);
        for family in SegmentFamily::PASS_ORDER {
            assert_eq!(
                family.parse_mirror_path(&scope, &family.mirror_path(&scope, "mail")),
                Some("mail")
            );
            assert_eq!(
                family.parse_mirror_path(&scope, &family.dat_path(&scope, 7)),
                None,
                "a segment is not a mirror"
            );
            assert_eq!(
                family.parse_mirror_path(&scope, &format!("{scope}/manifest.bogus")),
                None,
                "a kind the derivation does not know"
            );
        }
        assert_eq!(
            SegmentFamily::Content.parse_mirror_path(
                &scope,
                &SegmentFamily::Placement.mirror_path(&scope, "mail")
            ),
            None,
            "the other family's mirror"
        );
        assert_eq!(
            SegmentFamily::Placement
                .parse_mirror_path(&scope, &SegmentFamily::Content.mirror_path(&scope, "mail")),
            None
        );
    }

    #[test]
    fn list_request_round_trips() {
        let req = SegmentsListRequest {
            kind: "mail".to_string(),
            actor_id: "11".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SegmentsListRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn compact_request_round_trips_with_full_scope() {
        let req = CompactRequest {
            kind: Some("mail".to_string()),
            actor_id: Some(ActorId([0x33u8; 32])),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: CompactRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn compact_request_round_trips_whole_nest() {
        let req = CompactRequest {
            kind: None,
            actor_id: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: CompactRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn compact_reply_round_trips() {
        let reply = CompactReply {
            segments_rewritten: 3,
            errors: 0,
            scope: CompactScopeEcho {
                kind: Some("mail".to_string()),
                actor_id: Some(ActorId([0x44u8; 32])),
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: CompactReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn list_reply_round_trips() {
        let reply = SegmentsListReply {
            segments: vec![SegmentRef {
                segment_id: 1,
                blake3_hex: "aa".repeat(32),
                bucket: "2026-05".to_string(),
                record_count: 42,
                tombstone_count: 1,
                size_bytes: 12345,
                created_at_secs: 1700000000,
                is_open: false,
                meta_blake3_hex: "bb".repeat(32),
                extra: BTreeMap::new(),
            }],
            next_segment_id: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SegmentsListReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    /// A ref without the sidecar hash is refused at decode: a listing or
    /// mirror that could omit it would let the least trusted party on the wire
    /// switch the sidecar's in-transit check off.
    #[test]
    fn a_ref_without_a_sidecar_hash_is_refused() {
        let mut as_map = BTreeMap::<String, Value>::new();
        as_map.insert("segment_id".into(), Value::Integer(3));
        as_map.insert("blake3_hex".into(), Value::String("cc".repeat(32)));
        as_map.insert("bucket".into(), Value::String("2026-08".into()));
        as_map.insert("record_count".into(), Value::Integer(1));
        as_map.insert("tombstone_count".into(), Value::Integer(0));
        as_map.insert("size_bytes".into(), Value::Integer(10));
        as_map.insert("created_at_secs".into(), Value::Integer(1));
        as_map.insert("is_open".into(), Value::Bool(false));
        let bytes = encode_canonical(&as_map).unwrap();
        assert!(decode::<SegmentRef>(&bytes).is_err());
    }
}
