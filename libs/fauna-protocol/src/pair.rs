//! `fauna.pair.*` WS-RPC payload types.
//!
//! The full per-user-pairing surface is bearer WS-RPC: `fauna.pair.list`,
//! `.add`, and `.revoke` (per-user-pairing design, 2026-05-25), plus the two
//! forward-queue actions `.forward_retry` and `.forward_discard` (2026-09-23 —
//! the list reply also carries the caller's [`ForwardQueue`]). The retired
//! `POST /api/v1/pair`(+`/revoke`) peer-authenticated handshake endpoints and
//! `fauna.admin.pairings.{list,approve}` kinds are gone — there is no admin
//! approval and no peer handshake left in this surface. The nest↔nest *sync*
//! surface these pairings feed also moved off HTTP: Spec Y2 slice 5 retired
//! the interim `/api/v1/nest-sync/*` routes in favor of the federation WS
//! channel (`docs/goal/architecture/nest/private-mode.md` § Pairing Flow).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ByteBuf, Value};

/// Canonical pairing capabilities — what a linked nest may sync on the user's
/// behalf. Each is an operation the actor already performs from a normal
/// client (`docs/goal/architecture/nest/private-mode.md` § Pairing Flow;
/// design tracked internally).
///
/// These supersede the pre-design `"sync"` / `"federation"` strings that the
/// retired admin approve-paths defaulted to — do **not** carry those forward
/// into `fauna.pair.add` (design spec § 3, "Known drift to resolve").
pub mod capability {
    /// Pull buffered MLS Welcome + channel messages from the relay nest.
    pub const MLS_PULL: &str = "mls_pull";
    /// Sync the actor's namespace entries.
    pub const NAMESPACE_SYNC: &str = "namespace_sync";
    /// Forward the actor's posts through the relay nest.
    pub const POST_FORWARD: &str = "post_forward";
    /// Pull the actor's inbound + Sent mail records (`__mail`) from the public
    /// relay nest to the paired private nest. The public→private mail relay
    /// (`docs/goal/architecture/nest/deployment-home-with-public-relay.md`
    /// § Inbound mail). Channel kinds `fauna.federation.sync.mail_{pull,ack}`.
    pub const MAIL_PULL: &str = "mail_pull";
    /// Bridge the actor's Nostr relay between a keyless public *serving* box and
    /// the paired head holding the deposited `nsec`. Gates **both** federation
    /// legs (as `mail_pull` gates `mail_ack`): the head pushes its `origin='ingest'`
    /// rows public-ward and pulls externally-deposited rows head-ward
    /// (`docs/goal/ui/nostr.md` § The bridging gate → Phase 2). Channel kinds
    /// `fauna.federation.sync.nostr_{push,pull}`.
    pub const NOSTR_PUSH: &str = "nostr_push";
    /// Hold a **secondary replica** of the actor's account plane: the user's
    /// recorded choice that this linked nest keeps the account's class-2 rows
    /// and escrow wraps, completed by the actor's own seed-holding devices
    /// over a second owner-authenticated connection
    /// (`docs/goal/architecture/account-sync-plane.md` § The bind leg,
    /// ruling 4). The nest enforces nothing for it — every door the
    /// completion uses admits the authenticated account — and no nest talks
    /// to another under it; the client reads it.
    pub const ACCOUNT_REPLICA: &str = "account_replica";
}

/// The default capability set granted when a user links one of their own
/// nests = the user's **full self-sync** (`mls_pull` + `namespace_sync` +
/// `post_forward` + `mail_pull` + `nostr_push` + `account_replica`).
/// Per-capability scoping is a
/// documented future refinement, not v1 (design spec § 3). `mail_pull` is
/// included so a linked private nest relays the user's mail out-of-the-box (the
/// home-with-public-relay deployment); on a nest with no mail bridge it is
/// simply never exercised. `nostr_push` is included on the same rationale so a
/// keyless public serving box's paired head bridges the Nostr relay
/// out-of-the-box (`docs/goal/ui/nostr.md` § The bridging gate → Phase 2, R3 (account-data-plane.md § The ratified decisions));
/// on a nest with no Nostr account it is simply never exercised.
/// `account_replica` is included so a linked nest is a complete replica of the
/// account plane out-of-the-box — a device the user never binds to it still
/// finds everything there (`account-sync-plane.md` § The bind leg, ruling 4).
pub fn default_self_sync() -> Vec<String> {
    vec![
        capability::MLS_PULL.to_string(),
        capability::NAMESPACE_SYNC.to_string(),
        capability::POST_FORWARD.to_string(),
        capability::MAIL_PULL.to_string(),
        capability::NOSTR_PUSH.to_string(),
        capability::ACCOUNT_REPLICA.to_string(),
    ]
}

