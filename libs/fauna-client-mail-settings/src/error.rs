//! Error taxonomy for the mail-settings state machine.

use fauna_mls::wrapped_blob::{UnwrapError, WrapError};
use thiserror::Error;

/// Returned by every `NestClient` provision/revoke method — the shared
/// [`fauna_protocol::NestSeamError`] under this crate's own name. `Transient` is
/// a WS disconnect / rate limit / 5xx; `Rejected` is a structured refusal
/// (precondition failure, unknown actor, unknown bridge pubkey, …).
///
/// ⚠ The bare name is safe *because* it never enters the flat `fauna-ffi`
/// UniFFI namespace — [`DispatchError`] is the exported one, and its
/// `flat_error` repr is why (as its own doc says) the inner seam errors need no
/// annotation. Until 2026-08-23 this was one of four hand-written copies;
/// [`fauna_protocol::NestSeamError`] carries the full account.
///
/// `fauna-client-bridges`' `DiscoverHoldersError` is now this very type, so the
/// variant-to-variant `From` impl the rotation-heal driver's
/// `content_processor_holders` seam used to need is gone.
pub use fauna_protocol::NestSeamError as NestError;

/// Map a transport error into the two-class [`NestError`] via the shared
/// [`fauna_protocol::nest_seam_error`] classifier — the generic replacement for
/// each app's hand-rolled `map_admin_err` (linux's `mail_glue.rs`). Used by
/// the shared `rpc_glue` seams (native + wasm), which differ only in transport.
pub fn nest_error<E: fauna_protocol::RpcErrorClass + core::fmt::Display>(e: E) -> NestError {
    fauna_protocol::nest_seam_error(e)
}

/// Returned by the store seams' load/save. The **shared** seam error
/// (`fauna_client_config::StoreError`) — re-exported here so `DispatchError`'s
/// `#[from]` and every `crate::error::StoreError` reference keep resolving after
/// the seam moved to `fauna-client-config` (priority #4: one shared seam).
pub use fauna_client_config::StoreError;

/// Returned by `IdentitySigner::sign_submission_token`. The per-app
/// glue layer holds the user's Ed25519 signing key and never exposes
/// it to the state machine.
#[derive(Debug, Error)]
pub enum SignerError {
    #[error("submission-token sign: {0}")]
    Sign(String),
    /// Signing a capability grant-event (`content.read{spam-model}` mint/revoke,
    /// the baseline-contribution toggle) failed. Same seam split as `Sign` — the
    /// glue holds the identity key, the machine only sees the signed event.
    #[error("grant-event sign: {0}")]
    GrantEventSign(String),
}

/// Wraps every failure mode an action dispatch can produce. The
/// per-app UI surfaces the `Display` form via the
/// `error-message` element.
///
/// `flat_error` represents it by its `Display` string at the FFI boundary, so
/// the inner `NestError` / `StoreError` / `SignerError` need no annotation.
/// Shared with `MailSettingsMachine` — the flat repr adds no constraint there.
#[derive(Debug, Error)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum DispatchError {
    #[error(transparent)]
    Nest(#[from] NestError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Signer(#[from] SignerError),
    /// Crypto failure on the client side (wrap step panicked, bad
    /// credential bytes, …). Class (1) in the plan's error-state
    /// taxonomy.
    #[error("wrap: {0}")]
    Wrap(String),
    /// Action precondition failed (e.g. AddCredential before
    /// EnableMail). Surfaced as a user-visible error.
    #[error("{0}")]
    InvalidState(String),
    /// User asked to revoke or re-wrap a credential the local
    /// mail custody doesn't know about.
    #[error("unknown credential_id: {0}")]
    UnknownCredential(String),
}

impl From<WrapError> for DispatchError {
    fn from(e: WrapError) -> Self {
        Self::Wrap(e.to_string())
    }
}

impl From<UnwrapError> for DispatchError {
    fn from(e: UnwrapError) -> Self {
        Self::Wrap(e.to_string())
    }
}

/// Decode a hex-encoded 16-byte wire id, with `label` naming the id kind in
/// the error message (`"alias id"`, `"list id"`, …). A corrupted snapshot
/// surfaces as a user-visible error rather than panicking / sending garbage.
/// Shared by every `decode_*_id` in this crate (`aliases`, `forwarders`,
/// `lists`, `export`, `spam`), which differed only in this label.
pub(crate) fn decode_hex_id16(label: &str, hex_str: &str) -> Result<Vec<u8>, DispatchError> {
    let bytes = hex::decode(hex_str.trim())
        .map_err(|e| DispatchError::Wrap(format!("{label} hex: {e}")))?;
    if bytes.len() != 16 {
        return Err(DispatchError::InvalidState(format!(
            "{label} must be 16 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_hex_id16_accepts_valid_16_byte_hex() {
        let bytes = decode_hex_id16("alias id", "00112233445566778899aabbccddeeef").unwrap();
        assert_eq!(
            bytes,
            vec![
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xef,
            ]
        );
    }

    #[test]
    fn decode_hex_id16_rejects_malformed_hex_with_the_label() {
        let err = decode_hex_id16("list id", "not-hex").unwrap_err();
        match err {
            DispatchError::Wrap(msg) => assert!(
                msg.starts_with("list id hex: "),
                "expected label-prefixed message, got {msg:?}"
            ),
            other => panic!("expected Wrap, got {other:?}"),
        }
    }

    #[test]
    fn decode_hex_id16_rejects_wrong_length_with_the_label_and_count() {
        // 4 bytes hex-encoded, not 16.
        let err = decode_hex_id16("spam history id", "00112233").unwrap_err();
        match err {
            DispatchError::InvalidState(msg) => assert_eq!(
                msg, "spam history id must be 16 bytes, got 4",
                "message must preserve the exact pre-dedup wording"
            ),
            other => panic!("expected InvalidState, got {other:?}"),
        }
    }

    /// The rotation-heal driver's `content_processor_holders` seam returns
    /// `discover_holders`' error straight through, with no conversion — sound
    /// only while the two names denote **one** type. This coerces one to the
    /// other, so re-splitting either back into a private enum is a compile
    /// error here rather than a silent re-divergence of the two `Display`
    /// spellings `fauna_protocol::NestSeamError` now owns.
    const _: fn(fauna_client_bridges::DiscoverHoldersError) -> NestError = |e| e;
}
