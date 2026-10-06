//! X-Wing wraps of R14 (account-data-plane.md § The ratified decisions) **generation keys** — R14 build step 5
//! (`account-data-plane.md` § The generation machinery → *The mint protocol*;
//! key half: `owner-key-material.md` § Path A-sibling-2 → *The schedule build
//! design*).
//!
//! A generation's random key ([`fauna_core::crypto::GenerationKey`]) reaches a
//! device only as an X-Wing wrap — inline on the mint entry (a
//! [`MemberWrap`]), as a `fauna.state.generation-wrap` top-up row, or as the
//! identity-targeted escrow wrap a holder serves back at recovery. This module
//! owns the seal/open of all three, sharing [`super::xwing_envelope`]'s
//! RFC 9180 base-mode schedule (the mail-at-rest precedent: wraps are
//! promiscuously-replicated resting ciphertext forever — the
//! harvest-now-decrypt-later shape X-Wing exists for).
//!
//! **Every open verifies the key commitment** before handing the key out:
//! `commit = BLAKE3(gen_key)` under the frozen commit context must equal the
//! commitment the caller resolved from the mint (which the content-derived
//! generation id covers), so a substituted key is refused at the unwrapping
//! device, never trusted from the wire ([`UnwrapError::CommitmentMismatch`]).
//!
//! [`build_mint`] is the pure assembly half of the mint protocol: fleet
//! members + escrow target in, `Minted` record + escrow wrap out. The
//! *sequence* around it — read merged device-set state, refuse without an
//! escrow target, **deposit escrow first**, write the entries — is engine
//! work (`fauna_sync_engine::generation_mint`), deliberately not here: this
//! crate knows crypto, not planes.

use fauna_core::crypto::GenerationKey;
use fauna_core::generation::{
    EscrowTargetRecord, FleetMember, GenerationMintRecord, MAX_MINT_MEMBERS, MemberWrap, MintCore,
    generation_id, inline_wrap_members,
};
use fauna_pq_kem::{XWingPublicKey, XWingSecretKey};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use super::format::{AadBinding, UnwrapError, WrapError};
use super::xwing_envelope::{XWING_ENC_LEN, xwing_open, xwing_seal};

// The escrow wrap's AAD `target_key` is the `fauna.state.escrow-target` row's
// logical key — `fauna_core::generation::escrow_target_identity_key`
// (`identity/<actor-id-hex>`, one per identity) for the identity-derived
// target. Callers pass it; this crate deliberately does not depend on the
// registry and names no key of its own.

/// Wire form of one generation wrap: a versioned canonical-CBOR container
/// around the X-Wing `(enc, ct)` pair. Versioned because these bytes rest
/// forever on every custodian and holder — a future suite migrates by a new
/// version byte, never by reinterpreting v1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct GenerationWrapWire {
    #[serde(rename = "v")]
    pub(super) version: u8,
    /// The X-Wing encapsulation ([`XWING_ENC_LEN`] bytes).
    pub(super) enc: ByteBuf,
    /// The AEAD ciphertext (32-byte key + Poly1305 tag).
    pub(super) ct: ByteBuf,
}

const WRAP_VERSION: u8 = 1;

pub(super) fn encode_wrap(enc: Vec<u8>, ct: Vec<u8>) -> Result<Vec<u8>, WrapError> {
    fauna_cbor::encode_canonical(&GenerationWrapWire {
        version: WRAP_VERSION,
        enc: ByteBuf::from(enc),
        ct: ByteBuf::from(ct),
    })
    .map_err(|e| WrapError::CborEncode(e.to_string()))
}

pub(super) fn decode_wrap(bytes: &[u8]) -> Result<GenerationWrapWire, UnwrapError> {
    let wire: GenerationWrapWire = fauna_cbor::decode_strict(bytes)
        .map_err(|e| UnwrapError::InvalidFormat(format!("generation wrap cbor: {e}")))?;
    if wire.version != WRAP_VERSION {
        return Err(UnwrapError::InvalidFormat(format!(
            "unsupported generation wrap version: {}",
            wire.version
        )));
    }
    if wire.enc.len() != XWING_ENC_LEN {
        return Err(UnwrapError::InvalidFormat(format!(
            "generation wrap enc must be {XWING_ENC_LEN} bytes, got {}",
            wire.enc.len()
        )));
    }
    Ok(wire)
}

