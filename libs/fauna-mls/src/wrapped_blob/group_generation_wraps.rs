//! X-Wing wraps of T20 **group generation keys** —
//! [`super::generation_wraps`] re-targeted at storage groups
//! (`account-data-plane.md` § The audience ladder → *The recipient-set
//! scheme*, the generations bullet; key half: `key-material-hierarchy.md`
//! § Audience: a storage group).
//!
//! A group generation's random key reaches a member only as an X-Wing wrap
//! sealed to a **roster entry's reception public key** — inline on the mint
//! entry (a [`GroupMemberWrap`]) or as a `fauna.group.generation-wrap`
//! member top-up row. There is **no escrow wrap on this plane** (delta (ii):
//! durability is each member's own account custody), so this module owns
//! exactly two seal/open doors and the two pure builders.
//!
//! **Every open verifies the group key commitment** —
//! `BLAKE3::derive_key(GROUP_GENERATION_KEY_COMMIT_CONTEXT, gen_key)` must
//! equal the commitment the caller resolved from the mint — so a substituted
//! key is refused at the unwrapping member, never trusted from the wire. The
//! wire container, AEAD schedule, and refusal shapes are
//! [`super::generation_wraps`]'s own (shared `pub(super)` helpers, never a
//! re-implementation).
//!
//! [`build_group_mint`] is the pure assembly half only; the sequence around
//! it — read merged roster state, choose the trigger, write the entries —
//! is engine work, deliberately not here (this crate knows crypto, not
//! planes). ST-007 carries over: builder guards stop nobody who crafts rows
//! directly; `fauna_core::group_generation::resolve_admissible_group_tip` is
//! the trust boundary.

use fauna_core::crypto::GenerationKey;
use fauna_core::group_generation::{
    GroupAdmissionBundle, GroupGenerationMintRecord, GroupMemberWrap, GroupMintCore,
    GroupRetainedGeneration, GroupTopupRecord, group_generation_id,
    group_generation_key_commitment, group_topup_cell_key, sign_group_mint_as_minter,
    sign_group_topup_as_healer,
};
use fauna_core::group_scope::RosterMember;
use fauna_pq_kem::XWingSecretKey;

use super::format::{AadBinding, UnwrapError, WrapError};
use super::generation_wraps::{decode_wrap, encode_wrap, parse_target};
use super::xwing_envelope::{xwing_open, xwing_seal};

/// Seal `gen_key` to one roster entry's reception public key, bound to
/// `(group_generation_id, entry_id)` — the wrap bytes a [`GroupMemberWrap`]
/// or a top-up row carries.
///
/// # Errors
/// [`WrapError::InvalidInput`] on a reception key that is malformed OR fails
/// FIPS 203 validation ([`parse_target`] catches both); [`WrapError::HpkeFailed`]
/// only on an AEAD seal failure.
pub fn seal_group_generation_key_to_entry(
    gen_key: &GenerationKey,
    reception_pubkey: &[u8],
    group_generation_id: &[u8; 32],
    entry_id: &[u8; 32],
) -> Result<Vec<u8>, WrapError> {
    let target = parse_target(reception_pubkey)?;
    let binding = AadBinding::for_group_generation_wrap(group_generation_id, entry_id);
    let (enc, ct) = xwing_seal(&target, &binding, &binding, gen_key.as_bytes())?;
    encode_wrap(enc, ct)
}

