//! `fauna.peer.share.*` — the cross-user share leg's Y.1 kinds over the
//! contact-plane peer channel (`docs/goal/behavior/p2p-shared-set-build.md` § Cross-user
//! shared-set transfer → *Build contract*, the W8 (account-data-plane.md § Workstreams) share twin).
//!
//! **A DISJOINT sibling of `fauna.peer.sync.*`, never an extension of it.**
//! The same-account leg's kind family ships in every flavor (store-safe
//! included); this family is the `p2p-share` registry feature's data plane
//! and compiles away with the `p2p-share` cargo feature
//! (`dynamic-features.md` § Compile-time excision) — the store-safe witness
//! greps `fauna.peer.share.` for absence, which only a separate family can
//! make provable (wormability walk rule 5, both legs' records).
//!
//! The family, allowlisted by the share serve set and nothing else
//! (wormability rule 3 — no config, capability-mint, admin, or key-material
//! kinds; content keys ride only the M2 rail):
//!
//! - the **admission exchange** ([`KIND_PEER_SHARE_ADMIT`]) — the requester
//!   CLAIMS shared sets by channel id; the responder verifies each claim
//!   against its own local MLS roster for that set (the channel-proven actor
//!   key IS the roster entry — PT-1b: contact-plane pairs dial by actor
//!   key), yielding a `Named` folder-scope verdict through the admission
//!   seam's verdict core. The witness is a *claim, not a certificate*: no
//!   envelope travels for [`WITNESS_M2_MEMBERSHIP`], and the roster consult
//!   is the evaluator's own store — offline-available, never a registry
//!   lookup.
//! - the **set change-log read** ([`KIND_PEER_SHARE_CHANGES_LIST`]), the
//!   **manifest fetch** ([`KIND_PEER_SHARE_MANIFESTS_GET`]), and the
//!   **want-list chunk pull** ([`KIND_PEER_SHARE_CHUNKS_PULL`]) — the data
//!   kinds. Their payload structs land with the serve/pull core; the kind strings are declared here so
//!   the family is one grep surface. Provenance split (ruled 2026-08-17 —
//!   p2p.md § *Peer-served change-row provenance*): manifests + chunks are
//!   hash-verified and multi-source; change ROWS are served and accepted
//!   only for the serving peer's OWN authorship (channel-proven), each row
//!   marked pending-vs-sequenced, ingested as a provisional read-side
//!   overlay the nest later confirms.
//!
//! **Pre-auth discipline (rule 2).** [`KIND_PEER_SHARE_ADMIT`] is pre-auth
//! surface: strict dag-cbor, no hand-rolled parsing, and every struct here
//! joins `fauna_peer_channel::hardening::KIND_PAYLOAD_COVERAGE` with corpus
//! entries in the same change that makes it reachable.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;
use crate::sync::SyncChange;

/// Peer-channel kind: the share leg's admission exchange (see module docs).
pub const KIND_PEER_SHARE_ADMIT: &str = "fauna.peer.share.admit";
/// Peer-channel kind: the admitted set's change-log delta read (struct lands
/// with the serve core — module docs).
pub const KIND_PEER_SHARE_CHANGES_LIST: &str = "fauna.peer.share.changes.list";
/// Peer-channel kind: manifest fetch by manifest hash (struct lands with the
/// serve core — module docs).
pub const KIND_PEER_SHARE_MANIFESTS_GET: &str = "fauna.peer.share.manifests.get";
/// Peer-channel kind: want-list chunk pull by ciphertext store key (struct
/// lands with the serve core — module docs).
pub const KIND_PEER_SHARE_CHUNKS_PULL: &str = "fauna.peer.share.chunks.pull";

/// The M2-membership witness kind — the string
/// `fauna_protocol::peer_sync`'s witness table reserves for the cross-account
/// twin. A **claim, not a certificate**: the "witness" is the claimed set
/// list on [`PeerShareAdmitRequest`] itself; verification is the responder's
/// local roster consult against the channel-proven actor key.
pub const WITNESS_M2_MEMBERSHIP: &str = "m2-membership";

