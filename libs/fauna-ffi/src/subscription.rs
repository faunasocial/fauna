//! UniFFI bindings for the encrypted-mode broadcast-`KeyBlob` author-side mint
//! primitive (`fauna_core::subscription::crypto::mint_key_blob`).
//!
//! In encrypted storage mode the nest holds no period key, so the author's
//! client mints + signs the broadcast `KeyBlob` covering the post-mutation
//! subscriber roster and uploads it via `fauna.subscriptions.requests.approve`
//! / `fauna.subscriptions.subscribers.remove`. This module is the per-platform
//! entry point (Apple/Android/Windows/Linux); the WASM twin for the web app
//! lives in `libs/fauna-wasm/src/subscription.rs`.
//!
//! Mirrors the crypto-binding conventions in `mail.rs` / `wrapped_blob.rs`:
//! `FfiError` taxonomy, `Vec<u8>` byte marshalling, fixed-length validation.
//!
//! Design tracked internally.

use crate::{FfiError, bytes_to_actor_id};
use fauna_core::subscription::crypto::{MLKEM768_ENCAPS_KEY_LEN, mint_key_blob_from_bytes};

/// A signed payload in the embed-as-bytes wire shape — the canonical bytes the
/// publisher signed plus the 100-byte envelope (36-byte CID || 64-byte Ed25519
/// signature). Mirrors `fauna_core::encoding::EmbedAsBytes`.
///
/// Used both as the `signer_auth` **input** (the author's `DeviceAuthorization`
/// the client already holds in this shape) and as the minted-`KeyBlob`
/// **output** that drops straight into `EncryptedKeyBlobUpload.key_blob`.
#[derive(uniffi::Record, Clone)]
pub struct FfiEmbedAsBytes {
    /// 100 bytes: 36-byte CID || 64-byte Ed25519 signature.
    pub envelope: Vec<u8>,
    /// Canonical-encoded (dag-cbor, sign-over-CID) bytes of the inner kind.
    pub bytes: Vec<u8>,
}

/// Mint and sign a broadcast `KeyBlob` author-side, covering `subscribers`.
///
/// Returns the minted blob in the embed-as-bytes wire shape, ready to drop into
/// the `key_blob` field of the `fauna.subscriptions.*` encrypted-upload
/// envelope. The nest verifies it via `into_signed()` →
/// `decode_signed_bytes::<KeyBlob>` → `verify_key_blob_signature` (see
/// `bins/fauna-nest/src/subscription_handlers.rs` `verify_encrypted_upload`).
///
/// - `signer_secret`: 32-byte Ed25519 seed of the signing device.
/// - `signer_auth`: the author's signed `DeviceAuthorization` (carrying
///   `ManageSubscribers` or `All`) in the embed-as-bytes shape; re-verified
///   before minting.
/// - `subscribers`: the post-mutation roster, each entry a 32-byte `ActorId`.
/// - `subscriber_eks`: the per-subscriber published ML-KEM-768 encapsulation
///   keys (post-quantum surface B, slice S4b), parallel to `subscribers` — entry
///   `i` is subscriber `i`'s 1184-byte ek, or an **empty** `Vec` if they
///   published none. An X-Wing entry is minted only when the subscriber
///   published an ek (else classical — a mixed roster mints
///   per-entry). Pass all-empty (or a shorter/empty outer vec) for an
///   all-classical mint.
/// - `wrapped_key`: 32-byte broadcast period key (or the final MLS epoch secret
///   for an archival blob).
/// - `rotated_at`: microseconds since the Unix epoch.
#[allow(clippy::too_many_arguments)]
#[uniffi::export]
pub fn mint_key_blob(
    signer_secret: Vec<u8>,
    signer_auth: FfiEmbedAsBytes,
    tier: String,
    rotated_at: u64,
    subscribers: Vec<Vec<u8>>,
    subscriber_eks: Vec<Vec<u8>>,
    wrapped_key: Vec<u8>,
) -> Result<FfiEmbedAsBytes, FfiError> {
    // Platform-specific roster marshalling: UniFFI hands each subscriber as its
    // own 32-byte `Vec<u8>`, and each published ek as its own `Vec<u8>` (1184 B,
    // or empty for "no ek"). The shared core does the rest of the parse + mint.
    let subscriber_ids = subscribers
        .iter()
        .map(|b| bytes_to_actor_id(b))
        .collect::<Result<Vec<_>, _>>()?;
    let eks = subscriber_eks
        .iter()
        .map(|b| parse_optional_ek(b))
        .collect::<Result<Vec<_>, _>>()?;

    let (envelope, bytes) = mint_key_blob_from_bytes(
        &signer_secret,
        &signer_auth.envelope,
        &signer_auth.bytes,
        tier,
        rotated_at,
        &subscriber_ids,
        &eks,
        &wrapped_key,
    )
    .map_err(|msg| FfiError::General { msg })?;

    Ok(FfiEmbedAsBytes { envelope, bytes })
}

/// Parse one UniFFI roster ek slot into an `Option<[u8; 1184]>`: an empty `Vec`
/// is "no published ek" (`None`); a 1184-byte `Vec` is the published ek; any
/// other length is a marshalling error.
fn parse_optional_ek(b: &[u8]) -> Result<Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>, FfiError> {
    if b.is_empty() {
        return Ok(None);
    }
    let arr: [u8; MLKEM768_ENCAPS_KEY_LEN] = b.try_into().map_err(|_| FfiError::General {
        msg: format!(
            "subscriber ek must be {MLKEM768_ENCAPS_KEY_LEN} bytes (or empty), got {}",
            b.len()
        ),
    })?;
    Ok(Some(arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_optional_ek_empty_is_none() {
        assert!(parse_optional_ek(&[]).unwrap().is_none());
    }

    #[test]
    fn parse_optional_ek_valid_length_is_some() {
        let bytes = vec![7u8; MLKEM768_ENCAPS_KEY_LEN];
        let parsed = parse_optional_ek(&bytes).unwrap().unwrap();
        assert_eq!(parsed.as_slice(), bytes.as_slice());
    }

    #[test]
    fn parse_optional_ek_wrong_length_errors() {
        let bytes = vec![7u8; MLKEM768_ENCAPS_KEY_LEN - 1];
        assert!(parse_optional_ek(&bytes).is_err());
    }
}