/// Open a group wrap as the member holding `entry_id`'s reception secret,
/// verifying the recovered key's **group** commitment against
/// `expected_commitment` (the mint's [`GroupMintCore::key_commitment`],
/// which the content-derived generation id covers).
///
/// # Errors
/// [`UnwrapError::HpkeFailed`] for a wrong secret, tampered bytes, or a wrap
/// bound to another `(generation, entry)` slot;
/// [`UnwrapError::CommitmentMismatch`] for a well-formed wrap carrying a key
/// that is not the mint's — wrap substitution, refused here.
pub fn open_group_generation_key_as_entry(
    wrap: &[u8],
    reception_secret: &XWingSecretKey,
    group_generation_id: &[u8; 32],
    entry_id: &[u8; 32],
    expected_commitment: &[u8; 32],
) -> Result<GenerationKey, UnwrapError> {
    let wire = decode_wrap(wrap)?;
    let binding = AadBinding::for_group_generation_wrap(group_generation_id, entry_id);
    let pt = xwing_open(reception_secret, &binding, &binding, &wire.enc, &wire.ct)?;
    let bytes: [u8; 32] = pt
        .as_slice()
        .try_into()
        .map_err(|_| UnwrapError::InvalidFormat("group generation key must be 32 bytes".into()))?;
    let key = GenerationKey::from_bytes(bytes);
    if group_generation_key_commitment(&key) != *expected_commitment {
        return Err(UnwrapError::CommitmentMismatch);
    }
    Ok(key)
}

/// One assembled-but-unpublished group mint. No escrow half exists (delta
/// (ii)) — the record and the key are the whole yield.
pub struct BuiltGroupMint {
    /// The content-derived group generation id — the mint row's logical key.
    pub generation_id: [u8; 32],
    /// The `Minted` record: DAG core + minter proof + every enrolled entry's
    /// inline wrap.
    pub record: GroupGenerationMintRecord,
    /// The minted key itself — the minter seals content under it once the
    /// row is written.
    pub gen_key: GenerationKey,
}

/// Assemble one group generation mint: mint a fresh random key, commit it
/// under the group context, derive the content-derived id, sign it as the
/// minting authority device, and seal a wrap for **every** verified roster
/// member handed in.
///
/// `members` is the verified wrap-target set from the *observer's* merged
/// roster ([`fauna_core::group_scope::RosterView::wrap_targets`] — entry-id
/// order, deterministic); `minter_key` is the minting **authority device's**
/// signing key and `authorization` its `DeviceAuthorization` carriage (the
/// caller holds the cert; the resolver verifies it chains to the authority
/// root — nothing here could). The minter is not a member (module header).
/// `revoked_past` is the authority devices the minter's line revokes — the
/// set the mint is minted PAST ([`GroupMintCore::revoked_past`]: sorted and
/// de-duplicated here, so a caller may hand in the line's iterator in any
/// order); a minter on a plane with no authority line passes an empty set.
///
/// # Errors
/// [`WrapError::InvalidInput`] on an empty member set or a reception key that
/// is malformed OR fails FIPS 203 validation; [`WrapError::HpkeFailed`] only
/// on an AEAD seal failure; [`WrapError::CborEncode`] on a core that fails
/// canonical encoding.
pub fn build_group_mint(
    members: &[RosterMember],
    parents: Vec<[u8; 32]>,
    revoked_past: Vec<[u8; 32]>,
    minter_key: &ed25519_dalek::SigningKey,
    authorization: Vec<u8>,
    minted_at_ms: i64,
) -> Result<BuiltGroupMint, WrapError> {
    if members.is_empty() {
        return Err(WrapError::InvalidInput(
            "a group mint needs at least one verified roster member to wrap to".into(),
        ));
    }
    let gen_key = GenerationKey::mint();
    let mut revoked_past = revoked_past;
    revoked_past.sort_unstable();
    revoked_past.dedup();
    let core = GroupMintCore {
        parents,
        member_entries: members.iter().map(|m| m.entry_id).collect(),
        minter: minter_key.verifying_key().to_bytes(),
        key_commitment: group_generation_key_commitment(&gen_key),
        minted_at_ms,
        revoked_past,
    };
    let id = group_generation_id(&core)
        .map_err(|e| WrapError::CborEncode(format!("group mint core encode: {e}")))?;
    let minter_sig = sign_group_mint_as_minter(minter_key, &id);

    let mut wraps = Vec::with_capacity(members.len());
    for member in members {
        wraps.push(GroupMemberWrap {
            entry_id: member.entry_id,
            wrap: seal_group_generation_key_to_entry(
                &gen_key,
                &member.reception_pubkey,
                &id,
                &member.entry_id,
            )?,
        });
    }

    Ok(BuiltGroupMint {
        generation_id: id,
        record: GroupGenerationMintRecord::Minted {
            core,
            minter_sig,
            authorization,
            wraps,
        },
        gen_key,
    })
}

