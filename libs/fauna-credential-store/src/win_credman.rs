//! The Windows Credential Manager arm — the same five `keyring_*` primitives
//! as the Secret Service (linux) and login-Keychain (macOS) arms, over the
//! Win32 `CredRead/Write/Delete/Enumerate` generic-credential API. This is
//! the arm `apps/tui.md` § Credential storage owed (and the one
//! `apps/sync-agent.md` § Credential model consumes for the agent's
//! persisted capability); until it landed, windows fell through to the Secret
//! Service code path, whose probe always fails there.
//!
//! Mapping: one item = one generic credential, `TargetName` =
//! `"{application}/{account}"` (the namespace is the prefix — Credential
//! Manager has no attribute search, so the composite name carries both
//! halves; `CredEnumerateW("{application}/*")` is the namespace sweep),
//! `CredentialBlob` = the UTF-8 value, `Persist` = `LOCAL_MACHINE` (persists
//! across logons **for this user's profile** — per-user despite the name;
//! `ENTERPRISE` would roam, which key material must never do). All calls are
//! plain blocking C calls, like the macOS arm — no thread/runtime dance — and
//! every one of them holds the per-user [`lock_credman`], because Credential
//! Manager itself loses overlapping updates.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW,
    CredEnumerateW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{PCWSTR, PWSTR};

/// NUL-terminated UTF-16 of `s`, for the `*W` APIs.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The composite `TargetName` carrying both the namespace and the item key.
fn target_name(app: &str, account: &str) -> String {
    format!("{app}/{account}")
}

/// The lock file's name, under the per-user sync state base
/// ([`fauna_core::platform_ids::sync_state_base`]).
const CREDMAN_LOCK_FILE: &str = "credential-manager.lock";

/// Take the per-user lock every call of this arm holds for its duration.
///
/// **Why a lock at all.** Credential Manager loses updates when two writers of
/// one user overlap, even on rows that have nothing to do with each other.
/// Measured 2026-10-27 on Windows 11 ARM64: two processes, one thread
/// apiece, each writing, reading back and deleting targets of its own, got
/// about one iteration in a hundred wrong — a `CredWriteW` that returned
/// success whose row never read back, a `CredDeleteW` that returned success
/// whose row was still there, a deleted row back a millisecond later — and
/// four threads of one process about one in six. One writer at a time lost
/// nothing in 1800 iterations. No call reports an error, so a lost write is
/// silent: a `fauna/index` that keeps its old value, a deleted secret that
/// survives a sign-out. Every Fauna process that touches this store goes
/// through these primitives — the windows app over the FFI, fauna-tui, the
/// sync agent — so one lock serializes them all. A writer outside Fauna (a git
/// credential helper) can still overlap; that is rare, and not ours to fix.
///
/// **A file, not a named mutex.** The credential set is per user across logon
/// sessions; a `Local\` mutex is per session, and a `Global\` one needs a
/// privilege the standard-user agent lacks — the reason the sync agent's
/// `InstanceLock` ruling gives (`apps/sync-agent.md`). The file is the
/// crate's one lock mechanism ([`crate::lock_file_at`]), so the kernel releases
/// it if a holder dies.
///
/// **Advisory degrade**, as the file arm's [`crate::lock_cred_file`]: with no
/// lock the call runs unserialized and says so, never refused.
fn lock_credman() -> Option<std::fs::File> {
    let guard = fauna_core::platform_ids::sync_state_base()
        .and_then(|base| crate::lock_file_at(&base.join(CREDMAN_LOCK_FILE)));
    if guard.is_none() {
        tracing::warn!(
            "[account-store] credman: could not take {CREDMAN_LOCK_FILE}; this call runs unserialized"
        );
    }
    guard
}

