//! `fauna.peer.sync.*` — the same-account peer leg's Y.1 kinds over the
//! peer channel (W2.6 (account-data-plane.md § Workstreams); `docs/goal/architecture/account-data-plane.md`
//! § The peer leg).
//!
//! The peer leg is *the same sync contract over a different transport*: the
//! store↔store walk reuses [`crate::sync`]'s `fauna.sync.changes.list`
//! request/reply **unchanged** (a replica serves its relay plane where the
//! nest serves `sync_changes`), so this module adds only what the nest leg
//! has no analog for:
//!
//! - the **admission exchange** (`fauna.peer.sync.admit`) — each side
//!   presents its witness and independently admits the remote
//!   (`account-data-plane.md` § The peer leg → *The admission seam*);
//! - the **want-list block pull** (`fauna.peer.sync.blocks.pull`) — class-1
//!   record blocks / class-3 blobs fetched by CID from an admitted peer's
//!   block plane (the nest's analogs are HTTP byte planes, which the peer
//!   channel deliberately does not carry — bounded frames instead);
//! - the **ranged chunk pull** (`fauna.peer.sync.chunks.pull`) — a file-sync
//!   folder's stored chunk bodies from a sibling device that holds the file,
//!   sliced to the frame (the nest's analog is `GET /api/v1/chunks/{key}`).
//!
//! **Kind-family separability (wormability rule 5).** The same-account leg's
//! kinds live under `fauna.peer.sync.` — a family distinct from the base
//! `fauna.peer.*` connectivity kinds and from any future cross-user share
//! family — so the store-safe artifact witness can prove "same-account sync
//! PRESENT, `p2p-share` ABSENT" on kind strings alone
//! (`account-data-plane.md` § Wormability walk rule 5).
//!
//! **Pre-auth discipline (rule 2).** `fauna.peer.sync.admit` is pre-auth
//! surface: strict canonical dag-cbor via [`crate::decode_strict`], no
//! hand-rolled parsing, and its kinds are in PQ-2's fuzz scope — built
//! 2026-08-12: `fauna_peer_channel::hardening::check_kind_payload_decode`
//! decodes every struct below, corpus-replayed by the
//! `peer-channel-hardening-check` merge gate and fuzzed by
//! `libs/fauna-peer-channel/fuzz`. Like every peer kind, payloads carry the
//! rule-4 `extra` catch-all and a peer that predates a kind answers
//! `fauna.protocol.unknown_kind` (the `PeerChannel::serve` default) — the
//! additive-everywhere shape on the peer wire.
//!
//! These kinds ride the **peer channel only** — they are deliberately NOT
//! [`crate::KindRegistry`] members (that registry is the nest↔client
//! dispatcher's; the peer dispatcher is the allowlist in
//! `fauna-peer-sync`, wormability rule 3).

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;
use fauna_core::encoding::EmbedAsBytes;

/// Peer-channel kind: the admission exchange (see module docs).
pub const KIND_PEER_SYNC_ADMIT: &str = "fauna.peer.sync.admit";
/// Peer-channel kind: want-list block pull from an admitted peer.
pub const KIND_PEER_SYNC_BLOCKS_PULL: &str = "fauna.peer.sync.blocks.pull";
/// Peer-channel kind: ranged want-list pull of a file-sync folder's stored
/// chunk bodies from an admitted sibling (see [`PeerSyncChunksPullRequest`]).
pub const KIND_PEER_SYNC_CHUNKS_PULL: &str = "fauna.peer.sync.chunks.pull";

/// The `DeviceAuthorization` witness kind — same-account admission
/// (`fauna_core::encoding::verify_device_admission_witness`). The seam's
/// M2-membership witness kind lives with the cross-account twin
/// (`crate::peer_share::WITNESS_M2_MEMBERSHIP`, behind the `p2p-share`
/// feature — its exchange is `fauna.peer.share.admit`, never this one).
pub const WITNESS_DEVICE_AUTHORIZATION: &str = "device-authorization";