/// Build one `fauna.group.generation-wrap` member top-up row: `gen_key`
/// sealed to a verified, non-removed roster member currently lacking a wrap
/// — the self-healing path for admission/mint races and the vehicle an add's
/// retained-generation backfill rides. Returns the row as
/// `(cell key, record)`; the healer is a **member actor** (pass the healing
/// member's own `actor.signing_key()`), and callers check merged roster
/// state before targeting — the receiving member verifies the commitment on
/// open.
///
/// # Errors
/// As [`seal_group_generation_key_to_entry`].
pub fn build_group_topup_wrap(
    gen_key: &GenerationKey,
    target: &RosterMember,
    group_generation_id: &[u8; 32],
    healer_key: &ed25519_dalek::SigningKey,
    at_ms: i64,
) -> Result<(String, GroupTopupRecord), WrapError> {
    let wrap = seal_group_generation_key_to_entry(
        gen_key,
        &target.reception_pubkey,
        group_generation_id,
        &target.entry_id,
    )?;
    let healer = healer_key.verifying_key().to_bytes();
    let healer_sig = sign_group_topup_as_healer(
        healer_key,
        group_generation_id,
        &target.entry_id,
        at_ms,
        &wrap,
    );
    Ok((
        group_topup_cell_key(group_generation_id, &target.entry_id, &healer),
        GroupTopupRecord::Wrap {
            generation_id: *group_generation_id,
            target_entry: target.entry_id,
            healer,
            at_ms,
            wrap,
            healer_sig,
        },
    ))
}

/// The opened admission bundle: the verified machinery root plus the
/// retained (generation id, key) pairs awaiting the caller's per-mint
/// commitment checks.
pub type OpenedAdmissionBundle = (
    fauna_core::crypto::GroupMachineryRoot,
    Vec<([u8; 32], GenerationKey)>,
);

/// Seal one admission bundle — the scope's machinery root plus the retained
/// generation bundle `(generation_id, key)` pairs — to a joiner's reception
/// public key, bound to `(scope_id, entry_id)`. The deliver step's
/// `admission_wrap` bytes (`fauna_core::group_ceremony::GroupShareDeliver`).
///
/// # Errors
/// As [`seal_group_generation_key_to_entry`], plus a bundle that fails
/// canonical encoding.
pub fn seal_group_admission_bundle(
    root: &fauna_core::crypto::GroupMachineryRoot,
    retained: &[([u8; 32], &GenerationKey)],
    reception_pubkey: &[u8],
    scope_id: &[u8; 32],
    entry_id: &[u8; 32],
) -> Result<Vec<u8>, WrapError> {
    let bundle = GroupAdmissionBundle {
        machinery_root: fauna_core::secret::SecretByteBuf::from(root.as_bytes().to_vec()),
        retained: retained
            .iter()
            .map(|(id, key)| GroupRetainedGeneration {
                generation_id: *id,
                key: fauna_core::secret::SecretByteBuf::from(key.as_bytes().to_vec()),
            })
            .collect(),
    };
    let plaintext = fauna_core::encoding::canonical_encode(&bundle)
        .map_err(|e| WrapError::CborEncode(format!("admission bundle encode: {e}")))?;
    let target = parse_target(reception_pubkey)?;
    let binding = AadBinding::for_group_admission_bundle(scope_id, entry_id);
    let (enc, ct) = xwing_seal(&target, &binding, &binding, &plaintext)?;
    encode_wrap(enc, ct)
}

