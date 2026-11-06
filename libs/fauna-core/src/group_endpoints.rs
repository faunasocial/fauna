//! **A group member's dial targets** — the `fauna.group.endpoints` value and
//! the writer check every reader of it runs (`group-set.md` § Build design —
//! the group set, decision 8: "a member's dial targets for a scope are the
//! `fauna.group.endpoints` cells of its other members").
//!
//! One cell per writer device, its logical key the device id hex, sealed
//! under the machinery root so a member can dial before it holds any
//! generation key. The entry envelope's own writer signature names
//! whichever replica last re-published the cell, so authorship rides inside
//! the value, in the shape of [`crate::group_name::GroupNameRecord`]: a
//! signed core, the writer device's `DeviceAuthorization` carriage, and a
//! signature over both. A reader honours a cell only when the carriage
//! chains the device to the scope's authority (revocation-aware) or to a
//! live roster member, the cell sits under that device's key, the entry's
//! `node_id` is that member's own actor, and the signature verifies — so a
//! member can say where IT is and nothing else: not where a co-member is,
//! and not a cell under a co-member's key. The check
//! runs at the walk, before the merge, and again in the pump's read.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::device_endpoints::DeviceEndpointsEntry;
use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::Error;
use crate::group_scope::{GroupAuthority, RosterView, verify_device_cert_carriage};
use crate::identity::ActorId;

/// Domain-separation tag for an endpoints cell's writer signature.
/// **Frozen**: every cell a member ever wrote verifies through it.
pub const GROUP_ENDPOINTS_SIG_CONTEXT: &[u8] = b"fauna.group.endpoints.sig.v1 2026-11-02\0";

/// Everything an endpoints cell asserts — exactly what its writer signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEndpointsCore {
    /// The group scope the cell belongs to. Signed, so a cell is never
    /// replayed into another scope the same member sits in.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// Where the writer's device can be dialled. Its `node_id` must be the
    /// writer's own actor — the cross-user seat is actor-keyed — or the cell
    /// is refused.
    pub entry: DeviceEndpointsEntry,
    /// The writer's clock, unix ms — the `LwwStamp` the entry merges under
    /// carries exactly this instant and the writer device, so a re-publisher
    /// cannot re-rank a signed cell.
    pub stamped_at_ms: i64,
}

/// One `fauna.group.endpoints` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEndpointsRecord {
    /// The signed assertion.
    pub core: GroupEndpointsCore,
    /// The writer device's `DeviceAuthorization`, in the `EmbedAsBytes`
    /// carriage every witness surface uses.
    #[serde(with = "serde_bytes")]
    pub authorization: Vec<u8>,
    /// Ed25519 by the authorized device over [`group_endpoints_signing_bytes`].
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// The exact bytes a cell's writer signs: the frozen tag, the BLAKE3 of the
/// canonical core, and the BLAKE3 of the carried authorization (signed, so a
/// copy with the cert swapped no longer verifies — the file row's shape).
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn group_endpoints_signing_bytes(
    core: &GroupEndpointsCore,
    authorization: &[u8],
) -> Result<Vec<u8>, Error> {
    let core_bytes = canonical_encode(core)?;
    let mut out = Vec::with_capacity(GROUP_ENDPOINTS_SIG_CONTEXT.len() + 64);
    out.extend_from_slice(GROUP_ENDPOINTS_SIG_CONTEXT);
    out.extend_from_slice(blake3::hash(&core_bytes).as_bytes());
    out.extend_from_slice(blake3::hash(authorization).as_bytes());
    Ok(out)
}

/// The logical key of `device`'s cell — the device id hex.
#[must_use]
pub fn group_endpoints_cell_key(device: &[u8; 32]) -> String {
    crate::hex32::encode(device)
}

impl GroupEndpointsRecord {
    /// Sign `core` as `device`, carrying `authorization` (its cert).
    ///
    /// # Errors
    /// Only on a core that fails canonical encoding.
    pub fn sign(
        core: GroupEndpointsCore,
        device: &SigningKey,
        authorization: Vec<u8>,
    ) -> Result<GroupEndpointsRecord, Error> {
        let signature = device
            .sign(&group_endpoints_signing_bytes(&core, &authorization)?)
            .to_bytes()
            .to_vec();
        Ok(GroupEndpointsRecord {
            core,
            authorization,
            signature,
        })
    }
}

