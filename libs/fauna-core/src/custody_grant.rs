//! The custody-grant admission witness — the owner-minted, keyless capability
//! that admits a **non-fleet** custodian replica to pull and re-serve an
//! account's sealed planes.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § Replica posture →
//! *The custody grant + ceremony* (T13, resolved 2026-08-15). The witness is
//! deliberately NOT a `GrantBlob`: the blob is an HPKE key conveyance carrying
//! no owner signature, while this witness conveys no keys and is all
//! signature. It rides the same sign-over-CID [`EmbedAsBytes`] carriage as its
//! sibling witness, the `DeviceAuthorization` (`verify_device_admission_witness`
//! in [`crate::encoding`]) — self-contained, verifiable against nothing but
//! the account identity, no registry lookup (the admission seam's carriage
//! rule).
//!
//! Lifecycle (mint/renew/revoke events, the keyless nest row, the succession
//! re-mint sweep) lives with the capability plane, not here — this module owns
//! only the witness shape and its admission-side verification.

use serde::{Deserialize, Serialize};

use crate::data::Timestamp;
use crate::encoding::{EmbedAsBytes, Signed, decode_signed_bytes, sign_envelope, verify_envelope};
use crate::error::{Error, Result};
use crate::identity::{ActorId, ActorKeypair};

/// A custody grant's id is 16 opaque bytes in the capability-grant id space
/// (the nest `capability_grants` PK is `(owner_actor_id, grant_id)`).
pub const CUSTODY_GRANT_ID_LEN: usize = 16;

/// Mint a fresh ceremony grant id — 16 caller-random bytes in the
/// capability-grant id space (`OfferParams.grant_id`'s contract). One shared
/// source so no app hand-rolls its own randomness idiom.
pub fn random_grant_id() -> [u8; CUSTODY_GRANT_ID_LEN] {
    let mut id = [0u8; CUSTODY_GRANT_ID_LEN];
    getrandom::fill(&mut id).expect("getrandom failed");
    id
}

/// Refuse a grant id of the wrong length. `qualifier` names the payload kind
/// for the error text (`""` for the bare grant, `"witness"`/`"ceremony"`/
/// `"receipt"` for its siblings) — every custody payload that carries a
/// `grant_id` field checks it this way, so this is the one shared source
/// (`custody_ceremony`/`custody_receipt` used to hand-copy it).
pub(crate) fn check_grant_id(grant_id: &[u8], qualifier: &str) -> Result<()> {
    if grant_id.len() != CUSTODY_GRANT_ID_LEN {
        let noun = if qualifier.is_empty() {
            "custody grant id".to_string()
        } else {
            format!("custody {qualifier} grant id")
        };
        return Err(Error::Encoding(format!(
            "{noun} must be {CUSTODY_GRANT_ID_LEN} bytes, got {}",
            grant_id.len()
        )));
    }
    Ok(())
}

/// The class-2 entry key both custody registry kinds
/// (`fauna.state.custodies-held` on the custodian's plane,
/// `fauna.state.custodian-endpoints` on the owner's) use for a custody's
/// row: the grant id, lowercase hex — one row per custody, one spelling.
pub fn custody_entry_key(grant_id: &[u8]) -> String {
    hex::encode(grant_id)
}

/// Which of the owner's scopes a custody grant covers.
///
/// The `Account` form is the coverage-decay antidote (T13's register
/// constraint): it names the owner's whole **single-principal** scope set,
/// current *and future* — the admission side evaluates it as a predicate over
/// scope strings, never as a set frozen at mint, so a scope joined tomorrow is
/// covered with no re-mint. The shared-audience carve-out binds it: scopes
/// whose plane other accounts co-author (a `conv` channel; any future group
/// scope) are **excluded** — such a scope enters a cross-account custody grant
/// only as a deliberate [`Self::Scopes`] entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodyScopeSet {
    /// The owner's whole single-principal scope set, current and future
    /// (evaluated live at admission; co-authored scopes carved out).
    Account,
    /// Exactly the named scope strings (`fauna_protocol::scope` vocabulary —
    /// canonical spellings, validated at mint, matched by string equality at
    /// admission). The subset form; also the only door for co-authored scopes.
    Scopes(Vec<String>),
    /// A form a newer build mints and this one does not name, carried whole
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*): the witness still decodes, so its `removed_devices` exclusion
    /// list is still honoured, while the set itself covers no scope here.
    #[serde(untagged)]
    Unknown(crate::carried::CarriedValue),
}