/// Read one item's value by (`application`, `account`). `None` on absent item
/// or any error (matching the other arms' read posture).
pub fn keyring_get(app: &str, account: &str) -> Option<String> {
    let _lock = lock_credman();
    let target = to_wide(&target_name(app, account));
    let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `target` outlives the call; `pcred` receives an allocation we
    // free with `CredFree` on every path that got one.
    let read = unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut pcred) };
    if read.is_err() || pcred.is_null() {
        return None;
    }
    // SAFETY: `pcred` is a valid CREDENTIALW from CredReadW until CredFree.
    let value = unsafe {
        let cred = &*pcred;
        // An empty value is a real row (a registry slot cleared to ""), and
        // Credential Manager may hand back a NULL blob pointer for it — which
        // `from_raw_parts` must never see, whatever the length.
        if cred.CredentialBlobSize == 0 || cred.CredentialBlob.is_null() {
            Some(String::new())
        } else {
            let blob =
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
            String::from_utf8(blob.to_vec()).ok()
        }
    };
    // SAFETY: allocated by CredReadW.
    unsafe { CredFree(pcred as *const std::ffi::c_void) };
    value
}

/// Write one item (add-or-replace — `CredWriteW` with no preserve flag
/// replaces an existing credential of the same `TargetName`). Best-effort +
/// log, matching every other arm's write.
///
/// A value over [`crate::MAX_ITEM_VALUE_BYTES`] is refused here rather than
/// handed to `CredWriteW`, which refuses it too but says only
/// `The stub received bad data`; either way the item keeps what it had.
pub fn keyring_set(app: &str, account: &str, value: &str) {
    let name = target_name(app, account);
    if value.len() > crate::MAX_ITEM_VALUE_BYTES {
        tracing::warn!(
            "[account-store] credman set {name:?} refused: {} bytes is over the {}-byte cap",
            value.len(),
            crate::MAX_ITEM_VALUE_BYTES
        );
        return;
    }
    let _lock = lock_credman();
    let mut target = to_wide(&name);
    // A display-only user name; generic credentials require none, but naming
    // the namespace makes the Credential Manager UI listing legible.
    let mut user = to_wide(app);
    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        Comment: PWSTR::null(),
        LastWritten: FILETIME::default(),
        CredentialBlobSize: value.len() as u32,
        CredentialBlob: value.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: PWSTR::null(),
        UserName: PWSTR(user.as_mut_ptr()),
    };
    // SAFETY: every pointer in `cred` outlives the call.
    if let Err(e) = unsafe { CredWriteW(&cred, 0) } {
        tracing::warn!(
            "[account-store] credman set {name:?} failed: {}",
            set_failure_note(&e)
        );
    }
}

/// The warn text for a refused `CredWriteW`. One failure gets words of its
/// own: `ERROR_NOT_ENOUGH_MEMORY` is not RAM but this user's whole credential
/// set at its size budget (about 730 KB, every row counting its value, its
/// names and some bookkeeping — `apps/windows.md` § Key storage → *The bound
/// the store brings*). While it is full Windows refuses every write of this
/// user, Fauna's or any other program's, and says only that memory ran out.
fn set_failure_note(e: &windows::core::Error) -> String {
    if e.code() == windows::Win32::Foundation::ERROR_NOT_ENOUGH_MEMORY.to_hresult() {
        format!(
            "Credential Manager is full: this user's whole credential set is at its size \
             budget, and every write is refused until rows are deleted ({e})"
        )
    } else {
        e.to_string()
    }
}

/// Delete the item matching (`application`, `account`). Best-effort + log; an
/// absent item is a quiet no-op (the trait's delete contract).
pub fn keyring_delete(app: &str, account: &str) {
    let _lock = lock_credman();
    let name = target_name(app, account);
    let target = to_wide(&name);
    // SAFETY: `target` outlives the call.
    let deleted = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
    if let Err(e) = deleted {
        // ERROR_NOT_FOUND is the expected no-op; anything else is worth a log.
        if e.code() != windows::Win32::Foundation::ERROR_NOT_FOUND.to_hresult() {
            tracing::warn!("[account-store] credman delete {name:?} failed: {e}");
        }
    }
}