/// The custody-grant witness kind — non-fleet custodian admission
/// (`fauna_core::custody_grant::verify_custody_witness`; shape ruled at
/// `account-data-plane.md` § Replica posture → *The custody grant +
/// ceremony*, W8). The witness's inner bytes decode as an owner-actor-signed
/// `CustodyGrant`, never a `GrantBlob`.
pub const WITNESS_CUSTODY_GRANT: &str = "custody-grant";

/// `RpcError` code: a sync-transfer request arrived on a connection with no
/// (or an expired) admission verdict, or for a scope outside the verdict's
/// set. The remedy is to (re-)present a witness via
/// [`KIND_PEER_SYNC_ADMIT`].
pub const ERR_NOT_ADMITTED: &str = "fauna.peer.sync.not_admitted";
/// `RpcError` code: the witness did not verify (bad signature, wrong device
/// key, foreign account, expired).
pub const ERR_WITNESS_REFUSED: &str = "fauna.peer.sync.witness_refused";
/// `RpcError` code: a per-peer / per-window quota refused the request
/// (wormability rule 8 — covers admission-refused attempts too).
pub const ERR_OVER_QUOTA: &str = "fauna.peer.sync.over_quota";
/// `RpcError` code: the request named a shape this build's peer serve does
/// not carry (a non-`state-entry` item class, a malformed want-list CID).
/// Loud by design — an empty page would read as "converged" to a walk.
pub const ERR_UNSUPPORTED: &str = "fauna.peer.sync.unsupported";

// ── fauna.peer.sync.admit ────────────────────────────────────────────────────

/// `fauna.peer.sync.admit` request — the requester presents its admission
/// witness. **Carriage is inline and self-contained** (the seam's rule): the
/// witness envelope travels here; a listener never depends on a registry
/// lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncAdmitRequest {
    /// Which witness kind [`Self::witness`] carries —
    /// [`WITNESS_DEVICE_AUTHORIZATION`] for the same-account leg. A verifier
    /// refuses a kind it does not implement (never guesses from shape).
    pub witness_kind: String,
    /// The witness itself — for `device-authorization`, the embed-as-bytes of
    /// the root-signed `DeviceAuthorization` covering the *requester's*
    /// channel-proven key.
    pub witness: EmbedAsBytes,
    /// The requester's current dial identity + candidates — the per-session
    /// endpoint re-exchange of the custody ceremony (T13 step 4: the
    /// fleet-only `device-endpoints` kind never reaches a non-fleet peer, so
    /// each custody session re-exchanges candidates here). Additive
    /// (2026-08-16, W8.4): an older decoder absorbs it through
    /// [`Self::extra`]'s flatten slot and `skip_serializing_if` keeps the
    /// absent shape byte-identical. Carried from W8.4; filled and consumed
    /// since the endpoint re-exchange landed (2026-08-16) — the consumers
    /// fold it into the `custodies-held` / `custodian-endpoints` rows.
    /// Advisory transport truth, never identity: the channel-proven key is
    /// the peer's identity regardless of what this names — bind it with
    /// `fauna_peer_sync::bind_carried_endpoints`, which REFUSES a value
    /// naming anyone else — and a carried `relay_url` stays subject to the
    /// own-nest relay-provenance rule at the consumer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<fauna_core::device_endpoints::DeviceEndpoints>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.sync.admit` reply — the responder's own witness, so one round
/// trip yields mutual (independently evaluated) admission. A responder that
/// refuses the requester's witness answers the Y.1 `ok = false` reply
/// ([`ERR_WITNESS_REFUSED`]) instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncAdmitReply {
    /// The responder's witness kind — the sides' kinds may differ (the seam's
    /// rule: "mutual" means both directions independently admitted, never
    /// witness-kind symmetry).
    pub witness_kind: String,
    /// The responder's witness, covering the *responder's* channel-proven key.
    pub witness: EmbedAsBytes,
    /// The responder's current dial identity + candidates — the reply half
    /// of the per-session endpoint re-exchange (see
    /// [`PeerSyncAdmitRequest::endpoints`]; same additive/advisory rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<fauna_core::device_endpoints::DeviceEndpoints>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.sync.blocks.pull ──────────────────────────────────────────────