/// The custody-grant witness: owner-actor-signed over its canonical dag-cbor
/// encoding, carried as [`EmbedAsBytes`] in the admission exchange.
///
/// Field idioms mirror the sibling `DeviceAuthorization` (the other
/// [`Signed`] admission witness) so the two verifiers read alike.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyGrant {
    /// 16 opaque bytes in the capability-grant id space — what the nest row
    /// and the grant-event log key on, and what a revocation store answers
    /// about. A CBOR byte string (`serialization.md` § Canonical IPLD
    /// dag-cbor), as on every type carrying the id.
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// The granting account — the signer of this witness.
    pub owner: ActorId,
    /// The custodian's device-principal Ed25519 public key, which IS its
    /// peer-plane NodeId (R5 (account-data-plane.md § The ratified decisions)). A witness admits this proven key, never a
    /// bearer.
    #[serde(with = "serde_bytes")]
    pub custodian_key: [u8; 32],
    pub scopes: CustodyScopeSet,
    pub minted_at: Timestamp,
    /// Required, not optional: expiry is load-bearing for the ceremony's
    /// decay-to-re-offer and the ~90-day capability default.
    pub expires_at: Timestamp,
    /// The owner's **removed-device exclusion list**: the fleet ids the
    /// minting device's own verified fleet view excluded at mint
    /// (`account-replica-posture.md` § The custody grant + ceremony, the
    /// witness bullet). A custodian cannot read the custodied account's
    /// sealed device-set rows, so this owner-signed list is its only
    /// exclusion set for that account: a device named here is refused at the
    /// custodian for the grant's whole life, in both directions. Never
    /// learned from an admitted device's say-so — the removed device is
    /// itself admitted there.
    ///
    /// Canonical (sorted, deduplicated — [`canonical_removed_devices`]; the
    /// sign door refuses anything else). Additive: an empty list is absent on
    /// the wire and an absent key decodes empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(with = "crate::byte_array::vec")]
    pub removed_devices: Vec<[u8; 32]>,
}

/// Put a removed-device set in the witness's canonical form — sorted,
/// deduplicated — the one shape [`sign_custody_grant`] accepts.
pub fn canonical_removed_devices(ids: impl IntoIterator<Item = [u8; 32]>) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = ids.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    v
}

impl Signed for CustodyGrant {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.owner.0
    }
}

/// A verified custody admission — what the witness proves about a
/// channel-proven peer key. The admission seam's adaptor
/// (`fauna_peer_sync::admission`) turns it into a per-connection verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyAdmission {
    /// The grant id — what the serve side's revocation stores key on.
    pub grant_id: Vec<u8>,
    /// The custodied account (the witness's signer).
    pub owner: ActorId,
    /// The admitted custodian key — equal to the channel-proven peer key by
    /// construction (checked here, never assumed).
    pub custodian_key: [u8; 32],
    pub scopes: CustodyScopeSet,
    /// The verdict's validity bound is `min(connection lifetime, this)`.
    pub expires_at: Timestamp,
    /// The owner's signed exclusion list ([`CustodyGrant::removed_devices`]).
    pub removed_devices: Vec<[u8; 32]>,
}

