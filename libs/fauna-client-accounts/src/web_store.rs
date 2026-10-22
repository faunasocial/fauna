//! Web's [`SecretStore`]: `window.localStorage`, addressed by logical keys.
//!
//! Lives in this crate (moved here from `fauna-wasm/src/accounts.rs` when
//! `fauna-wasm-launch` became the second consumer; two wasm chunks can share
//! code only through a common crate).
//!
//! Every logical key is the localStorage key **verbatim**: the `fauna/index`
//! blob, the `fauna/{actor}/…` per-actor slots and the install device secret.
//! The pre-multi-account `fauna_secret` / `fauna_node_url` / `fauna_handle` /
//! … single-slot keys and the eleven per-field wizard-resume keys the retired
//! TS slot stores wrote are **gone** (2026-09-24): web's onboarding hand-off
//! writes the registry directly and every read goes through the registry's
//! session material, so no `legacy/*` key is ever mapped or written on web
//! (`long-term-store.md` § Downgrade mirror + abandoned-append recovery).

#![cfg(target_arch = "wasm32")]

use std::sync::OnceLock;

use wasm_bindgen::prelude::*;

use crate::{SealError, SealedSecretStore, SealingReport, SecretStore, is_identity_store_key};

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The `sessionStorage` key of this tab's account pin — the browser twin of
/// [`crate::BOUND_ACCOUNT_ENV`] (`account-scoping.md` § Concurrent instances →
/// *Web*). The SPA's `#lib/tabPin` writes it; this is its Rust-side reader, so
/// the two must name the same key.
pub const TAB_PIN_KEY: &str = "fauna_tab_account";

/// This tab's account pin, normalized like the launch binding, or `None` for an
/// unpinned tab — and for one whose `sessionStorage` cannot be read, which then
/// behaves as the primary, the SPA's own degrade (`#lib/tabPin::tabPin`). Pass
/// it to [`crate::AccountRegistry::tab_launch_persistence`].
pub fn tab_pin() -> Option<String> {
    let raw = web_sys::window()?
        .session_storage()
        .ok()
        .flatten()?
        .get_item(TAB_PIN_KEY)
        .ok()
        .flatten();
    crate::parse_bound_account(raw.as_deref())
}

/// localStorage-backed [`SecretStore`] — the browser twin of native
/// `CredentialStore`. Stateless: every call goes straight to the global
/// `window.localStorage`. Best-effort like native stores — a quota /
/// private-mode error must never break a live sign-in (mirrors
/// `LocalStoragePinStore`).
pub struct LocalStorageSecretStore;

impl SecretStore for LocalStorageSecretStore {
    fn get(&self, key: &str) -> Option<String> {
        local_storage()?.get_item(key).ok().flatten()
    }
    fn set(&self, key: &str, value: &str) {
        if let Some(ls) = local_storage() {
            let _ = ls.set_item(key, value);
        }
    }
    fn delete(&self, key: &str) {
        if let Some(ls) = local_storage() {
            let _ = ls.remove_item(key);
        }
    }
}

/// This wasm runtime's sealing wrapper over `localStorage`. Each chunk is a
/// separately compiled binary with its own linear memory, so each holds its
/// own — locked until the page installs the store key it unlocked
/// (`web.md` § Identity store at rest → *The unlocked store key lives in page
/// memory and nowhere else*).
fn sealed() -> &'static SealedSecretStore<LocalStorageSecretStore> {
    static SEALED: OnceLock<SealedSecretStore<LocalStorageSecretStore>> = OnceLock::new();
    SEALED.get_or_init(|| SealedSecretStore::new(LocalStorageSecretStore))
}

/// The web identity store — the one [`SecretStore`] every wasm chunk reads and
/// writes the registry's rows through. Which arm serves a call follows the
/// rows, never a flag: **no seal header** → the rows are plain and pass
/// straight through (a browser before its create, and the test build's plain
/// arm); **a header** → every call goes through this runtime's
/// [`SealedSecretStore`], which reads nothing and writes nothing until it is
/// unlocked (`web.md` § Identity store at rest → *Unlock comes before launch
/// routing*).
pub struct WebIdentityStore;

