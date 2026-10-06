//! FFI-level tests for the encrypted-mode broadcast-`KeyBlob` mint binding
//! (`fauna_ffi::mint_key_blob`). Drives the author-side path the
//! linux/windows/apple/android subscriptions sessions will call, then asserts
//! the returned embed-as-bytes wire shape satisfies the nest consumer —
//! `EmbedAsBytes::into_signed` → `decode_signed_bytes::<KeyBlob>` →
//! `verify_key_blob_signature` — exactly as
//! `bins/fauna-nest/src/subscription_handlers.rs::verify_encrypted_upload`
//! does, and that each subscriber can unwrap their entry.

use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, decode_signed_bytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_core::subscription::crypto::{
    decrypt_key_blob_entry, decrypt_key_blob_entry_for, subscriber_mlkem_encaps_key,
    verify_key_blob_signature,
};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob};
use fauna_ffi::{FfiEmbedAsBytes, mint_key_blob};

/// Build a signed `DeviceAuthorization` (author signs, granting `caps` to
/// `device`) and return it in the FFI embed-as-bytes shape — what the client
/// holds and passes as `signer_auth`.
fn signed_auth_embed(
    author: &ActorKeypair,
    device: &ActorKeypair,
    caps: Vec<Capability>,
) -> FfiEmbedAsBytes {
    let auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: device.actor_id().0,
        capabilities: caps,
        created_at: Timestamp(1_700_000_000_000_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(author, &auth).expect("sign auth");
    let wire = EmbedAsBytes::from_signed(bytes, env);
    FfiEmbedAsBytes {
        envelope: wire.envelope,
        bytes: wire.bytes,
    }
}

fn secret_bytes(kp: &ActorKeypair) -> Vec<u8> {
    kp.signing_key().to_bytes().to_vec()
}

/// Replay the nest's `verify_encrypted_upload` decode + verify chain against a
/// minted `key_blob` embed and its `signer_auth` embed; return the decoded
/// `KeyBlob` so callers can inspect entries.
fn verify_as_nest_would(minted: FfiEmbedAsBytes, auth: FfiEmbedAsBytes) -> KeyBlob {
    let (blob_bytes, blob_env) = EmbedAsBytes {
        envelope: minted.envelope,
        bytes: minted.bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("minted envelope splits");
    let blob: KeyBlob = decode_signed_bytes(&blob_bytes).expect("decode KeyBlob (dag-cbor)");

    let (auth_bytes, auth_env) = EmbedAsBytes {
        envelope: auth.envelope,
        bytes: auth.bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("auth envelope splits");
    let device_auth: DeviceAuthorization =
        decode_signed_bytes(&auth_bytes).expect("decode DeviceAuthorization");

    assert!(
        verify_key_blob_signature(
            &blob,
            &blob_bytes,
            &blob_env,
            &device_auth,
            &auth_bytes,
            &auth_env,
        )
        .expect("verify chain runs"),
        "minted blob must verify under the same auth the nest checks",
    );
    blob
}

#[test]
fn mint_key_blob_ffi_roundtrip_delegated_device() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let auth = signed_auth_embed(&author, &device, vec![Capability::ManageSubscribers]);

    let subscribers: Vec<ActorKeypair> = (0..3).map(|_| ActorKeypair::generate()).collect();
    let roster: Vec<Vec<u8>> = subscribers
        .iter()
        .map(|s| s.actor_id().0.to_vec())
        .collect();
    let period_key = vec![0x33u8; 32];

    let minted = mint_key_blob(
        secret_bytes(&device),
        auth.clone(),
        "Pro".to_string(),
        2_000_000,
        roster,
        vec![],
        period_key.clone(),
    )
    .expect("mint via FFI");

    let blob = verify_as_nest_would(minted, auth);
    assert_eq!(blob.author, author.actor_id());
    assert_eq!(blob.signer, device.actor_id().0);
    assert_eq!(blob.tier, "Pro");
    assert_eq!(blob.entries.len(), 3);
    for (i, sub) in subscribers.iter().enumerate() {
        let recovered = decrypt_key_blob_entry(sub, &blob.entries[i].encrypted_key).unwrap();
        assert_eq!(recovered.to_vec(), period_key);
    }
}

#[test]
fn mint_key_blob_ffi_self_signed_author() {
    // Author and signing device are the same keypair (single-device author).
    let author = ActorKeypair::generate();
    let auth = signed_auth_embed(&author, &author, vec![Capability::All]);

    let subscriber = ActorKeypair::generate();
    let period_key = vec![0x44u8; 32];

    let minted = mint_key_blob(
        secret_bytes(&author),
        auth.clone(),
        "Followers".to_string(),
        3_000_000,
        vec![subscriber.actor_id().0.to_vec()],
        vec![],
        period_key.clone(),
    )
    .expect("self-signed mint via FFI");

    let blob = verify_as_nest_would(minted, auth);
    assert_eq!(blob.author, author.actor_id());
    assert_eq!(blob.signer, author.actor_id().0);
    let recovered = decrypt_key_blob_entry(&subscriber, &blob.entries[0].encrypted_key).unwrap();
    assert_eq!(recovered.to_vec(), period_key);
}

#[test]
fn mint_key_blob_ffi_rejects_missing_capability() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    // Post grants no ManageSubscribers/All → core mint rejects.
    let auth = signed_auth_embed(&author, &device, vec![Capability::Post]);

    let result = mint_key_blob(
        secret_bytes(&device),
        auth,
        "Pro".to_string(),
        2_000_000,
        vec![ActorKeypair::generate().actor_id().0.to_vec()],
        vec![],
        vec![0x55u8; 32],
    );
    assert!(result.is_err(), "missing capability must error");
}

#[test]
fn mint_key_blob_ffi_rejects_bad_lengths() {
    let author = ActorKeypair::generate();
    let auth = signed_auth_embed(&author, &author, vec![Capability::ManageSubscribers]);
    let good_sub = author.actor_id().0.to_vec();

    // wrapped_key not 32 bytes.
    assert!(
        mint_key_blob(
            secret_bytes(&author),
            auth.clone(),
            "Pro".to_string(),
            1,
            vec![good_sub.clone()],
            vec![],
            vec![0u8; 16],
        )
        .is_err(),
        "short wrapped_key must error"
    );

    // signer_secret not 32 bytes.
    assert!(
        mint_key_blob(
            vec![0u8; 16],
            auth.clone(),
            "Pro".to_string(),
            1,
            vec![good_sub],
            vec![],
            vec![0u8; 32],
        )
        .is_err(),
        "short signer_secret must error"
    );

    // subscriber id not 32 bytes.
    assert!(
        mint_key_blob(
            secret_bytes(&author),
            auth,
            "Pro".to_string(),
            1,
            vec![vec![0u8; 31]],
            vec![],
            vec![0u8; 32],
        )
        .is_err(),
        "malformed subscriber id must error"
    );

    // subscriber ek present but wrong length (not 1184, not empty).
    assert!(
        mint_key_blob(
            secret_bytes(&author),
            signed_auth_embed(&author, &author, vec![Capability::ManageSubscribers]),
            "Pro".to_string(),
            1,
            vec![author.actor_id().0.to_vec()],
            vec![vec![0u8; 100]],
            vec![0u8; 32],
        )
        .is_err(),
        "malformed subscriber ek must error"
    );
}

#[test]
fn mint_key_blob_ffi_hybrid_mixed_roster() {
    // S4b: the UniFFI `Vec<Vec<u8>>` ek marshalling (1184-byte ek, or empty for
    // "no ek") mints a per-entry suite — X-Wing for the subscriber who published
    // an ek, classical for the empty slot — and the read dispatcher opens both.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let auth = signed_auth_embed(&author, &device, vec![Capability::ManageSubscribers]);

    let pq_sub = ActorKeypair::generate();
    let classical_sub = ActorKeypair::generate();
    let roster = vec![
        pq_sub.actor_id().0.to_vec(),
        classical_sub.actor_id().0.to_vec(),
    ];
    let eks = vec![subscriber_mlkem_encaps_key(&pq_sub).to_vec(), vec![]];
    let period_key = vec![0x66u8; 32];

    let minted = mint_key_blob(
        secret_bytes(&device),
        auth.clone(),
        "Pro".to_string(),
        2_000_000,
        roster,
        eks,
        period_key.clone(),
    )
    .expect("hybrid mint via FFI");

    let blob = verify_as_nest_would(minted, auth);
    assert_eq!(blob.entries[0].suite, KemSuiteId::Xwing);
    assert_eq!(blob.entries[1].suite, KemSuiteId::Classical);
    assert_eq!(
        decrypt_key_blob_entry_for(&pq_sub, &blob.entries[0])
            .unwrap()
            .to_vec(),
        period_key
    );
    assert_eq!(
        decrypt_key_blob_entry_for(&classical_sub, &blob.entries[1])
            .unwrap()
            .to_vec(),
        period_key
    );
}