/// The group-scope membership witness kind — the admission seam's **fourth**
/// kind (`account-data-plane.md` § The admission seam), whose form T20 ruled
/// 2026-08-17. Unlike [`WITNESS_M2_MEMBERSHIP`] this one IS a certificate:
/// the member's `Enrolled` roster entry travels inline (the seam's
/// self-contained-carriage rule), and the evaluator additionally consults its
/// own merged roster frontier for a superseding `Removed`. Verdict scope is
/// exactly the named group scope.
pub const WITNESS_GROUP_MEMBERSHIP: &str = "group-membership";

/// `RpcError` code: a share-transfer request arrived on a connection with no
/// admission verdict for the named set. The remedy is to (re-)present the
/// claim via [`KIND_PEER_SHARE_ADMIT`].
pub const ERR_NOT_ADMITTED: &str = "fauna.peer.share.not_admitted";
/// `RpcError` code: no claimed set admitted the channel-proven actor (the
/// roster consult found nothing).
pub const ERR_WITNESS_REFUSED: &str = "fauna.peer.share.witness_refused";
/// `RpcError` code: a per-peer / per-window quota refused the request
/// (wormability rule 8 — covers admission-refused attempts too).
pub const ERR_OVER_QUOTA: &str = "fauna.peer.share.over_quota";
/// `RpcError` code: the request named a shape this build's share serve does
/// not carry. Loud by design — an empty page would read as "converged" to a
/// puller.
pub const ERR_UNSUPPORTED: &str = "fauna.peer.share.unsupported";

// ── fauna.peer.share.admit ───────────────────────────────────────────────────

/// `fauna.peer.share.admit` request — the requester claims shared sets. The
/// responder evaluates each claimed set against its own roster state for the
/// channel-proven actor key and holds a `Named` folder-scope verdict over
/// the admitted subset; sets it cannot vouch for are simply not admitted
/// (later use answers [`ERR_NOT_ADMITTED`]), so a partially-stale claim
/// degrades instead of failing whole.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerShareAdmitRequest {
    /// Which witness kind this claim is — [`WITNESS_M2_MEMBERSHIP`] for the
    /// M2 share leg, [`WITNESS_GROUP_MEMBERSHIP`] for the group-scope kind
    /// (whose material rides [`Self::group_witnesses`]). A verifier refuses
    /// a kind it does not implement (never guesses from shape); the claim
    /// list stays this exchange's common vocabulary.
    pub witness_kind: String,
    /// The claimed sets — each a 32-byte derived MLS `ChannelId`, exactly
    /// the id that keys the set's group, custody envelope, and
    /// `fauna.folders.*` wire. The M2 kind's material; empty for the group
    /// kind (a certificate is carried, never claimed).
    pub claimed_sets: Vec<ByteBuf>,
    /// The group-membership kind's material — the additive slot the request
    /// doc always promised: one carried certificate per claimed group scope.
    /// Additive (`default`), so a request claiming no group scope carries none.
    #[serde(default)]
    pub group_witnesses: Vec<PeerShareGroupWitness>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One carried group-membership witness ([`WITNESS_GROUP_MEMBERSHIP`]): the
