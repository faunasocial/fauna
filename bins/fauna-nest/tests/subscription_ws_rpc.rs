//! WS-RPC handler tests for fauna.subscriptions.* kinds.

mod common;
use common::encrypted_keyblob::*;

use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, decode_signed_bytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_core::subscription::crypto::{decrypt_key_blob_entry_for, subscriber_mlkem_encaps_key};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob};
use fauna_nest::subscription_handlers::verify_encrypted_upload;
use fauna_protocol::subscriptions::EncryptedKeyBlobUpload;

#[test]
fn verify_upload_happy_path_self_signed() {
    let author = ActorKeypair::generate();
    let auth = make_device_authorization(&author, &author, vec![Capability::ManageSubscribers]);
    let (blob, upload) = mint_test_key_blob(
        &author,
        &auth,
        "tier1",
        Timestamp(1_700_000_000_000_000),
        &[],
        &[0u8; 32],
    );

    let result = verify_encrypted_upload(author.actor_id().0, &upload, author.actor_id().0);
    let (decoded_blob, decoded_auth) = result.expect("happy path");
    assert_eq!(decoded_blob.tier, "tier1");
    assert_eq!(decoded_auth.actor_id, blob.author);
}

#[test]
fn verify_upload_accepts_client_minted_hybrid_blob() {
    // S4b acceptance: a client-minted *hybrid* (X-Wing) KeyBlob survives the
    // nest's encrypted-upload verify identically to a classical one
    // (`verify_encrypted_upload` checks only the signature chain + author scope,
    // never the per-entry suite), and the wrapped-to subscriber opens their
    // X-Wing entry to recover the period key.
    let author = ActorKeypair::generate();
    let subscriber = ActorKeypair::generate();
    let auth = make_device_authorization(&author, &author, vec![Capability::ManageSubscribers]);
    let period_key = [0x5Au8; 32];
    let ek = subscriber_mlkem_encaps_key(&subscriber);

    let (blob, upload) = mint_test_key_blob_suite(
        &author,
        &auth,
        "tier1",
        Timestamp(1_700_000_000_000_000),
        &[subscriber.actor_id()],
        &[Some(ek)],
        &period_key,
    );

    // The nest accepts the hybrid upload.
    let (decoded_blob, _decoded_auth) =
        verify_encrypted_upload(author.actor_id().0, &upload, author.actor_id().0)
            .expect("hybrid upload accepted");

    // The minted + re-decoded entry is X-Wing and opens to the period key.
    assert_eq!(blob.entries[0].suite, KemSuiteId::Xwing);
    let stored: KeyBlob =
        decode_signed_bytes(&upload.key_blob.clone().into_signed().unwrap().0).unwrap();
    assert_eq!(stored.entries[0].suite, KemSuiteId::Xwing);
    assert_eq!(
        decrypt_key_blob_entry_for(&subscriber, &decoded_blob.entries[0]).expect("hybrid open"),
        period_key,
    );
}

#[test]
fn verify_upload_rejects_malformed_bare() {
    let author = ActorKeypair::generate();
    let result = verify_encrypted_upload(
        author.actor_id().0,
        &EncryptedKeyBlobUpload {
            extra: Default::default(),
            // 100-byte envelope of zeros — CID prefix decode fails → malformed_upload.
            key_blob: EmbedAsBytes {
                envelope: vec![0u8; 100],
                bytes: vec![0xff; 32],
                signer_auth: None,
            },
            signer_auth: EmbedAsBytes {
                envelope: vec![0u8; 100],
                bytes: vec![0xff; 32],
                signer_auth: None,
            },
        },
        author.actor_id().0,
    );
    let err = result.unwrap_err();
    assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
}

#[test]
fn verify_upload_rejects_bearer_not_in_chain() {
    let author = ActorKeypair::generate();
    let unrelated = ActorKeypair::generate();
    let auth = make_device_authorization(&author, &author, vec![Capability::ManageSubscribers]);
    let (_blob, upload) = mint_test_key_blob(
        &author,
        &auth,
        "tier1",
        Timestamp(1_700_000_000_000_000),
        &[],
        &[0u8; 32],
    );

    let result = verify_encrypted_upload(unrelated.actor_id().0, &upload, author.actor_id().0);
    let err = result.unwrap_err();
    assert_eq!(err.code, "fauna.subscriptions.permission_denied");
}

#[test]
fn verify_upload_rejects_author_scope_mismatch() {
    let author = ActorKeypair::generate();
    let other_author = ActorKeypair::generate();
    let auth = make_device_authorization(&author, &author, vec![Capability::ManageSubscribers]);
    let (_blob, upload) = mint_test_key_blob(
        &author,
        &auth,
        "tier1",
        Timestamp(1_700_000_000_000_000),
        &[],
        &[0u8; 32],
    );

    let result = verify_encrypted_upload(author.actor_id().0, &upload, other_author.actor_id().0);
    let err = result.unwrap_err();
    assert_eq!(err.code, "fauna.subscriptions.permission_denied");
}

#[test]
fn verify_upload_rejects_missing_capability() {
    // Forge a blob+auth pair where the auth lacks ManageSubscribers.
    // Bypass mint_key_blob (which would reject the auth) by assembling manually.
    let author = ActorKeypair::generate();

    let bad_auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: author.actor_id().0,
        capabilities: vec![],
        created_at: Timestamp(1_700_000_000_000_000),
        expires_at: None,
    };
    let (bad_auth_bytes, bad_auth_env) = sign_envelope(&author, &bad_auth).unwrap();

    let blob = KeyBlob {
        author: author.actor_id(),
        tier: "tier1".to_string(),
        rotated_at: Timestamp(1_700_000_000_000_000),
        entries: vec![],
        signer: author.actor_id().0,
        key_commitment: [0x6b; 32],
    };
    let (blob_bytes, blob_env) = sign_envelope(&author, &blob).unwrap();

    let upload = EncryptedKeyBlobUpload {
        extra: Default::default(),
        key_blob: EmbedAsBytes::from_signed(blob_bytes, blob_env),
        signer_auth: EmbedAsBytes::from_signed(bad_auth_bytes, bad_auth_env),
    };

    let result = verify_encrypted_upload(author.actor_id().0, &upload, author.actor_id().0);
    let err = result.unwrap_err();
    assert_eq!(err.code, "fauna.subscriptions.invalid_signature");
}
