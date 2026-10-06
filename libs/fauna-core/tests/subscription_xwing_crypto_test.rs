//! S4 (post-quantum surface B) — subscription `KeyBlob` X-Wing hybrid wrap.
//!
//! These pin the shared-Rust core: the subscriber's identity-seed-derived
//! X-Wing keypair, the author-side `create_key_blob_entry_xwing` wrap, and the
//! suite-dispatching unwrap. Goal doc
//! `docs/goal/architecture/security/post-quantum.md` § Post-quantum key
//! publication and derivation (subscriptions).

use fauna_core::identity::ActorKeypair;
use fauna_core::subscription::crypto::{
    KeyBlobWrapError, create_key_blob_entry, create_key_blob_entry_auto,
    create_key_blob_entry_xwing, decrypt_key_blob_entry_for, derive_subscriber_xwing_keypair,
    subscriber_mlkem_encaps_key,
};
use fauna_core::subscription::types::KemSuiteId;

/// The load-bearing correctness invariant: the X25519 half of a subscriber's
/// derived X-Wing public key MUST equal the X25519 public derived from their
/// public ActorId (Ed25519 → Montgomery). The author reconstructs the X-Wing
/// public as `from_parts(published_ek, actor_id.to_x25519_public())`, so if
/// these halves disagree every X-Wing unwrap fails.
#[test]
fn subscriber_xwing_public_x25519_half_matches_actor_id() {
    let kp = ActorKeypair::generate();
    let xwing = derive_subscriber_xwing_keypair(&kp);
    assert_eq!(
        xwing.public.x25519_public(),
        kp.actor_id().to_x25519_public().as_bytes(),
        "X-Wing X25519 half must match the ActorId-derived X25519 public",
    );
}

/// The published encapsulation key is exactly the 1184-byte ML-KEM half of the
/// subscriber's derived X-Wing keypair.
#[test]
fn published_encaps_key_matches_derived_keypair() {
    let kp = ActorKeypair::generate();
    let ek = subscriber_mlkem_encaps_key(&kp);
    assert_eq!(ek.len(), 1184);
    let xwing = derive_subscriber_xwing_keypair(&kp);
    assert_eq!(&ek, xwing.public.mlkem_encaps_key());
}

/// The derivation is deterministic in the identity seed — every device of the
/// same subscriber re-derives the identical key (fleet-consistency).
#[test]
fn subscriber_xwing_derivation_is_deterministic() {
    let secret = [7u8; 32];
    let a = subscriber_mlkem_encaps_key(&ActorKeypair::from_secret(secret));
    let b = subscriber_mlkem_encaps_key(&ActorKeypair::from_secret(secret));
    assert_eq!(a, b);
    // Distinct identities derive distinct keys.
    let other = subscriber_mlkem_encaps_key(&ActorKeypair::from_secret([8u8; 32]));
    assert_ne!(a, other);
}

/// Author wraps the period key to the subscriber's published ek; the subscriber
/// unwraps it via the suite dispatcher and recovers the exact period key. The
/// entry self-describes `suite = Xwing` and the framing is
/// `[1120 X-Wing ct][12 nonce][ct][16 tag]`.
#[test]
fn xwing_key_blob_entry_round_trips() {
    let subscriber = ActorKeypair::generate();
    let period_key = [42u8; 32];
    let ek = subscriber_mlkem_encaps_key(&subscriber);

    let entry = create_key_blob_entry_xwing(&subscriber.actor_id(), &ek, &period_key)
        .expect("wrap to a valid published ek");

    assert_eq!(entry.suite, KemSuiteId::Xwing);
    assert_eq!(entry.subscriber, subscriber.actor_id());
    // 1120 X-Wing ciphertext + 12 nonce + 32 plaintext + 16 tag.
    assert_eq!(entry.encrypted_key.len(), 1120 + 12 + 32 + 16);

    let recovered = decrypt_key_blob_entry_for(&subscriber, &entry).expect("unwrap");
    assert_eq!(recovered, period_key);
}