/// member's `Enrolled` roster record for one group scope, canonical bytes
/// VERBATIM (the evaluator decodes and verifies it against its OWN copy of
/// the group's authority root — `fauna_peer_share::admission::
/// verdict_for_group_membership` owns the check order).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareGroupWitness {
    /// The group scope's content-derived 32-byte id.
    pub scope_id: ByteBuf,
    /// The member's `Enrolled` roster record, canonical bytes verbatim.
    pub entry: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.admit` reply — the responder's answer about the
/// requester's claim, plus the responder's OWN claim so one round trip
/// yields mutual (independently evaluated) admission, exactly as the
/// same-account exchange does. A responder that admits *nothing* answers
/// the Y.1 `ok = false` reply ([`ERR_WITNESS_REFUSED`]) instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerShareAdmitReply {
    /// Which of the requester's claimed sets the responder admitted — the
    /// dialer plans its pulls from this instead of probing into
    /// [`ERR_NOT_ADMITTED`]. Advisory: the verdict held server-side is the
    /// enforcement, this list only reports it.
    pub admitted_sets: Vec<ByteBuf>,
    /// The responder's own witness kind for the mutual direction.
    pub witness_kind: String,
    /// The responder's own claimed sets, which the requester evaluates
    /// against ITS roster state (sides admit independently; their witness
    /// kinds may differ).
    pub claimed_sets: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.share.changes.list ────────────────────────────────────────────

/// `fauna.peer.share.changes.list` request — read the named set's change-log
/// delta from an admitted member's own replica.
///
/// The set is named explicitly (never inferred from the connection): one
/// connection may be admitted to several sets, and every request re-checks its
/// named set against the verdict (`fauna_peer_share::verdict_admits_set`) — the
/// admission seam's "the core's only admission duty is scope enforcement".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChangesListRequest {
    /// The set's derived 32-byte MLS `ChannelId`.
    pub set: ByteBuf,
    /// Return rows with `seq` strictly greater than this (the catch-up cursor,
    /// same meaning as `fauna.sync.changes.list`'s `since`).
    #[serde(default)]
    pub since: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One served change row, with the marking the provenance ruling requires
/// (`p2p.md` § *Peer-served change-row provenance*).
///
/// **Self-contained** (`mls-group-key-material.md` § M2 → *Writer-signed change
/// records*, ruling (3)): a writer-signed row verifies on its own — its
/// `signature`/`signer_key` pair, plus [`Self::signer_cert`] for a delegated
/// signer — so a peer serves every row it holds, its own and other writers'
/// alike, and the receiver judges each through the one shared reader
/// ([`crate::sync_row_verify`]). An unsigned row has only the channel's proof
/// (PT-1b), which admits nothing outside the signature check's class
/// exemptions: every writer signs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChange {
    /// The row itself — the same shape the nest serves
    /// ([`crate::sync::SyncChange`]), so a receiver folds a peer-served row
    /// through exactly one row vocabulary.
    pub change: SyncChange,
    /// `true` ⇒ the serving replica holds this row **nest-sequenced** (some
    /// nest stamped and ordered it). `false` ⇒ no nest has sequenced it yet (a
    /// writer's offline-authored pending row, the serving peer's or one it
    /// relays) — a receiver may overlay it provisionally but must never treat
    /// it as converged log.
    pub sequenced: bool,
    /// The row's delegated signer cert, inline — the embed-as-bytes
    /// `DeviceAuthorization` its `signer_key` chains to the signed actor
    /// through. Absent for a direct (identity-key) signer and on an unsigned
    /// row. Inline per row rather than a page side table so the row stays
    /// self-contained across every hop (the relay's retention, the app → agent
    /// ingest door).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_cert: Option<fauna_core::encoding::EmbedAsBytes>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.changes.list` reply — one page of the rows the serving
/// peer holds for the set.
///
/// Deliberately carries no serving-peer field: echoing the serving peer's
/// identity would invite a receiver to believe the echo — a row's writer is its
/// verified signature, or (unsigned) the channel's proof (PT-1b), never a
/// claim on the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChangesListReply {
    pub changes: Vec<PeerShareChange>,
    /// `true` ⇒ the page stopped at a serve bound with rows still to come;
    /// re-request from the last `seq`. Explicit so a bounded page is never
    /// mistaken for convergence (the same reason the pull reply splits
    /// `missing` from `deferred`).
    #[serde(default)]
    pub more: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.share.manifests.get ───────────────────────────────────────────

/// `fauna.peer.share.manifests.get` request — fetch chunk manifests by their
/// own content hash. Multi-source safe: the reply is verified against the
/// requested hash, so *who* served it carries no weight (rule 4).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareManifestsGetRequest {
    /// The set whose members may ask for these manifests (scope enforcement).
    pub set: ByteBuf,
    /// The 32-byte manifest content hashes, most-wanted first.
    pub manifest_hashes: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One served manifest — canonical-encoded [`fauna_core::chunk::ChunkManifest`]
