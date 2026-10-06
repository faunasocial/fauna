//! The share leg's cached discovery row — one member of one shared file set,
//! as this fleet last learned to reach them (`fauna.state.share-endpoints`,
//! registered in `fauna_protocol::merge_policy`; the W8 (account-data-plane.md § Workstreams) share twin's slice F).
//!
//! Authority: `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer →
//! *Discovery* ("peer endpoints learned only through authenticated channels")
//! and `p2p-shared-set-build.md` § *Build contract* → *Discovery carriage is its own slice* ("the set's
//! own conversation channel carries members' endpoint advertisements, cached
//! for offline dial").
//!
//! **This is the T5 symmetry, applied across users.** The same-account leg
//! caches siblings in [`crate::device_endpoints`] and granted custodians in
//! [`crate::custodian_endpoints`]; this kind is the third member of that
//! family — *this* fleet's cache of *another user's* location data, learned
//! over the set's own MLS-authenticated channel rather than written by the
//! subject. The entry's logical key is [`share_entry_key`].
//!
//! **`endpoints.node_id` is the member's ACTOR key here, not a device
//! principal.** The share leg dials the contact plane, whose NodeId *is* the
//! Ed25519 actor key (PT-1b), where the same-account leg dials device
//! principals. The 32 bytes flow through the shared
//! `fauna_peer_sync::discovery::dial_target_from` composition untouched, so
//! the reuse is exact — only the meaning differs, and both ends say so.
//!
//! Rung/epoch and evolution posture: exactly its two siblings' (fleet-only,
//! tip-sealed — location data; whole-record LWW with tolerant decode in both
//! skew directions).

use serde::{Deserialize, Serialize};

use crate::device_endpoints::DeviceEndpoints;

/// The logical key of one `fauna.state.share-endpoints` entry:
/// `<channel-id-hex>:<member-actor-hex>`, both exactly 64 lowercase hex
/// chars. One row per (set, member) — a member is reachable at one actor
/// NodeId per set, which is precisely the granularity the contract's
/// one-live-connection-per-remote-actor bullet leaves.
pub fn share_entry_key(channel_id: &[u8; 32], member_actor: &[u8; 32]) -> String {
    format!(
        "{}:{}",
        crate::hex32::encode(channel_id),
        crate::hex32::encode(member_actor)
    )
}

/// One member of one shared set — the value of one
/// `fauna.state.share-endpoints` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareEndpoints {
    /// The set's derived 32-byte MLS `ChannelId` — redundant with the
    /// entry's logical key on purpose (cross-checkable, like every registry
    /// row in this family).
    #[serde(with = "serde_bytes", default)]
    pub channel_id: Vec<u8>,
    /// The advertising member's 32-byte actor id — likewise redundant with
    /// the key, and likewise cross-checked before a consumer dials. This is
    /// the identity the ingest side bound to the MLS-authenticated sender:
    /// a row naming anyone but the channel-proven advertiser is never
    /// written (`fauna_peer_share::endpoints`).
    #[serde(with = "serde_bytes", default)]
    pub member_actor: Vec<u8>,
    /// The member's dial identity + candidates as last advertised. Its
    /// `node_id` is the member's ACTOR key (see the module header); its
    /// `relay_url` is subject to the same relay-provenance rule at the
    /// consumer as its siblings (availability, never a blind dial URL).
    #[serde(default)]
    pub endpoints: DeviceEndpoints,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SET: [u8; 32] = [0x5E; 32];
    const MEMBER: [u8; 32] = [0x3B; 32];

    fn sample() -> ShareEndpoints {
        ShareEndpoints {
            channel_id: SET.to_vec(),
            member_actor: MEMBER.to_vec(),
            endpoints: DeviceEndpoints {
                node_id: MEMBER,
                lan_addrs: vec!["192.168.1.9:4433".into()],
                public_addrs: vec!["198.51.100.7:4433".into()],
                relay_url: Some("https://relay.example/".into()),
            },
        }
    }

    /// The key is the two ids in one canonical spelling — the string a
    /// consumer scans for, and the one the cross-check compares against.
    #[test]
    fn the_entry_key_is_both_ids_lowercase_hex() {
        let key = share_entry_key(&SET, &MEMBER);
        assert_eq!(key.len(), 64 + 1 + 64);
        assert_eq!(&key[..64], &crate::hex32::encode(&SET));
        assert_eq!(&key[65..], &crate::hex32::encode(&MEMBER));
        assert_eq!(&key[64..65], ":");
    }

    #[test]
    fn canonical_round_trip() {
        let v = sample();
        let bytes = crate::encoding::canonical_encode(&v).unwrap();
        let back: ShareEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, v);
    }

    /// A newer build's extra field decodes tolerantly — the same posture the
    /// two sibling registry rows carry (skew in both directions).
    #[test]
    fn a_newer_builds_value_decodes_tolerantly() {
        #[derive(Serialize)]
        struct V2 {
            #[serde(with = "serde_bytes")]
            channel_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            member_actor: Vec<u8>,
            endpoints: DeviceEndpoints,
            advertised_at: u64, // the field this build predates
        }
        let bytes = crate::encoding::canonical_encode(&V2 {
            channel_id: SET.to_vec(),
            member_actor: MEMBER.to_vec(),
            endpoints: DeviceEndpoints::default(),
            advertised_at: 1_700_000_000,
        })
        .unwrap();
        let got: ShareEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.channel_id, SET.to_vec());
        assert_eq!(got.member_actor, MEMBER.to_vec());
    }

    /// An older build's value (no candidates yet) decodes with defaults
    /// rather than failing the row — a replica must carry forward what it
    /// cannot fully read.
    #[test]
    fn an_older_builds_value_decodes_with_defaults() {
        #[derive(Serialize)]
        struct V0 {
            #[serde(with = "serde_bytes")]
            channel_id: Vec<u8>,
        }
        let bytes = crate::encoding::canonical_encode(&V0 {
            channel_id: SET.to_vec(),
        })
        .unwrap();
        let got: ShareEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.channel_id, SET.to_vec());
        assert!(got.member_actor.is_empty());
        assert_eq!(got.endpoints, DeviceEndpoints::default());
    }
}
