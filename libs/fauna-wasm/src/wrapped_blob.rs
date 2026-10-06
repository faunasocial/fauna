//! WASM bindings for the wrapped-blob seal/unseal surface
//! (I3 Phase A.2). Mirrors the `#[uniffi::export]` surface in
//! `libs/fauna-ffi/src/mail.rs`; the Svelte SPA on
//! `apps/fauna-web/` calls these to provision wrapped blobs through
//! nest's `fauna.bridges.provision_*` RPCs from the browser.
//!
//! The seal-side caller flow is identical to the per-platform
//! UniFFI flow (Swift/Kotlin/C#/Go): hand a 32-byte MSEK + 32-byte
//! actor_id + credential bytes to the seal function, get back the
//! canonical DAG-CBOR bytes ready to upload. The unseal-side
//! exports are present here for parity (and for browser-side test
//! harnesses); the production path runs unseal in the MDA bridge,
//! not the web app.
//!
//! KDF parameter override: pass `argon2_m_kib`/`argon2_t`/`argon2_p`
//! when `credential_kind == "plain"` and you need non-default
//! Argon2id cost; pass `None`/`undefined` to use the library default
//! (Argon2id Interactive: m = 65 536 KiB, t = 2, p = 1) per
//! `docs/goal/behavior/mail-credentials.md` § KDF choice. For
//! `credential_kind == "oauthbearer"`, the Argon2id parameters are
//! ignored — the KDF is always HKDF-SHA-256 (no cost parameters).

use fauna_mls::wrapped_blob::{
    Argon2idParams, CredentialInput, KdfParams, MlsSnapshotBlob, SubmissionToken, WrappedMsekBlob,
    seal_mls_snapshot, seal_submission_token, seal_wrapped_msek, unseal_mls_snapshot,
    unseal_wrapped_msek,
};
use wasm_bindgen::prelude::*;

// 8 positional args mirror the UniFFI seal surface (fauna-ffi/src/mail.rs); a
// params struct would diverge from that shape and the SPA call site.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn seal_wrapped_msek_blob(
    msek: &[u8],
    actor_id: &[u8],
    credential_id: &str,
    credential_kind: &str,
    credential_bytes: &[u8],
    argon2_m_kib: Option<u32>,
    argon2_t: Option<u32>,
    argon2_p: Option<u32>,
) -> Result<Vec<u8>, JsValue> {
    let msek = array_32(msek, "msek")?;
    let actor_id = array_32(actor_id, "actor_id")?;
    let credential = credential_input(credential_kind, credential_bytes)?;
    let kdf = resolve_kdf_params(credential_kind, argon2_m_kib, argon2_t, argon2_p)?;
    let blob = seal_wrapped_msek(&msek, &actor_id, credential_id, &credential, kdf)
        .map_err(|e| JsValue::from_str(&format!("seal wrapped-msek: {e}")))?;
    blob.to_canonical_bytes()
        .map_err(|e| JsValue::from_str(&format!("encode wrapped-msek: {e}")))
}

#[wasm_bindgen]
pub fn unseal_wrapped_msek_blob(
    blob_bytes: &[u8],
    credential_kind: &str,
    credential_bytes: &[u8],
) -> Result<Vec<u8>, JsValue> {
    let blob = WrappedMsekBlob::from_canonical_bytes(blob_bytes)
        .map_err(|e| JsValue::from_str(&format!("decode wrapped-msek: {e}")))?;
    let credential = credential_input(credential_kind, credential_bytes)?;
    let msek = unseal_wrapped_msek(&blob, &credential)
        .map_err(|e| JsValue::from_str(&format!("unseal wrapped-msek: {e}")))?;
    Ok(msek.to_vec())
}

#[wasm_bindgen]
pub fn seal_mls_snapshot_blob(
    serialized_state: &[u8],
    actor_id: &[u8],
    msek: &[u8],
) -> Result<Vec<u8>, JsValue> {
    let actor_id = array_32(actor_id, "actor_id")?;
    let msek = array_32(msek, "msek")?;
    let blob = seal_mls_snapshot(serialized_state, &actor_id, &msek)
        .map_err(|e| JsValue::from_str(&format!("seal mls-snapshot: {e}")))?;
    blob.to_canonical_bytes()
        .map_err(|e| JsValue::from_str(&format!("encode mls-snapshot: {e}")))
}

