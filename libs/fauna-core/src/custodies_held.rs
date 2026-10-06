//! The custodian-side custody registry row's value shape — one custody this
//! device's account HOLDS for another account (`fauna.state.custodies-held`,
//! registered in `fauna_protocol::merge_policy`; W8.3 (account-data-plane.md § Workstreams)).
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Replica
//! posture → *The custody grant + ceremony* (T13): "The custodian stores the
//! witness (plus the owner's pinned NodeIds and dial candidates) in a
//! fleet-only class-2 kind of its **own** account plane — 'custodies held' —
//! and presents it inline in the admission exchange." The entry's logical
//! key is the custody grant id (hex — [`crate::custody_grant::custody_entry_key`]),
//! so one row per custody and the single writing engine never collides.
//!
//! # Rung: fleet-only, epoch: generation-tip
//!
//! The value carries the OWNER fleet's NodeIds and dial candidates —
//! location data, the same reasoning that tip-sealed `device-endpoints`
//! (generation keying severs a removed device from reading it).
//!
//! # Evolution posture
//!
//! Whole-record LWW like `crate::device_endpoints` — a reader adopts or
//! keeps verbatim bytes, never re-encodes a merge — so tolerant decoding is
//! safe in both skew directions: every field `#[serde(default)]`, unknown
//! fields ignored. Additive evolution is in-place.

use serde::{Deserialize, Serialize};

use crate::device_endpoints::DeviceEndpoints;

/// One held custody — the value of one `fauna.state.custodies-held` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyHeld {
    /// The custody grant id (16 bytes) — redundant with the entry's logical
    /// key on purpose (consumers read the decoded value; the walk can
    /// cross-check the two).
    #[serde(with = "serde_bytes", default)]
    pub grant_id: Vec<u8>,
    /// The custodied account's actor id.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub owner: [u8; 32],
    /// The owner-signed admission witness, as the canonical bytes of its
    /// `EmbedAsBytes` wire shape — presented inline at every admission
    /// exchange (self-contained carriage; no registry lookup).
    #[serde(with = "serde_bytes", default)]
    pub witness: Vec<u8>,
    /// The owner fleet's pinned dial identities + candidates, as delivered
    /// at the ceremony and refreshed per custody session — each element the
    /// same shape a fleet sibling publishes for itself. A peer-supplied
    /// `relay_url` inside is subject to the own-nest relay-provenance rule
    /// at the consumer (availability boolean only, never a dial URL).
    #[serde(default)]
    pub owner_devices: Vec<DeviceEndpoints>,
    /// The owner's nest base URL — the always-on anchor the custodian's
    /// nest-pull leg (W8.6) dials; delivered in the ceremony offer.
    #[serde(default)]
    pub owner_nest_url: Option<String>,
    /// The host-side accepted byte budget (T15's `retained_bytes_cap`) —
    /// carried from day one so the policy build changes behavior,
    /// never shape.
    #[serde(default)]
    pub retained_bytes_cap: u64,
    /// The host stopped holding (T16's stop control — "the host may always
    /// stop"). The custody leg skips a stopped row for both serve and pull,
    /// so the owner sees receipts go stale → degraded redundancy, exactly
    /// the honest signal T15 promises. The row itself stays (whole-record
    /// LWW; a resumed hold is a fresh put with `stopped: false`); payload
    /// reclamation of the local pull store is a deliberate follow-up, not
    /// implied by the flag.
    #[serde(default)]
    pub stopped: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CustodyHeld {
        CustodyHeld {
            grant_id: vec![0x1D; 16],
            owner: [0xAB; 32],
            witness: vec![0xEE; 140],
            owner_devices: vec![DeviceEndpoints {
                node_id: [7u8; 32],
                lan_addrs: vec!["192.168.1.7:4433".into()],
                public_addrs: vec!["203.0.113.9:4433".into()],
                relay_url: None,
            }],
            owner_nest_url: Some("https://nest.example/".into()),
            retained_bytes_cap: 8 * 1024 * 1024 * 1024,
            stopped: false,
        }
    }

    #[test]
    fn canonical_round_trip() {
        let v = sample();
        let bytes = crate::encoding::canonical_encode(&v).unwrap();
        let back: CustodyHeld = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, v);
    }

    /// The LWW evolution posture (see `device_endpoints`): a newer build's
    /// value with an unknown field still decodes.
    #[test]
    fn a_newer_builds_value_decodes_tolerantly() {
        #[derive(Serialize)]
        struct V2 {
            #[serde(with = "serde_bytes")]
            grant_id: Vec<u8>,
            #[serde(with = "serde_bytes")]
            owner: [u8; 32],
            #[serde(with = "serde_bytes")]
            witness: Vec<u8>,
            owner_devices: Vec<DeviceEndpoints>,
            owner_nest_url: Option<String>,
            retained_bytes_cap: u64,
            eviction_hint: String, // the field this build predates
        }
        let bytes = crate::encoding::canonical_encode(&V2 {
            grant_id: vec![0x1D; 16],
            owner: [0xAB; 32],
            witness: vec![0xEE; 8],
            owner_devices: Vec::new(),
            owner_nest_url: None,
            retained_bytes_cap: 42,
            eviction_hint: "oldest-first".into(),
        })
        .unwrap();
        let got: CustodyHeld = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.owner, [0xAB; 32]);
        assert_eq!(got.retained_bytes_cap, 42);
    }

    /// And an older build's value — fields missing — decodes with defaults.
    #[test]
    fn an_older_builds_value_decodes_with_defaults() {
        #[derive(Serialize)]
        struct V0 {
            #[serde(with = "serde_bytes")]
            owner: [u8; 32],
        }
        let bytes = crate::encoding::canonical_encode(&V0 { owner: [0xAB; 32] }).unwrap();
        let got: CustodyHeld = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.owner, [0xAB; 32]);
        assert!(got.witness.is_empty());
        assert!(got.owner_devices.is_empty());
        assert_eq!(got.retained_bytes_cap, 0);
    }
}