/// One block a pull reply carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncBlock {
    /// The block's canonical 36-byte CID (`serialization.md` § CID shape).
    pub cid: ByteBuf,
    /// The block bytes. The receiver re-verifies them against [`Self::cid`]
    /// before storing (content-address verification on every block write —
    /// wormability rule 4: received content is inert; a poisoned block fails
    /// its hash check and is refused).
    pub bytes: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.sync.blocks.pull` request — the want-list: CIDs of blocks the
/// requester lacks (its record index knows of them; its block plane does not
/// hold them).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncBlocksPullRequest {
    /// Canonical 36-byte CIDs, most-wanted first.
    pub cids: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.sync.blocks.pull` reply. Three disjoint outcomes per requested
/// CID, so "doesn't have it" is never conflated with "didn't fit this frame":
/// in [`Self::blocks`] (served), in [`Self::missing`] (the peer's block plane
/// does not hold it — try another peer or the nest), or in [`Self::deferred`]
/// (held, but over this reply's frame budget — re-request in a smaller
/// want-list).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncBlocksPullReply {
    pub blocks: Vec<PeerSyncBlock>,
    pub missing: Vec<ByteBuf>,
    pub deferred: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.sync.chunks.pull ──────────────────────────────────────────────
//
// The file-sync plane's class-3 bytes between two devices of one account
// (`docs/goal/behavior/file-sync.md` § Content residency: seats fetch content
// seat↔seat over the peer leg, "the same want-list chunk pull"). The twin of
// `fauna.peer.share.chunks.pull` on the same-account family: same ranged
// shape, the folder named where the share leg names its set. Admitted by an
// own-account `DeviceAuthorization` verdict alone.

/// One want: a chunk by store key, and how much of it the requester holds.
/// Ranged for the reason the share twin is
/// ([`crate::peer_share::PeerShareChunkWant`]): a chunk body is up to 8 MiB
/// and a peer-channel frame caps at 1 MiB.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncChunkWant {
    /// The 32-byte store key (`fauna_core::chunk::ChunkManifest::store_keys`).
    pub store_key: ByteBuf,
    /// Byte offset into the stored body to resume from; `0` for a fresh want.
    /// Past the body's end is a protocol error ([`ERR_UNSUPPORTED`]), never an
    /// empty slice — an empty slice would read as complete to a puller.
    #[serde(default)]
    pub offset: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.sync.chunks.pull` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncChunksPullRequest {
    /// The folder the chunks belong to, as its `FolderRef` wire string — the
    /// spelling the relay ask carries (`fauna.sync.chunk.wanted`), which is
    /// how the serving device routes the want to that folder's engine.
    pub folder: String,
    /// The wants, most-wanted first.
    pub wants: Vec<PeerSyncChunkWant>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One served slice of a stored chunk body, verbatim as the nest would store
/// it — still sealed. The receiver checks the completed body against its store
/// key before using it (rule 4).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncChunk {
    pub store_key: ByteBuf,
    /// Where this slice starts in the body.
    #[serde(default)]
    pub offset: u64,
    pub bytes: ByteBuf,
    /// The whole body's length (`offset + bytes.len() == total_len` ends it).
    #[serde(default)]
    pub total_len: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.sync.chunks.pull` reply — served / missing / deferred, the