#[wasm_bindgen]
pub fn unseal_mls_snapshot_blob(blob_bytes: &[u8], msek: &[u8]) -> Result<Vec<u8>, JsValue> {
    let msek = array_32(msek, "msek")?;
    let blob = MlsSnapshotBlob::from_canonical_bytes(blob_bytes)
        .map_err(|e| JsValue::from_str(&format!("decode mls-snapshot: {e}")))?;
    let plaintext = unseal_mls_snapshot(&blob, &msek)
        .map_err(|e| JsValue::from_str(&format!("unseal mls-snapshot: {e}")))?;
    Ok(plaintext.to_vec())
}

// 8 positional args mirror the UniFFI seal surface (fauna-ffi/src/mail.rs); a
// params struct would diverge from that shape and the SPA call site.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn seal_submission_token_blob(
    token_canonical_bytes: &[u8],
    actor_id: &[u8],
    credential_id: &str,
    credential_kind: &str,
    credential_bytes: &[u8],
    argon2_m_kib: Option<u32>,
    argon2_t: Option<u32>,
    argon2_p: Option<u32>,
) -> Result<Vec<u8>, JsValue> {
    let actor_id = array_32(actor_id, "actor_id")?;
    let credential = credential_input(credential_kind, credential_bytes)?;
    let kdf = resolve_kdf_params(credential_kind, argon2_m_kib, argon2_t, argon2_p)?;
    let token = SubmissionToken::from_canonical_bytes(token_canonical_bytes)
        .map_err(|e| JsValue::from_str(&format!("decode submission-token: {e}")))?;
    let blob = seal_submission_token(&token, &actor_id, credential_id, &credential, kdf)
        .map_err(|e| JsValue::from_str(&format!("seal submission-token: {e}")))?;
    blob.to_canonical_bytes()
        .map_err(|e| JsValue::from_str(&format!("encode submission-token: {e}")))
}

// ── helpers ──

fn array_32(bytes: &[u8], field: &str) -> Result<[u8; 32], JsValue> {
    if bytes.len() != 32 {
        return Err(JsValue::from_str(&format!(
            "{field} must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

/// WASM error-type adapter over the shared
/// [`fauna_mls::wrapped_blob::credential_input`]; the credential-kind string
/// contract (`"plain"` / `"oauthbearer"`) lives there (priority #2).
fn credential_input<'a>(kind: &str, bytes: &'a [u8]) -> Result<CredentialInput<'a>, JsValue> {
    fauna_mls::wrapped_blob::credential_input(kind, bytes).map_err(|e| JsValue::from_str(&e))
}

/// WASM error-type adapter over the shared
/// [`fauna_mls::wrapped_blob::default_kdf_for`]; the default-KDF-per-credential
/// contract (PLAIN → Argon2id Interactive, OAUTHBEARER → HKDF-SHA-256) lives
/// there (priority #2).
fn default_kdf_for(credential_kind: &str) -> Result<KdfParams, JsValue> {
    fauna_mls::wrapped_blob::default_kdf_for(credential_kind).map_err(|e| JsValue::from_str(&e))
}

/// Resolve KDF params for the seal call. `None` on all argon2_*
/// args selects the library default per credential_kind. Mixing
/// `Some` and `None` on argon2_* args is an error (all three or
/// none).
fn resolve_kdf_params(
    credential_kind: &str,
    m_kib: Option<u32>,
    t: Option<u32>,
    p: Option<u32>,
) -> Result<KdfParams, JsValue> {
    match credential_kind {
        "plain" => match (m_kib, t, p) {
            (None, None, None) => default_kdf_for(credential_kind),
            (Some(m), Some(t), Some(p)) => Ok(KdfParams::Argon2id(Argon2idParams { m, t, p })),
            _ => Err(JsValue::from_str(
                "argon2_m_kib, argon2_t, argon2_p must all be set or all unset",
            )),
        },
        "oauthbearer" => default_kdf_for(credential_kind),
        other => Err(JsValue::from_str(&format!(
            "unknown credential_kind {other:?}; expected \"plain\" or \"oauthbearer\""
        ))),
    }
}