/// Parse AND validate a device-set row's published KEM public key (the
/// 1216-byte X-Wing wire form) — [`XWingPublicKey::parse_and_validate`], the
/// one function every writer that stores this shape and every sealer that
/// targets it now shares: a malformed OR a FIPS-203-invalid target
/// is a refusal here, never a silently skipped member and never a failure
/// deferred to the seal below — a mint that cannot wrap to an enrolled member
/// must not pretend it did.
pub(super) fn parse_target(pubkey: &[u8]) -> Result<XWingPublicKey, WrapError> {
    XWingPublicKey::parse_and_validate(pubkey).map_err(|e| WrapError::InvalidInput(e.to_string()))
}

/// Seal `gen_key` to one device's KEM public key, bound to
/// `(generation_id, target_device)` — the wrap bytes a [`MemberWrap`] or a
/// [`fauna_core::generation::GenerationWrapRecordV2`] carries.
///
/// # Errors
/// [`WrapError::InvalidInput`] on a malformed target key OR one that fails
/// FIPS 203 validation ([`parse_target`] catches both before the seal runs);
/// [`WrapError::HpkeFailed`] only on an AEAD seal failure.
pub fn seal_generation_key_to_device(
    gen_key: &GenerationKey,
    target_pubkey: &[u8],
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
) -> Result<Vec<u8>, WrapError> {
    let target = parse_target(target_pubkey)?;
    let binding = AadBinding::for_generation_wrap(generation_id, target_device);
    let (enc, ct) = xwing_seal(&target, &binding, &binding, gen_key.as_bytes())?;
    encode_wrap(enc, ct)
}

/// Open a device wrap as `target_device`, verifying the recovered key's
/// commitment against `expected_commitment` (the mint's
/// [`MintCore::key_commitment`], which the generation id covers).
///
/// # Errors
/// [`UnwrapError::HpkeFailed`] for a wrong secret, tampered bytes, or a wrap
/// bound to another `(generation, device)` slot;
/// [`UnwrapError::CommitmentMismatch`] for a well-formed wrap carrying a key
/// that is not the mint's — wrap substitution, refused here.
pub fn open_generation_key_as_device(
    wrap: &[u8],
    device_secret: &XWingSecretKey,
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    expected_commitment: &[u8; 32],
) -> Result<GenerationKey, UnwrapError> {
    let wire = decode_wrap(wrap)?;
    let binding = AadBinding::for_generation_wrap(generation_id, target_device);
    let pt = xwing_open(device_secret, &binding, &binding, &wire.enc, &wire.ct)?;
    key_if_committed(pt, expected_commitment)
}

/// Seal `gen_key` to a published escrow target — the deposit the mint hands
/// the escrow doors, bound to `(generation_id, target_key)` where
/// `target_key` is the escrow-target row's logical key
/// (`fauna_core::generation::escrow_target_identity_key` for the
/// identity-derived target).
///
/// # Errors
/// As [`seal_generation_key_to_device`].
pub fn seal_generation_key_to_escrow(
    gen_key: &GenerationKey,
    escrow_target: &EscrowTargetRecord,
    generation_id: &[u8; 32],
    target_key: &str,
) -> Result<Vec<u8>, WrapError> {
    let target = parse_target(&escrow_target.xwing_escrow_pubkey)?;
    let binding = AadBinding::for_generation_escrow(generation_id, target_key);
    let (enc, ct) = xwing_seal(&target, &binding, &binding, gen_key.as_bytes())?;
    encode_wrap(enc, ct)
}

/// Open an escrow wrap with the escrow secret (re-derived from the identity
/// seed at a recovery ceremony —
/// `fauna_core::generation::derive_escrow_xwing_keypair`), verifying the
/// commitment exactly like the device path.
///
/// # Errors
/// As [`open_generation_key_as_device`].
pub fn open_generation_key_from_escrow(
    wrap: &[u8],
    escrow_secret: &XWingSecretKey,
    generation_id: &[u8; 32],
    target_key: &str,
    expected_commitment: &[u8; 32],
) -> Result<GenerationKey, UnwrapError> {
    let wire = decode_wrap(wrap)?;
    let binding = AadBinding::for_generation_escrow(generation_id, target_key);
    let pt = xwing_open(escrow_secret, &binding, &binding, &wire.enc, &wire.ct)?;
    key_if_committed(pt, expected_commitment)
}

fn key_if_committed(
    plaintext: Vec<u8>,
    expected_commitment: &[u8; 32],
) -> Result<GenerationKey, UnwrapError> {
    let bytes: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| UnwrapError::InvalidFormat("generation key must be 32 bytes".into()))?;
    let key = GenerationKey::from_bytes(bytes);
    if key.commitment() != *expected_commitment {
        return Err(UnwrapError::CommitmentMismatch);
    }
    Ok(key)
}

