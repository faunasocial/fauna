//! The RecoveryKey's escrow keypair must be usable by the HPKE suite the
//! seed-escrow blob is sealed under — `docs/goal/behavior/identity-succession.md`
//! § Seed escrow.
//!
//! This lives in `fauna-mls` rather than beside the primitives because it is the
//! only crate holding *both* halves: `fauna_core::recovery` derives the keypair,
//! and `wrapped_blob::envelope` owns the DHKEM(X25519, HKDF-SHA-256) /
//! HKDF-SHA-256 / ChaCha20-Poly1305 suite. `fauna-core` deliberately carries no
//! `hpke` dependency (the escrow *plane* is slice 2's; only the derivation is
//! slice 1's), so without this test the two conventions could silently disagree
//! and the mismatch would not surface until a real recovery kit failed to open a
//! real escrow blob — the worst possible place to discover it.
//!
//! What is pinned here is exactly the **key convention**: that
//! `escrow_public()` is the public key HPKE computes for `escrow_secret()`.
//! Slice 2 defined the real `(kind = "seed-escrow", actor_id)` binding
//! (`AadBinding::for_seed_escrow`), so this test now uses it rather than the
//! stand-in it opened with — the seal here is byte-for-byte the one the escrow
//! plane performs.

use fauna_core::recovery::RecoveryKey;
use fauna_mls::wrapped_blob::envelope::{hpke_open, hpke_seal};
use fauna_mls::wrapped_blob::format::{AadBinding, SeedEscrowBlob};
use fauna_mls::wrapped_blob::{
    PredecessorSeed, PredecessorsOutcome, seal_seed_escrow, seal_seed_escrow_with_predecessors,
    unseal_seed_escrow, unseal_seed_escrow_with_predecessors,
};

#[test]
fn the_identity_seed_seals_to_the_escrow_public_and_opens_with_the_escrow_secret() {
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let actor_id = [0x42u8; 32];
    let info = AadBinding::for_seed_escrow(&actor_id);
    let aad = AadBinding::for_seed_escrow(&actor_id);

    // The plaintext an escrow blob actually carries: the 32-byte identity seed.
    let identity_seed = [0xABu8; 32];

    let (enc, ciphertext) = hpke_seal(&recovery.escrow_public(), &info, &aad, &identity_seed)
        .expect("seal the identity seed to the escrow public key");

    let opened = hpke_open(&recovery.escrow_secret(), &info, &aad, &enc, &ciphertext)
        .expect("the escrow secret opens what the escrow public sealed");

    assert_eq!(
        opened, identity_seed,
        "a recovery-kit holder must recover the exact identity seed"
    );
}

