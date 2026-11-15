//! **The phones' engine roster** — the predicate the projection walk's
//! take-over asks before it claims an adoption marker (`succession-cut.md`
//! ruling (11)(l), *one consumer per marker*: the walk claims a marker only for
//! a set with no engine binding on this device at that moment).
//!
//! On a desktop the sync agent hosts every engine, and its location roster is
//! read in shared Rust (`fauna_client_folders::agent_bound_sets`). A phone
//! hosts its engines in places only the app can see — iOS in its File Provider
//! extension (one domain per set) and its in-app photo ingest, android in its
//! documents provider's on-demand sets, its watched folders and its photo
//! backup — so the app lends the roster through [`FfiEngineRoster`], installed
//! once with [`install_engine_roster`], and the launch pass reads it through
//! [`installed_engine_roster`] at each pass. No roster installed, or one whose
//! read fails, is an unread roster: the take-over claims no new marker on that
//! pass and finishes a walk already claimed.

use std::sync::{Arc, Mutex};

use fauna_client_folders::{EngineBoundSets, EngineHoldings};

use crate::FfiError;

/// The app's reading of which sets an engine on this device is bound to.
/// Called off the UI thread, at each launch folder pass.
#[uniffi::export(with_foreign)]
pub trait FfiEngineRoster: Send + Sync {
    /// Every set an engine on this device is bound to now, each as its
    /// `FolderRef` wire form or its actor-scoped identity (the File Provider
    /// domain identifier on iOS) — a paused or access-revoked binding
    /// included, since it is still a binding. An `Err` when a host could not
    /// be read: an unread binding may be an engine's.
    fn bound_folder_refs(&self) -> Result<Vec<String>, FfiError>;
}

/// This process's roster (one per process, last install wins).
static ROSTER: Mutex<Option<Arc<dyn FfiEngineRoster>>> = Mutex::new(None);

/// Lend the app's engine roster to the launch folder pass, replacing any
/// earlier one; `None` clears it. Call at app start, before the conversations
/// session is built — a pass that runs with none installed claims nothing new.
/// The desktops never need it: their sync agent's roster is read in Rust.
#[uniffi::export]
pub fn install_engine_roster(roster: Option<Arc<dyn FfiEngineRoster>>) {
    *ROSTER.lock().unwrap_or_else(|e| e.into_inner()) = roster;
}

fn installed() -> Option<Arc<dyn FfiEngineRoster>> {
    ROSTER.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The launch pass's roster over the installed [`FfiEngineRoster`], folded
/// into account `actor`'s holdings (`EngineHoldings::from_host_roster`).
#[cfg_attr(not(any(target_os = "ios", target_os = "android")), allow(dead_code))]
pub(crate) fn installed_engine_roster(actor: [u8; 32]) -> Arc<dyn EngineBoundSets> {
    Arc::new(HostRoster {
        actor,
        source: installed,
    })
}

/// [`EngineBoundSets`] over a lent roster, read fresh at every pass so a
/// roster installed after the session was built is still seen.
struct HostRoster {
    actor: [u8; 32],
    source: fn() -> Option<Arc<dyn FfiEngineRoster>>,
}

#[async_trait::async_trait]
impl EngineBoundSets for HostRoster {
    async fn engine_holdings(&self) -> anyhow::Result<EngineHoldings> {
        let roster = (self.source)()
            .ok_or_else(|| anyhow::anyhow!("no engine roster is installed on this device"))?;
        // Off the runtime's workers: the app's read may touch its own storage.
        let entries = tokio::task::spawn_blocking(move || roster.bound_folder_refs())
            .await
            .map_err(|e| anyhow::anyhow!("the engine roster read did not finish: {e}"))?
            .map_err(|e| anyhow::anyhow!("the engine roster is unreadable: {e}"))?;
        EngineHoldings::from_host_roster(self.actor, entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::folder_keys::FolderRef;

    struct Fixed(Result<Vec<String>, String>);

    impl FfiEngineRoster for Fixed {
        fn bound_folder_refs(&self) -> Result<Vec<String>, FfiError> {
            self.0.clone().map_err(|msg| FfiError::General { msg })
        }
    }

    const ME: [u8; 32] = [3; 32];

    fn bound_set() -> FolderRef {
        FolderRef::parse("local:1").unwrap()
    }

    fn with(source: fn() -> Option<Arc<dyn FfiEngineRoster>>) -> HostRoster {
        HostRoster { actor: ME, source }
    }

    /// The installed roster is folded under the account's actor: this
    /// account's scoped identity binds its bare ref, and the walk may not claim
    /// that set while it may claim another.
    #[tokio::test]
    async fn an_installed_roster_binds_the_sets_it_names() {
        let h =
            with(|| Some(Arc::new(Fixed(Ok(vec![bound_set().scoped_to(ME).to_wire()]))) as Arc<_>))
                .engine_holdings()
                .await
                .unwrap();
        assert!(!h.walk_may_claim(Some(&bound_set().to_wire()), &[1; 32]));
        assert!(h.walk_may_claim(Some("local:2"), &[1; 32]));
    }

    /// No roster installed, and a roster whose read fails, are both unread —
    /// never an empty roster, which would let the walk claim every set.
    #[tokio::test]
    async fn no_roster_or_a_failing_one_is_unread() {
        assert!(with(|| None).engine_holdings().await.is_err());
        assert!(
            with(|| Some(Arc::new(Fixed(Err("domains unreadable".into()))) as Arc<_>))
                .engine_holdings()
                .await
                .is_err()
        );
    }

    /// The process slot: an install is what the pass reads, a clear empties it.
    #[test]
    fn install_and_clear_reach_the_slot() {
        install_engine_roster(Some(Arc::new(Fixed(Ok(Vec::new())))));
        assert!(installed().is_some());
        install_engine_roster(None);
        assert!(installed().is_none());
    }
}