/// bytes, still exactly as any store holds them.
///
/// The receiver re-hashes [`Self::bytes`] against the hash it asked for before
/// use; a substituted manifest fails that check (rule 4's inert-content rule,
/// and the same anchor `fauna_core::file_download` already applies to a
/// nest-served manifest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerShareManifest {
    pub hash: ByteBuf,
    pub bytes: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.manifests.get` reply — the three disjoint outcomes per
/// requested hash, so "doesn't have it" is never conflated with "didn't fit
/// this frame" (the same shape `fauna.peer.sync.blocks.pull` settled on).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareManifestsGetReply {
    pub manifests: Vec<PeerShareManifest>,
    /// This peer cannot produce it — try another member or the nest.
    pub missing: Vec<ByteBuf>,
    /// Held, but over this reply's frame budget — re-request in a smaller want
    /// list.
    pub deferred: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.share.chunks.pull ─────────────────────────────────────────────

/// One entry of the want list: a chunk, and how much of it the requester
/// already holds.
///
/// **Why chunk pulls are RANGED and manifest fetches are not.** A chunk body is
/// 512 KiB–8 MiB (`fauna_core::chunker`: anything under the 8 MiB single-chunk
/// threshold is ONE chunk), while a peer-channel frame caps at 1 MiB
/// (`fauna_peer_channel::MAX_FRAME_LEN`). So an ordinary file's chunk **cannot
/// cross this channel in one reply** — the whole-body shape the same-account
/// leg uses for its small CBOR blocks does not transfer a holiday video, which
/// is the very scenario this leg exists for. Measured 2026-08-17 during the
/// slice-B build (`an_ordinary_multi_megabyte_file_transfers`). Manifests are
/// bounded by their chunk-hash list and stay whole-body.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChunkWant {
    /// The 32-byte store key (the ciphertext hash a sealed chunk is
    /// content-addressed under — `fauna_core::chunk::ChunkManifest::store_key`).
    pub store_key: ByteBuf,
    /// Byte offset into the stored body to resume from; `0` for a fresh want.
    /// An offset past the body's end is a protocol error
    /// ([`ERR_UNSUPPORTED`]), never a silent empty slice — an empty slice would
    /// read as "converged" to a puller.
    #[serde(default)]
    pub offset: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.chunks.pull` request — the ranged want list.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChunksPullRequest {
    /// The set whose members may ask for these chunks (scope enforcement).
    pub set: ByteBuf,
    /// The wants, most-wanted first.
    pub wants: Vec<PeerShareChunkWant>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One served **slice** of a chunk body, verbatim as stored — still sealed,
/// still compression-framed. Opening is the receiver's job under the set's own
/// M2 content key, which never rides this plane.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChunk {
    /// The store key the **completed** body must hash to. Verification is on
    /// completion rather than per slice (a slice has no address of its own), and
    /// still strictly before the body is used or stored — rule 4.
    pub store_key: ByteBuf,
    /// Where this slice starts in the body.
    #[serde(default)]
    pub offset: u64,
    /// The slice.
    pub bytes: ByteBuf,
    /// The whole body's length, so a puller knows when it holds all of it
    /// (`offset + bytes.len() == total_len`) without a separate probe.
    #[serde(default)]
    pub total_len: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.chunks.pull` reply — served / missing / deferred, exactly
/// as the manifest reply.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareChunksPullReply {
    pub chunks: Vec<PeerShareChunk>,
    pub missing: Vec<ByteBuf>,
    pub deferred: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.share.ceremony.* ──────────────────────────────────────────────
//
// The offline share-initiation ceremony's carriage (`p2p.md` § Offline share
// initiation; row 62 slice 3's transport half). Every ceremony leg is
// INITIATOR-ORIGINATED over one dialed contact-plane channel — the recipient's
// side is purely reactive (the accept side of `PeerNode` serves, it never
// originates), so the recipient's accept crosses as the poll kind's reply once
// their user consents; "not consented yet" is a `None` reply, never an error.
//
// Admission is the actor-level pre-parse gate behind
// `fauna_peer_share::ceremony::CeremonyState` — a live receive-act expectation
// or an in-flight ceremony record naming the channel-proven actor (the
// § Inbound authorization carve-out: a stronger per-kind admission of the
// ceremony's own). The frames themselves are `fauna_core::group_ceremony`
// envelopes, signature-verified against the channel-proven sender at ingest;
// this carriage never re-encodes them.

/// Peer-channel kind: push one ceremony **offer** frame to the recipient.
pub const KIND_PEER_SHARE_CEREMONY_OFFER: &str = "fauna.peer.share.ceremony.offer";
/// Peer-channel kind: poll the recipient for the owed **accept** frame (the
/// consent gap lives between the offer's ack and this reply turning `Some`).
pub const KIND_PEER_SHARE_CEREMONY_ACCEPT_POLL: &str = "fauna.peer.share.ceremony.accept.poll";
/// Peer-channel kind: push the **deliver** frame (admission bundle + machinery
/// snapshot, sealed to the recipient's reception key — the T20 rail).
pub const KIND_PEER_SHARE_CEREMONY_DELIVER: &str = "fauna.peer.share.ceremony.deliver";

/// `RpcError` code: no live expectation or in-flight ceremony record admits
/// the channel-proven actor for the ceremony kinds — refused BEFORE any
/// payload parsing (wormability rule 1). Also the fail-closed answer when the
/// serving side has no ceremony state wired at all.
pub const ERR_CEREMONY_NOT_EXPECTED: &str = "fauna.peer.share.ceremony.not_expected";
/// `RpcError` code: the ceremony state refused the step — a frame failing
/// verification, a step out of order, or a declined invitation (the poll's
/// terminal answer, so an initiator never polls a dead ceremony forever).
pub const ERR_CEREMONY_REFUSED: &str = "fauna.peer.share.ceremony.refused";

/// `fauna.peer.share.ceremony.{offer,deliver}` request — one ceremony frame,
/// verbatim (`fauna_core::group_ceremony::encode_group_ceremony_message`
/// bytes; the signatures inside cover exactly that encoding).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareCeremonyFrameRequest {
    /// The encoded `GroupCeremonyMessage`.
    pub frame: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.ceremony.{offer,deliver}` reply — the frame was verified
/// and recorded (record-then-act: recording IS the outcome; any refusal is an
/// `ok = false` reply instead).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareCeremonyFrameAck {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.ceremony.accept.poll` request — ask for the owed accept
/// frame of the named scope's ceremony.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareCeremonyAcceptPollRequest {
    /// The offered scope's content-derived 32-byte id.
    pub scope_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.share.ceremony.accept.poll` reply — the accept frame once the
/// recipient's user has consented, else `None` (poll again; a *declined*
/// invitation answers [`ERR_CEREMONY_REFUSED`] instead, terminally).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerShareCeremonyAcceptPollReply {
    /// The encoded `GroupCeremonyMessage::Accept`, or `None` while consent is
    /// pending.
    pub frame: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    fn set_id(b: u8) -> ByteBuf {
        ByteBuf::from(vec![b; 32])
    }

    fn change_row(seq: i64, author: Option<&str>) -> SyncChange {
        SyncChange {
            seq,
            path_hash: "aa".repeat(32),
            manifest_hash: Some("bb".repeat(32)),
            size_bytes: 1234,
            change_type: "create".to_string(),
            created_at: 1_760_000_000,
            path: Some("holiday/clip.mp4".to_string()),
            device_id: None,
            content_key_version: Some(3),
            author_actor_id: author.map(|a| a.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn admit_request_round_trips_strict() {
        let req = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![set_id(0x4F), set_id(0x50)],
            group_witnesses: vec![PeerShareGroupWitness {
                scope_id: set_id(0x5C),
                entry: ByteBuf::from(vec![0xE7; 40]),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).expect("encode");
        let back: PeerShareAdmitRequest = decode_strict(&bytes).expect("strict decode");
        assert_eq!(back, req);
    }

    /// The additive-everywhere direction that matters for the new slot: an
    /// OLD encoder's request (no `group_witnesses` key at all) decodes on a
    /// NEW peer with the slot defaulted empty — never a refusal.
    #[test]
    fn an_old_admit_request_without_the_group_slot_still_decodes() {
        #[derive(Serialize)]
        struct OldAdmitRequest {
            witness_kind: String,
            claimed_sets: Vec<ByteBuf>,
        }
        let old = OldAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![set_id(0x4F)],
        };
        let bytes = encode_canonical(&old).expect("encode");
        let back: PeerShareAdmitRequest = decode_strict(&bytes).expect("strict decode");
        assert!(back.group_witnesses.is_empty());
        assert_eq!(back.claimed_sets, vec![set_id(0x4F)]);
    }

    #[test]
    fn admit_reply_round_trips_strict() {
        let reply = PeerShareAdmitReply {
            admitted_sets: vec![set_id(0x4F)],
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![set_id(0x51)],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        let back: PeerShareAdmitReply = decode_strict(&bytes).expect("strict decode");
        assert_eq!(back, reply);
    }

    /// The additive-compat slot: a future field lands in `extra` on an older
    /// decoder instead of failing the exchange (the wire's additive-everywhere
    /// rule).
    #[test]
    fn unknown_fields_land_in_extra_not_in_a_refusal() {
        let mut extra = BTreeMap::new();
        extra.insert("field-from-the-future".to_string(), Value::from(7u64));
        let req = PeerShareAdmitRequest {
            witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
            claimed_sets: vec![set_id(0x4F)],
            group_witnesses: Vec::new(),
            extra,
        };
        let bytes = encode_canonical(&req).expect("encode");
        let back: PeerShareAdmitRequest = decode_strict(&bytes).expect("strict decode");
        assert_eq!(back.extra.len(), 1, "the unknown field survives round-trip");
    }

    /// The family is one grep surface: every kind string carries the
    /// `fauna.peer.share.` prefix the store-safe absence column greps, and
    /// none collides with the same-account family.
    #[test]
    fn the_kind_family_is_disjoint_and_uniformly_prefixed() {
        for kind in [
            KIND_PEER_SHARE_ADMIT,
            KIND_PEER_SHARE_CHANGES_LIST,
            KIND_PEER_SHARE_MANIFESTS_GET,
            KIND_PEER_SHARE_CHUNKS_PULL,
            ERR_NOT_ADMITTED,
            ERR_WITNESS_REFUSED,
            ERR_OVER_QUOTA,
            ERR_UNSUPPORTED,
        ] {
            assert!(kind.starts_with("fauna.peer.share."), "{kind}");
            assert!(!kind.starts_with("fauna.peer.sync."), "{kind}");
        }
    }

    #[test]
    fn changes_list_round_trips_strict_with_the_sequenced_marking() {
        let reply = PeerShareChangesListReply {
            changes: vec![
                PeerShareChange {
                    change: change_row(7, Some(&"c1".repeat(32))),
                    sequenced: true,
                    ..Default::default()
                },
                PeerShareChange {
                    // A pending row: no nest has sequenced it yet.
                    change: change_row(0, None),
                    sequenced: false,
                    ..Default::default()
                },
            ],
            more: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        let back: PeerShareChangesListReply = decode_strict(&bytes).expect("strict decode");
        assert_eq!(back, reply);
        assert!(back.changes[0].sequenced);
        assert!(
            !back.changes[1].sequenced,
            "an own-pending row survives as pending — a receiver must be able to \
             tell it from converged log"
        );

        let req = PeerShareChangesListRequest {
            set: set_id(0x4F),
            since: 12,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).expect("encode");
        assert_eq!(
            decode_strict::<PeerShareChangesListRequest>(&bytes).expect("strict decode"),
            req
        );
    }

    #[test]
    fn manifests_get_round_trips_strict_with_all_three_outcomes() {
        let reply = PeerShareManifestsGetReply {
            manifests: vec![PeerShareManifest {
                hash: set_id(0xA1),
                bytes: ByteBuf::from(vec![0xA1; 64]),
                extra: BTreeMap::new(),
            }],
            missing: vec![set_id(0xB2)],
            deferred: vec![set_id(0xC3)],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        assert_eq!(
            decode_strict::<PeerShareManifestsGetReply>(&bytes).expect("strict decode"),
            reply
        );

        let req = PeerShareManifestsGetRequest {
            set: set_id(0x4F),
            manifest_hashes: vec![set_id(0xA1), set_id(0xB2)],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).expect("encode");
        assert_eq!(
            decode_strict::<PeerShareManifestsGetRequest>(&bytes).expect("strict decode"),
            req
        );
    }

    #[test]
    fn chunks_pull_round_trips_strict_with_all_three_outcomes() {
        // A mid-body slice: offset past zero, total_len larger than the slice —
        // the shape every ordinary file's chunk actually travels as.
        let reply = PeerShareChunksPullReply {
            chunks: vec![PeerShareChunk {
                store_key: set_id(0xD4),
                offset: 716_800,
                bytes: ByteBuf::from(vec![0xE5; 128]),
                total_len: 1_500_000,
                extra: BTreeMap::new(),
            }],
            missing: vec![set_id(0xE5)],
            deferred: vec![set_id(0xF6)],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        assert_eq!(
            decode_strict::<PeerShareChunksPullReply>(&bytes).expect("strict decode"),
            reply
        );

        let req = PeerShareChunksPullRequest {
            set: set_id(0x4F),
            wants: vec![
                PeerShareChunkWant {
                    store_key: set_id(0xD4),
                    offset: 0,
                    extra: BTreeMap::new(),
                },
                PeerShareChunkWant {
                    store_key: set_id(0xD5),
                    offset: 716_800,
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).expect("encode");
        assert_eq!(
            decode_strict::<PeerShareChunksPullRequest>(&bytes).expect("strict decode"),
            req
        );
    }

    /// An `offset`/`total_len` absent on the wire reads as 0 — the additive
    /// default that keeps a hypothetical older encoder's whole-body slice
    /// decodable (it would be offset 0, and `total_len` 0 then reads as
    /// "complete", which is the honest reading of a body with no declared
    /// length).
    #[test]
    fn the_range_fields_default_to_zero_when_absent() {
        let mut minimal = BTreeMap::new();
        minimal.insert("store_key".to_string(), Value::Bytes(vec![0xD4; 32]));
        minimal.insert("bytes".to_string(), Value::Bytes(vec![0x01; 4]));
        let encoded = encode_canonical(&minimal).expect("encode");
        let back: PeerShareChunk = decode_strict(&encoded).expect("strict decode");
        assert_eq!(back.offset, 0);
        assert_eq!(back.total_len, 0);
    }

    /// The inline cert is additive both ways: absent (a direct signer), the row encodes
    /// with no cert key; present, it round-trips strict, and a receiver that
    /// does not read it keeps it in `extra` rather than failing the page.
    #[test]
    fn the_inline_signer_cert_is_additive_on_the_wire() {
        let bare = PeerShareChange {
            change: change_row(7, Some(&"c1".repeat(32))),
            sequenced: true,
            ..Default::default()
        };
        #[derive(Serialize)]
        struct CertLess<'a> {
            change: &'a SyncChange,
            sequenced: bool,
        }
        assert_eq!(
            encode_canonical(&bare).expect("encode"),
            encode_canonical(&CertLess {
                change: &bare.change,
                sequenced: true,
            })
            .expect("encode"),
            "no cert ⇒ no cert key on the wire"
        );

        let with_cert = PeerShareChange {
            signer_cert: Some(fauna_core::encoding::EmbedAsBytes {
                envelope: vec![1; 100],
                bytes: vec![2; 8],
                signer_auth: None,
            }),
            ..bare
        };
        let bytes = encode_canonical(&with_cert).expect("encode");
        let back: PeerShareChange = decode_strict(&bytes).expect("strict decode");
        assert_eq!(back, with_cert);
    }

    /// The additive-everywhere rule on every data kind, not just the admit
    /// exchange: a future field lands in `extra` instead of failing the page.
    #[test]
    fn a_future_field_lands_in_extra_on_every_data_kind() {
        let mut extra = BTreeMap::new();
        extra.insert("field-from-the-future".to_string(), Value::from(9u64));

        let bytes = encode_canonical(&PeerShareChangesListReply {
            changes: Vec::new(),
            more: false,
            extra: extra.clone(),
        })
        .expect("encode");
        assert_eq!(
            decode_strict::<PeerShareChangesListReply>(&bytes)
                .expect("strict decode")
                .extra
                .len(),
            1
        );

        let bytes = encode_canonical(&PeerShareManifestsGetReply {
            extra: extra.clone(),
            ..Default::default()
        })
        .expect("encode");
        assert_eq!(
            decode_strict::<PeerShareManifestsGetReply>(&bytes)
                .expect("strict decode")
                .extra
                .len(),
            1
        );

        let bytes = encode_canonical(&PeerShareChunksPullReply {
            extra,
            ..Default::default()
        })
        .expect("encode");
        assert_eq!(
            decode_strict::<PeerShareChunksPullReply>(&bytes)
                .expect("strict decode")
                .extra
                .len(),
            1
        );
    }
}
