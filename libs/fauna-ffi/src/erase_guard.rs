//! UniFFI door for the question every erasing gesture asks first — **is
//! another instance still serving an account this erase would reach?**
//! (`account-scoping.md` § Concurrent instances → *An erase refuses while a
//! sibling serves the account*).
//!
//! The decision and the lines are shared (`fauna_client_accounts::sole_instance`
//! and `erase_residue`); this module only carries them across the seam, with the
//! two things only the FFI seat knows:
//!
//! - **Where it erases.** The bases asked about are exactly the two
//!   [`crate::account_state_erase_all_scopes`] sweeps — the app's `base_dir` and
//!   the shared store root, resolved from the same `store_container_dir` by the
//!   same resolver — so the question and the erase cannot drift apart.
//! - **Which lock is its own reflection.** A seat on the process-global holder
//!   ([`crate::become_process_session_instance`] — windows) has its lock put
//!   down by the shared probe itself. A seat holding a raw
//!   [`FfiAccountInstanceLock`] (apple) passes it as `own_lock`, and it is put
//!   down and taken again around the probe the same way. ⚠ A raw-lock seat that
//!   leaves `own_lock` out refuses **every** sign-out, even alone on the device.
//!
//! Ask at the gesture, before anything destructive: the erase's own sweep
//! deliberately cannot fail, so a refusal returned from it would come after the
//! stores were already gone.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fauna_core::localized::LocalizedText;

use crate::{FfiAccountInstanceLock, FfiAccountRegistry};

/// An all-accounts erase refused (`fauna_client_accounts::EraseBlocked`) —
/// another instance still serves `accounts`. Refuse the whole gesture: erase
/// nothing, wipe no credentials, stay signed in.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiEraseBlocked {
    /// The accounts another instance serves, for the log. The line names none:
    /// the remedy is the same whichever one it is.
    pub accounts: Vec<String>,
    /// The gesture's shared line, resolved through the app's own i18n pipeline
    /// and painted on the surface that gesture already uses for its residue.
    pub line: LocalizedText,
}

/// A remove-account refused (`fauna_client_accounts::RemoveAccountBlocked`).
/// Paint `line` on the Settings page's `error-message`; the two cases name
/// different remedies, which is why the line travels with the case.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiEraseRemoveBlocked {
    /// This instance serves the account — removing it would unlink the stores
    /// this window is running from.
    ServedHere {
        /// `settings.remove_account_blocked_this_window`.
        line: LocalizedText,
    },
    /// Another live instance serves it.
    ServedElsewhere {
        /// The account, as the probe reported it, for the log.
        accounts: Vec<String>,
        /// `settings.remove_account_blocked_other_window`.
        line: LocalizedText,
    },
}

/// **May sign-out erase every account on this device?** `None` = proceed;
/// `Some` = refuse the whole sign-out and paint `line` where this seat paints
/// sign-out residue.
///
/// `base_dir` and `store_container_dir` are the arguments this seat passes to
/// [`crate::account_state_erase_all_scopes`]; `base_dir` is also its install
/// base, where a sibling window of this app holds its instance lock. `own_lock`
/// is this process's raw lock when it holds one (see the module docs).
#[uniffi::export]
pub fn sign_out_blocked(
    registry: Arc<FfiAccountRegistry>,
    base_dir: String,
    store_container_dir: Option<String>,
    own_lock: Option<Arc<FfiAccountInstanceLock>>,
) -> Option<FfiEraseBlocked> {
    all_accounts_erase_blocked(
        &registry,
        Path::new(&base_dir),
        &crate::account_state::store_root_for(store_container_dir.map(PathBuf::from)),
        own_lock.as_deref(),
    )
    .map(|accounts| FfiEraseBlocked {
        accounts,
        line: fauna_client_accounts::sign_out_blocked_copy(),
    })
}

/// [`sign_out_blocked`] for the unreadable-index floor ("start over on this
/// device"), which runs the same erase over an index naming nobody — so the
/// accounts asked about come from the disk. Its own line: the user pressed
/// *start over*, not *sign out*.
#[uniffi::export]
pub fn start_over_blocked(
    registry: Arc<FfiAccountRegistry>,
    base_dir: String,
    store_container_dir: Option<String>,
    own_lock: Option<Arc<FfiAccountInstanceLock>>,
) -> Option<FfiEraseBlocked> {
    all_accounts_erase_blocked(
        &registry,
        Path::new(&base_dir),
        &crate::account_state::store_root_for(store_container_dir.map(PathBuf::from)),
        own_lock.as_deref(),
    )
    .map(|accounts| FfiEraseBlocked {
        accounts,
        line: fauna_client_accounts::start_over_blocked_copy(),
    })
}