#[test]
fn a_different_recovery_root_cannot_open_the_escrow_blob() {
    // The offline root is the only thing that opens it — the nest stores the
    // blob opaque and cannot read it (key-material rule #4).
    let owner = RecoveryKey::from_bytes([0x11; 32]);
    let other = RecoveryKey::from_bytes([0x12; 32]);
    let actor_id = [0x42u8; 32];
    let info = AadBinding::for_seed_escrow(&actor_id);
    let aad = AadBinding::for_seed_escrow(&actor_id);

    let (enc, ciphertext) =
        hpke_seal(&owner.escrow_public(), &info, &aad, &[0xABu8; 32]).expect("seal");

    assert!(
        hpke_open(&other.escrow_secret(), &info, &aad, &enc, &ciphertext).is_err(),
        "only the registered recovery root may unseal the escrow blob"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The container the escrow plane actually stores
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_escrow_container_round_trips_through_the_bytes_the_nest_stores() {
    // The full client-side path: seal → canonical bytes (what
    // `fauna.recovery.escrow.put` carries and the nest holds opaque) → decode →
    // unseal with nothing but the phrase.
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let actor_id = [0x42u8; 32];
    let identity_seed = [0xABu8; 32];

    let stored = seal_seed_escrow(&identity_seed, &actor_id, &recovery.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");

    let recovered = unseal_seed_escrow(
        &SeedEscrowBlob::from_canonical_bytes(&stored).expect("decode"),
        &recovery.escrow_secret(),
    )
    .expect("the phrase alone reopens the blob");

    assert_eq!(*recovered, identity_seed);
}

#[test]
fn an_escrow_blob_cannot_be_replayed_under_another_actor_id() {
    // The AAD binding is what stops a blob lifted from one account's row being
    // served as another's — even to a holder of *that* row's recovery root.
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let mut blob = seal_seed_escrow(&[0xABu8; 32], &[0x42u8; 32], &recovery.escrow_public())
        .expect("seal for the real actor");

    // Re-index the container at another account, exactly as a hostile nest
    // serving the wrong row would.
    blob.index.0 = serde_bytes::ByteBuf::from([0x43u8; 32].to_vec());

    assert!(
        unseal_seed_escrow(&blob, &recovery.escrow_secret()).is_err(),
        "an escrow blob is bound to the account it was sealed for"
    );
}

#[test]
fn a_foreign_kind_tag_is_refused_before_any_open_is_attempted() {
    // `seed-escrow` and `mls-snapshot` are indexed identically by `(actor_id,)`,
    // so the kind tag is their only separator — the container checks it rather
    // than letting a mis-routed blob fail as an opaque AEAD error.
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let mut blob =
        seal_seed_escrow(&[0xABu8; 32], &[0x42u8; 32], &recovery.escrow_public()).expect("seal");
    blob.kind = "mls-snapshot".into();

    assert!(unseal_seed_escrow(&blob, &recovery.escrow_secret()).is_err());
    let bytes = blob.to_canonical_bytes().expect("encode");
    assert!(SeedEscrowBlob::from_canonical_bytes(&bytes).is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// The predecessor section (identity-succession.md § Seed escrow: the
// successor's blob carries the predecessor seed(s) until the corpus re-seal
// completes — additively, so an old client's restore still recovers the
// account and merely misses the predecessor corpus)
// ─────────────────────────────────────────────────────────────────────────────

/// The container as a decoder that knows only the four original
/// fields decodes it — those fields and nothing else. Serde ignores unknown map keys by default, which
/// is exactly the additive-evolution property the ratified degradation relies
/// on; this mirror pins it against a future `deny_unknown_fields` regression.
#[derive(serde::Deserialize)]
struct LegacySeedEscrowBlob {
    #[serde(rename = "v")]
    _version: u8,
    #[serde(rename = "kind")]
    kind: String,
    #[serde(rename = "ix")]
    _index: serde_bytes::ByteBuf,
    #[serde(rename = "hpke")]
    _hpke: fauna_mls::wrapped_blob::format::HpkeWire,
}

#[test]
fn the_successors_blob_carries_the_predecessor_seed_and_degrades_additively() {
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let successor_actor = [0x42u8; 32];
    let new_seed = [0xABu8; 32];
    let pred = PredecessorSeed {
        actor_id: [0xA1u8; 32],
        seed: [0xCDu8; 32],
    };

    let stored = seal_seed_escrow_with_predecessors(
        &new_seed,
        &successor_actor,
        &recovery.escrow_public(),
        &[pred],
    )
    .expect("seal")
    .to_canonical_bytes()
    .expect("encode");

    // A current client recovers both halves from the stored bytes.
    let blob = SeedEscrowBlob::from_canonical_bytes(&stored).expect("decode");
    let opened = unseal_seed_escrow_with_predecessors(&blob, &recovery.escrow_secret())
        .expect("the phrase reopens the blob");
    assert_eq!(*opened.seed, new_seed);
    let PredecessorsOutcome::Opened(preds) = &opened.predecessors else {
        panic!("the predecessor section must open");
    };
    assert_eq!(preds.len(), 1);
    assert_eq!(preds[0].actor_id, [0xA1u8; 32]);
    assert_eq!(*preds[0].seed, [0xCDu8; 32]);

    // An old client — the primary-only path over a legacy-shape decode —
    // still recovers the account from the very same stored bytes.
    let legacy: LegacySeedEscrowBlob =
        fauna_cbor::decode_strict(&stored).expect("an old decoder tolerates the new field");
    assert_eq!(legacy.kind, "seed-escrow");
    assert_eq!(
        *unseal_seed_escrow(&blob, &recovery.escrow_secret())
            .expect("the primary-only path ignores the predecessor section"),
        new_seed
    );
}

#[test]
fn a_blob_sealed_without_predecessors_encodes_no_predecessor_key() {
    // The absence guarantee: a pre-succession kit's blob stays byte-shaped
    // exactly as before the field existed (skip_serializing_if), so nothing
    // about ordinary accounts changes on the wire or at rest.
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let stored = seal_seed_escrow(&[0xABu8; 32], &[0x42u8; 32], &recovery.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");

    let value: fauna_cbor::Value = fauna_cbor::decode_strict(&stored).expect("decode");
    let fauna_cbor::Value::Map(entries) = value else {
        panic!("the container is a map");
    };
    assert!(
        !entries.contains_key("pred"),
        "an absent predecessor section must not appear in the encoding"
    );

    let blob = SeedEscrowBlob::from_canonical_bytes(&stored).expect("decode");
    let opened =
        unseal_seed_escrow_with_predecessors(&blob, &recovery.escrow_secret()).expect("open");
    assert!(matches!(opened.predecessors, PredecessorsOutcome::Absent));
}

#[test]
fn a_tampered_predecessor_section_is_reported_without_costing_the_account() {
    // The auxiliary section must never take the primary down with it — a
    // hostile nest that corrupts `pred` must not be able to turn a working
    // account restore into a failure (the same never-let-the-auxiliary-
    // destroy-the-primary rule the lost-reply fix ratified). But the tamper
    // is REPORTED, never silently dropped.
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let mut blob = seal_seed_escrow_with_predecessors(
        &[0xABu8; 32],
        &[0x42u8; 32],
        &recovery.escrow_public(),
        &[PredecessorSeed {
            actor_id: [0xA1u8; 32],
            seed: [0xCDu8; 32],
        }],
    )
    .expect("seal");
    let mut tampered = blob.pred.take().expect("pred present");
    tampered.ciphertext[0] ^= 0x01;
    blob.pred = Some(tampered);

    let opened =
        unseal_seed_escrow_with_predecessors(&blob, &recovery.escrow_secret()).expect("open");
    assert_eq!(*opened.seed, [0xABu8; 32]);
    assert!(matches!(
        opened.predecessors,
        PredecessorsOutcome::Unreadable(_)
    ));
}

#[test]
fn a_predecessor_section_is_bound_to_the_successors_account() {
    // Same replay rule as the primary: a section lifted from one account's
    // blob does not open under another's, even for the right recovery root.
    let recovery = RecoveryKey::from_bytes([0x11; 32]);
    let donor = seal_seed_escrow_with_predecessors(
        &[0xABu8; 32],
        &[0x42u8; 32],
        &recovery.escrow_public(),
        &[PredecessorSeed {
            actor_id: [0xA1u8; 32],
            seed: [0xCDu8; 32],
        }],
    )
    .expect("seal donor");
    let mut victim =
        seal_seed_escrow(&[0xEFu8; 32], &[0x43u8; 32], &recovery.escrow_public()).expect("seal");
    victim.pred = donor.pred.clone();

    let opened =
        unseal_seed_escrow_with_predecessors(&victim, &recovery.escrow_secret()).expect("open");
    assert!(matches!(
        opened.predecessors,
        PredecessorsOutcome::Unreadable(_)
    ));
}