/// One assembled-but-unpublished mint: everything the engine's deposit-first
/// sequence needs, in the order it needs it — the escrow wrap goes to the
/// doors *before* [`Self::record`] and the receipt row are written anywhere.
pub struct BuiltMint {
    /// The content-derived generation id — the mint row's logical key.
    pub generation_id: [u8; 32],
    /// The `Minted` record: DAG core (every member listed) + the inline wraps
    /// of the members [`fauna_core::generation::inline_wrap_members`] names.
    pub record: GenerationMintRecord,
    /// The escrow deposit, sealed to the published target.
    pub escrow_wrap: Vec<u8>,
    /// The minted key itself — the minter seals under it once the sequence
    /// completes, and hands it to its own key schedule
    /// (`FleetOnlySchedule::derive_for_generation`).
    pub gen_key: GenerationKey,
    /// The **spill**: every listed member WITHOUT an inline wrap (the bounded
    /// mint, charter § The mint protocol → *The bounded mint*), in the order
    /// `members` gave them. The minter reaches each with its own top-up row
    /// (`build_topup_wrap_v2` — the ordinary healer shape, from
    /// [`Self::gen_key`]) in the same sequence, ahead of the escrow receipt.
    /// Empty for a fleet of at most `MAX_INLINE_MEMBER_WRAPS` members.
    pub spilled: Vec<FleetMember>,
}

/// Assemble one generation mint: mint a fresh random key, commit it, derive
/// the content-derived id, sign it as the minter, list **every** verified
/// member, seal an inline wrap for the bounded inline set plus the escrow
/// target, and name the spill.
///
/// `members` is the verified wrap-target set from the *observer's* merged
/// device-set view (`FleetView::wrap_targets` — id order, deterministic);
/// `minter_key` is the minting device's own signing key — its public half is
/// the minter id, which must be one of the members ("any enrolled device may
/// mint" — and only an enrolled one). The member set is embedded in the core
/// and the mint is signed by its minter, so downstream admissibility
/// re-verifies both against merged state and the signature (ST-007: the
/// resolver is the trust boundary; these builder guards alone stop nobody who
/// crafts the row directly).
///
/// **The bounded mint** (ruled 2026-09-15): one inline wrap was ~2.4 KB as
/// encoded then (~1.3 KB since its bytes ride as a CBOR byte string) and the
/// plane caps a sealed entry at 64 KiB, so a row wrapping every member
/// stopped fitting past about 27 devices and the fleet could
/// never mint again. The inline set is capped at
/// `fauna_core::generation::MAX_INLINE_MEMBER_WRAPS` (the minter always
/// among them); the rest are returned as [`BuiltMint::spilled`] for the
/// caller to top up — same key, same wrap format, the healer cell shape the
/// top-up pass already reads and counts as coverage. Admissibility is
/// unchanged: `member_ids` still lists everyone.
///
/// # Errors
/// [`WrapError::InvalidInput`] on an empty member set, a minter outside it,
/// a member set over `MAX_MINT_MEMBERS` (the ceiling the member list's own
/// encoding imposes under the per-entry cap), or a target key that is
/// malformed OR fails FIPS 203 validation ([`parse_target`]);
/// [`WrapError::HpkeFailed`] only on an AEAD seal failure.
pub fn build_mint(
    members: &[FleetMember],
    escrow_target: &EscrowTargetRecord,
    escrow_target_key: &str,
    parents: Vec<[u8; 32]>,
    minter_key: &ed25519_dalek::SigningKey,
    minted_at_ms: i64,
) -> Result<BuiltMint, WrapError> {
    let minter = minter_key.verifying_key().to_bytes();
    if members.is_empty() {
        return Err(WrapError::InvalidInput(
            "a mint needs at least one verified member to wrap to".into(),
        ));
    }
    if !members.iter().any(|m| m.device_id == minter) {
        return Err(WrapError::InvalidInput(
            "the minter must be a verified member of its own mint".into(),
        ));
    }
    if members.len() > MAX_MINT_MEMBERS {
        return Err(WrapError::InvalidInput(format!(
            "a mint over {} members is over the {MAX_MINT_MEMBERS}-member ceiling: the member \
             list alone would not seal under the plane's per-entry cap (charter § The bounded \
             mint) — remove devices the account no longer uses",
            members.len()
        )));
    }

    let gen_key = GenerationKey::mint();
    let core = MintCore {
        parents,
        member_ids: members.iter().map(|m| m.device_id).collect(),
        minter,
        key_commitment: gen_key.commitment(),
        minted_at_ms,
    };
    let id = generation_id(&core)
        .map_err(|e| WrapError::CborEncode(format!("mint core encode: {e}")))?;
    let minter_sig = fauna_core::generation::sign_mint_as_minter(minter_key, &id);

    // Every member's target key is validated (`parse_target`)
    // whether or not it is wrapped inline: a mint that cannot wrap to an
    // enrolled member must not pretend it did, and the spill's top-ups seal
    // to the same keys later in the sequence.
    let inline = inline_wrap_members(&core.member_ids, &minter);
    let mut wraps = Vec::with_capacity(inline.len());
    let mut spilled = Vec::new();
    for member in members {
        if inline.contains(&member.device_id) {
            wraps.push(MemberWrap {
                device_id: member.device_id,
                wrap: seal_generation_key_to_device(
                    &gen_key,
                    &member.xwing_pubkey,
                    &id,
                    &member.device_id,
                )?,
            });
        } else {
            parse_target(&member.xwing_pubkey)?;
            spilled.push(member.clone());
        }
    }
    let escrow_wrap =
        seal_generation_key_to_escrow(&gen_key, escrow_target, &id, escrow_target_key)?;

    Ok(BuiltMint {
        generation_id: id,
        record: GenerationMintRecord::Minted {
            core,
            minter_sig,
            wraps,
        },
        escrow_wrap,
        gen_key,
        spilled,
    })
}