/// **May remove-account erase `actor_id_hex`?** Ask before the registry
/// removal, which drops the account's secret slots — a refusal after it would
/// leave the stores on disk with nothing left to sign in to them.
///
/// The account this instance serves is, in order: `own_lock`'s (a raw-lock
/// seat), else the explicit `serving_here` (a seat with no lock at all —
/// iOS admits one instance per app and takes none, so this is its only way
/// to name the account it serves), else the process-global holder's — so
/// every seat shape is refused [`FfiEraseRemoveBlocked::ServedHere`] for its
/// own account, not only a raw-lock or holder one.
#[uniffi::export]
pub fn remove_account_blocked(
    base_dir: String,
    actor_id_hex: String,
    store_container_dir: Option<String>,
    own_lock: Option<Arc<FfiAccountInstanceLock>>,
    serving_here: Option<String>,
) -> Option<FfiEraseRemoveBlocked> {
    use fauna_client_accounts::RemoveAccountBlocked;
    let store_root = crate::account_state::store_root_for(store_container_dir.map(PathBuf::from));
    let own_lock = own_lock.as_deref();
    let serving_here = own_lock
        .and_then(FfiAccountInstanceLock::serving_actor)
        .or(serving_here)
        .or_else(fauna_client_accounts::process_session_account);
    let blocked = with_own_lock_down(own_lock, || {
        fauna_client_accounts::remove_account_blocked_as(
            serving_here.as_deref(),
            serving_bases(Path::new(&base_dir), &store_root),
            &actor_id_hex,
        )
    })?;
    let line = blocked.copy();
    Some(match blocked {
        RemoveAccountBlocked::ServedHere => FfiEraseRemoveBlocked::ServedHere { line },
        RemoveAccountBlocked::ServedElsewhere(e) => FfiEraseRemoveBlocked::ServedElsewhere {
            accounts: e.accounts,
            line,
        },
    })
}

/// The two places an instance declares itself: the seat's install base and the
/// shared store root.
pub(crate) fn serving_bases<'a>(
    base_dir: &'a Path,
    store_root: &'a Path,
) -> fauna_client_accounts::ServingBases<'a> {
    fauna_client_accounts::ServingBases {
        state_base: Some(base_dir),
        store_root: Some(store_root),
    }
}

/// The all-accounts question, over exactly what
/// [`crate::account_state_erase_all_scopes`] sweeps: the registry's accounts
/// plus every actor scope under `base_dir` and the store root.
fn all_accounts_erase_blocked(
    registry: &FfiAccountRegistry,
    base_dir: &Path,
    store_root: &Path,
    own_lock: Option<&FfiAccountInstanceLock>,
) -> Option<Vec<String>> {
    let blocked = with_own_lock_down(own_lock, || {
        fauna_client_accounts::sign_out_blocked(
            registry.registry(),
            serving_bases(base_dir, store_root),
            &[base_dir, store_root],
        )
    })?;
    Some(blocked.accounts)
}