/// Delete every item in the `application` namespace
/// (`TargetName` prefix `"{app}/"`), the credman arm of
/// [`crate::CredentialStore::delete_namespace`]. Multi-pass like the Secret
/// Service arm ([`crate::NAMESPACE_DELETE_PASSES`]): enumerate is a snapshot,
/// so items written mid-sweep survive a single pass — search again until the
/// namespace reads empty, `Err` if it never does.
pub fn keyring_delete_namespace(app: &str) -> Result<(), anyhow::Error> {
    let _lock = lock_credman();
    let filter = to_wide(&format!("{app}/*"));
    for _ in 0..crate::NAMESPACE_DELETE_PASSES {
        let mut count: u32 = 0;
        let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `filter` outlives the call; the returned array is freed with
        // one CredFree, per the API contract.
        let enumerated =
            unsafe { CredEnumerateW(PCWSTR(filter.as_ptr()), None, &mut count, &mut creds) };
        if enumerated.is_err() || creds.is_null() {
            // ERROR_NOT_FOUND ⇒ nothing matches the filter: namespace empty.
            return Ok(());
        }
        // SAFETY: CredEnumerateW yielded `count` valid entries until CredFree.
        unsafe {
            for i in 0..count as usize {
                let cred = &**creds.add(i);
                if let Err(e) = CredDeleteW(PCWSTR(cred.TargetName.0), cred.Type, None) {
                    tracing::warn!("[account-store] credman namespace delete: item failed: {e}");
                }
            }
            CredFree(creds as *const std::ffi::c_void);
        }
    }
    // One final emptiness check mirrors the Secret Service arm's posture.
    let mut count: u32 = 0;
    let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: as above.
    let still = unsafe { CredEnumerateW(PCWSTR(filter.as_ptr()), None, &mut count, &mut creds) };
    if still.is_ok() && !creds.is_null() {
        // SAFETY: allocated by CredEnumerateW.
        unsafe { CredFree(creds as *const std::ffi::c_void) };
        anyhow::bail!(
            "[account-store] namespace {app:?} still holds items after {} delete passes — \
             a concurrent writer is re-creating them",
            crate::NAMESPACE_DELETE_PASSES
        );
    }
    Ok(())
}

/// Whether the Credential Manager is usable. It is a per-user OS service with
/// no lock state (unlike gnome-keyring collections) and no session-bus
/// dependency, so inside any interactive or service user session it simply
/// is; a write probe would only churn the store. Constant `true` mirrors what
/// `CredWriteW` availability means in practice.
pub fn keyring_probe() -> bool {
    true
}

#[cfg(test)]
mod tests {
    //! Live Credential Manager round-trip — windows-only by construction
    //! (this whole module is `cfg(target_os = "windows")`), and safe to run on
    //! any windows session: items live under a per-run probe namespace that
    //! the test sweeps, so it never touches real `fauna-*` namespaces (the
    //! same posture as the macOS `live-keychain` tests).
    use super::*;

    /// A probe namespace swept when dropped, so a test that fails part-way
    /// leaves no rows behind: a leaked row stays in the developer's store and
    /// counts against this user's whole credential set ([`set_failure_note`]).
    struct ProbeNamespace(String);

    impl Drop for ProbeNamespace {
        fn drop(&mut self) {
            let _ = keyring_delete_namespace(&self.0);
        }
    }