/// The classical path is unchanged and the dispatcher routes it correctly — a
/// classical entry round-trips byte-for-byte as before, suite = Classical.
#[test]
fn classical_key_blob_entry_still_round_trips_via_dispatcher() {
    let subscriber = ActorKeypair::generate();
    let period_key = [99u8; 32];

    let entry = create_key_blob_entry(&subscriber.actor_id(), &period_key);
    assert_eq!(entry.suite, KemSuiteId::Classical);
    // Classical framing: 32 ephemeral pubkey + 12 nonce + 32 plaintext + 16 tag.
    assert_eq!(entry.encrypted_key.len(), 32 + 12 + 32 + 16);

    let recovered = decrypt_key_blob_entry_for(&subscriber, &entry).expect("unwrap");
    assert_eq!(recovered, period_key);
}

/// A different subscriber cannot unwrap an X-Wing entry (ML-KEM implicit
/// rejection yields a different shared secret → the AEAD rejects).
#[test]
fn xwing_unwrap_rejects_wrong_subscriber() {
    let subscriber = ActorKeypair::generate();
    let attacker = ActorKeypair::generate();
    let period_key = [1u8; 32];
    let ek = subscriber_mlkem_encaps_key(&subscriber);

    let entry =
        create_key_blob_entry_xwing(&subscriber.actor_id(), &ek, &period_key).expect("wrap");

    let result = decrypt_key_blob_entry_for(&attacker, &entry);
    assert!(
        result.is_err(),
        "wrong subscriber must not unwrap an X-Wing entry"
    );
}

/// A malformed published ek is rejected at wrap time (FIPS 203 input
/// validation) so the caller can degrade to classical (PQ-4b).
#[test]
fn xwing_wrap_rejects_malformed_encaps_key() {
    let subscriber = ActorKeypair::generate();
    let period_key = [5u8; 32];
    let bogus_ek = [0xFFu8; 1184]; // coefficients ≥ q → fails validation.

    let result = create_key_blob_entry_xwing(&subscriber.actor_id(), &bogus_ek, &period_key);
    assert!(matches!(result, Err(KeyBlobWrapError::InvalidEncapsKey)));
}

/// The auto selector picks X-Wing iff an ek is published.
#[test]
fn auto_selects_xwing_when_ek_published() {
    let subscriber = ActorKeypair::generate();
    let period_key = [11u8; 32];
    let ek = subscriber_mlkem_encaps_key(&subscriber);

    let entry = create_key_blob_entry_auto(&subscriber.actor_id(), Some(&ek), &period_key);
    assert_eq!(entry.suite, KemSuiteId::Xwing);
    assert_eq!(
        decrypt_key_blob_entry_for(&subscriber, &entry).expect("unwrap"),
        period_key
    );
}

/// No published ek ⇒ classical.
#[test]
fn auto_degrades_to_classical_without_published_ek() {
    let subscriber = ActorKeypair::generate();
    let period_key = [12u8; 32];

    let entry = create_key_blob_entry_auto(&subscriber.actor_id(), None, &period_key);
    assert_eq!(entry.suite, KemSuiteId::Classical);
    assert_eq!(
        decrypt_key_blob_entry_for(&subscriber, &entry).expect("unwrap"),
        period_key
    );
}

/// A malformed published ek ⇒ degrade to classical (PQ-4b),
/// the entry still round-trips.
#[test]
fn auto_degrades_to_classical_on_malformed_ek() {
    let subscriber = ActorKeypair::generate();
    let period_key = [14u8; 32];
    let bogus_ek = [0xFFu8; 1184];

    let entry = create_key_blob_entry_auto(&subscriber.actor_id(), Some(&bogus_ek), &period_key);
    assert_eq!(entry.suite, KemSuiteId::Classical);
    assert_eq!(
        decrypt_key_blob_entry_for(&subscriber, &entry).expect("unwrap"),
        period_key
    );
}