/// Build one healer-attributed `fauna.state.generation-wrap` top-up row —
/// `gen_key` sealed to a verified, non-removed member currently lacking a
/// wrap: the self-healing path for enrollment/mint races, for handing
/// retained older generations to a newer device, and the succession
/// re-escrow vehicle. Callers check merged device-set state before targeting
/// (charter § The generation machinery — never a removed id); the receiving
/// device verifies the commitment on open. The hardening's shape, for the per-healer cell
/// [`fauna_core::generation::wrap_cell_key_per_healer`] names for
/// `(generation, target, healer)`. The record is signed by the healer's
/// device key over every field including the wrap bytes' hash, which is what
/// lets the kind's join prefer it over any forged bytes and lets the top-up
/// pass count it as coverage ([`GenerationWrapRecordV2`] owns the contract).
///
/// # Errors
/// As [`seal_generation_key_to_device`].
pub fn build_topup_wrap_v2(
    gen_key: &GenerationKey,
    generation_id: &[u8; 32],
    target: &FleetMember,
    healer_key: &ed25519_dalek::SigningKey,
    at_ms: i64,
) -> Result<fauna_core::generation::GenerationWrapRecordV2, WrapError> {
    let wrap = seal_generation_key_to_device(
        gen_key,
        &target.xwing_pubkey,
        generation_id,
        &target.device_id,
    )?;
    let healer_sig = fauna_core::generation::sign_topup_as_healer(
        healer_key,
        generation_id,
        &target.device_id,
        at_ms,
        &wrap,
    );
    Ok(fauna_core::generation::GenerationWrapRecordV2::Wrap {
        generation_id: *generation_id,
        target_device: target.device_id,
        healer: healer_key.verifying_key().to_bytes(),
        at_ms,
        wrap,
        healer_sig,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target key — any string works for the AAD binding; production's is
    /// `fauna_core::generation::escrow_target_identity_key`.
    const ESCROW_TARGET_IDENTITY_KEY: &str = "identity/test";
    use fauna_core::generation::{derive_device_xwing_keypair, derive_escrow_xwing_keypair};

    /// The device's Ed25519 signing key — `[seed; 32]` is the secret, the
    /// device id is its public half, and the device KEM keypair derives from
    /// the same secret bytes, exactly the production shape
    /// (`derive_device_xwing_keypair(&writer_key.to_bytes())`).
    fn member_key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn member(seed: u8) -> (FleetMember, XWingSecretKey) {
        let kp = derive_device_xwing_keypair(&[seed; 32]);
        (
            FleetMember {
                device_id: member_key(seed).verifying_key().to_bytes(),
                xwing_pubkey: kp.public.to_bytes().to_vec(),
                enrolled_at_ms: 1_000,
            },
            kp.secret,
        )
    }

    fn escrow(seed: u8) -> (EscrowTargetRecord, XWingSecretKey) {
        let kp = derive_escrow_xwing_keypair(&[seed; 32]);
        (
            EscrowTargetRecord {
                xwing_escrow_pubkey: kp.public.to_bytes().to_vec(),
            },
            kp.secret,
        )
    }

    fn core_of(record: &GenerationMintRecord) -> &MintCore {
        match record {
            GenerationMintRecord::Minted { core, .. } => core,
            GenerationMintRecord::Shredded { core, .. } => core,
        }
    }

    /// The whole mint in one flow: every member's inline wrap opens to the
    /// same key, the escrow wrap opens under the escrow secret, and every
    /// recovered key matches the commitment the generation id covers.
    #[test]
    fn a_built_mint_reaches_every_member_and_the_escrow_holder() {
        let (a, a_secret) = member(0x0A);
        let (b, b_secret) = member(0x0B);
        let (target, escrow_secret) = escrow(0x0E);

        let built = build_mint(
            &[a.clone(), b.clone()],
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            5_000,
        )
        .unwrap();

        let core = core_of(&built.record);
        assert_eq!(core.member_ids, vec![a.device_id, b.device_id]);
        assert_eq!(core.key_commitment, built.gen_key.commitment());
        assert_eq!(built.generation_id, generation_id(core).unwrap());

        let GenerationMintRecord::Minted { wraps, .. } = &built.record else {
            panic!("a fresh mint is Minted");
        };
        for (m, secret) in [(&a, &a_secret), (&b, &b_secret)] {
            let wrap = &wraps
                .iter()
                .find(|w| w.device_id == m.device_id)
                .expect("every member has an inline wrap")
                .wrap;
            let opened = open_generation_key_as_device(
                wrap,
                secret,
                &built.generation_id,
                &m.device_id,
                &core.key_commitment,
            )
            .expect("a member opens its own wrap");
            assert_eq!(opened.as_bytes(), built.gen_key.as_bytes());
        }

        let recovered = open_generation_key_from_escrow(
            &built.escrow_wrap,
            &escrow_secret,
            &built.generation_id,
            ESCROW_TARGET_IDENTITY_KEY,
            &core.key_commitment,
        )
        .expect("the escrow secret opens the deposit");
        assert_eq!(recovered.as_bytes(), built.gen_key.as_bytes());
    }

    #[test]
    fn a_mint_refuses_an_empty_member_set_and_a_foreign_minter() {
        let (a, _) = member(0x0A);
        let (target, _) = escrow(0x0E);
        assert!(matches!(
            build_mint(
                &[],
                &target,
                ESCROW_TARGET_IDENTITY_KEY,
                vec![],
                &member_key(0),
                1
            ),
            Err(WrapError::InvalidInput(_))
        ));
        assert!(matches!(
            build_mint(
                &[a],
                &target,
                ESCROW_TARGET_IDENTITY_KEY,
                vec![],
                &member_key(0xFF),
                1
            ),
            Err(WrapError::InvalidInput(_))
        ));
    }

    /// **Row 749.** A target key of the RIGHT length but a FIPS-203-invalid
    /// ML-KEM half is refused at `parse_target` — before the seal ever runs
    /// — as `InvalidInput`, never surfacing as `HpkeFailed` further down. Red
    /// before `parse_target` validated anything past length: an all-0xFF
    /// 1216-byte key passed the length check and only failed inside
    /// `encapsulate`, so this pin is what change (1) of the fix makes true.
    #[test]
    fn a_mint_refuses_a_fips_invalid_target_key_before_sealing() {
        let (a, _) = member(0x0A);
        let (target, _) = escrow(0x0E);
        let poisoned = FleetMember {
            device_id: member_key(0x0B).verifying_key().to_bytes(),
            xwing_pubkey: vec![0xFFu8; fauna_pq_kem::XWING_ENCAPS_KEY_LEN],
            enrolled_at_ms: 1_000,
        };
        assert!(matches!(
            build_mint(
                &[a, poisoned],
                &target,
                ESCROW_TARGET_IDENTITY_KEY,
                vec![],
                &member_key(0x0A),
                1
            ),
            Err(WrapError::InvalidInput(_))
        ));
    }

    /// Wrap substitution is refused at the unwrapping device: a well-formed
    /// wrap from generation X presented as generation Y fails the AEAD (the
    /// binding covers the id), and a key re-wrapped under the right binding
    /// but not matching the mint's commitment fails the commitment check —
    /// the two layers the Key↔id ruling demands, each red-verified by
    /// construction here (remove either check and its arm goes green).
    #[test]
    fn wrap_substitution_fails_binding_or_commitment() {
        let (a, a_secret) = member(0x0A);
        let (target, _) = escrow(0x0E);
        let mint_x = build_mint(
            std::slice::from_ref(&a),
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            1,
        )
        .unwrap();
        let mint_y = build_mint(
            std::slice::from_ref(&a),
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            2,
        )
        .unwrap();
        let x_core = core_of(&mint_x.record);
        let GenerationMintRecord::Minted { wraps: y_wraps, .. } = &mint_y.record else {
            unreachable!()
        };

        // Layer 1 — the AAD binding: Y's wrap presented under X's id fails
        // AEAD before any key surfaces.
        assert!(matches!(
            open_generation_key_as_device(
                &y_wraps[0].wrap,
                &a_secret,
                &mint_x.generation_id,
                &a.device_id,
                &x_core.key_commitment,
            ),
            Err(UnwrapError::HpkeFailed)
        ));

        // Layer 2 — the commitment: re-seal Y's key under X's binding (what a
        // malicious wrap author CAN do — the binding is public) and the
        // commitment check refuses the substituted key.
        let forged = seal_generation_key_to_device(
            &mint_y.gen_key,
            &a.xwing_pubkey,
            &mint_x.generation_id,
            &a.device_id,
        )
        .unwrap();
        assert!(matches!(
            open_generation_key_as_device(
                &forged,
                &a_secret,
                &mint_x.generation_id,
                &a.device_id,
                &x_core.key_commitment,
            ),
            Err(UnwrapError::CommitmentMismatch)
        ));
    }

    /// The escrow door's twin of layer 2. The escrow target's public key and
    /// the `(generation_id, target_key)` binding are both public, so the
    /// escrow holder can seal ANY key under a generation's binding; the
    /// commitment check is the one thing that refuses it.
    #[test]
    fn an_escrow_wrap_substituted_under_the_binding_fails_the_commitment() {
        let (a, _) = member(0x0A);
        let (target, escrow_secret) = escrow(0x0E);
        let build = |epoch| {
            build_mint(
                std::slice::from_ref(&a),
                &target,
                ESCROW_TARGET_IDENTITY_KEY,
                vec![],
                &member_key(0x0A),
                epoch,
            )
            .unwrap()
        };
        let (mint_x, mint_y) = (build(1), build(2));
        let x_core = core_of(&mint_x.record);

        let forged = seal_generation_key_to_escrow(
            &mint_y.gen_key,
            &target,
            &mint_x.generation_id,
            ESCROW_TARGET_IDENTITY_KEY,
        )
        .unwrap();
        assert!(matches!(
            open_generation_key_from_escrow(
                &forged,
                &escrow_secret,
                &mint_x.generation_id,
                ESCROW_TARGET_IDENTITY_KEY,
                &x_core.key_commitment,
            ),
            Err(UnwrapError::CommitmentMismatch)
        ));
    }

    #[test]
    fn a_wrap_bound_to_one_device_does_not_open_for_another() {
        let (a, _) = member(0x0A);
        let (b, b_secret) = member(0x0B);
        let (target, _) = escrow(0x0E);
        let built = build_mint(
            &[a.clone(), b.clone()],
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            1,
        )
        .unwrap();
        let GenerationMintRecord::Minted { wraps, .. } = &built.record else {
            unreachable!()
        };
        let a_wrap = &wraps
            .iter()
            .find(|w| w.device_id == a.device_id)
            .unwrap()
            .wrap;
        // B's secret against A's wrap: wrong KEM key ⇒ AEAD refuses (X-Wing
        // implicit rejection), even before the device-id binding mismatch.
        assert!(matches!(
            open_generation_key_as_device(
                a_wrap,
                &b_secret,
                &built.generation_id,
                &a.device_id,
                &core_of(&built.record).key_commitment,
            ),
            Err(UnwrapError::HpkeFailed)
        ));
    }

    #[test]
    fn a_topup_wrap_carries_the_key_to_a_late_member() {
        let (a, _) = member(0x0A);
        let (late, late_secret) = member(0x1C);
        let (target, _) = escrow(0x0E);
        let built = build_mint(
            std::slice::from_ref(&a),
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            1,
        )
        .unwrap();

        // "The mint that raced my enrollment": a key-holding device tops the
        // late enrollee up through the wrap kind.
        let fauna_core::generation::GenerationWrapRecordV2::Wrap {
            generation_id,
            target_device,
            wrap,
            ..
        } = build_topup_wrap_v2(
            &built.gen_key,
            &built.generation_id,
            &late,
            &member_key(0x0A),
            1,
        )
        .unwrap();
        assert_eq!(generation_id, built.generation_id);
        assert_eq!(target_device, late.device_id);
        let opened = open_generation_key_as_device(
            &wrap,
            &late_secret,
            &generation_id,
            &target_device,
            &core_of(&built.record).key_commitment,
        )
        .expect("the top-up target opens its wrap");
        assert_eq!(opened.as_bytes(), built.gen_key.as_bytes());
    }

    /// The shape: a built v2 row verifies at its own per-healer
    /// cell, its wrap opens for the target, and — the point of the signature
    /// — it stops verifying the moment any field is filed under a different
    /// cell.
    #[test]
    fn a_v2_topup_wrap_verifies_at_its_cell_and_opens_for_the_target() {
        let (a, _) = member(0x0A);
        let (late, late_secret) = member(0x1C);
        let (target, _) = escrow(0x0E);
        let built = build_mint(
            std::slice::from_ref(&a),
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            1,
        )
        .unwrap();

        let record = build_topup_wrap_v2(
            &built.gen_key,
            &built.generation_id,
            &late,
            &member_key(0x0A),
            7,
        )
        .unwrap();
        assert!(record.verifies_at(&built.generation_id, &late.device_id, &a.device_id));
        assert!(
            !record.verifies_at(&built.generation_id, &late.device_id, &late.device_id),
            "the same bytes filed under another healer's cell verify as nothing"
        );
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { wrap, .. } = &record;
        let opened = open_generation_key_as_device(
            wrap,
            &late_secret,
            &built.generation_id,
            &late.device_id,
            &core_of(&built.record).key_commitment,
        )
        .expect("the v2 top-up target opens its wrap");
        assert_eq!(opened.as_bytes(), built.gen_key.as_bytes());
    }

    #[test]
    fn malformed_wraps_and_targets_are_refused_precisely() {
        let (a, a_secret) = member(0x0A);
        let (target, _) = escrow(0x0E);
        // A target key of the wrong length refuses at seal time.
        assert!(matches!(
            seal_generation_key_to_device(
                &GenerationKey::from_bytes([1; 32]),
                &[0u8; 10],
                &[2; 32],
                &a.device_id
            ),
            Err(WrapError::InvalidInput(_))
        ));
        // Junk bytes refuse as format, not as a crypto failure.
        assert!(matches!(
            open_generation_key_as_device(b"junk", &a_secret, &[2; 32], &a.device_id, &[0; 32]),
            Err(UnwrapError::InvalidFormat(_))
        ));
        // A truncated enc refuses as format.
        let built = build_mint(
            std::slice::from_ref(&a),
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &member_key(0x0A),
            1,
        )
        .unwrap();
        let mut wire = decode_wrap(match &built.record {
            GenerationMintRecord::Minted { wraps, .. } => &wraps[0].wrap,
            GenerationMintRecord::Shredded { .. } => unreachable!(),
        })
        .unwrap();
        wire.enc.truncate(XWING_ENC_LEN - 1);
        let short = fauna_cbor::encode_canonical(&wire).unwrap();
        assert!(matches!(
            open_generation_key_as_device(
                &short,
                &a_secret,
                &built.generation_id,
                &a.device_id,
                &[0; 32]
            ),
            Err(UnwrapError::InvalidFormat(_))
        ));
    }

    // ── The bounded mint ─────────────────────────────────────────────────────

    /// A 32-byte seed for member `i` — `member(u8)` runs out at 256, and the
    /// ceiling test needs more than that.
    fn seed_n(i: u16) -> [u8; 32] {
        let mut s = [0xA5u8; 32];
        s[0] = i as u8;
        s[1] = (i >> 8) as u8;
        s
    }

    fn member_n(i: u16) -> (FleetMember, XWingSecretKey) {
        let kp = derive_device_xwing_keypair(&seed_n(i));
        (
            FleetMember {
                device_id: ed25519_dalek::SigningKey::from_bytes(&seed_n(i))
                    .verifying_key()
                    .to_bytes(),
                xwing_pubkey: kp.public.to_bytes().to_vec(),
                enrolled_at_ms: 1_000,
            },
            kp.secret,
        )
    }

    /// Past the inline cap the mint still LISTS every member (admissibility
    /// and severance range over the whole fleet), wraps exactly the cap
    /// inline — the minter among them whatever its id — and names the rest
    /// as the spill; a spilled member is reached by an ordinary top-up from
    /// the minted key, which opens against the same commitment.
    #[test]
    fn a_mint_past_the_inline_cap_lists_everyone_wraps_the_cap_and_names_the_spill() {
        use fauna_core::generation::MAX_INLINE_MEMBER_WRAPS;
        let n = MAX_INLINE_MEMBER_WRAPS as u16 + 3;
        let fleet: Vec<(FleetMember, XWingSecretKey)> = (0..n).map(member_n).collect();
        let members: Vec<FleetMember> = fleet.iter().map(|(m, _)| m.clone()).collect();
        let (target, _) = escrow(0x0E);
        // The minter is the byte-order LAST id, so "first `cap - 1` others"
        // and "the minter always" are both exercised.
        let minter_ix = (0..fleet.len())
            .max_by_key(|&i| members[i].device_id)
            .unwrap();
        let minter_key = ed25519_dalek::SigningKey::from_bytes(&seed_n(minter_ix as u16));

        let built = build_mint(
            &members,
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &minter_key,
            5_000,
        )
        .unwrap();
        let GenerationMintRecord::Minted { core, wraps, .. } = &built.record else {
            panic!("a fresh mint is Minted");
        };
        assert_eq!(core.member_ids.len(), n as usize, "every member is listed");
        assert_eq!(
            wraps.len(),
            MAX_INLINE_MEMBER_WRAPS,
            "exactly the cap is inline"
        );
        assert!(
            wraps.iter().any(|w| w.device_id == core.minter),
            "the minter is always inline"
        );
        assert_eq!(built.spilled.len(), 3);
        for s in &built.spilled {
            assert!(
                core.member_ids.contains(&s.device_id),
                "a spilled member is listed"
            );
            assert!(
                !wraps.iter().any(|w| w.device_id == s.device_id),
                "a spilled member has no inline wrap"
            );
        }
        // The inline set is the smallest ids among the non-minters.
        let mut others: Vec<[u8; 32]> = members
            .iter()
            .map(|m| m.device_id)
            .filter(|id| *id != core.minter)
            .collect();
        others.sort_unstable();
        for id in &others[..MAX_INLINE_MEMBER_WRAPS - 1] {
            assert!(
                wraps.iter().any(|w| w.device_id == *id),
                "smallest ids inline"
            );
        }
        // A spilled member keys the generation through the minter's top-up.
        let spilled = &built.spilled[0];
        let (_, spilled_secret) = fleet
            .iter()
            .find(|(m, _)| m.device_id == spilled.device_id)
            .unwrap();
        let row = build_topup_wrap_v2(
            &built.gen_key,
            &built.generation_id,
            spilled,
            &minter_key,
            5_001,
        )
        .unwrap();
        assert!(row.verifies_at(&built.generation_id, &spilled.device_id, &core.minter));
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { wrap, .. } = &row;
        let opened = open_generation_key_as_device(
            wrap,
            spilled_secret,
            &built.generation_id,
            &spilled.device_id,
            &core.key_commitment,
        )
        .expect("the spilled member opens the minter's top-up");
        assert_eq!(opened.as_bytes(), built.gen_key.as_bytes());
    }

    /// A fleet of at most the cap is all-inline: the spill is empty and
    /// nothing about the pre-bounding shape changed for it.
    #[test]
    fn a_fleet_within_the_inline_cap_is_all_inline_with_no_spill() {
        use fauna_core::generation::MAX_INLINE_MEMBER_WRAPS;
        let members: Vec<FleetMember> = (0..MAX_INLINE_MEMBER_WRAPS as u16)
            .map(|i| member_n(i).0)
            .collect();
        let (target, _) = escrow(0x0E);
        let built = build_mint(
            &members,
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &ed25519_dalek::SigningKey::from_bytes(&seed_n(0)),
            5_000,
        )
        .unwrap();
        let GenerationMintRecord::Minted { wraps, .. } = &built.record else {
            unreachable!()
        };
        assert_eq!(wraps.len(), members.len());
        assert!(built.spilled.is_empty());
    }

    /// One member past the ceiling refuses before any key material exists,
    /// naming the ceiling; a spilled member with an invalid target key still
    /// refuses the whole mint (the rule does not stop at the inline set).
    #[test]
    fn a_mint_over_the_member_ceiling_or_with_a_bad_spilled_target_is_refused() {
        let mut members: Vec<FleetMember> = (0..=MAX_MINT_MEMBERS as u16)
            .map(|i| member_n(i).0)
            .collect();
        let (target, _) = escrow(0x0E);
        let minter_key = ed25519_dalek::SigningKey::from_bytes(&seed_n(0));
        // `.err().expect` rather than `expect_err`: a `BuiltMint` carries the
        // minted key and deliberately implements no `Debug`.
        #[allow(clippy::err_expect)]
        let err = build_mint(
            &members,
            &target,
            ESCROW_TARGET_IDENTITY_KEY,
            vec![],
            &minter_key,
            1,
        )
        .err()
        .expect("one over the ceiling");
        assert!(
            matches!(&err, WrapError::InvalidInput(m) if m.contains("member ceiling")),
            "{err}"
        );

        members.truncate(MAX_MINT_MEMBERS);
        // Poison a member that is certainly spilled: the byte-order-last id
        // that is not the minter.
        let victim = (1..members.len())
            .max_by_key(|&i| members[i].device_id)
            .unwrap();
        members[victim].xwing_pubkey = vec![0xFFu8; fauna_pq_kem::XWING_ENCAPS_KEY_LEN];
        assert!(matches!(
            build_mint(
                &members,
                &target,
                ESCROW_TARGET_IDENTITY_KEY,
                vec![],
                &minter_key,
                1
            ),
            Err(WrapError::InvalidInput(_))
        ));
    }
}