    #[test]
    fn round_trip_and_namespace_sweep() {
        let probe = ProbeNamespace(format!("fauna-credman-probe-{}", std::process::id()));
        let app = probe.0.as_str();

        assert!(keyring_probe(), "credman is always reachable per-user");
        assert!(keyring_get(app, "alpha").is_none(), "starts absent");

        keyring_set(app, "alpha", "value-1");
        assert_eq!(keyring_get(app, "alpha").as_deref(), Some("value-1"));
        // Replace-on-write, not append.
        keyring_set(app, "alpha", "value-2");
        assert_eq!(keyring_get(app, "alpha").as_deref(), Some("value-2"));

        // An empty value replaces the old one and reads back as empty — never
        // as the value it replaced (a registry slot cleared to "", e.g. an
        // actor with no handle yet).
        keyring_set(app, "alpha", "");
        assert_eq!(keyring_get(app, "alpha").as_deref(), Some(""));

        keyring_set(app, "beta", "value-3");
        // A value over the cap is refused, and the item keeps what it had.
        keyring_set(app, "beta", &"x".repeat(crate::MAX_ITEM_VALUE_BYTES + 1));
        assert_eq!(keyring_get(app, "beta").as_deref(), Some("value-3"));
        keyring_delete(app, "alpha");
        assert!(keyring_get(app, "alpha").is_none(), "deleted item is gone");
        assert_eq!(
            keyring_get(app, "beta").as_deref(),
            Some("value-3"),
            "sibling item survives a single delete"
        );

        keyring_delete_namespace(app).expect("namespace sweep succeeds");
        assert!(
            keyring_get(app, "beta").is_none(),
            "sweep cleared the namespace"
        );
        // Deleting an absent item / empty namespace is a quiet no-op.
        keyring_delete(app, "beta");
        keyring_delete_namespace(app).expect("empty namespace sweep is Ok");
    }

    /// A write refused because this user's whole credential set is full says
    /// so. Windows' own text for the error points at RAM, and nothing else in
    /// the log would tell a reader that every writer of this user is refused.
    #[test]
    fn a_full_store_is_named_full() {
        use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_NOT_ENOUGH_MEMORY};
        let full = windows::core::Error::from_hresult(ERROR_NOT_ENOUGH_MEMORY.to_hresult());
        let note = set_failure_note(&full);
        assert!(note.contains("Credential Manager is full"), "{note}");
        let denied = windows::core::Error::from_hresult(ERROR_ACCESS_DENIED.to_hresult());
        let note = set_failure_note(&denied);
        assert!(!note.contains("is full"), "{note}");
    }

    /// Overlapping writers lose nothing: each of four threads writes, reads
    /// back, deletes and re-reads items of its own, and no write, delete or
    /// re-read may come out wrong. Unserialized, Credential Manager got 17 of
    /// these 200 iterations wrong when this test was written — writes that
    /// never read back, deletes that did not take, deleted items back a
    /// millisecond later ([`lock_credman`] has the wider measurement) — so
    /// this is red without the lock.
    #[test]
    fn overlapping_writers_lose_no_update() {
        let probe = ProbeNamespace(format!(
            "fauna-credman-probe-{}-overlap",
            std::process::id()
        ));
        let app = probe.0.as_str();
        let anomalies = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for w in 0..4 {
                let anomalies = &anomalies;
                s.spawn(move || {
                    for i in 0..50 {
                        let account = format!("w{w}-i{i}");
                        let note = |what: &str| {
                            anomalies.lock().unwrap().push(format!("{account}: {what}"));
                        };
                        keyring_set(app, &account, "value");
                        if keyring_get(app, &account).as_deref() != Some("value") {
                            note("the write did not read back");
                        }
                        keyring_delete(app, &account);
                        if keyring_get(app, &account).is_some() {
                            note("the delete did not take");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        if keyring_get(app, &account).is_some() {
                            note("the deleted item came back");
                        }
                    }
                });
            }
        });
        let swept = keyring_delete_namespace(app);
        let anomalies = anomalies.into_inner().unwrap();
        assert!(
            anomalies.is_empty(),
            "{} of 200 iterations went wrong under overlapping writers:\n{}",
            anomalies.len(),
            anomalies.join("\n")
        );
        swept.expect("the probe namespace sweeps clean");
    }
}
