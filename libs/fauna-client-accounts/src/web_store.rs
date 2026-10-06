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

use crate::SecretStore;

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
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