/// A cell a reader honours, with the member and device its checks
/// established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HonouredGroupEndpoints {
    /// The verified record.
    pub record: GroupEndpointsRecord,
    /// The member whose device wrote it — the entry's `node_id`.
    pub member: ActorId,
    /// The device that signed it — the cell's key and the `LwwStamp` writer
    /// the entry must merge under.
    pub device: [u8; 32],
}

impl HonouredGroupEndpoints {
    /// The dial target.
    #[must_use]
    pub fn entry(&self) -> &DeviceEndpointsEntry {
        &self.record.core.entry
    }
}

/// **The cell's writer check**: decode `value` at cell `key` of scope
/// `scope_id` and honour it iff its carriage chains the writer device to an
/// actor M that is the authority (on `authority`'s revocation-aware line) or
/// a live member of `roster`, the cell sits under that device's key, the
/// entry names M as its `node_id`, and the signature verifies under that
/// device.
///
/// Live membership, not enrolment at a generation (the file row's arm): a
/// removed member's past files stay in the set, but nobody dials a removed
/// member, so its stored cell leaves the merge with its entry.
///
/// # Errors
/// The refusal reason — a cell failing any arm is counted `refused`, never
/// merged and never dialled.
pub fn verify_group_endpoints(
    scope_id: &[u8; 32],
    key: &str,
    value: &[u8],
    authority: &GroupAuthority,
    roster: &RosterView,
) -> Result<HonouredGroupEndpoints, String> {
    let record: GroupEndpointsRecord =
        canonical_decode(value).map_err(|e| format!("endpoints cell does not decode: {e}"))?;
    let core = &record.core;
    if core.scope_id != *scope_id {
        return Err("endpoints cell names another scope".into());
    }
    // Arm (a): the carriage chains the device to an actor.
    let cert = verify_device_cert_carriage(
        "endpoints cell authorization",
        &record.authorization,
        core.stamped_at_ms,
    )?;
    if group_endpoints_cell_key(&cert.device_key) != key {
        return Err("endpoints cell sits under a key that is not its writer device".into());
    }
    // Arm (b): the authority, or a live member.
    if cert.actor_id == *authority.root() {
        authority.verify_device_cert(
            "endpoints cell authorization",
            &record.authorization,
            core.stamped_at_ms,
        )?;
    } else if !roster.is_verified_member(&cert.actor_id) {
        return Err("endpoints cell's writer is not a live member of the scope".into());
    }
    // Arm (c): the entry names its writer and nobody else.
    if core.entry.node_id != cert.actor_id.0 {
        return Err("endpoints cell names a node id that is not its writer's actor".into());
    }
    // Arm (d): the signature.
    let signed =
        group_endpoints_signing_bytes(core, &record.authorization).map_err(|e| e.to_string())?;
    if !crate::identity::verify_detached(&cert.device_key, &signed, &record.signature) {
        return Err("endpoints cell signature does not verify".into());
    }
    Ok(HonouredGroupEndpoints {
        member: cert.actor_id,
        device: cert.device_key,
        record,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Capability, DeviceAuthorization, Timestamp};
    use crate::encoding::{EmbedAsBytes, sign_envelope};
    use crate::group_scope::{RosterEntryCore, roster_cell_key, sign_roster_enrollment};
    use crate::identity::ActorKeypair;

    const SCOPE: [u8; 32] = [0x5e; 32];
    const AT: i64 = 1_700_000_000_000;

    fn device(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn cert_bytes(signer: &ActorKeypair, device: &SigningKey) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(signer, &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
            .expect("encode carriage")
            .to_vec()
    }

    /// The scope: authority Alice (device 0xA1) with Bob (device 0xB1)
    /// enrolled; Carol (device 0xC1) is a stranger.
    struct Scope {
        alice: ActorKeypair,
        bob: ActorKeypair,
        carol: ActorKeypair,
        authority: GroupAuthority,
        roster: RosterView,
    }

    fn scope() -> Scope {
        let alice = ActorKeypair::from_secret([0x01; 32]);
        let bob = ActorKeypair::from_secret([0x02; 32]);
        let carol = ActorKeypair::from_secret([0x03; 32]);
        let authority = GroupAuthority::build(&SCOPE, &alice.actor_id(), &[], []);
        let alice_device = device(0xA1);
        let mut rows = Vec::new();
        for member in [&alice, &bob] {
            let (entry_id, record) = sign_roster_enrollment(
                &alice_device,
                RosterEntryCore {
                    scope_id: SCOPE,
                    member_actor: member.actor_id(),
                    admission_salt: [member.actor_id().0[0]; 32],
                },
                vec![0xE0; 8],
                cert_bytes(&alice, &alice_device),
                2_000,
            )
            .expect("entry");
            rows.push((
                roster_cell_key(&entry_id, &alice_device.verifying_key().to_bytes()),
                canonical_encode(&record).expect("row").to_vec(),
            ));
        }
        let roster = RosterView::build(
            &SCOPE,
            &authority,
            rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        );
        assert!(roster.is_verified_member(&bob.actor_id()));
        Scope {
            alice,
            bob,
            carol,
            authority,
            roster,
        }
    }

    fn cell(writer: &ActorKeypair, device: &SigningKey, node_id: [u8; 32]) -> (String, Vec<u8>) {
        let record = GroupEndpointsRecord::sign(
            GroupEndpointsCore {
                scope_id: SCOPE,
                entry: DeviceEndpointsEntry {
                    node_id,
                    lan_addrs: vec!["10.0.0.7:4433".into()],
                    ..Default::default()
                },
                stamped_at_ms: AT,
            },
            device,
            cert_bytes(writer, device),
        )
        .unwrap();
        (
            group_endpoints_cell_key(&device.verifying_key().to_bytes()),
            canonical_encode(&record).unwrap().to_vec(),
        )
    }

    #[test]
    fn a_members_own_cell_is_honoured_as_that_member() {
        let s = scope();
        for (who, device) in [(&s.alice, device(0xA1)), (&s.bob, device(0xB1))] {
            let (key, value) = cell(who, &device, who.actor_id().0);
            let honoured =
                verify_group_endpoints(&SCOPE, &key, &value, &s.authority, &s.roster).unwrap();
            assert_eq!(honoured.member, who.actor_id());
            assert_eq!(honoured.device, device.verifying_key().to_bytes());
            assert_eq!(
                honoured.entry().lan_addrs,
                vec!["10.0.0.7:4433".to_string()]
            );
        }
    }

    /// The defect: a member names its own actor and nobody else — a cell
    /// naming a co-member steers nobody's dial.
    #[test]
    fn a_cell_naming_a_co_member_is_refused() {
        let s = scope();
        let (key, value) = cell(&s.bob, &device(0xB1), s.alice.actor_id().0);
        assert_eq!(
            verify_group_endpoints(&SCOPE, &key, &value, &s.authority, &s.roster).unwrap_err(),
            "endpoints cell names a node id that is not its writer's actor"
        );
    }

    /// A cell filed under another device's key — the displacement forgery —
    /// is refused before any stamp is compared.
    #[test]
    fn a_cell_under_another_devices_key_is_refused() {
        let s = scope();
        let (_, value) = cell(&s.bob, &device(0xB1), s.bob.actor_id().0);
        let alice_key = group_endpoints_cell_key(&device(0xA1).verifying_key().to_bytes());
        assert_eq!(
            verify_group_endpoints(&SCOPE, &alice_key, &value, &s.authority, &s.roster)
                .unwrap_err(),
            "endpoints cell sits under a key that is not its writer device"
        );
    }

    #[test]
    fn a_strangers_cell_and_a_borrowed_carriage_are_refused() {
        let s = scope();
        let (key, value) = cell(&s.carol, &device(0xC1), s.carol.actor_id().0);
        assert_eq!(
            verify_group_endpoints(&SCOPE, &key, &value, &s.authority, &s.roster).unwrap_err(),
            "endpoints cell's writer is not a live member of the scope"
        );
        // Bob's cert on a core Carol's device signed: the signature arm.
        let bob_device = device(0xB1);
        let mut forged = GroupEndpointsRecord::sign(
            GroupEndpointsCore {
                scope_id: SCOPE,
                entry: DeviceEndpointsEntry {
                    node_id: s.bob.actor_id().0,
                    ..Default::default()
                },
                stamped_at_ms: AT,
            },
            &device(0xC1),
            cert_bytes(&s.bob, &bob_device),
        )
        .unwrap();
        let key = group_endpoints_cell_key(&bob_device.verifying_key().to_bytes());
        let value = canonical_encode(&forged).unwrap().to_vec();
        assert_eq!(
            verify_group_endpoints(&SCOPE, &key, &value, &s.authority, &s.roster).unwrap_err(),
            "endpoints cell signature does not verify"
        );
        // And the scope is signed.
        forged.core.scope_id = [0x5f; 32];
        let value = canonical_encode(&forged).unwrap().to_vec();
        assert_eq!(
            verify_group_endpoints(&SCOPE, &key, &value, &s.authority, &s.roster).unwrap_err(),
            "endpoints cell names another scope"
        );
    }
}