/// Sign a [`CustodyGrant`] with the owner's actor identity key and pack it as
/// the [`EmbedAsBytes`] witness the admission exchange carries.
///
/// Refuses a grant whose `owner` is not the signing keypair's actor — a
/// witness that could never verify must not be mintable.
pub fn sign_custody_grant(owner: &ActorKeypair, grant: &CustodyGrant) -> Result<EmbedAsBytes> {
    if grant.owner != owner.actor_id() {
        return Err(Error::Encoding(
            "custody grant names an owner other than the signing keypair".into(),
        ));
    }
    check_grant_id(&grant.grant_id, "")?;
    if !grant.removed_devices.windows(2).all(|w| w[0] < w[1]) {
        return Err(Error::Encoding(
            "custody grant removed-device list must be sorted and deduplicated".into(),
        ));
    }
    let (bytes, env) = sign_envelope(owner, grant)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Verify a custody-grant **admission witness** (`account-data-plane.md`
/// § The peer leg → *The admission seam*; shape ruled at § Replica posture →
/// *The custody grant + ceremony*).
///
/// The seam's rules, in order — deliberately the same four as the
/// `DeviceAuthorization` twin ([`crate::encoding::verify_device_admission_witness`]):
///
/// 1. The grant's own envelope verifies under `grant.owner` — owner-signed,
///    self-contained (carriage is inline; no registry lookup).
/// 2. **A witness admits a proven key, never a bearer**: `grant.custodian_key`
///    must equal `proven_key`, the channel-proven identity. Possession of the
///    envelope alone conveys nothing.
/// 3. The grant's owner is the expected account — the account whose planes the
///    evaluating side would serve (or, on the pull side, the account it dialed
///    a replica of).
/// 4. The witness has not expired at `now` (`expires_at == now` is still
///    valid — the bound is "past", not "at").
///
/// **Revocation is deliberately not checked here.** The witness is
/// self-contained; the two revocation stores (the synced grant-event log on
/// fleet replicas, the capability row on the nest) are each consulted by their
/// own evaluator beside this call — T13's two-stores rule.
pub fn verify_custody_witness(
    witness: &EmbedAsBytes,
    proven_key: &[u8; 32],
    expected_account: &ActorId,
    now: Timestamp,
) -> Result<CustodyAdmission> {
    // Step 1 — the grant verifies under its own owner.
    let (grant_bytes, grant_env) = witness.clone().into_signed()?;
    let grant: CustodyGrant = decode_signed_bytes(&grant_bytes)?;
    verify_envelope(&grant, &grant_bytes, &grant_env)
        .map_err(|_| Error::Encoding("custody witness signature invalid".into()))?;
    check_grant_id(&grant.grant_id, "witness")?;
    // Step 2 — the witness admits the channel-proven key, never a bearer.
    if grant.custodian_key != *proven_key {
        return Err(Error::Encoding(
            "custody witness names a custodian key that is not the channel-proven peer".into(),
        ));
    }
    // Step 3 — the witness's owner is the account in question.
    if grant.owner != *expected_account {
        return Err(Error::Encoding(
            "custody witness is signed by a different account".into(),
        ));
    }
    // Step 4 — validity bound.
    if grant.expires_at.0 < now.0 {
        return Err(Error::Encoding("custody witness has expired".into()));
    }
    Ok(CustodyAdmission {
        grant_id: grant.grant_id,
        owner: grant.owner,
        custodian_key: grant.custodian_key,
        scopes: grant.scopes,
        expires_at: grant.expires_at,
        removed_devices: grant.removed_devices,
    })
}

/// The witness's grant, read **without** an admission check — for decisions
/// over rows a verifying ingest door already admitted: the hosting pump's
/// expired-past-grace store GC ([`custody_witness_expiry`]) and the
/// custodian's removed-device exclusion map, which unions the lists of every
/// grant it holds for an account. The envelope is still signature-verified
/// under its own owner — a corrupted or forged row must not steer
/// reclamation or name a device as removed — but no proven-key / account /
/// `now` rule runs: an EXPIRED witness is exactly what the GC reads, and
/// [`verify_custody_witness`] refuses those by design.
pub fn verified_custody_grant(witness: &EmbedAsBytes) -> Result<CustodyGrant> {
    let (grant_bytes, grant_env) = witness.clone().into_signed()?;
    let grant: CustodyGrant = decode_signed_bytes(&grant_bytes)?;
    verify_envelope(&grant, &grant_bytes, &grant_env)
        .map_err(|_| Error::Encoding("custody witness signature invalid".into()))?;
    Ok(grant)
}

/// The witness's expiry instant, read through [`verified_custody_grant`].
pub fn custody_witness_expiry(witness: &EmbedAsBytes) -> Result<Timestamp> {
    Ok(verified_custody_grant(witness)?.expires_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared refusal `custody_ceremony`/`custody_receipt` now delegate
    /// to: right length passes, wrong length fails with the qualifier
    /// stitched into the noun (empty qualifier omits the extra word) so each
    /// caller's original error text is unchanged byte-for-byte.
    #[test]
    fn check_grant_id_qualifies_its_error_noun() {
        let ok = vec![0x1D; CUSTODY_GRANT_ID_LEN];
        assert!(check_grant_id(&ok, "").is_ok());
        assert!(check_grant_id(&ok, "witness").is_ok());

        let short = vec![0x1D; CUSTODY_GRANT_ID_LEN - 1];
        let err = check_grant_id(&short, "").unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "custody grant id must be {CUSTODY_GRANT_ID_LEN} bytes, got {}",
                CUSTODY_GRANT_ID_LEN - 1
            )),
            "{err}"
        );
        let err = check_grant_id(&short, "ceremony").unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "custody ceremony grant id must be {CUSTODY_GRANT_ID_LEN} bytes, got {}",
                CUSTODY_GRANT_ID_LEN - 1
            )),
            "{err}"
        );

        // Over-length, not just short — every other fixture in the family is
        // short, which leaves an accept-and-truncate mutant undetected (row
        // 601).
        let long = vec![0x1D; CUSTODY_GRANT_ID_LEN + 1];
        let err = check_grant_id(&long, "").unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "custody grant id must be {CUSTODY_GRANT_ID_LEN} bytes, got {}",
                CUSTODY_GRANT_ID_LEN + 1
            )),
            "{err}"
        );
    }

    fn grant(owner: &ActorKeypair, custodian: [u8; 32], scopes: CustodyScopeSet) -> CustodyGrant {
        CustodyGrant {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: owner.actor_id(),
            custodian_key: custodian,
            scopes,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(5_000),
            removed_devices: Vec::new(),
        }
    }

    /// The lifecycle read: the expiry comes back for a witness that
    /// [`verify_custody_witness`] refuses as expired — that is the point —
    /// but never for a tampered envelope (a corrupt row must not steer GC).
    #[test]
    fn witness_expiry_reads_an_expired_witness_but_never_a_tampered_one() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let g = grant(&owner, [0xC5u8; 32], CustodyScopeSet::Account);
        let witness = sign_custody_grant(&owner, &g).expect("sign");
        assert!(
            verify_custody_witness(&witness, &[0xC5u8; 32], &owner.actor_id(), Timestamp(9_000))
                .is_err(),
            "expired at 9_000 for the admission check"
        );
        assert_eq!(
            custody_witness_expiry(&witness).expect("expiry reads"),
            Timestamp(5_000)
        );

        // A forged envelope (signed by someone other than the named owner)
        // answers nothing.
        let mallory = ActorKeypair::from_secret([13u8; 32]);
        let mut forged = g.clone();
        forged.owner = mallory.actor_id();
        let forged_witness = sign_custody_grant(&mallory, &forged).expect("sign as mallory");
        // Re-point the inner owner claim at the honest owner by tampering:
        // decode, swap, re-embed without a matching signature.
        let (bytes, env) = witness.clone().into_signed().expect("split");
        let tampered = EmbedAsBytes::from_signed(
            {
                let mut b = bytes.clone();
                let last = b.len() - 1;
                b[last] ^= 0x01;
                b
            },
            env,
        );
        assert!(custody_witness_expiry(&tampered).is_err(), "tampered bytes");
        // The mallory-signed grant naming mallory as owner verifies under its
        // own envelope — expiry read is per-envelope-honesty, and admission
        // (owner binding) stays verify_custody_witness's job.
        assert!(custody_witness_expiry(&forged_witness).is_ok());
    }

    #[test]
    fn custody_witness_admits_the_proven_key_in_both_scope_forms() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        for scopes in [
            CustodyScopeSet::Account,
            CustodyScopeSet::Scopes(vec![format!("content:conv:{}", "2b".repeat(32))]),
        ] {
            let g = grant(&owner, custodian, scopes.clone());
            let witness = sign_custody_grant(&owner, &g).expect("sign");
            let admission =
                verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(2_000))
                    .expect("witness admits");
            assert_eq!(admission.grant_id, g.grant_id);
            assert_eq!(admission.owner, owner.actor_id());
            assert_eq!(admission.custodian_key, custodian);
            assert_eq!(admission.scopes, scopes);
            assert_eq!(admission.expires_at, Timestamp(5_000));
        }
    }

    /// The seam's key-binding rule: a valid envelope presented from a
    /// *different* channel-proven key conveys nothing.
    #[test]
    fn custody_witness_rejects_a_bearer_with_someone_elses_grant() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let g = grant(&owner, [0xC5u8; 32], CustodyScopeSet::Account);
        let witness = sign_custody_grant(&owner, &g).expect("sign");
        let err = verify_custody_witness(
            &witness,
            &[0xC6u8; 32], // the channel proved a different key
            &owner.actor_id(),
            Timestamp(2_000),
        )
        .expect_err("bearer must be refused");
        assert!(err.to_string().contains("channel-proven"), "{err}");
    }

    /// A grant signed by a different account's identity is refused even when
    /// the custodian key matches — the witness verifies against nothing but
    /// the expected account's own identity.
    #[test]
    fn custody_witness_rejects_a_foreign_owner() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let other = ActorKeypair::from_secret([10u8; 32]);
        let custodian = [0xC5u8; 32];
        let g = grant(&other, custodian, CustodyScopeSet::Account);
        let witness = sign_custody_grant(&other, &g).expect("sign");
        let err = verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(2_000))
            .expect_err("foreign owner must be refused");
        assert!(err.to_string().contains("different account"), "{err}");
    }

    #[test]
    fn custody_witness_rejects_expired_and_accepts_at_the_bound() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let g = grant(&owner, custodian, CustodyScopeSet::Account);
        let witness = sign_custody_grant(&owner, &g).expect("sign");
        verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(5_000))
            .expect("at the bound is still valid");
        let err = verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(5_001))
            .expect_err("past expiry must be refused");
        assert!(err.to_string().contains("expired"), "{err}");
    }

    /// Tampered inner bytes fail the envelope's CID check before any field is
    /// believed.
    #[test]
    fn custody_witness_rejects_tampered_bytes() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let g = grant(&owner, custodian, CustodyScopeSet::Account);
        let mut witness = sign_custody_grant(&owner, &g).expect("sign");
        let last = witness.bytes.len() - 1;
        witness.bytes[last] ^= 0x01;
        verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(2_000))
            .expect_err("tampered bytes must be refused");
    }

    /// The id-space guard holds on both doors: an off-length grant id can be
    /// neither minted nor believed.
    #[test]
    fn custody_grant_id_length_is_enforced_at_sign_and_verify() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let mut g = grant(&owner, custodian, CustodyScopeSet::Account);
        g.grant_id = vec![0x1D; 8];
        sign_custody_grant(&owner, &g).expect_err("short id must not mint");
        // Force-sign the malformed grant through the generic path to prove the
        // verifier holds its own door.
        let (bytes, env) = sign_envelope(&owner, &g).expect("raw sign");
        let witness = EmbedAsBytes::from_signed(bytes, env);
        let err = verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(2_000))
            .expect_err("short id must not verify");
        assert!(err.to_string().contains("grant id"), "{err}");
    }

    /// A grant naming an owner the signing keypair is not cannot be minted —
    /// it could never verify, so the mint door refuses it early.
    #[test]
    fn sign_refuses_an_owner_mismatch() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let other = ActorKeypair::from_secret([10u8; 32]);
        let g = grant(&other, [0xC5u8; 32], CustodyScopeSet::Account);
        sign_custody_grant(&owner, &g).expect_err("owner mismatch must not mint");
    }

    /// The wire shape round-trips through canonical encoding — the same
    /// embed-as-bytes discipline every signed kind rides.
    #[test]
    fn custody_witness_round_trips_through_canonical_encoding() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let g = grant(
            &owner,
            [0xC5u8; 32],
            CustodyScopeSet::Scopes(vec!["state".into()]),
        );
        let witness = sign_custody_grant(&owner, &g).expect("sign");
        let encoded = crate::encoding::canonical_encode(&witness).expect("encode");
        let decoded: EmbedAsBytes = crate::encoding::canonical_decode(&encoded).expect("decode");
        assert_eq!(decoded, witness);
        verify_custody_witness(
            &decoded,
            &g.custodian_key,
            &owner.actor_id(),
            Timestamp(2_000),
        )
        .expect("round-tripped witness still verifies");
    }

    // ── the removed-device exclusion list ────────────────────────────────

    /// The pre-field witness shape, field for field — what every owner
    /// minted before the exclusion list existed.
    #[derive(Serialize, Deserialize)]
    struct PreFieldCustodyGrant {
        #[serde(with = "serde_bytes")]
        grant_id: Vec<u8>,
        owner: ActorId,
        #[serde(with = "serde_bytes")]
        custodian_key: [u8; 32],
        scopes: CustodyScopeSet,
        minted_at: Timestamp,
        expires_at: Timestamp,
    }

    fn pre_field(g: &CustodyGrant) -> PreFieldCustodyGrant {
        PreFieldCustodyGrant {
            grant_id: g.grant_id.clone(),
            owner: g.owner,
            custodian_key: g.custodian_key,
            scopes: g.scopes.clone(),
            minted_at: g.minted_at,
            expires_at: g.expires_at,
        }
    }

    /// An empty list is BYTE-IDENTICAL to the pre-field encoding, so an owner
    /// with nothing removed mints exactly what an old build minted — and an
    /// old custodian, which decodes without `deny_unknown_fields`, reads a
    /// listing grant as the grant it always was (the field is ignored).
    #[test]
    fn an_empty_exclusion_list_encodes_byte_identically_to_a_pre_field_grant() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let g = grant(&owner, [0xC5u8; 32], CustodyScopeSet::Account);
        assert!(g.removed_devices.is_empty());
        assert_eq!(
            crate::encoding::canonical_encode(&g).unwrap(),
            crate::encoding::canonical_encode(&pre_field(&g)).unwrap(),
            "an empty list must not appear on the wire"
        );

        // New owner → old custodian: the listing grant decodes as the old
        // shape (the unknown field is ignored, not refused).
        let mut listing = g.clone();
        listing.removed_devices = vec![[0x0Fu8; 32]];
        let bytes = crate::encoding::canonical_encode(&listing).unwrap();
        let old: PreFieldCustodyGrant = crate::encoding::canonical_decode(&bytes)
            .expect("an old decoder ignores the additive field");
        assert_eq!(old.grant_id, g.grant_id);
    }

    /// Old owner → new custodian: pre-field bytes decode with an empty list,
    /// and a witness an old owner signed still verifies and admits.
    #[test]
    fn pre_field_bytes_decode_with_an_empty_exclusion_list() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let old = pre_field(&grant(&owner, custodian, CustodyScopeSet::Account));
        let bytes = crate::encoding::canonical_encode(&old).unwrap();
        let decoded: CustodyGrant = crate::encoding::canonical_decode(&bytes).unwrap();
        assert!(decoded.removed_devices.is_empty());

        let (b, env) = sign_envelope(&owner, &decoded).unwrap();
        assert_eq!(b, bytes, "the new shape re-encodes an old grant exactly");
        let witness = EmbedAsBytes::from_signed(b, env);
        let admission =
            verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(2_000))
                .expect("an old owner's witness still admits");
        assert!(admission.removed_devices.is_empty());
        assert!(
            verified_custody_grant(&witness)
                .unwrap()
                .removed_devices
                .is_empty()
        );
    }

    /// The list is signed with the rest of the witness and carried through
    /// both reads — the admission, and the verified lifecycle read the
    /// custodian's exclusion map derives from.
    #[test]
    fn the_exclusion_list_is_signed_and_carried_through_both_reads() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let mut g = grant(&owner, custodian, CustodyScopeSet::Account);
        g.removed_devices = canonical_removed_devices([[0x0Fu8; 32], [0x0Au8; 32], [0x0Fu8; 32]]);
        assert_eq!(g.removed_devices, vec![[0x0Au8; 32], [0x0Fu8; 32]]);
        let witness = sign_custody_grant(&owner, &g).expect("sign");
        let admission =
            verify_custody_witness(&witness, &custodian, &owner.actor_id(), Timestamp(2_000))
                .expect("admits");
        assert_eq!(admission.removed_devices, g.removed_devices);
        assert_eq!(verified_custody_grant(&witness).unwrap(), g);

        // An edit to the list under the original signature does not verify:
        // a custodian never believes a list the owner did not sign.
        let (_, env) = witness.clone().into_signed().unwrap();
        let mut edited = g.clone();
        edited.removed_devices = vec![[0x0Au8; 32]];
        let edited_bytes = crate::encoding::canonical_encode(&edited).unwrap();
        let swapped = EmbedAsBytes::from_signed(edited_bytes, env);
        verified_custody_grant(&swapped).expect_err("an unsigned list edit must not verify");
    }

    /// Canonical at mint, like `scopes`: an unsorted or duplicated list is
    /// refused by the sign door (one grant content, one encoding).
    #[test]
    fn sign_refuses_a_non_canonical_exclusion_list() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        for list in [
            vec![[0x0Fu8; 32], [0x0Au8; 32]],
            vec![[0x0Au8; 32], [0x0Au8; 32]],
        ] {
            let mut g = grant(&owner, [0xC5u8; 32], CustodyScopeSet::Account);
            g.removed_devices = list;
            let err = sign_custody_grant(&owner, &g).expect_err("non-canonical list");
            assert!(err.to_string().contains("removed-device"), "{err}");
        }
    }
}
