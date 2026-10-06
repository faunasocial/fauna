//! WASM binding for the encrypted-mode broadcast-`KeyBlob` author-side mint
//! primitive (`fauna_core::subscription::crypto::mint_key_blob`). Mirrors the
//! `#[uniffi::export]` surface in `libs/fauna-ffi/src/subscription.rs`; the
//! Svelte SPA on `apps/fauna-web/` calls this to mint the broadcast `KeyBlob`
//! and upload it through `fauna.subscriptions.*` WS-RPC in encrypted mode.
//!
//! In encrypted storage mode the nest holds no period key, so the author's
//! client mints + signs the `KeyBlob` covering the post-mutation subscriber
//! roster. The returned `(envelope, bytes)` pair is the embed-as-bytes wire
//! shape that drops straight into `EncryptedKeyBlobUpload.key_blob`; the nest
//! verifies it via `into_signed()` → `decode_signed_bytes::<KeyBlob>` →
//! `verify_key_blob_signature` (see
//! `bins/fauna-nest/src/subscription_handlers.rs` `verify_encrypted_upload`).
//!
//! Spec tracked internally.

use fauna_core::identity::ActorId;
use fauna_core::subscription::crypto::{MLKEM768_ENCAPS_KEY_LEN, mint_key_blob_from_bytes};
use wasm_bindgen::prelude::*;

/// A minted, signed broadcast `KeyBlob` in the embed-as-bytes wire shape.
/// `envelope` is 100 bytes (36-byte CID || 64-byte Ed25519 signature);
/// `bytes` is the canonical dag-cbor of the `KeyBlob`. Both surface to JS as
/// `Uint8Array`. Together they are exactly the `EncryptedKeyBlobUpload.key_blob`
/// field value.
#[wasm_bindgen]
pub struct MintedKeyBlob {
    envelope: Vec<u8>,
    bytes: Vec<u8>,
}

#[wasm_bindgen]
impl MintedKeyBlob {
    #[wasm_bindgen(getter)]
    pub fn envelope(&self) -> Vec<u8> {
        self.envelope.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// Mint and sign a broadcast `KeyBlob` author-side, covering `subscribers`.
///
/// - `signer_secret`: 32-byte Ed25519 seed of the signing device.
/// - `signer_auth_envelope` / `signer_auth_bytes`: the author's signed
///   `DeviceAuthorization` (carrying `ManageSubscribers` or `All`) as the
///   embed-as-bytes pair the client already holds; re-verified before minting.
/// - `tier`: the tier name the blob is for.
/// - `rotated_at`: microseconds since the Unix epoch.
/// - `subscribers`: the post-mutation roster as `32·N` concatenated `ActorId`
///   bytes.
/// - `subscriber_eks`: the per-subscriber published ML-KEM-768 encapsulation
///   keys (post-quantum surface B, slice S4b) as `1184·N` flat bytes parallel to
///   `subscribers` (slot `i` is subscriber `i`'s ek). An **all-zero** slot means
///   "no published ek" (a real ML-KEM ek is never all-zero), so the web app
///   zero-fills classical subscribers' slots. Pass **empty** for an
///   all-classical roster. An X-Wing entry is minted only when the slot is
///   non-zero (else classical — mixed rosters mint
///   per-entry).
/// - `wrapped_key`: 32-byte broadcast period key (or final MLS epoch secret for
///   an archival blob).
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn mint_key_blob(
    signer_secret: &[u8],
    signer_auth_envelope: &[u8],
    signer_auth_bytes: &[u8],
    tier: &str,
    rotated_at: u64,
    subscribers: &[u8],
    subscriber_eks: &[u8],
    wrapped_key: &[u8],
) -> Result<MintedKeyBlob, JsValue> {
    let (envelope, bytes) = mint_key_blob_inner(
        signer_secret,
        signer_auth_envelope,
        signer_auth_bytes,
        tier,
        rotated_at,
        subscribers,
        subscriber_eks,
        wrapped_key,
    )
    .map_err(|e| JsValue::from_str(&e))?;
    Ok(MintedKeyBlob { envelope, bytes })
}

/// Host-testable core of [`mint_key_blob`]. Returns the minted blob's
/// `(envelope, bytes)` embed-as-bytes pair, or a human-readable error string.
#[allow(clippy::too_many_arguments)]
pub fn mint_key_blob_inner(
    signer_secret: &[u8],
    signer_auth_envelope: &[u8],
    signer_auth_bytes: &[u8],
    tier: &str,
    rotated_at: u64,
    subscribers: &[u8],
    subscriber_eks: &[u8],
    wrapped_key: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), String> {
    // Platform-specific roster marshalling: the web app packs subscribers as
    // `32·N` flat bytes (one `ActorId` per 32-byte chunk). The shared core does
    // the rest of the parse + mint.
    if !subscribers.len().is_multiple_of(32) {
        return Err(format!(
            "subscribers must be a multiple of 32 bytes (one ActorId each), got {}",
            subscribers.len()
        ));
    }
    let subscriber_ids: Vec<ActorId> = subscribers
        .chunks_exact(32)
        .map(|c| {
            let mut a = [0u8; 32];
            a.copy_from_slice(c);
            ActorId(a)
        })
        .collect();

    // Per-subscriber eks: empty ⇒ all classical; else `1184·N` parallel to the
    // roster, an all-zero slot meaning "no ek" for that subscriber.
    let eks: Vec<Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>> = if subscriber_eks.is_empty() {
        Vec::new()
    } else {
        if subscriber_eks.len() != MLKEM768_ENCAPS_KEY_LEN * subscriber_ids.len() {
            return Err(format!(
                "subscriber_eks must be empty or {} bytes ({MLKEM768_ENCAPS_KEY_LEN}·{} subscribers), got {}",
                MLKEM768_ENCAPS_KEY_LEN * subscriber_ids.len(),
                subscriber_ids.len(),
                subscriber_eks.len()
            ));
        }
        subscriber_eks
            .chunks_exact(MLKEM768_ENCAPS_KEY_LEN)
            .map(|c| {
                if c.iter().all(|&b| b == 0) {
                    None
                } else {
                    let mut a = [0u8; MLKEM768_ENCAPS_KEY_LEN];
                    a.copy_from_slice(c);
                    Some(a)
                }
            })
            .collect()
    };

    mint_key_blob_from_bytes(
        signer_secret,
        signer_auth_envelope,
        signer_auth_bytes,
        tier.to_string(),
        rotated_at,
        &subscriber_ids,
        &eks,
        wrapped_key,
    )
}