impl WebIdentityStore {
    /// This runtime's sealing wrapper: create / unlock / change the
    /// passphrase, hand or install the store key.
    pub fn sealed() -> &'static SealedSecretStore<LocalStorageSecretStore> {
        sealed()
    }

    /// Rewrite every plain identity-store row sealed, in place, resuming a
    /// pass a crash interrupted ([`SealedSecretStore::resume_sealing`] over
    /// this browser's [`is_identity_store_key`] rows).
    ///
    /// # Errors
    ///
    /// As [`SealedSecretStore::resume_sealing`].
    pub fn resume_sealing() -> Result<SealingReport, SealError> {
        let keys: Vec<String> = local_storage()
            .map(|ls| {
                let len = ls.length().unwrap_or(0);
                (0..len)
                    .filter_map(|i| ls.key(i).ok().flatten())
                    .filter(|k| is_identity_store_key(k))
                    .collect()
            })
            .unwrap_or_default();
        sealed().resume_sealing(keys.iter().map(String::as_str))
    }

    fn arm() -> &'static dyn SecretStore {
        let sealed = sealed();
        if sealed.has_header() {
            sealed
        } else {
            sealed.inner()
        }
    }
}

impl SecretStore for WebIdentityStore {
    fn get(&self, key: &str) -> Option<String> {
        Self::arm().get(key)
    }
    fn set(&self, key: &str, value: &str) {
        Self::arm().set(key, value);
    }
    fn delete(&self, key: &str) {
        Self::arm().delete(key);
    }
}

fn seal_error(e: SealError) -> JsValue {
    let code = match e {
        SealError::NoHeader => "no_header",
        SealError::HeaderExists => "header_exists",
        SealError::WrongPassphraseOrCorrupt => "wrong_passphrase_or_corrupt",
        SealError::PasskeyDidNotOpen => "passkey_did_not_open",
        SealError::MalformedPasskey => "malformed_passkey",
        SealError::NewerHeader(_) => "newer_header",
        SealError::Locked => "locked",
        SealError::WrongStoreKey => "wrong_store_key",
        SealError::ReadBackFailed(_) => "read_back_failed",
        SealError::Kdf(_) => "kdf",
    };
    JsValue::from_str(code)
}

// The page's door to this chunk's identity store. Every chunk that links this
// crate carries them; the page calls each loaded chunk's own (they do not
// share memory). Errors reject with a stable code string.

/// Whether this browser's identity store has a seal header — unlock mode, not
/// create mode.
#[wasm_bindgen(js_name = identityStoreHasHeader)]
pub fn identity_store_has_header() -> bool {
    sealed().has_header()
}

/// Whether this chunk's sealed store still waits for its key. Always `false`
/// with no header (the plain arm serves).
#[wasm_bindgen(js_name = identityStoreIsLocked)]
pub fn identity_store_is_locked() -> bool {
    sealed().has_header() && sealed().is_locked()
}

/// Write the seal header under `passphrase`, unlock, and seal every plain
/// row this browser already holds.
#[wasm_bindgen(js_name = identityStoreCreate)]
pub fn identity_store_create(passphrase: &str) -> Result<(), JsValue> {
    sealed().create(passphrase).map_err(seal_error)?;
    WebIdentityStore::resume_sealing().map_err(seal_error)?;
    Ok(())
}

/// Open the header under `passphrase` and resume any interrupted sealing.
#[wasm_bindgen(js_name = identityStoreUnlock)]
pub fn identity_store_unlock(passphrase: &str) -> Result<(), JsValue> {
    sealed().unlock(passphrase).map_err(seal_error)?;
    WebIdentityStore::resume_sealing().map_err(seal_error)?;
    Ok(())
}

/// Re-wrap the store key under `new` (one header write; no row changes).
#[wasm_bindgen(js_name = identityStoreChangePassphrase)]
pub fn identity_store_change_passphrase(current: &str, new: &str) -> Result<(), JsValue> {
    sealed().change_passphrase(current, new).map_err(seal_error)
}

