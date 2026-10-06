//! What a sign-out's **credential** erase could not remove.
//!
//! The filesystem half of a sign-out already answers this
//! (`fauna_account_store::db::EraseSweep`). This is the same answer for the
//! credential half — the identity seed, `fauna/index`, the
//! store writer key and each principal bundle — so the residue line a user sees
//! is fed by BOTH halves rather than by the one that happens to return a value
//! (`account-scoping.md` § Erasure follows scope → *the credential half is a
//! residue class too*).
//!
//! **Why this exists at all.** A sign-out whose credential wipe fails — a locked
//! or unreachable keyring collection, a backend error, a store that silently
//! refuses the delete — used to paint the same clean "Signed out" as one that
//! worked, over a device whose store still held the identity seed. The residue
//! line had been built to kill exactly that false negative for the content
//! stores; the credentials sat one namespace over, unasked.
//!
//! **Why a read-back and not an error.** [`SecretStore::delete`] returns
//! nothing, and on every arm the reason is baked in: the keyring arm warn-logs,
//! the file arm warn-logs a failed rewrite, apple drops the keychain status,
//! android is best-effort. Widening that seam to return a `Result` would change
//! three foreign store implementations to obtain a signal the erase can get by
//! itself — **a key that still reads back after its delete survived it**,
//! whatever the store's reason. What a read-back cannot see is a store that
//! cannot be *read* either (a locked collection reads as absent), which is why
//! a caller that also wipes the whole namespace folds that wipe's own failure in
//! through [`CredentialSweep::wipe_failed`].

use crate::SecretStore;

/// The outcome of [`crate::AccountRegistry::clear_all`]: which credential keys
/// are still readable after the erase, and whether a wholesale namespace wipe
/// the caller ran reported failure.
///
/// Empty `survivors` **and** no wipe failure is the only outcome that means the
/// credentials are gone — the same "empty is the only clean" rule the
/// filesystem sweep carries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[must_use = "a sign-out that drops this reports success over a device that may \
              still hold the signed-out identity's credentials - the defect this \
              type exists to prevent"]
pub struct CredentialSweep {
    /// Logical keys (`fauna/<actor>/secret`, `fauna/index`, …) the erase
    /// deleted and that still read back from at least one namespace it reaches.
    /// For the **log** only: a user is owed the fact that their sign-in
    /// credentials survived, never a key name.
    pub survivors: Vec<String>,
    /// A whole-namespace wipe run after [`crate::AccountRegistry::clear_all`]
    /// (`CredentialStore::delete_namespace` on the freedesktop apps) returned an
    /// error. Carried separately because it is the one failure a read-back can
    /// miss: a keyring that refused the wipe because it could not be reached
    /// refuses the read-back the same way, and reads as empty.
    pub wipe_failed: bool,
}

impl CredentialSweep {
    /// True when nothing survived and no wipe failed — the only clean outcome.
    pub fn is_clean(&self) -> bool {
        self.survivors.is_empty() && !self.wipe_failed
    }

    /// Record that a wholesale namespace wipe the caller ran failed.
    pub fn record_wipe_failure(&mut self) {
        self.wipe_failed = true;
    }
}

/// The keys in `keys` still readable from `store` or any of `aux` — deduplicated,
/// order kept. Pure reads: an erase must never write, and a read-back that
/// wrote would re-create the slot it is checking.
pub(crate) fn still_readable(
    store: &dyn SecretStore,
    aux: &[std::sync::Arc<dyn SecretStore>],
    keys: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut survivors: Vec<String> = Vec::new();
    for key in keys {
        if survivors.contains(&key) {
            continue;
        }
        if store.get(&key).is_some() || aux.iter().any(|a| a.get(&key).is_some()) {
            survivors.push(key);
        }
    }
    survivors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_unfailed_is_the_only_clean_sweep() {
        assert!(CredentialSweep::default().is_clean());
        assert!(
            !CredentialSweep {
                survivors: vec!["fauna/index".into()],
                wipe_failed: false,
            }
            .is_clean()
        );
        let mut refused = CredentialSweep::default();
        refused.record_wipe_failure();
        assert!(
            !refused.is_clean(),
            "a wipe that failed with nothing readable is the locked-keyring shape — \
             the read-back cannot see it, so the flag must carry it"
        );
    }
}