/// three disjoint outcomes of [`PeerSyncBlocksPullReply`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerSyncChunksPullReply {
    pub chunks: Vec<PeerSyncChunk>,
    /// This device holds no body for it — the nest path serves it.
    pub missing: Vec<ByteBuf>,
    /// Not looked at within this reply's budget — re-request.
    pub deferred: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict as decode, encode_canonical};

    fn witness_fixture() -> EmbedAsBytes {
        EmbedAsBytes {
            envelope: vec![0xEE; 100],
            bytes: vec![0xBB; 40],
            signer_auth: None,
        }
    }

    #[test]
    fn admit_request_and_reply_round_trip() {
        let req = PeerSyncAdmitRequest {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: witness_fixture(),
            endpoints: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: PeerSyncAdmitRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let reply = PeerSyncAdmitReply {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: witness_fixture(),
            endpoints: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: PeerSyncAdmitReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    /// The W8.4 per-session endpoint re-exchange slot is ADDITIVE in both
    /// skew directions: `None` keeps the pre-slot bytes exactly (an older
    /// build's decoder sees the shape it always saw), a carried value
    /// round-trips, and an older decoder — modeled by the pre-slot struct
    /// shape — absorbs the unknown key through its `extra` flatten slot and
    /// re-emits it, never dropping or refusing it.
    #[test]
    fn admit_endpoint_slot_is_additive_both_directions() {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        struct PreSlotAdmitRequest {
            witness_kind: String,
            witness: EmbedAsBytes,
            #[serde(flatten, default)]
            extra: BTreeMap<String, Value>,
        }

        // None ⇒ byte-identical to the pre-slot encoding.
        let old = PreSlotAdmitRequest {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: witness_fixture(),
            extra: BTreeMap::new(),
        };
        let new_none = PeerSyncAdmitRequest {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: witness_fixture(),
            endpoints: None,
            extra: BTreeMap::new(),
        };
        assert_eq!(
            encode_canonical(&old).unwrap(),
            encode_canonical(&new_none).unwrap(),
            "an absent slot must not move a single wire byte"
        );

        // A carried value round-trips on the new decoder…
        let carried = PeerSyncAdmitRequest {
            endpoints: Some(fauna_core::device_endpoints::DeviceEndpoints {
                node_id: [7u8; 32],
                lan_addrs: vec!["192.168.1.7:4433".into()],
                public_addrs: Vec::new(),
                relay_url: None,
            }),
            ..new_none
        };
        let bytes = encode_canonical(&carried).unwrap();
        let back: PeerSyncAdmitRequest = decode(&bytes).unwrap();
        assert_eq!(back, carried);

        // …and the OLD decoder absorbs it via `extra` and re-emits it
        // verbatim (the flatten slot's whole purpose).
        let as_old: PreSlotAdmitRequest = decode(&bytes).unwrap();
        assert!(
            as_old.extra.contains_key("endpoints"),
            "the pre-slot decoder must park the unknown key in extra"
        );
        assert_eq!(
            encode_canonical(&as_old).unwrap(),
            bytes,
            "a pre-slot re-encode must carry the slot through unchanged"
        );
    }

    #[test]
    fn blocks_pull_round_trips_with_all_three_outcomes() {
        let reply = PeerSyncBlocksPullReply {
            blocks: vec![PeerSyncBlock {
                cid: ByteBuf::from(vec![1u8; 36]),
                bytes: ByteBuf::from(b"block bytes".to_vec()),
                extra: BTreeMap::new(),
            }],
            missing: vec![ByteBuf::from(vec![2u8; 36])],
            deferred: vec![ByteBuf::from(vec![3u8; 36])],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: PeerSyncBlocksPullReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    /// Additive-everywhere on the peer wire: an admit request from a newer
    /// build carrying an unknown field decodes, the field surviving in
    /// `extra` (`transport.md` § forward-compat rule 4).
    #[test]
    fn admit_request_tolerates_an_unknown_field() {
        #[derive(Serialize)]
        struct Newer {
            witness_kind: String,
            witness: EmbedAsBytes,
            some_future_field: u32,
        }
        let bytes = encode_canonical(&Newer {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: witness_fixture(),
            some_future_field: 7,
        })
        .unwrap();
        let back: PeerSyncAdmitRequest = decode(&bytes).unwrap();
        assert_eq!(
            back.extra.get("some_future_field"),
            Some(&Value::Integer(7))
        );
    }
}