/// Open an admission bundle as the joiner holding `entry_id`'s reception
/// secret, verifying the recovered **machinery root's commitment against
/// the birth record** in-door — root substitution at admission is a crisp
/// [`UnwrapError::CommitmentMismatch`] at the joiner, never silent AEAD
/// garbage downstream (the id↔root binding's joiner half; the caller
/// separately re-derives the offered scope id from the same birth record).
///
/// Returns the root and the retained `(generation_id, key)` pairs. The
/// generation keys are deliberately NOT commitment-checked here: their
/// commitments live in the mint DAG, which the joiner can only read once
/// the root is in hand — the caller verifies each against its resolved mint
/// (`group_generation_key_commitment`) before first use, exactly what the
/// resolver's `observer_keyable` closure does.
///
/// # Errors
/// [`UnwrapError::HpkeFailed`] for a wrong secret, tampered bytes, or a
/// bundle bound to another `(scope, entry)` slot;
/// [`UnwrapError::CommitmentMismatch`] for a substituted root;
/// [`UnwrapError::InvalidFormat`] for malformed plaintext.
pub fn open_group_admission_bundle(
    wrap: &[u8],
    reception_secret: &XWingSecretKey,
    scope_id: &[u8; 32],
    entry_id: &[u8; 32],
    birth: &fauna_core::group_scope::GroupBirthRecord,
) -> Result<OpenedAdmissionBundle, UnwrapError> {
    let wire = decode_wrap(wrap)?;
    let binding = AadBinding::for_group_admission_bundle(scope_id, entry_id);
    let pt = xwing_open(reception_secret, &binding, &binding, &wire.enc, &wire.ct)?;
    let bundle: GroupAdmissionBundle = fauna_core::encoding::canonical_decode(&pt)
        .map_err(|e| UnwrapError::InvalidFormat(format!("admission bundle decode: {e}")))?;
    let root_bytes: [u8; 32] = bundle.machinery_root.as_ref().try_into().map_err(|_| {
        UnwrapError::InvalidFormat("admission bundle machinery root must be 32 bytes".into())
    })?;
    let root = fauna_core::crypto::GroupMachineryRoot::from_bytes(root_bytes);
    if !fauna_core::group_scope::verify_machinery_root_commitment(birth, &root) {
        return Err(UnwrapError::CommitmentMismatch);
    }
    let mut retained = Vec::with_capacity(bundle.retained.len());
    for entry in &bundle.retained {
        let key_bytes: [u8; 32] = entry.key.as_ref().try_into().map_err(|_| {
            UnwrapError::InvalidFormat("admission bundle generation key must be 32 bytes".into())
        })?;
        retained.push((entry.generation_id, GenerationKey::from_bytes(key_bytes)));
    }
    Ok((root, retained))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use fauna_pq_kem::XWingKeyPair;

    fn reception_keypair(seed: u8) -> XWingKeyPair {
        fauna_pq_kem::derive_keypair_from_ikm(
            &[seed; 32],
            "fauna.test.group-reception.mlkem",
            "fauna.test.group-reception.x25519",
        )
    }

    fn member_at(entry_seed: u8, key_seed: u8) -> RosterMember {
        RosterMember {
            entry_id: [entry_seed; 32],
            member_actor: ActorKeypair::from_secret([key_seed; 32]).actor_id(),
            reception_pubkey: reception_keypair(key_seed).public.to_bytes().to_vec(),
            enrolled_at_ms: 2_000,
        }
    }

    fn minter() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x41; 32])
    }

    #[test]
    fn a_built_mint_opens_at_each_members_own_slot_and_nowhere_else() {
        let alice = member_at(0xA1, 0x31);
        let bob = member_at(0xB2, 0x32);
        let built = build_group_mint(
            &[alice.clone(), bob.clone()],
            vec![[0x01; 32]],
            vec![],
            &minter(),
            vec![0xCE; 8],
            5_000,
        )
        .expect("build");
        let GroupGenerationMintRecord::Minted { core, wraps, .. } = &built.record else {
            panic!("built a Minted record");
        };
        assert_eq!(core.member_entries, vec![alice.entry_id, bob.entry_id]);

        // Each member opens its own wrap and recovers the committed key.
        let alice_wrap = &wraps.iter().find(|w| w.entry_id == alice.entry_id).unwrap();
        let opened = open_group_generation_key_as_entry(
            &alice_wrap.wrap,
            &reception_keypair(0x31).secret,
            &built.generation_id,
            &alice.entry_id,
            &core.key_commitment,
        )
        .expect("alice opens");
        assert_eq!(opened.as_bytes(), built.gen_key.as_bytes());

        // Bob's secret does not open Alice's slot...
        assert!(matches!(
            open_group_generation_key_as_entry(
                &alice_wrap.wrap,
                &reception_keypair(0x32).secret,
                &built.generation_id,
                &alice.entry_id,
                &core.key_commitment,
            ),
            Err(UnwrapError::HpkeFailed)
        ));
        // ...and Alice's wrap lifted to Bob's slot fails AEAD (binding).
        assert!(matches!(
            open_group_generation_key_as_entry(
                &alice_wrap.wrap,
                &reception_keypair(0x31).secret,
                &built.generation_id,
                &bob.entry_id,
                &core.key_commitment,
            ),
            Err(UnwrapError::HpkeFailed)
        ));
    }

    /// **Row 749** — the exact shape that let one poisoned room seat freeze
    /// every mint: a reception key of the right length (1216 bytes) but a
    /// FIPS-203-invalid ML-KEM half is refused by `parse_target` before any
    /// seal runs, as `InvalidInput`, on the SAME door every mint over a
    /// community room's floor calls. Before the fix this key passed the
    /// length-only gate that stored it and only failed here, inside
    /// `encapsulate` — freezing every later mint over the same roster, not
    /// just this one build call.
    #[test]
    fn a_group_mint_refuses_a_fips_invalid_reception_key_before_sealing() {
        let alice = member_at(0xA1, 0x31);
        let poisoned = RosterMember {
            entry_id: [0xB2; 32],
            member_actor: ActorKeypair::from_secret([0x32; 32]).actor_id(),
            reception_pubkey: vec![0xFFu8; fauna_pq_kem::XWING_ENCAPS_KEY_LEN],
            enrolled_at_ms: 2_000,
        };
        assert!(matches!(
            build_group_mint(&[alice, poisoned], vec![], vec![], &minter(), vec![], 5_000,),
            Err(WrapError::InvalidInput(_))
        ));
    }

    /// Wrap substitution: a well-formed wrap carrying a DIFFERENT key than
    /// the mint committed to is refused at the opener, never trusted.
    #[test]
    fn a_substituted_key_fails_the_commitment_check() {
        let alice = member_at(0xA1, 0x31);
        let built = build_group_mint(
            std::slice::from_ref(&alice),
            vec![],
            vec![],
            &minter(),
            vec![],
            5_000,
        )
        .expect("build");
        let GroupGenerationMintRecord::Minted { core, .. } = &built.record else {
            panic!("built a Minted record");
        };
        // An attacker re-wraps a key of its own choosing into Alice's slot.
        let substituted = GenerationKey::from_bytes([0x66; 32]);
        let forged_wrap = seal_group_generation_key_to_entry(
            &substituted,
            &alice.reception_pubkey,
            &built.generation_id,
            &alice.entry_id,
        )
        .expect("seal");
        assert!(matches!(
            open_group_generation_key_as_entry(
                &forged_wrap,
                &reception_keypair(0x31).secret,
                &built.generation_id,
                &alice.entry_id,
                &core.key_commitment,
            ),
            Err(UnwrapError::CommitmentMismatch)
        ));
    }

    #[test]
    fn a_topup_row_verifies_at_its_cell_and_opens_for_its_target() {
        let alice = member_at(0xA1, 0x31);
        let healer = ActorKeypair::from_secret([0x32; 32]);
        let gen_key = GenerationKey::from_bytes([0x0D; 32]);
        let generation_id = [0x51; 32];
        let (cell, record) = build_group_topup_wrap(
            &gen_key,
            &alice,
            &generation_id,
            healer.signing_key(),
            7_000,
        )
        .expect("build");
        let (g, t, h) =
            fauna_core::group_generation::parse_group_topup_cell_key(&cell).expect("parses");
        assert!(record.verifies_at(&g, &t, &h));
        let GroupTopupRecord::Wrap { wrap, .. } = &record;
        let opened = open_group_generation_key_as_entry(
            wrap,
            &reception_keypair(0x31).secret,
            &generation_id,
            &alice.entry_id,
            &fauna_core::group_generation::group_generation_key_commitment(&gen_key),
        )
        .expect("target opens");
        assert_eq!(opened.as_bytes(), gen_key.as_bytes());
    }

    #[test]
    fn an_admission_bundle_hands_the_root_and_retained_keys_to_its_joiner_only() {
        use fauna_core::crypto::GroupMachineryRoot;
        use fauna_core::group_scope::GroupBirthRecord;
        use fauna_core::identity::ActorKeypair;

        let root = GroupMachineryRoot::from_bytes([0xD7; 32]);
        let birth = GroupBirthRecord {
            authority_actor: ActorKeypair::from_secret([21u8; 32]).actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: root.commitment(),
            created_at_ms: 1_700_000_000_000,
        };
        let scope_id = fauna_core::group_scope::group_scope_id(&birth).unwrap();
        let joiner = member_at(0xA1, 0x31);
        let tip_key = GenerationKey::from_bytes([0x0D; 32]);
        let older_key = GenerationKey::from_bytes([0x0C; 32]);

        let wrap = seal_group_admission_bundle(
            &root,
            &[([0x51; 32], &tip_key), ([0x50; 32], &older_key)],
            &joiner.reception_pubkey,
            &scope_id,
            &joiner.entry_id,
        )
        .expect("seal");

        let (opened_root, retained) = open_group_admission_bundle(
            &wrap,
            &reception_keypair(0x31).secret,
            &scope_id,
            &joiner.entry_id,
            &birth,
        )
        .expect("joiner opens");
        assert_eq!(opened_root.as_bytes(), root.as_bytes());
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].0, [0x51; 32]);
        assert_eq!(retained[0].1.as_bytes(), tip_key.as_bytes());

        // Another member's secret does not open this joiner's bundle...
        assert!(matches!(
            open_group_admission_bundle(
                &wrap,
                &reception_keypair(0x32).secret,
                &scope_id,
                &joiner.entry_id,
                &birth,
            ),
            Err(UnwrapError::HpkeFailed)
        ));
        // ...and the bundle lifted to another entry slot fails AEAD.
        assert!(matches!(
            open_group_admission_bundle(
                &wrap,
                &reception_keypair(0x31).secret,
                &scope_id,
                &[0xB2; 32],
                &birth,
            ),
            Err(UnwrapError::HpkeFailed)
        ));
    }

    /// Root substitution at admission: a bundle carrying a root the birth
    /// record's scope id did not commit to is a crisp refusal at the joiner.
    #[test]
    fn a_substituted_machinery_root_fails_the_bundle_commitment_check() {
        use fauna_core::crypto::GroupMachineryRoot;
        use fauna_core::group_scope::GroupBirthRecord;
        use fauna_core::identity::ActorKeypair;

        let honest_root = GroupMachineryRoot::from_bytes([0xD7; 32]);
        let birth = GroupBirthRecord {
            authority_actor: ActorKeypair::from_secret([21u8; 32]).actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: honest_root.commitment(),
            created_at_ms: 1_700_000_000_000,
        };
        let scope_id = fauna_core::group_scope::group_scope_id(&birth).unwrap();
        let joiner = member_at(0xA1, 0x31);

        let substituted = GroupMachineryRoot::from_bytes([0xD8; 32]);
        let wrap = seal_group_admission_bundle(
            &substituted,
            &[],
            &joiner.reception_pubkey,
            &scope_id,
            &joiner.entry_id,
        )
        .expect("seal");
        assert!(matches!(
            open_group_admission_bundle(
                &wrap,
                &reception_keypair(0x31).secret,
                &scope_id,
                &joiner.entry_id,
                &birth,
            ),
            Err(UnwrapError::CommitmentMismatch)
        ));
    }

    #[test]
    fn an_empty_member_set_is_a_build_refusal() {
        assert!(build_group_mint(&[], vec![], vec![], &minter(), vec![], 5_000).is_err());
    }
}
