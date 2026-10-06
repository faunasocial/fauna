//! The macOS login-Keychain arm of the keyring backend — the same five
//! `keyring_*` primitives as the freedesktop Secret Service arm in `lib.rs`,
//! implemented over the Security framework's `SecItem` generic-password API
//! (the OS-specific seam `apps/tui.md` § Cross-platform names; design owner
//! `apps/tui.md` § Credential storage).
//!
//! Mapping: the Secret Service attribute pair (`application`, `account`)
//! becomes the generic-password pair (`kSecAttrService` = the app namespace,
//! `kSecAttrAccount` = the logical account attribute). Items land in the
//! user's **login keychain** and `kSecAttrSynchronizable` is never set, so the
//! identity secret is device-bound — it never migrates via iCloud Keychain,
//! the same device-bound default the apple apps' ruling targets
//! (`apps/common.md` § Credential storage).
//!
//! Sync + threading: unlike libsecret (async-only, one dedicated OS thread per
//! op — see `lib.rs`), Security-framework calls are plain blocking C calls,
//! safe from any thread — so these run inline, no runtime, no thread spawn.

use security_framework::base::Error as SecError;
use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// `errSecItemNotFound` — the one Security-framework "error" that is a clean
/// miss, not a failure (the analogue of libsecret's empty search).
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

fn is_not_found(e: &SecError) -> bool {
    e.code() == ERR_SEC_ITEM_NOT_FOUND
}

/// Read one item's value by (`application`, `account`). `None` on absent item
/// or any error — the same contract as the Secret Service arm.
pub fn keyring_get(app: &str, account: &str) -> Option<String> {
    match get_generic_password(app, account) {
        Ok(bytes) => String::from_utf8(bytes).ok(),
        Err(_) => None,
    }
}

/// Write one item (add-or-update — `set_generic_password` updates on
/// `errSecDuplicateItem`). Best-effort + log, matching every other keyring
/// write.
pub fn keyring_set(app: &str, account: &str, value: &str) {
    if let Err(e) = set_generic_password(app, account, value.as_bytes()) {
        tracing::warn!("[account-store] keychain set {account:?} failed: {e}");
    }
}

/// Delete the item matching (`application`, `account`). Best-effort + log; an
/// absent item is success. Removes only the one logical key's item — so
/// removing an account never touches its siblings' slots.
pub fn keyring_delete(app: &str, account: &str) {
    match delete_generic_password(app, account) {
        Ok(()) => {}
        Err(e) if is_not_found(&e) => {}
        Err(e) => tracing::warn!("[account-store] keychain delete {account:?} failed: {e}"),
    }
}