pub(crate) fn with_own_lock_down<T>(
    own_lock: Option<&FfiAccountInstanceLock>,
    probe: impl FnOnce() -> T,
) -> T {
    match own_lock {
        Some(lock) => lock.without_own_lock(probe),
        None => probe(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FfiSecretStore, acquire_account_instance_lock_shared};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MapStore(Mutex<HashMap<String, String>>);

    impl FfiSecretStore for MapStore {
        fn get(&self, key: String) -> Option<String> {
            self.0.lock().unwrap().get(&key).cloned()
        }
        fn set(&self, key: String, value: String) {
            self.0.lock().unwrap().insert(key, value);
        }
        fn delete(&self, key: String) {
            self.0.lock().unwrap().remove(&key);
        }
    }

    /// One device: an install base, a store container, and a registry holding
    /// one account.
    struct Device {
        _tmp: tempfile::TempDir,
        base: String,
        container: String,
        registry: Arc<FfiAccountRegistry>,
        actor: String,
    }

    impl Device {
        fn new(secret_fill: char) -> Self {
            let tmp = tempfile::tempdir().expect("temp dir");
            let base = tmp.path().join("app");
            let container = tmp.path().join("store");
            std::fs::create_dir_all(&base).expect("app base");
            std::fs::create_dir_all(&container).expect("store container");
            let registry = FfiAccountRegistry::new(Arc::new(MapStore::default()));
            let actor = registry
                .add_account(
                    secret_fill.to_string().repeat(64),
                    Some("https://a.example".into()),
                    None,
                )
                .expect("add account");
            Self {
                base: base.to_string_lossy().into_owned(),
                container: container.to_string_lossy().into_owned(),
                _tmp: tmp,
                registry,
                actor,
            }
        }

        fn store_root(&self) -> PathBuf {
            crate::account_state::store_root_for(Some(PathBuf::from(&self.container)))
        }

        /// This app's raw shared lock, as apple takes it.
        fn raw_lock(&self, actor: &str) -> Arc<FfiAccountInstanceLock> {
            let lock = acquire_account_instance_lock_shared(
                self.base.clone(),
                actor.to_string(),
                Some(self.container.clone()),
            )
            .expect("the shared acquire is not refused");
            assert!(lock.is_held(), "a genuinely held lock, not the degrade");
            lock
        }

        fn sign_out(&self, own: Option<&Arc<FfiAccountInstanceLock>>) -> Option<FfiEraseBlocked> {
            sign_out_blocked(
                self.registry.clone(),
                self.base.clone(),
                Some(self.container.clone()),
                own.cloned(),
            )
        }

        fn served_at_root(&self, actor: &str) -> bool {
            fauna_account_store::locks::ServingLock::is_served(&self.store_root(), actor)
        }

        fn served_at_base(&self, actor: &str) -> bool {
            fauna_client_accounts::AccountInstanceLock::is_served(Path::new(&self.base), actor)
        }
    }

    /// ⚠ **The single-window Mac — the case a wrong door refuses.** apple holds
    /// its lock as a raw object; passed as `own_lock`, the probe puts it down,
    /// answers "not blocked", and takes it (and its store-root declaration)
    /// again afterwards.
    #[test]
    fn a_lone_raw_lock_instance_may_sign_out_and_still_holds_its_locks_afterwards() {
        let device = Device::new('1');
        let own = device.raw_lock(&device.actor);
        assert!(
            device.served_at_root(&device.actor),
            "the raw route declares itself at the root"
        );

        assert_eq!(
            device.sign_out(Some(&own)),
            None,
            "nobody else serves this account"
        );

        assert!(
            own.is_held(),
            "the instance lock was restored after the probe"
        );
        assert!(
            device.served_at_base(&device.actor),
            "and is really held again"
        );
        assert!(
            device.served_at_root(&device.actor),
            "and so is the serving lock"
        );
    }

    /// Why `own_lock` exists: leave it out and the lone raw-lock instance meets
    /// its own reflection. This pins that the release above is load-bearing.
    #[test]
    fn a_raw_lock_left_out_of_the_door_refuses_its_own_sign_out() {
        let device = Device::new('2');
        let _own = device.raw_lock(&device.actor);
        assert!(
            device.sign_out(None).is_some(),
            "without its own lock put down the probe cannot tell itself from a sibling"
        );
    }

    /// A sibling window of the same app serving the account refuses the sign-out,
    /// with the shared line — even with this instance's own lock put down.
    #[test]
    fn a_sibling_window_refuses_the_sign_out_with_the_shared_line() {
        let device = Device::new('3');
        let own = device.raw_lock(&device.actor);
        let _sibling = device.raw_lock(&device.actor);

        let blocked = device
            .sign_out(Some(&own))
            .expect("a sibling serves the account");
        assert_eq!(blocked.accounts, vec![device.actor.clone()]);
        assert_eq!(blocked.line, fauna_client_accounts::sign_out_blocked_copy());
        assert!(own.is_held(), "the refusal path restores the lock too");
    }

    /// A sibling **app** declared only at the shared store root — tui or linux on
    /// the same OS login — refuses the sign-out too.
    #[test]
    fn another_app_serving_at_the_shared_root_refuses_the_sign_out() {
        let device = Device::new('4');
        let own = device.raw_lock(&device.actor);
        let other_app = match fauna_account_store::locks::ServingLock::acquire(
            &device.store_root(),
            &device.actor,
        ) {
            fauna_account_store::locks::ServingLockOutcome::Held(lock) => lock,
            fauna_account_store::locks::ServingLockOutcome::Degraded(e) => {
                panic!("serving lock: {e}")
            }
        };

        assert!(
            device.sign_out(Some(&own)).is_some(),
            "the other app is still serving"
        );
        drop(other_app);
        assert_eq!(
            device.sign_out(Some(&own)),
            None,
            "and once it is gone, sign-out proceeds"
        );
    }

    /// The erase sweeps every actor scope under its two roots whether or not the
    /// registry names it, so the question does too — the floor's index names
    /// nobody, and its line is the start-over one.
    #[test]
    fn an_account_only_the_disk_names_is_asked_about() {
        let device = Device::new('5');
        let orphan = "ab".repeat(32);
        std::fs::create_dir_all(Path::new(&device.base).join(&orphan)).expect("orphan scope");
        let _sibling = device.raw_lock(&orphan);

        let blocked = start_over_blocked(
            device.registry.clone(),
            device.base.clone(),
            Some(device.container.clone()),
            None,
        )
        .expect("a sibling serves an account the erase would reach");
        assert_eq!(blocked.accounts, vec![orphan]);
        assert_eq!(
            blocked.line,
            fauna_client_accounts::start_over_blocked_copy()
        );
    }

    // The holder route (windows' single-window box) is pinned in
    // `accounts_registry`'s process-session-instance test, which owns the
    // process-global holder and launch binding: a second test here driving the
    // same globals would race it.

    /// A sibling **app** declared only at the shared store root — tui or linux
    /// on the same OS login — refuses a remove-account too, the
    /// [`remove_account_blocked`] twin of
    /// [`another_app_serving_at_the_shared_root_refuses_the_sign_out`]. The
    /// account being removed is deliberately NOT `device.actor` (this window's
    /// own served account, which would hit `ServedHere` regardless of any
    /// sibling) — a second actor, served only by `other_app`'s store-root lock,
    /// isolates the ServedElsewhere-over-the-shared-root path.
    #[test]
    fn another_app_serving_at_the_shared_root_refuses_remove_account() {
        let device = Device::new('6');
        let own = device.raw_lock(&device.actor);
        let elsewhere = "ee".repeat(32);
        let other_app = match fauna_account_store::locks::ServingLock::acquire(
            &device.store_root(),
            &elsewhere,
        ) {
            fauna_account_store::locks::ServingLockOutcome::Held(lock) => lock,
            fauna_account_store::locks::ServingLockOutcome::Degraded(e) => {
                panic!("serving lock: {e}")
            }
        };

        let remove = || {
            remove_account_blocked(
                device.base.clone(),
                elsewhere.clone(),
                Some(device.container.clone()),
                Some(own.clone()),
                None,
            )
        };
        assert_eq!(
            remove(),
            Some(FfiEraseRemoveBlocked::ServedElsewhere {
                accounts: vec![elsewhere.clone()],
                line: fauna_client_accounts::remove_account_blocked_copy(),
            }),
            "a sibling app declared only at the shared store root is still a sibling"
        );

        drop(other_app);
        assert_eq!(
            remove(),
            None,
            "and once it is gone, the account is removable"
        );
        assert!(own.is_held(), "this instance still holds its own lock");
    }

    /// Remove-account's three answers through the raw route: this instance's own
    /// account is `ServedHere`, an account a sibling serves is `ServedElsewhere`,
    /// and an account nobody serves may go — each with its own line.
    #[test]
    fn remove_account_answers_served_here_served_elsewhere_and_free() {
        let device = Device::new('7');
        let own = device.raw_lock(&device.actor);
        let remove = |actor: &str| {
            remove_account_blocked(
                device.base.clone(),
                actor.to_string(),
                Some(device.container.clone()),
                Some(own.clone()),
                None,
            )
        };

        assert_eq!(
            remove(&device.actor),
            Some(FfiEraseRemoveBlocked::ServedHere {
                line: fauna_client_accounts::remove_account_served_here_copy(),
            })
        );

        let elsewhere = "cd".repeat(32);
        let _sibling = device.raw_lock(&elsewhere);
        assert_eq!(
            remove(&elsewhere),
            Some(FfiEraseRemoveBlocked::ServedElsewhere {
                accounts: vec![elsewhere.clone()],
                line: fauna_client_accounts::remove_account_blocked_copy(),
            })
        );

        assert_eq!(
            remove(&"ef".repeat(32)),
            None,
            "an account nobody serves may go"
        );
        assert!(own.is_held(), "and this instance still holds its own lock");
    }
}
