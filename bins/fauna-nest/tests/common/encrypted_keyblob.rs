//! Helpers for the fauna.subscriptions.* WS-RPC test matrix.
//!
//! Spec: the encrypted-mode broadcast-keyblob upload/accept design (tracked
//! internally).

use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::crypto::{MLKEM768_ENCAPS_KEY_LEN, mint_key_blob};
use fauna_core::subscription::types::KeyBlob;
use fauna_protocol::subscriptions::EncryptedKeyBlobUpload;

/// Bundled `(DeviceAuthorization, bytes, envelope)` for the embed-as-bytes
/// wire callers.
pub struct SignedAuth {
    pub auth: DeviceAuthorization,
    pub bytes: Vec<u8>,
    pub envelope: fauna_cbor::SignedEnvelope,
}

impl SignedAuth {
    pub fn wire(&self) -> EmbedAsBytes {
        // Delegate to the canonical constructor rather than hand-building the
        // 100-byte envelope: it is the same layout, and going through it keeps
        // this fixture immune to additive `EmbedAsBytes` fields (the
        // `signer_auth` delegated-authoring field added broke
        // the hand-listed literal that used to live here). Supersedes the
        // minimal `signer_auth: None` repair that landed concurrently on main —
        // same behavior, without the duplicated layout logic that keeps breaking.
        EmbedAsBytes::from_signed(self.bytes.clone(), self.envelope)
    }
}

/// Build + sign a DeviceAuthorization. Self-signed if `author_kp == device_kp`.
pub fn make_device_authorization(
    author_kp: &ActorKeypair,
    device_kp: &ActorKeypair,
    capabilities: Vec<Capability>,
) -> SignedAuth {
    let auth = DeviceAuthorization {
        actor_id: author_kp.actor_id(),
        device_key: device_kp.actor_id().0,
        capabilities,
        created_at: Timestamp::now(),
        expires_at: None,
    };
    let (bytes, envelope) = sign_envelope(author_kp, &auth).expect("sign device authorization");
    SignedAuth {
        auth,
        bytes,
        envelope,
    }
}

/// The birth `KeyBlob` envelope `fauna.subscriptions.tiers.create` requires:
/// an empty-roster blob for `tier`, self-signed by `author` under a
/// `ManageSubscribers` self-delegation. `rotated_at` is the earliest instant,
/// so any later approve / rotation in the test passes the monotonic check.
pub fn birth_upload(author: &ActorKeypair, tier: &str) -> EncryptedKeyBlobUpload {
    let auth = make_device_authorization(author, author, vec![Capability::ManageSubscribers]);
    mint_test_key_blob(author, &auth, tier, Timestamp(1), &[], &[0x42; 32]).1
}

/// Mint a classical KeyBlob via the shared-Rust author-side primitive and
/// package it as an EncryptedKeyBlobUpload ready for direct use as a Request
/// payload field.
pub fn mint_test_key_blob(
    signer: &ActorKeypair,
    signer_auth: &SignedAuth,
    tier: &str,
    rotated_at: Timestamp,
    subscribers: &[ActorId],
    wrapped_key: &[u8; 32],
) -> (KeyBlob, EncryptedKeyBlobUpload) {
    mint_test_key_blob_suite(
        signer,
        signer_auth,
        tier,
        rotated_at,
        subscribers,
        &[],
        wrapped_key,
    )
}

/// Mint a KeyBlob with per-subscriber suite selection (surface B, S4b):
/// `subscriber_eks` runs parallel to `subscribers` (each `Some(ek)` ⇒ that
/// entry is X-Wing, each `None` classical). The encrypted-mode author
/// mint path the client takes; the nest verifies it identically regardless of
/// suite (`verify_encrypted_upload` never inspects the suite).
#[allow(clippy::too_many_arguments)]
pub fn mint_test_key_blob_suite(
    signer: &ActorKeypair,
    signer_auth: &SignedAuth,
    tier: &str,
    rotated_at: Timestamp,
    subscribers: &[ActorId],
    subscriber_eks: &[Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>],
    wrapped_key: &[u8; 32],
) -> (KeyBlob, EncryptedKeyBlobUpload) {
    let minted = mint_key_blob(
        signer,
        &signer_auth.auth,
        &signer_auth.bytes,
        &signer_auth.envelope,
        tier.to_string(),
        rotated_at,
        subscribers,
        subscriber_eks,
        wrapped_key,
    )
    .expect("mint_key_blob");
    let upload = EncryptedKeyBlobUpload {
        extra: Default::default(),
        key_blob: EmbedAsBytes::from_signed(minted.bytes.clone(), minted.envelope),
        signer_auth: signer_auth.wire(),
    };
    (minted.blob, upload)
}