/// The header's passkey wraps, readable while locked (neither field is
/// secret): an array of `{ credentialId: Uint8Array, prfSalt: Uint8Array }`,
/// what the page asks an authenticator for — the `allowCredentials` entries
/// and each one's PRF eval input (`web.md` § Identity store at rest → *A
/// passkey wrap*). Empty with no header.
#[wasm_bindgen(js_name = identityStorePasskeys)]
pub fn identity_store_passkeys() -> Result<js_sys::Array, JsValue> {
    let out = js_sys::Array::new();
    if !sealed().has_header() {
        return Ok(out);
    }
    for wrap in sealed().passkey_wraps().map_err(seal_error)? {
        let entry = js_sys::Object::new();
        js_sys::Reflect::set(
            &entry,
            &"credentialId".into(),
            &js_sys::Uint8Array::from(wrap.credential_id.as_slice()),
        )?;
        js_sys::Reflect::set(
            &entry,
            &"prfSalt".into(),
            &js_sys::Uint8Array::from(wrap.prf_salt.as_slice()),
        )?;
        out.push(&entry);
    }
    Ok(out)
}

/// A fresh 32-byte PRF salt for a passkey wrap about to be added.
#[wasm_bindgen(js_name = identityStoreNewPasskeySalt)]
pub fn identity_store_new_passkey_salt() -> Vec<u8> {
    crate::fresh_passkey_prf_salt().to_vec()
}

/// Wrap the held store key under a passkey's PRF output — one header write,
/// no row touched; rejects `locked` while locked.
#[wasm_bindgen(js_name = identityStoreAddPasskey)]
pub fn identity_store_add_passkey(
    credential_id: &[u8],
    prf_salt: &[u8],
    prf_output: &[u8],
) -> Result<(), JsValue> {
    sealed()
        .add_passkey(credential_id, prf_salt, prf_output)
        .map_err(seal_error)
}

/// Open the header with a passkey's PRF output and resume any interrupted
/// sealing — the passphrase unlock's twin.
#[wasm_bindgen(js_name = identityStoreUnlockWithPasskey)]
pub fn identity_store_unlock_with_passkey(
    credential_id: &[u8],
    prf_output: &[u8],
) -> Result<(), JsValue> {
    sealed()
        .unlock_with_passkey(credential_id, prf_output)
        .map_err(seal_error)?;
    WebIdentityStore::resume_sealing().map_err(seal_error)?;
    Ok(())
}

/// Remove the passkey wrap for `credential_id` — one header write; the
/// passphrase still opens the store. `false` when there was none.
#[wasm_bindgen(js_name = identityStoreRemovePasskey)]
pub fn identity_store_remove_passkey(credential_id: &[u8]) -> Result<bool, JsValue> {
    sealed().remove_passkey(credential_id).map_err(seal_error)
}

/// The unlocked store key as 64 lowercase hex, for the page to install into
/// its other chunks and hand to a newly opened tab — kept in page memory only.
/// `None` while locked.
#[wasm_bindgen(js_name = identityStoreKey)]
pub fn identity_store_key() -> Option<String> {
    sealed()
        .store_key()
        .map(|k| fauna_core::hex32::encode(&k.to_array()))
}

/// Install a store key another chunk or tab unlocked, once it opens the
/// header's check.
#[wasm_bindgen(js_name = identityStoreInstallKey)]
pub fn identity_store_install_key(key_hex: &str) -> Result<(), JsValue> {
    let key =
        fauna_core::hex32::decode(key_hex).map_err(|_| seal_error(SealError::WrongStoreKey))?;
    sealed().unlock_with_store_key(key).map_err(seal_error)
}

/// Start over: erase this browser's identity store — every identity-store
/// row, the staging rows and the seal header — and lock this chunk. The page
/// calls it only after the user's explicit confirmation on the unlock page
/// (`unlock-reset-confirm-button`), then returns to create mode; it locks the
/// other chunks it installed a key into itself.
#[wasm_bindgen(js_name = identityStoreStartOver)]
pub fn identity_store_start_over() {
    let keys: Vec<String> = local_storage()
        .map(|ls| {
            let len = ls.length().unwrap_or(0);
            (0..len).filter_map(|i| ls.key(i).ok().flatten()).collect()
        })
        .unwrap_or_default();
    sealed().start_over(keys.iter().map(String::as_str));
}

/// Forget this chunk's store key.
#[wasm_bindgen(js_name = identityStoreLock)]
pub fn identity_store_lock() {
    sealed().lock();
}