/// `fauna.pair.list` — owner-implicit list of the bearer actor's active
/// pairings. Mirrors the shape of the retired HTTP twin
/// `GET /api/v1/pairings/{actor}` (deleted in the WS-RPC-everywhere
/// rip-out): the twin's `bearer.0.0 == actor_id` check becomes the
/// connection actor scope, so no `actor_id` rides on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One active pairing row (`NestPairing`), as surfaced by the twin's JSON.
/// `private_nest_id` rides as raw bytes (the twin hex-encoded it). `label` and
/// `nest_url` are the user-supplied display name and the linked nest's URL —
/// both surfaced so the `linked-nests` client list can render them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairingRow {
    pub private_nest_id: ByteBuf,
    pub capabilities: Vec<String>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub nest_url: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The caller's own view of this nest's post-forward queue, carried on
/// `fauna.pair.list` because that reply is what the Nests page renders and a
/// stuck forward is something its user must be able to see from the app,
/// never only in a log (`docs/goal/architecture/nest/private-mode.md` § Post
/// Forwarding → the queue is the user's to see). Counts are the caller's
/// entries only (`outbox.author_id`); a public nest reports zeros.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ForwardQueue {
    /// Every entry the caller has queued, backed-off ones included.
    pub queued: u64,
    /// Those past the retry ceiling — refusals that have outlasted the whole
    /// backoff (about 8.5 hours) and are still retrying.
    pub stuck: u64,
    /// The most recent failure the worker recorded on any of them; absent
    /// until a send has failed.
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairListReply {
    pub pairings: Vec<PairingRow>,
    /// The caller's post-forward queue on this nest — always reported, zeros
    /// when nothing is queued.
    pub forward_queue: ForwardQueue,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.pair.forward_retry` — re-arm every forward the caller has queued
/// for an immediate retry (bearer `User`, owner-implicit). The same nudge
/// `fauna.pair.add` gives, for the user who granted `post_forward` on the
/// relay's own side rather than by linking from here: without it the worker
/// waits out a backoff of up to 8.5 hours before it can find the grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairForwardRetryRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairForwardRetryReply {
    /// How many of the caller's entries were made due now.
    pub rearmed: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.pair.forward_discard` — drop every forward the caller has queued,
/// posts and deletions alike (bearer `User`, owner-implicit). The posts
/// themselves stay on this nest; only their relay leaves. A queued deletion
/// the relay never received leaves the relay's copy in place, which the user
/// can delete from the relay itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairForwardDiscardRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairForwardDiscardReply {
    /// How many of the caller's entries left the queue.
    pub discarded: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.pair.add` — the user authorizes a nest to sync their account
/// (bearer `User`, owner-scoped). Owner-implicit like `PairListRequest`: no
/// `actor_id` on the wire — the connection actor scopes the write
/// (`store_pairing(&connection_actor, …)`), so a user can only pair their
/// own account. Gated by the admin pairing-policy knob; rejected with a
/// clear error when pairing is disabled (design spec § 2/§ 4).
///
/// `capabilities` should be `default_self_sync()` for the v1 "link this nest"
/// action; `label` is a user-supplied display name persisted on the
/// `nest_pairings` row and echoed back by `fauna.pair.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairAddRequest {
    /// Ed25519 public key of the nest being linked.
    pub private_nest_id: ByteBuf,
    pub capabilities: Vec<String>,
    pub expires_at: Option<i64>,
    pub label: Option<String>,
    pub nest_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairAddReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.pair.revoke` — the user unlinks a nest (bearer `User`,
/// owner-scoped). Owner-implicit; calls `revoke_pairing(&connection_actor,
/// &private_nest_id)`. Revocation takes effect immediately (design spec § 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairRevokeRequest {
    /// Ed25519 public key of the nest being unlinked.
    pub private_nest_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairRevokeReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn list_request_round_trips() {
        let req = PairListRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<PairListRequest>(&bytes).unwrap(), req);
    }

    #[test]
    fn list_reply_round_trips() {
        let reply = PairListReply {
            pairings: vec![
                PairingRow {
                    private_nest_id: ByteBuf::from(vec![0x11u8; 32]),
                    capabilities: default_self_sync(),
                    expires_at: Some(1_700_000_000),
                    created_at: 1_699_000_000,
                    label: Some("home NAS".into()),
                    nest_url: Some("https://nest1.example".into()),
                    extra: Default::default(),
                },
                PairingRow {
                    private_nest_id: ByteBuf::from(vec![0x22u8; 32]),
                    capabilities: vec![],
                    expires_at: None,
                    created_at: 1_699_500_000,
                    label: None,
                    nest_url: None,
                    extra: Default::default(),
                },
            ],
            forward_queue: ForwardQueue {
                queued: 3,
                stuck: 1,
                last_error: Some("nest not paired for this actor".into()),
                extra: Default::default(),
            },
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<PairListReply>(&bytes).unwrap(), reply);
    }

    /// A list reply without `forward_queue` is refused: the queue is always
    /// reported, so its absence is a malformed reply, never "nothing queued".
    #[test]
    fn list_reply_without_forward_queue_is_refused() {
        let mut map = BTreeMap::new();
        map.insert("pairings".to_string(), Value::List(vec![]));
        let bytes = encode_canonical(&Value::Map(map)).unwrap();
        assert!(decode::<PairListReply>(&bytes).is_err());
    }

    #[test]
    fn forward_retry_and_discard_round_trip() {
        let req = PairForwardRetryRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<PairForwardRetryRequest>(&bytes).unwrap(), req);
        let reply = PairForwardRetryReply {
            rearmed: 2,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<PairForwardRetryReply>(&bytes).unwrap(), reply);

        let req = PairForwardDiscardRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<PairForwardDiscardRequest>(&bytes).unwrap(), req);
        let reply = PairForwardDiscardReply {
            discarded: 2,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<PairForwardDiscardReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn add_request_round_trips() {
        let req = PairAddRequest {
            private_nest_id: ByteBuf::from(vec![0x33u8; 32]),
            capabilities: default_self_sync(),
            expires_at: Some(1_800_000_000),
            label: Some("home NAS".into()),
            nest_url: Some("https://nest.example".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<PairAddRequest>(&bytes).unwrap(), req);
    }

    #[test]
    fn add_request_round_trips_minimal() {
        // No expiry / label / nest_url — the bare "link with full self-sync".
        let req = PairAddRequest {
            private_nest_id: ByteBuf::from(vec![0x44u8; 32]),
            capabilities: default_self_sync(),
            expires_at: None,
            label: None,
            nest_url: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<PairAddRequest>(&bytes).unwrap(), req);
    }

    #[test]
    fn add_reply_round_trips() {
        let reply = PairAddReply {
            ok: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<PairAddReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn revoke_request_round_trips() {
        let req = PairRevokeRequest {
            private_nest_id: ByteBuf::from(vec![0x55u8; 32]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<PairRevokeRequest>(&bytes).unwrap(), req);
    }

    #[test]
    fn revoke_reply_round_trips() {
        let reply = PairRevokeReply {
            ok: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<PairRevokeReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn default_self_sync_is_the_canonical_set() {
        // The canonical capability set — NOT the retired "sync"/"federation"
        // (design spec § 3, "Known drift to resolve").
        assert_eq!(
            default_self_sync(),
            vec![
                capability::MLS_PULL.to_string(),
                capability::NAMESPACE_SYNC.to_string(),
                capability::POST_FORWARD.to_string(),
                capability::MAIL_PULL.to_string(),
                capability::NOSTR_PUSH.to_string(),
                capability::ACCOUNT_REPLICA.to_string(),
            ]
        );
        assert!(
            !default_self_sync()
                .iter()
                .any(|c| c == "sync" || c == "federation")
        );
    }
}
