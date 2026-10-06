//! The owner-side custody registry row's value shape — one custodian this
//! account has GRANTED custody to (`fauna.state.custodian-endpoints`,
//! registered in `fauna_protocol::merge_policy`; W8.3 (account-data-plane.md § Workstreams)).
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Replica
//! posture → *The custody grant + ceremony* (T13), ceremony step 3: "The
//! owner's fleet records the custodian's NodeId + dial candidates as a
//! fleet-only class-2 entry, so every fleet replica learns whom to serve
//! and how to dial it." The entry's logical key is the custody grant id
//! (hex — [`crate::custody_grant::custody_entry_key`]).
//!
//! Rung/epoch and evolution posture: exactly `crate::custodies_held`'s
//! (fleet-only, tip-sealed — the custodian's location data; whole-record
//! LWW with tolerant decode both skew directions).

use serde::{Deserialize, Serialize};

use crate::device_endpoints::DeviceEndpoints;

/// One granted custody's custodian — the value of one
/// `fauna.state.custodian-endpoints` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianEndpoints {
    /// The custody grant id (16 bytes) — redundant with the entry's logical
    /// key on purpose (cross-checkable, like every registry row here).
    #[serde(with = "serde_bytes", default)]
    pub grant_id: Vec<u8>,
    /// The custodian's dial identity + candidates — the accept-bound device
    /// principal key in `endpoints.node_id` (what the witness names), plus
    /// its candidates as re-exchanged per custody session. Its `relay_url`
    /// is subject to the own-nest relay-provenance rule at the consumer
    /// (availability boolean only, never a dial URL).
    #[serde(default)]
    pub endpoints: DeviceEndpoints,
    /// The custodian's latest **verified** custody receipt, as the canonical
    /// bytes of its `EmbedAsBytes` wire shape — kept verbatim, never
    /// re-encoded, so the owner's fleet can re-check the signature at any time
    /// (a receipt is a claim, not a fact:
    /// [`crate::custody_receipt`]).
    ///
    /// `None` = **no receipt yet**, which the T16 rows must render as its own
    /// state, distinct from a stale one and from an empty custody
    /// (`ui/nests.md` § Trust facet — custody rows: "fresh / stale /
    /// no-receipt-yet are three different states with different words", the
    /// `GenerationsStatus::Unreachable` precedent). Storing the envelope
    /// rather than a decoded summary is what keeps that honesty checkable
    /// downstream instead of trusted.
    #[serde(default, with = "serde_bytes")]
    pub latest_receipt: Option<Vec<u8>>,
    /// `Some` = this custodian is a NEST (the nest-custodian identity fact):
    /// the accept bound the host's pinned nest actor identity in
    /// `endpoints.node_id` and this URL is the restore dial anchor — the
    /// fleet reads the render split (Nests-page vs Devices-page family) and
    /// the anchor from the row without re-opening the accept. Additive;
    /// absent = a device custodian, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian_nest_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CustodianEndpoints {
        CustodianEndpoints {
            grant_id: vec![0x1D; 16],
            endpoints: DeviceEndpoints {
                node_id: [0xC5; 32],
                lan_addrs: vec!["192.168.1.9:4433".into()],
                public_addrs: vec!["198.51.100.7:4433".into()],
                relay_url: Some("https://relay.example/".into()),
            },
            latest_receipt: Some(vec![0xAE; 96]),
            ..Default::default()
        }
    }

    /// A custody with no receipt yet decodes to `None`, not to an empty
    /// summary — the "no-receipt-yet" state the T16 rows must not collapse
    /// into "stale" or "nothing held".
    #[test]
    fn no_receipt_yet_is_its_own_state() {
        let v = CustodianEndpoints {
            grant_id: vec![0x1D; 16],
            ..Default::default()
        };
        let bytes = crate::encoding::canonical_encode(&v).unwrap();
        let back: CustodianEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back.latest_receipt, None);
    }

    #[test]
    fn canonical_round_trip() {
        let v = sample();
        let bytes = crate::encoding::canonical_encode(&v).unwrap();
        let back: CustodianEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn a_newer_builds_value_decodes_tolerantly() {
        #[derive(Serialize)]
        struct V2 {
            #[serde(with = "serde_bytes")]
            grant_id: Vec<u8>,
            endpoints: DeviceEndpoints,
            check_in_hint: u64, // the field this build predates
        }
        let bytes = crate::encoding::canonical_encode(&V2 {
            grant_id: vec![0x1D; 16],
            endpoints: DeviceEndpoints::default(),
            check_in_hint: 3600,
        })
        .unwrap();
        let got: CustodianEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.grant_id, vec![0x1D; 16]);
    }

    #[test]
    fn an_older_builds_value_decodes_with_defaults() {
        #[derive(Serialize)]
        struct V0 {
            #[serde(with = "serde_bytes")]
            grant_id: Vec<u8>,
        }
        let bytes = crate::encoding::canonical_encode(&V0 {
            grant_id: vec![0x1D; 16],
        })
        .unwrap();
        let got: CustodianEndpoints = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(got.grant_id, vec![0x1D; 16]);
        assert_eq!(got.endpoints, DeviceEndpoints::default());
        assert_eq!(
            got.latest_receipt, None,
            "a row written before receipts existed reads as no-receipt-yet"
        );
    }
}
