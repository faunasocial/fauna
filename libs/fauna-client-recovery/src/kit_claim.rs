//! Which seat of one install mints a succession's recovery kit.
//!
//! A succession owes its successor a fresh kit (`identity-succession.md`
//! § The RecoveryKey → *At succession*), and two seats of one install can each
//! come to owe it: the seat that ran the ceremony, and a sibling seat sitting on
//! the same retired identity, which meets the supersession at its next connect
//! and adopts the successor this install already holds
//! (`succession-propagation.md` § Propagation → *Own device fleet*, the
//! relaunch-adoption clause). Both minting is the defect this module closes: the
//! nest takes one first registration and refuses the other, and when the
//! refused seat is the ceremony's, the window the user is watching paints no
//! kit.
//!
//! The clause's rule is that the adoption owes the kit only for a ceremony that
//! never reached it. "Never reached" is a fact about a *process*, which no
//! record on disk can state (a crashed ceremony leaves the same record as a
//! running one), so it is read off an OS file lock, which the kernel releases
//! when the holder dies:
//!
//! - the ceremony **claims** the successor ([`KitClaims::claim`]) before it
//!   persists the successor's seed — and a sibling seat can adopt only a
//!   successor whose seed it finds persisted, so no adoption can precede the
//!   claim, whichever seat reads the chain first;
//! - the ceremony **releases** it when its own kit lands, or when its outcome
//!   leaves it nothing to mint (not landed, undecided, landed for another);
//! - every first-registration mint asks [`KitClaims::owned_elsewhere`] first,
//!   and a live claim held by another process refuses it with
//!   [`crate::RecoveryError::KitMintedElsewhere`] — the claimant mints, and it
//!   is the seat that shows the phrase.
//!
//! A ceremony that dies before its kit frees the lock with it, so the relaunch
//! adoption — the clause's own case — mints as before.
//!
//! Degrades open: a state base that will not resolve, or a lock file that will
//! not open, claims nothing and refuses nothing, so the worst case is the old
//! behaviour (a refused duplicate), never a kit nobody mints. Native only — a
//! web tab holds no install-scoped file lock.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// The directory under the install's state base the claim files live in — one
/// file per successor, never deleted (a lock file is never deleted: a deletion
/// between a holder's open and its lock would split it from a later opener).
const CLAIM_DIR: &str = "succession-kit";

/// One process's view of the install's kit claims.
///
/// Production uses [`KitClaims::process`]; a test builds one per simulated
/// process with [`KitClaims::at`] over a shared directory — two instances over
/// one base contend exactly as two processes do, since each holds its own open
/// file description.
pub struct KitClaims {
    base: Option<PathBuf>,
    held: Mutex<HashMap<String, File>>,
}

static PROCESS: LazyLock<KitClaims> = LazyLock::new(|| {
    let base = fauna_core::platform_ids::sync_state_base();
    if base.is_none() {
        tracing::warn!("[recovery] no state base: succession kit claims degrade open");
    }
    KitClaims {
        base,
        held: Mutex::new(HashMap::new()),
    }
});

impl KitClaims {
    /// This process's claims, over the platform state base the account store
    /// shares across every seat of the install.
    pub fn process() -> &'static KitClaims {
        &PROCESS
    }

    /// Claims over an explicit base — tests, one instance per simulated process.
    pub fn at(base: impl Into<PathBuf>) -> Self {
        Self {
            base: Some(base.into()),
            held: Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, actor_id_hex: &str) -> Option<PathBuf> {
        let base = self.base.as_deref()?;
        is_actor_hex(actor_id_hex).then(|| claim_path(base, actor_id_hex))
    }

    /// Claim the kit `successor_hex` is owed, for this process. Blocking on
    /// purpose: a fresh successor's file is contended only by a sibling's
    /// momentary [`Self::owned_elsewhere`] probe, never by another claimant.
    /// `true` when this process holds it now (already holding it included).
    pub fn claim(&self, successor_hex: &str) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        if held.contains_key(successor_hex) {
            return true;
        }
        let Some(path) = self.path(successor_hex) else {
            return false;
        };
        let file = match fauna_core::fs_lock::open_lock_file(&path) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!("[recovery] the successor kit claim would not open: {e}");
                return false;
            }
        };
        if let Err(e) = file.lock() {
            tracing::warn!("[recovery] the successor kit claim would not lock: {e}");
            return false;
        }
        held.insert(successor_hex.to_string(), file);
        true
    }

    /// Give the claim up — the kit landed, or the ceremony has none to mint.
    /// Releasing an unheld claim is a no-op.
    pub fn release(&self, successor_hex: &str) {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(successor_hex);
    }

    /// Whether this process holds the claim on `actor_id_hex`.
    pub fn holds(&self, actor_id_hex: &str) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(actor_id_hex)
    }

    /// Whether another live process of this install holds the claim on
    /// `actor_id_hex` — a ceremony that will mint that account's kit itself.
    /// Never creates a file: an identity no ceremony ever claimed (every
    /// ordinary account) costs one `stat`.
    pub fn owned_elsewhere(&self, actor_id_hex: &str) -> bool {
        if self.holds(actor_id_hex) {
            return false;
        }
        let Some(path) = self.path(actor_id_hex) else {
            return false;
        };
        if !path.exists() {
            return false;
        }
        let Ok(file) = fauna_core::fs_lock::open_lock_file(&path) else {
            return false;
        };
        match file.try_lock_shared() {
            // Free: nobody is minting it. Dropping `file` lets the probe go.
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(e)) => {
                tracing::warn!("[recovery] the successor kit claim would not probe: {e}");
                false
            }
        }
    }
}

fn claim_path(base: &Path, actor_id_hex: &str) -> PathBuf {
    base.join(CLAIM_DIR).join(format!("{actor_id_hex}.lock"))
}

fn is_actor_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir — no `tempfile` dev-dep for
    /// three tests.
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fauna-kit-claim-row-4934-{}", actor()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn actor() -> String {
        fauna_core::identity::ActorKeypair::generate().actor_id_hex()
    }

    #[test]
    fn a_claim_held_by_another_process_is_owned_elsewhere_until_released() {
        let dir = scratch();
        let ceremony = KitClaims::at(&dir);
        let sibling = KitClaims::at(&dir);
        let successor = actor();

        assert!(
            !sibling.owned_elsewhere(&successor),
            "nobody claimed it yet"
        );
        assert!(ceremony.claim(&successor));
        assert!(sibling.owned_elsewhere(&successor));
        assert!(
            !ceremony.owned_elsewhere(&successor),
            "the claimant's own probe never refuses itself"
        );
        ceremony.release(&successor);
        assert!(!sibling.owned_elsewhere(&successor));
    }

    #[test]
    fn a_dead_claimant_frees_the_claim() {
        let dir = scratch();
        let sibling = KitClaims::at(&dir);
        let successor = actor();
        {
            let ceremony = KitClaims::at(&dir);
            assert!(ceremony.claim(&successor));
            assert!(sibling.owned_elsewhere(&successor));
        } // the process is gone: its descriptors close
        assert!(!sibling.owned_elsewhere(&successor));
    }

    #[test]
    fn junk_and_an_unresolved_base_claim_nothing() {
        let dir = scratch();
        let claims = KitClaims::at(&dir);
        assert!(!claims.claim("../escape"));
        let degraded = KitClaims {
            base: None,
            held: Mutex::new(HashMap::new()),
        };
        assert!(!degraded.claim(&actor()));
        assert!(!degraded.owned_elsewhere(&actor()));
    }
}