/// Every `kSecAttrAccount` currently stored under `kSecAttrService == app`.
/// An `errSecItemNotFound` search is an empty namespace, not an error.
fn namespace_accounts(app: &str) -> Result<Vec<String>, anyhow::Error> {
    let results = match ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(app)
        .load_attributes(true)
        .limit(Limit::All)
        .search()
    {
        Ok(results) => results,
        Err(e) if is_not_found(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(results
        .iter()
        .filter_map(|r| r.simplify_dict())
        .filter_map(|attrs| attrs.get("acct").cloned())
        .collect())
}

/// Delete every item carrying `kSecAttrService == app` — the keychain arm of
/// `CredentialStore::delete_namespace`; see `cred_file_remove` (lib.rs) for
/// why a sign-out must clear the namespace *wholesale*.
///
/// **Searches again until the namespace reads empty**, and returns `Err` if it
/// never does — the same honesty against a concurrent writer as the Secret
/// Service arm (`SecItemDelete` per account is not a transaction either; see
/// `keyring_delete_namespace` in lib.rs for the racing-writer scenario). A
/// single delete failure never aborts the sweep: the remaining items still get
/// their pass, and the final search is what decides success.
pub fn keyring_delete_namespace(app: &str) -> Result<(), anyhow::Error> {
    for _ in 0..crate::NAMESPACE_DELETE_PASSES {
        let accounts = namespace_accounts(app)?;
        if accounts.is_empty() {
            return Ok(());
        }
        for account in accounts {
            match delete_generic_password(app, &account) {
                Ok(()) => {}
                Err(e) if is_not_found(&e) => {}
                Err(e) => {
                    tracing::warn!(
                        "[account-store] namespace delete: item {account:?} failed: {e}"
                    );
                }
            }
        }
    }
    Err(anyhow::anyhow!(
        "[account-store] namespace {app:?} still holds items after {} delete passes — \
         a concurrent writer is re-creating them",
        crate::NAMESPACE_DELETE_PASSES
    ))
}

/// Probe whether the login Keychain is usable **without user interaction**:
/// a default keychain resolves and its status reads unlocked-and-writable —
/// the macOS analogue of the Secret Service probe's "connect, default
/// collection, unlocked" (`apps/tui.md` § Credential storage, step ③).
/// Over SSH the login keychain is normally locked and no SecurityAgent UI can
/// unlock it, so the probe reads *unusable* and resolution falls to the
/// sealed arm — it is the absence of a desktop session, not the OS, that
/// selects headless.
///
/// `SecKeychainGetStatus` is purely observational: no item is touched, no
/// unlock dialog can appear.
pub fn keyring_probe() -> bool {
    use core_foundation::base::TCFType;
    use security_framework::os::macos::keychain::SecKeychain;

    // Not wrapped by security-framework(-sys); Security.framework is already
    // linked by security-framework-sys, so a direct declaration suffices.
    // SAFETY: the signature mirrors Security/SecKeychain.h's stable, decades-old
    // C ABI exactly — `OSStatus SecKeychainGetStatus(SecKeychainRef keychain,
    // SecKeychainStatus *keychainStatus)` — a plain `extern "C"` fn taking an
    // opaque CF-style ref and an out-pointer; no ownership transfer either way.
    unsafe extern "C" {
        fn SecKeychainGetStatus(
            keychain: security_framework_sys::base::SecKeychainRef,
            keychain_status: *mut u32,
        ) -> i32;
    }
    // SecKeychainStatus flags (Security/SecKeychain.h).
    const K_SEC_UNLOCK_STATE_STATUS: u32 = 1;
    const K_SEC_WR_PERM_STATUS: u32 = 4;

    let Ok(keychain) = SecKeychain::default() else {
        return false;
    };
    let mut status: u32 = 0;
    // SAFETY: `keychain.as_concrete_TypeRef()` is a valid `SecKeychainRef` for
    // the call's duration — `keychain` is a live, retained `SecKeychain` owned
    // by this stack frame (just returned by `SecKeychain::default()`), so the
    // ref it hands out cannot dangle mid-call. `&mut status` is a uniquely-
    // owned, aligned, stack-local `*mut u32` — exactly what the out-param
    // contract requires, and the call writes through it at most once.
    let os_status = unsafe { SecKeychainGetStatus(keychain.as_concrete_TypeRef(), &mut status) };
    os_status == 0 && status & K_SEC_UNLOCK_STATE_STATUS != 0 && status & K_SEC_WR_PERM_STATUS != 0
}

/// Live-keychain tests (`--features live-keychain`): they create and delete
/// real items in the developer's login keychain, under a per-run
/// `fauna-credential-store-test-…` service namespace that never collides with
/// a real client's (`fauna-tui` / `fauna-desktop`) and is deleted on exit.
/// Off by default — CI runners and SSH sessions have no unlocked login
/// keychain (the same opt-in shape as fauna-linux's `live-secret-service`).
///
/// MANUAL: no gate runs this (real unlocked macOS login keychain needed) —
/// run by hand with `cargo test -p fauna-credential-store --features
/// live-keychain` on macOS before touching keychain read/write
/// logic.
#[cfg(all(test, feature = "live-keychain"))]
mod live_tests {
    use super::*;
    use fauna_client_accounts::SecretStore;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn fresh_namespace() -> String {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("fauna-credential-store-test-{}-{n}", std::process::id())
    }

    /// The live suite is meaningless against a locked keychain; fail loudly
    /// with the remedy rather than green-skipping (the guarded-assertion trap,
    /// `testing.md` point 10 at the test level).
    fn require_probe() {
        assert!(
            keyring_probe(),
            "live-keychain tests need an unlocked login keychain — run inside \
             a GUI session, not over SSH"
        );
    }

    #[test]
    fn get_set_delete_round_trip_against_the_live_keychain() {
        require_probe();
        let ns = fresh_namespace();

        assert_eq!(keyring_get(&ns, "fauna/index"), None, "empty ns reads None");
        keyring_set(&ns, "fauna/index", r#"{"active":null,"accounts":[]}"#);
        assert_eq!(
            keyring_get(&ns, "fauna/index").as_deref(),
            Some(r#"{"active":null,"accounts":[]}"#)
        );
        // Overwrite (the errSecDuplicateItem → update path).
        keyring_set(&ns, "fauna/index", r#"{"active":"abc","accounts":[]}"#);
        assert_eq!(
            keyring_get(&ns, "fauna/index").as_deref(),
            Some(r#"{"active":"abc","accounts":[]}"#)
        );

        keyring_set(&ns, "secret_key", "1111");
        keyring_delete(&ns, "secret_key");
        assert_eq!(keyring_get(&ns, "secret_key"), None);
        // The sibling delete leaves the index untouched.
        assert!(keyring_get(&ns, "fauna/index").is_some());
        // Deleting an absent item is success (idempotent sign-out).
        keyring_delete(&ns, "secret_key");

        keyring_delete_namespace(&ns).unwrap();
    }

    #[test]
    fn delete_namespace_clears_all_items_and_spares_sibling_namespaces() {
        require_probe();
        let ns = fresh_namespace();
        let sibling = fresh_namespace();

        keyring_set(&ns, "secret_key", "1111");
        keyring_set(&ns, "fauna/index", "{}");
        keyring_set(&ns, "fauna/abc/secret", "2222");
        keyring_set(&sibling, "secret_key", "3333");

        keyring_delete_namespace(&ns).unwrap();

        assert_eq!(keyring_get(&ns, "secret_key"), None);
        assert_eq!(keyring_get(&ns, "fauna/index"), None);
        assert_eq!(keyring_get(&ns, "fauna/abc/secret"), None);
        assert_eq!(
            keyring_get(&sibling, "secret_key").as_deref(),
            Some("3333"),
            "a namespace wipe never crosses service boundaries"
        );
        // An empty namespace deletes as success (idempotent).
        keyring_delete_namespace(&ns).unwrap();

        keyring_delete_namespace(&sibling).unwrap();
    }

    #[test]
    fn credential_store_resolves_the_keychain_arm_on_a_desktop_session() {
        require_probe();
        // With no e2e dir, no sealed file, and a usable keychain, the tui
        // constructor must land on the keyring arm (`tui.md` § Credential
        // storage, step ③) — neither the file dir nor the sealed backend, and
        // no unlock surface. Guard: skip under FAUNA_E2E_CREDENTIAL_DIR (the
        // e2e file arm legitimately wins there, step ①).
        if crate::cred_file_dir().is_some() {
            eprintln!("skipped: FAUNA_E2E_CREDENTIAL_DIR is set in this environment");
            return;
        }
        // FAUNA_KEYRING_APP would override `ns` inside the constructor, and the
        // round-trip below would then write into — and namespace-WIPE — that
        // real namespace. Refuse rather than risk it.
        // Through the shared accessor, like the two guards around it — and it is
        // the more precise guard: an *empty* value leaves the constructor on the
        // default namespace, so it is not the hazard this refuses.
        if crate::keyring_app_override().is_some() {
            eprintln!("skipped: FAUNA_KEYRING_APP is set in this environment");
            return;
        }
        if crate::force_headless_store() {
            eprintln!("skipped: FAUNA_E2E_FORCE_HEADLESS_STORE is set in this environment");
            return;
        }
        let ns = fresh_namespace();
        let data_dir = std::env::temp_dir().join(&ns);
        let store = crate::CredentialStore::new_with_headless_fallback(&ns, data_dir);
        assert_eq!(store.file_backend_dir(), None, "not the file arm");
        assert!(store.sealed_backend().is_none(), "not the sealed arm");
        assert!(!store.needs_unlock(), "the keychain arm needs no unlock");

        // And it round-trips through the SecretStore trait.
        store.set("fauna/index", "{}");
        assert_eq!(store.get("fauna/index").as_deref(), Some("{}"));
        store.delete_namespace().unwrap();
    }
}
